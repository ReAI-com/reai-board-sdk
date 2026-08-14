//! 🔌 HID 协议定义模块
//!
//! 定义 AI 键盘 HID 通信所需的常量、数据结构和协议函数

use serde::{Deserialize, Serialize};

#[cfg(feature = "test-mode")]
use crate::kernel::error::{BoardError, Result};
#[cfg(feature = "test-mode")]
use crate::kernel::event::FactoryKeyEvent;

// ============ 设备标识 ============

/// USB Vendor ID
pub const VID: u16 = 0x363C;

/// USB Product ID
pub const PID_USB: u16 = 0xED20;

/// BLE Product ID（实测与 USB 相同）
pub const PID_BLE: u16 = 0xED20;

/// 兼容旧代码的别名（USB PID）
pub const PID: u16 = PID_USB;

/// 判断给定的 PID 是否为目标设备（USB 或 BLE）
pub fn is_target_pid(pid: u16) -> bool {
    pid == PID_USB || pid == PID_BLE
}

/// 根据连接类型返回对应的 PID
#[allow(dead_code)]
pub fn target_pid(conn_type: crate::kernel::types::ConnectionType) -> u16 {
    match conn_type {
        crate::kernel::types::ConnectionType::Usb => PID_USB,
        crate::kernel::types::ConnectionType::Ble => PID_BLE,
    }
}

// ============ Report IDs ============

/// 输入数据 Report ID (按键/状态)
pub const REPORT_ID_INPUT: u8 = 0x0A;

/// 输出命令 Report ID
pub const REPORT_ID_OUTPUT: u8 = 0x0B;

/// 按键事件 Report ID (实际观察)
pub const REPORT_ID_KEY_EVENT: u8 = 0x02;

/// 音频数据 Report ID
#[allow(dead_code)]
pub const REPORT_ID_AUDIO: u8 = 0xB1;

pub const AUDIO_ENVELOPE_VERSION: u8 = 1;
pub const AUDIO_FLAG_DATA: u8 = 0x01;
pub const AUDIO_FLAG_DISCONTINUITY: u8 = 0x02;

// ============ 命令码 ============

/// 获取按键配置命令
pub const CMD_GET_KEY_SETTING: u8 = 0x15;

/// 设置按键配置命令
pub const CMD_SET_KEY_SETTING: u8 = 0x16;

/// 状态命令
pub const CMD_STATUS: u8 = 0x12;

/// 获取设备信息命令（含电量）
pub const CMD_GET_DEVICE_INFO: u8 = 0x13;

/// BLE 音频数据命令（BLE 模式下通过 Report ID 0x0A + CMD 0x01 传输 mSBC 帧）
pub const CMD_AUDIO_DATA: u8 = 0x01;

/// 工作模式数据子命令
pub const CMD_WORK_MODE_DATA: u8 = 0xC9;

/// AI 关机命令 (0x5E)
/// action=0x01: 保留配对关机
/// action=0x02: 清除配对关机
#[cfg(feature = "test-mode")]
pub const CMD_AI_SHUTDOWN: u8 = 0x5E;

/// 设备主动断开通知 (0x60)
///
/// 固件在关机 / 用户主动断开前通过 Vendor GATT 事件通道发送。客户端收到后应
/// 停止自动重连（与用户手动断开同等级别），区别于超时 / 超距离等异常断开
/// （那些应自动重连）。macOS CoreBluetooth 不暴露 HCI disconnect reason，
/// 故用此显式事件代替固件零改动方案。
pub const CMD_DEVICE_DISCONNECT: u8 = 0x60;

/// 读取固件持久化的静默录音标志（固件 v1.41+）
pub const CMD_GET_SILENT_RECORD: u8 = 0x61;

/// 设置并持久化静默录音标志（固件 v1.41+）
pub const CMD_SET_SILENT_RECORD: u8 = 0x62;

/// 读取软休眠超时（固件 v1.51+）：未连接 / 已连接两组，单位秒
pub const CMD_GET_SLEEP_TIMEOUT: u8 = 0x63;

/// 设置并持久化软休眠超时（固件 v1.51+）：SET 响应回显钳制后生效值
pub const CMD_SET_SLEEP_TIMEOUT: u8 = 0x64;

/// App 上报在线状态（固件 v1.53+）：0=离线 1=在线。
/// App 连接成功后自动发上线；SDK shutdown 前自动发下线。
pub const CMD_AI_APP_ONLINE_NOTIFY: u8 = 0x65;

/// 查询 App 在线状态（固件 v1.53+）
pub const CMD_AI_GET_APP_ONLINE: u8 = 0x66;

/// 获取离线开网页 URL（固件 v1.53+）
pub const CMD_AI_GET_OPEN_URL: u8 = 0x67;

/// 设置离线开网页 URL 并持久化（固件 v1.53+）
pub const CMD_AI_SET_OPEN_URL: u8 = 0x68;

/// 分片读取绑定配置块（固件 v1.55+）。
///
/// 请求 `[offset(2 LE)]`，应答 `[result][offset(2)][total_len(2)][chunk(≤56)]`。
/// 按 offset 重复拉取直到收齐 total_len；旧固件不回包 → 超时判「不支持」。
pub const CMD_AI_READ_BINDINGS_BLOB: u8 = 0x69;

/// 分片写入绑定配置块（固件 v1.55+）。
///
/// 写分片 `[offset(2 LE)][chunk(≤56)]` 应答 ack；offset=0xFFFF 为 commit：
/// `[0xFFFF][total_len(2)][crc16(2)]`，固件校验齐全+CRC 后落盘并回读校验。
pub const CMD_AI_WRITE_BINDINGS_BLOB: u8 = 0x6A;

/// Versioned board-audio capability query (firmware 1.59+).
pub const CMD_AI_GET_AUDIO_CAPABILITIES: u8 = 0x6E;

/// Volatile short-TTL audio stream lease control (firmware 1.59+).
pub const CMD_AI_AUDIO_STREAM_CONTROL: u8 = 0x6F;

/// 工厂物理按键测试控制（固件 v1.58+）。
#[cfg(feature = "test-mode")]
pub const CMD_AI_FACTORY_KEY_TEST_CONTROL: u8 = 0x6C;

/// 工厂物理按键异步事件（固件 v1.58+）。
#[cfg(feature = "test-mode")]
pub const CMD_AI_FACTORY_KEY_EVENT: u8 = 0x6D;

#[cfg(feature = "test-mode")]
pub const FACTORY_KEY_TEST_PROTOCOL_VERSION: u8 = 0x01;

#[cfg(feature = "test-mode")]
pub const FACTORY_KEY_TEST_INPUT_COUNT: u8 = 12;

#[cfg(feature = "test-mode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum FactoryKeyControlResult {
    Ok = 0,
    UnsupportedVersion = 1,
    Busy = 2,
    SessionMismatch = 3,
    InvalidRequest = 4,
}

#[cfg(feature = "test-mode")]
impl TryFrom<u8> for FactoryKeyControlResult {
    type Error = BoardError;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Ok),
            1 => Ok(Self::UnsupportedVersion),
            2 => Ok(Self::Busy),
            3 => Ok(Self::SessionMismatch),
            4 => Ok(Self::InvalidRequest),
            _ => Err(BoardError::Protocol(format!(
                "未知工厂按键测试结果: {value}"
            ))),
        }
    }
}

