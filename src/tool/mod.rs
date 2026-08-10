//! 工具模块 —— 含 IO 的便利函数(非核心 API)。
//!
//! `parse`:设备信息字节解析(USB/GATT 共用)。
//! `msbc_file`:mSBC .bin 文件 → PCM(调试用,需 `ble` feature —— 解码器在
//! LGPL 的 `msbc-decoder` crate 里)。

#[cfg(feature = "ble")]
pub mod msbc_file;
pub mod parse;
