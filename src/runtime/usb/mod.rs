//! USB HID 平台层(usb feature)。
//!
//! `device_manager` / `monitor` 从 V1 原样搬来(hidapi 阻塞调用),
//! 在 V2 由 `runtime::device` 用 `spawn_blocking` 包装调用。

pub mod device_manager;
pub mod monitor;
