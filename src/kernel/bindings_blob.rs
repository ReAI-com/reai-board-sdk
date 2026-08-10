//! 绑定配置块（bindings blob）传输：命令 0x69/0x6A 的分片读写、CRC16 校验与帧编解码。
//!
//! 配置跟键盘走：键盘 flash 的 `ai_app_config` 4K 块内，0x100 起是 blob 区。
//! 帧布局（协议 offset 0 = flash 0x100）：
//!
//! ```text
//! [0..4]   magic "RBND"
//! [4]      format_version（当前 1）
//! [5]      flags（保留，写 0）
//! [6..8]   payload_len（LE）
//! [8..]    payload（≤ 3830 字节，JSON schema 归应用层）
//! [末尾 2] CRC16/CCITT-FALSE(payload)（LE）
//! ```
//!
//! 「从未写入」（擦除态全 0xFF）与「写过但损坏」（magic/CRC 不合法）必须区分：
//! 前者允许静默首配，后者上抛用户决策，绝不静默覆盖。
//!
//! 旧固件不认识 0x69/0x6A，不会回包 —— 超时即「不支持」，调用方据此降级，
//! 不影响其余功能。

use std::future::Future;

/// blob 帧魔数。
pub const BLOB_MAGIC: [u8; 4] = *b"RBND";
/// 当前帧格式版本。
pub const BLOB_FORMAT_VERSION: u8 = 1;
/// 帧头长度：magic(4) + version(1) + flags(1) + payload_len(2)。
pub const BLOB_HEADER_LEN: usize = 8;
/// 帧尾 CRC 长度。
pub const BLOB_CRC_LEN: usize = 2;
/// payload 上限：4K 块 − 0x100 保留区 − 帧头 − CRC。
pub const BLOB_MAX_PAYLOAD: usize = 0x1000 - 0x100 - BLOB_HEADER_LEN - BLOB_CRC_LEN;
/// 整帧上限。
pub const BLOB_MAX_FRAME: usize = BLOB_HEADER_LEN + BLOB_MAX_PAYLOAD + BLOB_CRC_LEN;
/// 单片负载上限（64 字节包 − report − cmd − len − offset(2) − result(1)…，取保守 56 对齐）。
pub const BLOB_CHUNK_SIZE: usize = 56;
/// 写命令的 commit 伪 offset。
pub const BLOB_OFFSET_COMMIT: u16 = 0xFFFF;

// commit 应答 detail 码（与固件侧定义保持一致）。
/// commit 成功。
pub const COMMIT_DETAIL_OK: u8 = 0;
/// 分片未收齐 / 总长不一致。
pub const COMMIT_DETAIL_INCOMPLETE: u8 = 1;
/// CRC 校验失败。
pub const COMMIT_DETAIL_CRC: u8 = 2;
/// 帧头非法（magic / 版本 / payload_len 对不上）。
pub const COMMIT_DETAIL_BAD_HEADER: u8 = 3;
/// 落盘后回读校验失败。
pub const COMMIT_DETAIL_VERIFY: u8 = 4;

/// CRC16/CCITT-FALSE（poly 0x1021，初值 0xFFFF，不反射，异或 0）。
///
/// 固件与 SDK 共用一个实现口径；测试锁定 `"123456789" → 0x29B1` 标准向量。
pub fn crc16_ccitt_false(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// 由 payload 组帧（帧头 + payload + CRC）。payload 超限报错。
pub fn build_blob_frame(payload: &[u8]) -> Result<Vec<u8>, String> {
    if payload.len() > BLOB_MAX_PAYLOAD {
        return Err(format!(
            "blob payload 超限: {} > {BLOB_MAX_PAYLOAD}",
            payload.len()
        ));
    }
    let mut frame = Vec::with_capacity(BLOB_HEADER_LEN + payload.len() + BLOB_CRC_LEN);
    frame.extend_from_slice(&BLOB_MAGIC);
    frame.push(BLOB_FORMAT_VERSION);
    frame.push(0); // flags
    frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    frame.extend_from_slice(payload);
    frame.extend_from_slice(&crc16_ccitt_false(payload).to_le_bytes());
    Ok(frame)
}

/// 帧检查结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameVerdict {
    /// 擦除态（magic 全 0xFF）：从未写入，允许静默首配。
    NeverWritten,
    /// 写过但损坏（magic 错 / 长度越界 / 版本不认识 / CRC 不匹配）。
    Corrupt(String),
    /// 有效帧。
    Valid {
        version: u8,
        flags: u8,
        payload: Vec<u8>,
    },
}

