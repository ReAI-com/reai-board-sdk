# Changelog

All notable changes to `reai-board-sdk` are documented here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [0.3.2] — 2026-09-08

### Added

- **`virtual-mic` feature (macOS, opt-in): the BLE-connected board can now
  surface as a system-level microphone.** Ships a bundled CoreAudio HAL plugin
  (`virtual-mic/`, built by `build.rs` via cmake, vendored
  [libASPL](https://github.com/gavv/libASPL) v3.1.2 under MIT) that registers
  a 16 kHz mono input device "ReAI-Vibe-Board", plus a `VirtualMic` `PcmSink`
  that pumps decoded board PCM into it over loopback UDP. One-time admin
  install via `VirtualMic::ensure_installed()` (copy bundle into
  `/Library/Audio/Plug-Ins/HAL/` + restart coreaudiod). Field-tested end to end
  on macOS 26 (BLE mSBC → decoder → virtual mic → app recording). Apps reading
  the device follow normal macOS microphone-permission rules — bare
  `AVAudioEngine` readers get silence instead of a prompt, documented as a
  known trap. The feature is off by default and pulls in no new dependencies
  (cmake only needed to build it).
- **Startup switch**: `BoardConfig { virtual_mic: VirtualMicConfig { enabled,
  auto_install } }` — the SDK manages the virtual-mic lifecycle (lazy start on
  the first audio link, parallel delivery alongside any user `PcmSink`,
  degrade-to-warn on failure). Manual `VirtualMic::start()` +
  `set_pcm_sink()` stays as the advanced path.
- Added `examples/virtual_mic_demo.rs` — scan → explicit `connect_ble()` (the
  auto-reconnect path intentionally never connects by name prefix alone) →
  pump BLE audio into the virtual microphone.
- Added `examples/ble_scan.rs` — unfiltered 15 s advertising scan listing every
  visible peripheral, to tell "board not advertising" and "no Bluetooth
  permission" apart when a connection fails.

## [0.3.1] — 2026-09-03

### Added

- UAC now pairs with the dual-mic stereo firmware (v1.72+, `[L,R]` interleaved
  16 kHz/2ch output). Explicit cpal config selection still prefers 16 kHz/1ch/F32
  but falls back to 2ch before the `default_input_config()` escape hatch, so
  stereo-firmware boards no longer live on the macOS cpal config-cache trap.
  The `PcmSink` 16 kHz mono contract is unchanged for all consumers, and legacy
  mono-firmware behavior is locked in by regression tests (1ch preference over
  2ch, F32-only matching, `default_input_config` fallback for I16-only devices).
- Added opt-in raw stereo capture: `UsbAudioCapture::new_with_stereo_wav` (and
  `BoardDevice::set_uac_stereo_wav_path` / blocking twin) dumps the pre-downmix
  interleaved samples to a PCM16 WAV on `stop()`. Samples buffer in memory
  (~128 KB/s stereo @ 16 kHz), keeping file I/O off the cpal realtime callback —
  not intended for very long recordings. Default behavior (`UsbAudioCapture::new`)
  writes nothing.
- Added `examples/uac_capture.rs`, the open-source counterpart of the internal
  `reai-vb audio --uac` command: level metering plus optional `--out` (downmixed
  16 kHz mono WAV) and `--stereo-out` (raw interleaved WAV) flags; with no flags
  it saves nothing.
- Added `flutter/reai_board_sdk`, a Flutter BLE package for iOS and Android.
  It mirrors the Rust `BoardDevice` command/event semantics over the firmware's
  FE60–FE63 Vendor GATT service, includes typed reconnect/MTU/capability state,
  and exposes versioned or legacy raw mSBC frames without bundling the LGPL
  decoder into the MIT Flutter package.
- Added shared Rust/Dart protocol golden vectors, a runnable mobile example,
  platform permission templates, and Flutter CI coverage.

### Fixed

- USB and BLE now share one Consumer mode-switch tracker. Endpoint usages
  `0x0F0A` / `0x0F0B` select YOLO / PLAN, while releasing the active endpoint
  selects the contactless middle CHAT position. Both transports emit the same
  `BoardEvent::ModeChange(ModeSource::Dial)` contract.
- Removed transport-specific mode inference and avoided continuous BLE work-mode
  polling. BLE keeps one best-effort connection initialization query; subsequent
  lever changes are driven by firmware events and do not occupy the GATT command
  channel or interfere with audio heartbeats.
- Added shared-state, USB, and BLE endpoint press/release regression coverage.
- USB commands now reuse one command-only HID connection for the lifetime of a
  physical USB connection instead of opening and closing `HidApi` / `HidDevice`
  for every heartbeat. The command handle remains separate from the config
  monitor and audio reader, complete write/read transactions stay serialized,
  and the cached handle is invalidated on reconnect, I/O failure, or DFU entry.
  Failed commands are not replayed automatically.
- Added regression coverage for connection reuse, reconnect epochs, I/O and
  open failures, serialized concurrent transactions, and DFU invalidation.

## [0.3.0] — 2026-08-13

### Added

- **Board-first versioned audio transport (firmware v1.59+).** The board's own
  microphone now streams over the vendor transports — USB Vendor HID or BLE
  GATT — as ordinary device data. No OS audio device is opened, so the default
  audio path no longer involves a microphone permission prompt.
  - `query_audio_capabilities()` (0x6E) reports capabilities as **feature bits**.
    Nothing is inferred from "the firmware looks new enough".
  - `start_board_audio()` / `control_audio_stream()` (0x6F) take a `Session` or
    `Timeline` lease with a TTL, renewed with `AudioStreamAction::Heartbeat`.
  - `AudioRouteRequest::BoardFirst` resolves strictly against those bits and
    fails loudly when no vendor transport is available. It never silently falls
    back to a host microphone.
  - `AudioFrame` unifies both transports on 16 kHz mono f32 and carries the
    transport, connection epoch, on-wire sequence, and three independent loss
    signals (device discontinuity, sequence gap, local drop).
  - The USB vendor-audio reader owns a separate `hidapi` handle plus dedicated
    reader/decode threads and a bounded drop-oldest queue, so it is unaffected
    by the config monitor's pause guard.
  - BLE FE63 accepts both the v1 sequence envelope and the older session-only
    envelope.

### Changed

- **Breaking:** `AudioFrameSink::on_msbc_frame(&[u8])` became
  `on_audio_frame(AudioFrame<'_>)`. The sink now receives decoded PCM together
  with transport and continuity metadata rather than raw 57-byte mSBC frames,
  and `MsbcDecoderSink` is now `EncodedAudioDecoderSink`.
- **Breaking:** connecting, probing, or registering a `PcmSink` no longer
  enumerates or opens a CoreAudio / WASAPI input device. The USB Audio Class
  path is now reachable only through the explicit `start_usb_uac_compat()`.
- **Breaking — BLE audio no longer starts on its own.** In 0.2.x, connecting over
  BLE and registering a sink was enough to receive audio. The GATT audio stream
  now starts disabled and is opened only by `start_board_audio_reader()` (or
  `start_legacy_ble_session_reader()` for pre-v1.59 firmware). Upgrading without
  adding that call means the sink is simply never invoked — audio goes silent
  with no error. Migration:

  ```rust
  // 0.2.x — audio began flowing on its own
  device.set_pcm_sink(sink);
  device.start().await?;

  // 0.3.0 — the stream is requested explicitly
  device.set_pcm_sink(sink);
  device.start().await?;
  let caps = device.query_audio_capabilities().await?;
  let transport = reai_board_sdk::kernel::audio::resolve_audio_transport(
      AudioRouteRequest::BoardFirst,
      device.connection(),
      &caps,
  ).expect("no vendor audio transport");
  device.start_board_audio(transport, AudioStreamScope::Session, lease_id, ttl_ms).await?;
  ```
- **Licensing:** board audio is mSBC on every transport, so the `usb` feature
  pulls in the LGPL-2.1-or-later `msbc-decoder` exactly like `ble` does. The
  LGPL-free build is now `default-features = false` (protocol layer only). Board
  audio remains an entirely optional feature — without it the keyboard still
  provides key mapping, the mode lever, the knob, device configuration and DFU,
  and users keep their system microphone and any dictation tool they already use.
- Examples now depend on tokio's `signal` feature through dev-dependencies, so
  `cargo run --example …` builds without adding the signal driver to the library.
- CI builds every advertised feature configuration (protocol-only, `usb`, `ble`),
  which the default/all-features jobs did not cover.

### Fixed

- Commands issued while board audio is streaming no longer read an audio packet
  instead of their response. The config and audio collections share one physical
  HID interface, so opening either one delivers every report on that interface;
  reading a single report after a write picked up a `0xB1` audio packet as soon
  as the stream was running. Stopping a session reported an invalid response and
  left the lease to expire on its own, and any command sent mid-recording could
  fail the same way.
- A device-announced discontinuity now resets the sequence tracker. Without it, a
  firmware-side encoder restart inside a live lease left every following packet
  looking out-of-order, so audio went silent — potentially for tens of thousands
  of packets — with nothing in the log.
- One undecodable frame no longer discards the good frames beside it in the same
  packet. Legacy BLE envelopes can truncate their payload, which used to turn
  every truncated packet into a total loss — worse than the pre-0.3.0 behaviour.
- Frames lost to decoding are now reported through `local_drop_frames`. They used
  to vanish with all three loss signals clear, leaving consumers no reason to
  reset their own VAD state.
- `BoardDeviceBlocking` can now start board audio. It forwarded the sink setters
  but none of the start methods, so a registered sink was never invoked and there
  was no way out within that API.
- Switching transports validates before it acts. Requesting an unsupported
  transport used to tear down the running reader first and then return an error,
  leaving the caller believing nothing had happened.
- USB vendor-audio parse failures and short reads are logged (rate-limited).
  They were silent, so the symptom was "no audio and no error".
- **Breaking:** `test-mode` is no longer enabled by default. Factory physical-key
  events and device shutdown commands now require explicit
  `features = ["test-mode"]` opt-in.
- Public product naming is aligned to **ReAI-Vibe-Board** across package metadata,
  README files, crate-level docs, and examples.
- docs.rs builds with all features so opt-in factory APIs remain discoverable.
- USB and BLE examples feature-gate their factory-event match arms alongside
  the `test-mode` opt-in change.

## [0.2.2] — 2026-08-09

### Changed

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

[Unreleased]: https://github.com/ReAI-com/reai-board-sdk/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/ReAI-com/reai-board-sdk/releases/tag/v0.3.0
[0.2.2]: https://github.com/ReAI-com/reai-board-sdk/releases/tag/v0.2.2

<!-- 0.1.0 – 0.2.1 predate the public repository and have no git tags. -->
