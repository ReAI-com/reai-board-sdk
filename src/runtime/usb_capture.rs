//! USB Audio (UAC 1.0) 采集器
//!
//! cpal 从 UAC 设备采集 f32 PCM(16kHz mono),推送给 [`PcmSink`]。
//! `cpal::Stream` 不是 `Send`,在独立线程持有。
//!
//! USB 模式下固件收到 `APP_STATUS=Offline` 后走 USB Audio 输出 PCM,
//! 本采集器从 UAC 设备把这路 PCM 读出来交给消费者(STT / 录音 / 转发)。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, SampleFormat, StreamConfig};

use crate::kernel::sink::PcmSink;
use crate::kernel::types::is_usb_audio_device_name;

const OUTPUT_SAMPLE_RATE: u32 = 16_000;

/// 将设备原始的 interleaved PCM 连续转换为 PcmSink 契约要求的 16kHz mono。
/// `phase` 跨 cpal callback 保留，避免每个 buffer 独立重采样造成累计漂移。
/// `out_buf` 复用,避免 cpal 回调热路径上每帧分配 Vec。
struct PcmNormalizer {
    input_sample_rate: u32,
    channels: usize,
    phase: u64,
    /// 复用输出缓冲,容量稳定后不再 alloc。
    out_buf: Vec<f32>,
}

impl PcmNormalizer {
    fn new(input_sample_rate: u32, channels: usize) -> Self {
        Self {
            input_sample_rate: input_sample_rate.max(1),
            channels: channels.max(1),
            phase: 0,
            out_buf: Vec::new(),
        }
    }

    /// 重采样 + downmix 到 16kHz mono,返回内部复用缓冲的 slice。
    /// 调用方在下次 process 前必须用完返回值(同一 normalizer 的下次调用会 clear)。
    fn process(&mut self, interleaved: &[f32]) -> &[f32] {
        self.out_buf.clear();
        let frame_count = interleaved.len() / self.channels;
        self.out_buf.reserve(
            frame_count * OUTPUT_SAMPLE_RATE as usize / self.input_sample_rate as usize + 1,
        );

        for frame in interleaved.chunks_exact(self.channels) {
            let mono = frame.iter().copied().sum::<f32>() / self.channels as f32;
            self.phase += u64::from(OUTPUT_SAMPLE_RATE);
            while self.phase >= u64::from(self.input_sample_rate) {
                self.out_buf.push(mono.clamp(-1.0, 1.0));
                self.phase -= u64::from(self.input_sample_rate);
            }
        }
        &self.out_buf
    }
}

/// USB Audio 采集器:cpal UAC → [`PcmSink`] (f32 PCM)
pub struct UsbAudioCapture {
    running: Arc<AtomicBool>,
    sink: Arc<dyn PcmSink>,
    handle: Mutex<Option<thread::JoinHandle<()>>>,
}

