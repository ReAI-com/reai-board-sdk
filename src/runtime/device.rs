//! BoardDeviceCore —— async 编排内核。
//!
//! 装配 HotplugManager + VendorGattClient,提供 async 命令路径。
//! 持有调用方 tokio runtime(通过 `tokio::runtime::Handle::current()` 拿),
//! **不自建 BLE runtime**。
//!
//! 这是**内部内核**:消费者用 facade 层的 BoardDevice,它持有 `Arc<BoardDeviceCore>`。
//!
//! 共享模型:`Arc<BoardDeviceCore>`,HotplugManager 的回调用 `Arc::clone` 捕获,
//! 跨 `tokio::spawn` 存活。故 `start` / `disconnect` 等需 spawn 的方法接收 `self: &Arc<Self>`。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use tokio::sync::broadcast;

use crate::kernel::audio::{
    AudioCapabilities, AudioStreamAction, AudioStreamScope, AudioStreamState, AudioTransport,
};
use crate::kernel::event::{
    BoardEvent, ConnectionEvent, DeviceInfo, DisconnectReason, ModeChangeEvent, ModeSource,
};
use crate::kernel::protocol_hid::*;
use crate::kernel::sink::{AudioFrameSink, CountingSink, PcmAudioFrameAdapter, PcmSink};
use crate::kernel::types::ConnectionType;
use crate::runtime::hotplug::{spawn_blocking_with_runloop, HotplugConfig, HotplugManager};
use crate::tool::parse::{parse_device_info_from_buf, parse_device_info_from_gatt};

#[cfg(feature = "usb")]
use {
    crate::runtime::usb::device_manager::DeviceConnection,
    crate::runtime::usb_capture::UsbAudioCapture,
    crate::runtime::usb_hid_audio::UsbVendorAudioReader,
};

#[cfg(feature = "ble")]
use crate::runtime::ble::gatt_client::VendorGattClient;
#[cfg(feature = "ble")]
use btleplug::platform::Adapter;

/// BoardDeviceCore(async 编排内核)
///
/// 消费者不直接用,通过 `Arc<BoardDeviceCore>` 由 facade::BoardDevice 持有。
pub struct BoardDeviceCore {
    event_tx: broadcast::Sender<BoardEvent>,
    hotplug_config: HotplugConfig,
    hotplug_stop: Mutex<Option<Arc<AtomicBool>>>,
    started: AtomicBool,
    /// 当前连接类型(hotplug on_connection_change 回写)
    connection_type: Mutex<Option<ConnectionType>>,
    /// 命令交互共享(hotplug on_monitor_ready 回写,USB 用)
    #[cfg(feature = "usb")]
    config_conn: Mutex<Option<Arc<Mutex<Option<DeviceConnection>>>>>,
    #[cfg(feature = "usb")]
    monitor_paused: Mutex<Option<Arc<AtomicBool>>>,

    #[cfg(feature = "ble")]
    cached_adapter: Arc<Mutex<Option<Adapter>>>,
    #[cfg(feature = "ble")]
    ble_target: Arc<Mutex<Option<String>>>,
    #[cfg(feature = "ble")]
    ble_auto_connect: Arc<AtomicBool>,
    /// BLE GATT 客户端（延迟创建）。
    ///
    /// 这里**不在 start() 时创建**——创建 adapter 会触发 macOS 蓝牙授权弹窗，
    /// 在用户没决定要用蓝牙之前弹它体验差。改成共享槽（`Arc<Mutex>`），让 hotplug 的 BLE
    /// 路径和 scan_ble_devices 在首次真正需要时通过 [`ensure_vendor_gatt_client`] 延迟建。
    #[cfg(feature = "ble")]
    vendor_gatt_client: Arc<Mutex<Option<Arc<VendorGattClient>>>>,

    /// BLE 音频:原始 mSBC 帧 sink(优先,不解码)
    audio_frame_sink: Mutex<Option<Arc<dyn AudioFrameSink>>>,
    /// PCM sink(USB Audio + BLE mSBC 解码后统一到 f32)
    pcm_sink: Mutex<Option<Arc<dyn PcmSink>>>,
    #[cfg(feature = "usb")]
    usb_capture: Mutex<Option<UsbAudioCapture>>,
    #[cfg(feature = "usb")]
    usb_hid_audio: Mutex<Option<UsbVendorAudioReader>>,
    audio_capabilities: Mutex<AudioCapabilities>,
    active_audio_transport: Mutex<Option<AudioTransport>>,
    audio_stream_state: Mutex<Option<AudioStreamState>>,
    audio_connection_epoch: AtomicU64,
}

impl BoardDeviceCore {
    /// 构造(async 内核,**不自建 runtime**)。
    /// adapter 预热推迟到 `start()` 用调用方 runtime spawn。
    pub fn new(config: HotplugConfig) -> Result<Self> {
        let (event_tx, _) = broadcast::channel(256);
        Ok(Self {
            event_tx,
            hotplug_config: config,
            hotplug_stop: Mutex::new(None),
            started: AtomicBool::new(false),
            connection_type: Mutex::new(None),
            #[cfg(feature = "usb")]
            config_conn: Mutex::new(None),
            #[cfg(feature = "usb")]
            monitor_paused: Mutex::new(None),
            #[cfg(feature = "ble")]
            cached_adapter: Arc::new(Mutex::new(None)),
            #[cfg(feature = "ble")]
            ble_target: Arc::new(Mutex::new(None)),
            // 默认 false：不随 App 启动自动连 BLE（也不建 adapter）。
            // 仅当用户主动 set_ble_target / scan_ble / connect_ble 后才翻 true。
            #[cfg(feature = "ble")]
            ble_auto_connect: Arc::new(AtomicBool::new(false)),
            #[cfg(feature = "ble")]
            vendor_gatt_client: Arc::new(Mutex::new(None)),
            audio_frame_sink: Mutex::new(None),
            pcm_sink: Mutex::new(None),
            #[cfg(feature = "usb")]
            usb_capture: Mutex::new(None),
            #[cfg(feature = "usb")]
            usb_hid_audio: Mutex::new(None),
            audio_capabilities: Mutex::new(AudioCapabilities::default()),
            active_audio_transport: Mutex::new(None),
            audio_stream_state: Mutex::new(None),
            audio_connection_epoch: AtomicU64::new(0),
        })
    }

    /// 事件广播 sender(给 facade::BoardDevice 用,它要 expose events())
    pub fn event_sender(&self) -> &broadcast::Sender<BoardEvent> {
        &self.event_tx
    }

    // ================================================================
    // sink / BLE 配置(同步,无 IO;须在 start 前调)
    // ================================================================

    /// 设置 PCM sink(板载 mSBC 经 EncodedAudioDecoderSink 解码后送;UAC 兼容路径直送)
    pub fn set_pcm_sink(&self, sink: Arc<dyn PcmSink>) {
        *self.pcm_sink.lock().unwrap() = Some(sink);
    }

    /// 设置解码后的 AudioFrame sink(优先于 pcm_sink,额外带传输与连续性信息)
    pub fn set_audio_frame_sink(&self, sink: Arc<dyn AudioFrameSink>) {
        *self.audio_frame_sink.lock().unwrap() = Some(sink);
    }

    /// 设置 BLE 目标设备名(None = 清除,停止 BLE 自动重连)。可在 start 后调。
    #[cfg(feature = "ble")]
    pub fn set_ble_target(&self, name: Option<&str>) {
        *self.ble_target.lock().unwrap() = name.map(String::from);
    }

    /// 当前记忆的 BLE 目标设备名。
    #[cfg(feature = "ble")]
    pub fn ble_target(&self) -> Option<String> {
        self.ble_target.lock().unwrap().clone()
    }

    /// 设置是否自动重连 BLE(手动断开时置 false)
    #[cfg(feature = "ble")]
    pub fn set_auto_reconnect(&self, on: bool) {
        self.ble_auto_connect.store(on, Ordering::SeqCst);
    }

    /// 扫描列出周围所有 Vendor GATT 设备(REAI_VB_ 前缀)。
    ///
    /// **要求 start() 后调用**。这是「用户主动点击才触发 BLE」的入口之一——首次调用会
    /// 通过 `ensure_vendor_gatt_client` 延迟建 adapter（此时才触发 macOS 蓝牙授权弹窗）。
    /// 扫满 `timeout`,返回去重后的设备列表(名/MAC/RSSI)。纯扫描,不连接。
    /// 注意:扫描期间 hotplug 若也在扫描会共享同一次 start_scan(btleplug 幂等),互不干扰。
    #[cfg(feature = "ble")]
    pub async fn scan_ble_devices(
        &self,
        timeout: std::time::Duration,
    ) -> Result<Vec<crate::runtime::ble::gatt_client::BleDeviceInfo>> {
        let client = self.ensure_vendor_gatt_client().await?;
        client.scan_all_vendor_devices(timeout).await
    }

    // ================================================================
    // 状态查询(同步,无 IO)
    // ================================================================

