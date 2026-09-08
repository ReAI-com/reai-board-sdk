//! macOS virtual microphone backend.
//!
//! BLE 链路是自定义 GATT + mSBC,系统蓝牙栈不会为它创建音频设备(USB 时硬件
//! 本身是 UAC 声卡,无需本模块)。本模块配合本 crate 附带的 CoreAudio HAL
//! 插件(源码在 `virtual-mic/`,随 `virtual-mic` feature 由 build.rs 用 cmake
//! 编译),在系统中注册一个 16 kHz 单声道的输入设备 **"ReAI-Vibe-Board"**,
//! 并把 [`PcmSink`] 收到的解码 PCM 通过 UDP 泵给它——此后系统设置与任意
//! App 都能直接选中该"麦克风"。
//!
//! # 典型用法
//!
//! 启动开关（推荐,SDK 自管生命周期,失败只降级）:
//!
//! ```no_run
//! use reai_board_sdk::{BoardConfig, BoardDevice, virtual_mic::VirtualMicConfig};
//!
//! # fn main() -> anyhow::Result<()> {
//! let device = BoardDevice::open(BoardConfig {
//!     virtual_mic: VirtualMicConfig { enabled: true, auto_install: true },
//!     ..Default::default()
//! })?;
//! # Ok(())
//! # }
//! ```
//!
//! 手工接管（等价,自行控制安装与启停）:
//!
//! ```no_run
//! use std::sync::Arc;
//! use reai_board_sdk::virtual_mic::VirtualMic;
//!
//! # fn main() -> reai_board_sdk::Result<()> {
//! // 一次性安装(需要管理员授权,会弹系统密码框;已装则覆盖升级)
//! if !VirtualMic::is_installed() {
//!     VirtualMic::ensure_installed()?;
//! }
//!
//! // start 后作为 PcmSink 交给 BoardDevice::set_pcm_sink(mic),
//! // BLE 连接并 start_board_audio 后系统即可从 "ReAI-Vibe-Board" 录音
//! let mic = Arc::new(VirtualMic::start()?);
//! # let _ = mic; // 完整接线见 examples/virtual_mic_demo.rs
//! # Ok(())
//! # }
//! ```
//!
//! # 平台与开关
//!
//! `virtual-mic` feature 默认关闭。构建 macOS 后端需要 cmake（另需 Xcode CLT）。
//!
//! # 注意事项(实测结论)
//!
//! - **消费方需要麦克风权限**:虚拟设备与物理麦克风一样受 macOS TCC 管辖。
//!   读取该设备的 App 必须是带 `NSMicrophoneUsageDescription` 的普通 App 并
//!   主动请求授权;`AVAudioEngine` 不会触发授权弹窗,未授权时读到的是**静默
//!   全零**(不是报错),极易误判为设备故障。
//! - 音质上限为 16 kHz 单声道(BLE mSBC 的带宽决定),系统按需自动做采样率
//!   转换;设备原生就是 16k,本模块不做重采样。
//! - 没有数据泵入时设备呈现静音;BLE 断开重连无需重新安装驱动。
//! - 驱动声明了 `CanBeDefault=false`,不会被系统选为默认输入设备。

use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::kernel::error::{BoardError, Result};
use crate::kernel::sink::PcmSink;

/// HAL 插件 bundle 文件名(安装到 `/Library/Audio/Plug-Ins/HAL/` 下)。
pub const DRIVER_BUNDLE_NAME: &str = "ReAIVibeBoard.driver";

/// 驱动内置的 UDP 接收端口(仅回环)。
pub const UDP_PORT: u16 = 47160;

const HAL_PLUGINS_DIR: &str = "/Library/Audio/Plug-Ins/HAL";

fn bundled_driver_path() -> PathBuf {
    // 测试/CI 可覆盖到任意已构建 bundle
    if let Some(path) = std::env::var_os("REAI_VIRTUAL_MIC_DRIVER_OVERRIDE") {
        return PathBuf::from(path);
    }
    PathBuf::from(env!("REAI_VIRTUAL_MIC_DRIVER"))
}

/// 虚拟麦克风发送端:实现 [`PcmSink`],把解码 PCM 泵入系统设备。
///
/// [`VirtualMic::start`] 后交给 [`crate::BoardDevice::set_pcm_sink`]。
/// 发送失败(驱动未装、coreaudiod 重启瞬间等)一律静默忽略——设备的静音
/// 由驱动侧兜底,发送端无需重试。
pub struct VirtualMic {
    socket: UdpSocket,
}

impl VirtualMic {
    /// 驱动在系统中的安装位置。
    pub fn installed_driver_path() -> PathBuf {
        Path::new(HAL_PLUGINS_DIR).join(DRIVER_BUNDLE_NAME)
    }

    /// 驱动是否已安装(存在性检查;不校验版本)。
    pub fn is_installed() -> bool {
        Self::installed_driver_path().exists()
    }

    /// 安装/升级驱动:拷贝 bundle 到 HAL 目录并重启 coreaudiod。
    ///
    /// 需要管理员授权,会弹出系统密码框(用户取消则返回错误)。重复调用安全,
    /// 即覆盖升级;装完设备即刻出现在系统中。
    pub fn ensure_installed() -> Result<()> {
        let source = bundled_driver_path();
        if !source.exists() {
            return Err(BoardError::Msg(format!(
                "virtual-mic 驱动 bundle 未随构建产出: {}(确认以 --features virtual-mic 构建)",
                source.display()
            )));
        }

        let dest = Self::installed_driver_path();
        // launchctl kickstart 在 osascript 提权环境下被 SIP 拦截,用 killall
        // (launchd 会自动拉起 coreaudiod)
        let script = format!(
            "rm -rf '{dest}' && cp -fR '{src}' '{hal}/' && killall coreaudiod",
            dest = dest.display(),
            src = source.display(),
            hal = HAL_PLUGINS_DIR,
        );
        let osascript = format!(
            "do shell script \"{}\" with administrator privileges",
            script
        );

        let status = Command::new("osascript")
            .arg("-e")
            .arg(&osascript)
            .status()?;
        if !status.success() {
            return Err(BoardError::Msg(
                "virtual-mic 驱动安装失败或被用户取消".into(),
            ));
        }
        Ok(())
    }

    /// 卸载驱动(需要管理员授权)。卸载后 "ReAI-Vibe-Board" 从系统消失。
    pub fn uninstall() -> Result<()> {
        let dest = Self::installed_driver_path();
        let script = format!(
            "rm -rf '{dest}' && killall coreaudiod",
            dest = dest.display()
        );
        let osascript = format!(
            "do shell script \"{}\" with administrator privileges",
            script
        );
        let status = Command::new("osascript")
            .arg("-e")
            .arg(&osascript)
            .status()?;
        if !status.success() {
            return Err(BoardError::Msg(
                "virtual-mic 驱动卸载失败或被用户取消".into(),
            ));
        }
        Ok(())
    }

    /// 打开发送端并连接到驱动接收端口。
    ///
    /// 不校验驱动是否已安装;未安装时发送静默失败,设备保持静音。
    pub fn start() -> Result<Self> {
        let socket = UdpSocket::bind("127.0.0.1:0")?;
        socket.connect(("127.0.0.1", UDP_PORT))?;
        Ok(Self { socket })
    }
}

impl PcmSink for VirtualMic {
    fn on_pcm(&self, samples: &[f32]) {
        let mut payload = Vec::with_capacity(samples.len() * 2);
        for &sample in samples {
            let value = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
            payload.extend_from_slice(&value.to_le_bytes());
        }
        let _ = self.socket.send(&payload);
    }
}
