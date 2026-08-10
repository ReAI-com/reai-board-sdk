// mSBC decoder — a bit-exact Rust translation of FFmpeg's SBC decoder.
//
// Derived from FFmpeg libavcodec/sbcdec.c, sbcdec_data.h and sbc.c:
//   Copyright (C) 2017  Aurelien Jacobs <aurel@gnuage.org>
//   Copyright (C) 2012-2013  Intel Corporation
//   Copyright (C) 2008-2010  Nokia Corporation
//   Copyright (C) 2004-2010  Marcel Holtmann <marcel@holtmann.org>
//   Copyright (C) 2004-2005  Henryk Ploetz <henryk@ploetzli.ch>
//   Copyright (C) 2005-2008  Brad Midgley <bmidgley@xmission.com>
//
// Rust translation:
//   Copyright (C) 2026  ReAI Team
//
// This library is free software; you can redistribute it and/or modify it
// under the terms of the GNU Lesser General Public License as published by
// the Free Software Foundation; either version 2.1 of the License, or (at
// your option) any later version.
//
// This library is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
// FITNESS FOR A PARTICULAR PURPOSE.  See the GNU Lesser General Public
// License for more details.
//
// You should have received a copy of the GNU Lesser General Public License
// along with this library; if not, see <https://www.gnu.org/licenses/>.
// A copy is included in the LICENSE file at the root of this crate.

//! 纯 Rust mSBC 解码器
//!
//! 无需 ffmpeg 运行时依赖即可解码 mSBC。
//! 算法精确匹配 FFmpeg sbcdec.c，已验证 bit-identical 输出。
//!
//! mSBC 固定参数：16kHz mono, 8 subbands, 15 blocks, bitpool=26
//! 每帧 57 字节 = sync(1) + header1(1) + header2(1) + CRC(1) + payload(53)
//! 每帧输出 120 个 i16 PCM 样本 (8 subbands × 15 blocks)
//!
//! # 许可证
//!
//! 本 crate 是 FFmpeg SBC 解码器的 bit-exact 翻译，属于衍生作品，
//! 因此按 **LGPL-2.1-or-later** 分发（而非依赖它的 `reai-board-sdk` 所用的 MIT）。
//! 集成到闭源产品前请确认符合 LGPL 的要求。
//!
//! 保留 0..N 索引循环风格以便与 FFmpeg 参考实现逐行对照。

// bit-exact 翻译:保留 0..N 索引循环风格对照 FFmpeg 源码,不改写成迭代器。
#![allow(clippy::needless_range_loop)]

// ─── mSBC 固定参数 ───
pub const MSBC_FRAME_SIZE: usize = 57;
pub const MSBC_SYNC_WORD: u8 = 0xAD;
const MSBC_SUBBANDS: usize = 8;
const MSBC_BLOCKS: usize = 15;
const MSBC_BITPOOL: i32 = 26;
pub const MSBC_SAMPLE_RATE: u32 = 16000;
const SAMPLES_PER_FRAME: usize = MSBC_SUBBANDS * MSBC_BLOCKS; // 120

/// FFmpeg SBCDEC_FIXED_EXTRA_BITS = 2
const FIXED_EXTRA_BITS: u32 = 2;

