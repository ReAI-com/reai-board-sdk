//! 音频帧接收 trait。
//!
//! BLE mSBC 音频是高频流(~43 帧/秒),走专门的 sink 而非 [`BoardEvent`](crate::BoardEvent),
//! 避免淹没语义事件。sink 在 HID / GATT 读取线程被调用,实现**必须非阻塞**。

use crate::kernel::audio::AudioFrame;
#[cfg(any(feature = "usb", feature = "ble"))]
use crate::kernel::audio::{EncodedAudioPacket, SequenceDisposition, SequenceTracker};
#[cfg(any(feature = "usb", feature = "ble"))]
use crate::kernel::msbc::MsbcDecoder;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
#[cfg(any(feature = "usb", feature = "ble"))]
use std::sync::Mutex;

/// Unified decoded audio-frame receiver. Implementations must stay non-blocking.
pub trait AudioFrameSink: Send + Sync {
    fn on_audio_frame(&self, frame: AudioFrame<'_>);
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

impl PcmSink for CountingSink {
    fn on_pcm(&self, frame: &[f32]) {
        self.frames.fetch_add(1, Ordering::Relaxed);
        self.bytes
            .fetch_add(std::mem::size_of_val(frame) as u64, Ordering::Relaxed);
    }
}

impl AudioFrameSink for CountingSink {
    fn on_audio_frame(&self, frame: AudioFrame<'_>) {
        self.frames.fetch_add(1, Ordering::Relaxed);
        self.bytes
            .fetch_add(std::mem::size_of_val(frame.pcm) as u64, Ordering::Relaxed);
    }
}

/// Decode versioned mSBC packets and attach transport/sequence diagnostics.
///
/// Available with any transport feature (`usb` / `ble`) because the decoder
/// lives in the separate LGPL-2.1-or-later `msbc-decoder` crate.
#[cfg(any(feature = "usb", feature = "ble"))]
pub struct EncodedAudioDecoderSink {
    inner: Mutex<EncodedAudioDecoderInner>,
    audio_sink: Arc<dyn AudioFrameSink>,
    connection_epoch: u64,
}

#[cfg(any(feature = "usb", feature = "ble"))]
struct EncodedAudioDecoderInner {
    decoder: MsbcDecoder,
    pcm_buf: Vec<f32>,
    sequence: SequenceTracker,
}

#[cfg(any(feature = "usb", feature = "ble"))]
impl EncodedAudioDecoderSink {
    pub fn new(audio_sink: Arc<dyn AudioFrameSink>, connection_epoch: u64) -> Self {
        Self {
            inner: Mutex::new(EncodedAudioDecoderInner {
                decoder: MsbcDecoder::new(),
                pcm_buf: Vec::with_capacity(960),
                sequence: SequenceTracker::default(),
            }),
            audio_sink,
            connection_epoch,
        }
    }

    pub fn reset_sequence(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        inner.sequence.reset();
    }

    pub fn on_packet(&self, packet: EncodedAudioPacket<'_>, local_drop_packets: u64) {
        let mut inner = match self.inner.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        // 设备自己通告了不连续(编码器重启/时间线断点)时，旧的 last 已经没有参照意义。
        // 不复位的话，新序号会一直被判成"乱序"而整包丢弃,最坏要丢到序号追上旧 last+1
        // 为止——几万个包、几分钟静音,而且过程中一条日志都没有。
        if packet.device_discontinuity {
            inner.sequence.reset();
        }
        let disposition = packet
            .sequence
            .map(|sequence| inner.sequence.observe(sequence))
            .unwrap_or(SequenceDisposition::First);
        if matches!(
            disposition,
            SequenceDisposition::Duplicate | SequenceDisposition::OutOfOrder
        ) {
            return;
        }
        let packet_frames = (packet.payload.len() / crate::kernel::msbc::MSBC_FRAME_SIZE) as u64;
        let sequence_missing_packets = match disposition {
            SequenceDisposition::Gap { missing } => u64::from(missing),
            _ => 0,
        };
        // Local queue loss is already included in a subsequent sequence jump. Report it as
        // local loss (bounded by that jump) and leave any remainder as on-wire loss. If a local
        // drop is observed without sequence evidence (legacy envelope/reset), it is still local.
        let local_missing_packets = if packet.sequence.is_some() {
            local_drop_packets.min(sequence_missing_packets)
        } else {
            local_drop_packets
        };
        let wire_missing_packets = sequence_missing_packets.saturating_sub(local_missing_packets);
        let sequence_gap_frames = wire_missing_packets
            .saturating_mul(packet_frames)
            .min(u64::from(u16::MAX)) as u16;
        let local_drop_frames = local_missing_packets.saturating_mul(packet_frames);
        inner.pcm_buf.clear();
        // 逐帧解码,坏帧只丢它自己。旧固件的 legacy 信封会截断负载,尾块可能不足一帧;
        // 让残片把同一包里已经解好的完整帧一起废掉,是比旧行为更差的回退。
        let mut dropped_frames: u64 = 0;
        for encoded in packet.payload.chunks(crate::kernel::msbc::MSBC_FRAME_SIZE) {
            if encoded.len() < crate::kernel::msbc::MSBC_FRAME_SIZE {
                dropped_frames = dropped_frames.saturating_add(1);
                continue;
            }
            match inner.decoder.decode_frame(encoded) {
                Ok(pcm_i16) => inner
                    .pcm_buf
                    .extend(pcm_i16.iter().map(|&sample| sample as f32 / 32768.0)),
                Err(error) => {
                    log::warn!(target: "audio", "mSBC 帧解码失败: {error}");
                    dropped_frames = dropped_frames.saturating_add(1);
                }
            }
        }
        if inner.pcm_buf.is_empty() {
            return;
        }
        // 解码丢掉的帧要计进丢失信号,否则下一包序号连续、三个信号全灭,
        // 消费者无从知道自己该重置 VAD 状态。
        let local_drop_frames = local_drop_frames.saturating_add(dropped_frames);
        let frame = AudioFrame {
            pcm: &inner.pcm_buf,
            sample_rate: crate::kernel::msbc::MSBC_SAMPLE_RATE,
            channels: 1,
            transport: packet.transport,
            connection_epoch: self.connection_epoch,
            sequence: packet.sequence,
            captured_at_monotonic: std::time::Instant::now(),
            device_discontinuity: packet.device_discontinuity,
            sequence_gap_frames,
            local_drop_frames,
        };
        self.audio_sink.on_audio_frame(frame);
    }
}

pub struct PcmAudioFrameAdapter {
    pcm_sink: Arc<dyn PcmSink>,
}

impl PcmAudioFrameAdapter {
    pub fn new(pcm_sink: Arc<dyn PcmSink>) -> Self {
        Self { pcm_sink }
    }
}

impl AudioFrameSink for PcmAudioFrameAdapter {
    fn on_audio_frame(&self, frame: AudioFrame<'_>) {
        self.pcm_sink.on_pcm(frame.pcm);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::audio::AudioTransport;
    use std::sync::Mutex as StdMutex;

    struct VecSink(StdMutex<Vec<f32>>);
    impl PcmSink for VecSink {
        fn on_pcm(&self, samples: &[f32]) {
            self.0.lock().unwrap().extend_from_slice(samples);
        }
    }

    #[test]
    fn counting_sink_counts() {
        let sink = CountingSink::new();
        sink.on_pcm(&[0.0; 57]);
        sink.on_pcm(&[0.0; 57]);
        assert_eq!(sink.frame_count(), 2);
        assert_eq!(sink.byte_count(), 114 * std::mem::size_of::<f32>() as u64);
    }

    #[test]
    fn pcm_adapter_forwards_unified_audio_frame() {
        let collected = Arc::new(VecSink(StdMutex::new(Vec::new())));
        let sink = PcmAudioFrameAdapter::new(collected.clone());
        // 全零帧 sync word 不对 → decode_frame 报错 → 不调用 pcm 回调
        let frame = AudioFrame {
            pcm: &[0.25, -0.25],
            sample_rate: 16_000,
            channels: 1,
            transport: AudioTransport::BleGatt,
            connection_epoch: 1,
            sequence: Some(1),
            captured_at_monotonic: std::time::Instant::now(),
            device_discontinuity: false,
            sequence_gap_frames: 0,
            local_drop_frames: 0,
        };
        sink.on_audio_frame(frame);
        assert_eq!(*collected.0.lock().unwrap(), vec![0.25, -0.25]);
    }
}
