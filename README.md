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

[![ReAI-Vibe-Board](https://raw.githubusercontent.com/ReAI-com/reai-board-sdk/v0.3.0/assets/board-unibody.webp)](https://b.reai.com)

---

## The hardware

A CNC aluminium unibody board with a metal knob and a three-way mode lever.
What the SDK can actually observe:

| Hardware | What you get from the SDK |
|----------|---------------------------|
| Knob (rotate + press) | `KeyPressEvent` — KEY0 / KEY1 encoder phases, KEY2 press |
| 6 physical keys | `KeyPressEvent` KEY3–KEY8; KEY6 is the AI-voice key and also emits `AiVoiceKeyEvent` |
| Three-way mode lever | KEY9 / KEY10 / KEY11 → `ModeChangeEvent` (YOLO / PLAN / CHAT) |
| Microphone | `PcmSink` delivers 16 kHz mono f32 — mSBC over vendor USB HID or BLE, decoded in-Rust (optional feature) |
| USB-C / Bluetooth | Both transports, switched automatically |

The twelve `key_index` slots above cover every input the firmware reports:
three for the knob, six for the keys, three for the lever.

| | | |
|:-:|:-:|:-:|
| [![Knob](https://raw.githubusercontent.com/ReAI-com/reai-board-sdk/v0.3.0/assets/board-knob.webp)](https://b.reai.com) | [![Dual mic](https://raw.githubusercontent.com/ReAI-com/reai-board-sdk/v0.3.0/assets/board-mic.webp)](https://b.reai.com) | [![Keys](https://raw.githubusercontent.com/ReAI-com/reai-board-sdk/v0.3.0/assets/board-keys.webp)](https://b.reai.com) |
| Metal knob | Dual-mic array | Tactile keys |

---

## What it gives you

- **One unified API** for USB and BLE transports — same `BoardDevice`, same
  `BoardEvent` stream, same audio callbacks.
- **Auto hotplug + reconnect** — plug in USB, it takes over BLE; pull USB, BLE
  resumes. No glue code to write.
- **16 kHz mono f32 PCM** delivered through a single `PcmSink::on_pcm` trait —
  the board streams mSBC over vendor USB HID or BLE and the SDK decodes it
  in-Rust (no ffmpeg dependency), without opening an OS audio device. Entirely
  optional: skip it and the system microphone still works as it always did.
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
- **No microphone permission** on the board-audio path. From firmware v1.59 the
  board's own mic arrives over vendor USB HID / BLE GATT as ordinary device data,
  so the SDK does not enumerate or open a CoreAudio / WASAPI input device.
  `start_usb_uac_compat()` is the one route that goes through the OS audio stack
  and therefore the one route that can raise the microphone prompt.

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
| `usb`              | `hidapi 2.6` (USB HID) + `cpal 0.15` (USB Audio capture) + `msbc-decoder` | ✅       |
| `ble`              | `btleplug 0.12` (BLE GATT) + `futures-util` + `msbc-decoder` | ✅       |
| `test-mode`        | Factory test commands (e.g. `shutdown_device(0x5E)`)     | ❌       |

Board audio is mSBC on every transport, so both transport features pull in the
`msbc-decoder` crate — see [License](#license). `default-features = false`
gives you the protocol layer alone.

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

## Audio

Board audio is **optional**. Ignore this whole section and you still get key
events, the mode lever, the knob and every device command — plus the ordinary
system microphone, or whatever third-party dictation tool your users already
run. Nothing else in the SDK depends on it.

### Sinks

```rust
pub trait PcmSink: Send + Sync {
    fn on_pcm(&self, samples: &[f32]);                // 16 kHz mono f32
}

pub trait AudioFrameSink: Send + Sync {
    fn on_audio_frame(&self, frame: AudioFrame<'_>);  // same PCM + transport & continuity metadata
}
```

`AudioFrame` carries the decoded samples together with the transport they
arrived on, a connection epoch, the on-wire packet sequence and three
independent loss signals (`device_discontinuity`, `sequence_gap_frames`,
`local_drop_frames`). `frame.discontinuity()` is true when any of them fired —
use it to reset your own VAD / decoder state instead of guessing from timing.

`CountingSink` is a built-in `PcmSink` / `AudioFrameSink` for frame and byte
statistics. `EncodedAudioDecoderSink` is the decoder the SDK puts in front of
your sink — it turns versioned mSBC packets into `AudioFrame`s, it does not
implement the sink traits itself. `set_pcm_sink()` accepts `Arc<dyn PcmSink>`;
call it before `start().await`.

### Where the audio comes from

From firmware v1.59 the board streams its own mic over the **vendor transports**
— USB Vendor HID or BLE GATT — as ordinary device data. No OS audio device is
opened, so no microphone permission is involved:

```rust
use reai_board_sdk::kernel::audio::resolve_audio_transport;
use reai_board_sdk::{AudioRouteRequest, AudioStreamScope};

let caps = device.query_audio_capabilities().await?;          // 0x6E feature bits
let transport = resolve_audio_transport(
    AudioRouteRequest::BoardFirst,
    device.connection(),
    &caps,
).ok_or_else(|| anyhow::anyhow!("no vendor audio transport on this firmware"))?;

device.start_board_audio(transport, AudioStreamScope::Session, lease_id, ttl_ms).await?;
```

- `AudioRouteRequest::BoardFirst` resolves strictly against the capability bits
  the device reported. If neither vendor transport is available it fails loudly —
  it never silently falls back to a host microphone.
- Capabilities are read as **feature bits**, not as "firmware is new enough".
- `start_board_audio()` takes a lease (`Session` or `Timeline`) with a TTL;
  `control_audio_stream()` renews it with `AudioStreamAction::Heartbeat`.
- `start_usb_uac_compat()` is the compatibility route for older firmware. It
  goes through the OS audio stack, so it is explicit, opt-in, and the only route
  that can raise the microphone prompt.

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
| `default-features = false` (protocol only — keys, commands, DFU) | no | MIT |
| `features = ["usb"]` — USB HID + board audio | **yes** | MIT + LGPL-2.1-or-later |
| `features = ["ble"]` or default | **yes** | MIT + LGPL-2.1-or-later |

The board sends its mic as mSBC on **every** transport, so both `usb` and `ble`
pull the decoder in. The LGPL-free build is the protocol layer on its own.

That is a smaller trade-off than it looks, because **board audio is an optional
feature, not a dependency**. Turn it off and the keyboard still does everything
else: key mapping, the mode lever, the knob, device configuration, DFU. Your
users keep their system microphone and can pair the board with any dictation
tool they already use — including alongside your product. Nothing breaks.
