//! 热插拔/断线重连管理器(USB + BLE Vendor GATT)—— V2 tokio async。
//!
//! 四阶段循环:wait_for_device → connect → monitor → cleanup。
//! - USB:hidapi Config/Consumer，拔出立即 DeviceGone→自动重连；不枚举系统音频端点
//! - BLE:Vendor GATT(btleplug),扫描失败/连接失败保持 ble_auto_connect 重试,
//!   GATT 断连(超范围/固件重启)也自动重连;只有手动断开/CMD=0x60 才停
//!
//! USB 优先于 BLE(USB 插入时抢占 BLE 会话)。
//!
//! **V2 变化**:`run()` 改 async;`thread::sleep` → `tokio::time::sleep`;
//! 阻塞的 HID 枚举(`detect_hid_connection` / `connect_device_hid`)用
//! `tokio::task::spawn_blocking` 包装;BLE client 调用 `.await`。
//! 四阶段状态机骨架与 V1 一致(实战验证过,不重构为 select!,避免引入时序 bug)。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use tokio::sync::broadcast;

use crate::kernel::event::{
    BoardEvent, ConnectionEvent, DeviceInfo, DisconnectReason, ErrorEvent, ReconnectEvent,
    ReconnectState,
};
use crate::kernel::protocol_hid::*;
use crate::kernel::types::ConnectionType;

#[cfg(feature = "usb")]
use {
    crate::runtime::usb::device_manager::{DeviceConnection, DeviceManager},
    crate::runtime::usb::monitor::{HidMonitor, MonitorConfig},
    hidapi::{BusType, HidApi},
};

#[cfg(feature = "ble")]
use crate::runtime::ble::gatt_client::VendorGattClient;

// ============ CFRunLoop 辅助（macOS hidapi 兼容） ============

/// 在带活跃 CFRunLoop 的专用线程上运行阻塞闭包。
///
/// macOS 的 hidapi (`HidApi::new()`) 内部调用 IOKit 的
/// `IOHIDDeviceScheduleWithRunLoop` → `CFRunLoopAddSource`，要求当前线程有
/// **正在运行的** CFRunLoop（macOS 26.5 启用 PAC 指针认证，对 RunLoop source
/// 签名做校验，未运行的 RunLoop 会导致 `__CFCheckCFPACSignature` 崩溃）。
///
/// tokio 的 `spawn_blocking` 线程池没有 RunLoop。本函数使用一个全局常驻的
/// HID 专用线程（持续 `CFRunLoopRun`），把闭包投递到该线程执行。
/// 非 macOS 平台直接用 `tokio::task::spawn_blocking`。
pub(crate) async fn spawn_blocking_with_runloop<F, T>(f: F) -> std::thread::Result<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    #[cfg(target_os = "macos")]
    {
        hid_runloop_thread::execute(f)
    }
    #[cfg(not(target_os = "macos"))]
    {
        // 返回类型须与 macOS 分支的 `std::thread::Result` 对齐：panic 时交出
        // payload。阻塞任务不会被取消，join 失败只剩 panic 与 runtime 关闭两种，
        // 后者把错误信息包成 payload，调用方统一按线程 panic 处理。
        match tokio::task::spawn_blocking(f).await {
            Ok(value) => Ok(value),
            Err(join_err) => match join_err.try_into_panic() {
                Ok(payload) => Err(payload),
                Err(join_err) => {
                    Err(Box::new(join_err.to_string()) as Box<dyn std::any::Any + Send>)
                }
            },
        }
    }
}

#[cfg(target_os = "macos")]
mod hid_runloop_thread {
    use std::sync::Mutex;

    type Job = Box<dyn FnOnce() + Send + 'static>;

    /// 全局任务队列（HID 专用线程的 timer 回调从此取出闭包执行）
    static JOB_QUEUE: Mutex<Vec<Job>> = Mutex::new(Vec::new());

    // FFI: CoreFoundation RunLoop + Timer
    #[repr(C)]
    struct CFRunLoopTimerContext {
        version: isize,
        info: *mut std::ffi::c_void,
        retain: Option<unsafe extern "C" fn(*const std::ffi::c_void) -> *const std::ffi::c_void>,
        release: Option<unsafe extern "C" fn(*const std::ffi::c_void)>,
        copy_description:
            Option<unsafe extern "C" fn(*const std::ffi::c_void) -> *mut std::ffi::c_void>,
    }

