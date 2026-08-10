# Changelog

All notable changes to `reai-board-sdk` are documented here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

- First public release as an independent crate. Brand metadata, product
  website, README in English & Chinese, MIT LICENSE, and CI workflow added.
- Documentation pass: device-command reference expanded to cover the full
  public API surface (bindings blob, DFU recovery, BLE connection
  management, app-online notification), platform-support table aligned
  with what CI actually verifies, and the macOS Bluetooth permission note
  corrected.
- Removed the unused `facade-blocking` feature flag — `BoardDeviceBlocking`
  never required it.
- **Licensing correction — the mSBC decoder moved to its own crate.**
  `kernel::msbc` was a bit-exact translation of FFmpeg's `libavcodec/sbcdec.c`
  and therefore a derivative work under LGPL-2.1-or-later, which conflicted
  with this crate's MIT licence. It now lives in the separate `msbc-decoder`
  crate (LGPL-2.1-or-later, with the original FFmpeg copyright holders
  credited) and is an **optional dependency enabled only by the `ble`
  feature**. Builds without `ble` contain no LGPL code at all.
  - `kernel::msbc` still resolves to the same API when `ble` is enabled, so
    existing imports keep working.
  - `kernel::sink::MsbcDecoderSink` and `tool::msbc_file` now require `ble`.
  - Consumers who need BLE audio without LGPL can supply their own decoder
    via `set_audio_frame_sink()`, which receives undecoded 57-byte frames.
  - **Note for publishing**: `msbc-decoder` must be published to crates.io
    before `reai-board-sdk`, since the latter depends on it by version.
  - **Semver note**: dropping `ble` now also drops `kernel::msbc`,
    `kernel::sink::MsbcDecoderSink` and `tool::msbc_file`. This is a
    deliberate narrowing of the API surface to keep the licence boundary
    enforceable at compile time.

## [0.2.2] — 2026-08-09

### Added

- **`test-mode`: factory physical-key test (firmware v1.58+)**
  - `set_factory_key_test(enable, session)` works over both USB Vendor HID
    and BLE Vendor GATT.
  - New types: `FactoryKeyControlAck`, `FactoryKeyControlResult`.
  - New event variant: `BoardEvent::FactoryKey(FactoryKeyEvent)`. The
    `input_index` field is the pre-mapping 0..=11 physical slot.
  - 15-second lease semantics — production tools should renew every 5
    seconds and explicitly release.

### Fixed

- USB HID hotplug detection: when the target HID reports `BusType::Usb`
  directly, skip the 10×200 ms probe and connect immediately. Only
  `BusType::Unknown` falls back to the CMD 0x13 probe.

## [0.2.1] — 2026-07-29

### Fixed

- **Consumer channel "dial-while-holding" mis-release** (two rounds of fixes).
  Root cause: USB HID Consumer Page is single-valued; a dial pulse would push
  the held key out of the stream and the following zero frame was interpreted
  as a global release.
  - USB side: introduced a "currently held keys" ledger in the monitor.
  - Kernel + BLE side: lifted the ledger into `ConsumerHeldTracker` so USB
    and BLE share one interpreter, fixed two additional BLE regressions
    (held AI voice key falsely released while dialing; CHAT dial-bounce
    false-triggered by a dial-tail frame), and emit release events for
    still-held keys on disconnect.

## [0.2.0] — 2026-07-21

### Changed

- **Code-review remediation across the SDK** (issues grouped as P0 / High /
  Medium / Low). All Rust type changes are backward-compatible at the public
  API surface; consumers depending on JSON event envelopes or `BoardEvent`
  match arms continue to work.
- `ModeChangeEvent` gained a `source` field so consumers can tell a
  hardware dial change from a polled/queried one.

## [0.1.0] — 2026-06-12

### Added

- Initial release. `reai-board-sdk` v0.1 library, full four-layer architecture
  (kernel / runtime / facade / tool layers), supporting USB HID + USB
  Audio capture and BLE Vendor GATT scan / connect / notify / mSBC
  decode.

[Unreleased]: https://github.com/ReAI-com/reai-board-sdk/compare/v0.2.2...HEAD
[0.2.2]: https://github.com/ReAI-com/reai-board-sdk/releases/tag/v0.2.2

<!-- 0.1.0 – 0.2.1 predate the public repository and have no git tags. -->