    /// 当前连接类型
    pub fn connection(&self) -> Option<ConnectionType> {
        *self.connection_type.lock().unwrap()
    }

    pub fn is_connected(&self) -> bool {
        self.connection().is_some()
    }

    pub fn audio_capabilities(&self) -> AudioCapabilities {
        *self.audio_capabilities.lock().unwrap()
    }

    pub fn active_audio_transport(&self) -> Option<AudioTransport> {
        *self.active_audio_transport.lock().unwrap()
    }

    /// 当前是否启用 BLE 自动重连
    pub fn auto_reconnect(&self) -> bool {
        #[cfg(feature = "ble")]
        {
            self.ble_auto_connect.load(Ordering::SeqCst)
        }
        #[cfg(not(feature = "ble"))]
        {
            true
        }
    }

    // ================================================================
    // 生命周期
    // ================================================================

    /// 启动热插拔自动连接 + 断线重连(async;幂等)。
    ///
    /// 接收 `self: &Arc<Self>` —— HotplugManager 回调要 `Arc::clone` 捕获,
    /// 跨 `tokio::spawn` 存活。须在 tokio runtime 上下文调用。
    pub async fn start(self: &Arc<Self>) -> Result<()> {
        if self.started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        // 先存 stop_flag,确保 shutdown 能立即生效(避免下方 await 期间
        // 并发 shutdown 找不到 hotplug_stop 导致 task 泄漏)。
        let stop_flag = Arc::new(AtomicBool::new(true));
        *self.hotplug_stop.lock().unwrap() = Some(stop_flag.clone());

        // BLE adapter **不再在 start 时创建**——创建会触发 macOS 蓝牙授权弹窗。
        // vendor_gatt_client 保持 None，由 hotplug 的 BLE 路径和 scan_ble_devices 在
        // 首次真正需要时通过 ensure_vendor_gatt_client() 延迟建。

        // 组装 HotplugManager
        let mut hotplug = HotplugManager::new(self.event_tx.clone(), self.hotplug_config.clone());

        // on_connection_change:更新 connection_type。任何音频 endpoint 都只由显式
        // start_audio_stream/start_usb_uac 调用打开；连接本身绝不触发 cpal/UAC。
        let inner = self.clone();
        hotplug = hotplug.on_connection_change(Box::new(move |ct| {
            let previous = {
                let mut connection = inner.connection_type.lock().unwrap();
                let previous = *connection;
                *connection = ct;
                previous
            };
            if ct.is_some() && previous != ct {
                inner.audio_connection_epoch.fetch_add(1, Ordering::SeqCst);
            }
            if previous.is_some() && previous != ct {
                inner.stop_local_audio_reader();
                *inner.audio_stream_state.lock().unwrap() = None;
                *inner.audio_capabilities.lock().unwrap() = AudioCapabilities::default();
            }
            match ct {
                Some(ConnectionType::Usb) => {
                    // USB 连接就绪后 best-effort 查一次工作模式：
                    // monitor_paused/config_conn 已由 on_monitor_ready 设置，
                    // 直接 spawn 一个查询任务，失败只 warn 不影响连接。
                    let inner = inner.clone();
                    tokio::spawn(async move {
                        match inner.get_work_mode().await {
                            Ok(mode) => {
                                log::info!(target: "board", "[work-mode] USB 初始工作模式: {:?}", mode);
                                let _ = inner.event_tx.send(BoardEvent::ModeChange(ModeChangeEvent {
                                    mode: mode.display_name().to_string(),
                                    mode_value: mode as u8,
                                    source: ModeSource::Connection,
                                }));
                            }
                            Err(e) => {
                                log::warn!(target: "board", "[work-mode] USB 查询初始工作模式失败: {}", e);
                            }
                        }
                        // 自动上报 App 上线（best-effort，失败只 warn）
                        if let Err(e) = inner.notify_app_online(true).await {
                            log::warn!(target: "board", "[app-online] USB 自动上报上线失败: {}", e);
                        }
                    });
                }
                Some(ConnectionType::Ble) => {
                    // BLE：notify_connected 在 start_notification_loop() 之前触发，
                    // 此时 GATT notification loop 尚未启动，cmd_via_gatt 会超时。
                    // 短延迟等 loop 起来再查询。设备信息里包含 BLE 场景刚需的电量，
                    // 必须像 USB 一样广播 DeviceInfo；各项都是 best-effort，失败只 warn。
                    let inner = inner.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        match inner.read_device_info().await {
                            Ok(info) => {
                                log::info!(
                                    target: "board",
                                    "[device-info] BLE 初始设备信息: fw={} battery={}%",
                                    info.firmware_version,
                                    info.battery_level
                                );
                                let _ = inner.event_tx.send(BoardEvent::DeviceInfo(info));
                            }
                            Err(e) => {
                                log::warn!(
                                    target: "board",
                                    "[device-info] BLE 查询初始设备信息失败: {}",
                                    e
                                );
                            }
                        }
                        match inner.get_work_mode().await {
                            Ok(mode) => {
                                log::info!(target: "board", "[work-mode] BLE 初始工作模式: {:?}", mode);
                                let _ = inner.event_tx.send(BoardEvent::ModeChange(ModeChangeEvent {
                                    mode: mode.display_name().to_string(),
                                    mode_value: mode as u8,
                                    source: ModeSource::Connection,
                                }));
                            }
                            Err(e) => {
                                log::warn!(target: "board", "[work-mode] BLE 查询初始工作模式失败: {}", e);
                            }
                        }
                        // 自动上报 App 上线（best-effort，失败只 warn）
                        if let Err(e) = inner.notify_app_online(true).await {
                            log::warn!(target: "board", "[app-online] BLE 自动上报上线失败: {}", e);
                        }
                    });
                }
                _ => {
                    // 断开:释放所有本地 audio reader/capture，清 capability/route。
                    #[cfg(feature = "usb")]
                    {
                        if let Some(cap) = inner.usb_capture.lock().unwrap().take() {
                            cap.stop();
                        }
                        if let Some(reader) = inner.usb_hid_audio.lock().unwrap().take() {
                            reader.stop();
                        }
                    }
                    *inner.active_audio_transport.lock().unwrap() = None;
                    *inner.audio_stream_state.lock().unwrap() = None;
                    *inner.audio_capabilities.lock().unwrap() = AudioCapabilities::default();
                }
            }
        }));

        // on_monitor_ready:保存命令交互共享(USB only)
        #[cfg(feature = "usb")]
        {
            let inner = self.clone();
            hotplug = hotplug.on_monitor_ready(Box::new(move |conn, paused| {
                *inner.config_conn.lock().unwrap() = Some(conn);
                *inner.monitor_paused.lock().unwrap() = Some(paused);
            }));
        }

        #[cfg(feature = "ble")]
        {
            // 共享同一个 client 槽 + ensure 回调：hotplug 的 BLE 路径首次进入时延迟建
            // adapter（触发蓝牙授权），而不是 start() 时就建。
            let inner = self.clone();
            hotplug = hotplug
                .with_vendor_gatt_client_slot(self.vendor_gatt_client.clone())
                .with_ble_ensure_client(Box::new(move || {
                    let inner = inner.clone();
                    Box::pin(async move { inner.ensure_vendor_gatt_client().await })
                }))
                .with_ble_target_device_name(self.ble_target.clone())
                .with_ble_auto_connect(self.ble_auto_connect.clone());
        }

        hotplug = hotplug.with_running_flag(stop_flag);

        tokio::spawn(async move {
            hotplug.run().await;
        });

        Ok(())
    }

    /// 停止(设 stop_flag=false + 停 USB Audio)。
    /// 同步操作,不需 async;BLE 后台 task 由 hotplug 的 stop_flag 自然退出。
    pub fn shutdown(&self) {
        // 先关闭启动门，再通知/回收任务，防连接回调在清理窗口重建 capture。
        self.started.store(false, Ordering::SeqCst);
        if let Some(stop) = self.hotplug_stop.lock().unwrap().as_ref() {
            stop.store(false, Ordering::SeqCst);
        }
        #[cfg(feature = "usb")]
        {
            if let Some(cap) = self.usb_capture.lock().unwrap().take() {
                cap.stop();
            }
            if let Some(reader) = self.usb_hid_audio.lock().unwrap().take() {
                reader.stop();
            }
        }
        *self.active_audio_transport.lock().unwrap() = None;
        *self.audio_stream_state.lock().unwrap() = None;
    }

    /// 主动断开当前连接。
    /// - BLE：断开 peripheral + 置 auto_reconnect=false / 清 ble_target 防止立即重连,
    ///   发 `ConnectionEvent{reason: UserAction}`。
    /// - USB：物理连接无法主动断开，返回 Err 提示拔线。
    pub async fn disconnect(&self) -> Result<()> {
        match self.connection() {
            None => Ok(()),
            Some(ConnectionType::Usb) => Err(anyhow::anyhow!(
                "USB 为物理连接，无法主动断开（请拔出线缆）"
            )),
            Some(ConnectionType::Ble) => {
                #[cfg(feature = "ble")]
                {
                    self.set_auto_reconnect(false);
                    self.set_ble_target(None);
                    let client = { self.vendor_gatt_client.lock().unwrap().clone() };
                    if let Some(client) = client {
                        let _ = client.disconnect().await;
                    }
                    *self.connection_type.lock().unwrap() = None;
                    let _ = self.event_tx.send(BoardEvent::Connection(ConnectionEvent {
                        connected: false,
                        connection_type: None,
                        reason: Some(DisconnectReason::UserAction),
                    }));
                }
                #[cfg(not(feature = "ble"))]
                {
                    return Err(anyhow::anyhow!("BLE feature 未启用，无法断开 BLE 连接"));
                }
                Ok(())
            }
        }
    }

    // ================================================================
    // 命令(CMD 0x13/0x15/0x16/0x5E/0x61/0x62/0x63/0x64)
    // ================================================================

    /// 读设备信息(CMD 0x13):mode / MAC / 固件版本 / 电量 / chip_id
    pub async fn read_device_info(&self) -> Result<DeviceInfo> {
        let conn_type = self
            .connection()
            .ok_or_else(|| anyhow::anyhow!("设备未连接"))?;
        match conn_type {
            #[cfg(feature = "usb")]
            ConnectionType::Usb => {
                let (len, buf) = self.cmd_via_fresh_usb(HidPacket::get_device_info()).await?;
                if len < 24 {
                    return Err(anyhow::anyhow!("设备信息响应长度不足: {}", len));
                }
                if buf[1] != CMD_GET_DEVICE_INFO {
                    return Err(anyhow::anyhow!("响应 CMD 不匹配: 0x{:02X}", buf[1]));
                }
                if buf[3] != 0x00 {
                    return Err(anyhow::anyhow!("查询失败: result=0x{:02X}", buf[3]));
                }
                parse_device_info_from_buf(&buf, 4, conn_type)
            }
            #[cfg(not(feature = "usb"))]
            ConnectionType::Usb => Err(anyhow::anyhow!("usb feature 未启用")),
            ConnectionType::Ble => {
                #[cfg(feature = "ble")]
                {
                    let resp = self
                        .cmd_via_gatt(&HidPacket::get_device_info(), CMD_GET_DEVICE_INFO)
                        .await?;
                    parse_device_info_from_gatt(&resp, conn_type)
                }
                #[cfg(not(feature = "ble"))]
                {
                    let _ = conn_type;
                    Err(anyhow::anyhow!("BLE feature 未启用"))
                }
            }
        }
    }

    /// 读按键配置(CMD 0x15)
    pub async fn read_key_config(&self) -> Result<KeyConfig> {
        let conn_type = self
            .connection()
            .ok_or_else(|| anyhow::anyhow!("设备未连接"))?;
        match conn_type {
            #[cfg(feature = "usb")]
            ConnectionType::Usb => {
                let (len, buf) = self.cmd_via_fresh_usb(HidPacket::get_key_config()).await?;
                if len < 64 {
                    return Err(anyhow::anyhow!("按键配置响应长度不足: {}", len));
                }
                if buf[3] != 0x00 {
                    return Err(anyhow::anyhow!("读取失败: result=0x{:02X}", buf[3]));
                }
                let mut key_data = [0u8; KEY_DATA_LEN];
                key_data.copy_from_slice(&buf[4..64]);
                Ok(KeyConfig::from_bytes(&key_data))
            }
            #[cfg(not(feature = "usb"))]
            ConnectionType::Usb => Err(anyhow::anyhow!("usb feature 未启用")),
            ConnectionType::Ble => {
                #[cfg(feature = "ble")]
                {
                    let resp = self
                        .cmd_via_gatt(&HidPacket::get_key_config(), CMD_GET_KEY_SETTING)
                        .await?;
                    if resp.len() < 3 + KEY_DATA_LEN {
                        return Err(anyhow::anyhow!("按键配置数据长度不足: {}", resp.len()));
                    }
                    if resp[2] != 0x00 {
                        return Err(anyhow::anyhow!("读取失败: result=0x{:02X}", resp[2]));
                    }
                    let mut key_data = [0u8; KEY_DATA_LEN];
                    key_data.copy_from_slice(&resp[3..3 + KEY_DATA_LEN]);
                    Ok(KeyConfig::from_bytes(&key_data))
                }
                #[cfg(not(feature = "ble"))]
                {
                    let _ = conn_type;
                    Err(anyhow::anyhow!("BLE feature 未启用"))
                }
            }
        }
    }

    /// 写按键配置(CMD 0x16)
    pub async fn write_key_config(&self, config: &KeyConfig) -> Result<()> {
        let conn_type = self
            .connection()
            .ok_or_else(|| anyhow::anyhow!("设备未连接"))?;
        match conn_type {
            #[cfg(feature = "usb")]
            ConnectionType::Usb => {
                let (len, buf) = self
                    .cmd_via_fresh_usb(HidPacket::set_key_config(&config.to_bytes()))
                    .await?;
                if len < 4 {
                    return Err(anyhow::anyhow!("写入响应长度不足: {}", len));
                }
                if buf[3] != 0x00 {
                    return Err(anyhow::anyhow!("写入失败: result=0x{:02X}", buf[3]));
                }
                Ok(())
            }
            #[cfg(not(feature = "usb"))]
            ConnectionType::Usb => Err(anyhow::anyhow!("usb feature 未启用")),
            ConnectionType::Ble => {
                #[cfg(feature = "ble")]
                {
                    let resp = self
                        .cmd_via_gatt(
                            &HidPacket::set_key_config(&config.to_bytes()),
                            CMD_SET_KEY_SETTING,
                        )
                        .await?;
                    if resp.len() < 3 {
                        return Err(anyhow::anyhow!("写入响应长度不足"));
                    }
                    if resp[2] != 0x00 {
                        return Err(anyhow::anyhow!("写入失败: result=0x{:02X}", resp[2]));
                    }
                    Ok(())
                }
                #[cfg(not(feature = "ble"))]
                {
                    let _ = conn_type;
                    Err(anyhow::anyhow!("BLE feature 未启用"))
                }
            }
        }
    }

    /// 读取固件持久化的静默录音标志（CMD 0x61）。
    pub async fn get_silent_record(&self) -> Result<bool> {
        self.silent_record_command(HidPacket::get_silent_record(), CMD_GET_SILENT_RECORD)
            .await
    }

    /// 设置并持久化静默录音标志（CMD 0x62），返回固件最终生效值。
    pub async fn set_silent_record(&self, enable: bool) -> Result<bool> {
        self.silent_record_command(HidPacket::set_silent_record(enable), CMD_SET_SILENT_RECORD)
            .await
    }

    /// 进入/续租/退出固件 v1.58+ 的工厂物理按键测试模式。
    ///
    /// 测试模式是 15 秒易失租约；调用方应每 5 秒以相同 session 续租，并在结束时退出。
    #[cfg(feature = "test-mode")]
    pub async fn set_factory_key_test(
        &self,
        enable: bool,
        session: u16,
    ) -> Result<FactoryKeyControlAck> {
        let conn_type = self
            .connection()
            .ok_or_else(|| anyhow::anyhow!("设备未连接"))?;
        let packet = HidPacket::factory_key_test_control(enable, session)?;
        match conn_type {
            #[cfg(feature = "usb")]
            ConnectionType::Usb => {
                let (len, response) = self.cmd_via_fresh_usb(packet).await?;
                parse_factory_key_control_ack(&response[..len], session)
                    .map_err(anyhow::Error::from)
            }
            #[cfg(not(feature = "usb"))]
            ConnectionType::Usb => Err(anyhow::anyhow!("usb feature 未启用")),
            ConnectionType::Ble => {
                #[cfg(feature = "ble")]
                {
                    let response = self
                        .cmd_via_gatt(&packet, CMD_AI_FACTORY_KEY_TEST_CONTROL)
                        .await?;
                    parse_factory_key_control_ack(&response, session).map_err(anyhow::Error::from)
                }
                #[cfg(not(feature = "ble"))]
                {
                    let _ = packet;
                    Err(anyhow::anyhow!("BLE feature 未启用"))
                }
            }
        }
    }

    async fn silent_record_command(&self, packet: [u8; 64], expected_cmd: u8) -> Result<bool> {
        let conn_type = self
            .connection()
            .ok_or_else(|| anyhow::anyhow!("设备未连接"))?;
        match conn_type {
            #[cfg(feature = "usb")]
            ConnectionType::Usb => {
                let (len, response) = self.cmd_via_fresh_usb(packet).await?;
                parse_silent_record_hid_response(&response[..len], expected_cmd)
                    .ok_or_else(|| anyhow::anyhow!("静默录音响应无效或命令失败"))
            }
            #[cfg(not(feature = "usb"))]
            ConnectionType::Usb => Err(anyhow::anyhow!("usb feature 未启用")),
            ConnectionType::Ble => {
                #[cfg(feature = "ble")]
                {
                    let response = self.cmd_via_gatt(&packet, expected_cmd).await?;
                    parse_silent_record_gatt_response(&response, expected_cmd)
                        .ok_or_else(|| anyhow::anyhow!("静默录音 GATT 响应无效或命令失败"))
                }
                #[cfg(not(feature = "ble"))]
                {
                    let _ = (packet, expected_cmd);
                    Err(anyhow::anyhow!("BLE feature 未启用"))
                }
            }
        }
    }

    // ================================================================
    // 绑定配置块（CMD 0x69/0x6A，配置跟键盘走）
    // ================================================================

    /// 读取键盘里的绑定配置块（分片 + CRC 校验 + 帧解读）。
    ///
    /// 旧固件不回包 → [`BlobRead::Unsupported`](crate::kernel::bindings_blob::BlobRead::Unsupported)，调用方据此降级，不影响其余功能。
    pub async fn read_bindings_blob(&self) -> Result<crate::kernel::bindings_blob::BlobRead> {
        let mut link = self.blob_link()?;
        crate::kernel::bindings_blob::read_blob(&mut link)
            .await
            .map_err(anyhow::Error::msg)
    }

    /// 写入绑定配置块（组帧 → 分片 → commit → 回读校验）。
    pub async fn write_bindings_blob(
        &self,
        payload: &[u8],
    ) -> std::result::Result<(), crate::kernel::bindings_blob::BlobWriteError> {
        let mut link = self
            .blob_link()
            .map_err(|e| crate::kernel::bindings_blob::BlobWriteError::Transport(e.to_string()))?;
        crate::kernel::bindings_blob::write_blob(&mut link, payload).await
    }

    /// 按当前连接类型组装 blob 传输链路。
    fn blob_link(&self) -> Result<DeviceBlobLink<'_>> {
        let conn_type = self
            .connection()
            .ok_or_else(|| anyhow::anyhow!("设备未连接"))?;
        Ok(DeviceBlobLink {
            core: self,
            conn_type,
        })
    }

    /// 读取当前工作模式（CMD 0x12 + 子命令 0xC9）。
    ///
    /// 固件在 `CMD_STATUS` 分支处理：`cmd_type == CMD_WORK_MODE_DATA` 时同步回
    /// 当前工作模式，返回 0/1/2 = CHAT/YOLO/PLAN。
    ///
    /// **不持久化**：固件每次开机都重新从 GPIO 拨杆读取，所以
    /// "当前工作模式" 等于 "拨杆当前位置"。本方法用于连接建立后查询初始模式，
    /// 避免用户必须手动拨一下拨杆才能让 SDK 知道当前模式。
    pub async fn get_work_mode(&self) -> Result<WorkMode> {
        let conn_type = self
            .connection()
            .ok_or_else(|| anyhow::anyhow!("设备未连接"))?;
        log::debug!(target: "board", "[work-mode] GET (CMD 0x12/0xC9) 经 {:?}", conn_type);
        match conn_type {
            #[cfg(feature = "usb")]
            ConnectionType::Usb => {
                let (len, response) = self.cmd_via_fresh_usb(HidPacket::get_work_mode()).await?;
                let parsed = parse_work_mode_hid_response(&response[..len]);
                if parsed.is_none() {
                    log::warn!(target: "board", "[work-mode] HID 响应解析失败，原始响应: {}", fmt_hex_prefix_len(&response, len));
                }
                parsed.ok_or_else(|| anyhow::anyhow!("工作模式响应无效或命令失败"))
            }
            #[cfg(not(feature = "usb"))]
            ConnectionType::Usb => Err(anyhow::anyhow!("usb feature 未启用")),
            ConnectionType::Ble => {
                #[cfg(feature = "ble")]
                {
                    let response = self
                        .cmd_via_gatt(&HidPacket::get_work_mode(), CMD_STATUS)
                        .await?;
                    let parsed = parse_work_mode_gatt_response(&response);
                    if parsed.is_none() {
                        log::warn!(target: "board", "[work-mode] GATT 响应解析失败，原始响应: {}", fmt_hex_prefix(&response));
                    }
                    parsed.ok_or_else(|| anyhow::anyhow!("工作模式 GATT 响应无效或命令失败"))
                }
                #[cfg(not(feature = "ble"))]
                {
                    let _ = conn_type;
                    Err(anyhow::anyhow!("BLE feature 未启用"))
                }
            }
        }
    }

    /// 读取软休眠超时（CMD 0x63），返回未连接 / 已连接两组秒数。
    pub async fn get_sleep_timeout(&self) -> Result<crate::kernel::types::SleepTimeout> {
        log::info!(target: "board", "[sleep-timeout] GET (CMD 0x63) 开始");
        let res = self
            .sleep_timeout_command(HidPacket::get_sleep_timeout(), CMD_GET_SLEEP_TIMEOUT)
            .await;
        match &res {
            Ok(t) => {
                log::info!(target: "board", "[sleep-timeout] GET 成功: disconnected={}s connected={}s", t.disconnected, t.connected)
            }
            Err(e) => log::warn!(target: "board", "[sleep-timeout] GET 失败: {}", e),
        }
        res
    }

    /// 设置并持久化软休眠超时（CMD 0x64），返回固件钳制后的生效值。
    pub async fn set_sleep_timeout(
        &self,
        timeout: crate::kernel::types::SleepTimeout,
    ) -> Result<crate::kernel::types::SleepTimeout> {
        log::info!(target: "board", "[sleep-timeout] SET (CMD 0x64) 请求: disconnected={}s connected={}s", timeout.disconnected, timeout.connected);
        let res = self
            .sleep_timeout_command(HidPacket::set_sleep_timeout(timeout), CMD_SET_SLEEP_TIMEOUT)
            .await;
        match &res {
            Ok(t) => {
                log::info!(target: "board", "[sleep-timeout] SET 成功，固件生效值: disconnected={}s connected={}s", t.disconnected, t.connected)
            }
            Err(e) => log::warn!(target: "board", "[sleep-timeout] SET 失败: {}", e),
        }
        res
    }

    async fn sleep_timeout_command(
        &self,
        packet: [u8; 64],
        expected_cmd: u8,
    ) -> Result<crate::kernel::types::SleepTimeout> {
        let conn_type = self
            .connection()
            .ok_or_else(|| anyhow::anyhow!("设备未连接"))?;
        log::debug!(target: "board", "[sleep-timeout] 经 {:?} 下发 CMD 0x{:02X}", conn_type, expected_cmd);
        match conn_type {
            #[cfg(feature = "usb")]
            ConnectionType::Usb => {
                let (len, response) = self.cmd_via_fresh_usb(packet).await?;
                let parsed = parse_sleep_timeout_hid_response(&response[..len], expected_cmd);
                if parsed.is_none() {
                    log::warn!(target: "board", "[sleep-timeout] HID 响应解析失败，原始响应: {}", fmt_hex_prefix_len(&response, len));
                }
                parsed.ok_or_else(|| anyhow::anyhow!("软休眠超时响应无效或命令失败"))
            }
            #[cfg(not(feature = "usb"))]
            ConnectionType::Usb => Err(anyhow::anyhow!("usb feature 未启用")),
            ConnectionType::Ble => {
                #[cfg(feature = "ble")]
                {
                    let response = self.cmd_via_gatt(&packet, expected_cmd).await?;
                    let parsed = parse_sleep_timeout_gatt_response(&response, expected_cmd);
                    if parsed.is_none() {
                        log::warn!(target: "board", "[sleep-timeout] GATT 响应解析失败，原始响应: {}", fmt_hex_prefix(&response));
                    }
                    parsed.ok_or_else(|| anyhow::anyhow!("软休眠超时 GATT 响应无效或命令失败"))
                }
                #[cfg(not(feature = "ble"))]
                {
                    let _ = (packet, expected_cmd);
                    Err(anyhow::anyhow!("BLE feature 未启用"))
                }
            }
        }
    }

    /// 上报 App 在线状态（CMD 0x65）。
    ///
    /// App 连接成功后自动发 `online=true`；SDK shutdown 前自动发 `online=false`。
    /// 也可由上层手动调用。无需解析响应（fire-and-forget，但会等待确认）。
    pub async fn notify_app_online(&self, online: bool) -> Result<()> {
        let conn_type = self
            .connection()
            .ok_or_else(|| anyhow::anyhow!("设备未连接"))?;
        let packet = HidPacket::app_online_notify(online);
        log::debug!(target: "board", "[app-online] 经 {:?} 上报 online={}", conn_type, online);
        match conn_type {
            #[cfg(feature = "usb")]
            ConnectionType::Usb => {
                let (_len, _response) = self.cmd_via_fresh_usb(packet).await?;
            }
            #[cfg(not(feature = "usb"))]
            ConnectionType::Usb => {
                let _ = conn_type;
                return Err(anyhow::anyhow!("usb feature 未启用"));
            }
            ConnectionType::Ble => {
                #[cfg(feature = "ble")]
                {
                    let _ = self.cmd_via_gatt(&packet, CMD_AI_APP_ONLINE_NOTIFY).await?;
                }
                #[cfg(not(feature = "ble"))]
                {
                    let _ = conn_type;
                    return Err(anyhow::anyhow!("BLE feature 未启用"));
                }
            }
        }
        Ok(())
    }

    /// Query versioned board-audio capabilities. Unsupported/old firmware returns an error;
    /// callers must not infer support from being "latest" or silently fall back to UAC.
    pub async fn query_audio_capabilities(&self) -> Result<AudioCapabilities> {
        let connection = self
            .connection()
            .ok_or_else(|| anyhow::anyhow!("设备未连接"))?;
        let packet = HidPacket::get_audio_capabilities();
        let capabilities = match connection {
            #[cfg(feature = "usb")]
            ConnectionType::Usb => {
                let (length, response) = self.cmd_via_fresh_usb(packet).await?;
                parse_audio_capabilities_hid_response(&response[..length])
            }
            #[cfg(not(feature = "usb"))]
            ConnectionType::Usb => None,
            #[cfg(feature = "ble")]
            ConnectionType::Ble => {
                let response = self
                    .cmd_via_gatt(&packet, CMD_AI_GET_AUDIO_CAPABILITIES)
                    .await?;
                parse_audio_capabilities_gatt_response(&response)
            }
            #[cfg(not(feature = "ble"))]
            ConnectionType::Ble => None,
        }
        .ok_or_else(|| anyhow::anyhow!("固件不支持版本化板载音频 capability"))?;
        *self.audio_capabilities.lock().unwrap() = capabilities;
        Ok(capabilities)
    }

    /// Start/heartbeat/stop a firmware stream lease. This API supports board transports only;
    /// it cannot open CoreAudio/WASAPI endpoints.
    pub async fn control_audio_stream(
        &self,
        action: AudioStreamAction,
        transport: AudioTransport,
        scope: AudioStreamScope,
        lease_id: u32,
        ttl_ms: u16,
    ) -> Result<AudioStreamState> {
        if lease_id == 0 {
            return Err(anyhow::anyhow!("audio lease_id 不能为 0"));
        }
        let packet = HidPacket::audio_stream_control(action, transport, scope, lease_id, ttl_ms)
            .ok_or_else(|| anyhow::anyhow!("系统/UAC 输入不使用固件 stream lease"))?;
        let response = match self.connection() {
            #[cfg(feature = "usb")]
            Some(ConnectionType::Usb) => {
                let (length, response) = self.cmd_via_fresh_usb(packet).await?;
                parse_audio_stream_hid_response(&response[..length])
            }
            #[cfg(not(feature = "usb"))]
            Some(ConnectionType::Usb) => None,
            #[cfg(feature = "ble")]
            Some(ConnectionType::Ble) => {
                let response = self
                    .cmd_via_gatt(&packet, CMD_AI_AUDIO_STREAM_CONTROL)
                    .await?;
                parse_audio_stream_gatt_response(&response)
            }
            #[cfg(not(feature = "ble"))]
            Some(ConnectionType::Ble) => None,
            None => return Err(anyhow::anyhow!("设备未连接")),
        }
        .ok_or_else(|| anyhow::anyhow!("audio stream-control 响应无效"))?;
        if response.result != crate::kernel::audio::AudioStreamResult::Ok {
            if response.result == crate::kernel::audio::AudioStreamResult::LeaseMismatch
                && action != AudioStreamAction::Start
            {
                *self.audio_stream_state.lock().unwrap() = None;
                self.stop_local_audio_reader();
            }
            return Err(anyhow::anyhow!(
                "audio stream-control 被固件拒绝: {:?}",
                response.result
            ));
        }
        if !response.matches_request(action, transport, scope, lease_id) {
            return Err(anyhow::anyhow!(
                "audio stream-control 回包与请求 owner/scope/lease 不一致"
            ));
        }
        match action {
            AudioStreamAction::Start | AudioStreamAction::Heartbeat => {
                *self.audio_stream_state.lock().unwrap() = Some(response);
            }
            AudioStreamAction::Stop => {
                *self.audio_stream_state.lock().unwrap() = None;
                self.stop_local_audio_reader();
            }
        }
        Ok(response)
    }

    /// Open the selected local reader only after a successful firmware START lease.
    pub async fn start_board_audio_reader(&self, transport: AudioTransport) -> Result<()> {
        if self.active_audio_transport() == Some(transport) {
            #[cfg(feature = "usb")]
            if transport == AudioTransport::UsbVendorHid
                && self
                    .usb_hid_audio
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(UsbVendorAudioReader::is_running)
            {
                return Ok(());
            }
            #[cfg(feature = "ble")]
            if transport == AudioTransport::BleGatt {
                return Ok(());
            }
        }
        // 先校验再动传输：校验失败就直接返回，调用方看到 Err 时现有音频原封不动。
        // 反过来的话，"切到一条不支持的传输"会先把正在跑的那条拆掉再报错，
        // 调用方以为什么都没发生，实际音频已经死了、固件那边的租约还活着。
        let capabilities = self.audio_capabilities();
        if !capabilities.supports(transport) {
            return Err(anyhow::anyhow!("当前 capability 不支持 {:?}", transport));
        }
        let lease = *self.audio_stream_state.lock().unwrap();
        if lease.and_then(|state| state.active_transport) != Some(transport) {
            return Err(anyhow::anyhow!(
                "必须先为 {:?} 成功建立固件 audio stream lease",
                transport
            ));
        }
        self.stop_local_audio_reader();
        match transport {
            AudioTransport::UsbVendorHid => {
                #[cfg(feature = "usb")]
                {
                    if self.connection() != Some(ConnectionType::Usb) {
                        return Err(anyhow::anyhow!("当前不是 USB 连接"));
                    }
                    let sink = self.build_audio_sink();
                    let epoch = self.audio_connection_epoch.load(Ordering::SeqCst);
                    let reader = UsbVendorAudioReader::start(sink, epoch).await?;
                    *self.usb_hid_audio.lock().unwrap() = Some(reader);
                }
                #[cfg(not(feature = "usb"))]
                return Err(anyhow::anyhow!("usb feature 未启用"));
            }
            AudioTransport::BleGatt => {
                #[cfg(feature = "ble")]
                {
                    if self.connection() != Some(ConnectionType::Ble) {
                        return Err(anyhow::anyhow!("当前不是 BLE 连接"));
                    }
                    let client = self
                        .vendor_gatt_client
                        .lock()
                        .unwrap()
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("VendorGattClient 未就绪"))?;
                    client.set_audio_enabled(true);
                }
                #[cfg(not(feature = "ble"))]
                return Err(anyhow::anyhow!("ble feature 未启用"));
            }
            AudioTransport::UsbUac | AudioTransport::System => {
                return Err(anyhow::anyhow!("此方法只启动 Board Vendor transport"));
            }
        }
        *self.active_audio_transport.lock().unwrap() = Some(transport);
        Ok(())
    }

    /// Negotiate capability + START lease + local reader as one rollback-safe operation.
    pub async fn start_board_audio(
        &self,
        transport: AudioTransport,
        scope: AudioStreamScope,
        lease_id: u32,
        ttl_ms: u16,
    ) -> Result<AudioStreamState> {
        let capabilities = self.query_audio_capabilities().await?;
        if !capabilities.supports(transport) {
            return Err(anyhow::anyhow!("当前 capability 不支持 {:?}", transport));
        }
        let state = self
            .control_audio_stream(AudioStreamAction::Start, transport, scope, lease_id, ttl_ms)
            .await?;
        if let Err(error) = self.start_board_audio_reader(transport).await {
            let _ = self
                .control_audio_stream(AudioStreamAction::Stop, transport, scope, lease_id, ttl_ms)
                .await;
            return Err(error);
        }
        Ok(state)
    }

    /// Enable old BLE FE63 session-only delivery. It never creates a continuous lease and is
    /// intentionally unavailable for USB or timeline scope.
    pub fn start_legacy_ble_session_reader(&self) -> Result<()> {
        if self.connection() != Some(ConnectionType::Ble) {
            return Err(anyhow::anyhow!("旧 envelope 仅支持 BLE session"));
        }
        #[cfg(feature = "ble")]
        {
            self.stop_local_audio_reader();
            let client = self
                .vendor_gatt_client
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| anyhow::anyhow!("VendorGattClient 未就绪"))?;
            client.set_audio_enabled(true);
            *self.active_audio_transport.lock().unwrap() = Some(AudioTransport::BleGatt);
            Ok(())
        }
        #[cfg(not(feature = "ble"))]
        Err(anyhow::anyhow!("ble feature 未启用"))
    }

    /// Explicit UAC compatibility opt-in. Merely connecting USB or installing a PcmSink never
    /// calls this function and therefore never requests OS microphone access.
    pub fn start_usb_uac_compat(&self) -> Result<()> {
        #[cfg(feature = "usb")]
        {
            if self.connection() != Some(ConnectionType::Usb) {
                return Err(anyhow::anyhow!("当前不是 USB 连接"));
            }
            if self.audio_stream_state.lock().unwrap().is_some() {
                return Err(anyhow::anyhow!(
                    "Board audio lease 仍活跃，必须先显式 STOP 后才能打开 UAC"
                ));
            }
            self.stop_local_audio_reader();
            let pcm = self
                .pcm_sink
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| anyhow::anyhow!("尚未设置 PcmSink"))?;
            let capture = UsbAudioCapture::new(pcm);
            capture.start()?;
            *self.usb_capture.lock().unwrap() = Some(capture);
            *self.active_audio_transport.lock().unwrap() = Some(AudioTransport::UsbUac);
            Ok(())
        }
        #[cfg(not(feature = "usb"))]
        Err(anyhow::anyhow!("usb feature 未启用"))
    }

    pub fn stop_local_audio_reader(&self) {
        #[cfg(feature = "usb")]
        {
            if let Some(reader) = self.usb_hid_audio.lock().unwrap().take() {
                reader.stop();
            }
            if let Some(capture) = self.usb_capture.lock().unwrap().take() {
                capture.stop();
            }
        }
        #[cfg(feature = "ble")]
        if let Some(client) = self.vendor_gatt_client.lock().unwrap().clone() {
            client.set_audio_enabled(false);
        }
        *self.active_audio_transport.lock().unwrap() = None;
    }

    /// 查询 App 在线状态（CMD 0x66）。
    pub async fn get_app_online(&self) -> Result<bool> {
        self.app_online_query_command(HidPacket::get_app_online(), CMD_AI_GET_APP_ONLINE)
            .await
    }

    /// 获取离线开网页 URL（CMD 0x67）。
    pub async fn get_open_url(&self) -> Result<String> {
        self.open_url_command(HidPacket::get_open_url(), CMD_AI_GET_OPEN_URL)
            .await
    }

    /// 设置离线开网页 URL 并持久化（CMD 0x68），返回固件最终生效值。
    pub async fn set_open_url(&self, url: &str) -> Result<String> {
        self.open_url_command(HidPacket::set_open_url(url), CMD_AI_SET_OPEN_URL)
            .await
    }

    async fn app_online_query_command(&self, packet: [u8; 64], expected_cmd: u8) -> Result<bool> {
        let conn_type = self
            .connection()
            .ok_or_else(|| anyhow::anyhow!("设备未连接"))?;
        match conn_type {
            #[cfg(feature = "usb")]
            ConnectionType::Usb => {
                let (len, response) = self.cmd_via_fresh_usb(packet).await?;
                parse_app_online_hid_response(&response[..len], expected_cmd)
                    .ok_or_else(|| anyhow::anyhow!("App 在线状态响应无效或命令失败"))
            }
            #[cfg(not(feature = "usb"))]
            ConnectionType::Usb => Err(anyhow::anyhow!("usb feature 未启用")),
            ConnectionType::Ble => {
                #[cfg(feature = "ble")]
                {
                    let response = self.cmd_via_gatt(&packet, expected_cmd).await?;
                    parse_app_online_gatt_response(&response, expected_cmd)
                        .ok_or_else(|| anyhow::anyhow!("App 在线状态 GATT 响应无效或命令失败"))
                }
                #[cfg(not(feature = "ble"))]
                {
                    let _ = (packet, expected_cmd);
                    Err(anyhow::anyhow!("BLE feature 未启用"))
                }
            }
        }
    }

    async fn open_url_command(&self, packet: [u8; 64], expected_cmd: u8) -> Result<String> {
        let conn_type = self
            .connection()
            .ok_or_else(|| anyhow::anyhow!("设备未连接"))?;
        match conn_type {
            #[cfg(feature = "usb")]
            ConnectionType::Usb => {
                let (len, response) = self.cmd_via_fresh_usb(packet).await?;
                let parsed = parse_open_url_hid_response(&response[..len], expected_cmd);
                if parsed.is_none() {
                    log::warn!(target: "board", "[open-url] HID 响应解析失败，原始响应: {}", fmt_hex_prefix_len(&response, len));
                }
                parsed.ok_or_else(|| anyhow::anyhow!("开网页 URL 响应无效或命令失败"))
            }
            #[cfg(not(feature = "usb"))]
            ConnectionType::Usb => Err(anyhow::anyhow!("usb feature 未启用")),
            ConnectionType::Ble => {
                #[cfg(feature = "ble")]
                {
                    let response = self.cmd_via_gatt(&packet, expected_cmd).await?;
                    let parsed = parse_open_url_gatt_response(&response, expected_cmd);
                    if parsed.is_none() {
                        log::warn!(target: "board", "[open-url] GATT 响应解析失败，原始响应: {}", fmt_hex_prefix(&response));
                    }
                    parsed.ok_or_else(|| anyhow::anyhow!("开网页 URL GATT 响应无效或命令失败"))
                }
                #[cfg(not(feature = "ble"))]
                {
                    let _ = (packet, expected_cmd);
                    Err(anyhow::anyhow!("BLE feature 未启用"))
                }
            }
        }
    }

    /// 执行固件 OTA 升级（USB-only，基于自定义 HID DFU 协议）。
    ///
    /// 完整流程：暂停 monitor → 发 CMD 0xEF 进 DFU → 等设备切 PID(0xFF06) →
    /// PREPARE/START/DATA 循环 → END → 等设备重启回正常模式。
    ///
    /// **防砖**：固件写入 FOTA 暂存分区，失败/中断/取消时 host 发 END 让设备
    /// 重启回旧固件；详见 [`crate::dfu`] 模块文档。
    ///
    /// - `firmware_path`：本地 .bin 固件文件路径。
    /// - `on_progress`：进度回调（在 HID 阻塞线程上调用，仅做轻量操作如发送 channel）。
    /// - `cancel_flag`：置 true 后 DATA 循环下次检查即终止并尝试复位设备。
    ///
    /// 仅 USB 连接可用；BLE 连接或未连接返回 Err。
    #[cfg(feature = "usb")]
    pub async fn dfu_upgrade(
        self: &Arc<Self>,
        firmware_path: std::path::PathBuf,
        on_progress: crate::dfu::client::ProgressCallback,
        cancel_flag: Arc<AtomicBool>,
    ) -> Result<()> {
        use crate::dfu::{build_enter_dfu_hid_command, client::DfuClient};

        // 0. 校验连接 + 文件
        let conn_type = self
            .connection()
            .ok_or_else(|| anyhow::anyhow!("设备未连接"))?;
        if conn_type != ConnectionType::Usb {
            return Err(anyhow::anyhow!(
                "固件升级仅支持 USB 连接（当前 {:?}）",
                conn_type
            ));
        }
        let firmware = std::fs::read(&firmware_path)
            .map_err(|e| anyhow::anyhow!("读取固件文件失败 {firmware_path:?}: {e}"))?;
        let total_len = firmware.len() as u32;
        if total_len == 0 {
            return Err(anyhow::anyhow!("固件文件为空"));
        }
        log::info!(target: "board", "[dfu] runtime: 开始升级 {firmware_path:?} ({} bytes)", total_len);

        // 1. 进 DFU：发 CMD 0xEF（通过正常 USB HID 通道；cmd_via_fresh_usb 内部已 PauseGuard）
        log::info!(target: "board", "[dfu] runtime: 发送 CMD 0xEF 进入 DFU 模式");
        let enter_cmd = build_enter_dfu_hid_command();
        // 发完命令设备会立即开始重启，read 可能超时/失败 —— 这是正常的（设备已离线）
        if let Err(e) = self.cmd_via_fresh_usb(enter_cmd).await {
            log::warn!(target: "board", "[dfu] runtime: CMD 0xEF 响应未收到（设备重启中），继续等待 DFU 设备: {e}");
        }

        // 2. 跑 DfuClient::upgrade（同步阻塞 → 投到 macOS HID 专用线程）
        //    enter_dfu 闭包传 no-op：进 DFU 已在 step 1 完成
        let paused_arc = self.monitor_paused.lock().unwrap().clone();
        let firmware_arc = Arc::new(firmware);
        let progress_clone = on_progress.clone();
        let cancel_clone = cancel_flag.clone();
        let thread_result = spawn_blocking_with_runloop::<_, Result<()>>(move || {
            // 整个 upgrade 期间持续暂停 monitor（设备会切 PID，monitor 不应误判为断线抢重连）
            let _pause_guard = PauseGuard::new(paused_arc);
            let client = DfuClient::new(cancel_clone, progress_clone);
            client.upgrade(&firmware_arc, || Ok(()))
        })
        .await;
        match thread_result {
            Ok(inner) => inner,
            Err(panic) => Err(anyhow::anyhow!("DFU 线程崩溃: {panic:?}")),
        }
    }

    // ================================================================
    // DFU 救砖（把卡在 DFU 模式的设备踢回正常模式）
    // ================================================================

    /// 设备当前是否卡在 DFU 模式（PID 0xFF06）。
    ///
    /// 与「是否已连接」无关 —— 卡在 DFU 时正常 PID 不存在，`connection()` 必然为 None，
    /// 所以本方法不校验连接状态，纯扫 USB 设备列表。
    #[cfg(feature = "usb")]
    pub async fn is_stuck_in_dfu(&self) -> Result<bool> {
        spawn_blocking_with_runloop(crate::dfu::recover::scan_stuck_device)
            .await
            .map_err(|e| anyhow::anyhow!("HID 线程崩溃: {e:?}"))?
    }

    /// 救砖：发 PREPARE+END 让卡住的设备丢弃暂存分区、带原固件重启。
    ///
    /// ## 为什么编排在 runtime 而不是 facade
    ///
    /// 恢复期间设备会切 PID，必须用 `PauseGuard` 暂停 monitor 防它误判断线抢重连；
    /// 而 `monitor_paused` 是本结构体的私有字段，facade 层拿不到 —— 这也是
    /// `dfu_upgrade` 同样落在 runtime 的原因。
    ///
    /// ## 为什么要分段投递
    ///
    /// macOS 上所有 hidapi 调用都排队在同一条 HID 专用线程上。若把「发包 + 等 20 秒」
    /// 整段丢进去，热插拔探测和所有 HID 命令都会被饿死 20 秒。因此这里只把
    /// **发包**和**每一次扫描**作为短任务投递，等待本身用 `tokio::time::sleep`
    /// 在 async 层完成。
    #[cfg(feature = "usb")]
    pub async fn recover_stuck_dfu(&self) -> Result<crate::dfu::RecoveryOutcome> {
        use crate::dfu::recover::{
            scan_normal_device, scan_stuck_device, send_recovery_sequence,
            RECOVERY_POLL_INTERVAL_MS, RECOVERY_WAIT_TIMEOUT_SECS,
        };
        use crate::dfu::RecoveryOutcome;

        // 1. 先确认真卡住了 —— 没卡住就别去打扰设备。
        let stuck = spawn_blocking_with_runloop(scan_stuck_device)
            .await
            .map_err(|e| anyhow::anyhow!("HID 线程崩溃: {e:?}"))??;
        if !stuck {
            log::info!(target: "board", "[dfu-recover] 未发现 DFU 设备，无需恢复");
            return Ok(RecoveryOutcome::NotStuck);
        }

        // 2. 暂停 monitor。PauseGuard::new 内部有 20ms sleep，放 spawn_blocking 里创建，
        //    guard 本身跨 await 持有（Drop 只写一个 AtomicBool，不阻塞）。
        let paused_arc = self.monitor_paused.lock().unwrap().clone();
        let _pause_guard = tokio::task::spawn_blocking(move || PauseGuard::new(paused_arc))
            .await
            .map_err(|e| anyhow::anyhow!("暂停 monitor 失败: {e}"))?;

        // 3. 发恢复序列（短任务，约 1~2 秒）
        log::info!(target: "board", "[dfu-recover] 发送恢复序列 PREPARE+END...");
        let delivered = spawn_blocking_with_runloop(send_recovery_sequence)
            .await
            .map_err(|e| anyhow::anyhow!("恢复线程崩溃: {e:?}"))??;

        // 一个包都没写出去（设备已拔线等）就别等了 —— 干等 20 秒既拖慢错误反馈，
        // 又让上层的 DFU 忙位白白多持有 20 秒。
        if !delivered {
            log::warn!(target: "board", "[dfu-recover] 恢复包未能送达设备，跳过等待");
            return Ok(RecoveryOutcome::StillStuck);
        }

        // 4. 分段轮询等设备回正常模式
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(RECOVERY_WAIT_TIMEOUT_SECS);
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(RECOVERY_POLL_INTERVAL_MS)).await;
            let scan = spawn_blocking_with_runloop(scan_normal_device)
                .await
                .map_err(|e| anyhow::anyhow!("HID 线程崩溃: {e:?}"))?;
            let back = match scan {
                Ok(found) => found,
                // 设备重启期间 USB 枚举短暂失败是常态，继续轮询而不是中断整轮恢复；
                // 但要留下日志，否则「权限不足导致每次枚举都失败」会伪装成普通超时。
                Err(e) => {
                    log::debug!(target: "board", "[dfu-recover] 等待期间枚举失败: {e}（继续轮询）");
                    false
                }
            };
            if back {
                log::info!(target: "board", "[dfu-recover] 设备已回到正常模式");
                return Ok(RecoveryOutcome::Recovered);
            }
        }

        log::warn!(
            target: "board",
            "[dfu-recover] 等待 {RECOVERY_WAIT_TIMEOUT_SECS}s 后设备仍未回到正常模式（需物理重插 USB）"
        );
        Ok(RecoveryOutcome::StillStuck)
    }

    /// 关机(CMD 0x5E,工厂测试)。keep_pair=true 保留 BLE 配对关机。
    #[cfg(feature = "test-mode")]
    pub async fn shutdown_device(&self, keep_pair: bool) -> Result<()> {
        let conn_type = self
            .connection()
            .ok_or_else(|| anyhow::anyhow!("设备未连接"))?;
        let cmd = HidPacket::shutdown(keep_pair);
        match conn_type {
            #[cfg(feature = "usb")]
            ConnectionType::Usb => {
                self.cmd_via_fresh_usb(cmd).await?;
            }
            #[cfg(not(feature = "usb"))]
            ConnectionType::Usb => {
                let _ = (conn_type, cmd);
                return Err(anyhow::anyhow!("usb feature 未启用"));
            }
            ConnectionType::Ble => {
                #[cfg(feature = "ble")]
                {
                    self.cmd_via_gatt(&cmd, CMD_AI_SHUTDOWN).await?;
                }
                #[cfg(not(feature = "ble"))]
                {
                    let _ = conn_type;
                    return Err(anyhow::anyhow!("BLE feature 未启用"));
                }
            }
        }
        Ok(())
    }

    // ================================================================
    // 命令路径(private)
    // ================================================================

    /// USB:新建独立 HidApi 连接发命令(共享连接会让固件崩溃)。
    /// PauseGuard 暂停 monitor 防吃响应。返回 (实际读取长度, 缓冲区)。
    #[cfg(feature = "usb")]
    async fn cmd_via_fresh_usb(&self, cmd: [u8; 64]) -> Result<(usize, [u8; 64])> {
        let paused_arc = self.monitor_paused.lock().unwrap().clone();
        log::debug!(target: "board", "[USB→] 发送 HID (CMD=0x{:02X}): {}", cmd[1], fmt_hex_prefix(&cmd));
        // 用 spawn_blocking_with_runloop：hidapi 的 IOKit 调用需要 CFRunLoop。
        let result = spawn_blocking_with_runloop::<_, Result<(usize, [u8; 64])>>(move || {
            let _guard = PauseGuard::new(paused_arc);

            let api = hidapi::HidApi::new()?;
            let dev_info = api
                .device_list()
                .find(|d| {
                    d.vendor_id() == VID
                        && is_target_pid(d.product_id())
                        && d.usage_page() == USAGE_PAGE_CONFIG
                        && d.usage() == 0x0002
                })
                .or_else(|| {
                    api.device_list().find(|d| {
                        d.vendor_id() == VID
                            && is_target_pid(d.product_id())
                            && d.usage_page() == USAGE_PAGE_CONFIG
                    })
                })
                .ok_or_else(|| anyhow::anyhow!("未找到 Config 接口"))?;
            let device = api.open_path(dev_info.path())?;
            device.write(&cmd)?;
            let mut buf = [0u8; 64];
            let len = device.read_timeout(&mut buf, 3000)?;
            Ok((len, buf))
        });
        let res = result
            .await
            .map_err(|e| anyhow::anyhow!("HID 线程崩溃: {:?}", e))?;
        match &res {
            Ok((len, buf)) => {
                log::debug!(target: "board", "[USB←] 收到 HID (len={}, CMD=0x{:02X}): {}", len, buf.get(1).copied().unwrap_or(0), fmt_hex_prefix_len(buf, *len))
            }
            Err(e) => log::warn!(target: "board", "[USB✗] HID 命令失败: {}", e),
        }
        res
    }

    /// BLE GATT:hid_to_gatt_command + send_command_and_read_response
    #[cfg(feature = "ble")]
    async fn cmd_via_gatt(&self, hid_packet: &[u8; 64], expected_cmd: u8) -> Result<Vec<u8>> {
        use crate::kernel::protocol_gatt::hid_to_gatt_command;
        let client = self
            .vendor_gatt_client
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("VendorGattClient 未就绪"))?;
        let gatt_cmd = hid_to_gatt_command(hid_packet);
        log::debug!(target: "board", "[GATT→] 发送 (CMD=0x{:02X}): {}", hid_packet[1], fmt_hex_prefix(hid_packet));
        let res = client
            .send_command_and_read_response(expected_cmd, &gatt_cmd, 3000)
            .await;
        match &res {
            Ok(resp) => {
                log::debug!(target: "board", "[GATT←] 收到 (len={}, CMD=0x{:02X}): {}", resp.len(), resp.first().copied().unwrap_or(0), fmt_hex_prefix(resp))
            }
            Err(e) => {
                log::warn!(target: "board", "[GATT✗] 命令失败 (CMD=0x{:02X}): {}", expected_cmd, e)
            }
        }
        res
    }

    // ================================================================
    // BLE 内部(private)
    // ================================================================

    /// 延迟创建 VendorGattClient（首次真正需要 BLE 时调用）。
    ///
    /// 延迟创建的核心：adapter 不在 start() 时建，而在 hotplug 的 BLE 路径或
    /// scan_ble_devices 首次进入时才建。建 adapter 会触发 macOS 蓝牙授权弹窗——
    /// 推迟到这一刻，意味着只有用户主动要用蓝牙时才弹，而不是 App 一启动就弹。
    ///
    /// 幂等：已建则直接返回克隆。共享槽（Arc<Mutex>）让 hotplug 和 core 看到同一个。
    #[cfg(feature = "ble")]
    pub(crate) async fn ensure_vendor_gatt_client(&self) -> Result<Arc<VendorGattClient>> {
        {
            let guard = self.vendor_gatt_client.lock().unwrap();
            if let Some(c) = guard.as_ref() {
                return Ok(c.clone());
            }
        }
        let adapter = self.get_or_create_adapter().await?;
        let audio_sink = self.build_ble_audio_sink();
        let client = Arc::new(VendorGattClient::new(
            adapter,
            audio_sink,
            self.event_tx.clone(),
        ));
        *self.vendor_gatt_client.lock().unwrap() = Some(client.clone());
        Ok(client)
    }

    /// 取缓存的 adapter,没有则同步现建(调用方 runtime 上 await)
    #[cfg(feature = "ble")]
    async fn get_or_create_adapter(&self) -> Result<Adapter> {
        {
            let guard = self.cached_adapter.lock().unwrap();
            if let Some(a) = guard.as_ref() {
                return Ok(a.clone());
            }
        }
        let adapter = Self::create_adapter().await?;
        *self.cached_adapter.lock().unwrap() = Some(adapter.clone());
        Ok(adapter)
    }

    #[cfg(feature = "ble")]
    async fn create_adapter() -> Result<Adapter> {
        use btleplug::api::Manager as _;
        let manager = btleplug::platform::Manager::new()
            .await
            .map_err(|e| anyhow::anyhow!("BLE Manager 失败: {}", e))?;
        let adapters = manager
            .adapters()
            .await
            .map_err(|e| anyhow::anyhow!("BLE adapters 失败: {}", e))?;
        adapters
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("无 BLE adapter"))
    }

    /// Construct the unified decoded-frame sink. Legacy PcmSink remains an adapter only.
    fn build_audio_sink(&self) -> Arc<dyn AudioFrameSink> {
        if let Some(frame_sink) = self.audio_frame_sink.lock().unwrap().clone() {
            return frame_sink;
        }
        if let Some(pcm) = self.pcm_sink.lock().unwrap().clone() {
            return Arc::new(PcmAudioFrameAdapter::new(pcm));
        }
        Arc::new(CountingSink::new())
    }

    #[cfg(feature = "ble")]
    fn build_ble_audio_sink(&self) -> Arc<dyn AudioFrameSink> {
        self.build_audio_sink()
    }
}

