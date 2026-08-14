//! 内核层 —— 纯逻辑,无线程无 IO,无平台依赖。
//!
//! 协议常量、数据结构、事件类型、错误、sink trait、mSBC 解码、按键聚合。
//! 这一层在任何 feature 组合下都编译(default / 仅 test-mode 也行)。

pub mod audio;
pub mod bindings_blob;
pub mod consumer_hold;
pub mod error;
pub mod event;
pub mod key_aggregator;
/// mSBC 解码器 —— 转发自独立的 [`msbc_decoder`] crate。
///
/// **许可证注意**：该 crate 是 FFmpeg SBC 解码器的 bit-exact 翻译，按
/// **LGPL-2.1-or-later** 分发，与本 crate 的 MIT 不同。板载音频在 USB 与 BLE
/// 上都是 mSBC，所以任一传输 feature 都会引入它；只要协议层
/// (`default-features = false`) 就不会有任何 LGPL 代码被编进二进制。
pub mod msbc {
    /// 只编协议层时解码器不在场，但帧长常量仍要能取到 —— 取 MIT 侧协议模块里的同名常量。
    #[cfg(not(any(feature = "usb", feature = "ble")))]
    pub use crate::kernel::protocol_hid::MSBC_FRAME_SIZE;
    #[cfg(any(feature = "usb", feature = "ble"))]
    pub use msbc_decoder::*;
    /// 帧长在两处独立定义：MIT 侧的协议模块，和 LGPL 侧的解码器 crate。协议校验用前者、
    /// 分块解码用后者，一旦漂移会变成「按旧值放行、按新值切块」的静默错配。
    /// msbc-decoder 发布后是 registry 上的 `^0.1`，不再是本仓库这份，所以这条断言必须在。
    #[cfg(any(feature = "usb", feature = "ble"))]
    const _: () =
        assert!(msbc_decoder::MSBC_FRAME_SIZE == crate::kernel::protocol_hid::MSBC_FRAME_SIZE);
}
pub mod protocol_gatt;
pub mod protocol_hid;
pub mod sink;
pub mod types;
