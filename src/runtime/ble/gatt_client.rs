//! Vendor GATT 客户端(V2 tokio 原生 async)。
//!
//! 通过 btleplug 连接固件的 Vendor GATT Service (0xFE60),
//! 订阅 Event (FE62) 和 Audio (FE63) 通知,通过 Command (FE61) 写入命令。
//!
//! 事件通过 `broadcast::Sender<BoardEvent>` 上报,mSBC 音频帧转给 [`AudioFrameSink`]。
//!
//! **V2 变化**:删除 V1 的 `ble_run` 同步包装(自建 current_thread runtime + std mpsc)。
//! 所有方法改为 async,直接 `.await` btleplug 原生 API,跑在调用方的 tokio runtime 上。
//! pending map 从 `std::sync::mpsc` 改为 `tokio::sync::oneshot`。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use btleplug::api::{Central as _, Peripheral as _, ScanFilter, ValueNotification, WriteType};
use btleplug::platform::{Adapter, Peripheral};
use futures_util::{Stream, StreamExt};
use tokio::sync::broadcast;

use crate::kernel::consumer_hold::{ConsumerHeldTracker, ConsumerModeTracker};
use crate::kernel::event::{
    AiVoiceKeyEvent, BoardEvent, ErrorEvent, KeySource, ModeChangeEvent, ModeSource,
};
use crate::kernel::key_aggregator::KeyStateAggregator;
use crate::kernel::protocol_gatt as protocol;
use crate::kernel::protocol_hid::{
    is_ai_voice_consumer_code, WorkMode, CMD_AUDIO_DATA, CMD_DEVICE_DISCONNECT, CMD_STATUS,
    CMD_WORK_MODE_DATA,
};
#[cfg(feature = "test-mode")]
use crate::kernel::protocol_hid::{parse_factory_key_event_unscoped, CMD_AI_FACTORY_KEY_EVENT};
use crate::kernel::sink::{AudioFrameSink, EncodedAudioDecoderSink};

/// Bluetooth CCC(Client Characteristic Configuration)描述符的标准 UUID。
///
/// 即 `00002902-0000-0000-1000-00805f9b34fb`。btleplug 在多数平台会自动订阅,
/// 但失败时需手动找到这个描述符写入 `0x01 0x00` 开启通知。
///
/// 注意:`Uuid::from_u128(0x2902)` **不**等于这个标准 UUID——前者高位全零,
/// 与 Bluetooth Base UUID 完全不同。旧代码里 `|| from_u128(0x2902)` 是永远不匹配的死分支。
const CCC_DESCRIPTOR_UUID: uuid::Uuid = uuid::Uuid::from_bytes([
    0x00, 0x00, 0x29, 0x02, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0x80, 0x5f, 0x9b, 0x34, 0xfb,
]);

/// 判断一条扫描结果是不是要找的 Vendor GATT 设备，返回命中的设备名。
///
/// 同时比对广播名（`advertisement_name`）与 `local_name`，任一命中即可：
/// btleplug 的 WinRT 后端在 connect 之后会把 `local_name` 改写成 Windows
/// 系统缓存的设备名，只看 `local_name` 会让重连时的精确匹配失败。
/// - `target = Some(t)`：任一名字等于 `t` 即命中，返回 `t`；
/// - `target = None`：取第一个带 `REAI_VB_` 前缀的名字（广播名优先）。
pub fn match_vendor_name(
    local_name: Option<&str>,
    advertisement_name: Option<&str>,
    target: Option<&str>,
) -> Option<String> {
    let names = [advertisement_name, local_name];
    match target {
        Some(t) => names
            .iter()
            .flatten()
            .any(|n| *n == t)
            .then(|| t.to_string()),
        None => names
            .iter()
            .flatten()
            .find(|n| n.starts_with(protocol::VENDOR_DEVICE_PREFIX))
            .map(|n| n.to_string()),
    }
}

/// BLE 连接节奏（对齐 webTest 在 Windows 上的经验）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BleConnectPacing {
    /// 扫描结束到发起连接之间的等待。
    pub pre_connect_delay: std::time::Duration,
    /// 「连接 + 服务发现 + 订阅」整体超时。
    pub connect_timeout: std::time::Duration,
    /// 同一个扫描结果最多连几次。
    pub attempts: u32,
    /// 第 1 次失败后的退避，之后每次翻倍。
    pub first_backoff: std::time::Duration,
}

impl BleConnectPacing {
    /// 当前平台的节奏。
    ///
    /// 非 Windows 保持原行为（不等待、10s 超时、只连一次，失败交回 hotplug 重扫）。
    /// Windows：扫描后等 500ms 再连；WinRT 首连会做 Uncached 服务发现（每个服务
    /// 最长 5s 才回落 Cached），超时放宽到 20s；同一个扫描结果最多连 3 次，
    /// 退避 1s、2s——设备已不再广播时，重扫往往拿不回它。
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self {
                pre_connect_delay: std::time::Duration::from_millis(500),
                connect_timeout: std::time::Duration::from_secs(20),
                attempts: 3,
                first_backoff: std::time::Duration::from_secs(1),
            }
        } else {
            Self {
                pre_connect_delay: std::time::Duration::ZERO,
                connect_timeout: std::time::Duration::from_secs(10),
                attempts: 1,
                first_backoff: std::time::Duration::from_secs(1),
            }
        }
    }

    /// 第 `failed_attempt` 次（从 1 起）失败后、下一次之前的退避。
    pub fn backoff_after(&self, failed_attempt: u32) -> std::time::Duration {
        self.first_backoff * 2u32.saturating_pow(failed_attempt.saturating_sub(1).min(4))
    }
}

async fn connect_with_pacing<C, CF, D, DF>(
    pacing: BleConnectPacing,
    still_wanted: &(dyn Fn() -> bool + Send + Sync),
    mut connect: C,
    mut disconnect: D,
) -> Result<()>
where
    C: FnMut() -> CF,
    CF: std::future::Future<Output = Result<()>>,
    D: FnMut() -> DF,
    DF: std::future::Future<Output = ()>,
{
    if !pacing.pre_connect_delay.is_zero() {
        tokio::time::sleep(pacing.pre_connect_delay).await;
    }
    let mut attempt = 1;
    loop {
        if !still_wanted() {
            return Err(anyhow::anyhow!("BLE 目标已取消或改变，放弃连接"));
        }
        match connect().await {
            Ok(()) => return Ok(()),
            Err(error) => {
                // 包括最后一次失败：已连接但发现/订阅失败的设备可能不再广播，
                // 不清理便交回扫描会让后续自动重连一直找不到它。
                let _ = tokio::time::timeout(std::time::Duration::from_secs(3), disconnect()).await;
                if attempt >= pacing.attempts {
                    return Err(error);
                }
                let backoff = pacing.backoff_after(attempt);
                log::warn!(
                    target: "gatt",
                    "BLE 连接第 {}/{} 次失败: {}，{}ms 后重试",
                    attempt,
                    pacing.attempts,
                    error,
                    backoff.as_millis()
                );
                tokio::time::sleep(backoff).await;
                attempt += 1;
            }
        }
    }
}

/// BLE 扫描发现的设备信息(手动扫描/选择连接用)
#[derive(Debug, Clone, serde::Serialize)]
pub struct BleDeviceInfo {
    /// 设备名(REAI_VB_XXXX)
    pub name: String,
    /// btleplug PeripheralId 的字符串形式(调试/去重用)
    pub id: String,
    /// 信号强度(dBm),可能为 None
    pub rssi: Option<i16>,
    /// MAC 地址(冒号分隔大写)
    pub address: String,
}

/// 发现的 GATT 特征值句柄
#[derive(Clone)]
struct GattChars {
    cmd: btleplug::api::Characteristic,
    event: btleplug::api::Characteristic,
    audio: btleplug::api::Characteristic,
}

/// 等待响应的 oneshot 注册项
struct PendingResponse {
    #[allow(dead_code)]
    cmd: u8,
    epoch: u64,
    tx: tokio::sync::oneshot::Sender<Vec<u8>>,
}

