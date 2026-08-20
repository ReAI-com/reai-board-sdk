//! BoardDevice 高级门面 —— 消费者主入口。
//!
//! 持有 [`BoardDeviceCore`] (async 内核),对外提供:
//! - **三层事件门面**:[`events()`](BoardDevice::events) / [`on_event()`](BoardDevice::on_event)
//!   / [`subscribe()`](BoardDevice::subscribe)(兼容)共用单一 broadcast 内核,不双写
//! - **async 命令**:`read_device_info` 等直接委托 core
//! - **同步状态查询**:`connection` / `is_connected` / `set_pcm_sink` 等(无 IO)
//! - **impl Drop 兜底**:消费者忘调 `shutdown()` 时自动发停信号(不 block_on,见下)
//!
//! 共享模型:`Arc<BoardDeviceCore>`,`start` 要求 `&Arc<Self>`(内部 clone 给回调)。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use crate::kernel::event::{BoardEvent, DeviceInfo};
use crate::kernel::protocol_hid::{KeyConfig, WorkMode};
use crate::kernel::sink::{AudioFrameSink, PcmSink};
use crate::kernel::types::ConnectionType;
use crate::runtime::device::BoardDeviceCore;
use crate::runtime::hotplug::HotplugConfig;

use super::events::{EventStream, EventStreamError};

/// Board 设备配置
#[derive(Debug, Clone, Default)]
pub struct BoardConfig {
    pub hotplug: HotplugConfig,
}

/// Board 设备高级门面(USB + BLE 统一入口)。
///
/// 消费者典型用法:
///
/// ```no_run
/// # use reai_board_sdk::{BoardConfig, BoardDevice, BoardEvent};
/// # #[tokio::main] async fn main() -> anyhow::Result<()> {
/// let device = BoardDevice::open(BoardConfig::default())?;
/// device.start().await?;          // 后台自动连接 USB/BLE + 断线重连
///
/// let mut events = device.events();
/// while let Ok(Some(evt)) = events.recv().await {
///     match evt {
///         BoardEvent::Connection(c) => { /* connected/type/reason */ }
///         BoardEvent::KeyPress(k) => { /* key_index 0-11, pressed */ }
///         _ => {}
///     }
/// }
/// device.shutdown();
/// # Ok(())
/// # }
/// ```
pub struct BoardDevice {
    core: Arc<BoardDeviceCore>,
    event_tx: broadcast::Sender<BoardEvent>,
    /// Drop 兜底用:是否需要清理(start 过就置 true)
    started: AtomicBool,
    /// 当前工作模式事件快照；None 表示未连接或尚未初始化。
    current_work_mode: Arc<Mutex<Option<WorkMode>>>,
    mode_tracker_started: AtomicBool,
    /// 当前 DFU 升级的取消标志（每次 start_dfu_upgrade 重置；cancel_dfu_upgrade 读它）
    dfu_cancel: Mutex<Option<Arc<AtomicBool>>>,
}

impl BoardDevice {
    /// 打开设备(构造 async 内核,**不自建 runtime**)。
    ///
    /// 不启动监听 —— 调 [`start()`](Self::start) 后才开始热插拔自动连接。
    /// 命令/事件消费需在 tokio runtime 上下文(async 命令要 await,start 要 tokio::spawn)。
    pub fn open(config: BoardConfig) -> anyhow::Result<Self> {
        let core = Arc::new(BoardDeviceCore::new(config.hotplug)?);
        let event_tx = core.event_sender().clone();
        Ok(Self {
            core,
            event_tx,
            started: AtomicBool::new(false),
            current_work_mode: Arc::new(Mutex::new(None)),
            mode_tracker_started: AtomicBool::new(false),
            dfu_cancel: Mutex::new(None),
        })
    }

    // ================================================================
    // 三层事件门面(共用单一 broadcast 内核)
    // ================================================================

    /// 【门面 1,推荐】事件流(async Stream + blocking_recv + recv)。
    ///
    /// 包装底层 broadcast::Receiver,三种消费方式见 [`EventStream`]。
    pub fn events(&self) -> EventStream {
        EventStream::new(self.event_tx.subscribe())
    }

