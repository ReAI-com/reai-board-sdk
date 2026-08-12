# reai-board-sdk

[![crates.io](https://img.shields.io/crates/v/reai-board-sdk.svg)](https://crates.io/crates/reai-board-sdk)
[![docs.rs](https://docs.rs/reai-board-sdk/badge.svg)](https://docs.rs/reai-board-sdk)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust 1.87+](https://img.shields.io/badge/rust-1.87%2B-orange.svg)](#rust-version)

An embeddable Rust crate that encapsulates **USB / BLE connectivity,
auto-reconnect, the HID protocol, mSBC decoding, and USB Audio capture** for
the **ReAI-Vibe-Board** — a voice-first mechanical keyboard built for AI
coding workflows.

[Product site](https://b.reai.com) | [中文文档](README.zh-CN.md) | [API docs (docs.rs)](https://docs.rs/reai-board-sdk) | [Changelog](CHANGELOG.md)

![ReAI-Vibe-Board](https://raw.githubusercontent.com/ReAI-com/reai-board-sdk/v0.3.0/assets/board-unibody.webp)

---

## The hardware

A CNC aluminium unibody board with a metal knob and a three-way mode lever.
What the SDK can actually observe:

| Hardware | What you get from the SDK |
|----------|---------------------------|
| Knob (rotate + press) | `KeyPressEvent` — KEY0 / KEY1 encoder phases, KEY2 press |
| 6 physical keys | `KeyPressEvent` KEY3–KEY8; KEY6 is the AI-voice key and also emits `AiVoiceKeyEvent` |
| Three-way mode lever | KEY9 / KEY10 / KEY11 → `ModeChangeEvent` (YOLO / PLAN / CHAT) |
| Microphone | `PcmSink` delivers 16 kHz mono f32 — USB Audio captured directly, BLE mSBC decoded first |
| USB-C / Bluetooth | Both transports, switched automatically |

The twelve `key_index` slots above cover every input the firmware reports:
three for the knob, six for the keys, three for the lever.

| | | |
|:-:|:-:|:-:|
| ![Knob](https://raw.githubusercontent.com/ReAI-com/reai-board-sdk/v0.3.0/assets/board-knob.webp) | ![Dual mic](https://raw.githubusercontent.com/ReAI-com/reai-board-sdk/v0.3.0/assets/board-mic.webp) | ![Keys](https://raw.githubusercontent.com/ReAI-com/reai-board-sdk/v0.3.0/assets/board-keys.webp) |
| Metal knob | Dual-mic array | Tactile keys |

---

## What it gives you

- **One unified API** for USB and BLE transports — same `BoardDevice`, same
  `BoardEvent` stream, same audio callbacks.
- **Auto hotplug + reconnect** — plug in USB, it takes over BLE; pull USB, BLE
  resumes. No glue code to write.
- **16 kHz mono f32 PCM** delivered through a single `PcmSink::on_pcm` trait —
  USB Audio is captured directly, BLE mSBC frames are decoded in-Rust (no
  ffmpeg dependency).
- **Typed device commands** — read/write key config, device info, bindings
  blob, silent-record flag, sleep timeout, work mode, factory physical-key
  test (firmware v1.58+), plus a vendor USB-HID DFU path for OTA upgrade
  *and recovery*.
- **Three ways in for events** — `events()` returns an `EventStream`
  (`recv().await` for `tokio::select!`, `blocking_recv()` for plain threads),
  `on_event()` is callback style, and `subscribe()` hands you the raw
  `broadcast::Receiver` if you want to drive it yourself.

> The protocol constants (USB VID/PID, BLE GATT service UUIDs, command opcodes,
> device-name prefix) are tuned for **ReAI-Vibe-Board** hardware. They are not
> generic USB/BLE abstractions.

---

## Quick start

Add to your `Cargo.toml`:

```toml
[dependencies]
reai-board-sdk = "0.3"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time", "sync"] }
```

Minimal usage:

```rust
use std::sync::Arc;
use reai_board_sdk::{BoardConfig, BoardDevice, BoardEvent};
use reai_board_sdk::sink::PcmSink;

struct ConsoleSink;
impl PcmSink for ConsoleSink {
    fn on_pcm(&self, samples: &[f32]) {
        // forward to STT / save to disk / draw a waveform — your choice
        let _ = samples;
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let device = BoardDevice::open(BoardConfig::default())?;

    // (Optional) register an audio sink before start()
    device.set_pcm_sink(Arc::new(ConsoleSink));

    device.start().await?;  // spawns hotplug + auto-reconnect task

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

## Supported platforms

| OS      | Status | Notes                                                          |
|---------|--------|----------------------------------------------------------------|
| macOS   | Verified in CI | `hidapi` uses `macos-shared-device`                    |
| Linux   | Builds in CI   | Needs `libdbus-1-dev libudev-dev libasound2-dev pkg-config`; `udev` rules may be needed for raw HID |
| Windows | Expected to work, **not yet verified** | WinUSB / Zadig driver for raw HID access |

All three transports (USB HID, USB Audio, BLE GATT) are implemented for every
platform above — the difference is only how much of it CI proves. Windows has
no CI job yet; treat it as untested rather than unsupported.

`ble` uses `btleplug` 0.12 (native async). First `start()` may take ~40 s on
macOS while CoreBluetooth warms up its adapter — this is the OS, not the SDK.

### OS permissions

The SDK **never simulates or injects keyboard input** — it reads from the
device and sends it commands. That has two consequences:

- **No Accessibility / Input Monitoring required** in principle. The board's keys
  flow over vendor HID `0xFFA0` / consumer `0x000C`, not standard keyboard
  Usage `0x0007`. macOS may still prompt — authorize if it does.
- **No microphone permission** is required; USB Audio capture here is device→host
  only (no host mic).

**macOS Bluetooth does prompt.** Creating the CoreBluetooth adapter triggers
the system authorization dialog, so the SDK defers adapter creation until BLE
is actually needed — you will see the prompt on the first
`scan_ble_devices()` / BLE connect, not at `start()`.

Note that device commands are not read-only: `write_key_config()`,
`set_sleep_timeout()`, `shutdown_device()` and `start_dfu_upgrade()` all change
device state. See [Security notes](#security-notes).

---

## Features

| Feature            | What it pulls in                                         | Default? |
|--------------------|----------------------------------------------------------|----------|
| `usb`              | `hidapi 2.6` (USB HID) + `cpal 0.15` (USB Audio capture) | ✅       |
| `ble`              | `btleplug 0.12` (BLE GATT) + `futures-util`              | ✅       |
| `test-mode`        | Factory test commands (e.g. `shutdown_device(0x5E)`)     | ❌       |

`BoardDeviceBlocking` needs no feature flag — it ships with `usb` or `ble`.

`test-mode` is opt-in. Enable it only for trusted factory or production-test
tools that need physical-key test events or device shutdown commands.

To use only the protocol layer with no hardware deps:

```toml
reai-board-sdk = { version = "0.3", default-features = false, features = ["test-mode"] }
```

### Rust version

`rust-version = "1.87"` (uses `usize::is_multiple_of` in a couple of hot-path code sites and examples).

---

## Architecture

```
                    BoardDevice (high-level API: open / start / subscribe)
                          │
            ┌─────────────┴─────────────┐
            ▼                           ▼
       HotplugManager              (USB) UsbAudioCapture
   (USB + BLE auto-connect /           │ cpal UAC → PcmSink (f32)
    auto-reconnect)                    │
            │
   ┌────────┼────────────────┐
   ▼        ▼                ▼
 HidMonitor  KeyStateAggregator  VendorGattClient
 (Config / Consumer parser)     (BLE GATT: scan / connect / notify)
   │                          │           │
   └──────── broadcast::Sender<BoardEvent> ┘
                    │
              Consumers subscribe()
```

**Event decoupling**: every internal module reports through one
`broadcast::Sender<BoardEvent>`. The high-frequency audio stream uses a
separate `AudioFrameSink` / `PcmSink` trait so semantic events are never
drowned out by 16 kHz PCM frames.

### Four-layer split

| Layer       | Module path                          | Purpose                                                                 |
|-------------|--------------------------------------|-------------------------------------------------------------------------|
| `kernel`    | `protocol_hid` / `protocol_gatt` / `event` / `sink` / `msbc` / `key_aggregator` / `types` / `error` | Pure logic. No threads, no I/O.                                         |
| `runtime`   | `device` / `hotplug` / `usb` / `ble` / `usb_capture` | tokio async orchestration (lifecycle / hotplug / USB / BLE I/O). |
| `facade`    | `device` / `events` / `blocking`     | `BoardDevice` high-level entry point, the three event entry points, and the sync command bridge. |
| `tool`      | `parse` / `msbc_file`                | I/O-aware helpers (parse device info from HID buffer, decode mSBC file). |

`runtime` and `facade` require at least one of `usb` / `ble`. With both off,
only `kernel` and `tool` are available — useful for embedding the protocol
layer without any hardware dependency.

---

## Events (`BoardEvent`)

A single enum — one `match` covers everything:

```rust
pub enum BoardEvent {
    Connection(ConnectionEvent),   // connect / disconnect (with reason)
    Reconnect(ReconnectEvent),     // reconnect-state machine changes
    KeyPress(KeyPressEvent),       // single key down / up
    ComboKey(ComboKeyEvent),       // multi-key combo (≥2 keys held simultaneously)
    AiVoiceKey(AiVoiceKeyEvent),   // physical "AI voice" key (key #6)
    ModeChange(ModeChangeEvent),   // dial switch: YOLO / PLAN / CHAT
    DeviceInfo(DeviceInfo),        // info read on demand or polled
    Error(ErrorEvent),             // non-fatal (e.g. single-command timeout)
    #[cfg(feature = "test-mode")]
    FactoryKey(FactoryKeyEvent),   // factory physical-key test event (firmware v1.58+)
}
```

Each variant is `#[derive(Serialize)]` with `#[serde(tag = "type")]` so it
serializes naturally to a JSON envelope if you need to forward events to a
WebSocket bridge or another process.

---

## Audio sinks

```rust
pub trait PcmSink: Send + Sync {
    fn on_pcm(&self, samples: &[f32]);     // 16 kHz mono f32 — unified across USB and BLE
}

pub trait AudioFrameSink: Send + Sync {
    fn on_msbc_frame(&self, frame: &[u8]); // raw 57-byte mSBC frames (BLE only, before decoding)
}
```

- USB Audio is captured directly via cpal and forwarded to `PcmSink`.
- BLE mSBC frames are decoded to f32 by the built-in `MsbcDecoderSink` and
  forwarded to the **same** `PcmSink` you registered.

Built-in sinks: `MsbcDecoderSink` (mSBC → f32), `CountingSink` (frame / byte
statistics). `set_pcm_sink()` accepts `Arc<dyn PcmSink>`; call it before
`start().await`.

---

## Device commands

All command methods are `async` on `BoardDevice` and sync on
`BoardDeviceBlocking`. They auto-pick USB HID or BLE GATT transport based on
the current connection.

**Device info & work mode**

```rust
device.read_device_info().await?;   // CMD 0x13: mode / MAC / firmware / battery / chip_id
device.get_work_mode().await?;      // CMD 0x12 + 0xC9 — reads the lever's current position
```

**Key configuration**

```rust
device.read_key_config().await?;          // CMD 0x15
device.write_key_config(&config).await?;  // CMD 0x16
```

**Bindings blob** — a 4 KB application-defined config block persisted on the
keyboard, transferred in fragments with a CRC16 check. `BlobRead` distinguishes
*never written* (safe to initialize silently) from *written but corrupt* (raise
it to the user — never overwrite blindly), and reports `Unsupported` on older
firmware that does not answer these commands.

```rust
device.read_bindings_blob().await?;         // CMD 0x69
device.write_bindings_blob(&payload).await?; // CMD 0x6A — payload ≤ 3830 bytes
```

**Power & sleep**

```rust
device.get_silent_record().await?;      // CMD 0x61 (firmware v1.41+)
device.set_silent_record(true).await?;  // CMD 0x62 — returns the effective value
device.get_sleep_timeout().await?;      // CMD 0x63 (firmware v1.51+, idle / connected seconds)
device.set_sleep_timeout(SleepTimeout::new(120, 900)).await?;  // CMD 0x64
device.notify_app_online(true).await?;  // CMD 0x65 (firmware v1.53+)
device.get_app_online().await?;         // CMD 0x66
device.get_open_url().await?;           // CMD 0x67
device.set_open_url("https://…").await?; // CMD 0x68
device.shutdown_device(true).await?;    // CMD 0x5E (test-mode only)
```

**BLE connection management**

```rust
device.scan_ble_devices(timeout).await?;  // list nearby boards
device.connect_ble("REAI_VB_XXXX");       // target a specific one
device.disconnect_ble().await?;
device.disconnect().await?;               // CMD 0x60 — ask the device to drop the link
```

**Firmware upgrade & recovery** — the DFU path is USB-only.

```rust
device.start_dfu_upgrade(path, |p| { /* progress */ }).await?;
device.cancel_dfu_upgrade();              // aborts within one ≤250 B transfer cycle

// If a board is left stranded in DFU mode (e.g. the host died mid-upgrade):
if device.is_stuck_in_dfu().await? {
    device.recover_from_dfu().await?;     // kicks it back to normal mode
}
```

`recover_from_dfu()` only touches the staging partition, never the main
application partition — it cannot brick a device that is already stuck.

**Factory physical-key test** (`test-mode`, firmware v1.58+) — a 15-second
lease; renew every 5 s and release explicitly.

```rust
device.set_factory_key_test(true, session).await?;  // CMD 0x6C; events arrive as 0x6D
```

---

## Examples

Run from the crate root:

```sh
cargo run --example usb_probe        # USB HID + USB Audio
cargo run --example ble_probe        # BLE scan + connect + audio + keys
cargo run --example device_demo      # read device info / key config / round-trip write
cargo run --example listen_demo      # both event facade flavors side-by-side
```

All examples need a board connected; they print events to stdout. Add
`--features test-mode` when you also need factory physical-key events.

---

## Security notes

This crate has **no built-in authentication or transport-layer encryption**. If
you build a service that exposes the device commands over a network, you are
responsible for:

- binding only to `127.0.0.1` (or a Unix socket) on the host that owns the
  hardware;
- putting a reverse proxy with TLS + token authentication in front of any
  remote surface;
- rate-limiting the DFU endpoint (`start_dfu_upgrade` will reflash the device —
  it is **not** idempotent).

`shutdown_device()` and `start_dfu_upgrade()` are destructive. There is no
second-factor prompt — whoever can call them owns the device.

`write_key_config()` and `write_bindings_blob()` are not destructive but they
**persist on the keyboard**: a bad write survives reboots and unplugging, and
recovering means writing known-good data back. Read before you write, and treat
a `BlobRead` that comes back corrupt as a prompt for the user rather than a
licence to overwrite.

---

## Contributing

Issues and PRs are welcome at
[github.com/ReAI-com/reai-board-sdk](https://github.com/ReAI-com/reai-board-sdk).
For the product itself, see [b.reai.com](https://b.reai.com).

Local development loop:

```sh
cargo build --all-features
cargo test  --all-features
cargo clippy --all-features --all-targets -- -D warnings
cargo doc   --no-deps --all-features
```

---

## License

**This crate is MIT** — see [LICENSE](LICENSE). Copyright (c) 2026 ReAI Team.

One caveat worth reading before you ship:

The `ble` feature pulls in [`msbc-decoder`](msbc-decoder/), a separate crate in
this repository that decodes the mSBC audio arriving over BLE. That decoder is
a bit-exact translation of FFmpeg's `libavcodec/sbcdec.c`, so it inherits
FFmpeg's licence and is distributed under **LGPL-2.1-or-later**, not MIT.

It lives in its own crate precisely so the boundary is explicit:

| Your build | mSBC decoder compiled in? | Effective licence |
|------------|---------------------------|-------------------|
| `default-features = false` (protocol only) | no | MIT |
| `features = ["usb"]` — USB HID + USB Audio | no | MIT |
| `features = ["ble"]` or default | **yes** | MIT + LGPL-2.1-or-later |

If you only talk to the board over USB, no LGPL code reaches your binary. If
you need BLE audio and LGPL is a problem for your product, talk to your legal
team, or supply your own mSBC decoder through the `AudioFrameSink` trait —
`set_audio_frame_sink()` hands you the raw 57-byte frames before any decoding
happens.
