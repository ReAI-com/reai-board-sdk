//! ReAI-Vibe-Board 一键连接诊断（主要给 Windows 用，macOS / Linux 也能跑）。
//!
//! 双击运行、无需参数。依次检查：
//! 1. 系统版本、SDK 版本；
//! 2. hidapi 枚举到的全部 VID 0x363C 条目（路径里的用户名已打码）；
//! 3. 每个 USB Config（0xFFA0）候选：按 SDK 修复后的逻辑发 CMD 0x13 读设备信息；
//! 4. BLE：适配器状态、扫描 10 秒看到的 `REAI_VB_*` 广播，以及（Windows）
//!    系统已配对 / 已连接的 `REAI_VB_*` 设备；
//! 5. 一行结论。
//!
//! 结果同时打印到窗口并写入 `reai-board-diag-YYYYMMDD-HHMMSS.txt`
//! （当前目录，写不了就放桌面），结束时等待回车，避免窗口一闪而过。
//!
//! ```sh
//! cargo run --release --example windows_diag
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use btleplug::api::{Central as _, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::Manager;
use hidapi::{BusType, HidApi};
use reai_board_sdk::runtime::ble::gatt_client::match_vendor_name;
use reai_board_sdk::runtime::ble::system_devices::{self, SystemBleDevice};
use reai_board_sdk::runtime::usb::probe::{
    config_candidates, probe_device_info, ProbeReport, ProbeTiming, SystemClock,
};

const VID: u16 = 0x363C;
const BLE_SCAN: Duration = Duration::from_secs(10);

/// 同时写屏幕与报告的收集器。
#[derive(Default)]
struct Report {
    text: String,
}

impl Report {
    fn line(&mut self, line: impl AsRef<str>) {
        let line = redact(line.as_ref());
        println!("{line}");
        self.text.push_str(&line);
        self.text.push_str("\r\n");
    }

    fn section(&mut self, title: &str) {
        self.line("");
        self.line(format!("== {title} =="));
    }
}

/// 把用户主目录替换为 `~`、路径里的用户名目录替换为 `<user>`，避免报告外传时泄露个人信息。
///
/// 只替换路径形态（`\Users\<名>`、`/Users/<名>`、`/home/<名>`），不替换裸用户名：
/// 用户名可能恰好是 `reai` 之类的常见词，裸替换会把正文里的设备名也打码。
fn redact(input: &str) -> String {
    let mut out = input.to_string();
    for key in ["USERPROFILE", "HOME"] {
        if let Ok(home) = std::env::var(key) {
            if home.len() > 3 {
                out = replace_case_insensitive(&out, &home, "~");
            }
        }
    }
    for key in ["USERNAME", "USER"] {
        if let Ok(user) = std::env::var(key) {
            if user.is_empty() {
                continue;
            }
            for prefix in ["\\Users\\", "/Users/", "/home/"] {
                out = replace_case_insensitive(
                    &out,
                    &format!("{prefix}{user}"),
                    &format!("{prefix}<user>"),
                );
            }
        }
    }
    out
}

fn replace_case_insensitive(haystack: &str, needle: &str, with: &str) -> String {
    let lower_h = haystack.to_lowercase();
    let lower_n = needle.to_lowercase();
    // 只在大小写折叠不改变字节长度时做不区分大小写替换，否则退回精确替换。
    if lower_h.len() != haystack.len() || lower_n.len() != needle.len() {
        return haystack.replace(needle, with);
    }
    let mut out = String::with_capacity(haystack.len());
    let mut rest = 0;
    while let Some(pos) = lower_h[rest..].find(&lower_n) {
        let start = rest + pos;
        out.push_str(&haystack[rest..start]);
        out.push_str(with);
        rest = start + needle.len();
    }
    out.push_str(&haystack[rest..]);
    out
}

/// 系统版本描述。
fn os_version() -> String {
    #[cfg(target_os = "windows")]
    {
        let ver = run_capture("cmd", &["/C", "ver"]).unwrap_or_default();
        // "Microsoft Windows [Version 10.0.22631.4317]"
        let full = ver
            .split(['[', ']'])
            .find_map(|part| {
                part.split_whitespace()
                    .find(|t| t.chars().next().is_some_and(|c| c.is_ascii_digit()))
            })
            .unwrap_or("")
            .to_string();
        let build: u32 = full
            .split('.')
            .nth(2)
            .and_then(|b| b.parse().ok())
            .unwrap_or(0);
        // 注册表 ProductName 在 Win11 上仍写 "Windows 10"，以内部版本号判断。
        let product = if build >= 22000 {
            "Windows 11"
        } else if build > 0 {
            "Windows 10"
        } else {
            "Windows（版本未知）"
        };
        let display = registry_value("DisplayVersion")
            .or_else(|| registry_value("ReleaseId"))
            .unwrap_or_else(|| "?".into());
        let arch = std::env::var("PROCESSOR_ARCHITECTURE").unwrap_or_default();
        format!("{product} {display}（内部版本 {full}，{arch}）")
    }
    #[cfg(target_os = "macos")]
    {
        let name = run_capture("sw_vers", &["-productVersion"]).unwrap_or_default();
        format!("macOS {}（{}）", name.trim(), std::env::consts::ARCH)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
    }
}

#[cfg(target_os = "windows")]
fn registry_value(name: &str) -> Option<String> {
    let out = run_capture(
        "reg",
        &[
            "query",
            r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion",
            "/v",
            name,
        ],
    )?;
    out.lines()
        .find(|l| l.contains(name) && l.contains("REG_"))
        .and_then(|l| l.split_whitespace().last())
        .map(str::to_string)
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn run_capture(program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn bus_name(bus: BusType) -> &'static str {
    match bus {
        BusType::Usb => "USB",
        BusType::Bluetooth => "Bluetooth",
        BusType::I2c => "I2C",
        BusType::Spi => "SPI",
        BusType::Unknown => "Unknown",
    }
}

/// USB 一节的结论素材。
#[derive(Default)]
struct UsbFacts {
    hid_entries: usize,
    hid_over_bluetooth: usize,
    config_candidates: usize,
    chip_id: Option<String>,
    hid_error: Option<String>,
}

/// BLE 一节的结论素材。
#[derive(Default)]
struct BleFacts {
    adapter_error: Option<String>,
    adverts: BTreeMap<String, Option<i16>>,
    system: Vec<SystemBleDevice>,
}

fn diagnose_usb(report: &mut Report) -> UsbFacts {
    let mut facts = UsbFacts::default();
    report.section("USB / HID 枚举（VID 0x363C）");
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            report.line(format!("hidapi 初始化失败: {e}"));
            facts.hid_error = Some(e.to_string());
            return facts;
        }
    };
    for d in api.device_list().filter(|d| d.vendor_id() == VID) {
        facts.hid_entries += 1;
        if matches!(d.bus_type(), BusType::Bluetooth) {
            facts.hid_over_bluetooth += 1;
        }
        report.line(format!(
            "- PID=0x{:04X} usage_page=0x{:04X} usage=0x{:04X} interface={} bus={} product={:?}",
            d.product_id(),
            d.usage_page(),
            d.usage(),
            d.interface_number(),
            bus_name(d.bus_type()),
            d.product_string().unwrap_or("")
        ));
        report.line(format!("  path={}", d.path().to_string_lossy()));
    }
    if facts.hid_entries == 0 {
        report.line("（没有任何 VID 0x363C 的 HID 条目）");
    }

    report.section("USB 设备信息探测（CMD 0x13：等 800ms，每次 1.5s，最多 6 次）");
    let candidates = config_candidates(&api);
    facts.config_candidates = candidates.len();
    if candidates.is_empty() {
        report.line("没有 USB 总线上的 Config(0xFFA0) 候选（蓝牙总线条目已排除）。");
    }
    for (i, candidate) in candidates.iter().enumerate() {
        report.line(format!(
            "候选 {}: usage=0x{:04X} interface={} bus={}",
            i + 1,
            candidate.usage,
            candidate.interface_number,
            bus_name(candidate.bus_type)
        ));
        match api.open_path(&candidate.path) {
            Ok(device) => {
                let probe: ProbeReport =
                    probe_device_info(&device, ProbeTiming::CONNECT, &SystemClock);
                report.line(format!("  尝试记录: {}", probe.summary()));
                match probe.info {
                    Some(info) => {
                        report.line(format!(
                            "  结果: 成功 chip_id={} 固件={} 接收器={} 电量={}% 充电={} MAC={} mode={}",
                            info.chip_id,
                            info.firmware_version,
                            info.receiver_version,
                            info.battery_level,
                            info.battery_charging,
                            info.mac_address,
                            info.mode
                        ));
                        report.line(format!("  对应 BLE 名: REAI_VB_{}", info.chip_id));
                        facts.chip_id.get_or_insert(info.chip_id);
                    }
                    None => report.line("  结果: 失败（未读到有效应答）"),
                }
            }
            Err(e) => report.line(format!("  打开失败: {e}")),
        }
    }
    facts
}

