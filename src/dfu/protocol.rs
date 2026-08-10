//! USB HID DFU 协议定义（固件升级）。
//!
//! 与本设备固件约定的自定义 HID DFU 协议，
//! **非标准 USB DFU class**，使用 vendor HID report。
//!
//! See the firmware-side protocol documentation (DFU upgrade spec) for
//! full frame-level details.
//!
//! ## 协议要点
//!
//! - **USB-only**：进入 DFU 通过正常模式 USB HID CMD `0xEF`，BLE 无法进入。
//! - **两段 PID**：正常模式 `0xED20`（report `0x0B`/`0x0A`，64B），DFU 模式
//!   `0xFF06`（report `0xA2` out 256B / `0xA1` in 64B）。
//! - **状态机**：进 DFU → 等设备枚举 → PREPARE(总长度) → START → DATA 循环
//!   (250B/包) → END → 等设备重启回正常模式。
//! - **校验**：逐字节 `u16` wrapping 累加（非 CRC32，非签名）。
//! - **防砖**：固件写入 `PARTITION_FOTA_DATA` 暂存分区，END 触发验证；失败/中断
//!   发 END 让设备重启回旧固件。

// ============ 设备标识 ============

/// DFU 模式 Product ID（正常模式为 0xED20，进 DFU 后 PID 切到 0xFF06）
pub const DFU_PID: u16 = 0xFF06;

/// DFU 输出 Report ID (Host → Device)
pub const DFU_REPORT_ID_OUTPUT: u8 = 0xA2;

/// DFU 输入 Report ID (Device → Host)
#[allow(dead_code)]
pub const DFU_REPORT_ID_INPUT: u8 = 0xA1;

/// DFU 输出包最大大小（含 Report ID）
pub const DFU_OUTPUT_MAX_SIZE: usize = 256;

/// DFU 输入包最大大小（含 Report ID）
pub const DFU_INPUT_MAX_SIZE: usize = 64;

/// DFU DATA 包最大载荷
pub const DFU_DATA_PAYLOAD_MAX: usize = 250;

// ============ 状态机 Flag ============

/// PREPARE — 通知设备固件总大小
pub const FLAG_PREPARE: u8 = 0xFF;

/// START — 初始化 DFU 缓冲区和 Flash 分区
pub const FLAG_START: u8 = 0x01;

/// DATA — 固件数据包
pub const FLAG_DATA: u8 = 0x02;

/// END — 传输完成，触发验证和重启
pub const FLAG_END: u8 = 0x00;

// ============ Result ============

pub const RESULT_SUCCESS: u8 = 0x00;
#[allow(dead_code)]
pub const RESULT_FAIL: u8 = 0xFF;

// ============ 超时 ============

/// 等待 DFU 设备枚举超时（秒）
pub const WAIT_DFU_DEVICE_TIMEOUT_SECS: u64 = 10;

/// 等待设备重启回正常模式超时（秒）
pub const WAIT_NORMAL_DEVICE_TIMEOUT_SECS: u64 = 15;

/// HID 读写超时（毫秒）—— DATA 包用
pub const DFU_RW_TIMEOUT_MS: i32 = 3000;

/// PREPARE/START 阶段较长超时（固件需初始化/擦除 Flash 分区）。
///
/// 早期用 DFU_RW_TIMEOUT_MS(3000ms) 偶发 "DFU 响应超时"：
/// 固件响应 PREPARE 实测约 1.8~2s，3s 余量不足，设备繁忙时会超时。
pub const DFU_PREPARE_TIMEOUT_MS: i32 = 8000;

/// END 阶段较长超时（设备需要验证 Flash）
pub const DFU_END_TIMEOUT_MS: i32 = 5000;

// ============ DFU 包编码 ============

/// 逐字节累加校验和（u16 wrapping）
pub fn compute_checksum(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |acc, &b| acc.wrapping_add(b as u16))
}

