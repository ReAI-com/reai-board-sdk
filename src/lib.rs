//! # ReAI-Vibe-Board Hardware SDK (V2)
//!
//! English:
//! An embeddable Rust crate that encapsulates USB/BLE connectivity,
//! auto-reconnect, the HID protocol, mSBC decoding, and USB Audio capture for
//! the ReAI-Vibe-Board hardware family. Designed to be embedded into any host
//! application that needs to talk to the board.
//!
//! Product site: <https://b.reai.com>
//!
//! 简体中文：
//! 封装 USB/BLE 连接、断线重连、HID 协议、mSBC 解码、USB Audio 采集能力的
//! Rust crate，可嵌入任意应用。
//!
//! ## V2 Architecture (four layers)
//! - [`kernel`] — pure logic kernel (protocol / events / errors / sink / mSBC /
//!   aggregator). No threads, no I/O.
//! - [`runtime`] — tokio async orchestration (device lifecycle / hotplug /
//!   USB / BLE I/O)
//! - [`facade`] — `BoardDevice` high-level entry point, three event flavors
//!   (`events()` / `on_event()` / `subscribe()`)
//! - [`tool`] — I/O-aware helpers (device-info parsing, mSBC file decoding)
//!
//! `runtime`/`facade` require at least one of `usb` / `ble`. With both disabled
//! only `kernel`/`tool` are available (lightweight protocol layer).
//!
//! ## Hardware targets
//! Protocol constants (USB VID/PID, BLE GATT service UUIDs, command opcodes)
//! are tuned for the **ReAI-Vibe-Board** hardware family. See module-level docs
//! for details; they are not generic USB/BLE abstractions.

pub mod dfu;
pub mod kernel;
pub mod tool;

// runtime layer requires at least one of usb / ble; with both disabled only
// kernel/tool are available (lightweight protocol layer).
#[cfg(any(feature = "usb", feature = "ble"))]
pub mod runtime;

#[cfg(any(feature = "usb", feature = "ble"))]
pub mod facade;

// ============ Top-level re-exports ============
pub use kernel::audio::{
    AudioCapabilities, AudioFrame, AudioRouteRequest, AudioStreamAction, AudioStreamScope,
    AudioStreamState, AudioTransport,
};
pub use kernel::error::{BoardError, Result};
#[cfg(feature = "test-mode")]
pub use kernel::event::FactoryKeyEvent;
pub use kernel::event::{BoardEvent, ModeChangeEvent, ModeSource};
pub use kernel::protocol_hid::WorkMode;
#[cfg(feature = "test-mode")]
pub use kernel::protocol_hid::{FactoryKeyControlAck, FactoryKeyControlResult};
pub use kernel::sink; // top-level re-export (consumers use reai_board_sdk::sink::PcmSink)
pub use kernel::types::ConnectionType;

#[cfg(any(feature = "usb", feature = "ble"))]
pub use facade::device::{BoardConfig, BoardDevice, EventListenerHandle};

#[cfg(any(feature = "usb", feature = "ble"))]
pub use facade::events::{EventStream, EventStreamError};

#[cfg(any(feature = "usb", feature = "ble"))]
pub use facade::blocking::BoardDeviceBlocking;

#[cfg(feature = "ble")]
pub use runtime::ble::gatt_client::BleDeviceInfo;
