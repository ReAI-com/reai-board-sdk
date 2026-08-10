//! BLE Vendor GATT 平台层(ble feature)。
//!
//! V2:`gatt_client` tokio 化重写(删 ble_run 同步包装,直接 await btleplug)。

pub mod gatt_client;
