# Phase 1.A — Frame Codec Design

**Status:** Approved 2026-04-21. Sub-project 1.A of the Phase 1 decomposition (see `2026-04-21-phase-1-decomposition.md`).

## Goal

Implement `transport::frame::FrameCodec` — the length-prefixed, type-tagged wire protocol that every Skattr connection uses after Tor gives us a byte stream. All Phase 1 sub-projects past 1.A layer on top of this codec.

## Scope

In scope:

- Fill in `FrameCodec::decode` and `FrameCodec::encode` (both currently `todo!()`).
- Unit tests, `proptest` round-trip coverage, `cargo-fuzz` harness with seed corpus.
- A single `CoreError::Frame(String)` variant (add if missing) for codec-level errors.

Out of scope (deferred to later sub-projects):

- Connection preamble version byte — lives in `transport::connection` (1.B territory). FrameCodec is stateless and frame-only.
- Noise-encrypting the payload — 1.B wraps post-handshake frames.
- Application-level ACK/retry logic — 1.E.
- Any change to the existing `Frame` / `FrameType` enums — keep the API as-is.

## Architecture

**Single file:** `crates/core/src/transport/frame.rs` (exists; stubbed). No new modules. Tests live inline (`#[cfg(test)] mod tests`). Property test in a new `crates/core/tests/frame_proptest.rs`. Fuzz target in `crates/core/fuzz/fuzz_targets/frame_decoder.rs`.

**Public API (unchanged from current stub):**

```rust
pub const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

#[repr(u8)]
pub enum FrameType { NoiseInit = 0x01, NoiseResp = 0x02, MlsWelcome = 0x03,
                     MlsCommit = 0x04, MlsApp = 0x05, Ack = 0x06,
                     Ping = 0x07, Pong = 0x08, Bye = 0x09, Error = 0x0A }

pub enum Frame {
    NoiseInit(Vec<u8>), NoiseResp(Vec<u8>),
    MlsWelcome(Vec<u8>), MlsCommit(Vec<u8>), MlsApp(Vec<u8>),
    Ack([u8; 16]),
    Ping, Pong, Bye,
    Error { code: u16, message: String },
}

pub struct FrameCodec { _private: () }
impl FrameCodec { pub fn new() -> Self; }
impl Decoder for FrameCodec { type Item = Frame; type Error = CoreError; }
impl Encoder<Frame> for FrameCodec { type Error = CoreError; }
```

## Wire format

Every frame on the wire:

```
+----------------+----------+------------------+
| length (u32 BE)| type (u8)|     payload      |
+----------------+----------+------------------+
```

`length` covers `type + payload` (NOT the length prefix itself). `length` minimum = 1 (type byte, empty payload). Maximum = 16 MiB. Payload maximum = `MAX_FRAME_SIZE - 1` = 16 MiB − 1.

### Payload encoding per variant

| Frame variant | Type byte | Payload |
|---|---|---|
| `NoiseInit(bytes)` | 0x01 | raw `bytes` |
| `NoiseResp(bytes)` | 0x02 | raw `bytes` |
| `MlsWelcome(bytes)` | 0x03 | raw `bytes` |
| `MlsCommit(bytes)` | 0x04 | raw `bytes` |
| `MlsApp(bytes)` | 0x05 | raw `bytes` |
| `Ack(id)` | 0x06 | raw 16 bytes (matches `MessageId` on-wire format; **no CBOR**) |
| `Ping` | 0x07 | empty (0 bytes) |
| `Pong` | 0x08 | empty |
| `Bye` | 0x09 | empty |
| `Error { code, message }` | 0x0A | CBOR of `{ code: u16, message: String }` via `ciborium` |

Type bytes 0x0B–0x1F are reserved (decoder rejects). 0x20+ reserved for future extension (design doc §1.2); decoder also rejects.

## Decoder state machine

`FrameCodec::decode(src: &mut BytesMut) -> Result<Option<Frame>>`:

1. If `src.len() < 4` → `Ok(None)` (need more bytes for the length prefix).
2. Peek `length = u32::from_be_bytes(src[0..4])` as `usize`.
3. If `length == 0` → `Err(CoreError::Frame("zero-length frame".into()))`.
4. If `length > MAX_FRAME_SIZE` → `Err(CoreError::Frame(format!("frame too large: {length}")))`.
5. If `src.len() < 4 + length` → `Ok(None)` (need more bytes for the payload).
6. `src.advance(4)`; read `type_byte = src[0]`; `src.advance(1)`.
7. `payload_len = length - 1`. Split off `payload = src.split_to(payload_len)` (bytes now owned, `src` rebalanced).
8. Match `type_byte`:

