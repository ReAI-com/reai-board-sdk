//! Windows development-mode backend.
//!
//! The companion SysVAD-derived driver owns `\\.\ReAIVibeBoardVirtualMic`.
//! It accepts little-endian S16LE 16 kHz mono PCM through buffered writes and
//! supplies those samples from its WaveRT capture pin.

use std::env;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

use crate::kernel::error::{BoardError, Result};
use crate::kernel::sink::PcmSink;

const CONTROL_DEVICE_PATH: &str = r"\\.\ReAIVibeBoardVirtualMic";
const DRIVER_DIR_ENV: &str = "REAI_VIRTUAL_MIC_DRIVER_DIR";
const DRIVER_INF: &str = "ReAIVibeBoardVirtualMic.inf";
const ROOT_DEVICE_ID: &str = "ROOT\\REAIVB\\0000";

/// Windows sender for the common [`PcmSink`] contract.
pub struct VirtualMic {
    file: Mutex<File>,
}

impl VirtualMic {
    /// True when the driver control device is active and this process can feed it.
    pub fn is_installed() -> bool {
        Self::open_device().is_ok()
    }

    /// Install the developer-provided, test-signed driver package.
    ///
    /// The package directory must be supplied through
    /// `REAI_VIRTUAL_MIC_DRIVER_DIR`; it must contain the signed `.inf`, `.cat`
    /// and `.sys` produced with the Windows WDK. This method deliberately does
    /// not create certificates, enable test-signing mode, or sign binaries.
    pub fn ensure_installed() -> Result<()> {
        if Self::is_installed() {
            return Ok(());
        }
        let dir = env::var_os(DRIVER_DIR_ENV).ok_or_else(|| {
            BoardError::Msg(format!(
                "Windows virtual-mic 驱动未安装；请设置 {DRIVER_DIR_ENV} 指向已测试签名的 WDK 驱动包"
            ))
        })?;
        let inf = PathBuf::from(dir).join(DRIVER_INF);
        if !inf.is_file() {
            return Err(BoardError::Msg(format!(
                "Windows virtual-mic INF 不存在：{}",
                inf.display()
            )));
        }
        run_elevated_pnputil(&["/add-driver", &inf.display().to_string(), "/install"])?;
        if !Self::is_installed() {
            return Err(BoardError::Msg(
                "Windows virtual-mic 驱动安装后未出现控制设备；确认测试签名模式、INF 与 WDK 构建产物".into(),
            ));
        }
        Ok(())
    }

    /// Remove the root-enumerated development device. The package remains in
    /// the Driver Store so a later `ensure_installed()` is cheap.
    pub fn uninstall() -> Result<()> {
        run_elevated_pnputil(&["/remove-device", ROOT_DEVICE_ID])
    }

    /// Connect to the running driver. It stays silent until PCM is supplied.
    pub fn start() -> Result<Self> {
        Ok(Self {
            file: Mutex::new(Self::open_device()?),
        })
    }

    fn open_device() -> std::io::Result<File> {
        OpenOptions::new().write(true).open(CONTROL_DEVICE_PATH)
    }
}

impl PcmSink for VirtualMic {
    fn on_pcm(&self, samples: &[f32]) {
        let mut payload = Vec::with_capacity(samples.len() * 2);
        for &sample in samples {
            payload.extend_from_slice(&((sample.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
        }
        // The driver owns a bounded producer ring. Never block the decoder:
        // failure/fullness drops a frame instead of accumulating latency.
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write(&payload);
        }
    }
}

fn run_elevated_pnputil(args: &[&str]) -> Result<()> {
    let arguments = args
        .iter()
        .map(|arg| format!("'{}'", arg.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    let script = format!(
        "$p = Start-Process -FilePath pnputil.exe -ArgumentList @({arguments}) -Verb RunAs -Wait -PassThru; exit $p.ExitCode"
    );
    let status = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .status()?;
    if !status.success() {
        return Err(BoardError::Msg(
            "Windows virtual-mic 驱动安装/卸载失败或被用户取消".into(),
        ));
    }
    Ok(())
}