async fn diagnose_ble(report: &mut Report) -> BleFacts {
    let mut facts = BleFacts::default();
    report.section("BLE 适配器");
    let adapter = match Manager::new().await {
        Ok(manager) => match manager.adapters().await {
            Ok(list) => list.into_iter().next(),
            Err(e) => {
                facts.adapter_error = Some(format!("枚举适配器失败: {e}"));
                None
            }
        },
        Err(e) => {
            facts.adapter_error = Some(format!("蓝牙管理器初始化失败: {e}"));
            None
        }
    };
    match &adapter {
        Some(adapter) => {
            let state = adapter
                .adapter_state()
                .await
                .map(|s| format!("{s:?}"))
                .unwrap_or_else(|e| format!("未知（{e}）"));
            let info = adapter.adapter_info().await.unwrap_or_default();
            report.line(format!("适配器: {info}，状态: {state}"));
            if state.contains("PoweredOff") {
                facts.adapter_error = Some("蓝牙已关闭".into());
            }
        }
        None => {
            facts
                .adapter_error
                .get_or_insert_with(|| "没有可用的蓝牙适配器".into());
            report.line(format!(
                "不可用: {}",
                facts.adapter_error.as_deref().unwrap_or("")
            ));
        }
    }

    if let Some(adapter) = &adapter {
        report.section(&format!("BLE 广播扫描（{} 秒）", BLE_SCAN.as_secs()));
        match adapter.start_scan(ScanFilter::default()).await {
            Ok(()) => {
                let deadline = tokio::time::Instant::now() + BLE_SCAN;
                let mut total = std::collections::BTreeSet::new();
                while tokio::time::Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    let Ok(list) = adapter.peripherals().await else {
                        continue;
                    };
                    for p in list {
                        total.insert(format!("{:?}", p.id()));
                        let Ok(Some(props)) = p.properties().await else {
                            continue;
                        };
                        if let Some(name) = match_vendor_name(
                            props.local_name.as_deref(),
                            props.advertisement_name.as_deref(),
                            None,
                        ) {
                            let entry = facts.adverts.entry(name).or_insert(None);
                            if props.rssi.is_some() {
                                *entry = props.rssi;
                            }
                        }
                    }
                }
                let _ = adapter.stop_scan().await;
                report.line(format!("共看到 {} 个蓝牙设备的广播。", total.len()));
                if facts.adverts.is_empty() {
                    report.line("没有看到 REAI_VB_* 广播。");
                }
                for (name, rssi) in &facts.adverts {
                    let rssi = rssi
                        .map(|r| format!("{r} dBm"))
                        .unwrap_or_else(|| "?".into());
                    report.line(format!("- {name}  RSSI={rssi}"));
                }
            }
            Err(e) => {
                report.line(format!("启动扫描失败: {e}"));
                facts.adapter_error = Some(format!("启动扫描失败: {e}"));
            }
        }
    }

    report.section("系统蓝牙中已配对 / 已连接的 REAI_VB_*");
    if cfg!(target_os = "windows") {
        match system_devices::list_system_vendor_devices(Duration::from_secs(15)).await {
            Ok(list) => {
                if list.is_empty() {
                    report.line("没有（正常：本软件不需要系统配对）。");
                }
                for d in &list {
                    report.line(format!(
                        "- {}  地址={}  已配对={}  已连接={}",
                        d.name, d.address, d.paired, d.connected
                    ));
                }
                facts.system = list;
            }
            Err(e) => report.line(format!("查询失败: {e}")),
        }
    } else {
        report.line("（仅 Windows 检查）");
    }
    facts
}