#[cfg(feature = "test-mode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactoryKeyControlAck {
    pub result: FactoryKeyControlResult,
    pub enabled: bool,
    pub session: u16,
}

/// AI 语音键默认媒体码：出厂配置使用 KEY_MEDIA + 0x0F04
pub const AI_VOICE_MEDIA_CODE: u16 = 0x0F04;

/// AI 语音键固件内部标记码：按键配置为 KEY_AI_VOICE 时固件发送 0x0F99
pub const AI_VOICE_MARK_CODE: u16 = 0x0F99;

/// 判断 Consumer code 是否表示 AI 语音键
pub fn is_ai_voice_consumer_code(code: u16) -> bool {
    matches!(code, AI_VOICE_MEDIA_CODE | AI_VOICE_MARK_CODE)
}

// ============ 数据包大小 ============

/// HID 数据包大小
pub const PACKET_SIZE: usize = 64;

/// 按键数据长度 (20 个按键 × 3 字节)
pub const KEY_DATA_LEN: usize = 60;

/// 按键总数
pub const KEY_COUNT: usize = 20;

/// 实际使用的按键数量
pub const ACTIVE_KEY_COUNT: usize = 12;

// ============ 音频参数 ============

/// 采样率
#[allow(dead_code)]
pub const SAMPLE_RATE: u32 = 16000;

/// mSBC 帧大小
pub const MSBC_FRAME_SIZE: usize = 57;

/// 每帧解码后的 PCM 样点数
#[allow(dead_code)]
pub const DECODED_SAMPLES_PER_FRAME: usize = 120;

/// 音频数据包中的 flag 偏移
#[allow(dead_code)]
pub const MSBC_FLAG_OFFSET: usize = 1;

/// 音频数据包中的 payload 长度偏移
#[allow(dead_code)]
pub const MSBC_LEN_OFFSET: usize = 2;

/// 音频数据包中的 mSBC 数据偏移。
/// BLE 固件格式: `[ReportID=0x0A][CMD=0x01][Len=0x39][57 bytes mSBC][padding]`
/// 实际验证: `data[3]`=0xAD(sync), `data[4]`=header1, `data[5]`=header2/CRC, `data[6..]`=payload
pub const MSBC_DATA_OFFSET: usize = 3;

/// 音频数据包中的 mSBC 数据长度
#[allow(dead_code)]
pub const MSBC_DATA_LEN: usize = 57;

// ============ Usage Pages ============

/// 键盘 Usage Page
#[allow(dead_code)]
pub const USAGE_PAGE_KEYBOARD: u16 = 0x0001;

/// 多媒体键 Usage Page
pub const USAGE_PAGE_CONSUMER: u16 = 0x000C;

/// 配置/状态 Usage Page
pub const USAGE_PAGE_CONFIG: u16 = 0xFFA0;

/// 音频数据 Usage Page
#[allow(dead_code)]
pub const USAGE_PAGE_AUDIO: u16 = 0xFFAA;

// ============ 按键类型枚举 ============

/// 按键功能类型。
///
/// 本协议族兼容多类 HID 输入设备，下列取值中只有一部分适用于本键盘
/// （常用的是 [`Media`](Self::Media)、[`Keyboard`](Self::Keyboard)、
/// [`AiVoice`](Self::AiVoice) 与 [`Macro`](Self::Macro)）。
/// 其余取值保留以便完整解析固件返回的按键配置，写入前请确认固件是否支持。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum KeyClass {
    /// 默认功能（已废弃）
    Default = 0x00,
    /// 鼠标功能按键
    MouseKey = 0x01,
    /// 鼠标 DPI 切换键
    MouseDpi = 0x02,
    /// 鼠标左/右滚
    MouseTilt = 0x03,
    /// 鼠标火力键
    MouseFireKey = 0x04,
    /// 快捷键
    MouseShortcutKey = 0x05,
    /// 宏定义键
    Macro = 0x06,
    /// 切换报告率键
    SwitchReportRate = 0x07,
    /// 切换配置文件
    SwitchProfile = 0x08,
    /// 滚轮
    Wheel = 0x09,
    /// 多媒体键 ⭐ 默认类型
    Media = 0x0A,
    /// 键盘按键
    Keyboard = 0x0B,
    /// 锁定 X 轴
    LockX = 0x0D,
    /// AI 语音输入 ⭐
    AiVoice = 0x0E,
    /// 锁定 Y 轴（保留，可能不支持）
    LockY = 0x0C,
    /// 禁用按键
    Disable = 0xFF,
}

impl KeyClass {
    /// 从 u8 值创建 KeyClass
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x00 => Some(Self::Default),
            0x01 => Some(Self::MouseKey),
            0x02 => Some(Self::MouseDpi),
            0x03 => Some(Self::MouseTilt),
            0x04 => Some(Self::MouseFireKey),
            0x05 => Some(Self::MouseShortcutKey),
            0x06 => Some(Self::Macro),
            0x07 => Some(Self::SwitchReportRate),
            0x08 => Some(Self::SwitchProfile),
            0x09 => Some(Self::Wheel),
            0x0A => Some(Self::Media),
            0x0B => Some(Self::Keyboard),
            0x0D => Some(Self::LockX),
            0x0E => Some(Self::AiVoice), // AI 语音输入
            0x0C => Some(Self::LockY),   // 锁定 Y 轴（保留）
            0xFF => Some(Self::Disable),
            _ => None,
        }
    }

    /// 获取显示名称
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Default => "默认",
            Self::MouseKey => "鼠标按键",
            Self::MouseDpi => "DPI切换",
            Self::MouseTilt => "鼠标滚轮倾斜",
            Self::MouseFireKey => "火力键",
            Self::MouseShortcutKey => "快捷键",
            Self::Macro => "宏",
            Self::SwitchReportRate => "报告率切换",
            Self::SwitchProfile => "配置切换",
            Self::Wheel => "滚轮",
            Self::Media => "多媒体键",
            Self::Keyboard => "键盘按键",
            Self::LockX => "锁定X轴",
            Self::LockY => "锁定Y轴",
            Self::AiVoice => "AI语音",
            Self::Disable => "禁用",
        }
    }
}

// ============ 按键信息结构 ============

/// 按键信息结构 (3 字节)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyInfo {
    /// 按键功能类型
    pub key_class: u8,
    /// 键值低字节
    pub key_value_l: u8,
    /// 键值高字节
    pub key_value_h: u8,
}

impl KeyInfo {
    /// 创建新的按键信息
    pub fn new(key_class: u8, key_value_l: u8, key_value_h: u8) -> Self {
        Self {
            key_class,
            key_value_l,
            key_value_h,
        }
    }

    /// 创建禁用键配置
    pub fn disabled() -> Self {
        Self {
            key_class: KeyClass::Disable as u8,
            key_value_l: 0x00,
            key_value_h: 0x00,
        }
    }

    /// 获取键值 (16 位)
    pub fn key_value(&self) -> u16 {
        ((self.key_value_h as u16) << 8) | (self.key_value_l as u16)
    }

    /// 设置键值 (16 位)
    #[allow(dead_code)]
    pub fn set_key_value(&mut self, value: u16) {
        self.key_value_l = (value & 0xFF) as u8;
        self.key_value_h = ((value >> 8) & 0xFF) as u8;
    }

    /// 从字节数组解析
    pub fn from_bytes(data: [u8; 3]) -> Self {
        Self {
            key_class: data[0],
            key_value_l: data[1],
            key_value_h: data[2],
        }
    }