    extern "C" {
        fn CFRunLoopGetCurrent() -> *mut std::ffi::c_void;
        fn CFRunLoopRun();
        fn CFRunLoopTimerCreate(
            allocator: *mut std::ffi::c_void,
            fire_date: f64,
            interval: f64,
            flags: u64,
            order: u64,
            callout: extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void),
            context: *mut CFRunLoopTimerContext,
        ) -> *mut std::ffi::c_void;
        fn CFRunLoopAddTimer(
            rl: *mut std::ffi::c_void,
            timer: *mut std::ffi::c_void,
            mode: *const std::ffi::c_void,
        );
        fn CFAbsoluteTimeGetCurrent() -> f64;
        // kCFRunLoopCommonModes — CFString 全局常量
        static kCFRunLoopCommonModes: *const std::ffi::c_void;
    }

    /// Timer 回调：在 RunLoop 线程上下文中取出并执行队列任务
    extern "C" fn timer_callback(_timer: *mut std::ffi::c_void, _info: *mut std::ffi::c_void) {
        let jobs: Vec<Job> = std::mem::take(&mut *JOB_QUEUE.lock().unwrap());
        for job in jobs {
            job();
        }
    }

    /// 在 HID 专用线程（带正在运行的 CFRunLoop）上执行闭包。
    ///
    /// 创建一个常驻线程，向其 CFRunLoop 添加一个 5ms 间隔的 Timer，Timer 回调里
    /// 取出队列任务执行。RunLoop 真正处于 CFRunLoopRun 运行状态，确保 hidapi
    /// IOKit 的 ScheduleWithRunLoop → CFRunLoopAddSource 的 PAC 签名校验通过。
    ///
    /// # 失败处理
    ///
    /// 线程创建在系统资源耗尽（线程数达上限、虚拟内存不足）时可能失败。此处用
    /// `OnceLock` 缓存首次 spawn 结果：失败不再 panic 整个进程,而是返回 `Err`,
    /// 让调用方降级处理(典型表现:USB 设备探测/命令交互不可用,但进程继续运行,
    /// BLE/音频路径不受影响)。
    pub fn execute<F, T>(f: F) -> std::thread::Result<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        // SAFETY: 首次进入时启动 HID runloop 线程,持有 CFRunLoopTimerContext 栈地址。
        // context 在 spawn 闭包内构造,RunLoop 与 timer 与该闭包同生命周期(RunLoop
        // 持续运行直到进程退出),flags=0 且 retain=None 表示 CF 不 retain info 指针,
        // 栈对象在闭包(从而 RunLoop 线程)存活期间始终有效。
        static RUNLOOP_STARTED: std::sync::OnceLock<
            Result<(), Box<dyn std::error::Error + Send + Sync>>,
        > = std::sync::OnceLock::new();

        let init = RUNLOOP_STARTED.get_or_init(|| {
            std::thread::Builder::new()
                .name("hid-runloop".to_string())
                .spawn(|| unsafe {
                    let rl = CFRunLoopGetCurrent();
                    // 创建 5ms 间隔的 Timer（fireDate=现在, interval=5ms）
                    let now = CFAbsoluteTimeGetCurrent();
                    let timer = CFRunLoopTimerCreate(
                        std::ptr::null_mut(),
                        now,
                        0.005, // 5ms 间隔
                        0,
                        0,
                        timer_callback,
                        &CFRunLoopTimerContext {
                            version: 0,
                            info: std::ptr::null_mut(),
                            retain: None,
                            release: None,
                            copy_description: None,
                        } as *const _ as *mut _,
                    );
                    if !timer.is_null() {
                        CFRunLoopAddTimer(rl, timer, kCFRunLoopCommonModes);
                    }
                    // 运行 RunLoop（阻塞，直到进程退出）
                    CFRunLoopRun();
                })
                .map(|_| ())
                .map_err(|e| {
                    log::error!(
                        target: "hotplug",
                        "HID runloop 线程创建失败: {} (USB 设备路径将不可用)",
                        e
                    );
                    Box::new(e) as Box<dyn std::error::Error + Send + Sync>
                })
        });

        if let Err(e) = init {
            return Err(Box::new(std::io::Error::other(e.to_string())));
        }

        let (tx, rx) = std::sync::mpsc::channel::<std::thread::Result<T>>();
        JOB_QUEUE.lock().unwrap().push(Box::new(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
            let _ = tx.send(result);
        }));

        rx.recv().unwrap_or_else(|e| Err(Box::new(e)))
    }
}

// ============ 配置 ============

/// 热插拔检测间隔配置
#[derive(Debug, Clone)]
pub struct HotplugConfig {
    /// 无设备时轮询间隔(默认 2s)
    pub retry_interval: Duration,
    /// 运行中检测连接变化间隔(默认 5s)
    pub check_interval: Duration,
}

impl Default for HotplugConfig {
    fn default() -> Self {
        Self {
            retry_interval: Duration::from_secs(2),
            check_interval: Duration::from_secs(5),
        }
    }
}

/// HID 枚举结果的快速判定。
///
/// hidapi 能明确给出 USB / Bluetooth 总线时直接采用，不再发送 CMD 0x13
/// 做最长约 2 秒的同步探测；仅总线类型为 Unknown 时保留命令探测兜底。
#[cfg(feature = "usb")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HidDetectionDecision {
    Connected,
    ProbeMode,
    NotConnected,
}

/// 复用当前线程的 [`HidApi`] 实例执行 `f`,避免每次检测都重建整个 HID 上下文。
///
/// ## 为什么是 thread_local 而不是全局单例
///
/// 检测路径(`detect_hid_connection` / `check_usb_preemption`)始终经
/// [`spawn_blocking_with_runloop`] 投递到同一条 HID 线程执行,天然是单线程复用,
/// 不需要为跨线程共享付锁与 Send/Sync 的代价。非 macOS 走 tokio 阻塞线程池时
/// 复用率会低一些,但仍然正确——每条线程各持一份。
///
/// ## 失败处理:保守,不把 API 故障当成拔线
///
/// `refresh_devices()` 失败先丢弃缓存重建一次;重建仍失败才返回 `None`,由调用方
/// 落到「HidApi 不可用」的既有分支(连同音频信号一起判断),**不允许**据此直接
/// 判定「未连接」——否则一次偶发的刷新失败会表现成设备闪断。
///
/// ## 重入
///
/// 实例在调用 `f` 之前就从槽里取走,**闭包执行期间不持有任何 `RefCell` 借用**,
/// 因此 `f` 内部再次进入本函数不会 panic。代价只是那次重入会另建一个临时实例、
/// 返回时被外层的写回覆盖掉——多一次构造,不影响正确性。若换成「借用贯穿闭包」
/// 的写法则会直接 `BorrowMutError`。
#[cfg(feature = "usb")]
fn with_reused_hid_api<T>(f: impl FnOnce(&HidApi) -> T) -> Option<T> {
    use std::cell::RefCell;

    thread_local! {
        static HID_API: RefCell<Option<HidApi>> = const { RefCell::new(None) };
    }

    HID_API.with(|cell| {
        // 取走实例,借用到此为止。
        let mut api = cell.borrow_mut().take();

        // 已有实例:刷新设备列表。失败则丢弃,走下面的重建。
        if let Some(existing) = api.as_mut() {
            if let Err(e) = existing.refresh_devices() {
                log::warn!(
                    target: "hid",
                    "HidApi refresh_devices 失败,丢弃缓存重建: {e}"
                );
                api = None;
            }
        }

        if api.is_none() {
            match HidApi::new() {
                Ok(created) => api = Some(created),
                Err(e) => {
                    log::warn!(target: "hid", "HidApi 创建失败: {e}");
                    return None;
                }
            }
        }

        // 此刻没有持有借用,f 内部即使重入本函数也只是另建实例,不会 panic。
        let result = api.as_ref().map(f);
        *cell.borrow_mut() = api;
        result
    })
}