```rust
match type_byte {
    0x01 => Ok(Some(Frame::NoiseInit(payload.to_vec()))),
    0x02 => Ok(Some(Frame::NoiseResp(payload.to_vec()))),
    0x03 => Ok(Some(Frame::MlsWelcome(payload.to_vec()))),
    0x04 => Ok(Some(Frame::MlsCommit(payload.to_vec()))),
    0x05 => Ok(Some(Frame::MlsApp(payload.to_vec()))),
    0x06 => {
        if payload.len() != 16 {
            return Err(CoreError::Frame(format!(
                "Ack payload must be 16 bytes, got {}", payload.len())));
        }
        let mut id = [0u8; 16];
        id.copy_from_slice(&payload);
        Ok(Some(Frame::Ack(id)))
    }
    0x07 => empty_ok(&payload, Frame::Ping),
    0x08 => empty_ok(&payload, Frame::Pong),
    0x09 => empty_ok(&payload, Frame::Bye),
    0x0A => {
        let ErrorPayload { code, message } = ciborium::from_reader(&payload[..])
            .map_err(|e| CoreError::Frame(format!("Error payload CBOR: {e}")))?;
        Ok(Some(Frame::Error { code, message }))
    }
    other => Err(CoreError::Frame(format!("unknown frame type 0x{other:02X}"))),
}
```

Helper `empty_ok(payload, frame)` returns `Err` if payload non-empty, else `Ok(Some(frame))`.

### Partial-read behaviour

`Ok(None)` is the cursor-unchanged "need more bytes" signal per `tokio_util::codec::Decoder`. We do NOT call `src.reserve()` — `FramedRead` handles buffer growth.

### Error semantics

Returning `Err` from `decode` causes `tokio_util::codec::Framed` to fuse the stream. Callers in 1.B/1.E must translate the error into a `Bye` + close. The `CoreError::Frame` message is for logging; do not leak CBOR internals or payload sizes beyond what's shown above.

## Encoder state machine

`FrameCodec::encode(frame: Frame, dst: &mut BytesMut) -> Result<()>`:

1. Compute the payload bytes per variant (see table). For `Error`, CBOR-encode `{code, message}` via `ciborium::into_writer`.
2. `length = 1 + payload.len()`. If `length > MAX_FRAME_SIZE` → `Err(CoreError::Frame(format!("encoded frame too large: {length}")))`.
3. `dst.reserve(4 + length)`.
4. `dst.extend_from_slice(&(length as u32).to_be_bytes())`.
5. `dst.put_u8(type_byte)`.
6. `dst.extend_from_slice(&payload)`.
7. `Ok(())`.

## Error taxonomy

Add one variant to `crates/core/src/error.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    // ... existing variants ...
    #[error("frame codec: {0}")]
    Frame(String),
}
```

Rationale: Phase 1 consumers (1.B connection layer) translate all frame errors into a common action (send `Bye`, close connection). A structured enum with sub-variants buys nothing right now; a free-form message with context wins on simplicity. Upgrade to an enum later if we discover downstream code needs to fork on specific failure modes.

## Testing strategy

### Unit tests (in `frame.rs`)

All round-trip or boundary tests. Using `bytes::BytesMut` directly.

1. **Round-trip each variant** (10 tests) — encode one `Frame::X`, decode the resulting bytes, assert equality with original.
2. **Partial length prefix** — feed 3 bytes → `Ok(None)`; feed 4th → progresses to the payload-wait state.
3. **Partial payload** — encode a frame, feed first 10 bytes to `decode` → `Ok(None)`; feed the rest → parses.
4. **Two frames in one buffer** — concat two encoded frames; `decode` returns first; second `decode` call on the same buffer returns the second.
5. **Oversized length** — craft bytes with `length = MAX_FRAME_SIZE + 1` → `Err`.
6. **Zero length** — `length = 0` → `Err`.
7. **Unknown type byte** — valid length but type = 0x20 → `Err`.
8. **Ack wrong size** — length = 1 + 8 (should be 1 + 16), type = 0x06 → `Err`.
9. **Ping with non-empty payload** — length = 5, type = 0x07, 4 byte payload → `Err`. (Also for Pong, Bye.)
10. **Error with malformed CBOR** — length = 1 + some garbage, type = 0x0A → `Err`. Do NOT use a plain text payload that happens to parse as CBOR; feed genuine garbage like `[0xFF, 0xFF, 0xFF]`.

### Property test (`crates/core/tests/frame_proptest.rs`)