// ─── Synthesis matrix 8×16 (FFmpeg SN8 = raw >> 14) ───
// 取自 FFmpeg sbcdec_data.h synmatrix8[16][8]
// 原始 hex 值 >> 14
const SYNMATRIX8: [[i32; 8]; 16] = {
    let raw: [[u32; 8]; 16] = [
        [
            0x05a82798, 0xfa57d868, 0xfa57d868, 0x05a82798, 0x05a82798, 0xfa57d868, 0xfa57d868,
            0x05a82798,
        ],
        [
            0x0471ced0, 0xf8275a10, 0x018f8b84, 0x06a6d988, 0xf9592678, 0xfe70747c, 0x07d8a5f0,
            0xfb8e3130,
        ],
        [
            0x030fbc54, 0xf89be510, 0x07641af0, 0xfcf043ac, 0xfcf043ac, 0x07641af0, 0xf89be510,
            0x030fbc54,
        ],
        [
            0x018f8b84, 0xfb8e3130, 0x06a6d988, 0xf8275a10, 0x07d8a5f0, 0xf9592678, 0x0471ced0,
            0xfe70747c,
        ],
        [
            0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000,
            0x00000000,
        ],
        [
            0xfe70747c, 0x0471ced0, 0xf9592678, 0x07d8a5f0, 0xf8275a10, 0x06a6d988, 0xfb8e3130,
            0x018f8b84,
        ],
        [
            0xfcf043ac, 0x07641af0, 0xf89be510, 0x030fbc54, 0x030fbc54, 0xf89be510, 0x07641af0,
            0xfcf043ac,
        ],
        [
            0xfb8e3130, 0x07d8a5f0, 0xfe70747c, 0xf9592678, 0x06a6d988, 0x018f8b84, 0xf8275a10,
            0x0471ced0,
        ],
        [
            0xfa57d868, 0x05a82798, 0x05a82798, 0xfa57d868, 0xfa57d868, 0x05a82798, 0x05a82798,
            0xfa57d868,
        ],
        [
            0xf9592678, 0x018f8b84, 0x07d8a5f0, 0x0471ced0, 0xfb8e3130, 0xf8275a10, 0xfe70747c,
            0x06a6d988,
        ],
        [
            0xf89be510, 0xfcf043ac, 0x030fbc54, 0x07641af0, 0x07641af0, 0x030fbc54, 0xfcf043ac,
            0xf89be510,
        ],
        [
            0xf8275a10, 0xf9592678, 0xfb8e3130, 0xfe70747c, 0x018f8b84, 0x0471ced0, 0x06a6d988,
            0x07d8a5f0,
        ],
        [
            0xf8000000, 0xf8000000, 0xf8000000, 0xf8000000, 0xf8000000, 0xf8000000, 0xf8000000,
            0xf8000000,
        ],
        [
            0xf8275a10, 0xf9592678, 0xfb8e3130, 0xfe70747c, 0x018f8b84, 0x0471ced0, 0x06a6d988,
            0x07d8a5f0,
        ],
        [
            0xf89be510, 0xfcf043ac, 0x030fbc54, 0x07641af0, 0x07641af0, 0x030fbc54, 0xfcf043ac,
            0xf89be510,
        ],
        [
            0xf9592678, 0x018f8b84, 0x07d8a5f0, 0x0471ced0, 0xfb8e3130, 0xf8275a10, 0xfe70747c,
            0x06a6d988,
        ],
    ];
    let mut table: [[i32; 8]; 16] = [[0i32; 8]; 16];
    let mut i = 0;
    while i < 16 {
        let mut j = 0;
        while j < 8 {
            let v = raw[i][j] as i32;
            table[i][j] = v >> 14;
            j += 1;
        }
        i += 1;
    }
    table
};

// ─── Prototype filter coefficients (FFmpeg SS8 = raw >> 14) ───
const PROTO_80M0: [i32; 40] = {
    let raw: [u32; 40] = [
        0x00000000, 0xfe8d1970, 0xee979f00, 0x11686100, 0x0172e690, 0xfff5bd1a, 0xfdf1c8d4,
        0xeac182c0, 0x0d9daee0, 0x00e530da, 0xffe9811d, 0xfd52986c, 0xe7054ca0, 0x0a00d410,
        0x006c1de4, 0xffdba705, 0xfcbc98e8, 0xe3889d20, 0x06af2308, 0x000bb7db, 0xffca00ed,
        0xfc3fbb68, 0xe071bc00, 0x03bf7948, 0xffc4e05c, 0xffb54b3b, 0xfbedadc0, 0xdde26200,
        0x0142291c, 0xff960e94, 0xff9f3e17, 0xfbd8f358, 0xdbf79400, 0xff405e01, 0xff7d4914,
        0xff8b1a31, 0xfc1417b8, 0xdac7bb40, 0xfdbb828c, 0xff762170,
    ];
    let mut table: [i32; 40] = [0i32; 40];
    let mut i = 0;
    while i < 40 {
        table[i] = raw[i] as i32 >> 14;
        i += 1;
    }
    table
};

