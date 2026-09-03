//! UAC 兼容通路采集示例(对应内部 CLI `reai-vb audio --uac`):
//! 降混后的 16kHz mono 走 `PcmSink`,降混前的交织原始双声道经
//! `set_uac_stereo_wav_path` opt-in 落盘,供双麦研究排障。
//!
//! Quick start(板子经 USB 连接,双麦立体声固件 v1.72+ 枚举为 16kHz/2ch):
//! ```sh
//! # 只看电平,不落任何文件(零副作用)
//! cargo run --example uac_capture --features usb -- --duration 15
//! # mono 喂识别(ASR 输入格式)+ stereo 原始研究数据
//! cargo run --example uac_capture --features usb -- --duration 15 \
//!     --out test-out/uac_mono.wav --stereo-out test-out/uac_stereo.wav
//! ```
//!
//! 简体中文：默认不保存任何文件;`--out` 为降混后 16kHz mono(PCM16 WAV),
//! `--stereo-out` 为降混前的设备原始交织音频(通道数/采样率与设备枚举一致)。
//!
//! 旧单声道固件(v1.72 之前)同样可用:PcmSink 降混路径不变,只是
//! `--stereo-out` 落盘的是 1ch WAV。

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reai_board_sdk::sink::PcmSink;
use reai_board_sdk::{BoardConfig, BoardDevice};

/// PcmSink 契约:16kHz mono f32(ASR/客户端兼容底线)
const OUTPUT_SAMPLE_RATE: u32 = 16_000;

/// 电平打印(每秒峰值)+ 可选 mono WAV 落盘
struct CaptureSink {
    mono_wav_path: Option<String>,
    state: Mutex<SinkState>,
    stop_writing: AtomicBool,
}

struct SinkState {
    mono_samples: Vec<f32>,
    peak: f32,
    total: u64,
}

impl CaptureSink {
    fn new(mono_wav_path: Option<String>) -> Self {
        Self {
            mono_wav_path,
            state: Mutex::new(SinkState {
                mono_samples: Vec::new(),
                peak: 0.0,
                total: 0,
            }),
            stop_writing: AtomicBool::new(false),
        }
    }

    /// 取走每秒电平窗口(峰值/样本数),重置窗口
    fn take_window(&self) -> (f32, u64) {
        let mut state = self.state.lock().unwrap();
        (std::mem::take(&mut state.peak), state.total)
    }
}

impl PcmSink for CaptureSink {
    fn on_pcm(&self, samples: &[f32]) {
        let mut state = self.state.lock().unwrap();
        for &s in samples {
            state.peak = state.peak.max(s.abs());
        }
        state.total += samples.len() as u64;
        if self.mono_wav_path.is_some() && !self.stop_writing.load(Ordering::SeqCst) {
            state.mono_samples.extend_from_slice(samples);
        }
    }
}

fn parse_args(args: impl Iterator<Item = String>) -> (u64, Option<String>, Option<String>) {
    let (mut duration, mut out, mut stereo_out) = (15u64, None, None);
    let mut iter = args;
    while let Some(arg) = iter.next() {
        let mut value = |name: &str| iter.next().unwrap_or_else(|| panic!("{name} 需要一个参数"));
        match arg.as_str() {
            "--duration" => duration = value("--duration").parse().expect("--duration 需要秒数"),
            "--out" => out = Some(value("--out")),
            "--stereo-out" => stereo_out = Some(value("--stereo-out")),
            other => panic!(
                "未知参数 {other};用法: [--duration 秒] [--out mono.wav] [--stereo-out stereo.wav]"
            ),
        }
    }
    (duration, out, stereo_out)
}

/// 手写 PCM16 WAV(44 字节头),示例用,与库内 usb_capture 的落盘同格式。
fn write_wav_pcm16(path: &str, channels: u16, sample_rate: u32, interleaved: &[f32]) {
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).expect("创建输出目录失败");
        }
    }
    let data_len = (interleaved.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * u32::from(channels) * 2).to_le_bytes());
    wav.extend_from_slice(&(channels * 2).to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for &s in interleaved {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        wav.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(path, wav).expect("写 WAV 失败");
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    let args = std::env::args().skip(1);
    let (duration, mono_out, stereo_out) = parse_args(args);

    let device = BoardDevice::open(BoardConfig::default()).expect("open 失败");
    device.start().await.expect("start 失败");
    if !device.is_connected() {
        eprintln!("设备未连接,请用 USB 连接板子后重试");
        device.shutdown();
        return;
    }

    // 立体声原始落盘(opt-in):须在 start_usb_uac_compat 前设置。
    // 原始样本驻留内存(stop 时一次性落盘),勿用于超长录制。
    if let Some(path) = &stereo_out {
        device.set_uac_stereo_wav_path(Some(path.clone()));
    }

    let sink = Arc::new(CaptureSink::new(mono_out.clone()));
    device.set_pcm_sink(sink.clone());
    device
        .start_usb_uac_compat()
        .expect("启动 UAC 兼容通路失败");

    println!(
        "UAC 采集 {} 秒:mono{}stereo{}",
        duration,
        mono_out
            .as_deref()
            .map(|p| format!(" → {p}"))
            .unwrap_or_else(|| " → (不落盘)".into()),
        stereo_out
            .as_deref()
            .map(|p| format!(" → {p} (原始交织,stop 时落盘)"))
            .unwrap_or_else(|| " → (不落盘)".into()),
    );

    let start = Instant::now();
    let deadline = start + Duration::from_secs(duration);
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let (peak, samples) = sink.take_window();
        let level = "#".repeat((peak * 40.0) as usize);
        println!(
            "[{:>2}s] 峰值 {:.3} |{}| (累计 {} 样本)",
            start.elapsed().as_secs(),
            peak,
            level,
            samples
        );
    }

    // stop 触发立体声原始 WAV 落盘
    device.stop_local_audio_reader();
    sink.stop_writing.store(true, Ordering::SeqCst);

    if let Some(path) = &mono_out {
        let samples = sink.state.lock().unwrap().mono_samples.clone();
        write_wav_pcm16(path, 1, OUTPUT_SAMPLE_RATE, &samples);
        println!(
            "mono WAV 已落盘: {path} ({}Hz, 1ch, {} 样本)",
            OUTPUT_SAMPLE_RATE,
            samples.len()
        );
    }
    // 立体声 WAV 由采集线程在 stop 时写完,稍等其收尾
    tokio::time::sleep(Duration::from_millis(500)).await;

    device.shutdown();
}