struct NotificationSession {
    epoch: u64,
    current_epoch: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
    pending: Arc<Mutex<HashMap<u8, PendingResponse>>>,
    closed: Arc<tokio::sync::Notify>,
}

enum NotificationPoll {
    Message(Option<ValueNotification>),
    Heartbeat,
    Stopped,
}

impl NotificationSession {
    fn current(&self) -> bool {
        self.current_epoch.load(Ordering::SeqCst) == self.epoch
            && self.running.load(Ordering::SeqCst)
    }

    async fn next(
        &self,
        stream: &mut (impl Stream<Item = ValueNotification> + Unpin),
        heartbeat_deadline: tokio::time::Instant,
    ) -> NotificationPoll {
        let closed = self.closed.notified();
        tokio::pin!(closed);
        closed.as_mut().enable();
        if !self.current() {
            return NotificationPoll::Stopped;
        }
        tokio::select! {
            _ = &mut closed => NotificationPoll::Stopped,
            notification = stream.next() => NotificationPoll::Message(notification),
            _ = tokio::time::sleep_until(heartbeat_deadline) => NotificationPoll::Heartbeat,
        }
    }

    fn dispatch_event(
        &self,
        data: &[u8],
        state: &mut GattEventState,
        event_tx: &broadcast::Sender<BoardEvent>,
    ) -> bool {
        if !self.current() {
            return false;
        }
        handle_event(data, state, event_tx, &self.pending);
        if state.device_disconnect_reason.load(Ordering::SeqCst) != 0 {
            self.finish(state, event_tx);
            return false;
        }
        true
    }

    fn finish(&self, state: &mut GattEventState, event_tx: &broadcast::Sender<BoardEvent>) {
        if self.current_epoch.load(Ordering::SeqCst) == self.epoch {
            state.release_all(event_tx);
            self.running.store(false, Ordering::SeqCst);
            self.pending
                .lock()
                .unwrap()
                .retain(|_, p| p.epoch != self.epoch);
            self.closed.notify_waiters();
        }
    }
}

/// handle_event 的有状态上下文(替代全局 static,支持单设备实例化)
#[derive(Default)]
struct GattEventState {
    /// 当前按着哪些键。
    ///
    /// 原来这里是「最近一次按下的键索引」，一次只记得住一个。BLE 与 USB 走的是同一条
    /// 单值 Consumer 流，于是按住 Tab 再转旋钮时，这个记录被旋钮顶掉，随后的 0x0000
    /// 把释放算在了旋钮头上——**按住的那个键再也等不到松手事件**，Command 一直按着，
    /// 只能靠上层 30 秒硬超时兜底。改用与 USB 共享的账本（[`ConsumerHeldTracker`]）。
    consumer: ConsumerHeldTracker,
    /// 按来源合并并 diff 出按下/松开事件，与 USB 用的是同一套。
    ///
    /// 延迟创建：它需要事件发送端，而发送端要等 `handle_event` 传进来。
    aggregator: Option<KeyStateAggregator>,
    ai_voice_pressed: bool,
    mode: ConsumerModeTracker,
    /// 设备主动断开通知(CMD=0x60)的原因码。0=本连接未收到,≥1=收到(固件 reason 字节)。
    ///
    /// Arc 与 [`VendorGattClient`] 共享:通知任务收到 0x60 时写入,hotplug 在 BLE 会话
    /// 结束时读取,把「固件主动断开(闲置休眠/长按断开)」与信号丢失区分开
    /// (上报 [`DisconnectReason::DeviceDisconnect`])。reason=0x03 也可能是认证超时，
    /// 只有身份握手中的明确拒绝才能判定绑定其他电脑并停止重连。
    device_disconnect_reason: Arc<AtomicU8>,
}

impl GattEventState {
    /// 断开时把还按着的键全部交代成松开。
    ///
    /// 这个状态本身随通知任务一起销毁，所以「不清也不会串到下一次连接」——
    /// 但**消费方那边不会自动忘记**：只收到过按下、没收到松开的键，会一直被当成按着。
    /// 语音键卡住尤其难受（录音停不下来），所以断开时必须补齐。
    fn release_all(&mut self, event_tx: &broadcast::Sender<BoardEvent>) {
        if let Some(aggregator) = &self.aggregator {
            aggregator.report_change(KeySource::Gatt, Vec::new(), None);
        }
        if self.ai_voice_pressed {
            self.ai_voice_pressed = false;
            log::info!(target: "gatt", "通知监听退出，补发 AI 语音键释放");
            let _ = event_tx.send(BoardEvent::AiVoiceKey(AiVoiceKeyEvent { pressed: false }));
        }
    }
}

pub struct VendorGattClient {
    adapter: Adapter,
    peripheral: Mutex<Option<Peripheral>>,
    chars: Mutex<Option<GattChars>>,
    running: Arc<AtomicBool>,
    audio_sink: Arc<dyn AudioFrameSink>,
    event_tx: broadcast::Sender<BoardEvent>,
    /// 等待命令响应的 pending map(cmd → oneshot sender)
    pending_responses: Arc<Mutex<HashMap<u8, PendingResponse>>>,
    /// GATT 写入序列化锁:防止并发写入导致 pending 响应错乱。
    /// 用 tokio::sync::Mutex —— 持锁期间会 `.await send_command()`(std Mutex 跨 await 是 UB)。
    write_lock: tokio::sync::Mutex<()>,
    seq: AtomicU64,
    /// Monotonic local epoch for every notification-loop lifetime/reconnect.
    connection_epoch: Arc<AtomicU64>,
    connection_closed: Arc<tokio::sync::Notify>,
    /// FE63 may be subscribed for connection health, but frames reach the Host only after an
    /// explicit board-audio route/lease activates this gate.
    audio_enabled: Arc<AtomicBool>,
    /// Changes on every route gate transition so queued packets cannot cross session bounds.
    audio_route_epoch: Arc<AtomicU64>,
    /// 通知监听 task 的 JoinHandle。`disconnect` 时 await,避免重连场景下
    /// 旧 task 末尾的 `pending.clear()` 清掉新 task 刚注册的 pending response。
    notify_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// 音频解码线程的 JoinHandle。`disconnect` 时 join(超时保护),避免线程悬挂。
    audio_thread_handle: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// 设备主动断开通知(0x60)原因码,与通知任务的 [`GattEventState`] 同名字段共享。
    device_disconnect_reason: Arc<AtomicU8>,
}

impl VendorGattClient {
    pub fn new(
        adapter: Adapter,
        audio_sink: Arc<dyn AudioFrameSink>,
        event_tx: broadcast::Sender<BoardEvent>,
    ) -> Self {
        Self {
            adapter,
            peripheral: Mutex::new(None),
            chars: Mutex::new(None),
            running: Arc::new(AtomicBool::new(false)),
            audio_sink,
            event_tx,
            pending_responses: Arc::new(Mutex::new(HashMap::new())),
            write_lock: tokio::sync::Mutex::new(()),
            seq: AtomicU64::new(0),
            connection_epoch: Arc::new(AtomicU64::new(0)),
            connection_closed: Arc::new(tokio::sync::Notify::new()),
            audio_enabled: Arc::new(AtomicBool::new(false)),
            audio_route_epoch: Arc::new(AtomicU64::new(0)),
            notify_handle: Mutex::new(None),
            audio_thread_handle: Mutex::new(None),
            device_disconnect_reason: Arc::new(AtomicU8::new(0)),
        }
    }

