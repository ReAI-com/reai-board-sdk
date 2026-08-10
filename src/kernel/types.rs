//! 基础类型(无依赖层)。
//!
//! `ConnectionType` 在 V1 放在 `hid::device`,被 event/protocol/monitor/device_manager/hotplug
//! 多处引用。V2 提到 `kernel::types` 解耦——它是协议/事件/IO 共用的基础枚举,不该绑死在 hid 模块。

use serde::{Deserialize, Serialize};

/// 连接类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionType {
    /// USB 有线连接
    Usb,
    /// BLE 无线连接
    Ble,
}

impl std::fmt::Display for ConnectionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usb => write!(f, "usb"),
            Self::Ble => write!(f, "ble"),
        }
    }
}

/// 软休眠超时配置（固件 v1.51+，CMD 0x63/0x64）。单位：秒。
///
/// 两个值独立可配置：未连接 / 已连接时的闲置超时。固件范围 `30~65535`，
/// `<30` 会被固件钳制到 30。默认 disconnected=60、connected=600。
/// SET 返回固件钳制后的生效值（以响应为准，而非调用参数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SleepTimeout {
    /// 未连接时闲置超时（默认 60s，下限 30）
    pub disconnected: u16,
    /// 已连接时闲置超时（默认 600s，下限 30）
    pub connected: u16,
}

impl SleepTimeout {
    /// 从两个 u16 构造（秒）。
    pub const fn new(disconnected: u16, connected: u16) -> Self {
        Self {
            disconnected,
            connected,
        }
    }
}

/// USB Audio 设备名称匹配关键词(统一维护)
///
/// 注意:不含 "hid",因为太宽泛会误匹配其他 HID 设备。
const USB_AUDIO_KEYWORDS: &[&str] = &["audio-hid", "reai", "vibe", "ai vibe"];

/// 检查 cpal 设备名称是否匹配本设备的 USB Audio 接口
pub fn is_usb_audio_device_name(name: &str) -> bool {
    let name_lower = name.to_lowercase();
    USB_AUDIO_KEYWORDS.iter().any(|kw| name_lower.contains(kw))
}
