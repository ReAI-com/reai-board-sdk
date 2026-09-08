//! Windows virtual microphone loopback self-test (no board needed).
//!
//! Feeds a 440 Hz sine into the `\\.\ReAIVibeBoardVirtualMic` control device
//! while capturing from the system "ReAI-Vibe-Board" endpoint via cpal, then
//! checks the capture actually carries the tone (RMS way above the silent
//! warm-up window) and writes a WAV for ear-checking.
//!
//! ```sh
//! cargo run --release --features usb,virtual-mic --example virtual_mic_loopback
//! ```
//!
//! Prereqs: the test-signed driver installed and test signing mode active.

#[tokio::main]
async fn main() {
    #[cfg(target_os = "windows")]
    if let Err(e) = imp::run().await {
        eprintln!("错误: {e:#}");
        std::process::exit(1);
    }

    #[cfg(not(target_os = "windows"))]
    println!("此 example 仅支持 Windows（需要 ReAIVibeBoardVirtualMic 驱动）");
}

#[cfg(target_os = "windows")]
mod imp {
    use std::f32::consts::PI;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use reai_board_sdk::sink::PcmSink;
    use reai_board_sdk::virtual_mic::VirtualMic;

    const RATE: u32 = 16_000;
    const FEED_HZ: f32 = 440.0;
    const WARMUP_SECS: u64 = 1;
    const FEED_SECS: u64 = 8;
    const TAIL_SECS: u64 = 2;
    const WAV_PATH: &str = r"..\..\reai-vbm\loopback.wav";

    pub async fn run() -> anyhow::Result<()> {
        // 1. Locate the capture endpoint.
        let host = cpal::default_host();
        let device = host
            .input_devices()?
            .find(|d| {
                d.name()
                    .map(|n| n.contains("ReAI-Vibe-Board"))
                    .unwrap_or(false)
            })
            .ok_or_else(|| anyhow::anyhow!("未找到 ReAI-Vibe-Board 捕获端点(设备已启动?)"))?;
        let config = device.default_input_config()?;
        println!(
            "端点: {} ({} Hz, {} ch, {:?})",
            device.name()?,
            config.sample_rate().0,
            config.channels(),
            config.sample_format()
        );

        // 2. Capture stream: collect everything into a buffer.
        let captured: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let err_fn = |e| eprintln!("capture stream error: {e}");
        let sample_format = config.sample_format();
        let sink = Arc::clone(&captured);
        let channels = config.channels() as usize;
        let stream = match sample_format {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config.clone().into(),
                move |d: &[f32], _| sink.lock().unwrap().extend_from_slice(d),
                err_fn,
                None,
            )?,
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config.clone().into(),
                move |d: &[i16], _| {
                    let mut sink = sink.lock().unwrap();
                    sink.extend(d.iter().map(|&s| s as f32 / 32768.0));
                },
                err_fn,
                None,
            )?,
            other => anyhow::bail!("不支持的采样格式: {other:?}"),
        };
        stream.play()?;

        // 3. Warm-up: expect (near-)silence while nothing is fed.
        let start_len = Arc::new(AtomicUsize::new(0));
        let feed_done = Arc::new(AtomicBool::new(false));
        println!("预热 {WARMUP_SECS}s（不喂音,应为静音）...");
        tokio::time::sleep(Duration::from_secs(WARMUP_SECS)).await;
        start_len.store(captured.lock().unwrap().len(), Ordering::SeqCst);

        // 4. Feed the sine through the control device on a worker thread.
        let mic = Arc::new(VirtualMic::start()?);
        let feed_flag = Arc::clone(&feed_done);
        std::thread::spawn(move || {
            let chunk_samples = RATE as usize / 10; // 100 ms of mono f32
            let mut phase = 0.0f32;
            let step = 2.0 * PI * FEED_HZ / RATE as f32;
            for _ in 0..(FEED_SECS * 10) {
                let chunk: Vec<f32> = (0..chunk_samples)
                    .map(|_| {
                        let v = phase.sin() * 0.3;
                        phase += step;
                        v
                    })
                    .collect();
                mic.on_pcm(&chunk); // PcmSink encodes to S16LE and writes the CDO
                std::thread::sleep(Duration::from_millis(100));
            }
            feed_flag.store(true, Ordering::SeqCst);
        });

        println!("喂入 {FEED_HZ}Hz 正弦 {FEED_SECS}s...");
        while !feed_done.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        println!("尾静音 {TAIL_SECS}s...");
        tokio::time::sleep(Duration::from_secs(TAIL_SECS)).await;
        drop(stream);

        // 5. Analyze: RMS over warm-up / feed / tail windows.
        let all = captured.lock().unwrap();
        let frames: Vec<f32> = all
            .chunks(channels)
            .map(|c| c.iter().sum::<f32>() / channels as f32)
            .collect();
        let warm_end = start_len.load(Ordering::SeqCst) / channels;
        let feed_start = warm_end;
        let feed_end = frames
            .len()
            .saturating_sub(TAIL_SECS as usize * (config.sample_rate().0 as usize));
        if feed_end <= feed_start {
            anyhow::bail!("采集样本不足,回调可能没有跑（检查隐私设置里桌面应用的麦克风访问）");
        }
        let rms =
            |slice: &[f32]| (slice.iter().map(|v| v * v).sum::<f32>() / slice.len() as f32).sqrt();
        let warm_rms = rms(&frames[..warm_end]);
        let feed_rms = rms(&frames[feed_start..feed_end]);
        let tail_rms = rms(&frames[feed_end..]);
        println!("RMS —— 预热静音: {warm_rms:.5} | 喂入窗口: {feed_rms:.5} | 尾部: {tail_rms:.5}");

        // 6. Write WAV (16-bit PCM, captured rate/channels) for ear-checking.
        write_wav(WAV_PATH, config.sample_rate().0, channels as u16, &frames)?;
        println!(
            "录音已写到 {}",
            std::path::Path::new(WAV_PATH)
                .canonicalize()
                .unwrap_or_default()
                .display()
        );

        if feed_rms > 0.02 && feed_rms > warm_rms * 5.0 {
            println!("PASS ✅  系统端点收到了喂入的正弦 —— 驱动数据链路(CDO→ring→WaveRT→端点)全通");
        } else {
            anyhow::bail!(
                "FAIL ❌  喂入窗口 RMS 不足（feed={feed_rms:.5}, warm={warm_rms:.5}）——数据链路不通"
            );
        }
        Ok(())
    }

    /// Minimal canonical 16-bit PCM WAV writer.
    fn write_wav(path: &str, rate: u32, channels: u16, mono: &[f32]) -> std::io::Result<()> {
        use std::io::Write;
        let data_len = (mono.len() * 2) as u32;
        let mut f = std::fs::File::create(path)?;
        f.write_all(b"RIFF")?;
        f.write_all(&(36 + data_len).to_le_bytes())?;
        f.write_all(b"WAVEfmt ")?;
        f.write_all(&16u32.to_le_bytes())?;
        f.write_all(&1u16.to_le_bytes())?; // PCM
        f.write_all(&channels.to_le_bytes())?;
        f.write_all(&rate.to_le_bytes())?;
        f.write_all(&(rate * channels as u32 * 2).to_le_bytes())?; // byte rate
        f.write_all(&(channels * 2).to_le_bytes())?; // block align
        f.write_all(&16u16.to_le_bytes())?; // bits
        f.write_all(b"data")?;
        f.write_all(&data_len.to_le_bytes())?;
        for &s in mono {
            let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
            f.write_all(&v.to_le_bytes())?;
        }
        Ok(())
    }
}
