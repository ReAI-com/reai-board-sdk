//! 音频帧接收 trait。
//!
//! BLE mSBC 音频是高频流(~43 帧/秒),走专门的 sink 而非 [`BoardEvent`](crate::BoardEvent),
//! 避免淹没语义事件。sink 在 HID / GATT 读取线程被调用,实现**必须非阻塞**。

#[cfg(feature = "ble")]
use crate::kernel::msbc::MsbcDecoder;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "ble")]
use std::sync::{Arc, Mutex};

/// mSBC 帧接收者(57 字节原始帧)
///
/// 实现者负责解码 / 落盘 / 转发。SDK 内置实现见 [`MsbcDecoderSink`] 与 [`CountingSink`]。
pub trait AudioFrameSink: Send + Sync {
    fn on_msbc_frame(&self, frame: &[u8]);
}

/// 解码后的 PCM 样本接收者(16kHz mono **f32**,USB cpal 与 BLE mSBC 解码统一到 f32)
pub trait PcmSink: Send + Sync {
    fn on_pcm(&self, samples: &[f32]);
}

/// 测试用 sink:统计收到的帧数/字节数,不解码
pub struct CountingSink {
    pub frames: AtomicU64,
    pub bytes: AtomicU64,
}

impl CountingSink {
    pub fn new() -> Self {
        Self {
            frames: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
        }
    }

    /// 累计收到的帧数
    pub fn frame_count(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    /// 累计收到的字节数
    pub fn byte_count(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }
}

impl Default for CountingSink {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioFrameSink for CountingSink {
    fn on_msbc_frame(&self, frame: &[u8]) {
        self.frames.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(frame.len() as u64, Ordering::Relaxed);
    }
}

/// 解码 mSBC → f32 PCM 后回调 [`PcmSink`]
///
/// 需要 `ble` feature —— 解码器在独立的 LGPL crate `msbc-decoder` 里。
///
/// 内部持有一个有状态的 [`MsbcDecoder`] 解码器和
/// 一个复用的 f32 缓冲(避免每帧分配 Vec)。两者共用同一把 Mutex。
///
/// `on_pcm` 在锁内回调——`PcmSink::on_pcm` 契约是非阻塞(见 trait 文档),
/// 典型实现是 `try_send` mpsc,锁持有时间 = 解码 + 转换 + try_send,微秒级可接受。
#[cfg(feature = "ble")]
pub struct MsbcDecoderSink {
    inner: Mutex<MsbcDecoderSinkInner>,
    pcm_sink: Arc<dyn PcmSink>,
}

#[cfg(feature = "ble")]
struct MsbcDecoderSinkInner {
    decoder: MsbcDecoder,
    /// 复用缓冲:msbc 单帧输出 320 个 i16 样本,首次扩容到 320 后不再 alloc。
    pcm_buf: Vec<f32>,
}

#[cfg(feature = "ble")]
impl MsbcDecoderSink {
    pub fn new(pcm_sink: Arc<dyn PcmSink>) -> Self {
        Self {
            inner: Mutex::new(MsbcDecoderSinkInner {
                decoder: MsbcDecoder::new(),
                pcm_buf: Vec::with_capacity(320),
            }),
            pcm_sink,
        }
    }
}

#[cfg(feature = "ble")]
impl AudioFrameSink for MsbcDecoderSink {
    fn on_msbc_frame(&self, frame: &[u8]) {
        let mut inner = match self.inner.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(), // Mutex 中毒也恢复,音频流不中断
        };
        match inner.decoder.decode_frame(frame) {
            Ok(pcm_i16) => {
                // mSBC 解码输出 i16 → 转 f32(统一 PcmSink 格式,STT 也用 f32)
                inner.pcm_buf.clear();
                inner
                    .pcm_buf
                    .extend(pcm_i16.iter().map(|&s| s as f32 / 32768.0));
                // PcmSink::on_pcm 契约非阻塞,锁内回调安全
                self.pcm_sink.on_pcm(&inner.pcm_buf);
            }
            Err(e) => log::warn!(target: "audio", "mSBC 帧解码失败: {}", e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "ble")]
    use std::sync::Mutex as StdMutex;

    // 只被 ble 下的 MsbcDecoderSink 用例使用。
    #[cfg(feature = "ble")]
    struct VecSink(StdMutex<Vec<f32>>);
    #[cfg(feature = "ble")]
    impl PcmSink for VecSink {
        fn on_pcm(&self, samples: &[f32]) {
            self.0.lock().unwrap().extend_from_slice(samples);
        }
    }

    #[test]
    fn counting_sink_counts() {
        let sink = CountingSink::new();
        sink.on_msbc_frame(&[0u8; 57]);
        sink.on_msbc_frame(&[0u8; 57]);
        assert_eq!(sink.frame_count(), 2);
        assert_eq!(sink.byte_count(), 114);
    }

    // MsbcDecoderSink 只在 ble feature 下存在（解码器在独立的 LGPL crate 里）。
    #[cfg(feature = "ble")]
    #[test]
    fn decoder_sink_rejects_bad_frame() {
        let collected = Arc::new(VecSink(StdMutex::new(Vec::new())));
        let sink = MsbcDecoderSink::new(collected.clone());
        // 全零帧 sync word 不对 → decode_frame 报错 → 不调用 pcm 回调
        sink.on_msbc_frame(&[0u8; 57]);
        assert!(collected.0.lock().unwrap().is_empty());
    }
}