// ================================================================
// 命令交互辅助(USB only)
// ================================================================

/// RAII 暂停 Monitor 读取,让出接口给命令交互(USB fresh 命令也要暂停,防 monitor 吃响应)。
/// 在 spawn_blocking 线程内使用,sleep(20ms) 不阻塞 async runtime。
#[cfg(feature = "usb")]
struct PauseGuard {
    paused: Option<Arc<AtomicBool>>,
}

#[cfg(feature = "usb")]
impl PauseGuard {
    fn new(paused: Option<Arc<AtomicBool>>) -> Self {
        if let Some(p) = &paused {
            p.store(true, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Self { paused }
    }
}

#[cfg(feature = "usb")]
impl Drop for PauseGuard {
    fn drop(&mut self) {
        if let Some(p) = &self.paused {
            p.store(false, Ordering::SeqCst);
        }
    }
}

// ============ 日志辅助 ============

/// 字节切片格式化为 hex（前 16 字节 + 省略号），给调试日志用。
fn fmt_hex_prefix(data: &[u8]) -> String {
    const PREFIX: usize = 16;
    if data.len() <= PREFIX {
        data.iter()
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        let head: String = data[..PREFIX]
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<_>>()
            .join(" ");
        format!("{} … (共 {} 字节)", head, data.len())
    }
}

/// 同上，但按实际读取长度截断（USB read 可能返回短于 64 的 len）。
fn fmt_hex_prefix_len(data: &[u8], len: usize) -> String {
    let end = len.min(data.len());
    fmt_hex_prefix(&data[..end])
}

// ============ bindings blob 传输链路 ============

/// 把 [`BoardDeviceCore`] 的 USB/GATT 命令通道适配成 [`BlobLink`]。
///
/// 超时归一为 `Ok(None)`（= 旧固件不支持）：USB 读超时 `len == 0`，
/// GATT 超时错误文案含「超时」。
struct DeviceBlobLink<'a> {
    core: &'a BoardDeviceCore,
    conn_type: ConnectionType,
}

impl DeviceBlobLink<'_> {
    /// 发 64 字节 HID 请求，按连接类型返回解析用的原始应答字节。
    ///
    /// USB 应答含 report id（64 字节定长）；GATT 应答剥了 report id（变长）。
    async fn exchange(
        &mut self,
        request: &[u8; 64],
        expected_cmd: u8,
    ) -> Result<Option<Vec<u8>>, String> {
        match self.conn_type {
            #[cfg(feature = "usb")]
            ConnectionType::Usb => {
                let (len, buf) = self
                    .core
                    .cmd_via_fresh_usb(*request)
                    .await
                    .map_err(|e| e.to_string())?;
                if len == 0 {
                    return Ok(None); // 读超时：旧固件不回包
                }
                Ok(Some(buf[..len].to_vec()))
            }
            #[cfg(not(feature = "usb"))]
            ConnectionType::Usb => Err("usb feature 未启用".to_string()),
            ConnectionType::Ble => {
                #[cfg(feature = "ble")]
                {
                    match self.core.cmd_via_gatt(request, expected_cmd).await {
                        Ok(resp) => Ok(Some(resp)),
                        Err(e) if e.to_string().contains("超时") => Ok(None),
                        Err(e) => Err(e.to_string()),
                    }
                }
                #[cfg(not(feature = "ble"))]
                {
                    let _ = (request, expected_cmd);
                    Err("BLE feature 未启用".to_string())
                }
            }
        }
    }
}

