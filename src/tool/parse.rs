//! 设备信息解析工具(从 V1 device.rs 搬来)。
//!
//! 把 CMD 0x13 的原始字节解析成 [`DeviceInfo`],USB(payload_offset=4)和
//! GATT(payload_offset=0,无 Report ID)共用核心解析逻辑。

use anyhow::Result;

use crate::kernel::event::DeviceInfo;
use crate::kernel::protocol_hid::CMD_GET_DEVICE_INFO;
use crate::kernel::types::ConnectionType;

/// 从 HID 缓冲区解析设备信息(payload_offset:HID=4 / GATT 已转 buf=0)
pub fn parse_device_info_from_buf(
    buf: &[u8],
    payload_offset: usize,
    conn_type: ConnectionType,
) -> Result<DeviceInfo> {
    let p = payload_offset;
    if buf.len() < p + 20 {
        return Err(anyhow::anyhow!("设备信息 payload 长度不足"));
    }
    let mode = buf[p];
    let mac = format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        buf[p + 1],
        buf[p + 2],
        buf[p + 3],
        buf[p + 4],
        buf[p + 5],
        buf[p + 6]
    );
    let receiver_version = format!("{}.{}", buf[p + 8], buf[p + 7]);
    let firmware_version = format!("{}.{}", buf[p + 10], buf[p + 9]);
    let battery_charging = buf[p + 13] != 0;
    let battery_level = buf[p + 14];
    let battery_full = buf[p + 15] != 0;
    let chip_id = format!(
        "{:02X}{:02X}{:02X}{:02X}",
        buf[p + 16],
        buf[p + 17],
        buf[p + 18],
        buf[p + 19]
    );
    Ok(DeviceInfo {
        mode,
        mac_address: mac,
        receiver_version,
        firmware_version,
        battery_level,
        battery_charging,
        battery_full,
        chip_id,
        connection_type: conn_type,
    })
}

/// 从 GATT 响应解析设备信息(GATT 无 Report ID,payload 从 `response[3]` 开始)
pub fn parse_device_info_from_gatt(
    response: &[u8],
    conn_type: ConnectionType,
) -> Result<DeviceInfo> {
    if response.len() < 3 + 20 {
        return Err(anyhow::anyhow!("设备信息响应长度不足: {}", response.len()));
    }
    if response[0] != CMD_GET_DEVICE_INFO {
        return Err(anyhow::anyhow!("响应 CMD 不匹配: 0x{:02X}", response[0]));
    }
    if response[2] != 0x00 {
        return Err(anyhow::anyhow!("查询失败: result=0x{:02X}", response[2]));
    }
    let mut buf = [0u8; 64];
    let payload_len = (response.len() - 3).min(64);
    buf[..payload_len].copy_from_slice(&response[3..3 + payload_len]);
    parse_device_info_from_buf(&buf, 0, conn_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ble_device_info_carries_battery_and_firmware() {
        let mut payload = [0u8; 20];
        payload[0] = 2; // BLE transport mode
        payload[7] = 55;
        payload[8] = 1;
        payload[9] = 55;
        payload[10] = 1;
        payload[13] = 0;
        payload[14] = 73;
        payload[15] = 0;
        payload[16..20].copy_from_slice(&[0x1C, 0xE6, 0x07, 0x29]);

        let mut response = vec![CMD_GET_DEVICE_INFO, payload.len() as u8, 0x00];
        response.extend_from_slice(&payload);

        let info = parse_device_info_from_gatt(&response, ConnectionType::Ble).unwrap();
        assert_eq!(info.connection_type, ConnectionType::Ble);
        assert_eq!(info.firmware_version, "1.55");
        assert_eq!(info.battery_level, 73);
        assert_eq!(info.chip_id, "1CE60729");
    }
}