    /// 转换为字节数组
    pub fn to_bytes(self) -> [u8; 3] {
        [self.key_class, self.key_value_l, self.key_value_h]
    }

    /// 获取按键类型
    #[allow(dead_code)]
    pub fn get_class(&self) -> Option<KeyClass> {
        KeyClass::from_u8(self.key_class)
    }
}

// ============ 按键名称映射 ============

/// 获取按键名称
pub fn get_key_name(index: usize) -> &'static str {
    match index {
        0 => "音量A相(KEY0)",
        1 => "音量B相(KEY1)",
        2 => "音量按压(KEY2)",
        3 => "Tab键(KEY3)",
        4 => "New键(KEY4)",
        5 => "Esc键(KEY5)",
        6 => "AI语音(KEY6)",
        7 => "Action键(KEY7)",
        8 => "Enter键(KEY8)",
        9 => "YOLO拨杆(KEY9)",
        10 => "PLAN拨杆(KEY10)",
        11 => "CHAT拨杆(KEY11)",
        _ => "未知",
    }
}

/// 默认键值映射
pub fn get_default_key_value(index: usize) -> u16 {
    match index {
        0 => 0x0F07,  // 音量 A 相
        1 => 0x0F08,  // 音量 B 相
        2 => 0x0F09,  // 音量按压
        3 => 0x0F01,  // Tab 键
        4 => 0x0F02,  // New 键
        5 => 0x0F03,  // Esc 键
        6 => 0x0F04,  // AI 语音
        7 => 0x0F05,  // Action 键
        8 => 0x0F06,  // Enter 键
        9 => 0x0F0A,  // YOLO 模式
        10 => 0x0F0B, // PLAN 模式
        11 => 0x0F0C, // CHAT 模式
        _ => 0x0000,
    }
}

// ============ 工作模式枚举 ============

/// 工作模式（CHAT / YOLO / PLAN，由硬件拨杆决定）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum WorkMode {
    /// CHAT 模式
    Chat = 0,
    /// YOLO 模式
    Yolo = 1,
    /// PLAN 模式
    Plan = 2,
}

impl WorkMode {
    /// 从 u8 值创建 WorkMode
    ///
    /// 模式值映射（根据实际观察）：
    /// - 0x00, 0x0C: CHAT 模式
    /// - 0x01, 0x0A: YOLO 模式
    /// - 0x02, 0x0B: PLAN 模式
    /// - 0x0F: 可能是 PLAN 或特殊状态
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x00 | 0x0C => Some(Self::Chat),
            0x01 | 0x0A => Some(Self::Yolo),
            0x02 | 0x0B | 0x0F => Some(Self::Plan),
            _ => None,
        }
    }

    /// 获取显示名称
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Chat => "CHAT",
            Self::Yolo => "YOLO",
            Self::Plan => "PLAN",
        }
    }
}

// ============ HID 数据包结构 ============

/// HID 数据包
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct HidPacket {
    /// Report ID
    pub report_id: u8,
    /// 命令码
    pub cmd: u8,
    /// 数据长度
    pub len: u8,
    /// 数据内容
    pub data: [u8; 61],
}

impl HidPacket {
    /// 创建新的 HID 数据包
    #[allow(dead_code)]
    pub fn new(report_id: u8, cmd: u8, len: u8, data: [u8; 61]) -> Self {
        Self {
            report_id,
            cmd,
            len,
            data,
        }
    }

    /// 创建 GET 按键配置命令
    pub fn get_key_config() -> [u8; PACKET_SIZE] {
        let mut packet = [0u8; PACKET_SIZE];
        packet[0] = REPORT_ID_OUTPUT;
        packet[1] = CMD_GET_KEY_SETTING;
        packet[2] = 0x00;
        packet
    }

    /// 创建 GET 设备信息命令（含电量）
    pub fn get_device_info() -> [u8; PACKET_SIZE] {
        let mut packet = [0u8; PACKET_SIZE];
        packet[0] = REPORT_ID_OUTPUT;
        packet[1] = CMD_GET_DEVICE_INFO;
        packet[2] = 0x02;
        packet
    }

    pub fn get_audio_capabilities() -> [u8; PACKET_SIZE] {
        let mut packet = [0u8; PACKET_SIZE];
        packet[0] = REPORT_ID_OUTPUT;
        packet[1] = CMD_AI_GET_AUDIO_CAPABILITIES;
        packet
    }

    pub fn audio_stream_control(
        action: crate::kernel::audio::AudioStreamAction,
        transport: crate::kernel::audio::AudioTransport,
        scope: crate::kernel::audio::AudioStreamScope,
        lease_id: u32,
        ttl_ms: u16,
    ) -> Option<[u8; PACKET_SIZE]> {
        let transport_mask = match transport {
            crate::kernel::audio::AudioTransport::UsbVendorHid => 0x01,
            crate::kernel::audio::AudioTransport::BleGatt => 0x02,
            crate::kernel::audio::AudioTransport::UsbUac
            | crate::kernel::audio::AudioTransport::System => return None,
        };
        let mut packet = [0u8; PACKET_SIZE];
        packet[0] = REPORT_ID_OUTPUT;
        packet[1] = CMD_AI_AUDIO_STREAM_CONTROL;
        packet[2] = 10;
        packet[3] = action as u8;
        packet[4] = transport_mask;
        packet[5] = scope as u8;
        packet[6] = crate::kernel::audio::AUDIO_PROTOCOL_VERSION;
        packet[7..11].copy_from_slice(&lease_id.to_le_bytes());
        packet[11..13].copy_from_slice(&ttl_ms.to_le_bytes());
        Some(packet)
    }

    /// 创建 SET 按键配置命令
    pub fn set_key_config(key_data: &[u8; KEY_DATA_LEN]) -> [u8; PACKET_SIZE] {
        let mut packet = [0u8; PACKET_SIZE];
        packet[0] = REPORT_ID_OUTPUT;
        packet[1] = CMD_SET_KEY_SETTING;
        packet[2] = KEY_DATA_LEN as u8;
        packet[3..63].copy_from_slice(key_data);
        packet
    }

    /// 创建读取静默录音标志命令（CMD 0x61）。
    pub fn get_silent_record() -> [u8; PACKET_SIZE] {
        let mut packet = [0u8; PACKET_SIZE];
        packet[0] = REPORT_ID_OUTPUT;
        packet[1] = CMD_GET_SILENT_RECORD;
        packet
    }

    /// 创建读取当前工作模式命令（CMD 0x12 + 子命令 0xC9）。
    ///
    /// 固件在 `CMD_STATUS` 分支处理：当 `cmd_type == CMD_WORK_MODE_DATA` 时
    /// 同步回当前工作模式。响应布局为
    /// `{ header{cmd=0x12,len}, cmd_type=0xC9, data, data1, data2 }`。
    pub fn get_work_mode() -> [u8; PACKET_SIZE] {
        let mut packet = [0u8; PACKET_SIZE];
        packet[0] = REPORT_ID_OUTPUT;
        packet[1] = CMD_STATUS;
        packet[2] = 0x04; // len: cmd_type + data + data1 + data2（与响应负载等宽）
        packet[3] = CMD_WORK_MODE_DATA;
        packet
    }

    /// 创建设置静默录音标志命令（CMD 0x62）。
    pub fn set_silent_record(enable: bool) -> [u8; PACKET_SIZE] {
        let mut packet = [0u8; PACKET_SIZE];
        packet[0] = REPORT_ID_OUTPUT;
        packet[1] = CMD_SET_SILENT_RECORD;
        packet[2] = 0x01;
        packet[3] = u8::from(enable);
        packet
    }

