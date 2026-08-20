//! BoardDevice 的同步门面 —— 给同步上下文消费者用。
//!
//! [`BoardDeviceBlocking`] 是 [`BoardDevice`] 的 zero-cost wrapper,
//! 把 async 命令用 `tokio::runtime::Handle::block_on` 包装成同步调用。
//!
//! Use cases:
//! - CLI-style non-interactive subcommands (one-shot commands that exit
//!   immediately after running)
//! - Migration scenarios where existing handlers run inside `spawn_blocking`
//!   and call synchronous wrappers
//! - Any consumer running on a `std::thread` that cannot easily `await`
//!
//! **红线**:禁止在 async 上下文调(block_on 会 panic / 卡死 runtime)。
//! async 消费者请直接用 [`BoardDevice`] 的 async 方法。

use std::sync::Arc;

use crate::kernel::event::DeviceInfo;
use crate::kernel::protocol_hid::KeyConfig;
use crate::kernel::sink::{AudioFrameSink, PcmSink};

use super::device::BoardDevice;

/// BoardDevice 的同步包装(block_on 桥)。
///
/// 构造:`let blocking = BoardDeviceBlocking::new(device);`
/// 内部持有 `BoardDevice` 和当前 tokio runtime Handle(构造时 `Handle::current()` 捕获)。
pub struct BoardDeviceBlocking {
    device: BoardDevice,
    handle: tokio::runtime::Handle,
}

impl BoardDeviceBlocking {
    /// 包装一个 BoardDevice。**必须在 tokio runtime 上下文调**(用 Handle::current 捕获)。
    ///
    /// 在 debug 构建里,如果检测到当前线程已是 tokio runtime worker(说明 new() 被误用在
    /// async 上下文里),会触发 debug_assert 提醒——后续的 block_on 调用会 panic / 卡死 runtime。
    /// release 构建不检查(性能),但误用会直接 panic。
    pub fn new(device: BoardDevice) -> Self {
        debug_assert!(
            std::thread::current()
                .name()
                .map(|n| !n.starts_with("tokio-runtime-worker"))
                .unwrap_or(true),
            "BoardDeviceBlocking::new 不应在 tokio runtime worker 线程调用 —— \
             block_on 会 panic。请改用 BoardDevice 的 async 方法,或用 spawn_blocking 桥接。"
        );
        Self {
            device,
            handle: tokio::runtime::Handle::current(),
        }
    }

    /// 暴露内部的 BoardDevice(需要 async 操作时取出)
    pub fn into_inner(self) -> BoardDevice {
        self.device
    }

    /// 启动(block_on core.start)
    pub fn start(&self) -> anyhow::Result<()> {
        self.handle.block_on(self.device.start())
    }

    /// 主动断开
    pub fn disconnect(&self) -> anyhow::Result<()> {
        self.handle.block_on(self.device.disconnect())
    }

    /// 读设备信息
    pub fn read_device_info(&self) -> anyhow::Result<DeviceInfo> {
        self.handle.block_on(self.device.read_device_info())
    }

    /// 读按键配置
    pub fn read_key_config(&self) -> anyhow::Result<KeyConfig> {
        self.handle.block_on(self.device.read_key_config())
    }

    /// 写按键配置
    pub fn write_key_config(&self, config: &KeyConfig) -> anyhow::Result<()> {
        self.handle.block_on(self.device.write_key_config(config))
    }

    pub fn get_silent_record(&self) -> anyhow::Result<bool> {
        self.handle.block_on(self.device.get_silent_record())
    }

    pub fn set_silent_record(&self, enable: bool) -> anyhow::Result<bool> {
        self.handle.block_on(self.device.set_silent_record(enable))
    }

    /// 进入/续租/退出工厂物理按键测试模式（固件 v1.58+）。
    #[cfg(feature = "test-mode")]
    pub fn set_factory_key_test(
        &self,
        enable: bool,
        session: u16,
    ) -> anyhow::Result<crate::FactoryKeyControlAck> {
        self.handle
            .block_on(self.device.set_factory_key_test(enable, session))
    }

    pub fn get_sleep_timeout(&self) -> anyhow::Result<crate::kernel::types::SleepTimeout> {
        self.handle.block_on(self.device.get_sleep_timeout())
    }

    pub fn set_sleep_timeout(
        &self,
        timeout: crate::kernel::types::SleepTimeout,
    ) -> anyhow::Result<crate::kernel::types::SleepTimeout> {
        self.handle.block_on(self.device.set_sleep_timeout(timeout))
    }

    /// 读取当前工作模式（CMD 0x12/0xC9）。
    pub fn get_work_mode(&self) -> anyhow::Result<crate::WorkMode> {
        self.handle.block_on(self.device.get_work_mode())
    }