/// 根据收集到的事实给出一行结论。
fn conclude(usb: &UsbFacts, ble: &BleFacts) -> String {
    let mut parts = Vec::new();

    if let Some(e) = &usb.hid_error {
        parts.push(format!("HID 不可用（{e}）"));
    } else if let Some(chip) = &usb.chip_id {
        parts.push(format!("USB 正常（chip_id={chip}）"));
    } else if usb.config_candidates > 0 {
        parts
            .push("USB 已识别但读不到设备信息（请拔插 USB 重试，仍失败请把本报告发给我们）".into());
    } else {
        parts.push("USB 未连接".into());
    }

    let expected = usb.chip_id.as_ref().map(|c| format!("REAI_VB_{c}"));
    let system_hit = |pred: fn(&SystemBleDevice) -> bool| {
        ble.system
            .iter()
            .filter(|d| expected.as_ref().is_none_or(|e| &d.name == e))
            .find(|d| pred(d))
            .map(|d| d.name.clone())
    };

    let target_advertising = expected
        .as_ref()
        .is_some_and(|e| ble.adverts.contains_key(e));

    if let Some(e) = &ble.adapter_error {
        parts.push(format!("蓝牙不可用（{e}）"));
    } else if target_advertising {
        // 目标在广播就说明没被系统占住；系统配对记录只作提示。
        let name = expected.as_deref().unwrap_or_default();
        let paired_note = if system_hit(|d| d.paired || d.connected).is_some() {
            "，但系统蓝牙里也有它的配对记录，建议删除"
        } else {
            ""
        };
        parts.push(format!("BLE 广播正常（看到 {name}{paired_note}）"));
    } else if let Some(name) = system_hit(|d| d.connected) {
        parts.push(format!(
            "键盘 {name} 已被系统蓝牙连接且未广播（请到系统蓝牙设置删除该设备）"
        ));
    } else if let Some(name) = system_hit(|d| d.paired) {
        parts.push(format!(
            "键盘 {name} 已在系统蓝牙中配对（请到系统蓝牙设置删除该设备）"
        ));
    } else if usb.hid_over_bluetooth > 0 && ble.adverts.is_empty() {
        parts.push("同款键盘正通过系统蓝牙连接、未广播（请到系统蓝牙设置删除该设备）".into());
    } else if ble.adverts.is_empty() {
        parts.push("没扫到 REAI_VB_* 广播（键盘未开机 / 未切到蓝牙 / 距离太远）".into());
    } else if let Some(e) = &expected {
        if ble.adverts.contains_key(e) {
            parts.push(format!("BLE 广播正常（看到 {e}）"));
        } else {
            parts.push(format!(
                "看到 {} 个 REAI_VB_* 广播，但没有本机 {e}",
                ble.adverts.len()
            ));
        }
    } else {
        parts.push(format!(
            "BLE 广播正常（看到 {} 个 REAI_VB_*）",
            ble.adverts.len()
        ));
    }
    parts.join("；")
}

