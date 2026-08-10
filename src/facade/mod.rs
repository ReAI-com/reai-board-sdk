//! 门面层 —— BoardDevice 高级 API,三层事件门面,blocking 桥。
//!
//! 对消费者暴露的统一入口:
//! - [`device::BoardDevice`] 持有 `runtime::BoardDeviceCore`,提供 `events()` /
//!   `on_event()` / `subscribe()` 三种事件消费形态 + async 命令 + impl Drop 兜底
//! - [`events::EventStream`] 包装 broadcast::Receiver,支持 recv/blocking_recv/impl Stream
//! - [`blocking::BoardDeviceBlocking`] — sync-context bridge for consumers that
//!   cannot await directly (CLI subcommands, threads not running a tokio runtime, etc.)
//!
//! facade 层需至少 usb 或 ble 之一(否则 runtime::BoardDeviceCore 不存在)。

#[cfg(any(feature = "usb", feature = "ble"))]
pub mod device;

#[cfg(any(feature = "usb", feature = "ble"))]
pub mod events;

#[cfg(any(feature = "usb", feature = "ble"))]
pub mod blocking;
