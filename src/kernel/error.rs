//! SDK 错误类型。
//!
//! V2 splits error variants by source, so downstream consumers can match on
//! the kind and recover appropriately. `Msg` / `Other` are kept as fallbacks;
//! new code is encouraged to use the typed variants.

use thiserror::Error;

/// SDK 统一 Result 别名
pub type Result<T> = std::result::Result<T, BoardError>;

/// SDK 错误
#[derive(Debug, Error)]
pub enum BoardError {
    /// 通用字符串消息(兜底,新代码尽量用细分变体)
    #[error("{0}")]
    Msg(String),

    /// 任意错误兜底(anyhow::Error)
    #[error(transparent)]
    Other(#[from] anyhow::Error),

    /// USB HID 操作失败(hidapi 错误、设备未找到、读超时)
    #[error("HID 错误: {0}")]
    Hid(String),

    /// BLE GATT 操作失败(btleplug 错误、扫描超时、连接断开)
    #[error("BLE 错误: {0}")]
    Ble(String),

    /// IO 错误(文件、管道)
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// 命令响应超时(USB read_timeout 或 GATT 响应 channel 超时)
    #[error("命令超时: {0}")]
    Timeout(String),

    /// 协议错误(响应 CMD 不匹配、长度不足、result!=0)
    #[error("协议错误: {0}")]
    Protocol(String),

    /// 设备未连接
    #[error("设备未连接")]
    NotConnected,

    /// SDK 未启动(需先 start())
    #[error("SDK 未启动")]
    NotStarted,
}

impl BoardError {
    /// 从字符串消息构造(兜底)
    pub fn msg<S: Into<String>>(s: S) -> Self {
        Self::Msg(s.into())
    }
}
