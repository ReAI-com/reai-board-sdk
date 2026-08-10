//! DFU 救砖 —— 把卡在 DFU 模式（PID 0xFF06）的设备踢回正常模式。
//!
//! ## 为什么不能只发一个 END
//!
//! 2026-07-25 真机实测：固件在**没收到 PREPARE** 的情况下完全忽略 END 包 ——
//! 既不响应也不重启。早期的"复位"路径都只发裸 END，因此从未真正生效，
//! 用户一旦卡在 DFU 就只能物理重插甚至返修。
//!
//! ## 恢复原理
//!
//! ```text
//! PREPARE(声明非 0 长度)  → 固件状态机进入传输态，回 result=0x00
//! END                     → 固件校验：已写入 0 字节 ≠ 声明长度 → result=0xFF
//!                         → 丢弃 FOTA 暂存分区 → 带原固件重启 → PID 回 0xED20
//! ```
//!
//! 全程只碰 `PARTITION_FOTA_DATA` 暂存分区，不触碰主应用分区，因此不存在变砖风险。
//!
//! ## 线程约束
//!
//! 本模块所有函数都是**同步阻塞**的 hidapi 调用，必须由 runtime 层通过
//! `spawn_blocking_with_runloop` 投递到 macOS HID 专用线程执行。且**每个函数
//! 都刻意只做一件短事**（扫描 / 收发），等待重启的轮询由 runtime 在 async 层
//! 分段驱动 —— 若整段占用 HID 线程做 20 秒轮询，会把热插拔探测一起饿死。

use anyhow::Result;

use super::client::{open_dfu_device, read_response, NORMAL_PID, VID};
use super::protocol::*;

/// 救砖时 PREPARE 声明的固件长度。
///
/// **必须非 0**。恢复的原理正是「已写入 0 字节 ≠ 声明长度」让 END 校验失败；
/// 若声明 0，`total_written == declared` 可能被固件判为传输完整，进而尝试应用
/// 一个空的暂存分区 —— 那比卡在 DFU 严重得多。
///
/// 取值 `0x40000`（256KB，接近真实固件体积）。真机实测固件对该值秒回
/// `result=0x00`，不存在"按声明长度预擦除"导致的长耗时。
///
/// ⚠️ 若集成方另行实现了一套 DFU 恢复逻辑（不共享本模块代码），
/// **两边的数值必须保持一致**，否则会出现"一个环境能救回、另一个救不回"的分裂。
pub const RECOVERY_DECLARED_LEN: u32 = 0x40000;

/// 发出恢复序列后，等待设备重新以正常 PID 枚举的上限（秒）。
///
/// 比升级路径的 15 秒略宽：救砖时设备状态本就异常，多给几秒容错；
/// 超时也不代表失败，只是需要用户物理重插一次 USB。
pub const RECOVERY_WAIT_TIMEOUT_SECS: u64 = 20;

/// 等待重启期间的轮询间隔（毫秒）。
pub const RECOVERY_POLL_INTERVAL_MS: u64 = 500;

/// 扫描当前是否存在 DFU 模式设备（VID=0x363C, PID=0xFF06）。
///
/// **须在 HID 专用线程调用。**
pub fn scan_stuck_device() -> Result<bool> {
    let api = hidapi::HidApi::new().map_err(|e| anyhow::anyhow!("HidApi 创建失败: {e}"))?;
    let found = api
        .device_list()
        .any(|d| d.vendor_id() == VID && d.product_id() == DFU_PID);
    Ok(found)
}

/// 扫描设备是否已回到正常模式（PID=0xED20）。
///
/// **须在 HID 专用线程调用。**
pub fn scan_normal_device() -> Result<bool> {
    let api = hidapi::HidApi::new().map_err(|e| anyhow::anyhow!("HidApi 创建失败: {e}"))?;
    let found = api
        .device_list()
        .any(|d| d.vendor_id() == VID && d.product_id() == NORMAL_PID);
    Ok(found)
}

/// 发送恢复序列 PREPARE → END。只做收发，**不等待重启**（由调用方分段轮询）。
///
/// 返回**是否至少有一个包成功写入设备**。调用方据此决定要不要进入等待：
/// 两个包都没写出去（设备已拔线等）时再等 20 秒毫无意义，只会拖慢错误反馈
/// 并延长 DFU 忙位的持有时间。
///
/// ## 尽力复位语义
///
/// PREPARE 失败（超时 / 抖包）时**仍然照发 END**。救砖本就是救援路径，
/// 任何中间步骤的失败都不该成为放弃的理由 —— 多发一个 END 没有副作用，
/// 而少发一个可能让用户白白损失一次恢复机会。
///
/// ## 为什么用 write 成功而不是「收到响应」判断送达
///
/// 读不到响应**不代表包没送到**：固件常在 END 后立即重启，来不及回包 ——
/// 那恰恰是恢复成功的征兆。只有 write 本身失败才说明包没出去。
/// 最终是否恢复成功，一律以设备能否重新以正常 PID 枚举为准。
///
/// **须在 HID 专用线程调用。**
pub fn send_recovery_sequence() -> Result<bool> {
    let device = open_dfu_device()?;
    Ok(send_recovery_sequence_on(&device))
}

