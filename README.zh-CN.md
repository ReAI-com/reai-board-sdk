# reai-board-sdk

[![crates.io](https://img.shields.io/crates/v/reai-board-sdk.svg)](https://crates.io/crates/reai-board-sdk)
[![docs.rs](https://docs.rs/reai-board-sdk/badge.svg)](https://docs.rs/reai-board-sdk)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust 1.87+](https://img.shields.io/badge/rust-1.87%2B-orange.svg)](#rust-版本要求)

封装 **USB / BLE 连接、断线重连、HID 协议、mSBC 解码、USB Audio 采集**
能力的 Rust crate，可嵌入任意应用。面向 **ReAI-Vibe-Board** —— 一块为 AI
编程工作流设计的语音优先机械键盘。

[产品官网](https://b.reai.com) | [English README](README.md) | [API 文档 (docs.rs)](https://docs.rs/reai-board-sdk) | [更新日志](CHANGELOG.md)

[![ReAI-Vibe-Board](https://raw.githubusercontent.com/ReAI-com/reai-board-sdk/v0.3.0/assets/board-unibody.webp)](https://b.reai.com)

---

## 这块键盘

CNC 铝合金一体机身，带金属旋钮和三段式模式拨杆。SDK 实际能观察到的：

| 硬件 | SDK 里对应什么 |
|------|----------------|
| 旋钮（旋转 + 按下） | `KeyPressEvent` —— KEY0 / KEY1 编码器相位，KEY2 按压 |
| 6 个实体按键 | `KeyPressEvent` KEY3~KEY8；KEY6 是 AI 语音键，另有独立的 `AiVoiceKeyEvent` |
| 三段模式拨杆 | KEY9 / KEY10 / KEY11 → `ModeChangeEvent`（YOLO / PLAN / CHAT） |
| 麦克风 | `PcmSink` 投递 16 kHz mono f32 —— 板载 mSBC 走 USB 厂商 HID 或 BLE，Rust 内解码（可选功能） |
| USB-C / 蓝牙 | 两种通道，自动切换 |

上面 12 个 `key_index` 槽位覆盖了固件会上报的全部输入：旋钮占 3 个、
实体键占 6 个、拨杆占 3 个。

| | | |
|:-:|:-:|:-:|
| [![金属旋钮](https://raw.githubusercontent.com/ReAI-com/reai-board-sdk/v0.3.0/assets/board-knob.webp)](https://b.reai.com) | [![双麦阵列](https://raw.githubusercontent.com/ReAI-com/reai-board-sdk/v0.3.0/assets/board-mic.webp)](https://b.reai.com) | [![段落感按键](https://raw.githubusercontent.com/ReAI-com/reai-board-sdk/v0.3.0/assets/board-keys.webp)](https://b.reai.com) |
| 金属旋钮 | 双麦阵列 | 段落感按键 |

---

## 能力一览

- **统一 API**：USB / BLE 共用同一个 `BoardDevice`、同一套 `BoardEvent` 流、同一份音频回调。
- **热插拔 + 断线重连**：插上 USB 自动抢占 BLE 会话；拔 USB 后 BLE 自动恢复。无需胶水代码。
- **统一 16 kHz mono f32 PCM**：通过单一 `PcmSink::on_pcm` 回调投递 —— USB Audio 直采，BLE mSBC 由内置 Rust 解码器解码（**不依赖 ffmpeg**）。
- **类型化命令**：读/写按键配置、设备信息、绑定配置块、静默录音标志、软休眠超时、工作模式、工厂物理按键测试（固件 v1.58+）、厂商 USB-HID DFU OTA 升级**与救砖恢复**。
- **三个事件入口**：`events()` 拿 `EventStream`（`recv().await` 可进 `tokio::select!`，`blocking_recv()` 给普通线程用）、`on_event()`（回调）、`subscribe()`（直接拿原始 `broadcast::Receiver` 自己驱动）。

> 协议常量（USB VID/PID、BLE GATT service UUID、命令码、设备名前缀）针对
> **ReAI-Vibe-Board** 硬件调优，**不是通用 USB/BLE 抽象**。

---

## 快速开始

在 `Cargo.toml` 加：

```toml
[dependencies]
reai-board-sdk = "0.3"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time", "sync"] }
```

最小使用示例：

```rust
use std::sync::Arc;
use reai_board_sdk::{BoardConfig, BoardDevice, BoardEvent};
use reai_board_sdk::sink::PcmSink;

struct ConsoleSink;
impl PcmSink for ConsoleSink {
    fn on_pcm(&self, samples: &[f32]) {
        // 转发到 STT / 存盘 / 画波形 —— 随你
        let _ = samples;
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let device = BoardDevice::open(BoardConfig::default())?;

    // （可选）在 start() 前注册音频 sink
    device.set_pcm_sink(Arc::new(ConsoleSink));

    device.start().await?;  // 后台 spawn 热插拔 + 断线重连

    let mut events = device.events();
    while let Ok(Some(evt)) = events.recv().await {
        match evt {
            BoardEvent::Connection(c) => println!("connected={} type={:?}", c.connected, c.connection_type),
            BoardEvent::KeyPress(k)   => println!("key {} {} pressed={}", k.key_index, k.key_name, k.pressed),
            BoardEvent::AiVoiceKey(a) => println!("AI voice pressed={}", a.pressed),
            BoardEvent::ModeChange(m) => println!("mode {}", m.mode),
            _ => {}
        }
    }
    device.shutdown();
    Ok(())
}
```

---

## 支持的平台

| 系统      | 状态 | 备注                                                         |
|-----------|------|--------------------------------------------------------------|
| macOS     | CI 已验证 | `hidapi` 用 `macos-shared-device` feature                |
| Linux     | CI 可编译 | 需装 `libdbus-1-dev libudev-dev libasound2-dev pkg-config`；原生 HID 可能还要 `udev` 规则 |
| Windows   | 预期可用，**尚未验证** | 原生 HID 需要 WinUSB / Zadig 驱动             |

三种通道（USB HID / USB Audio / BLE GATT）对上述平台都已实现，差别只在 CI
验证到什么程度。Windows 目前没有 CI job，请当作「未测试」而非「不支持」。

`ble` 用 `btleplug 0.12`（原生 async）。macOS 上首次 `start()` 可能要等 ~40 秒
（CoreBluetooth adapter 预热），这是系统行为，不是 SDK 慢。

### 系统权限

SDK **不模拟、不注入键盘输入**，它只读设备上报并向设备发命令。因此：

- **不需要辅助功能 / 输入监控**：本设备按键走 vendor HID `0xFFA0` / consumer
  `0x000C`，不走 macOS 受保护的标准键盘 Usage `0x0007`。系统偶尔仍会提示，
  授权即可。
- **板载音频这条路不需要麦克风权限**：固件 v1.59 起，键盘自己的麦克风走厂商
  USB HID / BLE GATT，以普通设备数据的形式传过来，SDK 不会枚举或打开系统录音
  设备。只有 `start_usb_uac_compat()` 走系统音频栈，也只有它会触发麦克风授权弹窗。

**macOS 蓝牙会弹权限框**：创建 CoreBluetooth adapter 会触发系统授权弹窗，
所以 SDK 把 adapter 的创建推迟到真正要用 BLE 的时候 —— 弹窗出现在首次
`scan_ble_devices()` / BLE 连接时，而不是 `start()` 时。

另外注意，设备命令并非只读：`write_key_config()`、`set_sleep_timeout()`、
`shutdown_device()`、`start_dfu_upgrade()` 都会改变设备状态，见
[安全注意事项](#安全注意事项)。

---

## Features

| Feature            | 引入依赖                                              | 默认？ |
|--------------------|-------------------------------------------------------|--------|
| `usb`              | `hidapi 2.6`（USB HID）+ `cpal 0.15`（USB Audio 采集）+ `msbc-decoder` | ✅     |
| `ble`              | `btleplug 0.12`（BLE GATT）+ `futures-util` + `msbc-decoder` | ✅     |
| `test-mode`        | 工厂测试命令（如 `shutdown_device(0x5E)`）            | ❌     |

板载音频在每条传输上都是 mSBC，所以两个传输 feature 都会引入 `msbc-decoder`
（见「协议」一节）。`default-features = false` 只给你协议层。

`BoardDeviceBlocking` 不需要额外 feature —— 开了 `usb` 或 `ble` 就自带。

`test-mode` 默认不启用。只有可信的工厂或产测工具需要物理按键测试事件、设备关机
命令时，才应显式开启。

只用协议层、不要硬件依赖：

```toml
reai-board-sdk = { version = "0.3", default-features = false, features = ["test-mode"] }
```

### Rust 版本要求

`rust-version = "1.87"`（hot path 和 examples 里用了 `usize::is_multiple_of`）。

---

## 架构

```
                    BoardDevice（高级 API：open / start / subscribe）
                          │
            ┌─────────────┴─────────────┐
            ▼                           ▼
       HotplugManager              (USB) UsbAudioCapture
   （USB + BLE 自动连接 /              │ cpal UAC → PcmSink (f32)
    自动重连）                          │
            │
   ┌────────┼────────────────┐
   ▼        ▼                ▼
 HidMonitor  KeyStateAggregator  VendorGattClient
 （Config / Consumer 解析）  （BLE GATT：扫描 / 连接 / 通知）
   │                          │           │
   └──────── broadcast::Sender<BoardEvent> ┘
                    │
              消费者 subscribe()
```

**事件解耦**：内部所有模块通过同一个 `broadcast::Sender<BoardEvent>` 上报。
高频音频走独立的 `AudioFrameSink` / `PcmSink` trait，**不让 16 kHz PCM
淹没语义事件**（按键/模式）。

### 四层划分

| 层         | 模块路径                                                  | 职责                                                  |
|------------|-----------------------------------------------------------|-------------------------------------------------------|
| `kernel`   | `protocol_hid` / `protocol_gatt` / `event` / `sink` / `msbc` / `key_aggregator` / `types` / `error` | 纯逻辑（无线程、无 IO）                                |
| `runtime`  | `device` / `hotplug` / `usb` / `ble` / `usb_capture`      | tokio async 编排（生命周期 / 热插拔 / USB / BLE IO） |
| `facade`   | `device` / `events` / `blocking`                          | `BoardDevice` 高级入口、三个事件入口、同步命令桥        |
| `tool`     | `parse` / `msbc_file`                                     | 含 IO 的便利函数（解析 HID buffer、解码 mSBC 文件）    |

`runtime` 和 `facade` 至少要开 `usb` / `ble` 之一。两个都关时只有 `kernel`
和 `tool` 可用 —— 适合"只嵌入协议层、不要硬件依赖"的场景。

---

## 事件（`BoardEvent`）

单一 enum，一次 `match` 全覆盖：

```rust
pub enum BoardEvent {
    Connection(ConnectionEvent),   // 连接/断开（含原因）
    Reconnect(ReconnectEvent),     // 重连状态机变化
    KeyPress(KeyPressEvent),       // 单键按下/释放
    ComboKey(ComboKeyEvent),       // 组合键（同时 ≥2 键）
    AiVoiceKey(AiVoiceKeyEvent),   // 物理 AI 语音键（6 号键）
    ModeChange(ModeChangeEvent),   // 拨杆切换：YOLO / PLAN / CHAT
    DeviceInfo(DeviceInfo),        // 主动读或轮询的设备信息
    Error(ErrorEvent),             // 非致命错误（单条命令超时等）
    #[cfg(feature = "test-mode")]
    FactoryKey(FactoryKeyEvent),   // 工厂物理按键测试（固件 v1.58+）
}
```

每个变体都 `#[derive(Serialize)]`，带 `#[serde(tag = "type")]`，需要往
WebSocket 或跨进程转发时直接序列化成 JSON 信封即可。

---

## 音频

板载音频是**可选的**。整节跳过也不影响按键事件、拨杆、旋钮和全部设备命令；用户
照样有系统麦克风，也可以继续用他们本来就在用的第三方语音输入法。SDK 的其他部分
不依赖它。

### Sink

```rust
pub trait PcmSink: Send + Sync {
    fn on_pcm(&self, samples: &[f32]);                // 16 kHz mono f32
}

pub trait AudioFrameSink: Send + Sync {
    fn on_audio_frame(&self, frame: AudioFrame<'_>);  // 同样的 PCM，外加传输与连续性信息
}
```

`AudioFrame` 除了解码后的样本，还带着它从哪条传输来、连接世代、线上包序号，以及
三个互相独立的丢失信号（`device_discontinuity`、`sequence_gap_frames`、
`local_drop_frames`）。任一命中时 `frame.discontinuity()` 为真 —— 用它来重置你自己
的 VAD / 解码状态，别靠时间间隔去猜。

`CountingSink` 是内置的 `PcmSink` / `AudioFrameSink`，做帧数与字节统计。
`EncodedAudioDecoderSink` 是 SDK 挡在你的 sink 前面的解码器 —— 它把带版本的
mSBC 包变成 `AudioFrame`，本身并不实现 sink trait。`set_pcm_sink()` 接
`Arc<dyn PcmSink>`，**必须在 `start().await` 前调用**。

### 声音是从哪来的

固件 v1.59 起，键盘把自己的麦克风数据当作普通设备数据，走**厂商传输**（USB
Vendor HID 或 BLE GATT）发上来。全程不打开系统录音设备，因此不涉及麦克风权限：

```rust
use reai_board_sdk::kernel::audio::resolve_audio_transport;
use reai_board_sdk::{AudioRouteRequest, AudioStreamScope};

let caps = device.query_audio_capabilities().await?;          // 0x6E 能力位
let transport = resolve_audio_transport(
    AudioRouteRequest::BoardFirst,
    device.connection(),
    &caps,
).ok_or_else(|| anyhow::anyhow!("该固件没有可用的厂商音频传输"))?;

device.start_board_audio(transport, AudioStreamScope::Session, lease_id, ttl_ms).await?;
```

- `AudioRouteRequest::BoardFirst` 严格按设备上报的能力位解析。两条厂商传输都不
  可用时它会明确失败，**绝不悄悄退回主机麦克风**。
- 能力按**特性位**判定，不按「固件够不够新」判定。
- `start_board_audio()` 拿的是带 TTL 的租约（`Session` 或 `Timeline`），
  用 `control_audio_stream()` 配 `AudioStreamAction::Heartbeat` 续租。
- `start_usb_uac_compat()` 是给旧固件的兼容路径。它走系统音频栈，所以必须显式
  调用，也只有它会触发麦克风授权弹窗。

---

## 设备命令

`BoardDevice` 上所有命令方法都是 async；`BoardDeviceBlocking` 上是 sync。
按当前连接类型自动选 USB HID 或 BLE GATT 路径。

**设备信息与工作模式**

```rust
device.read_device_info().await?;   // CMD 0x13：mode / MAC / 固件 / 电量 / chip_id
device.get_work_mode().await?;      // CMD 0x12 + 0xC9 —— 读拨杆当前档位
```

**按键配置**

```rust
device.read_key_config().await?;          // CMD 0x15
device.write_key_config(&config).await?;  // CMD 0x16
```

**绑定配置块（bindings blob）** —— 存在键盘上的 4 KB 应用自定义配置区，
分片传输 + CRC16 校验。`BlobRead` 会区分「从未写入」（可静默首配）与
「写过但损坏」（必须上抛用户决策，**绝不静默覆盖**）；旧固件不认这两条命令，
返回 `Unsupported`，调用方据此降级即可。

```rust
device.read_bindings_blob().await?;          // CMD 0x69
device.write_bindings_blob(&payload).await?; // CMD 0x6A —— payload ≤ 3830 字节
```

**电源与休眠**

```rust
device.get_silent_record().await?;      // CMD 0x61（固件 v1.41+）
device.set_silent_record(true).await?;  // CMD 0x62 —— 返回最终生效值
device.get_sleep_timeout().await?;      // CMD 0x63（固件 v1.51+，未连接/已连接两组秒数）
device.set_sleep_timeout(SleepTimeout::new(120, 900)).await?;  // CMD 0x64
device.notify_app_online(true).await?;  // CMD 0x65（固件 v1.53+）
device.get_app_online().await?;         // CMD 0x66
device.get_open_url().await?;           // CMD 0x67
device.set_open_url("https://…").await?; // CMD 0x68
device.shutdown_device(true).await?;    // CMD 0x5E（仅 test-mode）
```

**BLE 连接管理**

```rust
device.scan_ble_devices(timeout).await?;  // 列出周围的设备
device.connect_ble("REAI_VB_XXXX");       // 指定连某一台
device.disconnect_ble().await?;
device.disconnect().await?;               // CMD 0x60 —— 请求设备主动断链
```

**固件升级与救砖** —— DFU 路径仅 USB 可用。

```rust
device.start_dfu_upgrade(path, |p| { /* 进度 */ }).await?;
device.cancel_dfu_upgrade();              // 一个 ≤250B 传输周期内终止

// 万一设备被撂在 DFU 模式（比如升级中途主机挂了）：
if device.is_stuck_in_dfu().await? {
    device.recover_from_dfu().await?;     // 踢回正常模式
}
```

`recover_from_dfu()` 全程只碰暂存分区，不触碰主应用分区 —— 对一台已经卡住的
设备来说，它不会让情况变得更糟。

**工厂物理按键测试**（`test-mode`，固件 v1.58+）—— 15 秒租约，
每 5 秒续租一次，用完显式释放。

```rust
device.set_factory_key_test(true, session).await?;  // CMD 0x6C；事件走 0x6D
```

---

## Examples

在 crate 根目录跑：

```sh
cargo run --example usb_probe        # USB HID + USB Audio
cargo run --example ble_probe        # BLE 扫描 + 连接 + 音频 + 按键
cargo run --example device_demo      # 读设备信息 / 按键配置 / 写回往返
cargo run --example listen_demo      # 两种事件门面并排演示
```

所有 example 都需要连上设备；事件打到 stdout。若还要输出工厂原始物理按键事件，
请额外带 `--features test-mode`。

---

## 安全注意事项

本 crate **不带任何鉴权或传输加密**。如果你要把设备命令暴露到网络上：

- 必须绑 `127.0.0.1`（或 Unix socket），不能绑 `0.0.0.0`；
- 远程访问要套反向代理 + TLS + token 鉴权；
- DFU 端点（`start_dfu_upgrade`）要限速 —— 它会**直接刷写设备固件，不可幂等**。

`shutdown_device()` 和 `start_dfu_upgrade()` 都是破坏性操作，没有二次确认。
**能调用它们的人 = 能控制设备**。

`write_key_config()` 和 `write_bindings_blob()` 不算破坏性，但它们**会持久化到键盘上**：
写错了重启、拔线都不会恢复，只能再写一份正确数据回去。写前先读，
且回读发现 blob 损坏时应当交给用户决策，而不是当作可以直接覆盖的信号。

---

## 贡献

Issue 和 PR 欢迎发到
[github.com/ReAI-com/reai-board-sdk](https://github.com/ReAI-com/reai-board-sdk)。
产品本身见 [b.reai.com](https://b.reai.com)。

本地开发循环：

```sh
cargo build --all-features
cargo test  --all-features
cargo clippy --all-features --all-targets -- -D warnings
cargo doc   --no-deps --all-features
```

---

## 协议

**本 crate 是 MIT** —— 见 [LICENSE](LICENSE)。Copyright (c) 2026 ReAI Team。

有一点请在发版前读一下：

`ble` feature 会引入本仓库中的独立 crate [`msbc-decoder`](msbc-decoder/)，
用于解码 BLE 传来的 mSBC 音频。那个解码器是 FFmpeg `libavcodec/sbcdec.c` 的
bit-exact 翻译，属于衍生作品，因此沿用 FFmpeg 的协议按
**LGPL-2.1-or-later** 分发，而不是 MIT。

把它拆成独立 crate 就是为了让这条边界一目了然：

| 你的构建 | 是否编入 mSBC 解码器 | 实际生效的协议 |
|----------|---------------------|----------------|
| `default-features = false`（只要协议层：按键、命令、升级） | 否 | MIT |
| `features = ["usb"]` —— USB HID + 板载音频 | **是** | MIT + LGPL-2.1-or-later |
| `features = ["ble"]` 或默认 | **是** | MIT + LGPL-2.1-or-later |

键盘的麦克风在**每条传输**上都是 mSBC，所以 `usb` 和 `ble` 都会引入这个解码器。
真正不含 LGPL 的构建是只要协议层那一种。

这个取舍比看上去小，因为**板载音频是可选功能，不是依赖**。关掉它，键盘的其他
能力一样不少：按键映射、拨杆、旋钮、设备配置、固件升级。用户的系统麦克风照常
可用，也可以继续搭配他们已经在用的任何语音输入法 —— 哪怕和你的产品同时用。
什么都不会坏。