    /// 创建读取软休眠超时命令（CMD 0x63）。
    pub fn get_sleep_timeout() -> [u8; PACKET_SIZE] {
        let mut packet = [0u8; PACKET_SIZE];
        packet[0] = REPORT_ID_OUTPUT;
        packet[1] = CMD_GET_SLEEP_TIMEOUT;
        packet
    }

    /// 创建设置软休眠超时命令（CMD 0x64）。
    ///
    /// 负载 4 字节：`[disc_lo, disc_hi, conn_lo, conn_hi]`（uint16 小端）。
    pub fn set_sleep_timeout(timeout: crate::kernel::types::SleepTimeout) -> [u8; PACKET_SIZE] {
        let mut packet = [0u8; PACKET_SIZE];
        packet[0] = REPORT_ID_OUTPUT;
        packet[1] = CMD_SET_SLEEP_TIMEOUT;
        packet[2] = 0x04; // len = 4 字节负载
        packet[3..5].copy_from_slice(&timeout.disconnected.to_le_bytes());
        packet[5..7].copy_from_slice(&timeout.connected.to_le_bytes());
        packet
    }

    /// 创建 App 在线状态上报命令（CMD 0x65）。
    ///
    /// 负载 1 字节：`online`（0=离线 1=在线）。
    pub fn app_online_notify(online: bool) -> [u8; PACKET_SIZE] {
        let mut packet = [0u8; PACKET_SIZE];
        packet[0] = REPORT_ID_OUTPUT;
        packet[1] = CMD_AI_APP_ONLINE_NOTIFY;
        packet[2] = 0x01;
        packet[3] = u8::from(online);
        packet
    }

    /// 创建查询 App 在线状态命令（CMD 0x66）。
    pub fn get_app_online() -> [u8; PACKET_SIZE] {
        let mut packet = [0u8; PACKET_SIZE];
        packet[0] = REPORT_ID_OUTPUT;
        packet[1] = CMD_AI_GET_APP_ONLINE;
        packet
    }

    /// 创建获取离线开网页 URL 命令（CMD 0x67）。
    pub fn get_open_url() -> [u8; PACKET_SIZE] {
        let mut packet = [0u8; PACKET_SIZE];
        packet[0] = REPORT_ID_OUTPUT;
        packet[1] = CMD_AI_GET_OPEN_URL;
        packet
    }

    /// 创建设置离线开网页 URL 命令（CMD 0x68）。
    ///
    /// 负载 64 字节：URL 字符串（最长 63 字符 + \0 填充）。
    pub fn set_open_url(url: &str) -> [u8; PACKET_SIZE] {
        let mut packet = [0u8; PACKET_SIZE];
        packet[0] = REPORT_ID_OUTPUT;
        packet[1] = CMD_AI_SET_OPEN_URL;
        packet[2] = 0x40; // len = 64 字节负载
        let bytes = url.as_bytes();
        let copy_len = bytes.len().min(63); // 保留 1 字节给 \0
        packet[3..3 + copy_len].copy_from_slice(&bytes[..copy_len]);
        packet
    }

    /// 进入/续租/退出工厂物理按键测试（CMD 0x6C）。
    #[cfg(feature = "test-mode")]
    pub fn factory_key_test_control(enable: bool, session: u16) -> Result<[u8; PACKET_SIZE]> {
        if session == 0 {
            return Err(BoardError::Protocol(
                "工厂按键测试 session 不能为 0".to_string(),
            ));
        }
        let mut packet = [0u8; PACKET_SIZE];
        packet[0] = REPORT_ID_OUTPUT;
        packet[1] = CMD_AI_FACTORY_KEY_TEST_CONTROL;
        packet[2] = 0x04;
        packet[3] = u8::from(enable);
        packet[4] = FACTORY_KEY_TEST_PROTOCOL_VERSION;
        packet[5..7].copy_from_slice(&session.to_le_bytes());
        Ok(packet)
    }

    /// 创建关机命令 (0x5E)
    /// action: 0x01=保留配对关机, 0x02=清除配对关机
    #[cfg(feature = "test-mode")]
    pub fn shutdown(keep_pair: bool) -> [u8; PACKET_SIZE] {
        let mut packet = [0u8; PACKET_SIZE];
        packet[0] = REPORT_ID_OUTPUT;
        packet[1] = CMD_AI_SHUTDOWN;
        packet[2] = 0x01; // len
        packet[3] = if keep_pair { 0x01 } else { 0x02 };
        packet
    }

    /// 从原始数据解析
    #[allow(dead_code)]
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 4 {
            return None;
        }

        let mut packet_data = [0u8; 61];
        if data.len() > 3 {
            let copy_len = (data.len() - 3).min(61);
            packet_data[..copy_len].copy_from_slice(&data[3..3 + copy_len]);
        }

        Some(Self {
            report_id: data[0],
            cmd: data[1],
            len: data[2],
            data: packet_data,
        })
    }

    /// 检查是否为成功响应
    #[allow(dead_code)]
    pub fn is_success(&self) -> bool {
        self.data[0] == 0x00
    }
}

#[cfg(feature = "test-mode")]
fn factory_packet_without_report_id(data: &[u8], expected_cmd: u8) -> Result<&[u8]> {
    let packet = if data.first() == Some(&REPORT_ID_INPUT) {
        data.get(1..)
            .ok_or_else(|| BoardError::Protocol("工厂测试 HID 包长度不足".to_string()))?
    } else {
        data
    };
    if packet.first() != Some(&expected_cmd) {
        return Err(BoardError::Protocol(format!(
            "工厂测试 CMD 不匹配: expected=0x{expected_cmd:02X} actual=0x{:02X}",
            packet.first().copied().unwrap_or_default()
        )));
    }
    Ok(packet)
}

/// 解析 USB（含 report id）或 GATT（不含 report id）的 0x6C ACK。
#[cfg(feature = "test-mode")]
pub fn parse_factory_key_control_ack(
    data: &[u8],
    expected_session: u16,
) -> Result<FactoryKeyControlAck> {
    let packet = factory_packet_without_report_id(data, CMD_AI_FACTORY_KEY_TEST_CONTROL)?;
    if packet.len() < 7 || packet[1] != 0x05 {
        return Err(BoardError::Protocol(
            "工厂测试控制 ACK 长度无效".to_string(),
        ));
    }
    if packet[3] != FACTORY_KEY_TEST_PROTOCOL_VERSION {
        return Err(BoardError::Protocol(format!(
            "工厂测试协议版本不匹配: {}",
            packet[3]
        )));
    }
    if packet[4] > 1 {
        return Err(BoardError::Protocol(
            "工厂测试 ACK enabled 无效".to_string(),
        ));
    }
    let session = u16::from_le_bytes([packet[5], packet[6]]);
    if session != expected_session {
        return Err(BoardError::Protocol(format!(
            "工厂测试 ACK session 不匹配: expected={expected_session:#06X} actual={session:#06X}"
        )));
    }
    Ok(FactoryKeyControlAck {
        result: packet[2].try_into()?,
        enabled: packet[4] != 0,
        session,
    })
}