/// DFU 输出包编码器（PREPARE / START / DATA / END 四种包）
pub struct DfuPacketEncoder;

impl DfuPacketEncoder {
    /// 编码 PREPARE 包。
    ///
    /// 格式: `[0xA2][0xFF][total_len:u32 LE][checksum:u16 LE][padding...]`
    ///
    /// PREPARE 的 checksum 是 `total_length` 本身的逐字节 u16 累加。
    pub fn prepare(total_length: u32) -> Vec<u8> {
        let mut buf = vec![0u8; DFU_OUTPUT_MAX_SIZE];
        buf[0] = DFU_REPORT_ID_OUTPUT;
        buf[1] = FLAG_PREPARE;
        buf[2..6].copy_from_slice(&total_length.to_le_bytes());
        let cs = compute_checksum(&total_length.to_le_bytes());
        buf[6..8].copy_from_slice(&cs.to_le_bytes());
        buf
    }

    /// 编码 START 包。
    ///
    /// 格式: `[0xA2][0x01][0x00 0x00][0x00 0x00][padding...]`
    pub fn start() -> Vec<u8> {
        let mut buf = vec![0u8; DFU_OUTPUT_MAX_SIZE];
        buf[0] = DFU_REPORT_ID_OUTPUT;
        buf[1] = FLAG_START;
        buf
    }

    /// 编码 DATA 包。
    ///
    /// 格式: `[0xA2][0x02][packet_size:u16 LE][checksum:u16 LE][data...][padding...]`
    ///
    /// `payload` 不得超过 [`DFU_DATA_PAYLOAD_MAX`]（250B），否则返回 `Err`。
    pub fn data(payload: &[u8]) -> anyhow::Result<Vec<u8>> {
        if payload.len() > DFU_DATA_PAYLOAD_MAX {
            return Err(anyhow::anyhow!(
                "DATA 载荷超长: {} > {}",
                payload.len(),
                DFU_DATA_PAYLOAD_MAX
            ));
        }
        let mut buf = vec![0u8; DFU_OUTPUT_MAX_SIZE];
        buf[0] = DFU_REPORT_ID_OUTPUT;
        buf[1] = FLAG_DATA;
        let packet_size = payload.len() as u16;
        buf[2..4].copy_from_slice(&packet_size.to_le_bytes());
        let cs = compute_checksum(payload);
        buf[4..6].copy_from_slice(&cs.to_le_bytes());
        buf[6..6 + payload.len()].copy_from_slice(payload);
        Ok(buf)
    }

    /// 编码 END 包。
    ///
    /// 格式: `[0xA2][0x00][0x00 0x00][0x00 0x00][padding...]`
    pub fn end() -> Vec<u8> {
        let mut buf = vec![0u8; DFU_OUTPUT_MAX_SIZE];
        buf[0] = DFU_REPORT_ID_OUTPUT;
        buf[1] = FLAG_END;
        buf
    }
}

/// DFU 响应（设备 → host，in 包 64B）。
///
/// 格式: `[flag][packet_size:u16 LE][checksum:u16 LE][result:u8][total_written:u32 LE]`
#[derive(Debug)]
#[allow(dead_code)]
pub struct DfuResponse {
    pub flag: u8,
    pub packet_size: u16,
    pub checksum: u16,
    pub result: u8,
    pub total_written: u32,
}

impl DfuResponse {
    /// 从 HID 输入数据解析（**调用方应先跳过 Report ID 字节**）。
    ///
    /// 最少需要: `flag(1) + packet_size(2) + checksum(2) + result(1) + total_written(4) = 10` 字节。
    pub fn parse(data: &[u8]) -> anyhow::Result<Self> {
        if data.len() < 10 {
            return Err(anyhow::anyhow!("DFU 响应长度不足: {} < 10", data.len()));
        }

        let flag = data[0];
        let packet_size = u16::from_le_bytes([data[1], data[2]]);
        let checksum = u16::from_le_bytes([data[3], data[4]]);
        let result = data[5];
        let total_written = u32::from_le_bytes([data[6], data[7], data[8], data[9]]);

        Ok(Self {
            flag,
            packet_size,
            checksum,
            result,
            total_written,
        })
    }

