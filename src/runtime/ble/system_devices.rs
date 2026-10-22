//! 系统蓝牙里已配对 / 已连接的 `REAI_VB_*` 设备，以及「被系统蓝牙占用」的诊断提示。
//!
//! ## 为什么需要
//!
//! btleplug 0.12 的 Windows（WinRT）后端只从广播监听里建设备表，
//! `add_peripheral` 直接返回 `NotSupported`。键盘一旦被 Windows 当作系统蓝牙键盘
//! 配对并连上（HID over GATT），连接期间就不再广播，SDK 的扫描永远看不到它，
//! 以前只会静默地「扫描未发现设备」。
//!
//! 这里用 WinRT 的设备枚举（`BluetoothLEDevice::GetDeviceSelectorFromPairingState` /
//! `GetDeviceSelectorFromConnectionStatus` + `DeviceInformation::FindAllAsync`）查出系统
//! 名下的同款键盘，配合 hidapi 的蓝牙总线 HID 条目，给出可操作的提示。
//!
//! 只读查询，不配对、不连接、不改系统状态。非 Windows 平台返回空列表，行为不变。

use crate::kernel::protocol_gatt::VENDOR_DEVICE_PREFIX;

/// 「设备被系统蓝牙占用」类错误消息的固定前缀，供消费方识别并引导用户。
pub const SYSTEM_BLUETOOTH_OCCUPIED_PREFIX: &str = "系统蓝牙占用：";

/// 系统蓝牙里登记的一台 `REAI_VB_*` 设备。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SystemBleDevice {
    /// 设备名（`REAI_VB_XXXXXXXX`）
    pub name: String,
    /// 蓝牙地址（冒号分隔大写）；系统没给出时为空
    pub address: String,
    /// 当前是否被系统连着
    pub connected: bool,
    /// 是否已与系统配对
    pub paired: bool,
}

