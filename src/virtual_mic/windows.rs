//! Windows development-mode backend.
//!
//! The companion SysVAD-derived driver (source in
//! `virtual-mic/driver-windows/`, built with the WDK and test-signed) owns
//! the root-enumerated device `ROOT\ReAIVB` and exposes two things:
//!
//! - the capture endpoint **"ReAI-Vibe-Board"** (16 kHz mono, native rate —
//!   the Windows audio engine resamples for apps), and
//! - a control device `\\.\ReAIVibeBoardVirtualMic` accepting little-endian
//!   S16 writes that are drained into the capture stream (silence on
//!   underflow; overflow drops the incoming remainder, never queues latency).
//!
//! The `.sys`/`.cat` must sit next to the INF; the package directory is
//! given through `REAI_VIRTUAL_MIC_DRIVER_DIR`. Loading a test-signed driver
//! additionally requires test signing mode (`bcdedit /set TESTSIGNING ON` +
//! reboot) and the test certificate — signing is the integrator's job.

use std::env;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::kernel::error::{BoardError, Result};
use crate::kernel::sink::PcmSink;

const CONTROL_DEVICE_PATH: &str = r"\\.\ReAIVibeBoardVirtualMic";
const DRIVER_DIR_ENV: &str = "REAI_VIRTUAL_MIC_DRIVER_DIR";
const DRIVER_INF: &str = "ReAIVibeBoardVirtualMic.inf";
const ROOT_DEVICE_ID: &str = "ROOT\\REAIVB\\0000";
/// How long `ensure_installed` waits for the control device to appear after
/// pnputil reports success (the service starts asynchronously).
const DEVICE_SETTLE_TIMEOUT: Duration = Duration::from_secs(5);

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
        if Self::wait_for_device(Instant::now()) {
            return Ok(());
        }
        let dir = env::var_os(DRIVER_DIR_ENV).ok_or_else(|| {
            BoardError::Msg(format!(
                "Windows virtual-mic 驱动未安装；请设置 {DRIVER_DIR_ENV} 指向已测试签名的 WDK 驱动包"
            ))
        })?;
        let inf = PathBuf::from(&dir).join(DRIVER_INF);
        if !inf.is_file() {
            return Err(BoardError::Msg(format!(
                "Windows virtual-mic INF 不存在：{}（.cat/.sys 需与 INF 同目录）",
                inf.display()
            )));
        }
        run_elevated_pnputil(&["/add-driver", &inf.display().to_string(), "/install"])?;
        if !Self::wait_for_device(Instant::now() + DEVICE_SETTLE_TIMEOUT) {
            return Err(BoardError::Msg(
                "Windows virtual-mic 驱动安装后未出现控制设备；确认测试签名模式（bcdedit /set TESTSIGNING ON 并重启）、测试证书已导入、INF 与 WDK 构建产物齐全".into(),
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

    /// Poll for the control device until `deadline`. `Instant::now()` means
    /// "check once, do not wait".
    fn wait_for_device(deadline: Instant) -> bool {
        loop {
            if Self::open_device().is_ok() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

impl PcmSink for VirtualMic {
    fn on_pcm(&self, samples: &[f32]) {
        let payload = s16le_payload(samples);
        // The driver owns a bounded producer ring. Never block the decoder:
        // failure/fullness drops a frame instead of accumulating latency.
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write(&payload);
        }
    }
}

/// Encode f32 samples [-1, 1] as little-endian S16, the driver's wire format.
fn s16le_payload(samples: &[f32]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(samples.len() * 2);
    for &sample in samples {
        payload.extend_from_slice(&f32_to_s16(sample).to_le_bytes());
    }
    payload
}

fn f32_to_s16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32767.0) as i16
}

/// Run pnputil elevated via a UAC prompt. Returns an error when the user
/// cancels or pnputil itself fails.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s16le_payload_encodes_native_rate_mono() {
        // 16 kHz mono f32 in, little-endian S16 bytes out.
        let payload = s16le_payload(&[0.0, 1.0, -1.0, 0.5]);
        assert_eq!(
            payload,
            vec![0x00, 0x00, 0xFF, 0x7F, 0x01, 0x80, 0xFF, 0x3F]
        );
    }

    #[test]
    fn s16le_payload_clamps_out_of_range_samples() {
        let payload = s16le_payload(&[2.0, -2.0]);
        assert_eq!(payload, vec![0xFF, 0x7F, 0x01, 0x80]);
    }

    #[test]
    fn s16le_payload_empty_is_empty() {
        assert!(s16le_payload(&[]).is_empty());
    }
}
