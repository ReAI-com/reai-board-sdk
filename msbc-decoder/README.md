# msbc-decoder

A pure-Rust decoder for **mSBC** — the 16 kHz wideband variant of Bluetooth
SBC used by HFP wideband speech and by vendor audio links such as the
[ReAI Vibe Board](https://b.reai.com).

No ffmpeg runtime dependency, no C toolchain, no `unsafe`.

```rust
use msbc_decoder::{MsbcDecoder, MSBC_FRAME_SIZE};

let mut decoder = MsbcDecoder::new();
// frame is one 57-byte mSBC frame
let pcm: Vec<i16> = decoder.decode_frame(frame).unwrap();
assert_eq!(pcm.len(), 120);   // 8 subbands × 15 blocks
```

mSBC is fixed-format: 16 kHz mono, 8 subbands, 15 blocks, bitpool 26 —
57 bytes in, 120 `i16` samples out.

## License — please read

This crate is a **bit-exact translation of FFmpeg's `libavcodec/sbcdec.c`**,
including its synthesis matrix and prototype filter coefficient tables. A
translation is a derivative work, so this crate carries the license of the
original:

**LGPL-2.1-or-later** — see [LICENSE](LICENSE).

Original copyright holders are listed in the header of `src/lib.rs`
(Aurelien Jacobs, Intel, Nokia, Marcel Holtmann, Henryk Ploetz, Brad Midgley).

Note that this differs from `reai-board-sdk`, which is MIT. The decoder was
split into its own crate precisely so that the MIT SDK does not pull LGPL code
into builds that do not need it — it is an optional dependency there, enabled
only by the `ble` feature. If you consume the SDK over USB only, this crate is
never compiled into your binary.

If you are linking this into a proprietary product, make sure you meet the
LGPL's relinking requirement, or talk to your legal team first.