    /// 【门面 2】回调监听(贴 setListener 初衷,JS 风格消费者用)。
    ///
    /// 内部 tokio::spawn 一个 task 消费 [`events()`](Self::events) 调 cb。
    /// 返回 [`EventListenerHandle`],**drop 即停止回调**(取消 spawn)。
    /// 注意:cb 在 tokio runtime 上执行;若 cb 需阻塞 IO,请在 cb 内部 spawn_blocking。
    pub fn on_event<F>(&self, cb: F) -> EventListenerHandle
    where
        F: Fn(BoardEvent) + Send + Sync + 'static,
    {
        let cb = Arc::new(cb);
        let mut stream = self.events();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        tokio::spawn(async move {
            while !stop_clone.load(Ordering::SeqCst) {
                match stream.recv().await {
                    Ok(Some(evt)) => cb(evt),
                    Ok(None) => break,
                    Err(EventStreamError::Lagged(n)) => {
                        log::warn!(target: "board", "on_event 消费溢出,跳过 {} 条", n);
                        continue;
                    }
                }
            }
        });
        EventListenerHandle { stop }
    }

    /// Lower-level access to the underlying [`broadcast::Receiver`].
    ///
    /// Equivalent to `events()`, exposed for legacy consumers that prefer the
    /// raw broadcast API. New code should use [`events()`](Self::events).
    pub fn subscribe(&self) -> broadcast::Receiver<BoardEvent> {
        self.event_tx.subscribe()
    }

    // ================================================================
    // 生命周期
    // ================================================================