/// 解读一段完整帧字节（至少 [`BLOB_HEADER_LEN`] 字节）。
pub fn inspect_blob_frame(frame: &[u8]) -> FrameVerdict {
    let Some((never_written, payload_len)) = peek_blob_header(frame) else {
        return FrameVerdict::Corrupt(format!("帧不足 {BLOB_HEADER_LEN} 字节"));
    };
    if never_written {
        return FrameVerdict::NeverWritten;
    }
    if frame[..4] != BLOB_MAGIC {
        return FrameVerdict::Corrupt("magic 不是 RBND（写过但内容不认识）".to_string());
    }
    let version = frame[4];
    if version != BLOB_FORMAT_VERSION {
        return FrameVerdict::Corrupt(format!(
            "不认识的格式版本 {version}（本版本只认 {BLOB_FORMAT_VERSION}）"
        ));
    }
    let payload_len = payload_len as usize;
    if payload_len > BLOB_MAX_PAYLOAD {
        return FrameVerdict::Corrupt(format!("payload_len 越界: {payload_len}"));
    }
    let total = BLOB_HEADER_LEN + payload_len + BLOB_CRC_LEN;
    if frame.len() < total {
        return FrameVerdict::Corrupt(format!("帧不完整: {} < {total}", frame.len()));
    }
    let payload = &frame[BLOB_HEADER_LEN..BLOB_HEADER_LEN + payload_len];
    let trailing = u16::from_le_bytes([frame[total - 2], frame[total - 1]]);
    let computed = crc16_ccitt_false(payload);
    if computed != trailing {
        return FrameVerdict::Corrupt(format!(
            "CRC 不匹配: 存 0x{trailing:04X} 算 0x{computed:04X}"
        ));
    }
    FrameVerdict::Valid {
        version,
        flags: frame[5],
        payload: payload.to_vec(),
    }
}

/// 只读帧头：`(never_written, payload_len)`。不足 8 字节返回 `None`。
///
/// 读流程第一片回来就要靠它决定「还要拉多少」，不能等整帧收齐。
pub fn peek_blob_header(header: &[u8]) -> Option<(bool, u16)> {
    if header.len() < BLOB_HEADER_LEN {
        return None;
    }
    if header[..4].iter().all(|&b| b == 0xFF) {
        return Some((true, u16::from_le_bytes([header[6], header[7]])));
    }
    Some((false, u16::from_le_bytes([header[6], header[7]])))
}

/// 把整帧切成 56 字节对齐的分片（offset, chunk）；最后一片可短。
pub fn split_blob_chunks(frame: &[u8]) -> Vec<(u16, &[u8])> {
    frame
        .chunks(BLOB_CHUNK_SIZE)
        .enumerate()
        .map(|(i, chunk)| ((i * BLOB_CHUNK_SIZE) as u16, chunk))
        .collect()
}

/// 分片重组器：按 offset 落位、按 total_len 判齐。乱序 / 重复片都允许。
pub struct BlobReassembler {
    total: Option<u16>,
    buf: Vec<u8>,
    /// 每 bit 一片（56B）；3840 字节最多 69 片。
    marks: [u8; 12],
}

impl BlobReassembler {
    pub fn new() -> Self {
        Self {
            total: None,
            buf: Vec::new(),
            marks: [0u8; 12],
        }
    }

    /// 喂一片。offset 必须 56 对齐、chunk ≤ 56 字节（驱动侧只发对齐请求）。
    pub fn offer(&mut self, offset: u16, total_len: u16, chunk: &[u8]) -> Result<(), String> {
        if !(offset as usize).is_multiple_of(BLOB_CHUNK_SIZE) {
            return Err(format!("非对齐 offset: {offset}"));
        }
        if chunk.len() > BLOB_CHUNK_SIZE {
            return Err(format!("单片超长: {}", chunk.len()));
        }
        if total_len as usize > BLOB_MAX_FRAME {
            return Err(format!("total_len 越界: {total_len}"));
        }
        if let Some(known) = self.total {
            if known != total_len {
                return Err(format!("total_len 前后不一致: {known} vs {total_len}"));
            }
        } else {
            self.total = Some(total_len);
            self.buf = vec![0u8; total_len as usize];
        }
        let end = offset as usize + chunk.len();
        if end > total_len as usize {
            return Err(format!("分片越界: {end} > {total_len}"));
        }
        self.buf[offset as usize..end].copy_from_slice(chunk);
        let idx = offset as usize / BLOB_CHUNK_SIZE;
        self.marks[idx / 8] |= 1 << (idx % 8);
        Ok(())
    }

    pub fn is_complete(&self) -> bool {
        let Some(total) = self.total else {
            return false;
        };
        (0..total as usize).step_by(BLOB_CHUNK_SIZE).all(|off| {
            self.marks[off / BLOB_CHUNK_SIZE / 8] & (1 << ((off / BLOB_CHUNK_SIZE) % 8)) != 0
        })
    }

    /// 收齐后的整帧字节。
    pub fn frame_bytes(&self) -> Option<&[u8]> {
        self.is_complete().then_some(self.buf.as_slice())
    }
}

impl Default for BlobReassembler {
    fn default() -> Self {
        Self::new()
    }
}

/// 解析 HID 读应答：`[report, 0x69, len, result, off_lo, off_hi, total_lo, total_hi, chunk...]`。
/// 成功返回 `(offset, total_len, chunk)`。
pub fn parse_blob_read_hid_response(response: &[u8]) -> Option<(u16, u16, &[u8])> {
    if response.len() < 8
        || response[1] != CMD_AI_READ_BINDINGS_BLOB
        || response[2] < 5
        || response[3] != 0
    {
        return None;
    }
    let chunk_len = (response[2] - 5) as usize;
    if response.len() < 8 + chunk_len {
        return None;
    }
    let offset = u16::from_le_bytes([response[4], response[5]]);
    let total = u16::from_le_bytes([response[6], response[7]]);
    Some((offset, total, &response[8..8 + chunk_len]))
}