```rust
use proptest::prelude::*;
use skattr_core::test_exports::{Frame, FrameCodec, MAX_FRAME_SIZE};
use tokio_util::codec::{Decoder, Encoder};

fn arb_frame() -> impl Strategy<Value = Frame> {
    prop_oneof![
        prop::collection::vec(any::<u8>(), 0..65536).prop_map(Frame::NoiseInit),
        prop::collection::vec(any::<u8>(), 0..65536).prop_map(Frame::NoiseResp),
        prop::collection::vec(any::<u8>(), 0..65536).prop_map(Frame::MlsWelcome),
        prop::collection::vec(any::<u8>(), 0..65536).prop_map(Frame::MlsCommit),
        prop::collection::vec(any::<u8>(), 0..65536).prop_map(Frame::MlsApp),
        any::<[u8; 16]>().prop_map(Frame::Ack),
        Just(Frame::Ping),
        Just(Frame::Pong),
        Just(Frame::Bye),
        (any::<u16>(), "\\PC{0,256}").prop_map(|(code, message)|
            Frame::Error { code, message }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]
    #[test]
    fn encode_decode_round_trip(f in arb_frame()) {
        let mut codec = FrameCodec::new();
        let mut buf = bytes::BytesMut::new();
        codec.encode(f.clone(), &mut buf).unwrap();
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        prop_assert_eq!(format!("{f:?}"), format!("{decoded:?}"));
        prop_assert!(buf.is_empty(), "codec should consume exactly one frame");
    }
}
```

`Frame` doesn't derive `PartialEq` today (because of `Vec<u8>` content) — the comparison-via-`Debug` workaround is fine for the test. If `Frame` gets `PartialEq` later, switch to `prop_assert_eq!(f, decoded)`.

**Exposure note:** `Frame`, `FrameType`, `FrameCodec`, `MAX_FRAME_SIZE` currently live under `pub(crate) mod transport`. The integration test reaches them via `skattr_core::test_exports` gated on the `test-harness` feature (established pattern from 0.C/0.D). Add them to `test_exports` if not already there.

### Fuzz target (`crates/core/fuzz/fuzz_targets/frame_decoder.rs`)

```rust
#![no_main]
use libfuzzer_sys::fuzz_target;
use skattr_core::test_exports::FrameCodec;
use tokio_util::codec::Decoder;

fuzz_target!(|data: &[u8]| {
    let mut codec = FrameCodec::new();
    let mut buf = bytes::BytesMut::from(data);
    let _ = codec.decode(&mut buf); // must not panic
});
```

Seed corpus (`crates/core/fuzz/corpus/frame_decoder/`):

- One valid encoded instance of each Frame variant (`ping`, `pong`, `bye`, `noise_init_empty`, `mls_app_4kb`, `ack`, `error_simple`, etc.).
- `oversized_length` (length = MAX + 1, followed by zeros).
- `zero_length` (4 zero bytes).
- `unknown_type` (valid length, type = 0x20, empty payload).
- `truncated_noise_init` (length = 100, only 50 payload bytes).
- `two_frames_concat` (two valid frames glued together).

## Exit criteria

1. All unit tests pass.
2. 10 000-case proptest passes (`cargo test -p skattr-core --release frame_proptest`).
3. `cargo +nightly fuzz run frame_decoder -- -runs=1000000` completes with zero crashes locally (nightly CI will schedule 1-hour runs as part of workstream 4.B).
4. `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` green.
5. `cargo test --workspace --release` green.
6. CHANGELOG bullet under `[Unreleased]`.
7. CLAUDE.md "Repository state" paragraph notes 1.A complete (one-line addition, pattern from 0.B/C/D).

## Dependencies

All already in workspace (`Cargo.toml`):

- `tokio-util = { version = "0.7", features = ["codec"] }` — already present.
- `bytes = "1"` — already present (transitively, via tokio-util).
- `ciborium = "0.2"` — already present.
- `proptest = "1"` — already in `[dev-dependencies]`.

No new deps. Fuzz target uses the existing `crates/core/fuzz/Cargo.toml` harness introduced in 0.B.

## Risks

- **Fuzz corpus quality.** A sparse corpus means fuzzing takes longer to find real issues. Mitigation: curate by hand per the list above; add to corpus when real bugs surface in later sub-projects.
- **CBOR ambiguity in Error payload.** `ciborium` permits multiple encodings of the same value (map vs array, etc.). We define `Error` strictly as a CBOR map `{code, message}`. Struct serde derives enforce this.
- **`bytes::BytesMut` advance discipline.** Easy to get wrong — reading length but not advancing, or advancing too far and corrupting the buffer for the next frame. Covered by test 4 (two frames in one buffer).

## Open questions (deferred, not blockers for 1.A)

- **Error code catalogue.** `Error { code: u16 }` — who assigns codes? Propose 1.B carves out 0x0001–0x00FF for codec/noise errors and 1.E uses 0x0100–0x01FF for delivery errors. Doc this when we add the first non-zero code in a later sub-project.
- **Version negotiation.** The connection preamble's version byte (in 1.B) implies a future where 1.B's codec version differs from 1.A's. For now, there's only v1; cross the bridge when v2 lands.
