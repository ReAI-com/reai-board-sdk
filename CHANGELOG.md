# Changelog

All notable changes to `reai-board-sdk` are documented here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

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
