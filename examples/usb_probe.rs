//! USB smoke test: HID events + explicit Vendor HID audio + auto-reconnect.
//!
//! Quick start:
//! ```sh
//! cargo run --example usb_probe
//! ```
//! Requires the board connected via USB (VID `0x363C`, PID `0xED20`).
//!
//! 简体中文：
//! USB 真机冒烟验证：HID 事件 + 显式 Vendor HID 音频租约 + 断线重连。
//! 需要：USB 连接 ReAI-Vibe-Board（VID=0x363C, PID=0xED20）。
//! SDK 是纯数据采集层：读 HID（Config `0xFFA0` / Consumer `0x000C`）
//! 并在 probe 连接后显式建立 session lease；连接本身不会启动音频。
//! **不注入系统输入**，因此不需要「辅助功能」。理论上也不需要「输入监控」
//! （那是 macOS 对**标准键盘 Usage 0x0007** 的保护；本设备按键走 vendor `0xFFA0`
//! / consumer `0x000C` 接口）—— 若系统仍提示则授权。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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

    let device = BoardDevice::open(BoardConfig::default()).expect("open 失败");

    // 注册 sink 不会启动任何系统或板载采集。
    device.set_pcm_sink(Arc::new(StatsSink::new()));

    device.start().await.expect("start 失败");

    println!("=== ReAI-Vibe-Board USB Probe (V2) ===");
    println!("HID 事件 + Vendor HID Audio PCM 同时监测,Ctrl+C 退出\n");

    let mut events = device.events();
    let lease_id = 0x5052_4F42;
    let mut started = false;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = heartbeat.tick(), if started => {
                if let Err(error) = device.control_audio_stream(
                    AudioStreamAction::Heartbeat, AudioTransport::UsbVendorHid,
                    AudioStreamScope::Session, lease_id, 5_000,
                ).await {
                    eprintln!("[音频 heartbeat] {error}");
                    started = false;
                    device.stop_local_audio_reader();
                }
            }
            event = events.recv() => match event {
                Ok(Some(evt)) => {
                    let connected_usb = matches!(&evt, BoardEvent::Connection(c)
                        if c.connected && c.connection_type == Some(reai_board_sdk::ConnectionType::Usb));
                    print_event(&evt);
                    if connected_usb {
                        match device.start_board_audio(
                            AudioTransport::UsbVendorHid, AudioStreamScope::Session,
                            lease_id, 5_000,
                        ).await {
                            Ok(_) => started = true,
                            Err(error) => eprintln!("[音频 START] {error}"),
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => eprintln!("[事件错误] {:?}", e),
            }
        }
    }
    if started {
        let _ = device
            .control_audio_stream(
                AudioStreamAction::Stop,
                AudioTransport::UsbVendorHid,
                AudioStreamScope::Session,
                lease_id,
                5_000,
            )
            .await;
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
