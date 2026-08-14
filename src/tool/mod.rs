//! 工具模块 —— 含 IO 的便利函数(非核心 API)。
//!
//! `parse`:设备信息字节解析(USB/GATT 共用)。
//! `msbc_file`:mSBC .bin 文件 → PCM(调试用,需任一传输 feature(`usb` / `ble`)
//! —— 解码器在 LGPL 的 `msbc-decoder` crate 里)。

#[cfg(any(feature = "usb", feature = "ble"))]
pub mod msbc_file;
pub mod parse;
