//! Vendor GATT 协议常量和数据解析
//!
//! BLE_CONN_MODE=1 时固件注册自定义 Vendor GATT Service。
//! 包格式与 HID 相同：`[CMD][LEN][DATA...]`

use uuid::Uuid;

// ============ UUID ============
//
// 固件使用 Nordic SDK 自定义 UUID 格式（16-bit 值在末尾）：
// `00000000-0000-0000-0000-00000000XXXX`
// CoreBluetooth 发现后报告的就是这个格式。

/// Vendor GATT Service
#[allow(dead_code)]
pub const SERVICE_UUID: Uuid = Uuid::from_u128(0xFE60);
/// Command 特征值（Write, Host→Device）
pub const CMD_CHAR_UUID: Uuid = Uuid::from_u128(0xFE61);
/// Event 特征值（Notify, Device→Host）
pub const EVENT_CHAR_UUID: Uuid = Uuid::from_u128(0xFE62);
/// Audio 特征值（Notify, Device→Host）
pub const AUDIO_CHAR_UUID: Uuid = Uuid::from_u128(0xFE63);

/// Vendor GATT 设备名前缀
pub const VENDOR_DEVICE_PREFIX: &str = "REAI_VB_";

/// Audio 帧标志
#[allow(dead_code)]
pub const AUDIO_FLAG_SILENCE: u8 = 0;
pub const AUDIO_FLAG_DATA: u8 = 1;

// ============ 包解析 ============

/// 解析命令/事件包：`[CMD][LEN][DATA...]`
pub fn parse_packet(data: &[u8]) -> Option<(u8, u8, &[u8])> {
    if data.len() < 2 {
        return None;
    }
    let cmd = data[0];
    let len = data[1] as usize;
    if data.len() < 2 + len {
        return None;
    }
    Some((cmd, data[1], &data[2..2 + len]))
}

/// 解析音频包：`[FLAG][LEN][DATA...]`
/// FLAG=0 静音，FLAG=1 有 mSBC 数据
/// LEN 可能大于实际数据（固件缓冲区截断），取 min(len, data.len()-2)
pub fn parse_audio_packet(data: &[u8]) -> Option<(u8, &[u8])> {
    if data.len() < 2 {
        return None;
    }
    let flag = data[0];
    let len = data[1] as usize;
    if flag != AUDIO_FLAG_DATA {
        return None;
    }
    let available = data.len().saturating_sub(2);
    let actual_len = len.min(available);
    if actual_len == 0 {
        return None;
    }
    Some((flag, &data[2..2 + actual_len]))
}

/// 将 HID 64 字节包转换为 GATT 命令（去掉 Report ID 和 padding）
///
/// HID: `[ReportID=0x0B][CMD][LEN][DATA...padding]`
/// GATT: `[CMD][LEN][DATA...]`
pub fn hid_to_gatt_command(hid_packet: &[u8; 64]) -> Vec<u8> {
    let cmd = hid_packet[1];
    // 钳到剩余可读字节数,防止畸形 len(可达 255)导致切片越界 panic。
    // HID 包固定 64 字节,去掉 [report_id][cmd][len] 三个字节后剩余最多 61 字节。
    let len = (hid_packet[2] as usize).min(61);
    let mut result = Vec::with_capacity(2 + len);
    result.push(cmd);
    result.push(hid_packet[2]);
    if len > 0 {
        result.extend_from_slice(&hid_packet[3..3 + len]);
    }
    result
}
