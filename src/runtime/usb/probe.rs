//! USB Config 接口挑选与 CMD 0x13 设备信息探测。
//!
//! 这里的节奏与判据来自出厂测试工具 webTest 在 Windows 上踩过的坑（2026-07）：
//!
//! - **选接口**：同一 VID/PID（0x363C:0xED20）在 Windows 上既会枚举出 USB 的 HID
//!   集合，也会枚举出系统已配对的蓝牙 HID 集合，二者 usage_page 都可能是 0xFFA0。
//!   只认 USB 总线（或 hidapi 判不出总线的 Unknown 兜底），按 usage==0x0002 优先排序，
//!   逐个打开验证，失败换下一个。
//! - **探测节奏**：复合 HID 设备冷插入时，系统还在给兄弟接口做 SET_IDLE/SET_PROTOCOL，
//!   Config 接口会静默丢掉最早的命令。打开后先等 800ms，再发 0x13；每次等 1.5s，
//!   最多 6 次（约 10s）。
//! - **校验**：只认 report id=0x0A、cmd=0x13、result=0x00 且 payload 完整的应答；
//!   全零 chip_id 视为无效。读不到就明确失败，绝不拿未校验的缓冲区去解析——
//!   否则会得到 chip_id=00000000 的假 DeviceInfo，进而把 BLE 目标设成一个永远
//!   扫不到的 `REAI_VB_00000000`。

use std::ffi::CString;
use std::time::{Duration, Instant};

use hidapi::{BusType, HidApi, HidDevice};

use crate::kernel::event::DeviceInfo;
use crate::kernel::protocol_hid::{
    is_target_pid, HidPacket, CMD_GET_DEVICE_INFO, REPORT_ID_INPUT, USAGE_PAGE_CONFIG, VID,
};
use crate::kernel::types::ConnectionType;

/// Config 集合里承载命令的 usage（0xFFA0:0x0002）。
pub const USAGE_CONFIG_COMMAND: u16 = 0x0002;

/// USB 应答里 payload 的起始偏移：`[ReportID][CMD][LEN][result][payload...]`。
const USB_PAYLOAD_OFFSET: usize = 4;
/// 设备信息 payload 的最小长度（见 `tool::parse::parse_device_info_from_buf`）。
const DEVICE_INFO_PAYLOAD_LEN: usize = 20;

// ============ 接口挑选（纯逻辑） ============

/// 参与排序的 HID 枚举条目元数据。
#[derive(Debug, Clone, Copy)]
pub struct CandidateMeta {
    pub vendor_id: u16,
    pub product_id: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub bus_type: BusType,
}

/// 给一个枚举条目打分：`None` = 不是 USB Config 候选；数字越小越优先。
///
/// - 蓝牙 / I2C / SPI 总线一律排除——那是系统蓝牙连着的同款键盘，
///   不是这根 USB 线；
/// - usage==0x0002 优先（命令集合），其次 USB 总线优先于 Unknown。
pub fn config_candidate_rank(meta: &CandidateMeta) -> Option<u8> {
    if meta.vendor_id != VID
        || !is_target_pid(meta.product_id)
        || meta.usage_page != USAGE_PAGE_CONFIG
    {
        return None;
    }
    let bus_rank = match meta.bus_type {
        BusType::Usb => 0,
        BusType::Unknown => 1,
        BusType::Bluetooth | BusType::I2c | BusType::Spi => return None,
    };
    let usage_rank = if meta.usage == USAGE_CONFIG_COMMAND {
        0
    } else {
        2
    };
    Some(usage_rank + bus_rank)
}

/// 把条目按 [`config_candidate_rank`] 过滤并稳定排序，返回原下标。
pub fn rank_config_candidates(metas: &[CandidateMeta]) -> Vec<usize> {
    let mut ranked: Vec<(u8, usize)> = metas
        .iter()
        .enumerate()
        .filter_map(|(i, m)| config_candidate_rank(m).map(|r| (r, i)))
        .collect();
    ranked.sort_by_key(|&(rank, index)| (rank, index));
    ranked.into_iter().map(|(_, i)| i).collect()
}

