//! DFU 固件升级的进度类型。
//!
//! 采用与宿主应用一致的进度 schema，便于 CLI / server / web 复用同一套
//! phase/percent 语义。

use serde::{Deserialize, Serialize};

/// DFU 升级阶段（顺序流转，失败为终态）。
///
/// 序列：`EnteringDfu → Preparing → Transferring → Verifying → Rebooting → Completed`
/// 任一阶段失败转为 `Failed`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DfuPhase {
    /// 正在发 CMD 0xEF 进入 DFU 模式
    EnteringDfu,
    /// 已枚举到 DFU 设备，下发 PREPARE/START
    Preparing,
    /// DATA 包传输中（bytes_written 实时更新）
    Transferring,
    /// 传输完成，END 已下发，设备验证 Flash 中
    Verifying,
    /// 设备验证通过，正在重启回正常模式
    Rebooting,
    /// 升级完成，设备已回到正常模式（终态）
    Completed,
    /// 升级失败（终态；`message` 描述原因）
    Failed,
}

impl DfuPhase {
    /// 是否为活跃阶段（用于 server 拒绝并发升级）。
    pub fn is_active(self) -> bool {
        !matches!(self, Self::Completed | Self::Failed)
    }
}

/// 救砖结果（把卡在 DFU 模式的设备踢回正常模式的三态结论）。
///
/// 放在 `types` 而非 `recover`：`recover` 依赖 hidapi 仅 `usb` feature 编译，
/// 而本类型要能被 facade / CLI / 前端在任何 feature 组合下引用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryOutcome {
    /// 没发现 DFU 模式设备，无需恢复
    NotStuck,
    /// 已恢复：设备重新以正常 PID 枚举
    Recovered,
    /// 恢复序列已发出，但设备在超时内没回到正常模式（需物理重插）
    StillStuck,
}

/// DFU 升级进度事件，贯穿整条升级链路（kernel → runtime → facade → CLI/server → web）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DfuProgress {
    pub phase: DfuPhase,
    /// 已写入字节数
    pub bytes_written: u32,
    /// 固件总字节数
    pub total_bytes: u32,
    /// 0~100 整数百分比（total_bytes=0 时为 0）
    pub percent: u8,
    /// 人类可读消息（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl DfuProgress {
    pub fn new(
        phase: DfuPhase,
        bytes_written: u32,
        total_bytes: u32,
        message: Option<String>,
    ) -> Self {
        let percent = if total_bytes == 0 {
            0
        } else {
            ((bytes_written as u64 * 100 / total_bytes as u64).min(100)) as u8
        };
        Self {
            phase,
            bytes_written,
            total_bytes,
            percent,
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_is_zero_division_safe() {
        let p = DfuProgress::new(DfuPhase::Preparing, 0, 0, None);
        assert_eq!(p.percent, 0);
    }

    #[test]
    fn percent_caps_at_100() {
        let p = DfuProgress::new(DfuPhase::Transferring, 200, 100, None);
        assert_eq!(p.percent, 100);
    }

    #[test]
    fn percent_rounds_down() {
        let p = DfuProgress::new(DfuPhase::Transferring, 1, 3, None);
        assert_eq!(p.percent, 33);
    }

    #[test]
    fn phase_active_flag_distinguishes_terminal() {
        assert!(DfuPhase::Transferring.is_active());
        assert!(DfuPhase::EnteringDfu.is_active());
        assert!(!DfuPhase::Completed.is_active());
        assert!(!DfuPhase::Failed.is_active());
    }

    #[test]
    fn serde_phase_is_snake_case() {
        let json = serde_json::to_string(&DfuPhase::EnteringDfu).unwrap();
        assert_eq!(json, "\"entering_dfu\"");
    }
}
