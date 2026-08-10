//! 设备事件类型。
//!
//! SDK 内部通过 `broadcast::Sender<BoardEvent>` 上报,消费者(facade 层的 BoardDevice)
//! 用 `subscribe()` 拿到 Receiver。用单一 enum 替代散装 struct + 字符串 channel
//! 路由 —— 一次 match 全覆盖,类型安全。

use crate::kernel::types::ConnectionType;
use serde::{Deserialize, Serialize};

/// SDK 上报给消费者的所有设备事件
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum BoardEvent {
    /// 设备连接/断开(含断开原因)
    Connection(ConnectionEvent),
    /// 重连状态变化(进入等待/正在扫描/已连上等)
    Reconnect(ReconnectEvent),
    /// 单键按下/释放
    KeyPress(KeyPressEvent),
    /// 组合键(同时按下 ≥2 键)
    ComboKey(ComboKeyEvent),
    /// AI 语音键(物理键 6)按下/释放
    AiVoiceKey(AiVoiceKeyEvent),
    /// 模式拨杆切换(YOLO/PLAN/CHAT)
    ModeChange(ModeChangeEvent),
    /// 工厂测试模式中的映射前物理输入事件（固件 v1.58+）
    #[cfg(feature = "test-mode")]
    FactoryKey(FactoryKeyEvent),
    /// 设备信息(主动读取或轮询得到)
    DeviceInfo(DeviceInfo),
    /// 错误(非致命,如某次命令超时)
    Error(ErrorEvent),
}

/// 工厂测试物理输入事件。
///
/// `input_index` 是 PCB/GPIO 对应的稳定位置 0..=11，不受用户绑定影响。
#[cfg(feature = "test-mode")]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct FactoryKeyEvent {
    pub session: u16,
    pub input_index: u8,
    pub pressed: bool,
    pub sequence: u8,
}

/// 连接变化事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionEvent {
    pub connected: bool,
    pub connection_type: Option<ConnectionType>,
    pub reason: Option<DisconnectReason>,
}

/// 断开原因
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DisconnectReason {
    /// 物理消失(USB 拔出 / BLE 超范围)
    DeviceGone,
    /// USB↔BLE 切换
    ConnectionTypeChanged(ConnectionType),
    /// 主动断开
    UserAction,
    /// 固件主动断开(CMD=0x60,关机)
    DeviceDisconnect,
}

/// 重连状态事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconnectEvent {
    pub state: ReconnectState,
    pub attempt: Option<u32>,
    pub message: Option<String>,
}

/// 重连状态机
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReconnectState {
    /// 空闲(未启动)
    Idle,
    /// 等待设备出现
    WaitingForDevice,
    /// 正在扫描(BLE)
    Scanning,
    /// 正在连接
    Connecting,
    /// 已连接
    Connected,
    /// 自动重连被抑制(ble_auto_connect=false)
    Suppressed,
}

/// 按键事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyPressEvent {
    /// 按键索引 0-11
    pub key_index: usize,
    pub key_name: String,
    pub key_value: u16,
    pub pressed: bool,
    pub source: KeySource,
}

/// 按键来源
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum KeySource {
    /// Config 接口(bit mask)
    Config,
    /// Consumer 接口(usage code)
    Consumer,
    /// BLE GATT 事件通道
    Gatt,
}

/// 组合键事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComboKeyEvent {
    pub keys: Vec<usize>,
    pub key_names: Vec<String>,
    pub config_mask: Option<u8>,
}

/// AI 语音键事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiVoiceKeyEvent {
    pub pressed: bool,
}

/// 模式变化事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModeChangeEvent {
    /// "YOLO"/"PLAN"/"CHAT"
    pub mode: String,
    pub mode_value: u8,
    pub source: ModeSource,
}

/// 模式变化来源
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ModeSource {
    /// 物理拨杆
    Dial,
    /// 连接建立时上报
    Connection,
}

/// 设备信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub mode: u8,
    pub mac_address: String,
    pub receiver_version: String,
    pub firmware_version: String,
    pub battery_level: u8,
    pub battery_charging: bool,
    pub battery_full: bool,
    pub chip_id: String,
    pub connection_type: ConnectionType,
}

/// 错误事件(非致命)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEvent {
    pub message: String,
    pub recoverable: bool,
}