/// 一个可打开的 USB Config 候选。
#[derive(Debug, Clone)]
pub struct ConfigCandidate {
    pub path: CString,
    pub usage_page: u16,
    pub usage: u16,
    pub bus_type: BusType,
    pub interface_number: i32,
}

/// 从 hidapi 枚举里取出排好序的 USB Config 候选（路径 + 元数据）。
pub fn config_candidates(api: &HidApi) -> Vec<ConfigCandidate> {
    let infos: Vec<_> = api.device_list().collect();
    let metas: Vec<CandidateMeta> = infos
        .iter()
        .map(|d| CandidateMeta {
            vendor_id: d.vendor_id(),
            product_id: d.product_id(),
            usage_page: d.usage_page(),
            usage: d.usage(),
            bus_type: d.bus_type(),
        })
        .collect();
    rank_config_candidates(&metas)
        .into_iter()
        .map(|i| ConfigCandidate {
            path: infos[i].path().to_owned(),
            usage_page: metas[i].usage_page,
            usage: metas[i].usage,
            bus_type: metas[i].bus_type,
            interface_number: infos[i].interface_number(),
        })
        .collect()
}

/// 按排序逐个尝试打开，返回第一个能打开的设备及其候选信息。
///
/// 只负责「打得开」，不发命令；需要确认接口真能应答时用 [`probe_device_info_any`]。
pub fn open_first_config_candidate(api: &HidApi) -> anyhow::Result<(HidDevice, ConfigCandidate)> {
    let candidates = config_candidates(api);
    if candidates.is_empty() {
        return Err(anyhow::anyhow!(
            "未找到 USB Config 接口（0x{:04X}，已排除蓝牙总线）",
            USAGE_PAGE_CONFIG
        ));
    }
    let mut errors = Vec::new();
    for candidate in candidates {
        match api.open_path(&candidate.path) {
            Ok(device) => return Ok((device, candidate)),
            Err(e) => {
                log::warn!(
                    target: "hid",
                    "打开 Config 候选失败 usage=0x{:04X} bus={:?}: {}",
                    candidate.usage,
                    candidate.bus_type,
                    e
                );
                errors.push(e.to_string());
            }
        }
    }
    Err(anyhow::anyhow!(
        "所有 USB Config 候选都打不开: {}",
        errors.join("; ")
    ))
}

// ============ 应答校验（纯逻辑） ============

/// 一次读到的报文相对 0x13 请求的归类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InfoResponse {
    /// 不是 0x13 应答（按键/音频/其他命令），继续读。
    Unrelated,
    /// 是 0x13 应答，但设备返回非零 result。
    Rejected(u8),
    /// 是 0x13 应答，但长度不够解析完整 payload。
    Truncated(usize),
    /// 合法应答。
    Valid,
}

/// 校验一帧 USB 读结果是否是合法的 0x13 应答。
pub fn classify_device_info_response(buf: &[u8], len: usize) -> InfoResponse {
    let len = len.min(buf.len());
    if len < 2 || buf[0] != REPORT_ID_INPUT || buf[1] != CMD_GET_DEVICE_INFO {
        return InfoResponse::Unrelated;
    }
    if len < USB_PAYLOAD_OFFSET {
        return InfoResponse::Truncated(len);
    }
    if buf[3] != 0x00 {
        return InfoResponse::Rejected(buf[3]);
    }
    if len < USB_PAYLOAD_OFFSET + DEVICE_INFO_PAYLOAD_LEN {
        return InfoResponse::Truncated(len);
    }
    InfoResponse::Valid
}

