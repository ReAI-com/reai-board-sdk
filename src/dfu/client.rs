//! DFU 客户端 —— 完整固件升级流程编排（同步阻塞实现）。
//!
//! 流程: `enter_dfu` 闭包 → 等 DFU 设备枚举 → PREPARE/START → DATA 循环 → END → 等正常设备重启。
//!
//! 设计：本模块是**纯 hidapi 同步阻塞**实现，
//! 由 runtime 层（`crate::runtime::device::BoardDeviceCore::dfu_upgrade`）通过
//! `spawn_blocking_with_runloop` 把整个 `upgrade` 投到 macOS HID 专用线程跑。
//! 进度通过 `Arc<dyn Fn(DfuProgress) + Send + Sync>` 闭包上报；取消通过
//! `Arc<AtomicBool>` 检查（DATA 循环每包查一次）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};

use super::protocol::*;
use super::types::{DfuPhase, DfuProgress};

/// 正常模式 HID report id（与 kernel::protocol_hid::REPORT_ID_OUTPUT 一致，进 DFU 命令复用）
const REPORT_ID_OUTPUT: u8 = crate::kernel::protocol_hid::REPORT_ID_OUTPUT;
/// VID（与正常模式一致）
pub(crate) const VID: u16 = crate::kernel::protocol_hid::VID;
/// 正常模式 PID（0xED20），用于 END 后等设备回正常模式
pub(crate) const NORMAL_PID: u16 = 0xED20;

/// 进度回调类型（跨 spawn_blocking 边界，需 Send + Sync）。
pub type ProgressCallback = Arc<dyn Fn(DfuProgress) + Send + Sync>;

/// DFU 升级客户端。一次实例对应一次升级流程。
pub struct DfuClient {
    cancel_flag: Arc<AtomicBool>,
    on_progress: ProgressCallback,
}

impl DfuClient {
    /// 构造。`cancel_flag` 置 true 后 DATA 循环下次检查即终止并尝试复位设备。
    pub fn new(cancel_flag: Arc<AtomicBool>, on_progress: ProgressCallback) -> Self {
        Self {
            cancel_flag,
            on_progress,
        }
    }

    /// 执行完整 DFU 升级流程。**同步阻塞**，调用方应放进 spawn_blocking。
    ///
    /// `firmware` 为完整固件字节（由调用方先读入内存）。
    /// `enter_dfu` 闭包负责"发 CMD 0xEF 进 DFU + 等设备消失"（runtime 层实现，
    /// 因为它需要正常模式 HID 通道 + PauseGuard 暂停 monitor）。
    pub fn upgrade<F>(&self, firmware: &[u8], enter_dfu: F) -> Result<()>
    where
        F: FnOnce() -> Result<()>,
    {
        let total_len = firmware.len() as u32;
        if total_len == 0 {
            return Err(anyhow!("固件文件为空"));
        }
        log::info!(target: "board", "[dfu] 开始升级 ({} bytes)", total_len);

        // 1. 进入 DFU 模式（闭包内部完成：暂停 monitor → 发 0xEF → 等设备消失）
        self.emit(DfuPhase::EnteringDfu, 0, total_len, None);
        log::info!(target: "board", "[dfu] 进入 DFU 模式...");
        if let Err(e) = enter_dfu() {
            let msg = format!("进入 DFU 失败: {e}");
            self.emit_failed(0, total_len, &msg);
            return Err(anyhow!(msg));
        }

        // 2. 等待 DFU 设备枚举（PID 0xFF06）
        self.emit(
            DfuPhase::EnteringDfu,
            0,
            total_len,
            Some("等待 DFU 设备...".into()),
        );
        let dfu_device = match self.wait_for_dfu_device() {
            Ok(d) => d,
            Err(e) => {
                let msg = format!("{e}");
                self.emit_failed(0, total_len, &msg);
                return Err(anyhow!(msg));
            }
        };
        log::info!(target: "board", "[dfu] DFU 设备已连接 (PID 0xFF06)");

        // 3. PREPARE（固件 Flash 初始化慢，长超时 + 重试）
        self.emit(DfuPhase::Preparing, 0, total_len, None);
        if let Err(e) = self.run_prepare(&dfu_device, total_len) {
            self.fail_with_reset(&dfu_device, 0, total_len, "PREPARE", e);
            return Err(anyhow!("PREPARE 失败"));
        }
        log::debug!(target: "board", "[dfu] PREPARE 成功");

        // 4. START
        if let Err(e) = self.run_start(&dfu_device) {
            self.fail_with_reset(&dfu_device, 0, total_len, "START", e);
            return Err(anyhow!("START 失败"));
        }
        log::debug!(target: "board", "[dfu] START 成功");

        // 5. DATA 循环（250B/包，每包失败重试 ≤3，超限复位）
        self.emit(DfuPhase::Transferring, 0, total_len, None);
        // run_data_loop 内部已处理复位和进度上报
        self.run_data_loop(&dfu_device, firmware, total_len)?;
        log::info!(target: "board", "[dfu] DATA 传输完成 ({} bytes)", total_len);

        // 6. END（设备验证 Flash 后重启）
        self.emit(
            DfuPhase::Verifying,
            total_len,
            total_len,
            Some("设备正在验证固件...".into()),
        );
        match self.send_and_recv(&dfu_device, &DfuPacketEncoder::end(), DFU_END_TIMEOUT_MS) {
            Ok(resp) if resp.is_success() => {
                log::info!(target: "board", "[dfu] END 验证成功，设备即将重启");
            }
            Ok(_) => {
                // END 返回错误：固件验证失败，设备通常会自行重启回旧固件
                log::warn!(target: "board", "[dfu] END 验证失败，设备可能重启回旧固件");
            }
            Err(e) => {
                // END 通信失败：仍尝试发 END 复位（可能已经离线，忽略错误）
                log::warn!(target: "board", "[dfu] END 通信失败: {e}（设备可能需要物理重插）");
            }
        }

        // 7. 等待设备重启回正常模式（PID 0xED20）
        self.emit(
            DfuPhase::Rebooting,
            total_len,
            total_len,
            Some("设备重启中...".into()),
        );
        self.wait_for_normal_device();

        self.emit(DfuPhase::Completed, total_len, total_len, None);
        log::info!(target: "board", "[dfu] 固件升级完成");
        Ok(())
    }