    /// 扫描发现 Vendor GATT 设备,返回 (Peripheral, 设备名)。
    ///
    /// 传 target_name 精确匹配;否则匹配 REAI_VB_ 前缀。5 轮 × 2s。
    pub async fn scan_for_device(&self, target_name: Option<&str>) -> Result<(Peripheral, String)> {
        let adapter = self.adapter.clone();
        let target = target_name.map(|s| s.to_string());

        // 不用 Service UUID 过滤:固件广播包可能不含 0xFE60,改为名字前缀匹配
        adapter
            .start_scan(ScanFilter::default())
            .await
            .map_err(|e| anyhow::anyhow!("BLE 扫描启动失败: {}", e))?;

        for round in 0..5 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            let peripherals = adapter
                .peripherals()
                .await
                .map_err(|e| anyhow::anyhow!("获取设备列表失败: {}", e))?;

            for p in &peripherals {
                if let Ok(Some(props)) = p.properties().await {
                    if let Some(name) = match_vendor_name(
                        props.local_name.as_deref(),
                        props.advertisement_name.as_deref(),
                        target.as_deref(),
                    ) {
                        log::info!(target: "gatt", "发现 Vendor GATT 设备: {} ({})", name, p.id());
                        if let Err(e) = adapter.stop_scan().await {
                            log::warn!(target: "gatt", "stop_scan 失败(可能仍占用适配器): {}", e);
                        }
                        return Ok((p.clone(), name));
                    }
                }
            }

            log::debug!(
                target: "gatt",
                "扫描轮次 {}/5 未找到{}",
                round + 1,
                target.as_ref().map(|t| format!(" {}", t)).unwrap_or_default()
            );
        }

