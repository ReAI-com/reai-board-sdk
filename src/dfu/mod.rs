//! DFU 固件升级（基于固件自定义 USB HID DFU 协议）。
//!
//! - [`types`] — `DfuPhase` / `DfuProgress` / `RecoveryOutcome`（任何 feature 都可用）
//! - [`protocol`] — DFU 包编解码 + 校验和 + 响应解析（纯逻辑，任何 feature 都可用）
//! - [`client`] — 完整升级流程编排（依赖 hidapi，**仅 usb feature 下编译**）
//! - [`recover`] — 救砖：把卡在 DFU 的设备踢回正常模式（依赖 hidapi，**仅 usb**）
//!
//! Protocol: see the firmware-side DFU upgrade specification document.
//!
//! ## 接入路径
//!
//! 消费者通过 facade 的 `BoardDevice::start_dfu_upgrade(path, on_progress)` 触发，
//! 该方法内部走 runtime 的 [`BoardDeviceCore::dfu_upgrade`](crate::runtime::device::BoardDeviceCore::dfu_upgrade)，
//! 把整个 [`client::DfuClient::upgrade`] 投到 macOS HID 专用线程跑。
//!
//! ## 防砖保证
//!
//! 固件写入 `PARTITION_FOTA_DATA` 暂存分区（不写主应用分区），END 触发验证；
//! 失败/中断/取消时 host 发 PREPARE+END 让设备重启回旧固件。详见各方法注释。
//!
//! ⚠️ 复位序列是 **PREPARE + END**，不是裸 END —— 固件没收到 PREPARE 时会忽略
//! END（2026-07-25 真机实测）。详见 [`recover`]。

pub mod protocol;
pub mod types;

#[cfg(feature = "usb")]
pub mod client;

#[cfg(feature = "usb")]
pub mod recover;

#[cfg(feature = "usb")]
pub use client::{
    build_enter_dfu_hid_command, DfuClient, ProgressCallback, CMD_ENTER_HID_DFU_MODE,
};

#[cfg(feature = "usb")]
pub use recover::RECOVERY_DECLARED_LEN;

pub use protocol::*;
pub use types::{DfuPhase, DfuProgress, RecoveryOutcome};