impl crate::kernel::bindings_blob::BlobLink for DeviceBlobLink<'_> {
    async fn read_chunk(&mut self, offset: u16) -> Result<Option<(u16, u16, Vec<u8>)>, String> {
        use crate::kernel::bindings_blob as blob;
        let request = blob::read_bindings_blob_packet(offset);
        let Some(resp) = self
            .exchange(&request, blob::CMD_AI_READ_BINDINGS_BLOB)
            .await?
        else {
            return Ok(None);
        };
        let parsed = match self.conn_type {
            ConnectionType::Usb => {
                blob::parse_blob_read_hid_response(&resp).map(|(o, t, c)| (o, t, c.to_vec()))
            }
            ConnectionType::Ble => {
                blob::parse_blob_read_gatt_response(&resp).map(|(o, t, c)| (o, t, c.to_vec()))
            }
        };
        parsed
            .map(Some)
            .ok_or_else(|| "blob 读应答无效或命令失败".to_string())
    }

    async fn write_chunk(
        &mut self,
        offset: u16,
        chunk: &[u8],
    ) -> Result<Option<crate::kernel::bindings_blob::BlobWriteAck>, String> {
        use crate::kernel::bindings_blob as blob;
        let request = blob::write_bindings_blob_packet(offset, chunk);
        let Some(resp) = self
            .exchange(&request, blob::CMD_AI_WRITE_BINDINGS_BLOB)
            .await?
        else {
            return Ok(None);
        };
        let parsed = match self.conn_type {
            ConnectionType::Usb => blob::parse_blob_write_ack_hid_response(&resp),
            ConnectionType::Ble => blob::parse_blob_write_ack_gatt_response(&resp),
        };
        parsed
            .map(Some)
            .ok_or_else(|| "blob 写应答无效".to_string())
    }

    async fn commit(
        &mut self,
        total_len: u16,
        crc16: u16,
    ) -> Result<Option<crate::kernel::bindings_blob::BlobWriteAck>, String> {
        use crate::kernel::bindings_blob as blob;
        let request = blob::commit_bindings_blob_packet(total_len, crc16);
        let Some(resp) = self
            .exchange(&request, blob::CMD_AI_WRITE_BINDINGS_BLOB)
            .await?
        else {
            return Ok(None);
        };
        let parsed = match self.conn_type {
            ConnectionType::Usb => blob::parse_blob_write_ack_hid_response(&resp),
            ConnectionType::Ble => blob::parse_blob_write_ack_gatt_response(&resp),
        };
        parsed
            .map(Some)
            .ok_or_else(|| "blob commit 应答无效".to_string())
    }
}
