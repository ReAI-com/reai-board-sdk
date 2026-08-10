//! 内核层 —— 纯逻辑,无线程无 IO,无平台依赖。
//!
//! 协议常量、数据结构、事件类型、错误、sink trait、mSBC 解码、按键聚合。
//! 这一层在任何 feature 组合下都编译(default / 仅 test-mode 也行)。

pub mod bindings_blob;
pub mod consumer_hold;
pub mod error;
pub mod event;
pub mod key_aggregator;
/// mSBC 解码器 —— 转发自独立的 [`msbc_decoder`] crate。
///
/// **许可证注意**：该 crate 是 FFmpeg SBC 解码器的 bit-exact 翻译，按
/// **LGPL-2.1-or-later** 分发，与本 crate 的 MIT 不同。因此它是 `ble`
/// feature 下的可选依赖 —— 不开 `ble` 就不会有任何 LGPL 代码被编进二进制。
#[cfg(feature = "ble")]
pub mod msbc {
    pub use msbc_decoder::*;
}
pub mod protocol_gatt;
pub mod protocol_hid;
pub mod sink;
pub mod types;
