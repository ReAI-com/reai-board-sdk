//! BLE smoke test: scan → connect → keys / mode / audio + auto-reconnect.
//!
//! Quick start:
//! ```sh
//! cargo run --example ble_probe
//! ```
//!
//! Requires a board that has been USB-paired at least once (firmware broadcasts
//! the `REAI_VB_` prefix over BLE) and Bluetooth enabled. First run is slow:
//! CoreBluetooth adapter warm-up takes ~40 s (the `start()` call blocks).
//!
//! 简体中文：
//! BLE 真机验证：自动扫描 `REAI_VB_` 前缀 → 连接 → 按键/模式/音频 + 断线重连。
//! 需要：设备已 USB 配对过（固件 BLE 广播 `REAI_VB_` 前缀），macOS 蓝牙开启。
//! 首次启动慢：CoreBluetooth adapter 预热 ~40s（`start` 会等）。
//!
//! Uses `events().recv()` for async consumption.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reai_board_sdk::sink::PcmSink;
use reai_board_sdk::{
    AudioStreamAction, AudioStreamScope, AudioTransport, BoardConfig, BoardDevice, BoardEvent,
};

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    println!("打开 BoardDevice(tokio 内核)...");
    let device = BoardDevice::open(BoardConfig::default()).expect("open 失败");

    // BLE 音频:mSBC 解码后 PCM 统计
    let pcm = std::sync::Arc::new(StatsSink::new());
    device.set_pcm_sink(pcm);

    println!("start(首次 BLE 可能等 CoreBluetooth adapter 预热 ~40s)...");
    device.start().await.expect("start 失败");

    println!("=== ReAI-Vibe-Board BLE Probe ===");
    println!("扫描 REAI_VB_ → 连接,验证按键/模式/音频 + 断线重连,Ctrl+C 退出\n");

    let mut events = device.events();
    let lease_id = 0x5052_4F42;
    let mut managed_lease = false;
    let mut legacy_session = false;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = heartbeat.tick(), if managed_lease => {
                if let Err(error) = device.control_audio_stream(
                    AudioStreamAction::Heartbeat, AudioTransport::BleGatt,
                    AudioStreamScope::Session, lease_id, 5_000,
                ).await {
                    eprintln!("[音频 heartbeat] {error}");
                    managed_lease = false;
                    device.stop_local_audio_reader();
                }
            }
            event = events.recv() => match event {
                Ok(Some(evt)) => {
                    let connected_ble = matches!(&evt, BoardEvent::Connection(c)
                        if c.connected && c.connection_type == Some(reai_board_sdk::ConnectionType::Ble));
                    print_event(&evt);
                    if connected_ble && !managed_lease && !legacy_session {
                        match device.start_board_audio(
                            AudioTransport::BleGatt, AudioStreamScope::Session,
                            lease_id, 5_000,
                        ).await {
                            Ok(_) => managed_lease = true,
                            Err(error) => {
                                eprintln!("[版本化 BLE 音频不可用] {error}; 尝试旧固件 session-only");
                                match device.start_legacy_ble_session_reader() {
                                    Ok(()) => legacy_session = true,
                                    Err(error) => eprintln!("[旧 BLE 音频] {error}"),
                                }
                            }
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => eprintln!("[事件错误] {:?}", e),
            }
        }
    }
    if managed_lease {
        let _ = device
            .control_audio_stream(
                AudioStreamAction::Stop,
                AudioTransport::BleGatt,
                AudioStreamScope::Session,
                lease_id,
                5_000,
            )
            .await;
    } else {
        device.stop_local_audio_reader();
    }
    device.shutdown();
}

fn print_event(evt: &BoardEvent) {
    match evt {
        BoardEvent::Connection(c) => println!(
            "[连接] connected={} type={:?} reason={:?}",
            c.connected, c.connection_type, c.reason
        ),
        BoardEvent::Reconnect(r) => println!("[重连] state={:?}", r.state),
        BoardEvent::KeyPress(k) => println!(
            "[按键] idx={} {} pressed={} source={:?}",
            k.key_index, k.key_name, k.pressed, k.source
        ),
        BoardEvent::ComboKey(c) => println!("[组合] keys={:?}", c.keys),
        BoardEvent::AiVoiceKey(a) => println!("[AI语音] pressed={}", a.pressed),
        BoardEvent::ModeChange(m) => println!("[模式] {} (0x{:02X})", m.mode, m.mode_value),
        #[cfg(feature = "test-mode")]
        BoardEvent::FactoryKey(k) => println!(
            "[工厂物理键] session=0x{:04X} index={} pressed={} seq={}",
            k.session, k.input_index, k.pressed, k.sequence
        ),
        BoardEvent::DeviceInfo(d) => println!(
            "[设备] fw={} mac={} battery={}%",
            d.firmware_version, d.mac_address, d.battery_level
        ),
        BoardEvent::Error(e) => println!("[错误] {}", e.message),
    }
}

/// PCM 统计 sink:累计样本数,每秒打印本帧 RMS(验证 BLE mSBC 解码有数据)
struct StatsSink {
    samples: AtomicU64,
    last: Mutex<Instant>,
}

impl StatsSink {
    fn new() -> Self {
        Self {
            samples: AtomicU64::new(0),
            last: Mutex::new(Instant::now()),
        }
    }
}

impl PcmSink for StatsSink {
    fn on_pcm(&self, samples: &[f32]) {
        let total = self
            .samples
            .fetch_add(samples.len() as u64, Ordering::Relaxed);
        if let Ok(mut last) = self.last.lock() {
            if last.elapsed() >= Duration::from_secs(1) {
                let rms = (samples.iter().map(|x| x * x).sum::<f32>()
                    / samples.len().max(1) as f32)
                    .sqrt();
                println!(
                    "[音频] 累计 {} 样本(~{:.1}s),本帧 rms={:.4}",
                    total + samples.len() as u64,
                    (total + samples.len() as u64) as f64 / 16000.0,
                    rms
                );
                *last = Instant::now();
            }
        }
    }
}
