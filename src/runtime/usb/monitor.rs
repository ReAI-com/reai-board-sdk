//! HID 数据监控线程(USB)
//!
//! Config(0xFFA0)/ Consumer(0x000C)接口的阻塞读取线程,
//! 解析按键/模式事件,通过 `broadcast::Sender<BoardEvent>` 广播。
//!
//! USB-only:BLE GATT 走 `vendor_gatt::client`,不经此模块;
//! BLE HID 备选路径(走 hidapi)留作后续 `ble-hid` feature。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;

use super::device_manager::DeviceConnection;
use crate::kernel::consumer_hold::ConsumerHeldTracker;
use crate::kernel::event::{AiVoiceKeyEvent, BoardEvent, KeySource, ModeChangeEvent, ModeSource};
use crate::kernel::key_aggregator::{KeyStateAggregator, PressedKeyMeta};
use crate::kernel::protocol_hid::*;
use crate::kernel::types::ConnectionType;

// ============ 监控统计 ============

/// 监控统计
#[derive(Debug, Default)]
pub struct MonitorStats {
    pub key_event_count: AtomicU64,
    pub status_event_count: AtomicU64,
    pub audio_frame_total: AtomicU64,
}

/// Config 循环共享上下文(减少线程 spawn 的参数数)
struct MonitorContext {
    stats: Arc<MonitorStats>,
    aggregator: Arc<KeyStateAggregator>,
    event_tx: broadcast::Sender<BoardEvent>,
}

// ============ 监控器配置 ============

#[derive(Debug, Clone)]
pub struct MonitorConfig {
    pub connection_type: ConnectionType,
    #[allow(dead_code)]
    pub audio_timeout_ms: u64,
    #[allow(dead_code)]
    pub enable_consumer_interface: bool,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            connection_type: ConnectionType::Usb,
            audio_timeout_ms: 1000,
            enable_consumer_interface: false,
        }
    }
}

// ============ HID 监控器 ============

/// HID 数据监控器
///
/// 管理 Config/Consumer 接口的读取线程,将按键事件通过 KeyStateAggregator 聚合后广播。
pub struct HidMonitor {
    running: Arc<AtomicBool>,
    /// 暂停标志(命令交互时临时让出接口)
    paused: Arc<AtomicBool>,
    /// Config 接口连接(共享给命令交互)
    config_conn: Arc<Mutex<Option<DeviceConnection>>>,
    threads: Vec<JoinHandle<()>>,
    stats: Arc<MonitorStats>,
    aggregator: Arc<KeyStateAggregator>,
    event_tx: broadcast::Sender<BoardEvent>,
    ai_voice_pressed: Arc<AtomicBool>,
}

impl HidMonitor {
    /// 创建监控器
    pub fn new(event_tx: broadcast::Sender<BoardEvent>) -> Self {
        let aggregator = Arc::new(KeyStateAggregator::new(event_tx.clone()));
        Self {
            running: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
            config_conn: Arc::new(Mutex::new(None)),
            threads: Vec::new(),
            stats: Arc::new(MonitorStats::default()),
            aggregator,
            event_tx,
            ai_voice_pressed: Arc::new(AtomicBool::new(false)),
        }
    }

    #[allow(dead_code)]
    pub fn aggregator(&self) -> &Arc<KeyStateAggregator> {
        &self.aggregator
    }

    #[allow(dead_code)]
    pub fn stats(&self) -> &Arc<MonitorStats> {
        &self.stats
    }