        if let Err(e) = adapter.stop_scan().await {
            log::warn!(target: "gatt", "stop_scan 失败(可能仍占用适配器): {}", e);
        }
        Err(anyhow::anyhow!(
            "未发现 Vendor GATT 设备{}",
            target
                .as_ref()
                .map(|t| format!(" {}", t))
                .unwrap_or_default()
        ))
    }

    /// 扫描列出周围所有 Vendor GATT 设备(REAI_VB_ 前缀),返回去重后的列表。
    ///
    /// 与 [`scan_for_device`](Self::scan_for_device) 不同:后者找到第一个就返回,
    /// 本方法扫满 `timeout` 累积所有匹配设备(供手动选择)。
    /// 不连接、不订阅,纯扫描。扫完自动 stop_scan。
    pub async fn scan_all_vendor_devices(
        &self,
        timeout: std::time::Duration,
    ) -> Result<Vec<BleDeviceInfo>> {
        let adapter = self.adapter.clone();
        adapter
            .start_scan(ScanFilter::default())
            .await
            .map_err(|e| anyhow::anyhow!("BLE 扫描启动失败: {}", e))?;

        // 用 id 去重(同一设备多轮广播)
        let mut seen: std::collections::HashMap<String, BleDeviceInfo> =
            std::collections::HashMap::new();
        let deadline = tokio::time::Instant::now() + timeout;

        while tokio::time::Instant::now() < deadline {
            let peripherals = adapter
                .peripherals()
                .await
                .map_err(|e| anyhow::anyhow!("获取设备列表失败: {}", e))?;

            for p in &peripherals {
                if let Ok(Some(props)) = p.properties().await {
                    if let Some(name) = match_vendor_name(
                        props.local_name.as_deref(),
                        props.advertisement_name.as_deref(),
                        None,
                    ) {
                        let id_str = format!("{:?}", p.id());
                        seen.entry(id_str.clone()).or_insert(BleDeviceInfo {
                            name: name.clone(),
                            id: id_str,
                            rssi: props.rssi,
                            address: format!("{}", props.address),
                        });
                    }
                }
            }

            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        if let Err(e) = adapter.stop_scan().await {
            log::warn!(target: "gatt", "stop_scan 失败(可能仍占用适配器): {}", e);
        }
        Ok(seen.into_values().collect())
    }

    /// 连接设备、发现服务、订阅 Event 通知。Audio 留到身份准入之后。
    ///
    /// 整个「连接+服务发现+订阅」包 10s 超时:btleplug 在 macOS 遇到
    /// "Event receiver died" 时 discover_services().await 永不完成,
    /// 超时则返回失败 → hotplug 重新 scan+connect,给设备 BLE 就绪时间。
    pub async fn connect(&self, peripheral: &Peripheral) -> Result<()> {
        let p = peripheral.clone();
        let inner = async {
            p.connect()
                .await
                .map_err(|e| anyhow::anyhow!("Vendor GATT 连接失败: {}", e))?;

            log::info!(target: "gatt", "BLE 已连接,开始服务发现");

            p.discover_services()
                .await
                .map_err(|e| anyhow::anyhow!("GATT 服务发现失败: {}", e))?;

            let chars = p.characteristics();
            log::info!(target: "gatt", "GATT 服务发现完成,共 {} 个特征值", chars.len());

            let cmd = chars
                .iter()
                .find(|c| c.uuid == protocol::CMD_CHAR_UUID)
                .ok_or_else(|| anyhow::anyhow!("未找到 Command 特征值 (FE61)"))?;
            let event = chars
                .iter()
                .find(|c| c.uuid == protocol::EVENT_CHAR_UUID)
                .ok_or_else(|| anyhow::anyhow!("未找到 Event 特征值 (FE62)"))?;
            let audio = chars
                .iter()
                .find(|c| c.uuid == protocol::AUDIO_CHAR_UUID)
                .ok_or_else(|| anyhow::anyhow!("未找到 Audio 特征值 (FE63)"))?;

            subscribe_with_ccc_fallback(&p, event, "Event").await?;

            Ok(GattChars {
                cmd: cmd.clone(),
                event: event.clone(),
                audio: audio.clone(),
            })
        };

        let connect_timeout = BleConnectPacing::current().connect_timeout;
        let found_chars = match tokio::time::timeout(connect_timeout, inner).await {
            Ok(r) => r,
            Err(_) => Err(anyhow::anyhow!(
                "BLE connect 超时({}s)— 可能 CoreBluetooth Event receiver died 或 WinRT 服务发现卡住,将重试",
                connect_timeout.as_secs()
            )),
        }?;

        self.connection_epoch.fetch_add(1, Ordering::SeqCst);
        self.connection_closed.notify_waiters();
        *self.peripheral.lock().unwrap() = Some(peripheral.clone());
        *self.chars.lock().unwrap() = Some(found_chars);
        // 新连接清掉上一连接可能残留的 0x60 主动断开标志
        self.device_disconnect_reason.store(0, Ordering::SeqCst);
        self.running.store(true, Ordering::SeqCst);

        Ok(())
    }

    /// 按 [`BleConnectPacing::current`] 的节奏连接：先等一下，失败后退避重试。
    ///
    /// 非 Windows 只尝试一次；任何失败都会先清理可能残留的半截连接。
    /// `still_wanted` 在每次尝试前调用；返回 `false`（目标被取消/切换、管理器停止）
    /// 时放弃并返回错误，避免退避结束后连上一个已经不要的设备。
    pub async fn connect_with_retry(
        &self,
        peripheral: &Peripheral,
        still_wanted: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<()> {
        connect_with_pacing(
            BleConnectPacing::current(),
            still_wanted,
            || self.connect(peripheral),
            || async {
                let _ = peripheral.disconnect().await;
            },
        )
        .await
    }

    /// 写入命令到 Command 特征值(5s 超时)
    pub async fn send_command(&self, data: &[u8]) -> Result<()> {
        if !self.connection_is_current(self.connection_epoch()) {
            anyhow::bail!("Vendor GATT 会话已断开");
        }
        let p = self
            .peripheral
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Vendor GATT 未连接"))?;
        let chars = self
            .chars
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("GATT 特征值未发现"))?;
        let cmd_data = data.to_vec();
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);

        log::debug!(target: "gatt", "[GATT:{}] TX CMD=0x{:02X} len={}", seq, cmd_data[0], cmd_data.len());
        // FE61 只有 WRITE 属性,必须用 WithResponse(Windows 严格按属性匹配)
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            p.write(&chars.cmd, &cmd_data, WriteType::WithResponse),
        )
        .await
        .map_err(|_| anyhow::anyhow!("GATT 写入超时 5s"))?
        .map_err(|e| anyhow::anyhow!("GATT 写入失败: {}", e))
    }

    pub fn set_audio_enabled(&self, enabled: bool) {
        if self.audio_enabled.swap(enabled, Ordering::SeqCst) != enabled {
            self.audio_route_epoch.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// 发送命令并等待 Event 通道的匹配响应。断开取消整个交换，包括 ATT 写等待。
    pub async fn send_command_and_read_response(
        &self,
        expected_cmd: u8,
        data: &[u8],
        timeout_ms: u64,
    ) -> Result<Vec<u8>> {
        let epoch = self.connection_epoch();
        self.send_command_in_session(epoch, expected_cmd, data, timeout_ms)
            .await
    }

    pub(crate) async fn send_command_in_session(
        &self,
        epoch: u64,
        expected_cmd: u8,
        data: &[u8],
        timeout_ms: u64,
    ) -> Result<Vec<u8>> {
        let _write_guard = self.write_lock.lock().await;
        let closed = self.connection_closed.notified();
        tokio::pin!(closed);
        closed.as_mut().enable();
        if !self.connection_is_current(epoch) {
            anyhow::bail!("Vendor GATT 会话已断开或更换");
        }
        log::info!(
            target: "gatt",
            "[GATT] cmd_and_read: CMD=0x{:02X} len={} timeout={}ms",
            expected_cmd, data.len(), timeout_ms
        );
        let (tx, mut rx) = tokio::sync::oneshot::channel::<Vec<u8>>();
        self.pending_responses.lock().unwrap().insert(
            expected_cmd,
            PendingResponse {
                cmd: expected_cmd,
                epoch,
                tx,
            },
        );
        // Preserve an already delivered rejection response even if 60 follows
        // immediately. Admission still checks liveness before accepting success.
        let exchange = async {
            tokio::select! {
                result = self.send_command(data) => result?,
                _ = &mut closed => {
                    return rx.try_recv().map_err(|_| anyhow::anyhow!("GATT 会话已断开"));
                }
            }
            tokio::select! {
                result = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), &mut rx) => {
                    match result {
                        Ok(Ok(response)) => Ok(response),
                        Ok(Err(_)) => Err(anyhow::anyhow!("GATT 响应通道已关闭")),
                        Err(_) => Err(anyhow::anyhow!("[GATT] CMD=0x{expected_cmd:02X} 响应超时 ({timeout_ms}ms)")),
                    }
                }
                _ = &mut closed => rx.try_recv().map_err(|_| anyhow::anyhow!("GATT 会话已断开")),
            }
        };
        let result = exchange
            .await
            .or_else(|error| rx.try_recv().map_err(|_| error));
        let mut pending = self.pending_responses.lock().unwrap();
        if pending.get(&expected_cmd).is_some_and(|p| p.epoch == epoch) {
            pending.remove(&expected_cmd);
        }
        if self.connection_epoch() != epoch {
            anyhow::bail!("GATT 响应来自已结束的会话");
        }
        result
    }

    /// 启动通知监听循环(在调用方 tokio runtime 上 spawn)
    pub async fn start_notification_loop(&self) -> Result<()> {
        let p = self
            .peripheral
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Vendor GATT 未连接"))?;
        // Subscribe to the local notification stream before sending the first
        // command; merely spawning the listener leaves a first-response race.
        let mut stream = tokio::time::timeout(std::time::Duration::from_secs(1), p.notifications())
            .await
            .map_err(|_| anyhow::anyhow!("通知流创建超时"))??;
        let connection_epoch = self.connection_epoch();
        let current_epoch = self.connection_epoch.clone();
        let connection_closed = self.connection_closed.clone();
        let running = self.running.clone();
        let audio_sink = self.audio_sink.clone();
        let event_tx = self.event_tx.clone();
        let pending = self.pending_responses.clone();
        let device_disconnect_reason = self.device_disconnect_reason.clone();
        let event_uuid = protocol::EVENT_CHAR_UUID;
        let audio_uuid = protocol::AUDIO_CHAR_UUID;

        // 音频处理 channel:通知循环投递帧,独立线程解码。Full 时显式累计
        // local-drop；它与设备 sequence gap 分开上报，不能把时间线悄悄缩短。
        let (audio_tx, audio_rx) = std::sync::mpsc::sync_channel::<(Vec<u8>, u64, u64)>(64);
        // Producer-owned count attached to the first packet after the lost sequence range.
        let audio_local_drops = Arc::new(AtomicU64::new(0));
        let audio_running = running.clone();
        let decoder = EncodedAudioDecoderSink::new(audio_sink.clone(), connection_epoch);
        let decoder_enabled = self.audio_enabled.clone();
        let decoder_route_epoch = self.audio_route_epoch.clone();
        let producer_enabled = self.audio_enabled.clone();
        let producer_route_epoch = self.audio_route_epoch.clone();
        let audio_thread = std::thread::spawn(move || {
            let mut processed: u64 = 0;
            let mut active_route_epoch = None;
            while audio_running.load(Ordering::SeqCst) {
                match audio_rx.recv_timeout(std::time::Duration::from_secs(1)) {
                    Ok((data, local_drops, route_epoch)) => {
                        if !decoder_enabled.load(Ordering::SeqCst)
                            || decoder_route_epoch.load(Ordering::SeqCst) != route_epoch
                        {
                            continue;
                        }
                        if active_route_epoch != Some(route_epoch) {
                            decoder.reset_sequence();
                            active_route_epoch = Some(route_epoch);
                        }
                        let packet = protocol::parse_audio_packet_v1(&data).or_else(|| {
                            protocol::parse_audio_packet(&data).map(|(_, payload)| {
                                crate::kernel::audio::EncodedAudioPacket {
                                    payload,
                                    transport: crate::kernel::audio::AudioTransport::BleGatt,
                                    sequence: None,
                                    device_discontinuity: false,
                                }
                            })
                        });
                        if let Some(packet) = packet {
                            decoder.on_packet(packet, local_drops);
                            processed += 1;
                        } else {
                            log::warn!(target: "gatt", "[GATT-audio] 解析失败: data_len={}", data.len());
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            log::debug!(target: "gatt", "音频处理线程退出,共处理 {} 帧", processed);
        });
        if let Ok(mut slot) = self.audio_thread_handle.lock() {
            *slot = Some(audio_thread);
        }

        let session = NotificationSession {
            epoch: connection_epoch,
            current_epoch,
            running,
            pending,
            closed: connection_closed,
        };
        let notify_handle = tokio::spawn(async move {
            log::info!(target: "gatt", "通知监听已启动");

            let mut state = GattEventState {
                device_disconnect_reason,
                ..Default::default()
            };
            let mut notif_count: u64 = 0;
            let mut audio_count: u64 = 0;
            // 心跳:CoreBluetooth 断连后 notifications().next() 不立即返回 None,
            // 靠 is_connected() 主动探测。10s 无通知即检查。
            let heartbeat = std::time::Duration::from_secs(10);
            let mut last_notif = tokio::time::Instant::now();
            let mut last_audio_route_epoch = producer_route_epoch.load(Ordering::SeqCst);

            while session.current() {
                match session.next(&mut stream, last_notif + heartbeat).await {
                    NotificationPoll::Stopped => break,
                    NotificationPoll::Message(notification) => {
                        last_notif = tokio::time::Instant::now();
                        match notification {
                            Some(notif) => {
                                if !session.current() {
                                    break;
                                }
                                notif_count += 1;
                                if notif.uuid == audio_uuid {
                                    audio_count += 1;
                                    // Route gating happens before enqueue so packets received while
                                    // disabled cannot be decoded after a later START races with this
                                    // notification loop.
                                    if !producer_enabled.load(Ordering::SeqCst) {
                                        audio_local_drops.store(0, Ordering::SeqCst);
                                        continue;
                                    }
                                    let route_epoch = producer_route_epoch.load(Ordering::SeqCst);
                                    if route_epoch != last_audio_route_epoch {
                                        audio_local_drops.store(0, Ordering::SeqCst);
                                        last_audio_route_epoch = route_epoch;
                                    }
                                    // try_send:通道满丢帧保连接,不阻塞通知循环
                                    let pending_drops = audio_local_drops.swap(0, Ordering::SeqCst);
                                    match audio_tx.try_send((
                                        notif.value.to_vec(),
                                        pending_drops,
                                        route_epoch,
                                    )) {
                                        Ok(_) => {}
                                        Err(std::sync::mpsc::TrySendError::Full((
                                            _,
                                            before,
                                            _,
                                        ))) => {
                                            audio_local_drops.fetch_add(
                                                before.saturating_add(1),
                                                Ordering::SeqCst,
                                            );
                                            if audio_count.is_multiple_of(100) {
                                                log::warn!(target: "gatt", "[GATT] 音频帧丢弃(通道满): total={}", audio_count);
                                            }
                                        }
                                        Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                                            log::warn!(target: "gatt", "[GATT] 音频解码通道关闭,结束 BLE 会话触发重连");
                                            break;
                                        }
                                    }
                                } else if notif.uuid == event_uuid {
                                    if !session.dispatch_event(&notif.value, &mut state, &event_tx)
                                    {
                                        break;
                                    }
                                } else {
                                    log::warn!(target: "gatt", "收到未知 UUID 通知: {:?}", notif.uuid);
                                }
                            }
                            None => {
                                log::info!(target: "gatt", "通知流结束(设备断开),共 {} 通知", notif_count);
                                break;
                            }
                        }
                    }
                    NotificationPoll::Heartbeat => {
                        match tokio::time::timeout(
                            std::time::Duration::from_secs(1),
                            p.is_connected(),
                        )
                        .await
                        {
                            Ok(Ok(false)) | Ok(Err(_)) | Err(_) => {
                                log::info!(target: "gatt", "心跳:10s 无通知且设备已断开");
                                break;
                            }
                            Ok(Ok(true)) => {
                                last_notif = tokio::time::Instant::now();
                            }
                        }
                    }
                }
            }

            // 断开时把还按着的键交代清楚：只发过按下、没发过松开的话，
            // 消费方会一直以为那个键按着（语音键卡住尤其难受——录音停不下来）。
            // 与 USB 监控线程退出时的收尾对齐。
            session.finish(&mut state, &event_tx);
            log::info!(target: "gatt", "通知监听已退出");
        });
        // 保存 JoinHandle,disconnect 时 await —— 防止重连时旧 task 的 pending.clear()
        // 覆盖新 task 刚注册的 pending response(参见 disconnect 内注释)。
        if let Ok(mut slot) = self.notify_handle.lock() {
            *slot = Some(notify_handle);
        }
        Ok(())
    }

    pub(crate) async fn subscribe_audio(&self, epoch: u64) -> Result<()> {
        let closed = self.connection_closed.notified();
        tokio::pin!(closed);
        closed.as_mut().enable();
        if !self.connection_is_current(epoch) {
            anyhow::bail!("BLE 音频订阅前会话已结束");
        }
        let p = self
            .peripheral
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Vendor GATT 未连接"))?;
        let chars = self
            .chars
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("GATT 特征值未发现"))?;
        tokio::select! {
            result = subscribe_with_ccc_fallback(&p, &chars.audio, "Audio") => result?,
            _ = &mut closed => anyhow::bail!("BLE 音频订阅期间会话已结束"),
        }
        if !self.connection_is_current(epoch) {
            anyhow::bail!("BLE 音频订阅结果来自已结束的会话");
        }
        Ok(())
    }

    pub(crate) fn connection_epoch(&self) -> u64 {
        self.connection_epoch.load(Ordering::SeqCst)
    }

    pub(crate) fn connection_is_current(&self, epoch: u64) -> bool {
        self.connection_epoch() == epoch
            && self.running.load(Ordering::SeqCst)
            && self.device_disconnect_reason.load(Ordering::SeqCst) == 0
    }

    pub(crate) fn device_disconnect_reason(&self, epoch: u64) -> Option<u8> {
        if self.connection_epoch() != epoch {
            return None;
        }
        let reason = self.device_disconnect_reason.load(Ordering::SeqCst);
        (reason != 0).then_some(reason)
    }

    pub(crate) async fn connection_is_active(&self, epoch: u64) -> bool {
        if !self.connection_is_current(epoch) {
            return false;
        }
        let p = self.peripheral.lock().unwrap().clone();
        let Some(p) = p else {
            return false;
        };
        let connected = matches!(
            tokio::time::timeout(std::time::Duration::from_secs(1), p.is_connected()).await,
            Ok(Ok(true))
        );
        connected && self.connection_is_current(epoch)
    }

    /// 断开连接(含 3s 超时保护,避免 disconnect().await 卡死)
    pub async fn disconnect(&self) -> Result<()> {
        self.running.store(false, Ordering::SeqCst);
        self.connection_closed.notify_waiters();

        let p = self.peripheral.lock().unwrap().take();
        let c = self.chars.lock().unwrap().take();
        self.pending_responses.lock().unwrap().clear();

        if let (Some(p), Some(c)) = (p, c) {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(3), async {
                let _ = p.unsubscribe(&c.event).await;
                let _ = p.unsubscribe(&c.audio).await;
                let _ = p.disconnect().await;
            })
            .await;
            log::info!(target: "gatt", "disconnect 完成(含超时保护)");
        }

        // 等通知 task 真正退出,避免重连场景下两个 task 短时并存:
        // 旧 task 的尾部 `pending.clear()` 会清掉新 task 刚注册的 pending response,
        // 导致新会话的命令交互超时。task 自身已有 heartbeat + is_connected 兜底,
        // 但设备断开不一定让 `notifications().next()` 立即返回 None,这里加 3s 超时保护。
        let notify_handle = self.notify_handle.lock().unwrap().take();
        if let Some(mut h) = notify_handle {
            if tokio::time::timeout(std::time::Duration::from_secs(3), &mut h)
                .await
                .is_err()
            {
                // Dropping a timed-out JoinHandle detaches the old listener. Abort
                // and join it before allowing any replacement session.
                h.abort();
                let _ = h.await;
            }
        }

        // join 音频线程(它在 running=false 后最多 1s 退出,join 同样加超时保护)。
        // 不 join 会悬挂线程到 recv_timeout 超时;join 让资源释放可预测。
        let audio_handle = self.audio_thread_handle.lock().unwrap().take();
        if let Some(h) = audio_handle {
            let _ = h.join();
        }

        Ok(())
    }

    #[allow(dead_code)]
    pub fn is_connected(&self) -> bool {
        let _p = self.peripheral.lock().unwrap();
        self.running.load(Ordering::SeqCst)
    }

    pub fn running(&self) -> Arc<AtomicBool> {
        self.running.clone()
    }

    /// 取走设备主动断开通知(0x60)的原因码;None=本连接未收到过。
    ///
    /// 取走即清零,供 hotplug 在 BLE 会话结束时一次性判定断开原因。
    pub fn take_device_disconnect_reason(&self) -> Option<u8> {
        let reason = self.device_disconnect_reason.swap(0, Ordering::SeqCst);
        (reason != 0).then_some(reason)
    }
}

