//! mSBC 文件解码工具(读 .bin 文件 → f32 PCM)。
//!
//! 从 kernel/msbc.rs 挪来,因含文件 IO(`std::fs::read`),不属于纯算法内核。
//! 主要给 CLI/调试用(把固件抓的 mSBC .bin 转成 PCM 分析)。

use crate::kernel::msbc::{MsbcDecoder, MSBC_FRAME_SIZE, MSBC_SAMPLE_RATE, MSBC_SYNC_WORD};

/// 解码 mSBC .bin 文件 → f32 PCM 数据(16kHz mono)
///
/// 替代 `ffmpeg::decode_msbc_to_pcm()`,纯 Rust 实现。
pub fn decode_msbc_to_pcm(bin_path: &std::path::Path) -> anyhow::Result<Vec<f32>> {
    let data = std::fs::read(bin_path)?;
    let mut decoder = MsbcDecoder::new();
    let mut all_pcm = Vec::new();
    let mut frame_count = 0u32;
    let mut error_count = 0u32;

    let mut offset = 0;
    while offset + MSBC_FRAME_SIZE <= data.len() {
        if data[offset] != MSBC_SYNC_WORD {
            offset += 1;
            continue;
        }
        let frame = &data[offset..offset + MSBC_FRAME_SIZE];
        offset += MSBC_FRAME_SIZE;

        match decoder.decode_frame(frame) {
            Ok(pcm) => {
                // i16 → f32
                for &s in &pcm {
                    all_pcm.push(s as f32 / 32768.0);
                }
                frame_count += 1;
            }
            Err(e) => {
                if error_count < 10 {
                    log::warn!(target: "audio", "mSBC 帧 {} 解码失败 (offset 0x{:04X}): {}", frame_count, offset, e);
                }
                error_count += 1;
            }
        }
    }

    log::debug!(
        target: "audio",
        "mSBC 原生解码: {} 帧 ({} 错误), {} 样本, {:.2}s",
        frame_count,
        error_count,
        all_pcm.len(),
        all_pcm.len() as f64 / MSBC_SAMPLE_RATE as f64
    );

    Ok(all_pcm)
}