/// 解析 GATT 读应答：`[0x69, len, result, off_lo, off_hi, total_lo, total_hi, chunk...]`。
pub fn parse_blob_read_gatt_response(response: &[u8]) -> Option<(u16, u16, &[u8])> {
    if response.len() < 7
        || response[0] != CMD_AI_READ_BINDINGS_BLOB
        || response[1] < 5
        || response[2] != 0
    {
        return None;
    }
    let chunk_len = (response[1] - 5) as usize;
    if response.len() < 7 + chunk_len {
        return None;
    }
    let offset = u16::from_le_bytes([response[3], response[4]]);
    let total = u16::from_le_bytes([response[5], response[6]]);
    Some((offset, total, &response[7..7 + chunk_len]))
}

/// 写分片 / commit 应答。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobWriteAck {
    /// 0 = 成功，0xFF = 失败（沿用固件 CMD_RESULT 约定）。
    pub result: u8,
    /// 回显的 offset（commit 时为 [`BLOB_OFFSET_COMMIT`]）。
    pub offset: u16,
    /// commit 失败的细节码（见 `COMMIT_DETAIL_*`）；非 commit 应答为 0。
    pub detail: u8,
}

impl BlobWriteAck {
    pub fn is_ok(&self) -> bool {
        self.result == 0
    }
}

/// 解析 HID 写应答：`[report, 0x6A, len, result, off_lo, off_hi, detail?]`。
pub fn parse_blob_write_ack_hid_response(response: &[u8]) -> Option<BlobWriteAck> {
    if response.len() < 6 || response[1] != CMD_AI_WRITE_BINDINGS_BLOB || response[2] < 3 {
        return None;
    }
    Some(BlobWriteAck {
        result: response[3],
        offset: u16::from_le_bytes([response[4], response[5]]),
        detail: response.get(6).copied().unwrap_or(0),
    })
}

/// 解析 GATT 写应答：`[0x6A, len, result, off_lo, off_hi, detail?]`。
pub fn parse_blob_write_ack_gatt_response(response: &[u8]) -> Option<BlobWriteAck> {
    if response.len() < 5 || response[0] != CMD_AI_WRITE_BINDINGS_BLOB || response[1] < 3 {
        return None;
    }
    Some(BlobWriteAck {
        result: response[2],
        offset: u16::from_le_bytes([response[3], response[4]]),
        detail: response.get(5).copied().unwrap_or(0),
    })
}

/// 传输链路抽象：会话逻辑（拉取循环 / 分片写入 / commit / 回读校验）与
/// 真实 HID/GATT 收发之间的接缝。生产实现按连接类型包装 device 命令通道；
/// 测试用脚本化 mock。
///
/// 所有方法返回 `Ok(None)` 表示**超时无回包**——按协议结论即「固件不支持」。
pub trait BlobLink {
    fn read_chunk(
        &mut self,
        offset: u16,
    ) -> impl Future<Output = Result<Option<(u16, u16, Vec<u8>)>, String>> + Send;

    fn write_chunk(
        &mut self,
        offset: u16,
        chunk: &[u8],
    ) -> impl Future<Output = Result<Option<BlobWriteAck>, String>> + Send;

    fn commit(
        &mut self,
        total_len: u16,
        crc16: u16,
    ) -> impl Future<Output = Result<Option<BlobWriteAck>, String>> + Send;
}

/// 读 blob 的高层结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobRead {
    /// 键盘从未写入（擦除态）。
    NeverWritten,
    /// 写过但损坏——上抛用户决策，绝不静默覆盖。
    Corrupt(String),
    /// 有效 payload。
    Valid {
        version: u8,
        flags: u8,
        payload: Vec<u8>,
    },
    /// 旧固件不回包：同步不可用，调用方降级。
    Unsupported,
}

/// 完整读流程：offset 0 起逐片拉取直到收齐，解读帧。
pub async fn read_blob<L: BlobLink + ?Sized>(link: &mut L) -> Result<BlobRead, String> {
    let mut reassembler = BlobReassembler::new();
    let mut expected_total: Option<u16> = None;
    let mut next_offset: u16 = 0;

    loop {
        let Some((offset, total, chunk)) = link.read_chunk(next_offset).await? else {
            // 超时无回包：旧固件不认识 0x69
            return Ok(BlobRead::Unsupported);
        };
        if offset != next_offset {
            return Err(format!(
                "读应答 offset 错位: 期望 {next_offset} 实收 {offset}"
            ));
        }

        // 第一片含帧头：据此定总长（擦除态只回报头，判 NeverWritten 即收工）
        if next_offset == 0 {
            if chunk.len() < BLOB_HEADER_LEN {
                return Err(format!("首片不足帧头: {} 字节", chunk.len()));
            }
            let Some((never_written, payload_len)) = peek_blob_header(&chunk) else {
                return Err("帧头解读失败".to_string());
            };
            if never_written {
                return Ok(BlobRead::NeverWritten);
            }
            let payload_len = payload_len as usize;
            if payload_len > BLOB_MAX_PAYLOAD {
                // 帧头自报长度越界：典型的「写过但损坏」，拉不全也无需再拉
                return Ok(BlobRead::Corrupt(format!(
                    "payload_len 越界: {payload_len}"
                )));
            }
            let frame_total = (BLOB_HEADER_LEN + payload_len + BLOB_CRC_LEN) as u16;
            expected_total = Some(frame_total);
            // 固件回报的 total 与帧头自算不一致时信帧头（帧头是落盘数据，total 是现算的）
        }
        let total = expected_total.unwrap_or(total);
        reassembler.offer(offset, total, &chunk)?;

        if reassembler.is_complete() {
            break;
        }
        // 护栏：固件回零长片（chunk 空）会让 next_offset 不前进、is_complete 永假 → 死循环。
        // 正常固件单片负载必 > 0（至少含帧头），空片属异常，直接报错而非挂死 SDK。
        if chunk.is_empty() {
            return Err(format!("固件回零长片 @offset {offset}，读流程中止"));
        }
        next_offset = offset + chunk.len() as u16;
    }

    let frame = reassembler
        .frame_bytes()
        .ok_or_else(|| "重组未完成却退出循环".to_string())?;
    Ok(match inspect_blob_frame(frame) {
        FrameVerdict::NeverWritten => BlobRead::NeverWritten,
        FrameVerdict::Corrupt(detail) => BlobRead::Corrupt(detail),
        FrameVerdict::Valid {
            version,
            flags,
            payload,
        } => BlobRead::Valid {
            version,
            flags,
            payload,
        },
    })
}