/// 热插拔路径上查询系统蓝牙的默认总时限。
pub const DEFAULT_QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// 查询系统名下已配对或已连接的 `REAI_VB_*` 设备。
///
/// 整个查询受 `timeout` 约束（WinRT 的无线设备快照枚举可能很慢）；超时返回错误，
/// 后台的阻塞查询自行结束，不影响调用方继续。Windows 以外的平台直接返回空列表。
pub async fn list_system_vendor_devices(
    timeout: std::time::Duration,
) -> Result<Vec<SystemBleDevice>, String> {
    #[cfg(target_os = "windows")]
    {
        let query = tokio::task::spawn_blocking(windows_impl::list_blocking);
        match tokio::time::timeout(timeout, query).await {
            Ok(Ok(result)) => result.map_err(|e| format!("WinRT 设备枚举失败: {e}")),
            Ok(Err(e)) => Err(format!("WinRT 设备枚举线程异常: {e}")),
            Err(_) => Err(format!(
                "WinRT 设备枚举超时（{} 秒），跳过系统蓝牙检查",
                timeout.as_secs()
            )),
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = timeout;
        Ok(Vec::new())
    }
}

/// 根据系统蓝牙状态给出「被系统占用」的提示；没有证据时返回 `None`。
///
/// - `target`：要找的设备名；`None` 表示任意 `REAI_VB_*`。
/// - `system`：[`list_system_vendor_devices`] 的结果。
/// - `hid_over_bluetooth`：hidapi 是否枚举到蓝牙总线上的同款键盘 HID 条目
///   （它只说明「有一台同款键盘连着系统蓝牙」，不知道是哪一台）。
pub fn system_occupied_hint(
    target: Option<&str>,
    system: &[SystemBleDevice],
    hid_over_bluetooth: bool,
) -> Option<String> {
    let matches = |d: &&SystemBleDevice| match target {
        Some(t) => d.name == t,
        None => d.name.starts_with(VENDOR_DEVICE_PREFIX),
    };
    let remove_hint =
        "请到「设置 → 蓝牙和其他设备」里删除该设备后重试（删除后由本软件直接连接，不需要系统配对）";
    if let Some(d) = system.iter().filter(matches).find(|d| d.connected) {
        return Some(format!(
            "{SYSTEM_BLUETOOTH_OCCUPIED_PREFIX}键盘 {} 正被 Windows 系统蓝牙连接（系统蓝牙键盘），连接期间不再广播，SDK 扫描不到。{remove_hint}",
            d.name
        ));
    }
    if let Some(d) = system.iter().filter(matches).find(|d| d.paired) {
        return Some(format!(
            "{SYSTEM_BLUETOOTH_OCCUPIED_PREFIX}键盘 {} 已在 Windows 系统蓝牙中配对，键盘开机后会被系统抢先连上，SDK 扫描不到。{remove_hint}",
            d.name
        ));
    }
    if hid_over_bluetooth {
        return Some(format!(
            "{SYSTEM_BLUETOOTH_OCCUPIED_PREFIX}检测到同款键盘正通过系统蓝牙（HID）连接，连接期间不再广播，SDK 扫描不到。{remove_hint}"
        ));
    }
    None
}

/// 把 48 位蓝牙地址格式化为 `AA:BB:CC:DD:EE:FF`。
pub fn format_bluetooth_address(raw: u64) -> String {
    (0..6)
        .rev()
        .map(|i| format!("{:02X}", (raw >> (i * 8)) & 0xFF))
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::{format_bluetooth_address, SystemBleDevice};
    use crate::kernel::protocol_gatt::VENDOR_DEVICE_PREFIX;
    use windows::Devices::Bluetooth::{BluetoothConnectionStatus, BluetoothLEDevice};
    use windows::Devices::Enumeration::DeviceInformation;

    /// 阻塞查询（在 `spawn_blocking` 线程里调用；windows-rs 会按需进入 MTA）。
    pub(super) fn list_blocking() -> windows::core::Result<Vec<SystemBleDevice>> {
        let selectors = [
            BluetoothLEDevice::GetDeviceSelectorFromPairingState(true)?,
            BluetoothLEDevice::GetDeviceSelectorFromConnectionStatus(
                BluetoothConnectionStatus::Connected,
            )?,
        ];
        let mut seen_ids: Vec<String> = Vec::new();
        let mut out: Vec<SystemBleDevice> = Vec::new();
        for selector in selectors {
            let infos = DeviceInformation::FindAllAsyncAqsFilter(&selector)?.join()?;
            for index in 0..infos.Size()? {
                let info = infos.GetAt(index)?;
                let id = info.Id()?;
                let id_string = id.to_string();
                if seen_ids.contains(&id_string) {
                    continue;
                }
                seen_ids.push(id_string);

                let info_name = info.Name().map(|n| n.to_string()).unwrap_or_default();
                let paired = info.Pairing().and_then(|p| p.IsPaired()).unwrap_or(false);
                let (le_name, address, connected) =
                    match BluetoothLEDevice::FromIdAsync(&id).and_then(|op| op.join()) {
                        Ok(device) => (
                            device.Name().map(|n| n.to_string()).unwrap_or_default(),
                            device
                                .BluetoothAddress()
                                .map(format_bluetooth_address)
                                .unwrap_or_default(),
                            device
                                .ConnectionStatus()
                                .map(|s| s == BluetoothConnectionStatus::Connected)
                                .unwrap_or(false),
                        ),
                        Err(e) => {
                            log::debug!(target: "gatt", "BluetoothLEDevice::FromIdAsync 失败: {e}");
                            (String::new(), String::new(), false)
                        }
                    };
                let Some(name) = [info_name, le_name]
                    .into_iter()
                    .find(|n| n.starts_with(VENDOR_DEVICE_PREFIX))
                else {
                    continue;
                };
                out.push(SystemBleDevice {
                    name,
                    address,
                    connected,
                    paired,
                });
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(name: &str, connected: bool, paired: bool) -> SystemBleDevice {
        SystemBleDevice {
            name: name.into(),
            address: String::new(),
            connected,
            paired,
        }
    }

    #[test]
    fn connected_target_gets_the_strongest_hint() {
        let system = [
            dev("REAI_VB_11111111", false, true),
            dev("REAI_VB_22222222", true, true),
        ];
        let hint = system_occupied_hint(Some("REAI_VB_22222222"), &system, false).unwrap();
        assert!(hint.starts_with(SYSTEM_BLUETOOTH_OCCUPIED_PREFIX));
        assert!(hint.contains("REAI_VB_22222222"));
        assert!(hint.contains("正被 Windows 系统蓝牙连接"));
    }

    #[test]
    fn paired_only_target_is_reported_as_paired() {
        let system = [dev("REAI_VB_11111111", false, true)];
        let hint = system_occupied_hint(Some("REAI_VB_11111111"), &system, false).unwrap();
        assert!(hint.contains("已在 Windows 系统蓝牙中配对"));
    }

    #[test]
    fn other_boards_do_not_explain_a_missing_target() {
        let system = [dev("REAI_VB_11111111", true, true)];
        assert_eq!(
            system_occupied_hint(Some("REAI_VB_99999999"), &system, false),
            None
        );
        // 无目标时任意同款都算
        assert!(system_occupied_hint(None, &system, false).is_some());
    }

    #[test]
    fn hid_over_bluetooth_is_a_generic_fallback() {
        let hint = system_occupied_hint(Some("REAI_VB_99999999"), &[], true).unwrap();
        assert!(hint.contains("同款键盘"));
        assert_eq!(system_occupied_hint(None, &[], false), None);
    }

    #[test]
    fn bluetooth_address_is_formatted_big_endian() {
        assert_eq!(
            format_bluetooth_address(0x0000_A1B2_C3D4_E5F6),
            "A1:B2:C3:D4:E5:F6"
        );
    }
}