/// 解析已通过 [`classify_device_info_response`] 的应答；全零 chip_id 视为无效。
fn parse_valid_response(buf: &[u8]) -> Result<DeviceInfo, AttemptOutcome> {
    let info = crate::tool::parse::parse_device_info_from_buf(
        buf,
        USB_PAYLOAD_OFFSET,
        ConnectionType::Usb,
    )
    .map_err(|_| AttemptOutcome::Truncated(buf.len()))?;
    if info.chip_id.chars().all(|c| c == '0') {
        return Err(AttemptOutcome::ZeroChipId);
    }
    Ok(info)
}

// ============ 探测流程（可注入 IO 与时钟） ============

/// 探测所需的最小 HID 读写面，便于用假设备做单元测试。
pub trait HidCommandIo {
    fn write_report(&self, data: &[u8]) -> Result<usize, String>;
    fn read_report(&self, buf: &mut [u8], timeout_ms: i32) -> Result<usize, String>;
}

impl HidCommandIo for HidDevice {
    fn write_report(&self, data: &[u8]) -> Result<usize, String> {
        self.write(data).map_err(|e| e.to_string())
    }
    fn read_report(&self, buf: &mut [u8], timeout_ms: i32) -> Result<usize, String> {
        self.read_timeout(buf, timeout_ms)
            .map_err(|e| e.to_string())
    }
}

/// 探测用的时钟（测试里换成虚拟时钟，避免真睡 10 秒）。
pub trait ProbeClock {
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
}

/// 真实时钟。
pub struct SystemClock;

impl ProbeClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// 探测节奏。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeTiming {
    /// 打开后、首个 0x13 之前的等待。
    pub settle_delay: Duration,
    /// 最多发几次 0x13。
    pub attempts: u32,
    /// 每次发出后等应答的窗口（窗口到期即重发）。
    pub response_window: Duration,
    /// 单次 `read_timeout` 的切片（毫秒）。
    pub read_slice_ms: i32,
}

impl ProbeTiming {
    /// 连接后读设备信息：等 800ms，每次 1.5s，最多 6 次（与 webTest 一致）。
    pub const CONNECT: Self = Self {
        settle_delay: Duration::from_millis(800),
        attempts: 6,
        response_window: Duration::from_millis(1500),
        read_slice_ms: 100,
    };

    /// 周期性的模式探测（仅 hidapi 判不出总线时用）：不等待、单次、2s 窗口，
    /// 与改动前的单次 10×200ms 读取总时长一致。
    pub const QUICK: Self = Self {
        settle_delay: Duration::ZERO,
        attempts: 1,
        response_window: Duration::from_millis(2000),
        read_slice_ms: 200,
    };
}

/// 单次 0x13 尝试的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptOutcome {
    /// 读到合法应答。
    Ok,
    /// 写命令失败（设备多半已不可用，终止本候选）。
    WriteFailed(String),
    /// 读失败（设备多半已不可用，终止本候选）。
    ReadFailed(String),
    /// 窗口内没等到 0x13 应答；`unrelated` 是期间读到的无关报文数。
    Timeout { unrelated: u32 },
    /// 设备返回非零 result。
    Rejected(u8),
    /// 应答长度不足。
    Truncated(usize),
    /// 应答合法但 chip_id 全零。
    ZeroChipId,
}

impl std::fmt::Display for AttemptOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ok => write!(f, "成功"),
            Self::WriteFailed(e) => write!(f, "写入失败: {e}"),
            Self::ReadFailed(e) => write!(f, "读取失败: {e}"),
            Self::Timeout { unrelated } => {
                write!(f, "超时无应答（期间收到 {unrelated} 个无关报文）")
            }
            Self::Rejected(code) => write!(f, "设备拒绝 result=0x{code:02X}"),
            Self::Truncated(len) => write!(f, "应答过短 len={len}"),
            Self::ZeroChipId => write!(f, "chip_id 全零，视为无效"),
        }
    }
}

/// 一次完整探测（含重试）的报告。
#[derive(Debug, Clone)]
pub struct ProbeReport {
    pub info: Option<DeviceInfo>,
    pub attempts: Vec<AttemptOutcome>,
}