/// 在**已打开**的 DFU 设备上执行复位序列。
///
/// 与 [`send_recovery_sequence`] 的唯一区别是不自己开设备 —— 供升级流程的失败
/// 路径复用（那里手上已经有打开的句柄，重复开同一设备没必要）。
/// 两条路径共用这一份实现，避免「一边修好了另一边还是裸 END」的分裂。
pub(crate) fn send_recovery_sequence_on(device: &hidapi::HidDevice) -> bool {
    let mut delivered = false;

    match write_then_read(
        device,
        &DfuPacketEncoder::prepare(RECOVERY_DECLARED_LEN),
        DFU_PREPARE_TIMEOUT_MS,
    ) {
        WriteOutcome::Answered(resp) => {
            delivered = true;
            log::info!(target: "board", "[dfu-recover] PREPARE 已应答 result=0x{:02X}", resp.result);
        }
        WriteOutcome::SentNoAnswer(e) => {
            delivered = true;
            log::warn!(target: "board", "[dfu-recover] PREPARE 已发出但无应答: {e}（仍继续发 END）");
        }
        // 不 return —— 尽力复位，END 还有机会。
        WriteOutcome::WriteFailed(e) => {
            log::warn!(target: "board", "[dfu-recover] PREPARE 发送失败: {e}（仍继续发 END）");
        }
    }

    match write_then_read(device, &DfuPacketEncoder::end(), DFU_END_TIMEOUT_MS) {
        // result 非 0 是**预期结果**：已写入 0 字节，校验必然不通过，
        // 固件据此丢弃暂存分区并重启回旧固件 —— 这正是我们要的。
        WriteOutcome::Answered(resp) => {
            delivered = true;
            log::info!(
                target: "board",
                "[dfu-recover] END 已应答 result=0x{:02X} written={}（非 0 属预期）",
                resp.result, resp.total_written
            );
        }
        WriteOutcome::SentNoAnswer(e) => {
            delivered = true;
            log::info!(target: "board", "[dfu-recover] END 已发出但无应答: {e}（设备多半已在重启，属正常）");
        }
        WriteOutcome::WriteFailed(e) => {
            log::warn!(target: "board", "[dfu-recover] END 发送失败: {e}");
        }
    }

    if !delivered {
        log::warn!(target: "board", "[dfu-recover] 两个恢复包都未能写入设备（可能已拔线）");
    }
    delivered
}

/// 一个恢复包的投递结果 —— 区分「没发出去」和「发出去了但没回话」。
///
/// 这个区分是必要的：后者在 END 上是**正常且期待的**（设备重启来不及回包），
/// 前者才意味着这次恢复尝试白费。
enum WriteOutcome {
    Answered(DfuResponse),
    SentNoAnswer(anyhow::Error),
    WriteFailed(anyhow::Error),
}

fn write_then_read(device: &hidapi::HidDevice, packet: &[u8], timeout_ms: i32) -> WriteOutcome {
    if let Err(e) = device.write(packet) {
        return WriteOutcome::WriteFailed(anyhow::anyhow!("DFU write 失败: {e}"));
    }
    match read_response(device, timeout_ms) {
        Ok(resp) => WriteOutcome::Answered(resp),
        Err(e) => WriteOutcome::SentNoAnswer(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 恢复原理的地基：声明长度必须非 0，否则 END 可能被判为「传输完整」。
    #[test]
    fn declared_len_must_be_nonzero() {
        assert_ne!(RECOVERY_DECLARED_LEN, 0);
    }

    /// 锚定常量数值 —— 集成方若另有独立实现，两边必须一致，
    /// 否则恢复行为会在不同宿主应用之间分裂。
    #[test]
    fn declared_len_matches_cross_impl_contract() {
        assert_eq!(RECOVERY_DECLARED_LEN, 0x40000);
    }

    /// 恢复序列的第一个包必须是 PREPARE 且带非 0 长度 —— 裸 END 对固件无效。
    #[test]
    fn recovery_prepare_packet_declares_nonzero_length() {
        let packet = DfuPacketEncoder::prepare(RECOVERY_DECLARED_LEN);
        assert_eq!(packet[0], DFU_REPORT_ID_OUTPUT);
        assert_eq!(packet[1], FLAG_PREPARE);
        let declared = u32::from_le_bytes([packet[2], packet[3], packet[4], packet[5]]);
        assert_eq!(declared, RECOVERY_DECLARED_LEN);
        assert_ne!(declared, 0);
    }

    /// PREPARE 的校验和覆盖的是声明长度本身的字节，写错设备会拒收。
    #[test]
    fn recovery_prepare_packet_checksum_covers_declared_length() {
        let packet = DfuPacketEncoder::prepare(RECOVERY_DECLARED_LEN);
        let checksum = u16::from_le_bytes([packet[6], packet[7]]);
        assert_eq!(
            checksum,
            compute_checksum(&RECOVERY_DECLARED_LEN.to_le_bytes())
        );
    }

    /// 收尾包必须是 END —— 它才是触发校验失败与重启的那一下。
    #[test]
    fn recovery_end_packet_is_flag_end() {
        let packet = DfuPacketEncoder::end();
        assert_eq!(packet[0], DFU_REPORT_ID_OUTPUT);
        assert_eq!(packet[1], FLAG_END);
    }
}