    /// 启动热插拔自动连接 + 断线重连(async;幂等)。
    pub async fn start(&self) -> anyhow::Result<()> {
        self.start_mode_tracker();
        // BoardDeviceCore::start 要求 &Arc<Self>;此处 core 就是 Arc,直接调
        self.core.start().await?;
        self.started.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// 停止(断开 + 停 hotplug + 停 USB Audio)。
    /// 调用 core 的同步 shutdown(纯 AtomicBool + stop,不需 await)。
    pub fn shutdown(&self) {
        self.core.shutdown();
        *self.current_work_mode.lock().unwrap() = None;
        self.started.store(false, Ordering::SeqCst);
    }

    /// 优雅停止：先上报 App 下线（best-effort），再 shutdown。
    ///
    /// 退出程序前应调用此方法（而非直接 `shutdown()`），让固件知道 App 已下线。
    /// 下线命令失败不影响 shutdown 执行。
    pub async fn shutdown_with_offline(&self) {
        if let Err(e) = self.core.notify_app_online(false).await {
            log::warn!(target: "board", "[app-online] shutdown 前自动上报下线失败: {}", e);
        }
        self.shutdown();
    }

    /// 主动断开当前连接(BLE 真断 + 关自动重连;USB 返回 Err 提示拔线)。
    pub async fn disconnect(&self) -> anyhow::Result<()> {
        self.core.disconnect().await
    }

    // ================================================================
    // async 命令(委托 core)
    // ================================================================

    /// 读设备信息(CMD 0x13):mode / MAC / 固件版本 / 电量 / chip_id
    pub async fn read_device_info(&self) -> anyhow::Result<DeviceInfo> {
        self.core.read_device_info().await
    }

    /// 读按键配置(CMD 0x15)
    pub async fn read_key_config(&self) -> anyhow::Result<KeyConfig> {
        self.core.read_key_config().await
    }

    /// 读取键盘里的绑定配置块（CMD 0x69，配置跟键盘走）。
    ///
    /// 旧固件不回包 → [`crate::kernel::bindings_blob::BlobRead::Unsupported`]，
    /// 调用方据此降级（同步不可用，其余功能不受影响）。
    pub async fn read_bindings_blob(
        &self,
    ) -> anyhow::Result<crate::kernel::bindings_blob::BlobRead> {
        self.core.read_bindings_blob().await
    }

    /// 写入绑定配置块（CMD 0x6A：分片 → commit → 回读校验）。
    pub async fn write_bindings_blob(
        &self,
        payload: &[u8],
    ) -> std::result::Result<(), crate::kernel::bindings_blob::BlobWriteError> {
        self.core.write_bindings_blob(payload).await
    }

    /// 写按键配置(CMD 0x16)
    pub async fn write_key_config(&self, config: &KeyConfig) -> anyhow::Result<()> {
        self.core.write_key_config(config).await
    }

    /// 读取固件持久化的静默录音标志（CMD 0x61）。
    pub async fn get_silent_record(&self) -> anyhow::Result<bool> {
        self.core.get_silent_record().await
    }

    /// 设置并持久化静默录音标志（CMD 0x62），返回固件最终生效值。
    pub async fn set_silent_record(&self, enable: bool) -> anyhow::Result<bool> {
        self.core.set_silent_record(enable).await
    }

    /// 进入/续租/退出工厂物理按键测试模式（固件 v1.58+）。
    #[cfg(feature = "test-mode")]
    pub async fn set_factory_key_test(
        &self,
        enable: bool,
        session: u16,
    ) -> anyhow::Result<crate::FactoryKeyControlAck> {
        self.core.set_factory_key_test(enable, session).await
    }

    /// 读取软休眠超时（CMD 0x63），返回未连接 / 已连接两组秒数。
    pub async fn get_sleep_timeout(&self) -> anyhow::Result<crate::kernel::types::SleepTimeout> {
        self.core.get_sleep_timeout().await
    }

    /// 设置并持久化软休眠超时（CMD 0x64），返回固件钳制后的生效值。
    pub async fn set_sleep_timeout(
        &self,
        timeout: crate::kernel::types::SleepTimeout,
    ) -> anyhow::Result<crate::kernel::types::SleepTimeout> {
        self.core.set_sleep_timeout(timeout).await
    }

    /// 上报 App 在线状态（CMD 0x65）。
    ///
    /// SDK 连接成功后会自动发 `online=true`；通常无需手动调用。
    /// 退出程序前用 `shutdown_with_offline()` 自动发 `online=false`。
    pub async fn notify_app_online(&self, online: bool) -> anyhow::Result<()> {
        self.core.notify_app_online(online).await
    }

    pub async fn query_audio_capabilities(
        &self,
    ) -> anyhow::Result<crate::kernel::audio::AudioCapabilities> {
        self.core.query_audio_capabilities().await
    }

    pub async fn control_audio_stream(
        &self,
        action: crate::kernel::audio::AudioStreamAction,
        transport: crate::kernel::audio::AudioTransport,
        scope: crate::kernel::audio::AudioStreamScope,
        lease_id: u32,
        ttl_ms: u16,
    ) -> anyhow::Result<crate::kernel::audio::AudioStreamState> {
        self.core
            .control_audio_stream(action, transport, scope, lease_id, ttl_ms)
            .await
    }

    pub async fn start_board_audio_reader(
        &self,
        transport: crate::kernel::audio::AudioTransport,
    ) -> anyhow::Result<()> {
        self.core.start_board_audio_reader(transport).await
    }

    pub async fn start_board_audio(
        &self,
        transport: crate::kernel::audio::AudioTransport,
        scope: crate::kernel::audio::AudioStreamScope,
        lease_id: u32,
        ttl_ms: u16,
    ) -> anyhow::Result<crate::kernel::audio::AudioStreamState> {
        self.core
            .start_board_audio(transport, scope, lease_id, ttl_ms)
            .await
    }

    pub fn start_legacy_ble_session_reader(&self) -> anyhow::Result<()> {
        self.core.start_legacy_ble_session_reader()
    }

    pub fn start_usb_uac_compat(&self) -> anyhow::Result<()> {
        self.core.start_usb_uac_compat()
    }

    pub fn stop_local_audio_reader(&self) {
        self.core.stop_local_audio_reader();
    }

    /// 查询 App 在线状态（CMD 0x66）。
    pub async fn get_app_online(&self) -> anyhow::Result<bool> {
        self.core.get_app_online().await
    }

    /// 获取离线开网页 URL（CMD 0x67）。
    pub async fn get_open_url(&self) -> anyhow::Result<String> {
        self.core.get_open_url().await
    }

    /// 设置并持久化离线开网页 URL（CMD 0x68），返回固件最终生效值。
    pub async fn set_open_url(&self, url: &str) -> anyhow::Result<String> {
        self.core.set_open_url(url).await
    }

    /// 读取当前工作模式（CMD 0x12/0xC9）。
    ///
    /// 返回 0/1/2 = CHAT/YOLO/PLAN。**不持久化**，每次调用都向设备实时查询。
    ///
    /// 注意：连接建立后 SDK 会自动 spawn 一次 best-effort 查询，把结果通过
    /// `BoardEvent::ModeChange(ModeSource::Connection)` 推到事件流，
    /// `current_work_mode()` 快照随即被填充；通常无需手动调用此方法。
    pub async fn get_work_mode(&self) -> anyhow::Result<crate::WorkMode> {
        self.core.get_work_mode().await
    }

    /// 启动固件 OTA 升级（USB-only，自定义 HID DFU 协议；详见 [`crate::dfu`]）。
    ///
    /// 阻塞直到升级流程结束（成功或失败）。进度通过 `on_progress` 回调上报，
    /// 回调在 HID 阻塞线程上触发，**只做轻量操作**（如发送 channel 消息）。
    ///
    /// 升级过程中可调 [`cancel_dfu_upgrade`](Self::cancel_dfu_upgrade) 中断。
    ///
    /// 仅 USB 连接可用，BLE 或未连接返回 `Err`。
    #[cfg(feature = "usb")]
    pub async fn start_dfu_upgrade<P>(
        &self,
        firmware_path: std::path::PathBuf,
        on_progress: P,
    ) -> anyhow::Result<()>
    where
        P: Fn(crate::dfu::DfuProgress) + Send + Sync + 'static,
    {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        *self.dfu_cancel.lock().unwrap() = Some(cancel_flag.clone());
        let progress: crate::dfu::client::ProgressCallback = Arc::new(on_progress);
        let result = self
            .core
            .dfu_upgrade(firmware_path, progress, cancel_flag)
            .await;
        // 升级结束（无论成败）清掉 cancel_flag
        *self.dfu_cancel.lock().unwrap() = None;
        result
    }

    /// 取消正在进行的 DFU 升级（若有）。无升级在进行时为 no-op。
    ///
    /// 内部把 cancel_flag 置 true，DATA 循环下次检查（≤250B 传输周期）即终止，
    /// 并尝试发 END 复位设备（重启回旧固件）。
    #[cfg(feature = "usb")]
    pub fn cancel_dfu_upgrade(&self) {
        if let Some(flag) = self.dfu_cancel.lock().unwrap().as_ref() {
            log::info!(target: "board", "[dfu] facade: 收到取消请求");
            flag.store(true, Ordering::SeqCst);
        }
    }

    /// 设备是否卡在 DFU 模式（PID 0xFF06，键盘功能全部失效的状态）。
    ///
    /// **不要求设备已连接** —— 卡在 DFU 时正常 PID 根本不存在，
    /// 调用方也不该先做连接检查，否则永远问不到答案。
    #[cfg(feature = "usb")]
    pub async fn is_stuck_in_dfu(&self) -> anyhow::Result<bool> {
        self.core.is_stuck_in_dfu().await
    }

    /// 救砖：把卡在 DFU 模式的设备踢回正常模式（发 PREPARE+END 触发校验失败重启）。
    ///
    /// 设备带**原固件**重启，暂存分区数据被丢弃，主应用分区全程不受影响。
    /// 返回 [`RecoveryOutcome`](crate::dfu::RecoveryOutcome) 三态；`StillStuck`
    /// 表示序列已发出但设备没回来，需用户物理重插 USB。
    ///
    /// **不要求设备已连接**，理由同 [`is_stuck_in_dfu`](Self::is_stuck_in_dfu)。
    #[cfg(feature = "usb")]
    pub async fn recover_from_dfu(&self) -> anyhow::Result<crate::dfu::RecoveryOutcome> {
        self.core.recover_stuck_dfu().await
    }

    /// 关机(CMD 0x5E,工厂测试)。keep_pair=true 保留 BLE 配对关机。
    #[cfg(feature = "test-mode")]
    pub async fn shutdown_device(&self, keep_pair: bool) -> anyhow::Result<()> {
        self.core.shutdown_device(keep_pair).await
    }

    // ================================================================
    // BLE 手动控制(需 start 后;不破坏自动连接主线)
    // ================================================================

    /// 扫描列出周围所有 Vendor GATT 设备(REAI_VB_ 前缀)。
    ///
    /// 需 start() 后调(复用 adapter)。扫满 timeout 返回去重列表。纯扫描,不连接。
    #[cfg(feature = "ble")]
    pub async fn scan_ble_devices(
        &self,
        timeout: std::time::Duration,
    ) -> anyhow::Result<Vec<crate::runtime::ble::gatt_client::BleDeviceInfo>> {
        self.core.scan_ble_devices(timeout).await
    }

    /// 手动连接指定 BLE 设备(按 scan_ble_devices 返回的 name)。
    ///
    /// 实现:set_ble_target(name) + set_auto_reconnect(true),hotplug 下一轮即连该设备。
    /// 若当前已连别的设备,会先由 hotplug 切换。返回后实际连接异步发生,监听 Connection 事件确认。
    #[cfg(feature = "ble")]
    pub fn connect_ble(&self, name: &str) {
        self.core.set_ble_target(Some(name));
        self.core.set_auto_reconnect(true);
    }

    /// 手动断开当前 BLE 连接 + 停止自动重连(等价 disconnect,语义明确)。
    #[cfg(feature = "ble")]
    pub async fn disconnect_ble(&self) -> anyhow::Result<()> {
        self.core.disconnect().await
    }

    // ================================================================
    // 同步状态查询 / 配置(无 IO,委托 core)
    // ================================================================

    /// 当前连接类型(None = 未连接)
    pub fn connection(&self) -> Option<ConnectionType> {
        self.core.connection()
    }

    pub fn is_connected(&self) -> bool {
        self.core.is_connected()
    }

    pub fn auto_reconnect(&self) -> bool {
        self.core.auto_reconnect()
    }

    pub fn audio_capabilities(&self) -> crate::kernel::audio::AudioCapabilities {
        self.core.audio_capabilities()
    }

    /// Return whether board-audio capabilities are unqueried, unavailable, or ready.
    pub fn audio_capability_state(&self) -> crate::kernel::audio::AudioCapabilityState {
        self.core.audio_capability_state()
    }

    pub fn active_audio_transport(&self) -> Option<crate::kernel::audio::AudioTransport> {
        self.core.active_audio_transport()
    }

    /// 当前工作模式快照。
    ///
    /// 仅由固件 `CMD_STATUS / 0xC9` 或模式键产生的
    /// [`BoardEvent::ModeChange`] 更新；尚未收到或未连接时返回 None。
    pub fn current_work_mode(&self) -> Option<WorkMode> {
        if !self.is_connected() {
            return None;
        }
        *self.current_work_mode.lock().unwrap()
    }

    /// 设置 PCM sink(板载 mSBC 经 EncodedAudioDecoderSink 解码后送;UAC 兼容路径直送)。须在 start 前调。
    pub fn set_pcm_sink(&self, sink: Arc<dyn PcmSink>) {
        self.core.set_pcm_sink(sink);
    }

    /// 设置 BLE 原始 mSBC 帧 sink(优先于 pcm_sink,不解码)。须在 start 前调。
    pub fn set_audio_frame_sink(&self, sink: Arc<dyn AudioFrameSink>) {
        self.core.set_audio_frame_sink(sink);
    }

    /// 设置 BLE 目标设备名(None = 清除,停止 BLE 自动重连)。可在 start 后调。
    #[cfg(feature = "ble")]
    pub fn set_ble_target(&self, name: Option<&str>) {
        self.core.set_ble_target(name);
    }

    /// 当前记忆的 BLE 目标设备名。
    #[cfg(feature = "ble")]
    pub fn ble_target(&self) -> Option<String> {
        self.core.ble_target()
    }

    /// 设置是否自动重连 BLE(手动断开时置 false)
    #[cfg(feature = "ble")]
    pub fn set_auto_reconnect(&self, on: bool) {
        self.core.set_auto_reconnect(on);
    }

    fn start_mode_tracker(&self) {
        if self.mode_tracker_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut rx = self.event_tx.subscribe();
        let current = self.current_work_mode.clone();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(BoardEvent::Connection(event)) => {
                        let mut mode = current.lock().unwrap();
                        update_mode_for_connection(&mut mode, event.connected);
                    }
                    Ok(BoardEvent::ModeChange(event)) => {
                        if let Some(mode) = work_mode_from_event_name(&event.mode) {
                            *current.lock().unwrap() = Some(mode);
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
}

fn work_mode_from_event_name(name: &str) -> Option<WorkMode> {
    match name {
        "CHAT" => Some(WorkMode::Chat),
        "YOLO" => Some(WorkMode::Yolo),
        "PLAN" => Some(WorkMode::Plan),
        _ => None,
    }
}

fn update_mode_for_connection(mode: &mut Option<WorkMode>, connected: bool) {
    // 连接本身不能证明工作模式；只在断开时清空已观测状态。
    if !connected {
        *mode = None;
    }
}

impl Drop for BoardDevice {
    fn drop(&mut self) {
        // 兜底:消费者忘调 shutdown() 时,Drop 内尽力清理。
        // 不能 block_on / await(Rust 禁止 Drop 里阻塞),所以只发停信号:
        //  - core.shutdown() 是同步操作(AtomicBool + UsbAudioCapture::stop),安全
        //  - BLE 后台 task 由 hotplug stop_flag 自然退出(最坏 ~10s,等当前 scan/connect/sleep 完成)
        //
        // ⚠️ 注意:Drop 后 hotplug task 可能还存活几秒(持 Arc<BoardDeviceCore>),
        //    BLE 连接不会立即断。若需"立即释放"(如重启 board 场景),应在 Drop 前
        //    显式调 disconnect().await + 等待 Connection(connected=false) 事件。
        if self.started.swap(false, Ordering::SeqCst) {
            log::warn!(target: "board", "BoardDevice Drop 但未显式 shutdown,自动清理");
            self.core.shutdown();
        }
    }
}

/// `on_event` 返回的句柄,**drop 即停止回调**(取消内部 spawn 的消费 task)。
///
/// `#[must_use]`:忽略返回值时 handle 立即 drop,回调永远不会触发——
/// 这是常见的 API 误用陷阱(`device.on_event(|e| ...);` 漏接返回值)。
/// 必须把 handle 绑定到变量(`let _h = device.on_event(...);`)让它活到回调生命周期。
#[must_use = "handle drop 即停止回调,忽略返回值会导致回调永不触发"]
pub struct EventListenerHandle {
    stop: Arc<AtomicBool>,
}

impl Drop for EventListenerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::{update_mode_for_connection, work_mode_from_event_name};
    use crate::kernel::protocol_hid::WorkMode;

    #[test]
    fn work_mode_snapshot_accepts_semantic_event_names() {
        assert_eq!(work_mode_from_event_name("CHAT"), Some(WorkMode::Chat));
        assert_eq!(work_mode_from_event_name("YOLO"), Some(WorkMode::Yolo));
        assert_eq!(work_mode_from_event_name("PLAN"), Some(WorkMode::Plan));
        assert_eq!(work_mode_from_event_name("2"), None);
    }

    #[test]
    fn connection_event_does_not_invent_or_reset_mode() {
        let mut unknown = None;
        update_mode_for_connection(&mut unknown, true);
        assert_eq!(unknown, None);

        let mut mode = Some(WorkMode::Plan);
        update_mode_for_connection(&mut mode, true);
        assert_eq!(mode, Some(WorkMode::Plan));

        update_mode_for_connection(&mut mode, false);
        assert_eq!(mode, None);
    }
}