/// BLE 会话里每隔多少个 `check_interval` 强制跑一次完整检测。
///
/// 目标是约 5 秒一次:driver 覆盖的 500ms 周期 → 10(九成检测走轻量路径),
/// SDK 默认的 5s 周期 → 1(每次都完整检测,与改动前行为一致)。
///
/// 两端都收口:
/// - 下限 1 —— 避免除零,也保证再长的周期都不会退化成「永不完整检测」。
/// - 上限 [`MAX_LIGHTWEIGHT_STREAK`] —— 消费方若把周期配得极小(甚至 0),按比例算
///   会得出上千次才完整检测一回,等于把音频兜底这条路架空。宁可多跑几次完整检测。
#[cfg(feature = "usb")]
fn full_check_interval_ticks(check_interval: Duration) -> u32 {
    const FULL_CHECK_PERIOD_MS: u128 = 5_000;
    /// 连续走轻量检测的最大次数。
    const MAX_LIGHTWEIGHT_STREAK: u128 = 20;

    let interval_ms = check_interval.as_millis().max(1);
    (FULL_CHECK_PERIOD_MS / interval_ms).clamp(1, MAX_LIGHTWEIGHT_STREAK) as u32
}

/// BLE 会话期间轻量 USB 检测的结果。
#[cfg(feature = "usb")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlePreemptionCheck {
    /// 发现目标 USB HID,应当抢占。
    Preempt,
    /// 没发现,继续保持 BLE 会话。
    Keep,
    /// HID 侧不可用(权限/驱动),必须退回完整检测——音频是这类机器唯一的线索。
    NeedsFullCheck,
}

/// 扫描目标设备在 HID 侧的总线分布,返回 `(有 USB 总线的, 有未知总线的)`。
///
/// 完整检测与 BLE 抢占检测共用同一份枚举与过滤条件,避免两处各写一遍后随时间
/// 漂移出不同的设备识别口径。蓝牙/I2C/SPI 总线上的目标设备不计入——那是当前
/// 这条 BLE 连接自己,不能当成「USB 插上了」。
#[cfg(feature = "usb")]
fn scan_target_hid_buses(api: &HidApi) -> (bool, bool) {
    let mut has_usb_hid = false;
    let mut has_unknown_hid = false;
    for device in api
        .device_list()
        .filter(|d| d.vendor_id() == VID && is_target_pid(d.product_id()))
    {
        match device.bus_type() {
            BusType::Usb => has_usb_hid = true,
            BusType::Unknown => has_unknown_hid = true,
            BusType::Bluetooth | BusType::I2c | BusType::Spi => {}
        }
    }
    (has_usb_hid, has_unknown_hid)
}

/// BLE 抢占专用的 USB 判定:只要目标 HID 出现就该切走。
///
/// 与 [`decide_hid_detection`] 的区别是把 `BusType::Unknown` 也算数。完整路径
/// 对 Unknown 会再发 CMD 0x13 探测模式,而抢占只需要知道「该切了」,不必付探测
/// 的开销;宁可敏感一点,真连不上会由后续的连接流程收敛。
#[cfg(feature = "usb")]
fn decide_ble_preemption(has_usb_hid: bool, has_unknown_hid: bool) -> bool {
    has_usb_hid || has_unknown_hid
}

/// 把 HID 侧的判定结果映射成 BLE 抢占动作。
///
/// `None` 表示 HID 整体不可用(`HidApi` 创建与刷新都失败)。此时**必须**退回完整
/// 检测,而不是当作「没插 USB」——在 HID 受限的机器上音频是唯一线索,直接判否会
/// 让抢占永远不发生。
#[cfg(feature = "usb")]
fn ble_preemption_from_hid(hid_says_preempt: Option<bool>) -> BlePreemptionCheck {
    match hid_says_preempt {
        Some(true) => BlePreemptionCheck::Preempt,
        Some(false) => BlePreemptionCheck::Keep,
        None => BlePreemptionCheck::NeedsFullCheck,
    }
}

#[cfg(feature = "usb")]
fn decide_hid_detection(has_usb_hid: bool, has_unknown_hid: bool) -> HidDetectionDecision {
    if has_usb_hid {
        HidDetectionDecision::Connected
    } else if has_unknown_hid {
        HidDetectionDecision::ProbeMode
    } else {
        HidDetectionDecision::NotConnected
    }
}

// ============ 回调类型 ============

pub type OnConnectionChange = Box<dyn Fn(Option<ConnectionType>) + Send + Sync>;
#[cfg(feature = "usb")]
pub type OnMonitorReady = Box<
    dyn Fn(
            Arc<Mutex<Option<crate::runtime::usb::device_manager::DeviceConnection>>>,
            Arc<AtomicBool>,
        ) + Send
        + Sync,
