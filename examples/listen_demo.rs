//! Demonstrates two event facades side-by-side: `events()` (async stream
//! style) and `on_event` (callback).
//!
//! Quick start:
//! ```sh
//! cargo run --example listen_demo
//! ```
//!
//! Both flavors share the same underlying `broadcast` channel — no double
//! delivery, no extra cost.
//!
//! 简体中文：
//! 演示两层事件门面：同时用 `events()`（async Stream 风格）和 `on_event`（回调）。
//! 验证：两种门面都能收到事件（共用单一 broadcast 内核，不双写）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use reai_board_sdk::{BoardConfig, BoardDevice, BoardEvent};

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    let device = BoardDevice::open(BoardConfig::default()).expect("open 失败");
    device.start().await.expect("start 失败");

    // 门面 2:on_event 回调 —— 统计收到的按键数(演示回调门面)
    let cb_count = Arc::new(AtomicU64::new(0));
    let cb_count_for_cb = cb_count.clone();
    let _handle = device.on_event(move |evt| {
        if matches!(evt, BoardEvent::KeyPress(_)) {
            cb_count_for_cb.fetch_add(1, Ordering::SeqCst);
        }
    });

    // 门面 1:events() async —— 主循环打印事件
    let mut events = device.events();
    println!("监听事件中(Ctrl+C 退出)。on_event 回调同时在统计按键数。");
    let mut stream_count: u64 = 0;
    loop {
        match events.recv().await {
            Ok(Some(evt)) => {
                stream_count += 1;
                match &evt {
                    BoardEvent::Connection(c) => println!(
                        "[events] connected={} type={:?} reason={:?}",
                        c.connected, c.connection_type, c.reason
                    ),
                    BoardEvent::KeyPress(k) => println!(
                        "[events] key {} {} pressed={}",
                        k.key_index, k.key_name, k.pressed
                    ),
                    BoardEvent::ModeChange(m) => {
                        println!("[events] mode {} (0x{:02X})", m.mode, m.mode_value)
                    }
                    BoardEvent::Reconnect(r) => {
                        println!("[events] reconnect {:?}", r.state)
                    }
                    _ => {}
                }
                // 每 10 个事件汇报一次两个门面的计数(应相等)
                if stream_count.is_multiple_of(10) {
                    println!(
                        "  → events() 已收 {} 条,on_event 回调已收按键 {} 次",
                        stream_count,
                        cb_count.load(Ordering::SeqCst)
                    );
                }
            }
            Ok(None) => break,
            Err(e) => eprintln!("[events 错误] {:?}", e),
        }
    }
    device.shutdown();
}
