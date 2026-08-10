//! 编排层 —— tokio async,设备生命周期 / 热插拔 / IO 操作。
//!
//! 这一层在 V2 全部 tokio 化:
//! - `device::BoardDeviceCore` async 内核(持有调用方 runtime Handle)
//! - `hotplug::HotplugManager` async 四阶段状态机
//! - `usb::*`(usb feature):hidapi 阻塞操作放 spawn_blocking
//! - `ble::gatt_client`(ble feature):btleplug 原生 async
//! - `usb_capture`(usb feature):cpal UAC 采集
//!
//! runtime 层代表"USB+BLE 编排核心",需要至少 usb 或 ble 之一;
//! 两者都关闭时(runtime 不存在),仅 kernel/tool 可用(轻量协议层)。

#[cfg(any(feature = "usb", feature = "ble"))]
pub mod device;

#[cfg(any(feature = "usb", feature = "ble"))]
pub mod hotplug;

#[cfg(feature = "usb")]
pub mod usb;

#[cfg(feature = "ble")]
pub mod ble;

#[cfg(feature = "usb")]
pub mod usb_capture;