#[cfg(feature = "test-mode")]
fn parse_factory_key_event_inner(
    data: &[u8],
    expected_session: Option<u16>,
) -> Result<FactoryKeyEvent> {
    let packet = factory_packet_without_report_id(data, CMD_AI_FACTORY_KEY_EVENT)?;
    if packet.len() < 8 || packet[1] != 0x06 {
        return Err(BoardError::Protocol("工厂物理按键事件长度无效".to_string()));
    }
    if packet[2] != FACTORY_KEY_TEST_PROTOCOL_VERSION {
        return Err(BoardError::Protocol(format!(
            "工厂物理按键协议版本不匹配: {}",
            packet[2]
        )));
    }
    let session = u16::from_le_bytes([packet[3], packet[4]]);
    if session == 0 || expected_session.is_some_and(|expected| expected != session) {
        return Err(BoardError::Protocol(format!(
            "工厂物理按键 session 无效: {session:#06X}"
        )));
    }
    if packet[5] >= FACTORY_KEY_TEST_INPUT_COUNT {
        return Err(BoardError::Protocol(format!(
            "工厂物理按键索引越界: {}",
            packet[5]
        )));
    }
    if packet[6] > 1 {
        return Err(BoardError::Protocol(
            "工厂物理按键 pressed 无效".to_string(),
        ));
    }
    Ok(FactoryKeyEvent {
        session,
        input_index: packet[5],
        pressed: packet[6] != 0,
        sequence: packet[7],
    })
}

/// 按调用方持有的 session 解析工厂物理按键事件。
#[cfg(feature = "test-mode")]
pub fn parse_factory_key_event(data: &[u8], expected_session: u16) -> Result<FactoryKeyEvent> {
    parse_factory_key_event_inner(data, Some(expected_session))
}

/// SDK 事件监控路径使用：校验协议和边界，但把 session 留给消费方做会话过滤。
#[cfg(feature = "test-mode")]
pub(crate) fn parse_factory_key_event_unscoped(data: &[u8]) -> Result<FactoryKeyEvent> {
    parse_factory_key_event_inner(data, None)
}

/// 创建读取绑定配置块分片命令（CMD 0x69）。
///
/// 负载 2 字节：`offset`（LE），相对 blob 区起点。
pub fn read_bindings_blob_packet(offset: u16) -> [u8; PACKET_SIZE] {
    let mut packet = [0u8; PACKET_SIZE];
    packet[0] = REPORT_ID_OUTPUT;
    packet[1] = CMD_AI_READ_BINDINGS_BLOB;
    packet[2] = 0x02;
    packet[3..5].copy_from_slice(&offset.to_le_bytes());
    packet
}

/// 创建写入绑定配置块分片命令（CMD 0x6A）。
///
/// 负载 `2 + chunk.len()` 字节：`[offset(2 LE)][chunk]`，chunk ≤ 56。
///
/// `assert!`（非 debug_assert）：本函数 `pub`，release 下外部误传超长 chunk
/// 会 panic（fail-fast），而非静默写出非法分片或 copy 越界。内部调用方经
/// `split_blob_chunks` 保证 ≤56，不受影响。
pub fn write_bindings_blob_packet(offset: u16, chunk: &[u8]) -> [u8; PACKET_SIZE] {
    assert!(chunk.len() <= 56, "blob 分片超过 56 字节");
    let mut packet = [0u8; PACKET_SIZE];
    packet[0] = REPORT_ID_OUTPUT;
    packet[1] = CMD_AI_WRITE_BINDINGS_BLOB;
    packet[2] = (2 + chunk.len()) as u8;
    packet[3..5].copy_from_slice(&offset.to_le_bytes());
    packet[5..5 + chunk.len()].copy_from_slice(chunk);
    packet
}

/// 创建绑定配置块 commit 命令（CMD 0x6A，offset=0xFFFF）。
///
/// 负载 6 字节：`[0xFFFF][total_len(2 LE)][crc16(2 LE)]`。
pub fn commit_bindings_blob_packet(total_len: u16, crc16: u16) -> [u8; PACKET_SIZE] {
    let mut packet = [0u8; PACKET_SIZE];
    packet[0] = REPORT_ID_OUTPUT;
    packet[1] = CMD_AI_WRITE_BINDINGS_BLOB;
    packet[2] = 0x06;
    packet[3..5].copy_from_slice(&0xFFFFu16.to_le_bytes());
    packet[5..7].copy_from_slice(&total_len.to_le_bytes());
    packet[7..9].copy_from_slice(&crc16.to_le_bytes());
    packet
}

/// 解析完整 HID 静默录音响应：`[report_id, cmd, len, result, enabled]`。
pub fn parse_silent_record_hid_response(response: &[u8], expected_cmd: u8) -> Option<bool> {
    if response.len() < 5 || response[1] != expected_cmd || response[2] < 2 || response[3] != 0 {
        return None;
    }
    Some(response[4] != 0)
}

/// Parse `[report, 0x6E, len=13, result, protocol, caps u32, envelope,
/// usb_max, ble_max, default_ttl u16, max_ttl u16]`.
pub fn parse_audio_capabilities_hid_response(
    response: &[u8],
) -> Option<crate::kernel::audio::AudioCapabilities> {
    if response.len() < 16
        || response[0] != REPORT_ID_INPUT
        || response[1] != CMD_AI_GET_AUDIO_CAPABILITIES
        || response[2] != 13
        || response[3] != 0
    {
        return None;
    }
    parse_audio_capabilities_payload(&response[4..16])
}

/// Parse the GATT response equivalent without the HID report id.
pub fn parse_audio_capabilities_gatt_response(
    response: &[u8],
) -> Option<crate::kernel::audio::AudioCapabilities> {
    if response.len() < 15
        || response[0] != CMD_AI_GET_AUDIO_CAPABILITIES
        || response[1] != 13
        || response[2] != 0
    {
        return None;
    }
    parse_audio_capabilities_payload(&response[3..15])
}

fn parse_audio_capabilities_payload(
    payload: &[u8],
) -> Option<crate::kernel::audio::AudioCapabilities> {
    if payload.len() < 12 {
        return None;
    }
    let bits = u32::from_le_bytes(payload[1..5].try_into().ok()?);
    Some(crate::kernel::audio::AudioCapabilities::from_bits(
        payload[0],
        bits,
        payload[5],
        payload[6],
        payload[7],
        u16::from_le_bytes([payload[8], payload[9]]),
        u16::from_le_bytes([payload[10], payload[11]]),
    ))
}

pub fn parse_audio_stream_hid_response(
    response: &[u8],
) -> Option<crate::kernel::audio::AudioStreamState> {
    if response.len() < 13
        || response[0] != REPORT_ID_INPUT
        || response[1] != CMD_AI_AUDIO_STREAM_CONTROL
        || response[2] != 10
    {
        return None;
    }
    parse_audio_stream_payload(response[3], &response[4..13])
}

pub fn parse_audio_stream_gatt_response(
    response: &[u8],
) -> Option<crate::kernel::audio::AudioStreamState> {
    if response.len() < 12 || response[0] != CMD_AI_AUDIO_STREAM_CONTROL || response[1] != 10 {
        return None;
    }
    parse_audio_stream_payload(response[2], &response[3..12])
}

