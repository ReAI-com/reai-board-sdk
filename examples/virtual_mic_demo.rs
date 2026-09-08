//! BLE 虚拟麦克风实地测试:扫描 → 连接 → mSBC 解码 → 泵入系统虚拟麦克风。
//!
//! Quick start:
//! ```sh
//! cargo run --release --features virtual-mic --example virtual_mic_demo
//! ```
//!
//! 流程:首次运行会请求管理员授权安装驱动(弹系统密码框)→ 等 BLE 连上板子
//! → 解码 PCM 持续送入系统设备 "ReAI Vibe Board"。之后在 系统设置 → 声音 →
//! 输入 可见该设备;用 QuickTime / 语音备忘录等普通 App 选它录音即可
//! (普通 App 遵循正常麦克风权限流程)。
//!
//! 需要:macOS + cmake(`brew install cmake`)、板子已 USB 配对过、蓝牙开启。
//! Ctrl+C 退出。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reai_board_sdk::sink::PcmSink;
use reai_board_sdk::virtual_mic::VirtualMic;
use reai_board_sdk::{
    AudioStreamAction, AudioStreamScope, AudioTransport, BoardConfig, BoardDevice, BoardEvent,
};

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    // 驱动安装(一次性;重复执行=覆盖升级)
    if !VirtualMic::is_installed() {
        println!("虚拟麦克风驱动未安装,请求管理员授权安装(请留意系统密码框)...");
        VirtualMic::ensure_installed().expect("驱动安装失败");
        println!("驱动安装完成。");
    } else {
        println!("虚拟麦克风驱动已安装({})。", VirtualMic::installed_driver_path().display());
    }

    println!("打开 BoardDevice(tokio 内核)...");
    let device = BoardDevice::open(BoardConfig::default()).expect("open 失败");

    let mic = Arc::new(VirtualMic::start().expect("VirtualMic 启动失败"));
    device.set_pcm_sink(Arc::new(MeteredSink::new(mic)));

    println!("start(首次 BLE 可能等 CoreBluetooth adapter 预热 ~40s)...");
    device.start().await.expect("start 失败");

    // BLE-first:SDK 的自动连接只重连「有明确目标」的设备(防误连),纯 BLE 场景
    // 需要先扫描再显式 connect_ble。取信号最强的第一台;也可改 connect_ble 指定名字。
    println!("[BLE] 扫描周围板子(10s)...");
    let devices = device
        .scan_ble_devices(Duration::from_secs(10))
        .await
        .expect("BLE 扫描失败");
    match devices.iter().max_by_key(|d| d.rssi.unwrap_or(-200)) {
        Some(info) => {
            println!("[BLE] 找到 {} (rssi={:?}),连接...", info.name, info.rssi);
            device.connect_ble(&info.name);
        }
        None => {
            eprintln!(
                "[BLE] 未发现板子(REAI_VB_*);确认板子开机、未被其他设备连接。\
                 继续运行中,板子出现后重跑或插入 USB 也可触发。"
            );
        }
    }

    println!("=== ReAI-Vibe-Board Virtual Mic Demo ===");
    println!("连上板子后对其说话,系统内用 \"ReAI Vibe Board\" 这个麦克风录音。Ctrl+C 退出\n");

    let mut events = device.events();
    let lease_id = 0x564D_4143; // "VMAC"
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
                            Ok(_) => {
                                managed_lease = true;
                                println!("[音频] BLE 音频流已建立,泵入 \"ReAI Vibe Board\"");
                            }
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
        BoardEvent::ModeChange(m) => println!("[模式] {} (0x{:02X})", m.mode, m.mode_value),
        BoardEvent::DeviceInfo(d) => println!(
            "[设备] fw={} mac={} battery={}%",
            d.firmware_version, d.mac_address, d.battery_level
        ),
        BoardEvent::Error(e) => println!("[错误] {}", e.message),
        _ => {}
    }
}

/// 转发 sink:PCM 直通虚拟麦克风,每秒打印一次统计(实地测试时可观察数据流)。
struct MeteredSink {
    inner: Arc<VirtualMic>,
    samples: AtomicU64,
    last: Mutex<Instant>,
}

impl MeteredSink {
    fn new(inner: Arc<VirtualMic>) -> Self {
        Self {
            inner,
            samples: AtomicU64::new(0),
            last: Mutex::new(Instant::now()),
        }
    }
}

impl PcmSink for MeteredSink {
    fn on_pcm(&self, samples: &[f32]) {
        self.inner.on_pcm(samples);
        let total = self
            .samples
            .fetch_add(samples.len() as u64, Ordering::Relaxed);
        if let Ok(mut last) = self.last.lock() {
            if last.elapsed() >= Duration::from_secs(1) {
                let rms = (samples.iter().map(|x| x * x).sum::<f32>()
                    / samples.len().max(1) as f32)
                    .sqrt();
                println!(
                    "[泵入] 累计 {} 样本(~{:.1}s),本帧 rms={:.4}",
                    total + samples.len() as u64,
                    (total + samples.len() as u64) as f64 / 16000.0,
                    rms
                );
                *last = Instant::now();
            }
        }
    }
}
