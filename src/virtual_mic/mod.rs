//! 系统级虚拟麦克风（macOS / Windows）。
//!
//! 这是跨平台的 SDK 合同：调用方始终使用 [`VirtualMicConfig`] 放进
//! [`crate::BoardConfig`]，或手动调用 [`VirtualMic`]。平台后端只负责把
//! 已解码的 16 kHz 单声道 PCM 暴露给各自的系统音频栈。
//!
//! macOS 使用随 crate 构建的 CoreAudio HAL 插件；Windows 使用开发者以 WDK
//! 构建并测试签名的 SysVAD 派生驱动。Windows 驱动包位置由
//! `REAI_VIRTUAL_MIC_DRIVER_DIR` 指定；签名与测试签名模式均由集成方负责。

/// 设备在系统声音设置中呈现的名称。
pub const DEVICE_NAME: &str = "ReAI-Vibe-Board";

/// 虚拟麦克风启动开关（放 [`crate::BoardConfig::virtual_mic`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualMicConfig {
    /// 是否在首次建立音频链路时启用。
    pub enabled: bool,
    /// 驱动缺失时是否请求安装；失败只会降级并记录日志。
    pub auto_install: bool,
}

impl Default for VirtualMicConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            auto_install: false,
        }
    }
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::VirtualMic;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::VirtualMic;