fn parse_audio_stream_payload(
    result: u8,
    payload: &[u8],
) -> Option<crate::kernel::audio::AudioStreamState> {
    use crate::kernel::audio::{AudioStreamResult, AudioStreamScope, AudioTransport};
    if payload.len() < 9 {
        return None;
    }
    let result = AudioStreamResult::try_from(result).ok()?;
    let active_transport = match payload[1] {
        0 => None,
        1 => Some(AudioTransport::UsbVendorHid),
        2 => Some(AudioTransport::BleGatt),
        _ => return None,
    };
    let scope = match payload[2] {
        0 => None,
        1 => Some(AudioStreamScope::Session),
        2 => Some(AudioStreamScope::Timeline),
        _ => return None,
    };
    Some(crate::kernel::audio::AudioStreamState {
        result,
        protocol_version: payload[0],
        active_transport,
        scope,
        lease_id: u32::from_le_bytes(payload[3..7].try_into().ok()?),
        ttl_ms: u16::from_le_bytes([payload[7], *payload.get(8)?]),
    })
}

/// 读到的 HID 报告相对于"我刚发出的那条命令"是什么。
///
/// Config(0xFFA0) 与 Audio(0xFFAA) 是同一条物理 HID 接口上的两个顶层集合，
/// macOS 按 path 打开拿到的是整条接口。板载音频一开，命令响应就淹没在每秒
/// 上百个音频包里——不筛就会把音频包当成响应，而且录音期间任何命令都会中招。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandReportKind {
    /// 正是这条命令的响应。
    Match,
    /// 是命令响应，但不是这条命令的（异步上报等）。
    OtherCommand,
    /// 根本不是命令响应（音频包、按键事件等）。
    NotCommand,
}

/// 按报告类型给读到的报告归类。`expected_cmd` 是刚发出的那条命令的 CMD 字节。
pub fn classify_command_report(report: &[u8], expected_cmd: u8) -> CommandReportKind {
    if report.len() < 2 || report[0] != REPORT_ID_INPUT {
        return CommandReportKind::NotCommand;
    }
    if report[1] == expected_cmd {
        CommandReportKind::Match
    } else {
        CommandReportKind::OtherCommand
    }
}

pub fn parse_usb_audio_report(
    report: &[u8],
) -> Option<crate::kernel::audio::EncodedAudioPacket<'_>> {
    if report.len() != PACKET_SIZE
        || report[0] != REPORT_ID_AUDIO
        || report[1] != AUDIO_ENVELOPE_VERSION
        || report[2] & AUDIO_FLAG_DATA == 0
        || report[2] & !(AUDIO_FLAG_DATA | AUDIO_FLAG_DISCONTINUITY) != 0
    {
        return None;
    }
    let payload_len = report[5] as usize;
    if payload_len == 0
        || payload_len > MSBC_FRAME_SIZE
        || !payload_len.is_multiple_of(MSBC_FRAME_SIZE)
    {
        return None;
    }
    Some(crate::kernel::audio::EncodedAudioPacket {
        payload: &report[6..6 + payload_len],
        transport: crate::kernel::audio::AudioTransport::UsbVendorHid,
        sequence: Some(u16::from_le_bytes([report[3], report[4]])),
        device_discontinuity: report[2] & AUDIO_FLAG_DISCONTINUITY != 0,
    })
}

/// 解析 GATT 静默录音响应：`[cmd, len, result, enabled]`。
pub fn parse_silent_record_gatt_response(response: &[u8], expected_cmd: u8) -> Option<bool> {
    if response.len() < 4 || response[0] != expected_cmd || response[1] < 2 || response[2] != 0 {
        return None;
    }
    Some(response[3] != 0)
}

/// 解析 HID 工作模式响应：`[report_id, CMD_STATUS, len, 0xC9, mode]`。
///
/// 注意：CMD_STATUS (0x12) 是多路复用命令，必须额外校验 `response[3] == CMD_WORK_MODE_DATA`
/// 才能确认这是工作模式查询的回包（而非其它 status 子类型的回包）。
pub fn parse_work_mode_hid_response(response: &[u8]) -> Option<WorkMode> {
    if response.len() < 5 || response[1] != CMD_STATUS || response[3] != CMD_WORK_MODE_DATA {
        return None;
    }
    WorkMode::from_u8(response[4])
}

/// 解析 GATT 工作模式响应：`[CMD_STATUS, len, 0xC9, mode]`。
///
/// GATT 路径响应剥掉了 report_id，整体 offset 比 HID 少 1。
/// （`gatt_client::handle_event` 把 `[cmd, len]` prepend 到 payload 前返回。）
pub fn parse_work_mode_gatt_response(response: &[u8]) -> Option<WorkMode> {
    if response.len() < 4 || response[0] != CMD_STATUS || response[2] != CMD_WORK_MODE_DATA {
        return None;
    }
    WorkMode::from_u8(response[3])
}

/// 解析完整 HID 软休眠超时响应：
/// `[report_id, cmd, len, result, disc_lo, disc_hi, conn_lo, conn_hi]`。
pub fn parse_sleep_timeout_hid_response(
    response: &[u8],
    expected_cmd: u8,
) -> Option<crate::kernel::types::SleepTimeout> {
    if response.len() < 8 || response[1] != expected_cmd || response[2] < 5 || response[3] != 0 {
        return None;
    }
    let disconnected = u16::from_le_bytes([response[4], response[5]]);
    let connected = u16::from_le_bytes([response[6], response[7]]);
    Some(crate::kernel::types::SleepTimeout::new(
        disconnected,
        connected,
    ))
}

/// 解析 GATT 软休眠超时响应：
/// `[cmd, len, result, disc_lo, disc_hi, conn_lo, conn_hi]`。
pub fn parse_sleep_timeout_gatt_response(
    response: &[u8],
    expected_cmd: u8,
) -> Option<crate::kernel::types::SleepTimeout> {
    if response.len() < 7 || response[0] != expected_cmd || response[1] < 5 || response[2] != 0 {
        return None;
    }
    let disconnected = u16::from_le_bytes([response[3], response[4]]);
    let connected = u16::from_le_bytes([response[5], response[6]]);
    Some(crate::kernel::types::SleepTimeout::new(
        disconnected,
        connected,
    ))
}

/// 解析 HID App 在线状态响应：`[report_id, cmd, len, result, online]`。
pub fn parse_app_online_hid_response(response: &[u8], expected_cmd: u8) -> Option<bool> {
    if response.len() < 5 || response[1] != expected_cmd || response[2] < 2 || response[3] != 0 {
        return None;
    }
    Some(response[4] != 0)
}

/// 解析 GATT App 在线状态响应：`[cmd, len, result, online]`。
pub fn parse_app_online_gatt_response(response: &[u8], expected_cmd: u8) -> Option<bool> {
    if response.len() < 4 || response[0] != expected_cmd || response[1] < 2 || response[2] != 0 {
        return None;
    }
    Some(response[3] != 0)
}

/// 解析 HID 开网页 URL 响应：`[report_id, cmd, len, result, url(64)...]`。
pub fn parse_open_url_hid_response(response: &[u8], expected_cmd: u8) -> Option<String> {
    if response.len() < 5 || response[1] != expected_cmd || response[3] != 0 {
        return None;
    }
    let url_bytes = &response[4..];
    let end = url_bytes
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(url_bytes.len());
    Some(String::from_utf8_lossy(&url_bytes[..end]).into_owned())
}

/// 解析 GATT 开网页 URL 响应：`[cmd, len, result, url(64)...]`。
pub fn parse_open_url_gatt_response(response: &[u8], expected_cmd: u8) -> Option<String> {
    if response.len() < 4 || response[0] != expected_cmd || response[2] != 0 {
        return None;
    }
    let url_bytes = &response[3..];
    let end = url_bytes
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(url_bytes.len());
    Some(String::from_utf8_lossy(&url_bytes[..end]).into_owned())
}