    /// 暂停监控读取(命令交互时让出接口)
    #[allow(dead_code)]
    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(20));
    }

    #[allow(dead_code)]
    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }

    /// 获取共享 Config 连接(用于命令交互)
    pub fn config_conn(&self) -> Arc<Mutex<Option<DeviceConnection>>> {
        self.config_conn.clone()
    }

    /// 获取暂停标志 Arc(用于命令交互时暂停/恢复 Monitor)
    pub fn paused_arc(&self) -> Arc<AtomicBool> {
        self.paused.clone()
    }

    /// 启动 Config 接口监控线程
    pub fn start_config_monitor(
        &mut self,
        conn: DeviceConnection,
        config: MonitorConfig,
    ) -> anyhow::Result<()> {
        if self.running.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.running.store(true, Ordering::SeqCst);

        conn.set_nonblocking(true)?;
        let usage = conn.usage;

        let running = self.running.clone();
        let paused = self.paused.clone();
        let stats = self.stats.clone();
        let aggregator = self.aggregator.clone();
        let event_tx = self.event_tx.clone();

        // 共享连接给命令交互用
        let shared_conn = Arc::new(Mutex::new(Some(conn)));
        self.config_conn = shared_conn.clone();

        let ctx = MonitorContext {
            stats,
            aggregator,
            event_tx,
        };

        let handle = thread::spawn(move || {
            Self::monitor_config_loop(shared_conn, running, paused, ctx, config, usage);
        });
        self.threads.push(handle);
        log::debug!(target: "hid", "Config 监控线程已启动: usage=0x{:04X}", usage);
        Ok(())
    }

    /// 启动 Consumer 接口监控线程
    pub fn start_consumer_monitor(&mut self, conn: DeviceConnection) -> anyhow::Result<()> {
        conn.set_nonblocking(true)?;

        let running = self.running.clone();
        let stats = self.stats.clone();
        let aggregator = self.aggregator.clone();
        let ai_voice_pressed = self.ai_voice_pressed.clone();
        let event_tx = self.event_tx.clone();

        let handle = thread::spawn(move || {
            Self::monitor_consumer_loop(
                conn,
                running,
                stats,
                aggregator,
                ai_voice_pressed,
                event_tx,
            );
        });
        self.threads.push(handle);
        log::debug!(target: "hid", "Consumer 监控线程已启动");
        Ok(())
    }

    /// 停止所有监控线程
    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        for handle in self.threads.drain(..) {
            // 带超时的 join:设备断开时 HID read 可能阻塞,避免应用无法退出
            let timeout = Duration::from_secs(3);
            let joined = Self::timed_join(handle, timeout);
            if !joined {
                log::warn!(target: "hid", "监控线程在 {:?} 内未退出,已放弃等待", timeout);
            }
        }
        log::debug!(target: "hid", "所有监控线程已停止");
    }

    /// 带超时的 thread::join(JoinHandle 无原生超时,用 channel 通知模拟)
    fn timed_join(handle: JoinHandle<()>, timeout: Duration) -> bool {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = thread::spawn(move || {
            let _ = handle.join();
            let _ = tx.send(());
        });
        rx.recv_timeout(timeout).is_ok()
    }

    /// 是否正在运行
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    // ================================================================
    // Config 接口读取循环
    // ================================================================

    fn monitor_config_loop(
        conn: Arc<Mutex<Option<DeviceConnection>>>,
        running: Arc<AtomicBool>,
        paused: Arc<AtomicBool>,
        ctx: MonitorContext,
        _config: MonitorConfig,
        usage: u16,
    ) {
        let MonitorContext {
            stats,
            aggregator,
            event_tx,
        } = ctx;
        let interface_id = format!("USB:usage=0x{:04X}", usage);

        let mut consecutive_errors = 0u32;
        const MAX_ERRORS: u32 = 5;

        log::debug!(target: "hid", "Config 监控线程启动: {}", interface_id);

        while running.load(Ordering::SeqCst) {
            // 暂停时跳过读取(供命令交互使用)
            if paused.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }

            let mut buf = [0u8; PACKET_SIZE];
            let read_result = {
                let guard = conn.lock().unwrap();
                match guard.as_ref() {
                    Some(c) => c.read(&mut buf, 10),
                    None => break, // 连接已移除
                }
            };

            match read_result {
                Ok(len) if len > 0 => {
                    consecutive_errors = 0;
                    Self::handle_config_data(&buf, len, &stats, &aggregator, &event_tx);
                }
                Ok(_) => {}
                Err(e) => {
                    consecutive_errors += 1;
                    let err_msg = e.to_string();
                    if consecutive_errors >= MAX_ERRORS {
                        log::warn!(
                            target: "hid",
                            "Config 接口连续 {} 次读取错误,判定断开: {}",
                            consecutive_errors,
                            e
                        );
                        break;
                    }
                    if err_msg.contains("device")
                        || err_msg.contains("disconnect")
                        || err_msg.contains("removed")
                    {
                        log::warn!(target: "hid", "Config 接口设备已断开: {}", e);
                        break;
                    }
                }
            }

            std::thread::sleep(Duration::from_millis(1));
        }

        running.store(false, Ordering::SeqCst);
        aggregator.report_change(KeySource::Config, vec![], None);
        log::debug!(target: "hid", "Config 监控线程退出: {}", interface_id);
    }

    // ================================================================
    // Consumer 接口读取循环
    // ================================================================

    fn monitor_consumer_loop(
        conn: DeviceConnection,
        running: Arc<AtomicBool>,
        stats: Arc<MonitorStats>,
        aggregator: Arc<KeyStateAggregator>,
        ai_voice_pressed: Arc<AtomicBool>,
        event_tx: broadcast::Sender<BoardEvent>,
    ) {
        log::debug!(target: "hid", "Consumer 监控线程启动");

        let mut consecutive_errors = 0u32;
        const MAX_ERRORS: u32 = 5;

        let mut tracker = ConsumerHeldTracker::default();

        while running.load(Ordering::SeqCst) {
            let mut buf = [0u8; 64];

            match conn.read(&mut buf, 10) {
                Ok(len) if len >= 3 => {
                    consecutive_errors = 0;

                    let key_value = ((buf[2] as u16) << 8) | (buf[1] as u16);

                    // AI 语音键按下检测（释放挪到「真正清空」那一支，
                    // 否则按住语音键转旋钮，脉冲收尾的 0x0000 会误报一次释放）
                    if is_ai_voice_consumer_code(key_value) {
                        ai_voice_pressed.store(true, Ordering::SeqCst);
                        log::info!(target: "hid", "AI 语音键按下 (0x{:04X})", key_value);
                        emit_ai_voice(&event_tx, true);
                    }

                    let frame = tracker.on_frame(key_value, Instant::now());
                    if frame.counts_as_key_event {
                        stats.key_event_count.fetch_add(1, Ordering::SeqCst);
                    }
                    for batch in frame.batches {
                        aggregator.report_change(KeySource::Consumer, batch, None);
                    }
                    if frame.cleared && ai_voice_pressed.swap(false, Ordering::SeqCst) {
                        log::info!(target: "hid", "AI 语音键释放");
                        emit_ai_voice(&event_tx, false);
                    }

                    // 部分固件通过 Consumer usage 上报模式键，而不是
                    // Config 接口的 CMD_STATUS/0xC9。两条路径统一产生
                    // ModeChange；若随后收到状态帧，会以状态帧为最终值。
                    if let Some((mode_value, mode_name)) =
                        find_key_index_by_value(key_value).and_then(key_index_to_mode)
                    {
                        log::info!(
                            target: "hid",
                            "USB 模式切换 (Consumer): {} (0x{:02X})",
                            mode_name,
                            mode_value
                        );
                        emit_mode_dial(&event_tx, mode_name, mode_value);
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    consecutive_errors += 1;
                    let err_msg = e.to_string();
                    if err_msg.contains("device")
                        || err_msg.contains("disconnect")
                        || err_msg.contains("removed")
                        || consecutive_errors >= MAX_ERRORS
                    {
                        log::warn!(target: "hid", "Consumer 接口设备已断开: {}", e);
                        break;
                    }
                }
            }

            std::thread::sleep(Duration::from_millis(1));
        }

        running.store(false, Ordering::SeqCst);
        aggregator.report_change(KeySource::Consumer, vec![], None);
        // 断开时语音键可能正按着。这个标志跨线程存活（重连后新线程接着用同一份），
        // 不在这里收干净的话，下一次连接会带着上一次的「按着」状态开工。
        if ai_voice_pressed.swap(false, Ordering::SeqCst) {
            log::info!(target: "hid", "Consumer 线程退出，补发 AI 语音键释放");
            emit_ai_voice(&event_tx, false);
        }
        log::debug!(target: "hid", "Consumer 监控线程退出");
    }

    // ================================================================
    // 数据解析
    // ================================================================

    /// 处理 Config 接口数据(USB:0x02 按键 mask + 0x0A 状态/模式)
    fn handle_config_data(
        data: &[u8],
        len: usize,
        stats: &Arc<MonitorStats>,
        aggregator: &Arc<KeyStateAggregator>,
        event_tx: &broadcast::Sender<BoardEvent>,
    ) {
        if len < 3 {
            return;
        }
        let report_id = data[0];

        match report_id {
            // 按键事件 (Report ID 0x02)
            REPORT_ID_KEY_EVENT => {
                if len >= 4 {
                    let key_mask = data[3];
                    Self::handle_key_event_report(key_mask, aggregator, stats);
                }
            }
            // 输入数据 (Report ID 0x0A):状态/模式
            REPORT_ID_INPUT => {
                let cmd = data[1];
                let data_len = data[2] as usize;
                #[cfg(feature = "test-mode")]
                {
                    if cmd == CMD_AI_FACTORY_KEY_EVENT {
                        if let Ok(event) = parse_factory_key_event_unscoped(&data[..len]) {
                            let _ = event_tx.send(BoardEvent::FactoryKey(event));
                        }
                        return;
                    }
                }
                if cmd == CMD_STATUS && data_len >= 2 && data[3] == CMD_WORK_MODE_DATA {
                    let mode_value = data[4];
                    if let Some(mode) = WorkMode::from_u8(mode_value) {
                        stats.status_event_count.fetch_add(1, Ordering::SeqCst);
                        // USB/Config 路径唯一物理模式信号源(key mask 不含 9-11)
                        emit_mode_dial(event_tx, mode.display_name(), mode_value);
                    }
                }
            }
            _ => {}
        }
    }

    /// 解析按键掩码为键索引列表
    pub fn parse_key_mask(mask: u8) -> Vec<(usize, &'static str)> {
        let mut keys = Vec::new();
        if mask & 0x08 != 0 {
            keys.push((0, "音量A相(KEY0)"));
        }
        if mask & 0x10 != 0 {
            keys.push((1, "音量B相(KEY1)"));
        }
        if mask & 0x20 != 0 {
            keys.push((2, "音量按压(KEY2)"));
        }
        if mask & 0x01 != 0 {
            keys.push((6, "AI语音(KEY6)"));
        }
        if mask & 0x02 != 0 {
            keys.push((7, "Action键(KEY7)"));
        }
        if mask & 0x04 != 0 {
            keys.push((8, "Enter键(KEY8)"));
        }
        keys
    }

    /// 处理按键事件 Report
    fn handle_key_event_report(
        key_mask: u8,
        aggregator: &Arc<KeyStateAggregator>,
        stats: &Arc<MonitorStats>,
    ) {
        let keys = Self::parse_key_mask(key_mask);

        if keys.is_empty() {
            aggregator.report_change(KeySource::Config, vec![], None);
        } else {
            stats.key_event_count.fetch_add(1, Ordering::SeqCst);

            let pressed_keys: Vec<PressedKeyMeta> = keys
                .into_iter()
                .map(|(idx, name)| PressedKeyMeta {
                    key_index: idx,
                    key_name: name.to_string(),
                    key_value: key_mask as u16,
                    source: KeySource::Config,
                })
                .collect();
            aggregator.report_change(KeySource::Config, pressed_keys, Some(key_mask));
        }
    }
}