impl UsbAudioCapture {
    pub fn new(sink: Arc<dyn PcmSink>) -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            sink,
            handle: Mutex::new(None),
        }
    }

    /// 启动采集(非阻塞;独立线程持有 cpal Stream)
    pub fn start(&self) -> Result<()> {
        if self.running.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.running.store(true, Ordering::SeqCst);

        let running = self.running.clone();
        let sink = self.sink.clone();

        let handle = thread::spawn(move || {
            if let Err(e) = run_capture(running.clone(), sink) {
                log::warn!(target: "audio", "USB Audio 采集线程退出: {}", e);
            }
        });

        *self.handle.lock().unwrap() = Some(handle);
        Ok(())
    }

    /// 停止采集(等待采集线程退出 + drop stream)
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(h) = self.handle.lock().unwrap().take() {
            let _ = h.join();
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

impl Drop for UsbAudioCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_capture(running: Arc<AtomicBool>, sink: Arc<dyn PcmSink>) -> Result<()> {
    let host = cpal::default_host();

    // 找 USB Audio 设备(Windows 上 UAC 比 HID 慢几秒枚举,带重试)
    let device = {
        const MAX_RETRIES: u32 = 10;
        let mut retry = 0u32;
        loop {
            if let Some(d) = find_usb_audio_device(&host) {
                break d;
            }
            if !running.load(Ordering::SeqCst) || retry >= MAX_RETRIES {
                return Err(anyhow!("未找到 USB Audio (UAC) 设备"));
            }
            retry += 1;
            thread::sleep(Duration::from_secs(1));
        }
    };

    log::info!(
        target: "audio",
        "USB Audio 设备: {}",
        device.name().unwrap_or_default()
    );

    // 优先选 16kHz/1ch/F32，避开 cpal 在 macOS 上对 default_input_config 的缓存陷阱
    // （USB Audio 设备被 UsbAudioCapture 切到某个 alt setting 后，缓存的 default
    // 可能跟 HAL 实际状态不一致，导致 callback 拿到错误 sample rate 的数据）。
    // 找不到再回退到 default_input_config。
    let config = device
        .supported_input_configs()
        .ok()
        .and_then(|mut configs| {
            configs.find(|c| {
                c.channels() == 1
                    && c.sample_format() == SampleFormat::F32
                    && c.min_sample_rate().0 <= 16_000
                    && c.max_sample_rate().0 >= 16_000
            })
        })
        .and_then(|c| c.try_with_sample_rate(cpal::SampleRate(16_000)))
        .or_else(|| device.default_input_config().ok())
        .ok_or_else(|| anyhow!("USB Audio 设备无可用输入配置"))?;
    let input_sample_rate = config.sample_rate().0;
    let input_channels = config.channels() as usize;
    log::debug!(
        target: "audio",
        "cpal 配置: {}Hz, {}ch, {:?} → {}Hz mono",
        input_sample_rate,
        input_channels,
        config.sample_format(),
        OUTPUT_SAMPLE_RATE
    );

    let err_fn = |err: cpal::StreamError| {
        log::warn!(target: "audio", "cpal 录音流错误: {}", err);
    };

    // 建输入流(F32 优先,I16 回退),DataCallback 推给 PcmSink
    let stream = match config.sample_format() {
        SampleFormat::F32 => {
            let sink_f32 = sink.clone();
            let normalizer = Arc::new(Mutex::new(PcmNormalizer::new(
                input_sample_rate,
                input_channels,
            )));
            let stream_config = StreamConfig {
                channels: config.channels(),
                sample_rate: config.sample_rate(),
                buffer_size: BufferSize::Default,
            };
            device.build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if let Ok(mut normalizer) = normalizer.lock() {
                        let pcm = normalizer.process(data);
                        if !pcm.is_empty() {
                            sink_f32.on_pcm(pcm);
                        }
                    }
                },
                err_fn,
                None,
            )?
        }
        SampleFormat::I16 => {
            let sink_i16 = sink.clone();
            let normalizer = Arc::new(Mutex::new(PcmNormalizer::new(
                input_sample_rate,
                input_channels,
            )));
            let stream_config = StreamConfig {
                channels: config.channels(),
                sample_rate: config.sample_rate(),
                buffer_size: BufferSize::Default,
            };
            device.build_input_stream(
                &stream_config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let f32_data: Vec<f32> = data.iter().map(|&s| s as f32 / 32768.0).collect();
                    if let Ok(mut normalizer) = normalizer.lock() {
                        let pcm = normalizer.process(&f32_data);
                        if !pcm.is_empty() {
                            sink_i16.on_pcm(pcm);
                        }
                    }
                },
                err_fn,
                None,
            )?
        }
        fmt => return Err(anyhow!("不支持的采样格式: {:?}", fmt)),
    };

    stream.play()?;
    log::info!(target: "audio", "USB Audio 采集已启动");

    // 持有 stream 直到 stop(cpal::Stream 非 Send,留在本线程栈)
    while running.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(100));
    }

    // 显式 pause + drop。CoreAudio 的 AudioUnit 释放是异步的（run loop 调度），
    // 若 drop 后立即 build 新 stream（如模式切换、USB 重连），旧 AudioUnit 可能
    // 仍在 run loop 上回调，导致多路 callback 并存 → 音频翻倍。
    // 先 pause 停回调，再 drop 释放 AudioUnit，并留窗口让 run loop 处理。
    let _ = stream.pause();
    drop(stream);
    log::info!(target: "audio", "USB Audio 采集已停止（pause + drop，已留 CoreAudio 释放窗口）");
    std::thread::sleep(Duration::from_millis(300));
    Ok(())
}

/// 在 cpal host 中查找本设备的 USB Audio 接口(名字匹配 is_usb_audio_device_name)
fn find_usb_audio_device(host: &cpal::Host) -> Option<cpal::Device> {
    let devices = host.input_devices().ok()?;
    for device in devices {
        if let Ok(name) = device.name() {
            if is_usb_audio_device_name(&name) {
                return Some(device);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::PcmNormalizer;

    #[test]
    fn normalizer_downmixes_stereo_and_resamples_48k_to_16k() {
        let mut normalizer = PcmNormalizer::new(48_000, 2);
        let mut input = Vec::new();
        for frame in 0..480 {
            let sample = frame as f32 / 480.0;
            input.extend_from_slice(&[sample, sample]);
        }

        let output = normalizer.process(&input).to_vec();
        assert_eq!(output.len(), 160);
        assert!(output.iter().all(|sample| (0.0..=1.0).contains(sample)));
    }

    #[test]
    fn normalizer_preserves_ratio_across_callbacks() {
        let mut normalizer = PcmNormalizer::new(44_100, 1);
        // process 返回内部复用缓冲的 slice,下次调用会 clear —— 跨调用比较要 clone。
        let first = normalizer.process(&vec![0.25; 441]).to_vec();
        let second = normalizer.process(&vec![0.25; 441]).to_vec();

        assert_eq!(first.len() + second.len(), 320);
        assert!(first
            .iter()
            .chain(second.iter())
            .all(|sample| (*sample - 0.25).abs() < f32::EPSILON));
    }
}