impl ProbeReport {
    /// 尝试记录的一行摘要，供日志与诊断报告使用。
    pub fn summary(&self) -> String {
        self.attempts
            .iter()
            .enumerate()
            .map(|(i, a)| format!("#{} {}", i + 1, a))
            .collect::<Vec<_>>()
            .join("；")
    }
}

/// 在一个已打开的接口上按 `timing` 发 0x13 并严格校验应答。
pub fn probe_device_info(
    io: &impl HidCommandIo,
    timing: ProbeTiming,
    clock: &impl ProbeClock,
) -> ProbeReport {
    let mut attempts = Vec::new();
    if !timing.settle_delay.is_zero() {
        clock.sleep(timing.settle_delay);
    }
    let request = HidPacket::get_device_info();
    let mut buf = [0u8; 64];

    for _ in 0..timing.attempts.max(1) {
        if let Err(e) = io.write_report(&request) {
            attempts.push(AttemptOutcome::WriteFailed(e));
            break;
        }
        let deadline = clock.now() + timing.response_window;
        let mut unrelated = 0u32;
        let mut outcome = None;
        while clock.now() < deadline {
            buf.fill(0);
            let len = match io.read_report(&mut buf, timing.read_slice_ms.max(1)) {
                Ok(len) => len,
                Err(e) => {
                    outcome = Some(AttemptOutcome::ReadFailed(e));
                    break;
                }
            };
            if len == 0 {
                continue; // 本切片超时，窗口未到继续等
            }
            match classify_device_info_response(&buf, len) {
                InfoResponse::Unrelated => unrelated += 1,
                InfoResponse::Rejected(code) => {
                    outcome = Some(AttemptOutcome::Rejected(code));
                    break;
                }
                InfoResponse::Truncated(n) => {
                    outcome = Some(AttemptOutcome::Truncated(n));
                    break;
                }
                InfoResponse::Valid => match parse_valid_response(&buf[..len]) {
                    Ok(info) => {
                        attempts.push(AttemptOutcome::Ok);
                        return ProbeReport {
                            info: Some(info),
                            attempts,
                        };
                    }
                    Err(bad) => {
                        outcome = Some(bad);
                        break;
                    }
                },
            }
        }
        let outcome = outcome.unwrap_or(AttemptOutcome::Timeout { unrelated });
        let fatal = matches!(
            outcome,
            AttemptOutcome::WriteFailed(_) | AttemptOutcome::ReadFailed(_)
        );
        attempts.push(outcome);
        if fatal {
            break;
        }
    }
    ProbeReport {
        info: None,
        attempts,
    }
}

/// 单个候选的探测记录。
#[derive(Debug, Clone)]
pub struct CandidateProbe {
    pub candidate: ConfigCandidate,
    /// `Err` = 打不开；`Ok` = 打开后的探测报告。
    pub result: Result<ProbeReport, String>,
}