// ============ 按键配置数据 ============

/// 按键配置数据 (20 个按键)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyConfig {
    /// 按键信息列表
    pub keys: [KeyInfo; KEY_COUNT],
}

impl KeyConfig {
    /// 创建空的按键配置
    pub fn new() -> Self {
        Self {
            keys: [KeyInfo::new(0, 0, 0); KEY_COUNT],
        }
    }

    /// 从 60 字节数据解析
    pub fn from_bytes(data: &[u8; KEY_DATA_LEN]) -> Self {
        let mut keys = [KeyInfo::new(0, 0, 0); KEY_COUNT];

        for (i, key) in keys.iter_mut().enumerate() {
            let offset = i * 3;
            if offset + 2 < KEY_DATA_LEN {
                *key = KeyInfo::from_bytes([data[offset], data[offset + 1], data[offset + 2]]);
            }
        }

        Self { keys }
    }

    /// 转换为 60 字节数据
    pub fn to_bytes(&self) -> [u8; KEY_DATA_LEN] {
        let mut data = [0u8; KEY_DATA_LEN];

        for i in 0..KEY_COUNT {
            let offset = i * 3;
            let bytes = self.keys[i].to_bytes();
            data[offset] = bytes[0];
            data[offset + 1] = bytes[1];
            data[offset + 2] = bytes[2];
        }

        data
    }

    /// 获取指定按键的配置
    #[allow(dead_code)]
    pub fn get_key(&self, index: usize) -> Option<&KeyInfo> {
        self.keys.get(index)
    }

    /// 设置指定按键的配置
    pub fn set_key(&mut self, index: usize, key_info: KeyInfo) -> bool {
        if index < KEY_COUNT {
            self.keys[index] = key_info;
            true
        } else {
            false
        }
    }

    /// 设置指定按键为禁用
    #[allow(dead_code)]
    pub fn disable_key(&mut self, index: usize) -> bool {
        self.set_key(index, KeyInfo::disabled())
    }

    /// 清理未使用的按键（索引12-19）为 00 00 00
    pub fn clear_unused_keys(&mut self) {
        for i in ACTIVE_KEY_COUNT..KEY_COUNT {
            self.keys[i] = KeyInfo::new(0x00, 0x00, 0x00);
        }
    }
}

/// 当前按键配置中是否存在 AI 语音功能。
pub fn key_config_has_ai_voice(config: &KeyConfig) -> bool {
    config.keys[..ACTIVE_KEY_COUNT].iter().any(|key| {
        matches!(KeyClass::from_u8(key.key_class), Some(KeyClass::AiVoice))
            || matches!(KeyClass::from_u8(key.key_class), Some(KeyClass::Media))
                && is_ai_voice_consumer_code(key.key_value())
    })
}

impl Default for KeyConfig {
    fn default() -> Self {
        let mut config = Self::new();

        // 设置默认配置
        for i in 0..ACTIVE_KEY_COUNT {
            let default_value = get_default_key_value(i);
            config.keys[i] = KeyInfo::new(
                KeyClass::Media as u8,
                (default_value & 0xFF) as u8,
                ((default_value >> 8) & 0xFF) as u8,
            );
        }

        // 未使用的按键填充 00 00 00
        for i in ACTIVE_KEY_COUNT..KEY_COUNT {
            config.keys[i] = KeyInfo::new(0x00, 0x00, 0x00);
        }

        config
    }
}

/// 根据 Consumer 键值反查 key_index(USB monitor 与 BLE GATT 共用)
pub fn find_key_index_by_value(value: u16) -> Option<usize> {
    match value {
        0x0F01 => Some(3),  // Tab
        0x0F02 => Some(4),  // New
        0x0F03 => Some(5),  // Esc
        0x0F04 => Some(6),  // AI Voice
        0x0F05 => Some(7),  // Action
        0x0F06 => Some(8),  // Enter
        0x0F07 => Some(0),  // Vol A
        0x0F08 => Some(1),  // Vol B
        0x0F09 => Some(2),  // Vol Press
        0x0F0A => Some(9),  // YOLO
        0x0F0B => Some(10), // PLAN
        0x0F0C => Some(11), // CHAT
        _ => None,
    }
}

/// 这个键索引是不是旋钮转动的编码器脉冲。
///
/// KEY0/KEY1 是旋钮的两个相位，每转一格来一下、立刻收尾——它没有「按住」的语义，
/// 也就不能和真正能按住的键一样进「当前按着哪些键」的集合。
/// 放在这里与 [`find_key_index_by_value`] / [`key_index_to_mode`] 作伴，
/// 免得键索引的语义散落在各个上报路径里各写一份。
pub fn is_knob_pulse_key_index(key_index: usize) -> bool {
    matches!(key_index, 0 | 1)
}

/// key_index → mode 映射(拨杆 9/10/11;USB monitor 与 BLE GATT 共用)
pub fn key_index_to_mode(key_index: usize) -> Option<(u8, &'static str)> {
    match key_index {
        9 => Some((1, "YOLO")),
        10 => Some((2, "PLAN")),
        11 => Some((0, "CHAT")),
        _ => None,
    }
}

#[cfg(test)]
mod command_report_classification {
    use super::*;

    fn report(first: u8, second: u8) -> [u8; PACKET_SIZE] {
        let mut r = [0u8; PACKET_SIZE];
        r[0] = first;
        r[1] = second;
        r
    }

    #[test]
    fn audio_packets_are_never_mistaken_for_a_command_response() {
        // 这正是「录音期间发命令会拿到音频包」那个 bug 的核心。
        let audio = report(REPORT_ID_AUDIO, AUDIO_ENVELOPE_VERSION);
        assert_eq!(
            classify_command_report(&audio, CMD_AI_AUDIO_STREAM_CONTROL),
            CommandReportKind::NotCommand
        );
    }

    #[test]
    fn key_events_are_not_command_responses_either() {
        let key = report(REPORT_ID_KEY_EVENT, 0x01);
        assert_eq!(
            classify_command_report(&key, CMD_STATUS),
            CommandReportKind::NotCommand
        );
    }

    #[test]
    fn the_response_to_the_command_we_sent_is_a_match() {
        let ack = report(REPORT_ID_INPUT, CMD_AI_AUDIO_STREAM_CONTROL);
        assert_eq!(
            classify_command_report(&ack, CMD_AI_AUDIO_STREAM_CONTROL),
            CommandReportKind::Match
        );
    }

    #[test]
    fn another_commands_response_is_kept_apart_from_ours() {
        let other = report(REPORT_ID_INPUT, CMD_STATUS);
        assert_eq!(
            classify_command_report(&other, CMD_AI_AUDIO_STREAM_CONTROL),
            CommandReportKind::OtherCommand
        );
    }

