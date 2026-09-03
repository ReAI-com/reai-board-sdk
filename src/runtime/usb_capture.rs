//! USB Audio (UAC 1.0) 采集器
//!
//! cpal 从 UAC 设备采集 f32 PCM(16kHz mono),推送给 [`PcmSink`]。
//! `cpal::Stream` 不是 `Send`,在独立线程持有。
//!
//! USB 模式下固件收到 `APP_STATUS=Offline` 后走 USB Audio 输出 PCM,
//! 本采集器从 UAC 设备把这路 PCM 读出来交给消费者(STT / 录音 / 转发)。
//!
//! 双麦立体声固件(v1.72+)把 UAC 从单声道混音改为 \[L,R] 交织直出
//! (16kHz/2ch):本采集器仍按 [`PcmSink`] 的 16kHz mono 契约降混喂消费者;
//! 降混前的原始双声道可经 `new_with_stereo_wav` 显式 opt-in 落盘,供研究排障。

use std::io::Write as _;
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

/// 降混前的交织原始 f32 样本驻留缓冲:cpal 回调内只 push,`stop()` 后一次性
/// 落盘 PCM16 WAV —— 避免在实时回调内做文件 I/O。通道数/采样率记录设备枚举
/// 结果,WAV 头按其还原。
struct StereoTap {
    samples: Mutex<Vec<f32>>,
    channels: u16,
    sample_rate: u32,
}

/// 把交织 f32 样本一次性写成 PCM16 WAV(44 字节标准头,小端),返回数据字节数。
fn write_wav_pcm16(
    path: &str,
    channels: u16,
    sample_rate: u32,
    interleaved: &[f32],
) -> std::io::Result<u32> {
    let data_len = (interleaved.len() * 2) as u32;
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_len).to_le_bytes())?;
    file.write_all(b"WAVE")?;
    file.write_all(b"fmt ")?;
    file.write_all(&16_u32.to_le_bytes())?;
    file.write_all(&1_u16.to_le_bytes())?; // PCM
    file.write_all(&channels.to_le_bytes())?;
    file.write_all(&sample_rate.to_le_bytes())?;
    file.write_all(&(sample_rate * u32::from(channels) * 2).to_le_bytes())?;
    file.write_all(&(channels * 2).to_le_bytes())?; // block align
    file.write_all(&16_u16.to_le_bytes())?; // bits per sample
    file.write_all(b"data")?;
    file.write_all(&data_len.to_le_bytes())?;
    // 分块转换写出,避免超长录音时整体再驻留一份 i16。
    for chunk in interleaved.chunks(8192) {
        let mut bytes = Vec::with_capacity(chunk.len() * 2);
        for &s in chunk {
            let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        file.write_all(&bytes)?;
    }
    file.flush()?;
    Ok(data_len)
}

/// USB Audio 采集器:cpal UAC → [`PcmSink`] (f32 PCM)
pub struct UsbAudioCapture {
    running: Arc<AtomicBool>,
    sink: Arc<dyn PcmSink>,
    handle: Mutex<Option<thread::JoinHandle<()>>>,
    /// 立体声原始 WAV 落盘路径(None = 不落盘,默认)
    stereo_wav_path: Option<String>,
}

impl UsbAudioCapture {
    pub fn new(sink: Arc<dyn PcmSink>) -> Self {
        Self::new_with_stereo_wav(sink, None)
    }

    /// 同 [`UsbAudioCapture::new`],另外把降混前的交织原始音频(双麦研究数据)
    /// 落盘成 PCM16 WAV(opt-in)。`None` 等价于 [`UsbAudioCapture::new`]。
    ///
    /// 原始样本在 cpal 回调内驻留内存(16kHz 立体声约 128 KB/s),`stop()` 时
    /// 一次性落盘,通道数/采样率与设备枚举一致(旧单声道固件落盘 1ch 同样可用)。
    /// 代价是内存驻留,**勿用于超长录制**。
    pub fn new_with_stereo_wav(sink: Arc<dyn PcmSink>, path: Option<String>) -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            sink,
            handle: Mutex::new(None),
            stereo_wav_path: path,
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
        let stereo_wav_path = self.stereo_wav_path.clone();