const PROTO_80M1: [i32; 40] = {
    let raw: [u32; 40] = [
        0xff7c272c, 0xfcb02620, 0xda612700, 0xfcb02620, 0xff7c272c, 0xff762170, 0xfdbb828c,
        0xdac7bb40, 0xfc1417b8, 0xff8b1a31, 0xff7d4914, 0xff405e01, 0xdbf79400, 0xfbd8f358,
        0xff9f3e17, 0xff960e94, 0x0142291c, 0xdde26200, 0xfbedadc0, 0xffb54b3b, 0xffc4e05c,
        0x03bf7948, 0xe071bc00, 0xfc3fbb68, 0xffca00ed, 0x000bb7db, 0x06af2308, 0xe3889d20,
        0xfcbc98e8, 0xffdba705, 0x006c1de4, 0x0a00d410, 0xe7054ca0, 0xfd52986c, 0xffe9811d,
        0x00e530da, 0x0d9daee0, 0xeac182c0, 0xfdf1c8d4, 0xfff5bd1a,
    ];
    let mut table: [i32; 40] = [0i32; 40];
    let mut i = 0;
    while i < 40 {
        table[i] = raw[i] as i32 >> 14;
        i += 1;
    }
    table
};

// ─── LOUDNESS offset table (A2DP Appendix B, sbc_offset8[0] for 16kHz) ───
const SBC_OFFSET8_SF0: [i32; 8] = [-2, 0, 0, 0, 0, 0, 0, 1];

// ─── Bitstream Reader ───
struct BitReader<'a> {
    data: &'a [u8],
    byte_pos: usize,
    bit_pos: u8,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_pos: 0,
        }
    }

    fn read(&mut self, bits: u8) -> Result<u16, ()> {
        if bits == 0 {
            return Ok(0);
        }
        let mut result: u32 = 0;
        let mut remaining = bits;
        while remaining > 0 {
            if self.byte_pos >= self.data.len() {
                return Err(());
            }
            let available = 8 - self.bit_pos;
            let take = remaining.min(available);
            let byte = self.data[self.byte_pos] as u32;
            let shift = available - take;
            let mask = (1u32 << take) - 1;
            result = (result << take) | ((byte >> shift) & mask);
            remaining -= take;
            self.bit_pos += take;
            if self.bit_pos >= 8 {
                self.bit_pos = 0;
                self.byte_pos += 1;
            }
        }
        Ok(result as u16)
    }
}

// ─── Decoder State ───
/// mSBC 解码器状态
///
/// 内部维护 synthesis filterbank 的 `V[170]` 缓冲区和 `offset[16]` 循环索引。
/// 精确匹配 FFmpeg 的 `struct sbc_decoder_state`。
pub struct MsbcDecoder {
    v: [i32; 170],
    offset: [i32; 16],
}

impl Default for MsbcDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl MsbcDecoder {
    pub fn new() -> Self {
        let mut offset = [0i32; 16];
        for i in 0..16 {
            offset[i] = 10 * i as i32 + 10;
        }
        Self {
            v: [0i32; 170],
            offset,
        }
    }

    /// 解码一个 mSBC 帧 → 120 个 i16 PCM 样本
    ///
    /// `frame` 必须是完整的 57 字节 mSBC 帧。
    pub fn decode_frame(&mut self, frame: &[u8]) -> Result<Vec<i16>, &'static str> {
        if frame.len() < MSBC_FRAME_SIZE {
            return Err("frame too short");
        }
        if frame[0] != MSBC_SYNC_WORD {
            return Err("invalid sync word");
        }

        // Payload: skip sync(1) + header1(1) + header2(1) + CRC(1) = 4 bytes
        // FFmpeg: consumed = 32 (bits) → byte 4
        let payload = &frame[4..];
        let mut reader = BitReader::new(payload);

        // Read scale factors: 4 bits × 8 subbands
        let mut scale_factors = [0u8; 8];
        for sb in 0..8 {
            scale_factors[sb] = reader
                .read(4)
                .map_err(|_| "bitstream overflow reading scale factors")?
                as u8;
        }

        // Bit allocation (LOUDNESS mode — mSBC spec)
        let bits = Self::calculate_bits(&scale_factors);