    /// 启动固件 OTA 升级（阻塞当前线程直到完成）。仅 USB 连接可用。
    ///
    /// 进度通过 `on_progress` 回调上报。详见
    /// [`BoardDevice::start_dfu_upgrade`](super::device::BoardDevice::start_dfu_upgrade)。
    #[cfg(feature = "usb")]
    pub fn start_dfu_upgrade<P>(
        &self,
        firmware_path: std::path::PathBuf,
        on_progress: P,
    ) -> anyhow::Result<()>
    where
        P: Fn(crate::dfu::DfuProgress) + Send + Sync + 'static,
    {
        self.handle
            .block_on(self.device.start_dfu_upgrade(firmware_path, on_progress))
    }

    /// 取消正在进行的 DFU 升级（若有）。同步、无 IO。
    #[cfg(feature = "usb")]
    pub fn cancel_dfu_upgrade(&self) {
        self.device.cancel_dfu_upgrade();
    }

    /// 关机(test-mode)
    #[cfg(feature = "test-mode")]
    pub fn shutdown_device(&self, keep_pair: bool) -> anyhow::Result<()> {
        self.handle.block_on(self.device.shutdown_device(keep_pair))
    }

    // ===== 透传同步方法(无 IO,不需 block_on)=====

    pub fn connection(&self) -> Option<crate::kernel::types::ConnectionType> {
        self.device.connection()
    }
    pub fn is_connected(&self) -> bool {
        self.device.is_connected()
    }
    pub fn current_work_mode(&self) -> Option<crate::kernel::protocol_hid::WorkMode> {
        self.device.current_work_mode()
    }
    pub fn set_pcm_sink(&self, sink: Arc<dyn PcmSink>) {
        self.device.set_pcm_sink(sink);
    }
    pub fn set_audio_frame_sink(&self, sink: Arc<dyn AudioFrameSink>) {
        self.device.set_audio_frame_sink(sink);
    }

    /// 板载音频的启动路径。只注册 sink 是不够的——音频流默认关着,
    /// 不透传这几个方法的话,同步侧的调用方注册完 sink 会永远等不到回调。
    pub fn query_audio_capabilities(
        &self,
    ) -> anyhow::Result<crate::kernel::audio::AudioCapabilities> {
        self.handle.block_on(self.device.query_audio_capabilities())
    }
    pub fn control_audio_stream(
        &self,
        action: crate::kernel::audio::AudioStreamAction,
        transport: crate::kernel::audio::AudioTransport,
        scope: crate::kernel::audio::AudioStreamScope,
        lease_id: u32,
        ttl_ms: u16,
    ) -> anyhow::Result<crate::kernel::audio::AudioStreamState> {
        self.handle.block_on(
            self.device
                .control_audio_stream(action, transport, scope, lease_id, ttl_ms),
        )
    }
    pub fn start_board_audio(
        &self,
        transport: crate::kernel::audio::AudioTransport,
        scope: crate::kernel::audio::AudioStreamScope,
        lease_id: u32,
        ttl_ms: u16,
    ) -> anyhow::Result<crate::kernel::audio::AudioStreamState> {
        self.handle.block_on(
            self.device
                .start_board_audio(transport, scope, lease_id, ttl_ms),
        )
    }
    pub fn start_board_audio_reader(
        &self,
        transport: crate::kernel::audio::AudioTransport,
    ) -> anyhow::Result<()> {
        self.handle
            .block_on(self.device.start_board_audio_reader(transport))
    }
    pub fn start_legacy_ble_session_reader(&self) -> anyhow::Result<()> {
        self.device.start_legacy_ble_session_reader()
    }
    pub fn start_usb_uac_compat(&self) -> anyhow::Result<()> {
        self.device.start_usb_uac_compat()
    }
    pub fn stop_local_audio_reader(&self) {
        self.device.stop_local_audio_reader();
    }
    pub fn audio_capabilities(&self) -> crate::kernel::audio::AudioCapabilities {
        self.device.audio_capabilities()
    }
    pub fn audio_capability_state(&self) -> crate::kernel::audio::AudioCapabilityState {
        self.device.audio_capability_state()
    }
    pub fn active_audio_transport(&self) -> Option<crate::kernel::audio::AudioTransport> {
        self.device.active_audio_transport()
    }
    #[cfg(feature = "ble")]
    pub fn set_ble_target(&self, name: Option<&str>) {
        self.device.set_ble_target(name);
    }
    #[cfg(feature = "ble")]
    pub fn set_auto_reconnect(&self, on: bool) {
        self.device.set_auto_reconnect(on);
    }

    /// 事件流(同步消费用 events().blocking_recv())
    pub fn events(&self) -> super::events::EventStream {
        self.device.events()
    }
}

// 同步 shutdown/de Drop 由内部 BoardDevice 的 Drop 兜底(BoardDeviceBlocking 不额外实现)