// ============ 辅助 ============

/// subscribe 失败时手动写 CCC 描述符 (0x2902) 启用通知
async fn subscribe_with_ccc_fallback(
    p: &Peripheral,
    char: &btleplug::api::Characteristic,
    label: &str,
) -> Result<()> {
    log::info!(target: "gatt", "启用 {} 通知...", label);
    match tokio::time::timeout(std::time::Duration::from_secs(3), p.subscribe(char)).await {
        Ok(Ok(())) => {
            log::info!(target: "gatt", "{} 订阅成功", label);
            Ok(())
        }
        result => {
            log::warn!(target: "gatt", "{} subscribe 失败 {:?},尝试手动写 CCC", label, result);
            let ccc_desc = char
                .descriptors
                .iter()
                .find(|d| d.uuid == CCC_DESCRIPTOR_UUID);
            match ccc_desc {
                Some(desc) => {
                    let ccc: [u8; 2] = [0x01, 0x00];
                    p.write_descriptor(desc, &ccc)
                        .await
                        .map_err(|e| anyhow::anyhow!("{} CCC 写入失败: {}", label, e))?;
                    log::info!(target: "gatt", "{} 手动 CCC 写入成功", label);
                    Ok(())
                }
                None => Err(anyhow::anyhow!(
                    "{} 特征值无 CCC 描述符 (0x2902),无法启用通知",
                    label
                )),
            }
        }
    }
}