    pub fn is_success(&self) -> bool {
        self.result == RESULT_SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checksum_empty() {
        assert_eq!(compute_checksum(&[]), 0);
    }

    #[test]
    fn test_checksum_simple() {
        assert_eq!(compute_checksum(&[0x01, 0x02, 0x03]), 6);
    }

    #[test]
    fn test_checksum_overflow() {
        // u16 wrapping: 0xFFFF + 1 = 0
        let data = vec![0xFF; 257]; // 255 * 257 = 65535 wrapping
        let cs = compute_checksum(&data);
        assert_eq!(cs, 255u16.wrapping_mul(257));
    }

    #[test]
    fn test_prepare_packet() {
        let packet = DfuPacketEncoder::prepare(0x10000);
        assert_eq!(packet.len(), DFU_OUTPUT_MAX_SIZE);
        assert_eq!(packet[0], DFU_REPORT_ID_OUTPUT);
        assert_eq!(packet[1], FLAG_PREPARE);
        assert_eq!(&packet[2..6], &0x10000u32.to_le_bytes());
    }

    #[test]
    fn test_start_packet() {
        let packet = DfuPacketEncoder::start();
        assert_eq!(packet[0], DFU_REPORT_ID_OUTPUT);
        assert_eq!(packet[1], FLAG_START);
    }

    #[test]
    fn test_end_packet() {
        let packet = DfuPacketEncoder::end();
        assert_eq!(packet[0], DFU_REPORT_ID_OUTPUT);
        assert_eq!(packet[1], FLAG_END);
    }

    #[test]
    fn test_data_packet() {
        let payload = vec![0xAB; 100];
        let packet = DfuPacketEncoder::data(&payload).unwrap();
        assert_eq!(packet[0], DFU_REPORT_ID_OUTPUT);
        assert_eq!(packet[1], FLAG_DATA);
        assert_eq!(u16::from_le_bytes([packet[2], packet[3]]), 100);
        assert_eq!(&packet[6..106], &payload[..]);
    }

    #[test]
    fn test_data_packet_at_max_payload() {
        let payload = vec![0u8; DFU_DATA_PAYLOAD_MAX];
        assert!(DfuPacketEncoder::data(&payload).is_ok());
    }

    #[test]
    fn test_data_packet_too_large() {
        let payload = vec![0u8; DFU_DATA_PAYLOAD_MAX + 1];
        assert!(DfuPacketEncoder::data(&payload).is_err());
    }

    #[test]
    fn test_response_parse() {
        let mut data = vec![0u8; 64];
        data[0] = FLAG_DATA;
        data[1..3].copy_from_slice(&100u16.to_le_bytes());
        data[3..5].copy_from_slice(&0x1234u16.to_le_bytes());
        data[5] = RESULT_SUCCESS;
        data[6..10].copy_from_slice(&5000u32.to_le_bytes());

        let resp = DfuResponse::parse(&data).unwrap();
        assert_eq!(resp.flag, FLAG_DATA);
        assert_eq!(resp.packet_size, 100);
        assert_eq!(resp.checksum, 0x1234);
        assert_eq!(resp.result, RESULT_SUCCESS);
        assert_eq!(resp.total_written, 5000);
        assert!(resp.is_success());
    }

    #[test]
    fn test_response_parse_fail() {
        let mut data = vec![0u8; 64];
        data[5] = RESULT_FAIL;
        let resp = DfuResponse::parse(&data).unwrap();
        assert!(!resp.is_success());
    }

    #[test]
    fn test_response_parse_too_short() {
        let data = vec![0u8; 9];
        assert!(DfuResponse::parse(&data).is_err());
    }
}