        // Decode all 15 blocks
        let mut all_samples = Vec::with_capacity(SAMPLES_PER_FRAME);
        for _block in 0..MSBC_BLOCKS {
            let mut subband = [0i32; 8];
            for sb in 0..8 {
                if bits[sb] == 0 {
                    continue;
                }
                let raw = reader
                    .read(bits[sb])
                    .map_err(|_| "bitstream overflow reading samples")?
                    as i32;
                let shift = scale_factors[sb] as u32 + 1 + FIXED_EXTRA_BITS;
                let one = 1i32 << shift;
                let levels = (1i32 << bits[sb]) - 1;
                if levels > 0 {
                    // wrapping_mul：匹配 FFmpeg sbcdec.c 的 C int 补码语义（同 synthesis）。
                    // one 最大 1<<18=262144，raw 最大 65535，乘积可达 ~3.4e10 > i32::MAX，
                    // debug build overflow-checks 下裸 `*` 必 panic；wrapping 后 bit-identical。
                    subband[sb] = ((((raw << 1) | 1).wrapping_mul(one)) / levels) - one;
                }
            }
            let pcm = self.synthesis(&subband);
            all_samples.extend_from_slice(&pcm);
        }
        Ok(all_samples)
    }

    /// Bit allocation (LOUDNESS mode for mSBC)
    ///
    /// 精确翻译 FFmpeg ff_sbc_calculate_bits，MONO + LOUDNESS + 8 subbands。
    fn calculate_bits(scale_factors: &[u8; 8]) -> [u8; 8] {
        // LOUDNESS bitneed computation
        let mut bitneed = [0i32; 8];
        let mut max_bitneed = 0i32;

        for sb in 0..8 {
            let sf = scale_factors[sb] as i32;
            if sf == 0 {
                bitneed[sb] = -5;
            } else {
                let loudness = sf - SBC_OFFSET8_SF0[sb];
                bitneed[sb] = if loudness > 0 { loudness / 2 } else { loudness };
            }
            if bitneed[sb] > max_bitneed {
                max_bitneed = bitneed[sb];
            }
        }

        let mut bits = [0u8; 8];
        let mut bitcount: i32 = 0;
        let mut slicecount: i32 = 0;
        let mut bitslice = max_bitneed + 1;

        loop {
            bitslice -= 1;
            bitcount += slicecount;
            slicecount = 0;
            for sb in 0..8 {
                let n = bitneed[sb];
                if n > bitslice + 1 && n < bitslice + 16 {
                    slicecount += 1;
                } else if n == bitslice + 1 {
                    slicecount += 2;
                }
            }
            if bitcount + slicecount >= MSBC_BITPOOL {
                break;
            }
        }

        if bitcount + slicecount == MSBC_BITPOOL {
            bitcount += slicecount;
            bitslice -= 1;
        }

        for sb in 0..8 {
            bits[sb] = if bitneed[sb] < bitslice + 2 {
                0
            } else {
                (bitneed[sb] - bitslice).min(16) as u8
            };
        }

        for sb in 0..8 {
            if bitcount >= MSBC_BITPOOL {
                break;
            }
            if bits[sb] >= 2 && bits[sb] < 16 {
                bits[sb] += 1;
                bitcount += 1;
            } else if bitneed[sb] == bitslice + 1 && MSBC_BITPOOL > bitcount + 1 {
                bits[sb] = 2;
                bitcount += 2;
            }
        }

        for sb in 0..8 {
            if bitcount >= MSBC_BITPOOL {
                break;
            }
            if bits[sb] < 16 {
                bits[sb] += 1;
                bitcount += 1;
            }
        }

        bits
    }

    /// 8-subband synthesis filterbank
    ///
    /// 精确翻译 FFmpeg sbc_synthesize_eight:
    /// 1. 矩阵变换: synmatrix8[16][8] · subband[8] → V[170]
    /// 2. 窗函数: proto_80m0/m1 加窗 → 8 个 PCM 样本
    fn synthesis(&mut self, s: &[i32; 8]) -> [i16; 8] {
        let v = &mut self.v;
        let offset = &mut self.offset;

        // Matrix step: 16 行 dot product
        for i in 0..16 {
            offset[i] -= 1;
            if offset[i] < 0 {
                offset[i] = 159;
                let (left, right) = v.split_at_mut(160);
                right[0..9].copy_from_slice(&left[0..9]);
            }

            let row = &SYNMATRIX8[i];
            // (unsigned)a * (unsigned)b — 用 wrapping_mul 模拟 32 位无符号乘法
            let acc = (row[0] as u32)
                .wrapping_mul(s[0] as u32)
                .wrapping_add((row[1] as u32).wrapping_mul(s[1] as u32))
                .wrapping_add((row[2] as u32).wrapping_mul(s[2] as u32))
                .wrapping_add((row[3] as u32).wrapping_mul(s[3] as u32))
                .wrapping_add((row[4] as u32).wrapping_mul(s[4] as u32))
                .wrapping_add((row[5] as u32).wrapping_mul(s[5] as u32))
                .wrapping_add((row[6] as u32).wrapping_mul(s[6] as u32))
                .wrapping_add((row[7] as u32).wrapping_mul(s[7] as u32));

            v[offset[i] as usize] = (acc as i32) >> 15;
        }

        // Windowing + overlap-add → 8 PCM samples
        let mut output = [0i16; 8];
        let mut idx = 0usize;
        for i in 0..8 {
            let k = (i + 8) & 0xf;
            let oi = offset[i] as usize;
            let ok = offset[k] as usize;

            let acc = (v[oi] as u32)
                .wrapping_mul(PROTO_80M0[idx] as u32)
                .wrapping_add(v[ok + 1].wrapping_mul(PROTO_80M1[idx]) as u32)
                .wrapping_add(v[oi + 2].wrapping_mul(PROTO_80M0[idx + 1]) as u32)
                .wrapping_add(v[ok + 3].wrapping_mul(PROTO_80M1[idx + 1]) as u32)
                .wrapping_add(v[oi + 4].wrapping_mul(PROTO_80M0[idx + 2]) as u32)
                .wrapping_add(v[ok + 5].wrapping_mul(PROTO_80M1[idx + 2]) as u32)
                .wrapping_add(v[oi + 6].wrapping_mul(PROTO_80M0[idx + 3]) as u32)
                .wrapping_add(v[ok + 7].wrapping_mul(PROTO_80M1[idx + 3]) as u32)
                .wrapping_add(v[oi + 8].wrapping_mul(PROTO_80M0[idx + 4]) as u32)
                .wrapping_add(v[ok + 9].wrapping_mul(PROTO_80M1[idx + 4]) as u32);

            output[i] = saturate_i16((acc as i32) >> 15);
            idx += 5;
        }

        output
    }
}

