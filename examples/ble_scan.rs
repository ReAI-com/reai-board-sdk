//! BLE 诊断:扫描 15 秒,列出周围**所有**可发现的 peripherals(名字可能为空)。
//!
//! 用于排查"连不上板子":
//! - 什么都扫不到 → 本进程没有蓝牙权限(macOS 会静默拒绝,不弹窗)或适配器异常
//! - 能扫到别的设备但没有 REAI_VB_* → 板子没在广播
//!
//! ```sh
//! cargo run --release --features ble --example ble_scan
//! ```

use btleplug::api::{Central as _, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::Manager;
use std::collections::BTreeSet;
use std::time::Duration;

#[tokio::main]
async fn main() {
    let manager = Manager::new().await.expect("Manager::new");
    let adapters = manager.adapters().await.expect("adapters");
    let adapter = adapters.into_iter().next().expect("no BLE adapter");

    println!("开始扫描 15s(首次使用可能需要蓝牙权限)...");
    adapter
        .start_scan(ScanFilter::default())
        .await
        .expect("start_scan 失败——大概率是 macOS 蓝牙权限未授予");

    let mut named: BTreeSet<String> = BTreeSet::new();
    let mut anonymous = 0usize;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        for p in adapter.peripherals().await.expect("peripherals") {
            let name = p
                .properties()
                .await
                .ok()
                .flatten()
                .and_then(|pr| pr.local_name)
                .unwrap_or_default();
            if name.is_empty() {
                anonymous += 1;
            } else if named.insert(name.clone()) {
                if name.starts_with("REAI_VB_") {
                    println!("  {}  <== 找到 ReAI 板子!", name);
                }
            }
        }
    }
    adapter.stop_scan().await.ok();

    println!("扫描结束,共 {} 个具名设备(+ {} 个无名广播):", named.len(), anonymous);
    for name in &named {
        println!("  {}", name);
    }
}