>;
#[cfg(feature = "ble")]
pub type OnBleDeviceNameUpdated = Box<dyn Fn(&str) + Send + Sync>;
/// 延迟创建 VendorGattClient 的回调。
///
/// 返回一个 Future，解析到已就绪的 client。hotplug 的 BLE 路径首次进入时调用它——
/// 那一刻才建 adapter（触发 macOS 蓝牙授权弹窗），而不是 App 启动时就建。
#[cfg(feature = "ble")]
pub type EnsureVendorGattClient = Box<
    dyn Fn() -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Arc<VendorGattClient>>> + Send>,
        > + Send
        + Sync,
>;

// ============ HotplugManager ============

pub struct HotplugManager {
    running: Arc<AtomicBool>,
    stop_requested: Option<Arc<AtomicBool>>,
    event_tx: broadcast::Sender<BoardEvent>,
    config: HotplugConfig,
    on_connection_change: Option<OnConnectionChange>,
    #[cfg(feature = "usb")]
    on_monitor_ready: Option<OnMonitorReady>,
    #[cfg(feature = "ble")]
    /// BLE GATT 客户端的共享槽（与 BoardDeviceCore 同一个 `Arc<Mutex>`）。
    /// 初始为 None——由 [`ensure_client`] 在首次进入 BLE 路径时延迟填充。
    vendor_gatt_client_slot: Arc<Mutex<Option<Arc<VendorGattClient>>>>,
    #[cfg(feature = "ble")]
    ensure_client: Option<EnsureVendorGattClient>,
    #[cfg(feature = "ble")]
    ble_last_device_name: Arc<Mutex<Option<String>>>,
    #[cfg(feature = "ble")]
    ble_auto_connect: Arc<AtomicBool>,
    #[cfg(feature = "ble")]
    on_ble_device_name_updated: Option<OnBleDeviceNameUpdated>,
}

