//! HID 设备管理器
//!
//! 负责 HID 设备的枚举、连接、读写操作。

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait};
use hidapi::{DeviceInfo, HidApi, HidDevice};

use crate::kernel::protocol_hid::*;
use crate::kernel::types::{is_usb_audio_device_name, ConnectionType};

/// 设备连接(封装 HidDevice)
pub struct DeviceConnection {
    device: HidDevice,
    pub usage_page: u16,
    pub usage: u16,
    #[allow(dead_code)]
    pub interface: i32,
}

impl DeviceConnection {
    pub fn new(device: HidDevice, usage_page: u16, usage: u16, interface: i32) -> Self {
        Self {
            device,
            usage_page,
            usage,
            interface,
        }
    }

    /// 设置阻塞/非阻塞模式
    pub fn set_nonblocking(&self, nonblock: bool) -> Result<()> {
        self.device.set_blocking_mode(!nonblock)?;
        Ok(())
    }

    /// 读取数据(带超时 ms)
    pub fn read(&self, buf: &mut [u8], timeout_ms: i32) -> Result<usize> {
        let len = self.device.read_timeout(buf, timeout_ms)?;
        Ok(len)
    }

    /// 写入数据
    pub fn write(&self, data: &[u8]) -> Result<usize> {
        let len = self.device.write(data)?;
        Ok(len)
    }
}

/// HID 设备管理器
pub struct DeviceManager {
    hid_api: HidApi,
    devices: HashMap<u16, DeviceConnection>,
}

impl DeviceManager {
    /// 创建新的设备管理器
    pub fn new() -> Result<Self> {
        let hid_api = HidApi::new()?;
        log::debug!(target: "hid", "HID API 已初始化");
        Ok(Self {
            hid_api,
            devices: HashMap::new(),
        })
    }

    /// 刷新设备列表
    pub fn refresh(&mut self) -> Result<()> {
        self.hid_api.refresh_devices()?;
        Ok(())
    }

    /// 枚举所有匹配 VID/PID 的设备
    pub fn enumerate_devices(&self) -> Vec<&DeviceInfo> {
        self.hid_api
            .device_list()
            .filter(|d| d.vendor_id() == VID && is_target_pid(d.product_id()))
            .collect()
    }

    /// 检测设备是否物理存在(不依赖已打开的连接)
    #[allow(dead_code)]
    pub fn is_device_present(&self) -> bool {
        self.hid_api
            .device_list()
            .any(|d| d.vendor_id() == VID && is_target_pid(d.product_id()))
    }

    /// 检测连接类型:USB(有 USB Audio)或 BLE
    #[allow(dead_code)]
    pub fn detect_connection_type(&self) -> Option<ConnectionType> {
        // 优先级 1: USB — 通过 cpal 检测 USB Audio 设备
        let host = cpal::default_host();
        if let Ok(devices) = host.input_devices() {
            for device in devices {
                if let Ok(config) = device.default_input_config() {
                    if config.sample_rate().0 == 16000 && config.channels() == 1 {
                        let name = device.name().unwrap_or_default();
                        if is_usb_audio_device_name(&name) {
                            log::debug!(target: "hid", "检测到 USB Audio: {},连接类型: USB", name);
                            return Some(ConnectionType::Usb);
                        }
                    }
                }
            }
        }

        // 优先级 2: BLE — 通过 HID 枚举检测
        let ble_present = self
            .hid_api
            .device_list()
            .any(|d| d.vendor_id() == VID && d.product_id() == PID_BLE);
        if ble_present {
            log::debug!(target: "hid", "检测到 BLE HID 设备,连接类型: BLE");
            return Some(ConnectionType::Ble);
        }

        log::debug!(target: "hid", "未检测到任何设备");
        None
    }