        let handle = thread::spawn(move || {
            if let Err(e) = run_capture(running.clone(), sink, stereo_wav_path) {
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

/// 显式配置选择(纯逻辑,镜像 cpal 枚举的 `(channels, 格式, 频率下限, 频率上限)`,
/// 便于单元测试):**1ch 永远优先**(旧单声道固件的既有路径,行为不变),1ch 落空
/// 才兜底 2ch(双麦立体声固件 v1.72+);格式限定 F32、范围须覆盖 16kHz,与历史
/// 行为一致。都不匹配返回 `None` —— 调用侧再回退 `default_input_config()`。
fn select_explicit_probe(
    probes: &[(u16, SampleFormat, u32, u32)],
) -> Option<(u16, SampleFormat, u32, u32)> {
    const COVER_16K: fn(u32, u32) -> bool = |min, max| min <= 16_000 && max >= 16_000;
    [1u16, 2].iter().copied().find_map(|want| {
        probes.iter().copied().find(|&(ch, fmt, min, max)| {
            ch == want && fmt == SampleFormat::F32 && COVER_16K(min, max)
        })
    })
}

fn run_capture(
    running: Arc<AtomicBool>,
    sink: Arc<dyn PcmSink>,
    stereo_wav_path: Option<String>,
) -> Result<()> {
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
    // 选择规则见 select_explicit_probe（1ch 优先 = 旧固件路径不变，2ch 兜底 =
    // 立体声固件）；显式匹配落空再回退 default_input_config。
    let config = device
        .supported_input_configs()
        .ok()
        .and_then(|configs| {
            let probes: Vec<_> = configs
                .map(|c| {
                    (
                        c.channels(),
                        c.sample_format(),
                        c.min_sample_rate().0,
                        c.max_sample_rate().0,
                    )
                })
                .collect();
            select_explicit_probe(&probes)
        })
        .and_then(|probe| {
            // 按 probe 精确取回对应的 cpal range(谓词单一来源在上面,不会漂移)
            device
                .supported_input_configs()
                .ok()
                .and_then(|mut configs| {
                    configs.find(|c| {
                        (
                            c.channels(),
                            c.sample_format(),
                            c.min_sample_rate().0,
                            c.max_sample_rate().0,
                        ) == probe
                    })
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

    // 降混前的交织原始样本驻留(opt-in)。tap 的 Arc 会同时进回调闭包和收尾落盘。
    let stereo_tap = stereo_wav_path.as_ref().map(|_| {
        Arc::new(StereoTap {
            samples: Mutex::new(Vec::new()),
            channels: config.channels(),
            sample_rate: config.sample_rate().0,
        })
    });

    let err_fn = |err: cpal::StreamError| {
        log::warn!(target: "audio", "cpal 录音流错误: {}", err);
    };

    // 建输入流(F32 优先,I16 回退),DataCallback 推给 PcmSink
    let stream = match config.sample_format() {
        SampleFormat::F32 => {
            let sink_f32 = sink.clone();
            let tap = stereo_tap.clone();
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
                    // 降混之前先驻留原始交织样本(研究数据)
                    if let Some(tap) = &tap {
                        if let Ok(mut samples) = tap.samples.lock() {
                            samples.extend_from_slice(data);
                        }
                    }
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
            let tap = stereo_tap.clone();
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
                    // 降混之前先驻留原始交织样本(研究数据)
                    if let Some(tap) = &tap {
                        if let Ok(mut samples) = tap.samples.lock() {
                            samples.extend_from_slice(&f32_data);
                        }
                    }
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

    // 原始立体声(降混前)落盘 —— stop() 语义的一部分,线程退出前完成写文件
    if let (Some(tap), Some(path)) = (stereo_tap.as_ref(), stereo_wav_path.as_deref()) {
        let samples = tap.samples.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = write_wav_pcm16(path, tap.channels, tap.sample_rate, &samples) {
            log::warn!(target: "audio", "原始音频 WAV 落盘失败 {}: {}", path, e);
        } else {
            log::info!(
                target: "audio",
                "原始音频 WAV 已落盘: {} ({}Hz, {}ch, {} 帧)",
                path,
                tap.sample_rate,
                tap.channels,
                samples.len() / tap.channels as usize
            );
        }
    }
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
    use super::{
        select_explicit_probe, write_wav_pcm16, PcmNormalizer, StereoTap, UsbAudioCapture,
    };
    use cpal::SampleFormat;
    use std::sync::{Arc as StdArc, Mutex};

    /// 旧固件兼容性:仅枚举 1ch/F32/16k(历史单声道 UAC)时必须仍选中 1ch,
    /// 且 probe 原样返回 —— 行为与引入 2ch 兜底之前逐位一致。
    #[test]
    fn legacy_mono_firmware_still_selects_1ch() {
        let probes = [(1u16, SampleFormat::F32, 16_000u32, 16_000u32)];
        assert_eq!(
            select_explicit_probe(&probes),
            Some((1, SampleFormat::F32, 16_000, 16_000))
        );
    }

    /// 立体声固件(v1.72+)仅枚举 2ch/F32/16k 时选中 2ch(新增兼容分支)。
    #[test]
    fn stereo_firmware_selects_2ch_when_no_1ch_exists() {
        let probes = [(2u16, SampleFormat::F32, 16_000u32, 48_000u32)];
        assert_eq!(
            select_explicit_probe(&probes),
            Some((2, SampleFormat::F32, 16_000, 48_000))
        );
    }

    /// 优先级锁死:1ch 与 2ch 同时可枚举时永远选 1ch —— 2ch 兜底不得遮蔽
    /// 旧固件的既有首选路径,否则旧行为就被悄悄改掉了。
    #[test]
    fn mono_is_preferred_even_when_stereo_range_exists() {
        // 2ch 排在前面,1ch 仍须胜出
        let probes = [
            (2u16, SampleFormat::F32, 16_000u32, 48_000u32),
            (1u16, SampleFormat::F32, 16_000u32, 16_000u32),
        ];
        assert_eq!(
            select_explicit_probe(&probes),
            Some((1, SampleFormat::F32, 16_000, 16_000))
        );
    }

    /// I16-only(无 F32 range)必须返回 None —— 调用侧回退
    /// default_input_config,与引入 2ch 兜底之前的行为一致。
    #[test]
    fn i16_only_enumeration_falls_back_to_none() {
        let probes = [(1u16, SampleFormat::I16, 16_000u32, 16_000u32)];
        assert_eq!(select_explicit_probe(&probes), None);
    }

    /// 频率范围不覆盖 16kHz 的 range 一律不选(与历史匹配条件一致)。
    #[test]
    fn ranges_not_covering_16k_are_rejected() {
        let probes = [(1u16, SampleFormat::F32, 48_000u32, 96_000u32)];
        assert_eq!(select_explicit_probe(&probes), None);
    }

    /// 旧固件热路径(16kHz/1ch)无重采样:每帧 1:1 直通 downmix。
    #[test]
    fn normalizer_mono_16k_is_passthrough() {
        let mut normalizer = PcmNormalizer::new(16_000, 1);
        let output = normalizer.process(&[0.5, -0.5, 0.25]).to_vec();
        assert_eq!(output, vec![0.5, -0.5, 0.25]);
    }

    /// API 兼容:`new()`(既有调用方)等于不落盘;原始立体声只经
    /// `new_with_stereo_wav` 显式 opt-in 打开。
    #[test]
    fn new_defaults_to_no_dump_and_opt_in_carries_path() {
        struct NullSink;
        impl crate::kernel::sink::PcmSink for NullSink {
            fn on_pcm(&self, _samples: &[f32]) {}
        }
        let sink: StdArc<dyn crate::kernel::sink::PcmSink> = StdArc::new(NullSink);

        assert!(UsbAudioCapture::new(sink.clone()).stereo_wav_path.is_none());
        assert!(UsbAudioCapture::new_with_stereo_wav(sink.clone(), None)
            .stereo_wav_path
            .is_none());
        assert_eq!(
            UsbAudioCapture::new_with_stereo_wav(sink, Some("tap.wav".into()))
                .stereo_wav_path
                .as_deref(),
            Some("tap.wav")
        );
    }

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

    #[test]
    fn stereo_wav_writer_emits_canonical_pcm16_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tap.wav");
        let written =
            write_wav_pcm16(path.to_str().unwrap(), 2, 16_000, &[0.0, 0.5, -0.5, 1.0]).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(usize::try_from(written).unwrap() + 44, bytes.len());
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 36 + 8);
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[12..16], b"fmt ");
        assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 16);
        assert_eq!(u16::from_le_bytes(bytes[20..22].try_into().unwrap()), 1); // PCM
        assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 2); // channels
        assert_eq!(
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            16_000
        );
        assert_eq!(
            u32::from_le_bytes(bytes[28..32].try_into().unwrap()),
            16_000 * 2 * 2
        );
        assert_eq!(u16::from_le_bytes(bytes[32..34].try_into().unwrap()), 4); // block align
        assert_eq!(u16::from_le_bytes(bytes[34..36].try_into().unwrap()), 16);
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 8);
        // 交织顺序保持:L0=0.0 R0=0.5 L1=-0.5 R1=1.0(0.5*32767 截断为 16383)
        assert_eq!(
            &bytes[44..],
            &[0x00, 0x00, 0xFF, 0x3F, 0x01, 0xC0, 0xFF, 0x7F]
        );
    }

    #[test]
    fn stereo_wav_writer_clamps_out_of_range_samples() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tap.wav");
        write_wav_pcm16(path.to_str().unwrap(), 1, 16_000, &[2.0, -2.0]).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[44..46], &0x7FFF_i16.to_le_bytes());
        assert_eq!(&bytes[46..48], &(-0x7FFF_i16).to_le_bytes());
    }

    #[test]
    fn stereo_tap_accumulates_interleaved_samples() {
        let tap = StereoTap {
            samples: Mutex::new(Vec::new()),
            channels: 2,
            sample_rate: 16_000,
        };
        tap.samples.lock().unwrap().extend_from_slice(&[0.1, 0.2]);
        tap.samples.lock().unwrap().extend_from_slice(&[0.3, 0.4]);

        assert_eq!(*tap.samples.lock().unwrap(), vec![0.1, 0.2, 0.3, 0.4]);
    }
}