// ============ 事件发送 helper ============

fn emit_ai_voice(tx: &broadcast::Sender<BoardEvent>, pressed: bool) {
    let _ = tx.send(BoardEvent::AiVoiceKey(AiVoiceKeyEvent { pressed }));
}

fn emit_mode_dial(tx: &broadcast::Sender<BoardEvent>, mode: &str, value: u8) {
    let _ = tx.send(BoardEvent::ModeChange(ModeChangeEvent {
        mode: mode.to_string(),
        mode_value: value,
        source: ModeSource::Dial,
    }));
}

// 注:`find_key_index_by_value` / `key_index_to_mode` 已挪到 `kernel::protocol_hid`,
// 与 BLE GATT client 共用(本模块通过 `use crate::kernel::protocol_hid::*` 引入)。

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_key_mask_empty() {
        assert!(HidMonitor::parse_key_mask(0x00).is_empty());
    }

    #[test]
    fn test_parse_key_mask_single() {
        let keys = HidMonitor::parse_key_mask(0x01);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].0, 6); // AI Voice
    }

    #[test]
    fn test_parse_key_mask_combo() {
        let keys = HidMonitor::parse_key_mask(0x03);
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].0, 6);
        assert_eq!(keys[1].0, 7);
    }

    #[cfg(feature = "test-mode")]
    #[test]
    fn config_monitor_emits_factory_physical_event() {
        let (tx, mut rx) = broadcast::channel(4);
        let stats = Arc::new(MonitorStats::default());
        let aggregator = Arc::new(KeyStateAggregator::new(tx.clone()));
        let data = [
            REPORT_ID_INPUT,
            CMD_AI_FACTORY_KEY_EVENT,
            0x06,
            FACTORY_KEY_TEST_PROTOCOL_VERSION,
            0x34,
            0x12,
            0x04,
            0x01,
            0x2A,
        ];

        HidMonitor::handle_config_data(&data, data.len(), &stats, &aggregator, &tx);
        match rx.try_recv().expect("factory event") {
            BoardEvent::FactoryKey(event) => {
                assert_eq!(event.session, 0x1234);
                assert_eq!(event.input_index, 4);
                assert!(event.pressed);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn test_find_key_index_by_value() {
        assert_eq!(find_key_index_by_value(0x0F04), Some(6)); // AI Voice
        assert_eq!(find_key_index_by_value(0x0F0C), Some(11)); // CHAT
        assert_eq!(find_key_index_by_value(0xFFFF), None);
    }

    #[test]
    fn test_key_index_to_mode() {
        assert_eq!(key_index_to_mode(9), Some((1, "YOLO")));
        assert_eq!(key_index_to_mode(10), Some((2, "PLAN")));
        assert_eq!(key_index_to_mode(11), Some((0, "CHAT")));
        assert_eq!(key_index_to_mode(6), None);
    }
}
