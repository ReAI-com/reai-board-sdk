//! Dedicated USB Vendor HID audio reader.
//!
//! The audio interface owns a separate hidapi handle and two persistent threads. It never
//! shares the Config monitor handle or its PauseGuard. A bounded drop-oldest queue keeps HID
//! reads non-blocking; local queue loss is reported separately from device sequence gaps.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{anyhow, Result};

use crate::kernel::protocol_hid::{
    is_target_pid, parse_usb_audio_report, PACKET_SIZE, PID_USB, USAGE_PAGE_AUDIO, VID,
};
use crate::kernel::sink::{AudioFrameSink, EncodedAudioDecoderSink};
use crate::runtime::hotplug::spawn_blocking_with_runloop;

const QUEUE_CAPACITY: usize = 64;

#[derive(Default)]
struct QueueState {
    reports: VecDeque<QueuedReport>,
}

struct QueuedReport {
    report: [u8; PACKET_SIZE],
    /// Number of reports dropped immediately before this report in sequence order.
    local_drop_packets: u64,
}

type SharedQueue = Arc<(Mutex<QueueState>, Condvar)>;

pub struct UsbVendorAudioReader {
    running: Arc<AtomicBool>,
    queue: SharedQueue,
    reader: Mutex<Option<JoinHandle<()>>>,
    decoder: Mutex<Option<JoinHandle<()>>>,
}

impl UsbVendorAudioReader {
    pub async fn start(sink: Arc<dyn AudioFrameSink>, connection_epoch: u64) -> Result<Self> {
        // hidapi construction/open stays on the established macOS HID RunLoop, matching the
        // Config monitor. The returned handle is then owned exclusively by the audio thread.
        let device = spawn_blocking_with_runloop(move || -> Result<hidapi::HidDevice> {
            let api = hidapi::HidApi::new()?;
            let info = api
                .device_list()
                .find(|device| {
                    device.vendor_id() == VID
                        && is_target_pid(device.product_id())
                        && device.usage_page() == USAGE_PAGE_AUDIO
                })
                .ok_or_else(|| {
                    anyhow!("未找到 USB Vendor Audio HID 接口 (VID={VID:#06x}, PID={PID_USB:#06x})")
                })?;
            let device = api.open_path(info.path())?;
            device.set_blocking_mode(false)?;
            Ok(device)
        })
        .await
        .map_err(|panic| anyhow!("USB Audio HID 初始化线程崩溃: {panic:?}"))??;

        let running = Arc::new(AtomicBool::new(true));
        let queue: SharedQueue = Arc::new((
            Mutex::new(QueueState {
                reports: VecDeque::with_capacity(QUEUE_CAPACITY),
            }),
            Condvar::new(),
        ));

        let reader_running = running.clone();
        let reader_queue = queue.clone();
        let reader = thread::Builder::new()
            .name("board-usb-audio-hid".into())
            .spawn(move || {
                // 短包按次数限速，设备持续吐非 64 字节时不至于把日志刷爆。
                let mut short_reads: u64 = 0;
                while reader_running.load(Ordering::SeqCst) {
                    let mut report = [0u8; PACKET_SIZE];
                    match device.read_timeout(&mut report, 20) {
                        Ok(PACKET_SIZE) => {
                            let (lock, ready) = &*reader_queue;
                            let mut queue = lock.lock().unwrap_or_else(|e| e.into_inner());
                            if queue.reports.len() == QUEUE_CAPACITY {
                                if let Some(dropped) = queue.reports.pop_front() {
                                    if let Some(next) = queue.reports.front_mut() {
                                        next.local_drop_packets = next
                                            .local_drop_packets
                                            .saturating_add(dropped.local_drop_packets)
                                            .saturating_add(1);
                                    }
                                }
                            }
                            queue.reports.push_back(QueuedReport {
                                report,
                                local_drop_packets: 0,
                            });
                            ready.notify_one();
                        }
                        Ok(0) => thread::yield_now(),
                        Ok(length) => {
                            short_reads = short_reads.saturating_add(1);
                            if short_reads <= 3 || short_reads.is_multiple_of(100) {
                                log::warn!(
                                    target: "audio",
                                    "USB Audio HID 短包(累计 {short_reads}): {length}/{PACKET_SIZE}"
                                );
                            }
                        }
                        Err(error) => {
                            if reader_running.load(Ordering::SeqCst) {
                                log::warn!(target: "audio", "USB Audio HID 读取失败: {error}");
                            }
                            break;
                        }
                    }
                }
                reader_running.store(false, Ordering::SeqCst);
                reader_queue.1.notify_all();
            })?;

        let decoder_running = running.clone();
        let decoder_queue = queue.clone();
        let decoder = match thread::Builder::new()
            .name("board-usb-audio-decode".into())
            .spawn(move || {
                let decoder = EncodedAudioDecoderSink::new(sink, connection_epoch);
                // 解析失败按包频刷日志毫无用处，按次数限速：头几条给现场，之后每 100 条一条。
                let mut parse_failures: u64 = 0;
                while decoder_running.load(Ordering::SeqCst) {
                    let queued = {
                        let (lock, ready) = &*decoder_queue;
                        let mut queue = lock.lock().unwrap_or_else(|e| e.into_inner());
                        while queue.reports.is_empty() && decoder_running.load(Ordering::SeqCst) {
                            let waited = ready
                                .wait_timeout(queue, Duration::from_millis(250))
                                .unwrap_or_else(|e| e.into_inner());
                            queue = waited.0;
                        }
                        let Some(queued) = queue.reports.pop_front() else {
                            continue;
                        };
                        queued
                    };
                    match parse_usb_audio_report(&queued.report) {
                        Some(packet) => decoder.on_packet(packet, queued.local_drop_packets),
                        None => {
                            parse_failures = parse_failures.saturating_add(1);
                            if parse_failures <= 3 || parse_failures.is_multiple_of(100) {
                                log::warn!(
                                    target: "audio",
                                    "USB Audio HID 解析失败(累计 {parse_failures}): 信封版本/标志位/负载长度不合协议"
                                );
                            }
                        }
                    }
                }
            }) {
            Ok(decoder) => decoder,
            Err(error) => {
                running.store(false, Ordering::SeqCst);
                queue.1.notify_all();
                let _ = reader.join();
                return Err(error.into());
            }
        };

        // A reader can fail between its spawn and decoder initialization. Treat that as a
        // failed start instead of publishing a dead route as active.
        if !running.load(Ordering::SeqCst) {
            let _ = reader.join();
            let _ = decoder.join();
            return Err(anyhow!("USB Audio HID reader 在启动阶段退出"));
        }

        Ok(Self {
            running,
            queue,
            reader: Mutex::new(Some(reader)),
            decoder: Mutex::new(Some(decoder)),
        })
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        self.queue.1.notify_all();
        if let Some(handle) = self.reader.lock().unwrap().take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.decoder.lock().unwrap().take() {
            let _ = handle.join();
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

impl Drop for UsbVendorAudioReader {
    fn drop(&mut self) {
        self.stop();
    }
}
