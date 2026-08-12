//! USB smoke test: HID events + USB Audio PCM streams + auto-reconnect.
//!
//! Quick start:
//! ```sh
//! cargo run --example usb_probe
//! ```
//! Requires the board connected via USB (VID `0x363C`, PID `0xED20`).
//!
//! 简体中文：
//! USB 真机冒烟验证：HID 事件 + USB Audio PCM 两条数据流 + 断线重连。
//! 需要：USB 连接 ReAI-Vibe-Board（VID=0x363C, PID=0xED20）。
//! SDK 是纯数据采集层：读 HID（Config `0xFFA0` / Consumer `0x000C`）+ USB Audio，
//! **不注入系统输入**，因此不需要「辅助功能」。理论上也不需要「输入监控」
//! （那是 macOS 对**标准键盘 Usage 0x0007** 的保护；本设备按键走 vendor `0xFFA0`
//! / consumer `0x000C` 接口）—— 若系统仍提示则授权。
//!
//! Walks the `BoardDevice` facade (USB Audio capture starts automatically
//! when `on_connection_change` fires).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reai_board_sdk::sink::PcmSink;
use reai_board_sdk::{BoardConfig, BoardDevice, BoardEvent};

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    let device = BoardDevice::open(BoardConfig::default()).expect("open 失败");

    // USB Audio PCM 统计(USB 连接后 on_connection_change 自动启采集送此 sink)
    device.set_pcm_sink(Arc::new(StatsSink::new()));

    device.start().await.expect("start 失败");

    println!("=== ReAI-Vibe-Board USB Probe ===");
    println!("HID 事件 + USB Audio PCM 同时监测,拔插验证断线重连,Ctrl+C 退出\n");

    let mut events = device.events();
    loop {
        match events.recv().await {
            Ok(Some(evt)) => print_event(&evt),
            Ok(None) => break,
            Err(e) => eprintln!("[事件错误] {:?}", e),
        }
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
            "[按键] idx={} {} val=0x{:04X} pressed={} source={:?}",
            k.key_index, k.key_name, k.key_value, k.pressed, k.source
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
            "[设备信息] fw={} battery={}%",
            d.firmware_version, d.battery_level
        ),
        BoardEvent::Error(e) => println!("[错误] {}", e.message),
    }
}

/// USB Audio PCM 统计 sink:累计样本数,每秒打印一次本帧 RMS
struct StatsSink {
    samples: AtomicU64,
    last_print: Mutex<Instant>,
}

impl StatsSink {
    fn new() -> Self {
        Self {
            samples: AtomicU64::new(0),
            last_print: Mutex::new(Instant::now()),
        }
    }
}

impl PcmSink for StatsSink {
    fn on_pcm(&self, samples: &[f32]) {
        let total = self
            .samples
            .fetch_add(samples.len() as u64, Ordering::Relaxed);
        if let Ok(mut last) = self.last_print.lock() {
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