    // ================================================================
    // 各阶段实现
    // ================================================================

    fn run_prepare(&self, device: &hidapi::HidDevice, total_len: u32) -> Result<()> {
        let resp = self.send_and_recv_with_retry(
            device,
            &DfuPacketEncoder::prepare(total_len),
            DFU_PREPARE_TIMEOUT_MS,
            2,
            "PREPARE",
        )?;
        if !resp.is_success() {
            return Err(anyhow!("设备返回错误 result=0x{:02X}", resp.result));
        }
        Ok(())
    }

    fn run_start(&self, device: &hidapi::HidDevice) -> Result<()> {
        let resp = self.send_and_recv_with_retry(
            device,
            &DfuPacketEncoder::start(),
            DFU_PREPARE_TIMEOUT_MS,
            2,
            "START",
        )?;
        if !resp.is_success() {
            return Err(anyhow!("设备返回错误 result=0x{:02X}", resp.result));
        }
        Ok(())
    }

    fn run_data_loop(
        &self,
        device: &hidapi::HidDevice,
        firmware: &[u8],
        total_len: u32,
    ) -> Result<()> {
        let mut offset: usize = 0;
        let mut retry_count: u8 = 0;
        const MAX_RETRY: u8 = 3;

        while offset < firmware.len() {
            // 取消检查（每包一次）
            if self.cancel_flag.load(Ordering::SeqCst) {
                log::warn!(target: "board", "[dfu] 用户取消 (offset={})", offset);
                self.try_reset_device(device);
                let msg = "用户取消升级".to_string();
                self.emit_failed(offset as u32, total_len, &msg);
                return Err(anyhow!(msg));
            }

            let end = (offset + DFU_DATA_PAYLOAD_MAX).min(firmware.len());
            let chunk = &firmware[offset..end];
            let packet = DfuPacketEncoder::data(chunk)?;

            match self.send_and_recv(device, &packet, DFU_RW_TIMEOUT_MS) {
                Ok(resp) if resp.is_success() => {
                    offset = end;
                    retry_count = 0;
                    self.emit(DfuPhase::Transferring, offset as u32, total_len, None);
                }
                Ok(resp) => {
                    retry_count += 1;
                    log::warn!(
                        target: "board",
                        "[dfu] DATA 失败 (offset={}, retry={}/{}): result=0x{:02X}",
                        offset, retry_count, MAX_RETRY, resp.result
                    );
                    if retry_count >= MAX_RETRY {
                        self.try_reset_device(device);
                        let msg =
                            format!("DATA 传输失败，重试 {MAX_RETRY} 次后放弃 (offset={offset})");
                        self.emit_failed(offset as u32, total_len, &msg);
                        return Err(anyhow!(msg));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    retry_count += 1;
                    log::warn!(
                        target: "board",
                        "[dfu] DATA 通信错误 (offset={}, retry={}/{}): {}",
                        offset, retry_count, MAX_RETRY, e
                    );
                    if retry_count >= MAX_RETRY {
                        self.try_reset_device(device);
                        let msg = format!("DATA 通信失败: {e} (offset={offset})");
                        self.emit_failed(offset as u32, total_len, &msg);
                        return Err(anyhow!(msg));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
        Ok(())
    }

    // ================================================================
    // 底层收发
    // ================================================================

    /// 尝试重置 DFU 设备：走 PREPARE+END 复位序列触发验证失败→重启（失败/取消路径用）。
    ///
    /// 固件写入的是 FOTA 暂存分区，不是主应用分区。即使数据不完整，复位后
    /// 固件验证失败也会重启回正常模式（旧固件不受影响）。
    ///
    /// ⚠️ 这里**必须**走完整复位序列而不是裸 END：固件没收到 PREPARE 时会忽略
    /// END（2026-07-25 真机实测），此前只发 END 的实现从未真正复位过设备。
    /// 与救砖路径共用 [`recover::send_recovery_sequence_on`] 的同一份实现，
    /// 避免两条路径行为分裂。
    fn try_reset_device(&self, device: &hidapi::HidDevice) {
        // 一个包都没写出去（设备已拔线等）就别等了，干等只会拖慢失败反馈。
        if super::recover::send_recovery_sequence_on(device) {
            self.wait_for_normal_device();
        } else {
            log::warn!(target: "board", "[dfu] 复位包未能送达设备，跳过等待（设备可能需要物理重插）");
        }
    }

    fn send_and_recv(
        &self,
        device: &hidapi::HidDevice,
        packet: &[u8],
        timeout_ms: i32,
    ) -> Result<DfuResponse> {
        send_and_recv(device, packet, timeout_ms)
    }

    /// 带重试的 send_and_recv —— 用于 PREPARE/START 这类固件响应慢、偶发超时的命令。
    ///
    /// 固件进 DFU 后内部 Flash 初始化耗时不稳定（实测 ~2s，偶尔 >3s），
    /// 单次超时不代表失败，重发同一命令通常能成功。
    /// PREPARE/START 是幂等的（通知大小 / 初始化缓冲区），重发安全。
    fn send_and_recv_with_retry(
        &self,
        device: &hidapi::HidDevice,
        packet: &[u8],
        timeout_ms: i32,
        max_attempts: u8,
        label: &str,
    ) -> Result<DfuResponse> {
        let mut last_err = String::new();
        for attempt in 1..=max_attempts {
            if attempt > 1 {
                log::warn!(target: "board", "[dfu] {label} 超时，重试 {attempt}/{max_attempts}");
                std::thread::sleep(Duration::from_millis(500));
            }
            match self.send_and_recv(device, packet, timeout_ms) {
                Ok(resp) => return Ok(resp),
                Err(e) => last_err = e.to_string(),
            }
        }
        Err(anyhow!("{label} 失败: {last_err}"))
    }

    // ================================================================
    // 设备枚举
    // ================================================================

    fn wait_for_dfu_device(&self) -> Result<hidapi::HidDevice> {
        let poll = Duration::from_millis(500);
        let timeout = Duration::from_secs(WAIT_DFU_DEVICE_TIMEOUT_SECS);
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if self.cancel_flag.load(Ordering::SeqCst) {
                return Err(anyhow!("用户取消"));
            }
            if let Ok(dev) = open_dfu_device() {
                return Ok(dev);
            }
            std::thread::sleep(poll);
        }
        Err(anyhow!(
            "等待 DFU 设备超时 ({}s)",
            WAIT_DFU_DEVICE_TIMEOUT_SECS
        ))
    }

    fn wait_for_normal_device(&self) {
        let poll = Duration::from_millis(500);
        let timeout = Duration::from_secs(WAIT_NORMAL_DEVICE_TIMEOUT_SECS);
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if let Ok(api) = hidapi::HidApi::new() {
                let found = api
                    .device_list()
                    .any(|d| d.vendor_id() == VID && d.product_id() == NORMAL_PID);
                if found {
                    log::info!(target: "board", "[dfu] 正常设备已重新枚举");
                    return;
                }
            }
            std::thread::sleep(poll);
        }
        log::warn!(
            target: "board",
            "[dfu] 等待正常设备超时 ({}s)，hotplug 将自动重连",
            WAIT_NORMAL_DEVICE_TIMEOUT_SECS
        );
    }

    // ================================================================
    // 进度上报
    // ================================================================

    fn emit(&self, phase: DfuPhase, bytes_written: u32, total_bytes: u32, message: Option<String>) {
        let p = DfuProgress::new(phase, bytes_written, total_bytes, message);
        log::debug!(target: "board", "[dfu] progress {:?} {}% ({}/{})", p.phase, p.percent, p.bytes_written, p.total_bytes);
        (self.on_progress)(p);
    }

    fn emit_failed(&self, bytes_written: u32, total_bytes: u32, msg: &str) {
        log::warn!(target: "board", "[dfu] 升级失败: {msg}");
        let p = DfuProgress::new(
            DfuPhase::Failed,
            bytes_written,
            total_bytes,
            Some(msg.to_string()),
        );
        (self.on_progress)(p);
    }

    /// 阶段失败统一处理：复位设备 + 上报 Failed 进度。
    fn fail_with_reset(
        &self,
        device: &hidapi::HidDevice,
        bytes_written: u32,
        total_bytes: u32,
        label: &str,
        e: anyhow::Error,
    ) {
        log::warn!(target: "board", "[dfu] {label} 失败: {e}");
        self.try_reset_device(device);
        self.emit_failed(bytes_written, total_bytes, &format!("{label} 失败: {e}"));
    }
}

// ================================================================
// DFU 设备底层操作（模块级 —— 升级流程与救砖流程共用）
// ================================================================

/// 打开 DFU 模式设备（VID=0x363C, PID=0xFF06）并设为阻塞模式。
///
/// 设为模块级函数而非 [`DfuClient`] 方法：救砖流程不需要 `cancel_flag` /
/// `on_progress`，不应被迫构造一个 `DfuClient`。
///
/// **调用方须保证运行在 HID 专用线程上**（见 `runtime::hotplug::spawn_blocking_with_runloop`）。
pub(crate) fn open_dfu_device() -> Result<hidapi::HidDevice> {
    let api = hidapi::HidApi::new().map_err(|e| anyhow!("HidApi 创建失败: {e}"))?;
    let dev_info = api
        .device_list()
        .find(|d| d.vendor_id() == VID && d.product_id() == DFU_PID)
        .ok_or_else(|| anyhow!("DFU 设备未找到"))?;
    let device = api
        .open_path(dev_info.path())
        .map_err(|e| anyhow!("打开 DFU 设备失败: {e}"))?;
    device
        .set_blocking_mode(true)
        .map_err(|e| anyhow!("设置阻塞模式失败: {e}"))?;
    Ok(device)
}

/// 向 DFU 设备发一个包并读回响应。
///
/// **调用方须保证运行在 HID 专用线程上**。
pub(crate) fn send_and_recv(
    device: &hidapi::HidDevice,
    packet: &[u8],
    timeout_ms: i32,
) -> Result<DfuResponse> {
    device
        .write(packet)
        .map_err(|e| anyhow!("DFU write 失败: {e}"))?;
    read_response(device, timeout_ms)
}

/// 只读一个 DFU 响应（不发包）。
///
/// 与 [`send_and_recv`] 拆开是为了让救砖路径能区分「包没写出去」和
/// 「写出去了但没回话」—— 后者在 END 上是正常且期待的（设备重启来不及回包）。
pub(crate) fn read_response(device: &hidapi::HidDevice, timeout_ms: i32) -> Result<DfuResponse> {
    let mut buf = [0u8; DFU_INPUT_MAX_SIZE];
    let len = device
        .read_timeout(&mut buf, timeout_ms)
        .map_err(|e| anyhow!("DFU read 失败: {e}"))?;

    if len == 0 {
        return Err(anyhow!("DFU 响应超时 ({timeout_ms}ms)"));
    }

    // macOS hidapi read 包含 Report ID 字节（buf[0] = 0xA1），需要跳过。
    // 其他平台 report_id 字节可能不存在（hidapi 行为差异），根据 buf[0] 判断。
    let data_start = if len > 1 && buf[0] == DFU_REPORT_ID_INPUT {
        1
    } else {
        0
    };
    DfuResponse::parse(&buf[data_start..len])
}

// ================================================================
// 进入 DFU 模式的命令包（CMD 0xEF，正常 HID 通道发送）
// ================================================================

/// 进入 DFU 模式命令字节常量（正常模式 HID CMD）
pub const CMD_ENTER_HID_DFU_MODE: u8 = 0xEF;

/// 构造进入 DFU 模式的 HID 命令包（64B，通过正常模式 report 0x0B 发送）。
///
/// 格式: `[REPORT_ID_OUTPUT=0x0B][CMD=0xEF][0x00 × 62]`
pub fn build_enter_dfu_hid_command() -> [u8; 64] {
    let mut cmd = [0u8; 64];
    cmd[0] = REPORT_ID_OUTPUT;
    cmd[1] = CMD_ENTER_HID_DFU_MODE;
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_dfu_command_layout() {
        let cmd = build_enter_dfu_hid_command();
        assert_eq!(cmd[0], REPORT_ID_OUTPUT);
        assert_eq!(cmd[1], CMD_ENTER_HID_DFU_MODE);
        assert!(cmd[2..].iter().all(|&b| b == 0));
        assert_eq!(cmd.len(), 64);
    }

    #[test]
    fn cancel_flag_is_checked() {
        // 纯结构性测试：构造客户端，确认 cancel_flag 默认 false
        let cancel = Arc::new(AtomicBool::new(false));
        let _client = DfuClient::new(cancel.clone(), Arc::new(|_| {}));
        assert!(!cancel.load(Ordering::SeqCst));
    }
}