/// 处理 Event 通知 → 命令响应/状态变更
fn handle_event(
    data: &[u8],
    state: &mut GattEventState,
    event_tx: &broadcast::Sender<BoardEvent>,
    pending: &Arc<Mutex<HashMap<u8, PendingResponse>>>,
) {
    let Some((cmd, _len, payload)) = protocol::parse_packet(data) else {
        log::warn!(target: "gatt", "[GATT] Event 解析失败: data_len={}", data.len());
        let _ = event_tx.send(BoardEvent::Error(ErrorEvent {
            message: format!("GATT Event 解析失败(len={})", data.len()),
            recoverable: true,
        }));
        return;
    };

    // 1. 命令响应(匹配 pending map)
    {
        let mut pending_guard = pending.lock().unwrap();
        if let Some(pending_resp) = pending_guard.remove(&cmd) {
            let mut response = vec![cmd, _len];
            response.extend_from_slice(payload);
            let _ = pending_resp.tx.send(response); // oneshot send
            return;
        }
    }

    // 2. 异步事件,按 CMD 分发
    match cmd {
        #[cfg(feature = "test-mode")]
        CMD_AI_FACTORY_KEY_EVENT => {
            if let Ok(event) = parse_factory_key_event_unscoped(data) {
                let _ = event_tx.send(BoardEvent::FactoryKey(event));
            }
        }
        // Consumer 键事件:[CMD=0x0C][LEN=2][key_low][key_high]
        0x0C => {
            if payload.len() >= 2 {
                let key_value = ((payload[1] as u16) << 8) | (payload[0] as u16);

                // 按下/松开事件交给账本 + 聚合器，语义与 USB 完全一致：
                // 转旋钮不会顶掉按住的键，真正的松手才清空。
                let frame = state.consumer.on_frame(key_value, Instant::now());
                let mode_change = state.mode.on_frame(key_value, frame.cleared);
                let aggregator = state
                    .aggregator
                    .get_or_insert_with(|| KeyStateAggregator::new(event_tx.clone()));
                for batch in frame.batches {
                    aggregator.report_change(KeySource::Gatt, batch, None);
                }

                if is_ai_voice_consumer_code(key_value) {
                    state.ai_voice_pressed = true;
                    let _ =
                        event_tx.send(BoardEvent::AiVoiceKey(AiVoiceKeyEvent { pressed: true }));
                }

                // 与 USB 共用同一套拨杆状态机，传输差异到这里已经结束。
                if let Some((mode_value, mode_name)) = mode_change {
                    let _ = event_tx.send(BoardEvent::ModeChange(ModeChangeEvent {
                        mode: mode_name.to_string(),
                        mode_value,
                        source: ModeSource::Dial,
                    }));
                }

                // 下面两件事只在账本**真正清空**时做。挂在字面的 0x0000 上是不对的：
                // 紧跟旋钮那一格的收尾帧也是 0x0000，按住语音键转旋钮会因此误报一次释放。
                if frame.cleared && state.ai_voice_pressed {
                    state.ai_voice_pressed = false;
                    let _ =
                        event_tx.send(BoardEvent::AiVoiceKey(AiVoiceKeyEvent { pressed: false }));
                }
            }
        }
        CMD_AUDIO_DATA => {
            // Audio 偶尔经 Event 通道到达,实际音频走 FE63
            log::debug!(target: "gatt", "[GATT] Event 通道收到 Audio 数据");
        }
        CMD_STATUS => {
            // 模式变更:[CMD=0x12][LEN][SUB=0xC9][MODE]
            if payload.len() >= 2 && payload[0] == CMD_WORK_MODE_DATA {
                let mode_value = payload[1];
                if let Some(mode) = WorkMode::from_u8(mode_value) {
                    let _ = event_tx.send(BoardEvent::ModeChange(ModeChangeEvent {
                        mode: mode.display_name().to_string(),
                        mode_value,
                        source: ModeSource::Dial,
                    }));
                }
            }
        }
        CMD_DEVICE_DISCONNECT => {
            // 设备主动断开(闲置休眠/长按断开):记录原因码供断开上报区分
            // 「固件主动断开」与信号丢失(DeviceDisconnect vs DeviceGone)。
            // 是否重连由 hotplug 按原因码决定:reason=0x03(已绑定其他电脑)停止重连,
            // 其余仍按异常断开走自动重连(设备休眠唤醒/重启后会重新广播);
            // 不在此置 ble_auto_connect=false —— 那会导致设备关机重启后无法自动重连。
            // hotplug 通过扫描失败上限兜底永久离开。
            let reason = payload.first().copied().unwrap_or(0).max(1);
            state
                .device_disconnect_reason
                .store(reason, Ordering::SeqCst);
            log::info!(
                target: "gatt",
                "[GATT] 收到设备主动断开通知 (CMD=0x60, reason=0x{reason:02X}),断开上报将标记 DeviceDisconnect"
            );
        }
        _ => {
            log::debug!(target: "gatt", "[GATT] 未处理 Event CMD=0x{:02X}", cmd);
        }
    }
}

#[cfg(test)]
mod name_and_pacing_tests {
    use super::*;

    #[test]
    fn target_matches_either_advertised_or_local_name() {
        // WinRT connect 后 local_name 被改成系统名，广播名仍是 REAI_VB_*。
        assert_eq!(
            match_vendor_name(
                Some("ReAI Board"),
                Some("REAI_VB_1A2B3C4D"),
                Some("REAI_VB_1A2B3C4D")
            ),
            Some("REAI_VB_1A2B3C4D".into())
        );
        assert_eq!(
            match_vendor_name(Some("REAI_VB_1A2B3C4D"), None, Some("REAI_VB_1A2B3C4D")),
            Some("REAI_VB_1A2B3C4D".into())
        );
        assert_eq!(
            match_vendor_name(
                Some("REAI_VB_1A2B3C4D"),
                Some("REAI_VB_1A2B3C4D"),
                Some("REAI_VB_FFFFFFFF")
            ),
            None
        );
        assert_eq!(
            match_vendor_name(None, None, Some("REAI_VB_1A2B3C4D")),
            None
        );
    }

    #[test]
    fn prefix_scan_prefers_the_advertised_name() {
        assert_eq!(
            match_vendor_name(Some("Keyboard"), Some("REAI_VB_00C0FFEE"), None),
            Some("REAI_VB_00C0FFEE".into())
        );
        assert_eq!(
            match_vendor_name(Some("REAI_VB_00C0FFEE"), Some("Other"), None),
            Some("REAI_VB_00C0FFEE".into())
        );
        assert_eq!(match_vendor_name(Some("Mouse"), Some("Other"), None), None);
    }

    #[test]
    fn pacing_backoff_doubles_and_is_capped() {
        let pacing = BleConnectPacing {
            pre_connect_delay: std::time::Duration::ZERO,
            connect_timeout: std::time::Duration::from_secs(20),
            attempts: 3,
            first_backoff: std::time::Duration::from_secs(1),
        };
        assert_eq!(pacing.backoff_after(1), std::time::Duration::from_secs(1));
        assert_eq!(pacing.backoff_after(2), std::time::Duration::from_secs(2));
        assert_eq!(
            pacing.backoff_after(100),
            std::time::Duration::from_secs(16)
        );
    }