    #[test]
    fn a_truncated_report_is_not_a_command_response() {
        assert_eq!(
            classify_command_report(&[REPORT_ID_INPUT], CMD_STATUS),
            CommandReportKind::NotCommand
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_info() {
        let key_info = KeyInfo::new(0x0A, 0x01, 0x0F);
        assert_eq!(key_info.key_value(), 0x0F01);
        assert_eq!(key_info.get_class(), Some(KeyClass::Media));
    }

    #[test]
    fn test_key_config() {
        let mut config = KeyConfig::default();
        // V3 协议：AI 语音键使用 Media 类型 + 0x0F04 键值
        config.set_key(6, KeyInfo::new(KeyClass::Media as u8, 0x04, 0x0F));
        assert_eq!(config.keys[6].key_class, KeyClass::Media as u8);
        assert_eq!(config.keys[6].key_value(), AI_VOICE_MEDIA_CODE);
        assert!(is_ai_voice_consumer_code(AI_VOICE_MEDIA_CODE));
        assert!(is_ai_voice_consumer_code(AI_VOICE_MARK_CODE));
        assert!(!is_ai_voice_consumer_code(0x0F05));
    }

    #[test]
    fn test_key_config_has_ai_voice() {
        let mut config = KeyConfig::default();
        assert!(key_config_has_ai_voice(&config));

        config.set_key(6, KeyInfo::new(KeyClass::Media as u8, 0x05, 0x0F));
        assert!(!key_config_has_ai_voice(&config));

        config.set_key(7, KeyInfo::new(KeyClass::AiVoice as u8, 0x00, 0x00));
        assert!(key_config_has_ai_voice(&config));
    }

    #[test]
    fn test_work_mode() {
        assert_eq!(WorkMode::from_u8(0), Some(WorkMode::Chat));
        assert_eq!(WorkMode::from_u8(1), Some(WorkMode::Yolo));
        assert_eq!(WorkMode::from_u8(2), Some(WorkMode::Plan));
    }

    #[test]
    fn test_silent_record_packets() {
        let get = HidPacket::get_silent_record();
        assert_eq!(&get[..3], &[REPORT_ID_OUTPUT, CMD_GET_SILENT_RECORD, 0]);

        let enabled = HidPacket::set_silent_record(true);
        assert_eq!(
            &enabled[..4],
            &[REPORT_ID_OUTPUT, CMD_SET_SILENT_RECORD, 1, 1]
        );
        let disabled = HidPacket::set_silent_record(false);
        assert_eq!(disabled[3], 0);
    }

    #[test]
    fn test_silent_record_response_parsing() {
        assert_eq!(
            parse_silent_record_hid_response(&[0x0A, 0x61, 2, 0, 1], 0x61),
            Some(true)
        );
        assert_eq!(
            parse_silent_record_gatt_response(&[0x62, 2, 0, 0], 0x62),
            Some(false)
        );
        assert_eq!(
            parse_silent_record_hid_response(&[0x0A, 0x61, 2, 0xFF, 1], 0x61),
            None
        );
        assert_eq!(
            parse_silent_record_gatt_response(&[0x61, 1, 0, 1], 0x61),
            None
        );
    }

    #[test]
    fn test_work_mode_packets() {
        let get = HidPacket::get_work_mode();
        // [report_id=0x0B, cmd=0x12, len=0x04, sub=0xC9, ...padding]
        assert_eq!(
            &get[..4],
            &[REPORT_ID_OUTPUT, CMD_STATUS, 0x04, CMD_WORK_MODE_DATA]
        );
        // 其余字节应为 0（请求负载无 data/data1/data2）
        assert_eq!(&get[4..], &[0u8; PACKET_SIZE - 4]);
    }

    #[test]
    fn test_work_mode_response_parsing() {
        // HID 响应：[report_id, CMD_STATUS, len, 0xC9, mode]
        // CHAT(0) / YOLO(1) / PLAN(2) — 来自固件 CMD_STATUS handler 的回包
        assert_eq!(
            parse_work_mode_hid_response(&[0x0A, 0x12, 0x02, 0xC9, 0x00]),
            Some(WorkMode::Chat)
        );
        assert_eq!(
            parse_work_mode_hid_response(&[0x0A, 0x12, 0x02, 0xC9, 0x01]),
            Some(WorkMode::Yolo)
        );
        assert_eq!(
            parse_work_mode_hid_response(&[0x0A, 0x12, 0x02, 0xC9, 0x02]),
            Some(WorkMode::Plan)
        );
        // GATT 响应：[CMD_STATUS, len, 0xC9, mode]（剥掉 report_id，offset -1）
        assert_eq!(
            parse_work_mode_gatt_response(&[0x12, 0x02, 0xC9, 0x00]),
            Some(WorkMode::Chat)
        );
        assert_eq!(
            parse_work_mode_gatt_response(&[0x12, 0x02, 0xC9, 0x01]),
            Some(WorkMode::Yolo)
        );
        // cmd 不匹配（非 CMD_STATUS）
        assert_eq!(
            parse_work_mode_hid_response(&[0x0A, 0x61, 0x02, 0xC9, 0x00]),
            None
        );
        // sub-type 不匹配（CMD_STATUS 但非工作模式子命令）
        assert_eq!(
            parse_work_mode_hid_response(&[0x0A, 0x12, 0x02, 0xC8, 0x00]),
            None
        );
        assert_eq!(
            parse_work_mode_gatt_response(&[0x12, 0x02, 0xC8, 0x00]),
            None
        );
        // 长度不足
        assert_eq!(
            parse_work_mode_hid_response(&[0x0A, 0x12, 0x02, 0xC9]),
            None
        );
        assert_eq!(parse_work_mode_gatt_response(&[0x12, 0x02, 0xC9]), None);
        // 非法 mode 值
        assert_eq!(
            parse_work_mode_hid_response(&[0x0A, 0x12, 0x02, 0xC9, 0x05]),
            None
        );
    }

    #[test]
    fn test_sleep_timeout_packets() {
        use crate::kernel::types::SleepTimeout;

        let get = HidPacket::get_sleep_timeout();
        assert_eq!(&get[..3], &[REPORT_ID_OUTPUT, CMD_GET_SLEEP_TIMEOUT, 0]);

        // 文档例子：disconnected=60(0x003C)、connected=600(0x0258)
        let set = HidPacket::set_sleep_timeout(SleepTimeout::new(60, 600));
        assert_eq!(
            &set[..7],
            &[
                REPORT_ID_OUTPUT,
                CMD_SET_SLEEP_TIMEOUT,
                4,
                0x3C,
                0x00,
                0x58,
                0x02
            ]
        );
    }

    #[test]
    fn test_sleep_timeout_response_parsing() {
        use crate::kernel::types::SleepTimeout;

        // HID 响应：report_id + cmd + len + result + disc(LE) + conn(LE)
        assert_eq!(
            parse_sleep_timeout_hid_response(&[0x0A, 0x63, 5, 0, 0x3C, 0x00, 0x58, 0x02], 0x63),
            Some(SleepTimeout::new(60, 600))
        );
        // GATT 响应：cmd + len + result + disc(LE) + conn(LE)
        assert_eq!(
            parse_sleep_timeout_gatt_response(&[0x64, 5, 0, 0x3C, 0x00, 0x58, 0x02], 0x64),
            Some(SleepTimeout::new(60, 600))
        );
        // result != 0 → 失败
        assert_eq!(
            parse_sleep_timeout_hid_response(&[0x0A, 0x63, 5, 0xFF, 0x3C, 0x00, 0x58, 0x02], 0x63),
            None
        );
        // cmd 不匹配
        assert_eq!(
            parse_sleep_timeout_gatt_response(&[0x61, 5, 0, 0x3C, 0x00, 0x58, 0x02], 0x63),
            None
        );
        // 长度不足
        assert_eq!(
            parse_sleep_timeout_hid_response(&[0x0A, 0x63, 5, 0], 0x63),
            None
        );
    }
}