impl HotplugManager {
    pub fn new(event_tx: broadcast::Sender<BoardEvent>, config: HotplugConfig) -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            stop_requested: None,
            event_tx,
            config,
            on_connection_change: None,
            #[cfg(feature = "usb")]
            on_monitor_ready: None,
            #[cfg(feature = "ble")]
            vendor_gatt_client_slot: Arc::new(Mutex::new(None)),
            #[cfg(feature = "ble")]
            ensure_client: None,
            #[cfg(feature = "ble")]
            ble_last_device_name: Arc::new(Mutex::new(None)),
            #[cfg(feature = "ble")]
            ble_auto_connect: Arc::new(AtomicBool::new(true)),
            #[cfg(feature = "ble")]
            on_ble_device_name_updated: None,
        }
    }

    pub fn on_connection_change(mut self, cb: OnConnectionChange) -> Self {
        self.on_connection_change = Some(cb);
        self
    }

    #[cfg(feature = "usb")]
    pub fn on_monitor_ready(mut self, cb: OnMonitorReady) -> Self {
        self.on_monitor_ready = Some(cb);
        self
    }

    pub fn with_running_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.stop_requested = Some(flag);
        self
    }

    #[cfg(feature = "ble")]
    pub fn with_vendor_gatt_client_slot(
        mut self,
        slot: Arc<Mutex<Option<Arc<VendorGattClient>>>>,
    ) -> Self {
        self.vendor_gatt_client_slot = slot;
        self
    }

    /// 注册延迟创建 VendorGattClient 的回调。
    /// hotplug 的 BLE 路径首次进入时调用它——那一刻才建 adapter（触发蓝牙授权）。
    #[cfg(feature = "ble")]
    pub fn with_ble_ensure_client(mut self, ensure: EnsureVendorGattClient) -> Self {
        self.ensure_client = Some(ensure);
        self
    }

    #[cfg(feature = "ble")]
    pub fn with_ble_target_device_name(mut self, name: Arc<Mutex<Option<String>>>) -> Self {
        self.ble_last_device_name = name;
        self
    }

    #[cfg(feature = "ble")]
    pub fn with_ble_auto_connect(mut self, flag: Arc<AtomicBool>) -> Self {
        self.ble_auto_connect = flag;
        self
    }

    #[cfg(feature = "ble")]
    pub fn on_ble_device_name_updated(mut self, cb: OnBleDeviceNameUpdated) -> Self {
        self.on_ble_device_name_updated = Some(cb);
        self
    }

    /// 设置是否自动重连 BLE(BoardDevice 手动断开时置 false)
    #[cfg(feature = "ble")]
    pub fn set_ble_auto_connect(&self, on: bool) {
        self.ble_auto_connect.store(on, Ordering::SeqCst);
    }

    /// 设置 BLE 目标设备名(None = 清除,停止 BLE 自动重连)
    #[cfg(feature = "ble")]
    pub fn set_ble_target(&self, name: Option<&str>) {
        *self.ble_last_device_name.lock().unwrap() = name.map(|s| s.to_string());
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst) && !self.is_stop_requested()
    }

    fn is_stop_requested(&self) -> bool {
        self.stop_requested
            .as_ref()
            .map(|f| !f.load(Ordering::SeqCst))
            .unwrap_or(false)
    }

    // ================================================================
    // 主循环(四阶段)
    // ================================================================

    pub async fn run(&mut self) {
        self.running.store(true, Ordering::SeqCst);
        log::info!(target: "hotplug", "热插拔管理器启动");

        while self.running.load(Ordering::SeqCst) && !self.is_stop_requested() {
            self.emit_reconnect(ReconnectState::WaitingForDevice, None, None);
            let conn_type = match self.wait_for_device().await {
                Some(ct) => ct,
                None => break,
            };

            match conn_type {
                #[cfg(feature = "usb")]
                ConnectionType::Usb => self.run_usb_session().await,
                #[cfg(not(feature = "usb"))]
                ConnectionType::Usb => {
                    log::warn!(target: "hotplug", "检测到 USB 连接但 usb feature 未启用,跳过");
                }
                ConnectionType::Ble => self.run_ble_session().await,
            }

            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        log::info!(target: "hotplug", "热插拔管理器停止");
    }

    async fn wait_for_device(&self) -> Option<ConnectionType> {
        while self.running.load(Ordering::SeqCst) && !self.is_stop_requested() {
            if let Some(ct) = self.detect_connection().await {
                return Some(ct);
            }
            tokio::time::sleep(self.config.retry_interval).await;
        }
        None
    }

    /// 检测可用连接(USB 优先,BLE 次之)
    async fn detect_connection(&self) -> Option<ConnectionType> {
        // 第1优先级:USB(阻塞枚举,spawn_blocking)
        #[cfg(feature = "usb")]
        if let Some(ct) = Self::detect_hid_connection_async().await {
            return Some(ct);
        }

        // 第2优先级:BLE
        #[cfg(feature = "ble")]
        {
            // 只自动重连明确记录过的目标。没有目标时等待用户扫描选择，
            // 避免多台设备环境下误连任意一个 REAI_VB_ 设备。
            // vendor_gatt_client 不在这里检查——它由 run_ble_session 通过 ensure 回调
            // 延迟创建，此处只看「用户是否要 BLE」（auto_connect + target）。
            let has_target = self.ble_last_device_name.lock().unwrap().is_some();
            if has_target && self.ble_auto_connect.load(Ordering::SeqCst) {
                return Some(ConnectionType::Ble);
            }
        }

        #[allow(unreachable_code)]
        None
    }

    // ================================================================
    // USB HID 检测(spawn_blocking 包装阻塞枚举)
    // ================================================================

    /// 异步包装 `detect_hid_connection`(hidapi 阻塞枚举放带 CFRunLoop 的线程)
    #[cfg(feature = "usb")]
    async fn detect_hid_connection_async() -> Option<ConnectionType> {
        spawn_blocking_with_runloop(Self::detect_hid_connection)
            .await
            .ok()
            .flatten()
    }

    /// 异步包装 [`Self::check_usb_preemption`]。
    ///
    /// 线程投递本身失败(闭包 panic)时保守返回 `NeedsFullCheck`,让调用方退回完整
    /// 检测,而不是当作「没插 USB」。
    #[cfg(feature = "usb")]
    async fn check_usb_preemption_async() -> BlePreemptionCheck {
        spawn_blocking_with_runloop(Self::check_usb_preemption)
            .await
            .unwrap_or(BlePreemptionCheck::NeedsFullCheck)
    }

    /// 阻塞:BLE 会话期间判断 USB 是否插入,**只枚举 HID,跳过 cpal 音频枚举**。
    ///
    /// 全程不枚举 CoreAudio/WASAPI；HID 不可用时返回
    /// [`BlePreemptionCheck::NeedsFullCheck`] 重试 HID 完整检测。
    #[cfg(feature = "usb")]
    fn check_usb_preemption() -> BlePreemptionCheck {
        let decision = with_reused_hid_api(|api| {
            let (has_usb_hid, has_unknown_hid) = scan_target_hid_buses(api);
            decide_ble_preemption(has_usb_hid, has_unknown_hid)
        });

        ble_preemption_from_hid(decision)
    }

    /// 阻塞:只枚举 hidapi HID 判定 USB 是否可连。Board-first detection must never
    /// enumerate or open OS audio endpoints; an explicit UAC compatibility request owns that.
    #[cfg(feature = "usb")]
    fn detect_hid_connection() -> Option<ConnectionType> {
        let hid_decision = with_reused_hid_api(|api| {
            let (has_usb_hid, has_unknown_hid) = scan_target_hid_buses(api);

            match decide_hid_detection(has_usb_hid, has_unknown_hid) {
                HidDetectionDecision::Connected => Some(ConnectionType::Usb),
                HidDetectionDecision::ProbeMode => match Self::probe_device_mode(api) {
                    Some(3) => Some(ConnectionType::Usb),
                    _ => None,
                },
                HidDetectionDecision::NotConnected => None,
            }
        });

        hid_decision.flatten()
    }

    // ================================================================
    // USB 会话
    // ================================================================

    /// 连接 + 启动 HID monitor(spawn_blocking 包装 hidapi 阻塞操作)。
    /// 返回 (HidMonitor, config_conn, paused) —— 后两者供 on_monitor_ready 回调。
    #[cfg(feature = "usb")]
    async fn connect_device_hid(
        &self,
    ) -> Result<(
        HidMonitor,
        Arc<Mutex<Option<DeviceConnection>>>,
        Arc<AtomicBool>,
    )> {
        let event_tx = self.event_tx.clone();
        // on_monitor_ready 回调由 connect 完成后在 async 侧调(不在 blocking 线程里调),
        // 所以这里只把 HidMonitor 返回,config_conn/paused 从 monitor 取。
        // 用 spawn_blocking_with_runloop：hidapi 的 IOKit 调用需要 CFRunLoop。
        let monitor = spawn_blocking_with_runloop(move || -> Result<HidMonitor> {
            let mut device_mgr = DeviceManager::new()?;
            device_mgr.refresh()?;

            let mut monitor = HidMonitor::new(event_tx);
            let monitor_config = MonitorConfig::default();

            let config_conn = device_mgr
                .connect_usage_page_and_usage(USAGE_PAGE_CONFIG, 0x0002)
                .or_else(|_| device_mgr.connect_usage_page(USAGE_PAGE_CONFIG));
            match config_conn {
                Ok(conn) => monitor.start_config_monitor(conn, monitor_config)?,
                Err(e) => {
                    log::warn!(target: "hid", "Config 接口连接失败: {},等待重试", e);
                    return Err(e);
                }
            }

            if let Ok(conn) = device_mgr.connect_usage_page(USAGE_PAGE_CONSUMER) {
                let _ = monitor.start_consumer_monitor(conn);
            }

            Ok(monitor)
        })
        .await
        .map_err(|e| anyhow::anyhow!("HID 线程崩溃: {:?}", e))??;

        let config_conn = monitor.config_conn();
        let paused = monitor.paused_arc();

        // 回调 + 连接成功事件在 async 侧调(broadcast send 同步,回调同步,无阻塞)
        if let Some(ref cb) = self.on_monitor_ready {
            cb(config_conn.clone(), paused.clone());
        }
        self.notify_connected(ConnectionType::Usb);
        log::info!(target: "hid", "设备已连接: USB");

        Ok((monitor, config_conn, paused))
    }

    #[cfg(feature = "usb")]
    async fn run_usb_session(&mut self) {
        self.emit_reconnect(ReconnectState::Connecting, None, None);

        let mut monitor = match self.connect_device_hid().await {
            Ok((m, _, _)) => m,
            Err(e) => {
                log::warn!(target: "hid", "USB 连接失败: {}", e);
                self.emit_error(format!("USB 连接失败: {e}"), true);
                self.notify_disconnected(DisconnectReason::DeviceGone);
                return;
            }
        };

        // USB 连接成功 → 读完整设备信息（fresh HidApi，带 CFRunLoop 的线程）。
        // 1) chip_id → 推导 BLE 名 REAI_VB_{chip_id} → set_ble_target（USB 拔出后精确连同一设备）
        // 2) 广播 BoardEvent::DeviceInfo（消费者据此展示，无需手动 read）
        // 读不到非致命，跳过即可，不影响连接主线。
        if let Some(info) = spawn_blocking_with_runloop(|| {
            HidApi::new()
                .ok()
                .and_then(|api| Self::probe_usb_device_info(&api))
        })
        .await
        .ok()
        .flatten()
        {
            #[cfg(feature = "ble")]
            {
                let ble_name = format!("REAI_VB_{}", info.chip_id);
                log::info!(target: "hotplug", "USB 读到 chip_id={} → BLE 目标 {}", info.chip_id, ble_name);
                self.set_ble_target(Some(&ble_name));
            }
            let _ = self.event_tx.send(BoardEvent::DeviceInfo(info));
        }

        let reason = self.monitor_hid_while_connected(&monitor).await;

        monitor.stop();
        log::info!(target: "hid", "USB 设备已断开: {:?}", reason);
        self.notify_disconnected(reason);
    }

    #[cfg(feature = "usb")]
    async fn monitor_hid_while_connected(&self, monitor: &HidMonitor) -> DisconnectReason {
        loop {
            if !self.running.load(Ordering::SeqCst) || self.is_stop_requested() {
                return DisconnectReason::UserAction;
            }
            if !monitor.is_running() {
                return DisconnectReason::DeviceGone;
            }

            tokio::time::sleep(self.config.check_interval).await;

            if Self::detect_hid_connection_async().await.is_none() {
                log::warn!(target: "hid", "定时探测:USB 设备已不在");
                return DisconnectReason::DeviceGone;
            }
        }
    }

    // ================================================================
    // BLE 会话
    // ================================================================

    #[cfg(feature = "ble")]
    async fn run_ble_session(&mut self) {
        // client 延迟创建：先看共享槽里有没有，没有则通过 ensure 回调建
        // （那一刻才建 adapter，触发 macOS 蓝牙授权弹窗）。
        let client = {
            let guard = self.vendor_gatt_client_slot.lock().unwrap();
            guard.clone()
        };
        let client = match client {
            Some(c) => c,
            None => {
                let ensure = match self.ensure_client.as_ref() {
                    Some(f) => f,
                    None => {
                        log::error!(target: "hotplug", "BLE 模式未设置 ensure_client 回调");
                        return;
                    }
                };
                match ensure().await {
                    Ok(c) => c,
                    Err(e) => {
                        log::warn!(target: "hotplug", "BLE 延迟创建 VendorGattClient 失败: {}", e);
                        // 建失败多半是蓝牙未授权/未开——等一会再让循环重试
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        return;
                    }
                }
            }
        };

        // 1. 扫描精确匹配已记录的 ble_last_device_name。
        let target = self.ble_last_device_name.lock().unwrap().clone();
        self.emit_reconnect(ReconnectState::Scanning, None, None);

        let (peripheral, found_name) = match client.scan_for_device(target.as_deref()).await {
            Ok((p, name)) => {
                log::info!(target: "hotplug", "扫描到 BLE 设备: {}", name);
                (p, name)
            }
            Err(e) => {
                log::warn!(target: "hotplug", "BLE 扫描未发现设备: {}", e);
                self.emit_error(format!("BLE 扫描未发现设备: {e}"), true);
                tokio::time::sleep(Duration::from_secs(5)).await;
                return;
            }
        };

        // 扫描期间用户可能主动断开/清除目标，或切换到另一台设备。
        // 在真正 connect 前重新确认，避免过期扫描结果造成一次意外回连。
        let current_target = self.ble_last_device_name.lock().unwrap().clone();
        if !self.ble_auto_connect.load(Ordering::SeqCst)
            || current_target.as_deref() != Some(found_name.as_str())
        {
            log::info!(target: "hotplug", "BLE 目标已取消或改变，丢弃扫描结果 {}", found_name);
            return;
        }
        *self.ble_last_device_name.lock().unwrap() = Some(found_name.clone());
        if let Some(ref cb) = self.on_ble_device_name_updated {
            cb(&found_name);
        }

        // 2. 连接
        self.emit_reconnect(ReconnectState::Connecting, None, None);
        if let Err(e) = client.connect(&peripheral).await {
            log::warn!(target: "hotplug", "BLE 连接失败: {},5s 后重试", e);
            self.emit_error(format!("BLE 连接失败: {e}"), true);
            tokio::time::sleep(Duration::from_secs(5)).await;
            return;
        }

        // 3. 建立连接状态并启动通知消费。工作模式保持未知，
        // 直到固件上报 CMD_STATUS/0xC9 或模式键事件。
        self.notify_connected(ConnectionType::Ble);
        client.start_notification_loop();

        // CMD 0x13 的 mode 是传输模式（USB=3 / BLE=2），不是工作模式。
        // 后续工作模式仅由 CMD_STATUS/0xC9 或模式键事件更新。
        log::info!(
            target: "hotplug",
            "BLE 已连接: {:?}",
            self.ble_last_device_name.lock().unwrap()
        );

        // 6. 监控循环(BLE 在线 + running;USB 抢占检测仅 usb feature)
        let vendor_running = client.running();
        #[cfg(feature = "usb")]
        let mut usb_detected = false;
        // 轻量检测只扫 HID bus，每隔约 FULL_CHECK_PERIOD 跑一次完整 HID 检测兜底，
        // 防御轻量路径未覆盖的退化场景。按 driver 的 500ms 周期算是每 10 次一回,
        // 九成检测省掉了音频枚举;SDK 默认的 5s 周期则退化成每次都完整检测(N=1)。
        #[cfg(feature = "usb")]
        let full_check_every = full_check_interval_ticks(self.config.check_interval);
        #[cfg(feature = "usb")]
        let mut ticks_since_full = 0u32;
        loop {
            if !self.running.load(Ordering::SeqCst) || self.is_stop_requested() {
                break;
            }
            if !vendor_running.load(Ordering::SeqCst) {
                break;
            }
            #[cfg(feature = "usb")]
            if usb_detected {
                break;
            }
            tokio::time::sleep(self.config.check_interval).await;
            #[cfg(feature = "usb")]
            {
                ticks_since_full += 1;
                let detected = if ticks_since_full >= full_check_every {
                    ticks_since_full = 0;
                    Self::detect_hid_connection_async().await == Some(ConnectionType::Usb)
                } else {
                    match Self::check_usb_preemption_async().await {
                        BlePreemptionCheck::Preempt => true,
                        BlePreemptionCheck::Keep => false,
                        // HID 不可用,音频是唯一线索:立即完整检测,并重置计数。
                        BlePreemptionCheck::NeedsFullCheck => {
                            ticks_since_full = 0;
                            Self::detect_hid_connection_async().await == Some(ConnectionType::Usb)
                        }
                    }
                };
                if detected {
                    usb_detected = true;
                    log::info!(target: "hotplug", "USB 已插入,退出 BLE 会话(保留设备名供重连)");
                }
            }
        }

        // 7. 断开(fire-and-forget)
        let _ = client.disconnect().await;

        // 8. 不改 ble_auto_connect —— GATT 断连保持 true 自动重连
        self.notify_disconnected(DisconnectReason::DeviceGone);
        log::info!(target: "hotplug", "BLE 设备已断开");
    }

    #[cfg(not(feature = "ble"))]
    async fn run_ble_session(&mut self) {
        log::warn!(target: "hotplug", "检测到 BLE 连接但 ble feature 未启用,跳过");
    }

    // ================================================================
    // CMD 0x13 探测设备模式
    // ================================================================

    #[cfg(feature = "usb")]
    fn probe_device_mode(api: &HidApi) -> Option<u8> {
        let config_devices: Vec<_> = api
            .device_list()
            .filter(|d| {
                d.vendor_id() == VID
                    && is_target_pid(d.product_id())
                    && d.usage_page() == USAGE_PAGE_CONFIG
            })
            .collect();

        let target = config_devices
            .iter()
            .find(|d| d.usage() == 0x0002)
            .or_else(|| config_devices.first())?;

        let device = api.open_path(target.path()).ok()?;

        let cmd = HidPacket::get_device_info();
        device.write(&cmd).ok()?;

        let mut buf = [0u8; 64];
        for attempt in 0..10 {
            match device.read_timeout(&mut buf, 200) {
                Ok(n) if n >= 5 && buf[1] == CMD_GET_DEVICE_INFO => {
                    let mode = buf[4];
                    log::debug!(target: "hid", "CMD 0x13 探测成功: mode={} (skip={})", mode, attempt);
                    return Some(mode);
                }
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
        None
    }

    /// CMD 0x13 探测完整设备信息（USB）。
    ///
    /// 比 [`probe_device_mode`] 多解析 chip_id 等字段，用于 USB 连接后：
    /// 1. 推导 BLE 设备名 `REAI_VB_{chip_id}` → [`set_ble_target`](Self::set_ble_target)
    /// 2. 广播 `BoardEvent::DeviceInfo`（消费者据此展示，无需手动 read）
    ///
    /// 读不到（非致命）→ 返回 None，调用方跳过推导/广播，不影响连接主线。
    #[cfg(feature = "usb")]
    fn probe_usb_device_info(api: &HidApi) -> Option<DeviceInfo> {
        let config_devices: Vec<_> = api
            .device_list()
            .filter(|d| {
                d.vendor_id() == VID
                    && is_target_pid(d.product_id())
                    && d.usage_page() == USAGE_PAGE_CONFIG
            })
            .collect();

        let target = config_devices
            .iter()
            .find(|d| d.usage() == 0x0002)
            .or_else(|| config_devices.first())?;

        let device = api.open_path(target.path()).ok()?;

        let cmd = HidPacket::get_device_info();
        device.write(&cmd).ok()?;

        let mut buf = [0u8; 64];
        for _ in 0..10 {
            match device.read_timeout(&mut buf, 200) {
                Ok(n) if n >= 5 && buf[1] == CMD_GET_DEVICE_INFO => break,
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
        // USB payload_offset = 4（Report ID + CMD + LEN + result）
        let info =
            crate::tool::parse::parse_device_info_from_buf(&buf, 4, ConnectionType::Usb).ok()?;
        log::debug!(
            target: "hid",
            "CMD 0x13 完整探测: chip_id={} fw={} battery={}%",
            info.chip_id,
            info.firmware_version,
            info.battery_level
        );
        Some(info)
    }

    // ================================================================
    // 事件通知
    // ================================================================

    /// 上报非致命错误事件(连接失败/扫描失败等),供只订阅事件的消费者捕获
    fn emit_error(&self, msg: impl std::fmt::Display, recoverable: bool) {
        let _ = self.event_tx.send(BoardEvent::Error(ErrorEvent {
            message: msg.to_string(),
            recoverable,
        }));
    }

    fn notify_connected(&self, conn_type: ConnectionType) {
        if let Some(ref cb) = self.on_connection_change {
            cb(Some(conn_type));
        }
        let _ = self.event_tx.send(BoardEvent::Connection(ConnectionEvent {
            connected: true,
            connection_type: Some(conn_type),
            reason: None,
        }));
        let _ = self.event_tx.send(BoardEvent::Reconnect(ReconnectEvent {
            state: ReconnectState::Connected,
            attempt: None,
            message: None,
        }));
    }

    fn notify_disconnected(&self, reason: DisconnectReason) {
        if let Some(ref cb) = self.on_connection_change {
            cb(None);
        }
        let _ = self.event_tx.send(BoardEvent::Connection(ConnectionEvent {
            connected: false,
            connection_type: None,
            reason: Some(reason),
        }));
    }

    fn emit_reconnect(&self, state: ReconnectState, attempt: Option<u32>, message: Option<String>) {
        let _ = self.event_tx.send(BoardEvent::Reconnect(ReconnectEvent {
            state,
            attempt,
            message,
        }));
    }
}

#[cfg(all(test, feature = "usb"))]
mod tests {
    use super::*;

    #[test]
    fn explicit_usb_hid_connects_without_waiting_for_audio() {
        assert_eq!(
            decide_hid_detection(true, false),
            HidDetectionDecision::Connected
        );
    }

    #[test]
    fn usb_detection_requires_hid_and_never_uses_audio_endpoint_signal() {
        assert_eq!(
            decide_hid_detection(false, false),
            HidDetectionDecision::NotConnected
        );
    }

    #[test]
    fn unknown_hid_bus_keeps_command_probe_fallback() {
        assert_eq!(
            decide_hid_detection(false, true),
            HidDetectionDecision::ProbeMode
        );
    }

    #[test]
    fn bluetooth_only_target_is_not_misclassified_as_usb() {
        assert_eq!(
            decide_hid_detection(false, false),
            HidDetectionDecision::NotConnected
        );
    }

    #[test]
    fn usb_hid_is_sufficient_without_any_audio_enumeration() {
        assert_eq!(
            decide_hid_detection(true, false),
            HidDetectionDecision::Connected
        );
    }

    // ===== BLE 抢占判定:比完整路径更敏感 =====

    #[test]
    fn unknown_bus_target_still_triggers_preemption() {
        // 完整路径对 Unknown 走 ProbeMode 再发命令探测,抢占路径直接算数。
        assert!(decide_ble_preemption(false, true));
        assert_eq!(
            decide_hid_detection(false, true),
            HidDetectionDecision::ProbeMode
        );
    }

    #[test]
    fn usb_bus_target_triggers_preemption() {
        assert!(decide_ble_preemption(true, false));
    }

    #[test]
    fn no_usb_side_target_keeps_the_ble_session() {
        // 只有蓝牙总线上的目标设备(就是当前这条 BLE 连接自己)不该触发抢占。
        assert!(!decide_ble_preemption(false, false));
    }

    // ===== 降级行为:HID 不可用不等于没插 USB =====

    #[test]
    fn hid_unavailable_falls_back_to_full_check() {
        assert_eq!(
            ble_preemption_from_hid(None),
            BlePreemptionCheck::NeedsFullCheck
        );
    }

    #[test]
    fn hid_available_maps_directly() {
        assert_eq!(
            ble_preemption_from_hid(Some(true)),
            BlePreemptionCheck::Preempt
        );
        assert_eq!(
            ble_preemption_from_hid(Some(false)),
            BlePreemptionCheck::Keep
        );
    }

    // ===== 完整检测的兜底周期 =====

    #[test]
    fn driver_interval_keeps_most_checks_lightweight() {
        // driver 覆盖的 500ms:每 10 次一回完整检测,九成走轻量路径。
        assert_eq!(full_check_interval_ticks(Duration::from_millis(500)), 10);
    }

    #[test]
    fn sdk_default_interval_always_runs_full_check() {
        // SDK 默认 5s:退化成每次都完整检测,与改动前行为一致。
        assert_eq!(full_check_interval_ticks(Duration::from_secs(5)), 1);
    }

    #[test]
    fn long_interval_never_disables_full_check() {
        // 再长的周期也不能退化成「永不完整检测」。
        assert_eq!(full_check_interval_ticks(Duration::from_secs(60)), 1);
    }

    #[test]
    fn tiny_interval_does_not_starve_the_full_check() {
        // 周期被配得极小时,按比例算会得出上千次才完整检测一回,等于架空音频兜底。
        // 上限把连续轻量次数收在 20 以内。
        assert_eq!(full_check_interval_ticks(Duration::from_millis(1)), 20);
        assert_eq!(full_check_interval_ticks(Duration::from_millis(0)), 20);
        assert_eq!(full_check_interval_ticks(Duration::from_millis(100)), 20);
    }
}