/// 把全部候选的探测记录压成一行，供错误事件与日志使用。
pub fn describe_probe_records(records: &[CandidateProbe]) -> String {
    if records.is_empty() {
        return format!(
            "未找到 USB Config 接口（0x{:04X}，已排除蓝牙总线）",
            USAGE_PAGE_CONFIG
        );
    }
    records
        .iter()
        .map(|r| {
            let detail = match &r.result {
                Ok(report) => report.summary(),
                Err(e) => format!("打开失败: {e}"),
            };
            format!(
                "[usage=0x{:04X} bus={:?}] {}",
                r.candidate.usage, r.candidate.bus_type, detail
            )
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// 按排序逐个打开 USB Config 候选并探测 0x13，第一个成功即返回。
///
/// 返回 `(设备信息, 每个候选的记录)`；全部失败时设备信息为 `None`。
pub fn probe_device_info_any(
    api: &HidApi,
    timing: ProbeTiming,
) -> (Option<DeviceInfo>, Vec<CandidateProbe>) {
    let mut records = Vec::new();
    for candidate in config_candidates(api) {
        let result = match api.open_path(&candidate.path) {
            Ok(device) => Ok(probe_device_info(&device, timing, &SystemClock)),
            Err(e) => Err(e.to_string()),
        };
        let info = result.as_ref().ok().and_then(|r| r.info.clone());
        match &result {
            Ok(report) if info.is_some() => log::debug!(
                target: "hid",
                "0x13 探测成功 usage=0x{:04X} bus={:?}: {}",
                candidate.usage,
                candidate.bus_type,
                report.summary()
            ),
            // 失败只记 debug：周期性模式探测会反复走到这里，由调用方决定是否上报。
            Ok(report) => log::debug!(
                target: "hid",
                "0x13 探测失败 usage=0x{:04X} bus={:?}，换下一个候选: {}",
                candidate.usage,
                candidate.bus_type,
                report.summary()
            ),
            Err(e) => log::debug!(
                target: "hid",
                "打开 Config 候选失败 usage=0x{:04X} bus={:?}: {}",
                candidate.usage,
                candidate.bus_type,
                e
            ),
        }
        records.push(CandidateProbe { candidate, result });
        if info.is_some() {
            return (info, records);
        }
    }
    (None, records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    fn meta(usage_page: u16, usage: u16, bus_type: BusType) -> CandidateMeta {
        CandidateMeta {
            vendor_id: VID,
            product_id: crate::kernel::protocol_hid::PID_USB,
            usage_page,
            usage,
            bus_type,
        }
    }

    // ===== 接口挑选 =====

    #[test]
    fn bluetooth_collection_is_never_a_usb_candidate() {
        // Windows 上系统配对的蓝牙键盘与 USB 同 PID、同 0xFFA0:0x0002。
        let metas = [
            meta(USAGE_PAGE_CONFIG, 0x0002, BusType::Bluetooth),
            meta(USAGE_PAGE_CONFIG, 0x0002, BusType::Usb),
        ];
        assert_eq!(rank_config_candidates(&metas), vec![1]);
    }

    #[test]
    fn command_usage_is_preferred_then_usb_over_unknown() {
        let metas = [
            meta(USAGE_PAGE_CONFIG, 0x0001, BusType::Usb),
            meta(USAGE_PAGE_CONFIG, 0x0002, BusType::Unknown),
            meta(0x000C, 0x0001, BusType::Usb),
            meta(USAGE_PAGE_CONFIG, 0x0002, BusType::Usb),
            meta(USAGE_PAGE_CONFIG, 0x0001, BusType::Unknown),
        ];
        assert_eq!(rank_config_candidates(&metas), vec![3, 1, 0, 4]);
    }

    #[test]
    fn foreign_vendor_and_non_config_pages_are_excluded() {
        let mut other = meta(USAGE_PAGE_CONFIG, 0x0002, BusType::Usb);
        other.vendor_id = 0x1234;
        let metas = [other, meta(0xFFAA, 0x0002, BusType::Usb)];
        assert!(rank_config_candidates(&metas).is_empty());
    }

    #[test]
    fn equal_rank_keeps_enumeration_order() {
        let metas = [
            meta(USAGE_PAGE_CONFIG, 0x0002, BusType::Usb),
            meta(USAGE_PAGE_CONFIG, 0x0002, BusType::Usb),
        ];
        assert_eq!(rank_config_candidates(&metas), vec![0, 1]);
    }

    // ===== 应答校验 =====

    fn valid_response(chip: [u8; 4]) -> [u8; 64] {
        let mut buf = [0u8; 64];
        buf[0] = REPORT_ID_INPUT;
        buf[1] = CMD_GET_DEVICE_INFO;
        buf[2] = 0x15;
        buf[3] = 0x00;
        buf[4] = 0x03; // mode = USB
        buf[4 + 9] = 0x3B; // fw minor
        buf[4 + 10] = 0x01; // fw major
        buf[4 + 14] = 88; // battery
        buf[4 + 16..4 + 20].copy_from_slice(&chip);
        buf
    }

    #[test]
    fn classify_rejects_wrong_cmd_report_and_result() {
        let mut buf = valid_response([1, 2, 3, 4]);
        assert_eq!(classify_device_info_response(&buf, 64), InfoResponse::Valid);

        buf[1] = 0x14;
        assert_eq!(
            classify_device_info_response(&buf, 64),
            InfoResponse::Unrelated
        );

        // 音频报文（report id 0xB1）里碰巧出现 0x13 也不能当应答。
        let mut audio = valid_response([1, 2, 3, 4]);
        audio[0] = 0xB1;
        assert_eq!(
            classify_device_info_response(&audio, 64),
            InfoResponse::Unrelated
        );

        let mut rejected = valid_response([1, 2, 3, 4]);
        rejected[3] = 0x01;
        assert_eq!(
            classify_device_info_response(&rejected, 64),
            InfoResponse::Rejected(0x01)
        );

        assert_eq!(
            classify_device_info_response(&valid_response([1, 2, 3, 4]), 10),
            InfoResponse::Truncated(10)
        );
    }

    // ===== 探测流程 =====

    /// 虚拟时钟：read 超时与 sleep 只推进虚拟时间。
    struct FakeClock {
        start: Instant,
        elapsed: Cell<Duration>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                start: Instant::now(),
                elapsed: Cell::new(Duration::ZERO),
            }
        }
        fn advance(&self, d: Duration) {
            self.elapsed.set(self.elapsed.get() + d);
        }
    }

    impl ProbeClock for FakeClock {
        fn now(&self) -> Instant {
            self.start + self.elapsed.get()
        }
        fn sleep(&self, duration: Duration) {
            self.advance(duration);
        }
    }

    /// 假设备：前 `drop_writes` 次 0x13 被丢弃（模拟冷插入），之后每次写入都回 `reply`。
    struct FakeDevice<'a> {
        clock: &'a FakeClock,
        drop_writes: Cell<u32>,
        writes: Cell<u32>,
        noise_per_write: u32,
        reply: Option<[u8; 64]>,
        queue: RefCell<VecDeque<[u8; 64]>>,
    }

    impl<'a> FakeDevice<'a> {
        fn new(clock: &'a FakeClock, drop_writes: u32, reply: Option<[u8; 64]>) -> Self {
            Self {
                clock,
                drop_writes: Cell::new(drop_writes),
                writes: Cell::new(0),
                noise_per_write: 0,
                reply,
                queue: RefCell::new(VecDeque::new()),
            }
        }
    }

    impl HidCommandIo for FakeDevice<'_> {
        fn write_report(&self, data: &[u8]) -> Result<usize, String> {
            assert_eq!(data[1], CMD_GET_DEVICE_INFO);
            self.writes.set(self.writes.get() + 1);
            let mut queue = self.queue.borrow_mut();
            for _ in 0..self.noise_per_write {
                let mut key = [0u8; 64];
                key[0] = 0x02; // 按键报文
                key[1] = CMD_GET_DEVICE_INFO; // 字节碰巧相同也不算应答
                queue.push_back(key);
            }
            if self.drop_writes.get() > 0 {
                self.drop_writes.set(self.drop_writes.get() - 1);
            } else if let Some(reply) = self.reply {
                queue.push_back(reply);
            }
            Ok(data.len())
        }

        fn read_report(&self, buf: &mut [u8], timeout_ms: i32) -> Result<usize, String> {
            if let Some(report) = self.queue.borrow_mut().pop_front() {
                buf[..64].copy_from_slice(&report);
                self.clock.advance(Duration::from_millis(1));
                return Ok(64);
            }
            self.clock
                .advance(Duration::from_millis(timeout_ms.max(0) as u64));
            Ok(0)
        }
    }

    #[test]
    fn cold_plug_dropped_probes_are_retried_until_answered() {
        let clock = FakeClock::new();
        let device = FakeDevice::new(&clock, 3, Some(valid_response([0xAB, 0xCD, 0x12, 0x34])));
        let report = probe_device_info(&device, ProbeTiming::CONNECT, &clock);
        let info = report.info.expect("第 4 次应答成功");
        assert_eq!(info.chip_id, "ABCD1234");
        assert_eq!(device.writes.get(), 4);
        assert_eq!(report.attempts.len(), 4);
        assert!(matches!(report.attempts[0], AttemptOutcome::Timeout { .. }));
        assert_eq!(report.attempts[3], AttemptOutcome::Ok);
        // 800ms 等待 + 3 个 1.5s 窗口
        assert!(clock.elapsed.get() >= Duration::from_millis(800 + 3 * 1500));
    }

    #[test]
    fn silent_device_fails_after_six_attempts_without_fabricating_info() {
        let clock = FakeClock::new();
        let device = FakeDevice::new(&clock, u32::MAX, None);
        let report = probe_device_info(&device, ProbeTiming::CONNECT, &clock);
        assert!(report.info.is_none());
        assert_eq!(device.writes.get(), 6);
        assert_eq!(report.attempts.len(), 6);
        // 约 10 秒：800ms + 6 × 1.5s
        let elapsed = clock.elapsed.get();
        assert!(elapsed >= Duration::from_millis(9_800));
        assert!(elapsed < Duration::from_millis(10_500));
    }

    #[test]
    fn zero_chip_id_and_rejected_result_are_not_accepted() {
        let clock = FakeClock::new();
        let device = FakeDevice::new(&clock, 0, Some(valid_response([0, 0, 0, 0])));
        let report = probe_device_info(&device, ProbeTiming::CONNECT, &clock);
        assert!(report.info.is_none());
        assert!(report
            .attempts
            .iter()
            .all(|a| *a == AttemptOutcome::ZeroChipId));

        let clock = FakeClock::new();
        let mut rejected = valid_response([1, 2, 3, 4]);
        rejected[3] = 0x02;
        let device = FakeDevice::new(&clock, 0, Some(rejected));
        let report = probe_device_info(&device, ProbeTiming::CONNECT, &clock);
        assert!(report.info.is_none());
        assert_eq!(report.attempts[0], AttemptOutcome::Rejected(0x02));
    }

    #[test]
    fn unrelated_reports_are_skipped_within_the_window() {
        let clock = FakeClock::new();
        let mut device = FakeDevice::new(&clock, 0, Some(valid_response([9, 8, 7, 6])));
        device.noise_per_write = 5;
        let report = probe_device_info(&device, ProbeTiming::CONNECT, &clock);
        assert_eq!(report.info.map(|i| i.chip_id).as_deref(), Some("09080706"));
        assert_eq!(device.writes.get(), 1);
    }

    #[test]
    fn write_failure_stops_the_candidate_immediately() {
        struct Broken;
        impl HidCommandIo for Broken {
            fn write_report(&self, _: &[u8]) -> Result<usize, String> {
                Err("gone".into())
            }
            fn read_report(&self, _: &mut [u8], _: i32) -> Result<usize, String> {
                unreachable!()
            }
        }
        let clock = FakeClock::new();
        let report = probe_device_info(&Broken, ProbeTiming::CONNECT, &clock);
        assert!(report.info.is_none());
        assert_eq!(
            report.attempts,
            vec![AttemptOutcome::WriteFailed("gone".into())]
        );
    }

    #[test]
    fn quick_timing_matches_the_previous_single_probe_budget() {
        let clock = FakeClock::new();
        let device = FakeDevice::new(&clock, u32::MAX, None);
        let report = probe_device_info(&device, ProbeTiming::QUICK, &clock);
        assert!(report.info.is_none());
        assert_eq!(device.writes.get(), 1);
        assert_eq!(clock.elapsed.get(), Duration::from_millis(2000));
    }
}