/// 写失败的可区分原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobWriteError {
    /// payload 超过 [`BLOB_MAX_PAYLOAD`]。
    TooLarge,
    /// 旧固件不回包。
    Unsupported,
    /// commit 被固件拒绝（detail 码）。
    CommitRejected(u8),
    /// commit 后回读校验对不上。
    VerifyMismatch,
    /// 传输层错误（非超时）。
    Transport(String),
}

impl std::fmt::Display for BlobWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => write!(f, "blob payload 超过 {BLOB_MAX_PAYLOAD} 字节"),
            Self::Unsupported => write!(f, "固件不支持绑定配置块（无回包）"),
            Self::CommitRejected(detail) => write!(f, "commit 被固件拒绝（detail={detail}）"),
            Self::VerifyMismatch => write!(f, "写入后回读校验不一致"),
            Self::Transport(detail) => write!(f, "传输错误: {detail}"),
        }
    }
}

impl std::error::Error for BlobWriteError {}

/// 完整写流程：组帧 → 逐片写入 → commit → 回读校验。
///
/// commit 应答丢失（超时）时不直接判负：固件可能已落盘，回读校验才是终裁。
pub async fn write_blob<L: BlobLink + ?Sized>(
    link: &mut L,
    payload: &[u8],
) -> Result<(), BlobWriteError> {
    let frame = build_blob_frame(payload).map_err(|_| BlobWriteError::TooLarge)?;
    let total_len = frame.len() as u16;
    let crc = crc16_ccitt_false(payload);

    // 逐片写入（首片超时 = 旧固件，直接判不支持）
    let mut first = true;
    for (offset, chunk) in split_blob_chunks(&frame) {
        let ack = link
            .write_chunk(offset, chunk)
            .await
            .map_err(BlobWriteError::Transport)?;
        let Some(ack) = ack else {
            return if first {
                Err(BlobWriteError::Unsupported)
            } else {
                Err(BlobWriteError::Transport(format!(
                    "offset {offset} 写入无回包"
                )))
            };
        };
        first = false;
        if !ack.is_ok() || ack.offset != offset {
            return Err(BlobWriteError::Transport(format!(
                "offset {offset} 写入被拒（result=0x{:02X}）",
                ack.result
            )));
        }
    }

    // commit：被拒带 detail；应答丢失走回读终裁
    let commit_ack = link
        .commit(total_len, crc)
        .await
        .map_err(BlobWriteError::Transport)?;
    if let Some(ack) = commit_ack {
        if !ack.is_ok() {
            return Err(BlobWriteError::CommitRejected(ack.detail));
        }
    }

    // 回读校验（commit 成功或应答丢失都要过这一关）
    let read_back = read_blob(link).await.map_err(BlobWriteError::Transport)?;
    match read_back {
        BlobRead::Valid { payload: back, .. } if back == payload => Ok(()),
        _ => Err(BlobWriteError::VerifyMismatch),
    }
}