/// 报告文件位置：当前目录 → 桌面 → 临时目录。
fn write_report(text: &str, file_name: &str) -> Result<PathBuf, String> {
    let mut dirs = Vec::new();
    if let Ok(dir) = std::env::current_dir() {
        dirs.push(dir);
    }
    for key in ["USERPROFILE", "HOME"] {
        if let Ok(home) = std::env::var(key) {
            dirs.push(PathBuf::from(home).join("Desktop"));
        }
    }
    dirs.push(std::env::temp_dir());
    let mut bytes = Vec::with_capacity(text.len() + 3);
    bytes.extend_from_slice(b"\xEF\xBB\xBF"); // UTF-8 BOM：老版记事本也能正确显示中文
    bytes.extend_from_slice(text.as_bytes());
    let mut errors = Vec::new();
    for dir in dirs {
        let path = dir.join(file_name);
        match std::fs::write(&path, &bytes) {
            Ok(()) => return Ok(path),
            Err(e) => errors.push(format!("{}: {e}", redact(&path.to_string_lossy()))),
        }
    }
    Err(errors.join("; "))
}

fn main() {
    let now = chrono::Local::now();
    let mut report = Report::default();
    report.line("ReAI-Vibe-Board 连接诊断");
    report.line(format!("时间: {}", now.format("%Y-%m-%d %H:%M:%S %:z")));
    report.line(format!("SDK 版本: {}", env!("CARGO_PKG_VERSION")));
    report.line(format!("系统: {}", os_version()));
    println!();
    println!("正在检查，大约需要 20～40 秒，请保持键盘开机……");

    // HID 放在主线程做（不进 tokio），BLE 再起运行时。
    let usb = diagnose_usb(&mut report);

    let ble = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => {
            let facts = rt.block_on(diagnose_ble(&mut report));
            // 超时放弃的 WinRT 查询可能还在后台阻塞线程里跑；不等它，直接收尾。
            rt.shutdown_background();
            facts
        }
        Err(e) => BleFacts {
            adapter_error: Some(format!("tokio 运行时创建失败: {e}")),
            ..Default::default()
        },
    };

    report.section("结论");
    report.line(conclude(&usb, &ble));

    let file_name = format!("reai-board-diag-{}.txt", now.format("%Y%m%d-%H%M%S"));
    println!();
    match write_report(&report.text, &file_name) {
        Ok(path) => println!("报告已保存: {}", redact(&path.to_string_lossy())),
        Err(e) => println!("报告保存失败: {e}"),
    }
    println!("按回车退出……");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}