    /// 连接指定 Usage Page + Usage 的接口(用于 BLE 设备精确选择)
    pub fn connect_usage_page_and_usage(
        &mut self,
        usage_page: u16,
        usage: u16,
    ) -> Result<DeviceConnection> {
        let dev_info = self
            .enumerate_devices()
            .into_iter()
            .find(|d| d.usage_page() == usage_page && d.usage() == usage)
            .ok_or_else(|| {
                anyhow!(
                    "未找到 Usage Page=0x{:04X}, Usage=0x{:04X} 的接口",
                    usage_page,
                    usage
                )
            })?;

        let device = self
            .hid_api
            .open_path(dev_info.path())
            .map_err(|e| anyhow!("打开接口失败 (0x{:04X}:0x{:04X}): {}", usage_page, usage, e))?;

        device.set_blocking_mode(false).ok();

        let conn = DeviceConnection::new(
            device,
            dev_info.usage_page(),
            dev_info.usage(),
            dev_info.interface_number(),
        );

        log::debug!(
            target: "hid",
            "✅ 已连接接口: Usage Page=0x{:04X}, Usage=0x{:04X}",
            conn.usage_page,
            conn.usage
        );

        Ok(conn)
    }

    /// 连接指定 Usage Page 的接口
    pub fn connect_usage_page(&mut self, usage_page: u16) -> Result<DeviceConnection> {
        let dev_info = self
            .enumerate_devices()
            .into_iter()
            .find(|d| d.usage_page() == usage_page)
            .ok_or_else(|| anyhow!("未找到 Usage Page=0x{:04X} 的接口", usage_page))?;

        let device = self
            .hid_api
            .open_path(dev_info.path())
            .map_err(|e| anyhow!("打开接口失败 (0x{:04X}): {}", usage_page, e))?;

        device.set_blocking_mode(false).ok(); // 非阻塞

        let conn = DeviceConnection::new(
            device,
            dev_info.usage_page(),
            dev_info.usage(),
            dev_info.interface_number(),
        );

        log::debug!(
            target: "hid",
            "✅ 已连接接口: Usage Page=0x{:04X}, Usage=0x{:02X}",
            conn.usage_page,
            conn.usage
        );

        Ok(conn)
    }

    /// 连接所有接口
    pub fn connect_all(&mut self) -> Result<Vec<(u16, u16)>> {
        // 先收集设备信息,避免借用冲突
        let dev_infos: Vec<_> = self
            .enumerate_devices()
            .into_iter()
            .map(|d| {
                (
                    d.usage_page(),
                    d.usage(),
                    d.interface_number(),
                    d.path().to_owned(),
                )
            })
            .collect();

        let mut connected = Vec::new();
        log::debug!(target: "hid", "找到 {} 个 HID 接口", dev_infos.len());

        for (usage_page, usage, interface, path) in dev_infos {
            match self.hid_api.open_path(&path) {
                Ok(device) => {
                    device.set_blocking_mode(false).ok();

                    log::debug!(
                        target: "hid",
                        "  Interface {}: Usage Page=0x{:04X}, Usage=0x{:02X}",
                        interface,
                        usage_page,
                        usage
                    );

                    let conn = DeviceConnection::new(device, usage_page, usage, interface);
                    self.devices.insert(usage_page, conn);
                    connected.push((usage_page, usage));
                }
                Err(e) => {
                    log::warn!(target: "hid", "  打开失败: {}", e);
                }
            }
        }

        if connected.is_empty() {
            return Err(anyhow!(
                "未找到 VID=0x{:04X}, PID=0x{:04X} 的 HID 设备",
                VID,
                PID
            ));
        }

        log::debug!(target: "hid", "✅ 成功连接 {} 个 HID 接口", connected.len());
        Ok(connected)
    }

    /// 获取指定 Usage Page 的设备连接
    #[allow(dead_code)]
    pub fn get_device(&self, usage_page: u16) -> Option<&DeviceConnection> {
        self.devices.get(&usage_page)
    }

    /// 获取可变引用
    #[allow(dead_code)]
    pub fn get_device_mut(&mut self, usage_page: u16) -> Option<&mut DeviceConnection> {
        self.devices.get_mut(&usage_page)
    }

    /// 断开所有设备
    pub fn disconnect_all(&mut self) {
        self.devices.clear();
        log::debug!(target: "hid", "📴 所有设备已断开");
    }

    /// 检查是否已连接
    #[allow(dead_code)]
    pub fn is_connected(&self) -> bool {
        !self.devices.is_empty()
    }
}