    #[test]
    fn non_windows_pacing_keeps_the_previous_behaviour() {
        let pacing = BleConnectPacing::current();
        if cfg!(target_os = "windows") {
            assert_eq!(pacing.attempts, 3);
            assert_eq!(
                pacing.pre_connect_delay,
                std::time::Duration::from_millis(500)
            );
        } else {
            assert_eq!(pacing.attempts, 1);
            assert!(pacing.pre_connect_delay.is_zero());
            assert_eq!(pacing.connect_timeout, std::time::Duration::from_secs(10));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn final_connect_failure_cleans_up_the_partial_link() {
        let calls = Mutex::new(Vec::new());
        let pacing = BleConnectPacing {
            pre_connect_delay: std::time::Duration::ZERO,
            attempts: 1,
            ..BleConnectPacing::current()
        };
        let error = connect_with_pacing(
            pacing,
            &|| true,
            || async {
                calls.lock().unwrap().push("connect");
                Err(anyhow::anyhow!("service discovery timed out"))
            },
            || async {
                calls.lock().unwrap().push("disconnect");
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "service discovery timed out");
        assert_eq!(*calls.lock().unwrap(), ["connect", "disconnect"]);
    }

    #[tokio::test(start_paused = true)]
    async fn hung_final_cleanup_is_bounded_and_preserves_the_connect_error() {
        let start = tokio::time::Instant::now();
        let pacing = BleConnectPacing {
            pre_connect_delay: std::time::Duration::ZERO,
            attempts: 1,
            ..BleConnectPacing::current()
        };
        let error = connect_with_pacing(
            pacing,
            &|| true,
            || async { Err(anyhow::anyhow!("event subscription failed")) },
            std::future::pending,
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "event subscription failed");
        assert_eq!(start.elapsed(), std::time::Duration::from_secs(3));
    }

    #[tokio::test(start_paused = true)]
    async fn retry_cleans_up_before_reconnecting_and_keeps_a_successful_link() {
        let calls = Mutex::new(Vec::new());
        let start = tokio::time::Instant::now();
        let pacing = BleConnectPacing {
            pre_connect_delay: std::time::Duration::ZERO,
            attempts: 3,
            first_backoff: std::time::Duration::from_secs(1),
            ..BleConnectPacing::current()
        };
        connect_with_pacing(
            pacing,
            &|| true,
            || async {
                let mut calls = calls.lock().unwrap();
                calls.push("connect");
                if calls.len() == 1 {
                    Err(anyhow::anyhow!("first attempt failed"))
                } else {
                    Ok(())
                }
            },
            || async {
                calls.lock().unwrap().push("disconnect");
            },
        )
        .await
        .unwrap();
        assert_eq!(*calls.lock().unwrap(), ["connect", "disconnect", "connect"]);
        assert_eq!(start.elapsed(), std::time::Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::protocol_hid::AI_VOICE_MEDIA_CODE;

    /// 非阻塞 drain broadcast,避免 blocking_recv 阻塞测试
    fn drain(rx: &mut broadcast::Receiver<BoardEvent>) -> Vec<BoardEvent> {
        let mut v = Vec::new();
        while let Ok(e) = rx.try_recv() {
            v.push(e);
        }
        v
    }

    /// GATT 包格式 `[CMD=0x0C][LEN=2][lo][hi]`。
    fn key_frame(key_value: u16) -> Vec<u8> {
        vec![
            0x0C,
            0x02,
            (key_value & 0xFF) as u8,
            ((key_value >> 8) & 0xFF) as u8,
        ]
    }

    fn notification_session() -> NotificationSession {
        NotificationSession {
            epoch: 1,
            current_epoch: Arc::new(AtomicU64::new(1)),
            running: Arc::new(AtomicBool::new(true)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            closed: Arc::new(tokio::sync::Notify::new()),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn explicit_disconnect_cancels_pending_and_releases_held_voice_immediately() {
        let session = notification_session();
        let (tx, mut events) = broadcast::channel(16);
        let mut state = GattEventState::default();
        assert!(session.dispatch_event(&key_frame(AI_VOICE_MEDIA_CODE), &mut state, &tx));
        drain(&mut events);
        let (response_tx, mut response_rx) = tokio::sync::oneshot::channel();
        session.pending.lock().unwrap().insert(
            0x74,
            PendingResponse {
                cmd: 0x74,
                epoch: 1,
                tx: response_tx,
            },
        );

        assert!(!session.dispatch_event(&[0x60, 1, 3], &mut state, &tx));
        assert!(!session.current());
        assert_eq!(state.device_disconnect_reason.load(Ordering::SeqCst), 3);
        assert!(matches!(
            response_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ));
        assert!(drain(&mut events).iter().any(|event| matches!(
            event,
            BoardEvent::AiVoiceKey(AiVoiceKeyEvent { pressed: false })
        )));
    }

    #[tokio::test]
    async fn identity_rejection_response_survives_an_immediate_reason_03_close() {
        let session = notification_session();
        let (tx, _events) = broadcast::channel(8);
        let mut state = GattEventState::default();
        let (response_tx, mut response_rx) = tokio::sync::oneshot::channel();
        session.pending.lock().unwrap().insert(
            0x74,
            PendingResponse {
                cmd: 0x74,
                epoch: 1,
                tx: response_tx,
            },
        );
        assert!(session.dispatch_event(&[0x74, 1, 1], &mut state, &tx));
        assert!(!session.dispatch_event(&[0x60, 1, 3], &mut state, &tx));
        assert_eq!(response_rx.try_recv().unwrap(), vec![0x74, 1, 1]);
        assert_eq!(state.device_disconnect_reason.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn closing_a_silent_stream_wakes_before_heartbeat_and_preserves_release_cleanup() {
        let session = notification_session();
        let (tx, mut events) = broadcast::channel(16);
        let mut state = GattEventState::default();
        session.dispatch_event(&key_frame(AI_VOICE_MEDIA_CODE), &mut state, &tx);
        drain(&mut events);
        let mut stream = futures_util::stream::pending();
        let now = tokio::time::Instant::now();
        let close = async {
            tokio::task::yield_now().await;
            session.running.store(false, Ordering::SeqCst);
            session.closed.notify_waiters();
        };
        let (poll, ()) = tokio::join!(
            session.next(&mut stream, now + std::time::Duration::from_secs(10)),
            close
        );
        assert!(matches!(poll, NotificationPoll::Stopped));
        assert_eq!(tokio::time::Instant::now(), now);
        session.finish(&mut state, &tx);
        assert!(drain(&mut events).iter().any(|event| matches!(
            event,
            BoardEvent::AiVoiceKey(AiVoiceKeyEvent { pressed: false })
        )));
    }

    #[tokio::test]
    async fn stale_notification_loop_cannot_clear_new_pending_or_end_new_session() {
        let session = notification_session();
        let (tx, mut events) = broadcast::channel(8);
        let mut state = GattEventState::default();
        let (response_tx, mut response_rx) = tokio::sync::oneshot::channel();
        session.current_epoch.store(2, Ordering::SeqCst);
        session.pending.lock().unwrap().insert(
            0x74,
            PendingResponse {
                cmd: 0x74,
                epoch: 2,
                tx: response_tx,
            },
        );
        assert!(!session.dispatch_event(&[0x60, 1, 3], &mut state, &tx));
        session.finish(&mut state, &tx);
        assert!(session.running.load(Ordering::SeqCst));
        assert!(matches!(
            response_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        assert!(drain(&mut events).is_empty());
        assert_eq!(state.device_disconnect_reason.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn device_disconnect_notify_records_reason() {
        let (tx, _rx) = broadcast::channel(4);
        let pending = Arc::new(Mutex::new(HashMap::new()));

        // 收到 0x60 带原因码:[CMD=0x60][LEN=1][reason=0x02 闲置休眠]
        let mut state = GattEventState::default();
        handle_event(&[0x60, 0x01, 0x02], &mut state, &tx, &pending);
        assert_eq!(state.device_disconnect_reason.load(Ordering::SeqCst), 2);

        // 原因码缺失(LEN=0)也置标志(归一为 1),保证仍能区分主动断开
        let mut state = GattEventState::default();
        handle_event(&[0x60, 0x00], &mut state, &tx, &pending);
        assert_eq!(state.device_disconnect_reason.load(Ordering::SeqCst), 1);
    }

    #[cfg(feature = "test-mode")]
    #[test]
    fn gatt_monitor_emits_factory_physical_event() {
        let (tx, mut rx) = broadcast::channel(4);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let mut state = GattEventState::default();
        let frame = [
            CMD_AI_FACTORY_KEY_EVENT,
            0x06,
            0x01,
            0x34,
            0x12,
            0x06,
            0x01,
            0x09,
        ];

        handle_event(&frame, &mut state, &tx, &pending);
        match rx.try_recv().expect("factory event") {
            BoardEvent::FactoryKey(event) => {
                assert_eq!(event.session, 0x1234);
                assert_eq!(event.input_index, 6);
                assert!(event.pressed);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    /// 从事件流里挑出按键事件（忽略 ComboKey 之类的旁支）。
    fn key_events(rx: &mut broadcast::Receiver<BoardEvent>) -> Vec<(usize, bool)> {
        drain(rx)
            .into_iter()
            .filter_map(|event| match event {
                BoardEvent::KeyPress(key) => Some((key.key_index, key.pressed)),
                _ => None,
            })
            .collect()
    }

    fn mode_events(rx: &mut broadcast::Receiver<BoardEvent>) -> Vec<(String, u8)> {
        drain(rx)
            .into_iter()
            .filter_map(|event| match event {
                BoardEvent::ModeChange(mode) => Some((mode.mode, mode.mode_value)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn ble_consumer_endpoint_press_and_release_emit_yolo_then_chat() {
        let (tx, mut rx) = broadcast::channel(16);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let mut state = GattEventState::default();

        handle_event(&key_frame(0x0F0A), &mut state, &tx, &pending);
        assert_eq!(mode_events(&mut rx), vec![("YOLO".to_string(), 1)]);

        handle_event(&key_frame(0x0000), &mut state, &tx, &pending);
        assert_eq!(mode_events(&mut rx), vec![("CHAT".to_string(), 0)]);
    }

    /// 验证 key_index=0(KEY0,音量A相)的释放事件不会被吞掉。
    ///
    /// 回归背景:`last_key_index` 曾用 `usize=0` 兼做"未按下"哨兵,
    /// 而 `key_index=0` 是合法键——按下时 last_key_index=0,释放时 `> 0` 判 false
    /// 导致 KEY0 永远停在 pressed:true。
    ///
    /// 现在 KEY0/KEY1 被当作旋钮脉冲：一帧之内就进出一次（按下+松开），
    /// 所以"释放不会丢"这条不变量仍然成立，只是不再依赖后续那帧 0x0000。
    #[test]
    fn key0_pulse_emits_both_press_and_release() {
        let (tx, mut rx) = broadcast::channel(64);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let mut state = GattEventState::default();

        // KEY0 = consumer 0x0F07 → key_index 0
        handle_event(&key_frame(0x0F07), &mut state, &tx, &pending);
        assert_eq!(
            key_events(&mut rx),
            vec![(0, true), (0, false)],
            "KEY0 的按下与释放都必须发出——释放丢失正是当初修的 bug"
        );

        // 紧跟其后的 0x0000 是这一格的收尾，不该再产生任何按键事件
        handle_event(&key_frame(0x0000), &mut state, &tx, &pending);
        assert!(key_events(&mut rx).is_empty(), "脉冲收尾不该重复发释放");
    }

    /// 蓝牙下「按住 Tab → 转旋钮 → 松手」必须发出 Tab 的松开事件。
    ///
    /// 回归背景（2026-07-28 真机实测）：BLE 与 USB 走同一条单值 Consumer 流，
    /// 而这里原来只记得住「最后按下的那个键」。转旋钮时这个记录被旋钮顶掉，
    /// 随后的 0x0000 把释放算在了旋钮头上——**Tab 的松手事件永远不会发出**，
    /// 上层按住 Tab 打开的应用切换器就一直卡着，只能等 30 秒硬超时强制放开 Command。
    /// 日志原文：「按键编排会话超过 30s 仍未收到抬起（键=Tab）」。
    #[test]
    fn holding_tab_through_knob_turns_still_reports_the_release() {
        let (tx, mut rx) = broadcast::channel(64);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let mut state = GattEventState::default();

        // 按住 Tab（0x0F01 → key_index 3）
        handle_event(&key_frame(0x0F01), &mut state, &tx, &pending);
        assert_eq!(key_events(&mut rx), vec![(3, true)]);

        // 连转两格旋钮（0x0F08 → key_index 1），每格后面跟一帧收尾
        for _ in 0..2 {
            handle_event(&key_frame(0x0F08), &mut state, &tx, &pending);
            assert_eq!(
                key_events(&mut rx),
                vec![(1, true), (1, false)],
                "旋钮该敲一下就走"
            );
            handle_event(&key_frame(0x0000), &mut state, &tx, &pending);
            assert!(
                key_events(&mut rx).is_empty(),
                "脉冲收尾不能把按住的 Tab 判成松开"
            );
        }

        // 松手：Tab 的释放必须发出来
        handle_event(&key_frame(0x0000), &mut state, &tx, &pending);
        assert_eq!(
            key_events(&mut rx),
            vec![(3, false)],
            "松手后必须发出 Tab 的释放——缺了它，上层按住的修饰键永远放不掉"
        );
    }

    /// 按住语音键转旋钮，不能误报一次语音键释放（会把录音打断）。
    #[test]
    fn holding_voice_key_through_a_knob_turn_does_not_report_release() {
        let (tx, mut rx) = broadcast::channel(64);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let mut state = GattEventState::default();

        handle_event(&key_frame(AI_VOICE_MEDIA_CODE), &mut state, &tx, &pending);
        let _ = drain(&mut rx);

        handle_event(&key_frame(0x0F08), &mut state, &tx, &pending);
        handle_event(&key_frame(0x0000), &mut state, &tx, &pending);
        let voice_events: Vec<bool> = drain(&mut rx)
            .into_iter()
            .filter_map(|event| match event {
                BoardEvent::AiVoiceKey(voice) => Some(voice.pressed),
                _ => None,
            })
            .collect();
        assert!(
            voice_events.is_empty(),
            "转旋钮期间不该报语音键释放：{voice_events:?}"
        );
        assert!(state.ai_voice_pressed, "语音键仍应处于按住状态");

        // 真松手才释放
        handle_event(&key_frame(0x0000), &mut state, &tx, &pending);
        let released = drain(&mut rx)
            .into_iter()
            .any(|event| matches!(event, BoardEvent::AiVoiceKey(voice) if !voice.pressed));
        assert!(released, "真松手必须报语音键释放");
    }

    /// 断连时必须把还按着的键交代成松开，别让消费方一直以为它按着。
    ///
    /// 状态本身随通知任务销毁，所以「不清也不会串到下一次连接」——但消费方那边不会
    /// 自动忘记：只收到过按下、没收到松开的键会永远停在按下。语音键卡住最难受，
    /// 录音停不下来。USB 监控线程退出时有同样的收尾，这里对齐。
    #[test]
    fn disconnect_releases_everything_still_held() {
        let (tx, mut rx) = broadcast::channel(64);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let mut state = GattEventState::default();

        // 按住语音键，再按住 Tab（单值通道下第二个键是追加）
        handle_event(&key_frame(AI_VOICE_MEDIA_CODE), &mut state, &tx, &pending);
        handle_event(&key_frame(0x0F01), &mut state, &tx, &pending);
        let _ = drain(&mut rx);

        // 设备断开：通知任务退出前的收尾
        state.release_all(&tx);

        let events = drain(&mut rx);
        let released: Vec<usize> = events
            .iter()
            .filter_map(|event| match event {
                BoardEvent::KeyPress(key) if !key.pressed => Some(key.key_index),
                _ => None,
            })
            .collect();
        assert!(
            released.contains(&6) && released.contains(&3),
            "断连必须补发所有按住键的松开，实际：{released:?}"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, BoardEvent::AiVoiceKey(voice) if !voice.pressed)),
            "断连必须补发语音键释放，否则录音停不下来"
        );
        assert!(!state.ai_voice_pressed);
    }

    /// 锁住 CCC UUID 的字节序:必须等于标准 Bluetooth Base UUID `00002902-...-00805f9b34fb`。
    ///
    /// 回归背景:旧代码的 `|| Uuid::from_u128(0x2902)` 分支永远不匹配标准 CCC UUID,
    /// 若哪天有人把主分支删了改成 `from_u128(0x2902)`,CCC fallback 会静默失效。
    #[test]
    fn ccc_descriptor_uuid_matches_bluetooth_spec() {
        assert_eq!(
            CCC_DESCRIPTOR_UUID.to_string(),
            "00002902-0000-1000-8000-00805f9b34fb"
        );
    }
}