// 让 HidPacket 的构造器集中留在 protocol_hid（协议常量单点），这里只 re-export
// 便于会话实现与测试一处取用。
pub use super::protocol_hid::{
    commit_bindings_blob_packet, read_bindings_blob_packet, write_bindings_blob_packet,
    CMD_AI_READ_BINDINGS_BLOB, CMD_AI_WRITE_BINDINGS_BLOB,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    // ---------- CRC16 ----------

    #[test]
    fn crc16_标准向量() {
        // CRC-16/CCITT-FALSE 公认校验值
        assert_eq!(crc16_ccitt_false(b"123456789"), 0x29B1);
        assert_eq!(crc16_ccitt_false(b""), 0xFFFF);
        assert_eq!(crc16_ccitt_false(&[0x00]), 0xE1F0);
    }

    // ---------- 帧编解码 ----------

    #[test]
    fn 组帧布局与回读一致() {
        let payload = b"{\"v\":1,\"bindings\":{}}";
        let frame = build_blob_frame(payload).expect("组帧");
        assert_eq!(&frame[0..4], b"RBND");
        assert_eq!(frame[4], BLOB_FORMAT_VERSION);
        assert_eq!(frame[5], 0);
        assert_eq!(
            u16::from_le_bytes([frame[6], frame[7]]) as usize,
            payload.len()
        );
        assert_eq!(frame.len(), BLOB_HEADER_LEN + payload.len() + BLOB_CRC_LEN);
        let crc_off = frame.len() - 2;
        assert_eq!(
            u16::from_le_bytes([frame[crc_off], frame[crc_off + 1]]),
            crc16_ccitt_false(payload),
            "帧尾 CRC 必须盖住 payload"
        );

        match inspect_blob_frame(&frame) {
            FrameVerdict::Valid {
                version,
                flags,
                payload: back,
            } => {
                assert_eq!(version, BLOB_FORMAT_VERSION);
                assert_eq!(flags, 0);
                assert_eq!(back, payload);
            }
            other => panic!("自组的帧必须有效: {other:?}"),
        }
    }

    #[test]
    fn 空_payload_也是合法帧() {
        let frame = build_blob_frame(b"").expect("空 payload 组帧");
        assert_eq!(frame.len(), BLOB_HEADER_LEN + BLOB_CRC_LEN);
        assert_eq!(
            inspect_blob_frame(&frame),
            FrameVerdict::Valid {
                version: 1,
                flags: 0,
                payload: vec![]
            }
        );
    }

    #[test]
    fn payload_超限拒绝组帧() {
        let big = vec![0u8; BLOB_MAX_PAYLOAD + 1];
        assert!(build_blob_frame(&big).is_err());
        let max = vec![0u8; BLOB_MAX_PAYLOAD];
        assert!(build_blob_frame(&max).is_ok(), "上限本身必须能组");
    }

    #[test]
    fn 擦除态判从未写入() {
        let frame = vec![0xFFu8; BLOB_HEADER_LEN];
        assert_eq!(inspect_blob_frame(&frame), FrameVerdict::NeverWritten);
    }

    #[test]
    fn 坏_magic_判损坏而不是从未写入() {
        // 写过但 magic 不对——那里可能存着真实绑定，绝不能当空键盘静默覆盖。
        let mut frame = build_blob_frame(b"x").expect("组帧");
        frame[0] = b'X';
        match inspect_blob_frame(&frame) {
            FrameVerdict::Corrupt(_) => {}
            other => panic!("坏 magic 必须判损坏: {other:?}"),
        }
    }

    #[test]
    fn crc_错判损坏() {
        let mut frame = build_blob_frame(b"abc").expect("组帧");
        let last = frame.len() - 1;
        frame[last] ^= 0xFF;
        match inspect_blob_frame(&frame) {
            FrameVerdict::Corrupt(detail) => {
                assert!(detail.contains("CRC"), "要说明是 CRC: {detail}")
            }
            other => panic!("CRC 错必须判损坏: {other:?}"),
        }
    }

    #[test]
    fn 不认识的版本判损坏_不静默削() {
        let mut frame = build_blob_frame(b"abc").expect("组帧");
        frame[4] = BLOB_FORMAT_VERSION + 1;
        match inspect_blob_frame(&frame) {
            FrameVerdict::Corrupt(_) => {}
            other => panic!("未来版本必须上抛而不是当有效: {other:?}"),
        }
    }

    #[test]
    fn payload_len_越界判损坏() {
        let mut frame = build_blob_frame(b"abc").expect("组帧");
        let insane = (BLOB_MAX_PAYLOAD as u16) + 1;
        frame[6..8].copy_from_slice(&insane.to_le_bytes());
        match inspect_blob_frame(&frame) {
            FrameVerdict::Corrupt(_) => {}
            other => panic!("长度越界必须判损坏: {other:?}"),
        }
    }

    #[test]
    fn peek_帧头区分三种世界() {
        assert_eq!(peek_blob_header(&[0xFF; 8]), Some((true, 0xFFFF)));

        let payload = b"hello";
        let frame = build_blob_frame(payload).expect("组帧");
        assert_eq!(
            peek_blob_header(&frame[..8]),
            Some((false, payload.len() as u16))
        );
        assert_eq!(peek_blob_header(&frame[..7]), None, "不足 8 字节不给结论");
    }

    // ---------- 分片与重组 ----------

    #[test]
    fn 分片_56_对齐且覆盖整帧() {
        let frame = build_blob_frame(&[7u8; 200]).expect("组帧");
        let chunks = split_blob_chunks(&frame);
        let mut cursor = 0usize;
        for (offset, chunk) in &chunks {
            assert_eq!(*offset as usize % BLOB_CHUNK_SIZE, 0, "offset 必须对齐");
            assert_eq!(*offset as usize, cursor, "分片要紧挨着排");
            assert!(chunk.len() <= BLOB_CHUNK_SIZE);
            cursor += chunk.len();
        }
        assert_eq!(cursor, frame.len(), "分片合起来就是整帧");
    }

    #[test]
    fn 重组_顺序与乱序殊途同归() {
        let frame = build_blob_frame(&[3u8; 150]).expect("组帧");
        let chunks = split_blob_chunks(&frame);
        let total = frame.len() as u16;

        // 顺序
        let mut a = BlobReassembler::new();
        for (offset, chunk) in &chunks {
            a.offer(*offset, total, chunk).expect("顺序喂片");
        }
        assert!(a.is_complete());
        assert_eq!(a.frame_bytes(), Some(frame.as_slice()));

        // 乱序（倒序喂）
        let mut b = BlobReassembler::new();
        for (offset, chunk) in chunks.iter().rev() {
            b.offer(*offset, total, chunk).expect("乱序喂片");
        }
        assert!(b.is_complete());
        assert_eq!(b.frame_bytes(), Some(frame.as_slice()));
    }

    #[test]
    fn 重组_重复片幂等() {
        let frame = build_blob_frame(&[9u8; 60]).expect("组帧");
        let chunks = split_blob_chunks(&frame);
        let total = frame.len() as u16;

        let mut r = BlobReassembler::new();
        let (off0, chunk0) = &chunks[0];
        r.offer(*off0, total, chunk0).expect("第一片");
        r.offer(*off0, total, chunk0).expect("重试同一片不得报错");
        assert!(!r.is_complete(), "只收了一片不能判齐");
        for (offset, chunk) in &chunks[1..] {
            r.offer(*offset, total, chunk).expect("剩余片");
        }
        assert!(r.is_complete());
        assert_eq!(r.frame_bytes(), Some(frame.as_slice()));
    }

    #[test]
    fn 重组_拒绝非对齐_offset_与超长片() {
        let mut r = BlobReassembler::new();
        assert!(r.offer(3, 100, &[0u8; 10]).is_err(), "非对齐 offset");
        assert!(
            r.offer(0, 100, &[0u8; BLOB_CHUNK_SIZE + 1]).is_err(),
            "单片超长"
        );
    }

    // ---------- 包构造 ----------

    #[test]
    fn 读请求包布局() {
        let pkt = read_bindings_blob_packet(0x0123);
        assert_eq!(&pkt[..5], &[0x0B, 0x69, 0x02, 0x23, 0x01]);
        assert!(pkt[5..].iter().all(|&b| b == 0), "其余字节补零");
    }

    #[test]
    fn 写分片包布局() {
        let chunk = [0xAA, 0xBB, 0xCC];
        let pkt = write_bindings_blob_packet(0x0038, &chunk);
        assert_eq!(&pkt[..8], &[0x0B, 0x6A, 0x05, 0x38, 0x00, 0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn commit_包布局() {
        let pkt = commit_bindings_blob_packet(0x0100, 0x29B1);
        assert_eq!(
            &pkt[..9],
            &[0x0B, 0x6A, 0x06, 0xFF, 0xFF, 0x00, 0x01, 0xB1, 0x29]
        );
    }

    // ---------- 应答解析 ----------

    #[test]
    fn 读应答解析_hid_与_gatt() {
        // HID: [report, cmd, len, result, off(2), total(2), chunk...]
        let hid = [0x0A, 0x69, 0x09, 0x00, 0x38, 0x00, 0xC8, 0x00, 1, 2, 3, 4];
        let (offset, total, chunk) = parse_blob_read_hid_response(&hid).expect("HID 应答");
        assert_eq!(offset, 0x38);
        assert_eq!(total, 0xC8);
        assert_eq!(chunk, &[1, 2, 3, 4]);

        // GATT 剥掉 report id，整体前移 1
        let gatt = [0x69, 0x09, 0x00, 0x38, 0x00, 0xC8, 0x00, 1, 2, 3, 4];
        let (offset, total, chunk) = parse_blob_read_gatt_response(&gatt).expect("GATT 应答");
        assert_eq!(offset, 0x38);
        assert_eq!(total, 0xC8);
        assert_eq!(chunk, &[1, 2, 3, 4]);

        // result != 0 → 失败
        let bad = [0x0A, 0x69, 0x09, 0xFF, 0x38, 0x00, 0xC8, 0x00, 1, 2, 3, 4];
        assert_eq!(parse_blob_read_hid_response(&bad), None);
        // cmd 不匹配
        let wrong = [0x0A, 0x61, 0x09, 0x00, 0x38, 0x00, 0xC8, 0x00, 1, 2, 3, 4];
        assert_eq!(parse_blob_read_hid_response(&wrong), None);
        // 长度不足
        assert_eq!(parse_blob_read_hid_response(&hid[..8]), None);
        assert_eq!(parse_blob_read_gatt_response(&gatt[..7]), None);
    }

    #[test]
    fn 写应答解析() {
        // 分片 ack: [report, 0x6A, len=3, result, off_lo, off_hi]
        let hid = [0x0A, 0x6A, 0x03, 0x00, 0x38, 0x00];
        let ack = parse_blob_write_ack_hid_response(&hid).expect("分片 ack");
        assert!(ack.is_ok());
        assert_eq!(ack.offset, 0x38);

        // commit ack 带 detail: len=4
        let commit_ok = [0x0A, 0x6A, 0x04, 0x00, 0xFF, 0xFF, 0x00];
        let ack = parse_blob_write_ack_hid_response(&commit_ok).expect("commit ack");
        assert!(ack.is_ok());
        assert_eq!(ack.offset, BLOB_OFFSET_COMMIT);
        assert_eq!(ack.detail, COMMIT_DETAIL_OK);

        let commit_crc = [0x0A, 0x6A, 0x04, 0xFF, 0xFF, 0xFF, COMMIT_DETAIL_CRC];
        let ack = parse_blob_write_ack_hid_response(&commit_crc).expect("失败应答也解析");
        assert!(!ack.is_ok());
        assert_eq!(ack.detail, COMMIT_DETAIL_CRC);

        // GATT 形态
        let gatt = [0x6A, 0x04, 0xFF, 0xFF, 0xFF, COMMIT_DETAIL_INCOMPLETE];
        let ack = parse_blob_write_ack_gatt_response(&gatt).expect("GATT ack");
        assert!(!ack.is_ok());
        assert_eq!(ack.detail, COMMIT_DETAIL_INCOMPLETE);

        assert_eq!(parse_blob_write_ack_hid_response(&[0x0A, 0x6A, 0x03]), None);
        assert_eq!(
            parse_blob_write_ack_hid_response(&[0x0A, 0x61, 0x03, 0x00, 0, 0]),
            None
        );
    }

    // ---------- 会话流程（脚本化 mock 链路） ----------

    type ReadReply = Result<Option<(u16, u16, Vec<u8>)>, String>;
    type WriteReply = Result<Option<BlobWriteAck>, String>;

    #[derive(Default)]
    struct ScriptLink {
        read_replies: VecDeque<ReadReply>,
        write_replies: VecDeque<WriteReply>,
        commit_replies: VecDeque<WriteReply>,
        read_offsets: Vec<u16>,
        written: Vec<(u16, Vec<u8>)>,
        commits: Vec<(u16, u16)>,
    }

    impl BlobLink for ScriptLink {
        async fn read_chunk(&mut self, offset: u16) -> Result<Option<(u16, u16, Vec<u8>)>, String> {
            self.read_offsets.push(offset);
            self.read_replies.pop_front().expect("脚本里没有更多读应答")
        }

        async fn write_chunk(
            &mut self,
            offset: u16,
            chunk: &[u8],
        ) -> Result<Option<BlobWriteAck>, String> {
            self.written.push((offset, chunk.to_vec()));
            self.write_replies
                .pop_front()
                .expect("脚本里没有更多写应答")
        }

        async fn commit(
            &mut self,
            total_len: u16,
            crc16: u16,
        ) -> Result<Option<BlobWriteAck>, String> {
            self.commits.push((total_len, crc16));
            self.commit_replies
                .pop_front()
                .expect("脚本里没有更多 commit 应答")
        }
    }

    fn ok_read(offset: u16, total: u16, chunk: &[u8]) -> ReadReply {
        Ok(Some((offset, total, chunk.to_vec())))
    }

    fn ok_write(offset: u16) -> WriteReply {
        Ok(Some(BlobWriteAck {
            result: 0,
            offset,
            detail: 0,
        }))
    }

    /// 把一帧按真实固件的行为切成读应答脚本（56 对齐、total 一致）。
    fn scripted_reads(frame: &[u8]) -> VecDeque<ReadReply> {
        split_blob_chunks(frame)
            .into_iter()
            .map(|(offset, chunk)| ok_read(offset, frame.len() as u16, chunk))
            .collect()
    }

    #[tokio::test]
    async fn 读流程_从未写入() {
        let mut link = ScriptLink::default();
        // 固件对擦除态只回报 8 字节头（total=8，内容全 0xFF）
        link.read_replies
            .push_back(ok_read(0, BLOB_HEADER_LEN as u16, &[0xFF; 8]));
        let read = read_blob(&mut link).await.expect("读流程");
        assert_eq!(read, BlobRead::NeverWritten);
        assert_eq!(link.read_offsets, vec![0], "看到擦除态就该停手");
    }

    #[tokio::test]
    async fn 读流程_超时判不支持() {
        let mut link = ScriptLink::default();
        link.read_replies.push_back(Ok(None)); // 旧固件不回包
        let read = read_blob(&mut link).await.expect("超时不算传输错误");
        assert_eq!(read, BlobRead::Unsupported);
    }

    #[tokio::test]
    async fn 读流程_多片有效帧() {
        let payload = vec![5u8; 180];
        let frame = build_blob_frame(&payload).expect("组帧");
        let mut link = ScriptLink {
            read_replies: scripted_reads(&frame),
            ..Default::default()
        };
        let read = read_blob(&mut link).await.expect("读流程");
        assert_eq!(
            read,
            BlobRead::Valid {
                version: 1,
                flags: 0,
                payload
            }
        );
        // 请求 offset 必须 56 对齐递增
        assert!(link.read_offsets.iter().all(|o| o % 56 == 0));
        assert!(link.read_offsets.windows(2).all(|w| w[1] > w[0]));
    }

    #[tokio::test]
    async fn 读流程_crc_损坏上抛() {
        let mut frame = build_blob_frame(b"real bindings").expect("组帧");
        let last = frame.len() - 1;
        frame[last] ^= 0x01;
        let mut link = ScriptLink {
            read_replies: scripted_reads(&frame),
            ..Default::default()
        };
        match read_blob(&mut link).await.expect("读流程") {
            BlobRead::Corrupt(_) => {}
            other => panic!("损坏必须上抛: {other:?}"),
        }
    }

    #[tokio::test]
    async fn 读流程_零长片不死循环() {
        // 固件回零长片（chunk 空）会让 next_offset 不前进、is_complete 永假。
        // 护栏必须把这种情况变成 Err，而非挂死 SDK。
        let payload = vec![9u8; 120];
        let frame = build_blob_frame(&payload).expect("组帧");
        let total = frame.len() as u16;
        let mut replies = VecDeque::new();
        // 首片：完整帧头 + 前 56 字节（正常）
        replies.push_back(ok_read(0, total, &frame[..56]));
        // 次片：offset=56 但 chunk 空（异常固件行为）
        replies.push_back(Ok(Some((56, total, Vec::new()))));
        let mut link = ScriptLink {
            read_replies: replies,
            ..Default::default()
        };
        let result = read_blob(&mut link).await;
        assert!(result.is_err(), "零长片必须报错而非死循环/静默接受");
        // 确护栏生效：请求过 offset 0 和 56 各一次后就中止，不会反复请求 56
        assert_eq!(link.read_offsets, vec![0, 56]);
    }

    #[tokio::test]
    async fn 写流程_分片_commit_回读全链路() {
        let payload = b"{\"v\":1,\"bindings\":{\"key.tab\":{}}}".to_vec();
        let frame = build_blob_frame(&payload).expect("组帧");
        let n_chunks = split_blob_chunks(&frame).len();

        let mut link = ScriptLink::default();
        for (offset, _) in split_blob_chunks(&frame) {
            link.write_replies.push_back(ok_write(offset));
        }
        link.commit_replies.push_back(ok_write(BLOB_OFFSET_COMMIT));
        // 回读校验：固件把刚写入的帧原样读回
        link.read_replies = scripted_reads(&frame);

        write_blob(&mut link, &payload).await.expect("写流程");

        assert_eq!(link.written.len(), n_chunks, "每片都要写一次");
        assert_eq!(link.commits.len(), 1, "commit 恰好一次");
        let (total, crc) = link.commits[0];
        assert_eq!(total as usize, frame.len());
        assert_eq!(crc, crc16_ccitt_false(&payload));
        // 写全在 commit 之前，回读在 commit 之后
        assert!(!link.read_offsets.is_empty(), "必须有回读校验");
    }

    #[tokio::test]
    async fn 写流程_commit_被拒不落盘也不谎报() {
        let payload = b"x".to_vec();
        let frame = build_blob_frame(&payload).expect("组帧");
        let mut link = ScriptLink::default();
        for (offset, _) in split_blob_chunks(&frame) {
            link.write_replies.push_back(ok_write(offset));
        }
        link.commit_replies.push_back(Ok(Some(BlobWriteAck {
            result: 0xFF,
            offset: BLOB_OFFSET_COMMIT,
            detail: COMMIT_DETAIL_CRC,
        })));

        let err = write_blob(&mut link, &payload).await.unwrap_err();
        assert_eq!(err, BlobWriteError::CommitRejected(COMMIT_DETAIL_CRC));
    }

    #[tokio::test]
    async fn 写流程_首片超时判不支持() {
        let mut link = ScriptLink::default();
        link.write_replies.push_back(Ok(None));
        let err = write_blob(&mut link, b"x").await.unwrap_err();
        assert_eq!(err, BlobWriteError::Unsupported);
    }

    #[tokio::test]
    async fn 写流程_commit_超时靠回读终裁() {
        // commit 应答丢了不代表没落盘——回读到我们的数据就算成功，
        // 否则报 VerifyMismatch，绝不能直接谎报成功。
        let payload = b"abc".to_vec();
        let frame = build_blob_frame(&payload).expect("组帧");

        // 情形一：回读到一致数据 → 成功
        let mut link = ScriptLink::default();
        for (offset, _) in split_blob_chunks(&frame) {
            link.write_replies.push_back(ok_write(offset));
        }
        link.commit_replies.push_back(Ok(None)); // 应答丢失
        link.read_replies = scripted_reads(&frame);
        write_blob(&mut link, &payload)
            .await
            .expect("回读一致即成功");

        // 情形二：回读对不上 → VerifyMismatch
        let mut link = ScriptLink::default();
        for (offset, _) in split_blob_chunks(&frame) {
            link.write_replies.push_back(ok_write(offset));
        }
        link.commit_replies.push_back(Ok(None));
        link.read_replies
            .push_back(ok_read(0, BLOB_HEADER_LEN as u16, &[0xFF; 8])); // 还是擦除态
        let err = write_blob(&mut link, &payload).await.unwrap_err();
        assert_eq!(err, BlobWriteError::VerifyMismatch);
    }

    #[tokio::test]
    async fn 写流程_回读不一致判校验失败() {
        let payload = b"abc".to_vec();
        let frame = build_blob_frame(&payload).expect("组帧");
        let other = build_blob_frame(b"different").expect("组帧");

        let mut link = ScriptLink::default();
        for (offset, _) in split_blob_chunks(&frame) {
            link.write_replies.push_back(ok_write(offset));
        }
        link.commit_replies.push_back(ok_write(BLOB_OFFSET_COMMIT));
        link.read_replies = scripted_reads(&other); // 回读到别人的数据
        let err = write_blob(&mut link, &payload).await.unwrap_err();
        assert_eq!(err, BlobWriteError::VerifyMismatch);
    }

    #[tokio::test]
    async fn 写流程_payload_超限不进链路() {
        let mut link = ScriptLink::default();
        let big = vec![0u8; BLOB_MAX_PAYLOAD + 1];
        let err = write_blob(&mut link, &big).await.unwrap_err();
        assert_eq!(err, BlobWriteError::TooLarge);
        assert!(link.written.is_empty(), "超限一片都不许发");
    }
}