#[inline]
fn saturate_i16(v: i32) -> i16 {
    v.clamp(-32768, 32767) as i16
}

// 注:`decode_msbc_to_pcm`(读 .bin 文件解码)已挪到 `crate::tool::msbc_file`,
// 保持本模块为纯算法内核(无线程无 IO)。

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_synmatrix_first_element() {
        // 0x05a82798 >> 14 = 5790
        assert_eq!(SYNMATRIX8[0][0], 0x05a82798i32 >> 14);
    }

    #[test]
    fn test_proto_first_element() {
        // 0x00000000 >> 14 = 0
        assert_eq!(PROTO_80M0[0], 0);
        // 0xff7c272c >> 14（按 u32 位型重解释为 i32 再算术右移，修预存的字面量溢出编译失败）
        assert_eq!(PROTO_80M1[0], 0xff7c272cu32 as i32 >> 14);
    }

    #[test]
    fn test_bit_allocation() {
        // 使用实际测试数据的第一帧 scale factors
        let sf = [14u8, 9, 13, 11, 11, 9, 9, 8];
        let bits = MsbcDecoder::calculate_bits(&sf);
        // 所有 bits 之和应 <= bitpool (26) per block
        let total: i32 = bits.iter().map(|&b| b as i32).sum();
        assert!(
            total <= MSBC_BITPOOL,
            "total bits {} > bitpool {}",
            total,
            MSBC_BITPOOL
        );
        assert!(total > 0, "should have some bits allocated");
    }

    /// 回归：scale_factors 全 15 + samples 位全 1 的极端帧。
    /// 修复前 `((raw<<1)|1)*one` 乘积可达 ~3.4e10 > i32::MAX，
    /// debug build overflow-checks 下 panic；修复后 wrapping_mul 返回 Ok。
    #[test]
    fn test_decode_extreme_scale_factors_no_overflow() {
        let mut frame = [0u8; MSBC_FRAME_SIZE];
        frame[0] = MSBC_SYNC_WORD;
        // payload 全 0xFF → scale_factors 全 15、sample bits 全 1（raw 取最大）
        for b in frame[4..].iter_mut() {
            *b = 0xFF;
        }
        let mut dec = MsbcDecoder::new();
        let result = dec.decode_frame(&frame);
        assert!(
            result.is_ok(),
            "极端 scale factor 帧解码不应 panic: {:?}",
            result.err()
        );
        let pcm = result.unwrap();
        assert_eq!(pcm.len(), SAMPLES_PER_FRAME);
    }
}
