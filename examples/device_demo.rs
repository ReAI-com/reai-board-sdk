//! Full command-interaction demo: wait for connection → `read_device_info` →
//! `read_key_config` → `write_key_config` (write back to verify round-trip).
//!
//! Quick start:
//! ```sh
//! cargo run --example device_demo
//! ```
//!
//! Commands are async. This demo uses the `BoardDeviceBlocking` bridge
//! inside `#[tokio::main]` to keep a sync style; events come through
//! `events().blocking_recv()`. The demo does NOT auto-shutdown the device
//! (that's dangerous); call `shutdown_device(keep_pair)` manually if you
//! want to test the power-off path.
//!
//! 简体中文：
//! 完整命令交互验证：等连接 → `read_device_info` → `read_key_config` →
//! `write_key_config`（写回原值验证往返）。
//! 不自动关机（危险）；关机测试用 `shutdown_device(keep_pair)` 自行调。

use reai_board_sdk::{BoardConfig, BoardDevice, BoardDeviceBlocking, BoardEvent, EventStreamError};

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    let device = BoardDevice::open(BoardConfig::default()).expect("open 失败");
    // blocking 桥必须在 tokio 上下文构造(Handle::current 捕获)
    let dev = BoardDeviceBlocking::new(device);
    dev.start().expect("start 失败");

    println!("等待设备连接...");
    let mut events = dev.events();

    // 等连接(USB 或 BLE)
    let conn_type = loop {
        match events.blocking_recv() {
            Ok(Some(BoardEvent::Connection(c))) if c.connected => break c.connection_type,
            Ok(Some(_)) => continue,
            Ok(None) => {
                eprintln!("事件流关闭");
                return;
            }
            Err(EventStreamError::Lagged(n)) => eprintln!("[溢出 {}]", n),
        }
    };
    println!("✓ 已连接: {:?}\n", conn_type);

    // 1. 读设备信息(CMD 0x13)
    match dev.read_device_info() {
        Ok(info) => println!(
            "[设备信息] fw={} rv={} mac={} battery={}%(chg={} full={}) chip={} mode={}",
            info.firmware_version,
            info.receiver_version,
            info.mac_address,
            info.battery_level,
            info.battery_charging,
            info.battery_full,
            info.chip_id,
            info.mode
        ),
        Err(e) => eprintln!("[设备信息] 读取失败: {}", e),
    }

    // 2. 读按键配置 → 写回原值(验证往返一致)
    match dev.read_key_config() {
        Ok(cfg) => {
            println!("[按键配置] 读取成功({} 键)", cfg.keys.len());
            match dev.write_key_config(&cfg) {
                Ok(()) => println!("[按键配置] 写回成功(往返一致 ✓)"),
                Err(e) => eprintln!("[按键配置] 写回失败: {}", e),
            }
        }
        Err(e) => eprintln!("[按键配置] 读取失败: {}", e),
    }

    println!("\n命令交互验证完成。继续监听按键,Ctrl+C 退出。");

    // 继续打印按键/模式事件
    loop {
        match events.blocking_recv() {
            Ok(Some(BoardEvent::KeyPress(k))) => {
                println!(
                    "[按键] {} {} pressed={}",
                    k.key_index, k.key_name, k.pressed
                )
            }
            Ok(Some(BoardEvent::ModeChange(m))) => {
                println!("[模式] {} (0x{:02X})", m.mode, m.mode_value)
            }
            Ok(Some(BoardEvent::Connection(c))) => {
                if c.connected {
                    println!("[重连] {:?}", c.connection_type)
                } else {
                    println!("[断开] reason={:?},等待自动重连...", c.reason)
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(e) => eprintln!("[事件错误] {:?}", e),
        }
    }
    // dev(Drop)→ device(Drop) 会兜底 shutdown
}
