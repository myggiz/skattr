# Phase 1.A Frame Codec Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fill in `transport::frame::FrameCodec`'s encode/decode with full unit, property, and fuzz coverage.

**Architecture:** Single file to modify (`crates/core/src/transport/frame.rs`), one error variant to add (`crates/core/src/error.rs`), one test-exports line (`crates/core/src/lib.rs`), one new integration-test file (`crates/core/tests/frame_proptest.rs`), one new fuzz target + seed corpus. All ten `Frame` variants are encoded per the wire-format table in the design spec. Payload bytes for NoiseInit / NoiseResp / MlsWelcome / MlsCommit / MlsApp are passed through raw; `Ack` is a flat 16 bytes; `Ping` / `Pong` / `Bye` are empty; `Error` is a 2-field CBOR map.

**Tech Stack:** Rust 2021, `tokio_util::codec` (Decoder/Encoder traits), `bytes::BytesMut`, `ciborium` (CBOR for `Error`), `proptest` (property test), `cargo-fuzz` + `libfuzzer-sys` (fuzz target).

**Design spec:** `docs/superpowers/specs/2026-04-21-phase-1a-frame-codec-design.md` — read this first.

---

## Pre-flight

```bash
cd /home/myggiz/development/skattr-phase-1a-frame-codec
. "$HOME/.cargo/env"

cargo build --workspace
cargo test --workspace --release
cargo clippy --workspace --all-targets -- -D warnings
```

All three must pass before starting Task 1. The worktree was branched from `master` at `651fc9b` (Phase 1 decomposition spec); 0.A–0.E state is fully in place.

---

## File structure

```
crates/core/src/error.rs                          MODIFY: add CoreError::Frame variant
crates/core/src/transport/mod.rs                  MODIFY: twin-arm Frame/FrameCodec/FrameType/MAX_FRAME_SIZE re-export
crates/core/src/lib.rs                            MODIFY: re-export frame types in test_exports
crates/core/src/transport/frame.rs                MODIFY: fill FrameCodec::{encode, decode}, add #[cfg(test)] tests
crates/core/tests/frame_proptest.rs               CREATE: 10k-case round-trip proptest
crates/core/fuzz/Cargo.toml                       MODIFY: enable test-harness feature + [[bin]] entry
crates/core/fuzz/fuzz_targets/frame_decoder.rs    CREATE: fuzz harness
crates/core/fuzz/corpus/frame_decoder/*           CREATE: 11 seed-corpus files
CHANGELOG.md                                      MODIFY: bullet under [Unreleased]
CLAUDE.md                                         MODIFY: Repository-state paragraph one-liner
```

No other files touched.

---

## Task 1: `CoreError::Frame` variant + test-exports wiring

**Goal:** Give the codec a dedicated error variant and make `Frame` / `FrameCodec` / `FrameType` / `MAX_FRAME_SIZE` reachable from integration tests and the fuzz harness — reusing the existing twin-arm `pub use` pattern already used for `OnionListener` / `TorRuntime` / `Pool`.

**Files:**
- Modify: `crates/core/src/error.rs`
- Modify: `crates/core/src/transport/mod.rs`
- Modify: `crates/core/src/lib.rs`

- [ ] **Step 1: Add `CoreError::Frame` and tighten the `Transport` doc comment**

Open `crates/core/src/error.rs`. Find the `Transport` variant (around line 27) and update its doc comment:

```rust
    /// Transport-layer problem (Tor, Noise).
    #[error("transport: {0}")]
    Transport(String),
```

Then add a new variant just after the `Storage` variant (before `Config`):

```rust
    /// Frame codec (length-prefix, type byte, payload parse) problem.
    #[error("frame codec: {0}")]
    Frame(String),
```

The `#[non_exhaustive]` attribute on the enum means external crates can't match exhaustively, so adding a variant isn't a breaking change for downstream callers.

- [ ] **Step 2: Switch transport's frame re-export to a twin-arm**

Open `crates/core/src/transport/mod.rs`. Delete the existing line:

```rust
pub(crate) use frame::{Frame, FrameCodec, FrameType};
```

and replace it with a twin-arm that also promotes `MAX_FRAME_SIZE`:

```rust
#[cfg(not(feature = "test-harness"))]
pub(crate) use frame::{Frame, FrameCodec, FrameType, MAX_FRAME_SIZE};
#[cfg(feature = "test-harness")]
pub use frame::{Frame, FrameCodec, FrameType, MAX_FRAME_SIZE};
```

This matches the pattern already in place for `listener::OnionListener` and `tor::{TorRuntime, TorStatus}` a few lines below. Under the `test-harness` feature, these items become `pub` so `lib.rs::test_exports` can re-export them externally; without it they stay `pub(crate)`. `transport` itself is `pub(crate) mod transport`, so the effective outside-crate visibility is still capped unless the `test-harness` feature is active.

- [ ] **Step 3: Expose frame types via `test_exports`**

Open `crates/core/src/lib.rs`. Find the `test_exports` module (around line 52) and add one line so it reads:

```rust
#[cfg(feature = "test-harness")]
pub mod test_exports {
    pub use crate::transport::{OnionListener, TorConfig, TorRuntime, TorStatus};
    // Phase 0.D additions:
    pub use crate::storage::{ContactRepo, MessageRepo, Pool};
    // Phase 1.A additions:
    pub use crate::transport::{Frame, FrameCodec, FrameType, MAX_FRAME_SIZE};
}
```

- [ ] **Step 4: Verify it compiles without `test-harness`**

```bash
cargo build --workspace
```

Expected: clean build, no new warnings. This confirms the `#[cfg(not(feature = "test-harness"))]` arm still carries its weight.

- [ ] **Step 5: Verify it compiles with `test-harness`**

```bash
cargo build --workspace --all-features
```

Expected: clean build.

- [ ] **Step 6: Verify clippy is still clean**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/error.rs \
        crates/core/src/transport/mod.rs \
        crates/core/src/lib.rs
git commit -m "frame: add CoreError::Frame + twin-arm re-exports

Prepare for the 1.A codec implementation: a dedicated Frame error
variant (the existing Transport variant kept 'framing' in its doc
comment; move that responsibility here), plus promote Frame,
FrameCodec, FrameType, and MAX_FRAME_SIZE to the twin-arm pattern
used for OnionListener / TorRuntime / Pool — pub(crate) without
the test-harness feature, pub under it, so lib.rs::test_exports
can reach them for integration tests and the fuzz harness."
```

---

## Task 2: Encoder — empty and fixed-size payload variants

**Goal:** `FrameCodec::encode` produces correct bytes for `Ping`, `Pong`, `Bye`, and `Ack`. Tests pin the exact on-wire layout.

**Files:**
- Modify: `crates/core/src/transport/frame.rs`

- [ ] **Step 1: Add the `#[cfg(test)] mod tests` skeleton and the first failing test**

Open `crates/core/src/transport/frame.rs`. At the bottom of the file (after the `Encoder` impl), add:

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use bytes::BytesMut;
    use tokio_util::codec::{Decoder, Encoder};

    fn enc(frame: Frame) -> BytesMut {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::new();
        codec.encode(frame, &mut buf).unwrap();
        buf
    }

    #[test]
    fn encode_ping_is_length1_type07() {
        let buf = enc(Frame::Ping);
        assert_eq!(&buf[..], &[0, 0, 0, 1, 0x07]);
    }
}
```

- [ ] **Step 2: Run the test — it must fail on the `todo!()` in `encode`**

Run:

```bash
cargo test -p skattr-core --lib transport::frame::tests::encode_ping_is_length1_type07
```

Expected: panic with `todo!("write u32 BE length prefix, type byte, payload")`.

- [ ] **Step 3: Implement `encode` for `Ping`, `Pong`, `Bye`, and `Ack`**

Replace the entire `Encoder<Frame>` impl block with:

```rust
impl Encoder<Frame> for FrameCodec {
    type Error = CoreError;

    fn encode(&mut self, item: Frame, dst: &mut bytes::BytesMut) -> Result<()> {
        use bytes::BufMut as _;

        let (type_byte, payload): (u8, Vec<u8>) = match item {
            Frame::Ping => (0x07, Vec::new()),
            Frame::Pong => (0x08, Vec::new()),
            Frame::Bye => (0x09, Vec::new()),
            Frame::Ack(id) => (0x06, id.to_vec()),
            _ => todo!("remaining variants land in Task 3/4"),
        };

        let length = 1 + payload.len();
        if length > MAX_FRAME_SIZE {
            return Err(CoreError::Frame(format!(
                "encoded frame too large: {length} bytes"
            )));
        }

        dst.reserve(4 + length);
        dst.extend_from_slice(&u32::try_from(length).unwrap().to_be_bytes());
        dst.put_u8(type_byte);
        dst.extend_from_slice(&payload);
        Ok(())
    }
}
```

Note: `u32::try_from(length).unwrap()` is safe because the oversize check above guarantees `length <= 16 MiB`, which fits in a u32. This is one of the few places where `unwrap` is appropriate in library code — document the invariant with a comment if you prefer, but the check above makes it obviously safe.

- [ ] **Step 4: Run the Ping test — it passes**

```bash
cargo test -p skattr-core --lib transport::frame::tests::encode_ping_is_length1_type07
```

Expected: PASS.

- [ ] **Step 5: Add the Pong, Bye, and Ack tests**

Append inside `mod tests`:

```rust
    #[test]
    fn encode_pong_is_length1_type08() {
        assert_eq!(&enc(Frame::Pong)[..], &[0, 0, 0, 1, 0x08]);
    }

    #[test]
    fn encode_bye_is_length1_type09() {
        assert_eq!(&enc(Frame::Bye)[..], &[0, 0, 0, 1, 0x09]);
    }

    #[test]
    fn encode_ack_is_length17_type06_plus_16_bytes() {
        let id = [0xAB; 16];
        let buf = enc(Frame::Ack(id));
        // 4-byte length (17) + 1 type byte + 16 payload = 21 bytes total
        assert_eq!(buf.len(), 21);
        assert_eq!(&buf[..4], &[0, 0, 0, 17]);
        assert_eq!(buf[4], 0x06);
        assert_eq!(&buf[5..], &id);
    }
```

- [ ] **Step 6: Run the new tests**

```bash
cargo test -p skattr-core --lib transport::frame::tests::encode
```

Expected: 4 tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/transport/frame.rs
git commit -m "frame(encode): Ping/Pong/Bye/Ack with exact wire-layout tests

Emit length (u32 BE) = 1 + payload.len(), the type byte, then the
payload. Oversized encode rejected. Payload shapes for the empty
trio plus the 16-byte raw Ack are pinned by assertion on the
produced bytes."
```

---

## Task 3: Encoder — raw-bytes variants (`NoiseInit`, `NoiseResp`, `MlsWelcome`, `MlsCommit`, `MlsApp`)

**Goal:** Encode the five raw-bytes frame variants — payload is passed through unchanged.

**Files:**
- Modify: `crates/core/src/transport/frame.rs`

- [ ] **Step 1: Write a failing test for `NoiseInit`**

Append inside `mod tests`:

```rust
    #[test]
    fn encode_noise_init_wraps_payload_with_length_and_type01() {
        let payload = b"hello-noise".to_vec();
        let buf = enc(Frame::NoiseInit(payload.clone()));
        let length = 1 + payload.len();
        let expected_len_bytes = u32::try_from(length).unwrap().to_be_bytes();
        assert_eq!(&buf[..4], expected_len_bytes);
        assert_eq!(buf[4], 0x01);
        assert_eq!(&buf[5..], &payload[..]);
    }
```

- [ ] **Step 2: Run — it fails with `todo!`**

```bash
cargo test -p skattr-core --lib transport::frame::tests::encode_noise_init
```

Expected: panic at `todo!("remaining variants land in Task 3/4")`.

- [ ] **Step 3: Extend the encoder match with the five raw-bytes variants**

Replace the `match item {...}` block's tail so it reads:

```rust
        let (type_byte, payload): (u8, Vec<u8>) = match item {
            Frame::Ping => (0x07, Vec::new()),
            Frame::Pong => (0x08, Vec::new()),
            Frame::Bye => (0x09, Vec::new()),
            Frame::Ack(id) => (0x06, id.to_vec()),
            Frame::NoiseInit(p) => (0x01, p),
            Frame::NoiseResp(p) => (0x02, p),
            Frame::MlsWelcome(p) => (0x03, p),
            Frame::MlsCommit(p) => (0x04, p),
            Frame::MlsApp(p) => (0x05, p),
            _ => todo!("Error variant lands in Task 4"),
        };
```

- [ ] **Step 4: Verify the NoiseInit test passes and add tests for the other four**

```rust
    #[test]
    fn encode_noise_resp_uses_type02() {
        let buf = enc(Frame::NoiseResp(vec![0xAA, 0xBB]));
        assert_eq!(&buf[..], &[0, 0, 0, 3, 0x02, 0xAA, 0xBB]);
    }

    #[test]
    fn encode_mls_welcome_uses_type03() {
        let buf = enc(Frame::MlsWelcome(vec![0x11]));
        assert_eq!(&buf[..], &[0, 0, 0, 2, 0x03, 0x11]);
    }

    #[test]
    fn encode_mls_commit_uses_type04() {
        let buf = enc(Frame::MlsCommit(vec![0x22]));
        assert_eq!(&buf[..], &[0, 0, 0, 2, 0x04, 0x22]);
    }

    #[test]
    fn encode_mls_app_uses_type05() {
        let buf = enc(Frame::MlsApp(vec![0x33]));
        assert_eq!(&buf[..], &[0, 0, 0, 2, 0x05, 0x33]);
    }
```

- [ ] **Step 5: Run all new tests**

```bash
cargo test -p skattr-core --lib transport::frame::tests::encode
```

Expected: 9 tests pass (4 from Task 2 + 5 new).

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/transport/frame.rs
git commit -m "frame(encode): raw-bytes variants (Noise*, Mls*)

Five Frame variants whose payload is opaque to the codec: pass
the inner Vec<u8> through with the matching type byte (0x01-0x05).
Wire bytes pinned per variant."
```

---

## Task 4: Encoder — `Error` variant (CBOR payload)

**Goal:** Encode `Frame::Error { code, message }` as a 2-field CBOR map.

**Files:**
- Modify: `crates/core/src/transport/frame.rs`

- [ ] **Step 1: Introduce a serde struct for the CBOR shape + a failing encoder test**

Near the top of `frame.rs`, just below the `use` imports and before `MAX_FRAME_SIZE`, add:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct ErrorPayload {
    code: u16,
    message: String,
}
```

Append inside `mod tests`:

```rust
    #[test]
    fn encode_error_uses_type0a_and_cbor_payload() {
        let buf = enc(Frame::Error {
            code: 42,
            message: "bad".into(),
        });
        // Decode the payload back with ciborium to confirm it's valid CBOR
        // and matches what we passed in.
        let payload = &buf[5..]; // skip 4-byte length + 1 type byte
        let decoded: ErrorPayload =
            ciborium::from_reader(payload).expect("payload must be valid CBOR");
        assert_eq!(decoded.code, 42);
        assert_eq!(decoded.message, "bad");
        assert_eq!(buf[4], 0x0A); // type byte
    }
```

- [ ] **Step 2: Run — it fails with `todo!`**

```bash
cargo test -p skattr-core --lib transport::frame::tests::encode_error
```

Expected: panic at `todo!("Error variant lands in Task 4")`.

- [ ] **Step 3: Implement `Error` encoding**

Replace the `match item {...}` block again, dropping the final `todo!` branch:

```rust
        let (type_byte, payload): (u8, Vec<u8>) = match item {
            Frame::Ping => (0x07, Vec::new()),
            Frame::Pong => (0x08, Vec::new()),
            Frame::Bye => (0x09, Vec::new()),
            Frame::Ack(id) => (0x06, id.to_vec()),
            Frame::NoiseInit(p) => (0x01, p),
            Frame::NoiseResp(p) => (0x02, p),
            Frame::MlsWelcome(p) => (0x03, p),
            Frame::MlsCommit(p) => (0x04, p),
            Frame::MlsApp(p) => (0x05, p),
            Frame::Error { code, message } => {
                let mut buf = Vec::new();
                ciborium::into_writer(&ErrorPayload { code, message }, &mut buf)
                    .map_err(|e| CoreError::Frame(format!("encode Error: {e}")))?;
                (0x0A, buf)
            }
        };
```

- [ ] **Step 4: Run — the Error test passes; no more `todo!` reachable**

```bash
cargo test -p skattr-core --lib transport::frame::tests::encode
```

Expected: 10 tests pass.

- [ ] **Step 5: Add an encoder oversize-rejection test**

```rust
    #[test]
    fn encode_rejects_oversized_payload() {
        // MAX_FRAME_SIZE - 1 bytes fit; MAX_FRAME_SIZE bytes as payload
        // push length (1 + MAX_FRAME_SIZE) past MAX_FRAME_SIZE.
        let too_big = vec![0u8; MAX_FRAME_SIZE];
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::new();
        let err = codec
            .encode(Frame::MlsApp(too_big), &mut buf)
            .expect_err("oversize must error");
        assert!(matches!(err, CoreError::Frame(_)));
    }
```

- [ ] **Step 6: Run — the oversize test passes on the existing guard**

```bash
cargo test -p skattr-core --lib transport::frame::tests::encode_rejects_oversized_payload
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/transport/frame.rs
git commit -m "frame(encode): Error variant via CBOR map + oversize test

Frame::Error { code, message } serializes as a CBOR map via
ciborium, keyed under type byte 0x0A. Internal ErrorPayload
struct pins the wire shape (two named fields, u16 + String).
Encoder's oversize guard exercised: an MlsApp payload of
MAX_FRAME_SIZE pushes length over the cap and is rejected."
```

---

## Task 5: Decoder — length prefix, empty-payload variants (`Ping`, `Pong`, `Bye`)

**Goal:** Teach `FrameCodec::decode` to parse the length + type header and recognise the three empty-payload frames. Round-trip tests via the encoder.

**Files:**
- Modify: `crates/core/src/transport/frame.rs`

- [ ] **Step 1: Write a failing round-trip test for Ping**

Append inside `mod tests`:

```rust
    fn round_trip(f: Frame) -> Frame {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::new();
        codec.encode(f, &mut buf).unwrap();
        codec.decode(&mut buf).unwrap().unwrap()
    }

    #[test]
    fn decode_ping_round_trips() {
        assert!(matches!(round_trip(Frame::Ping), Frame::Ping));
    }
```

- [ ] **Step 2: Run — `todo!` in the decoder**

```bash
cargo test -p skattr-core --lib transport::frame::tests::decode_ping
```

Expected: panic at `todo!("parse u32 BE length, check MAX_FRAME_SIZE, switch on type byte")`.

- [ ] **Step 3: Implement the decoder skeleton + empty-payload variants**

Replace the entire `Decoder` impl block with:

```rust
impl Decoder for FrameCodec {
    type Item = Frame;
    type Error = CoreError;

    fn decode(&mut self, src: &mut bytes::BytesMut) -> Result<Option<Frame>> {
        if src.len() < 4 {
            return Ok(None);
        }

        let mut len_bytes = [0u8; 4];
        len_bytes.copy_from_slice(&src[0..4]);
        let length = u32::from_be_bytes(len_bytes) as usize;

        if length == 0 {
            return Err(CoreError::Frame("zero-length frame".into()));
        }
        if length > MAX_FRAME_SIZE {
            return Err(CoreError::Frame(format!(
                "frame too large: {length} bytes"
            )));
        }
        if src.len() < 4 + length {
            return Ok(None);
        }

        // Consume length prefix.
        let _ = src.split_to(4);
        // Consume type byte.
        let type_byte = src[0];
        let _ = src.split_to(1);
        let payload_len = length - 1;
        let payload = src.split_to(payload_len);

        let frame = match type_byte {
            0x07 => {
                if !payload.is_empty() {
                    return Err(CoreError::Frame(
                        "Ping must have empty payload".into(),
                    ));
                }
                Frame::Ping
            }
            0x08 => {
                if !payload.is_empty() {
                    return Err(CoreError::Frame(
                        "Pong must have empty payload".into(),
                    ));
                }
                Frame::Pong
            }
            0x09 => {
                if !payload.is_empty() {
                    return Err(CoreError::Frame("Bye must have empty payload".into()));
                }
                Frame::Bye
            }
            other => {
                return Err(CoreError::Frame(format!(
                    "unknown or not-yet-handled frame type 0x{other:02X}"
                )));
            }
        };

        Ok(Some(frame))
    }
}
```

- [ ] **Step 4: Run — Ping passes**

```bash
cargo test -p skattr-core --lib transport::frame::tests::decode_ping_round_trips
```

Expected: PASS.

- [ ] **Step 5: Add Pong and Bye round-trip tests**

```rust
    #[test]
    fn decode_pong_round_trips() {
        assert!(matches!(round_trip(Frame::Pong), Frame::Pong));
    }

    #[test]
    fn decode_bye_round_trips() {
        assert!(matches!(round_trip(Frame::Bye), Frame::Bye));
    }
```

- [ ] **Step 6: Run the three decode tests**

```bash
cargo test -p skattr-core --lib transport::frame::tests::decode
```

Expected: 3 decode tests pass. (Encoder tests from earlier tasks remain green.)

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/transport/frame.rs
git commit -m "frame(decode): length prefix + Ping/Pong/Bye

Decoder skeleton: read u32 BE length, validate (non-zero, <= MAX),
split off type byte, dispatch. Ping/Pong/Bye decoded by enforcing
an empty payload and returning the corresponding variant. All
other type bytes produce an 'unknown or not-yet-handled' error
that later tasks will narrow."
```

---

## Task 6: Decoder — `Ack` (fixed 16-byte payload)

**Goal:** Decode `Frame::Ack`; reject wrong-sized payloads.

**Files:**
- Modify: `crates/core/src/transport/frame.rs`

- [ ] **Step 1: Write failing tests (happy + wrong-size)**

Append inside `mod tests`:

```rust
    #[test]
    fn decode_ack_round_trips() {
        let id = [0xCD; 16];
        match round_trip(Frame::Ack(id)) {
            Frame::Ack(got) => assert_eq!(got, id),
            other => panic!("expected Ack, got {other:?}"),
        }
    }

    #[test]
    fn decode_ack_rejects_wrong_payload_length() {
        // Hand-craft bytes: length = 9 (1 type + 8 payload), type 0x06, 8 random bytes.
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&9u32.to_be_bytes());
        buf.extend_from_slice(&[0x06]);
        buf.extend_from_slice(&[0; 8]);
        let mut codec = FrameCodec::new();
        let err = codec.decode(&mut buf).expect_err("must reject wrong-size ack");
        assert!(matches!(err, CoreError::Frame(_)));
    }
```

- [ ] **Step 2: Run — both fail**

```bash
cargo test -p skattr-core --lib transport::frame::tests::decode_ack
```

Expected: first panics on "unknown or not-yet-handled"; second also fails for the same reason.

- [ ] **Step 3: Add the `0x06` arm to the decoder match**

In `Decoder::decode`, find the empty-payload arms and add the new one just before `0x07`:

```rust
            0x06 => {
                if payload.len() != 16 {
                    return Err(CoreError::Frame(format!(
                        "Ack payload must be 16 bytes, got {}",
                        payload.len()
                    )));
                }
                let mut id = [0u8; 16];
                id.copy_from_slice(&payload);
                Frame::Ack(id)
            }
```

- [ ] **Step 4: Run the Ack tests**

```bash
cargo test -p skattr-core --lib transport::frame::tests::decode_ack
```

Expected: both PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/transport/frame.rs
git commit -m "frame(decode): Ack (16-byte raw payload)

Type byte 0x06, payload must be exactly 16 bytes or the decoder
rejects with CoreError::Frame. Round-trip and wrong-size tests
pin both halves."
```

---

## Task 7: Decoder — raw-bytes variants (`NoiseInit`, `NoiseResp`, `MlsWelcome`, `MlsCommit`, `MlsApp`)

**Goal:** Decode the five raw-bytes variants.

**Files:**
- Modify: `crates/core/src/transport/frame.rs`

- [ ] **Step 1: Write failing round-trip tests for each**

Append inside `mod tests`:

```rust
    #[test]
    fn decode_noise_init_round_trips() {
        let payload = b"init-payload".to_vec();
        match round_trip(Frame::NoiseInit(payload.clone())) {
            Frame::NoiseInit(got) => assert_eq!(got, payload),
            other => panic!("expected NoiseInit, got {other:?}"),
        }
    }

    #[test]
    fn decode_noise_resp_round_trips() {
        let payload = b"resp-payload".to_vec();
        match round_trip(Frame::NoiseResp(payload.clone())) {
            Frame::NoiseResp(got) => assert_eq!(got, payload),
            other => panic!("expected NoiseResp, got {other:?}"),
        }
    }

    #[test]
    fn decode_mls_welcome_round_trips() {
        let payload = vec![0x11, 0x22, 0x33];
        match round_trip(Frame::MlsWelcome(payload.clone())) {
            Frame::MlsWelcome(got) => assert_eq!(got, payload),
            other => panic!("expected MlsWelcome, got {other:?}"),
        }
    }

    #[test]
    fn decode_mls_commit_round_trips() {
        let payload = vec![0xAA, 0xBB];
        match round_trip(Frame::MlsCommit(payload.clone())) {
            Frame::MlsCommit(got) => assert_eq!(got, payload),
            other => panic!("expected MlsCommit, got {other:?}"),
        }
    }

    #[test]
    fn decode_mls_app_round_trips() {
        let payload = vec![0x01; 1000];
        match round_trip(Frame::MlsApp(payload.clone())) {
            Frame::MlsApp(got) => assert_eq!(got, payload),
            other => panic!("expected MlsApp, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run — all five fail**

```bash
cargo test -p skattr-core --lib transport::frame::tests::decode_noise_init
cargo test -p skattr-core --lib transport::frame::tests::decode_mls
```

Expected: "unknown or not-yet-handled" error.

- [ ] **Step 3: Add the five arms in the decoder match**

Insert before the `0x06` arm:

```rust
            0x01 => Frame::NoiseInit(payload.to_vec()),
            0x02 => Frame::NoiseResp(payload.to_vec()),
            0x03 => Frame::MlsWelcome(payload.to_vec()),
            0x04 => Frame::MlsCommit(payload.to_vec()),
            0x05 => Frame::MlsApp(payload.to_vec()),
```

- [ ] **Step 4: Run the five decode tests**

```bash
cargo test -p skattr-core --lib transport::frame::tests::decode
```

Expected: all round-trip tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/transport/frame.rs
git commit -m "frame(decode): raw-bytes variants (Noise*, Mls*)

Type bytes 0x01-0x05 pass the payload through unchanged into the
matching Frame variant. Round-trip tests pin each."
```

---

## Task 8: Decoder — `Error` variant (CBOR payload)

**Goal:** Decode `Frame::Error { code, message }`; reject malformed CBOR.

**Files:**
- Modify: `crates/core/src/transport/frame.rs`

- [ ] **Step 1: Write failing tests (happy + malformed CBOR)**

Append inside `mod tests`:

```rust
    #[test]
    fn decode_error_round_trips() {
        let f = Frame::Error {
            code: 0x0042,
            message: "auth failed".into(),
        };
        match round_trip(f) {
            Frame::Error { code, message } => {
                assert_eq!(code, 0x0042);
                assert_eq!(message, "auth failed");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn decode_error_rejects_malformed_cbor() {
        // length = 4 (1 type + 3 payload), type = 0x0A, payload = garbage bytes.
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&4u32.to_be_bytes());
        buf.extend_from_slice(&[0x0A]);
        buf.extend_from_slice(&[0xFF, 0xFF, 0xFF]);
        let mut codec = FrameCodec::new();
        let err = codec
            .decode(&mut buf)
            .expect_err("must reject malformed CBOR");
        assert!(matches!(err, CoreError::Frame(_)));
    }
```

- [ ] **Step 2: Run — both fail**

```bash
cargo test -p skattr-core --lib transport::frame::tests::decode_error
```

Expected: "unknown or not-yet-handled" error.

- [ ] **Step 3: Add the `0x0A` arm**

Insert between the `0x06` and `0x07` arms:

```rust
            0x0A => {
                let parsed: ErrorPayload = ciborium::from_reader(&payload[..])
                    .map_err(|e| CoreError::Frame(format!("Error payload CBOR: {e}")))?;
                Frame::Error {
                    code: parsed.code,
                    message: parsed.message,
                }
            }
```

- [ ] **Step 4: Run the Error tests**

```bash
cargo test -p skattr-core --lib transport::frame::tests::decode_error
```

Expected: both PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/transport/frame.rs
git commit -m "frame(decode): Error variant (CBOR map)

Type byte 0x0A carries a CBOR-encoded {code, message} map. Invalid
CBOR is rejected with CoreError::Frame carrying the ciborium error
message. Round-trip confirms field values survive the encode/decode
cycle."
```

---

## Task 9: Decoder — header and payload error paths

**Goal:** Cover every remaining error path: zero length, oversized length, unknown type byte, non-empty empty-payload frames.

**Files:**
- Modify: `crates/core/src/transport/frame.rs`

- [ ] **Step 1: Write the error-path tests (no implementation changes expected — these should already pass)**

Append inside `mod tests`:

```rust
    #[test]
    fn decode_zero_length_rejected() {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&[0, 0, 0, 0]);
        let mut codec = FrameCodec::new();
        let err = codec.decode(&mut buf).expect_err("zero length must error");
        assert!(matches!(err, CoreError::Frame(_)));
    }

    #[test]
    fn decode_oversized_length_rejected() {
        let mut buf = BytesMut::new();
        let oversized = u32::try_from(MAX_FRAME_SIZE + 1).unwrap();
        buf.extend_from_slice(&oversized.to_be_bytes());
        // Do NOT push a payload; the length check short-circuits before reading.
        let mut codec = FrameCodec::new();
        let err = codec.decode(&mut buf).expect_err("oversized must error");
        assert!(matches!(err, CoreError::Frame(_)));
    }

    #[test]
    fn decode_unknown_type_rejected() {
        // length = 1 (type only), type = 0x20 (reserved).
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&1u32.to_be_bytes());
        buf.extend_from_slice(&[0x20]);
        let mut codec = FrameCodec::new();
        let err = codec.decode(&mut buf).expect_err("unknown type must error");
        assert!(matches!(err, CoreError::Frame(_)));
    }

    #[test]
    fn decode_ping_with_payload_rejected() {
        // length = 2 (1 type + 1 payload), type = 0x07, payload = one byte.
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&2u32.to_be_bytes());
        buf.extend_from_slice(&[0x07, 0xFF]);
        let mut codec = FrameCodec::new();
        let err = codec
            .decode(&mut buf)
            .expect_err("Ping with payload must error");
        assert!(matches!(err, CoreError::Frame(_)));
    }

    #[test]
    fn decode_pong_with_payload_rejected() {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&2u32.to_be_bytes());
        buf.extend_from_slice(&[0x08, 0xFF]);
        let mut codec = FrameCodec::new();
        let err = codec.decode(&mut buf).expect_err("Pong with payload must error");
        assert!(matches!(err, CoreError::Frame(_)));
    }

    #[test]
    fn decode_bye_with_payload_rejected() {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&2u32.to_be_bytes());
        buf.extend_from_slice(&[0x09, 0xFF]);
        let mut codec = FrameCodec::new();
        let err = codec.decode(&mut buf).expect_err("Bye with payload must error");
        assert!(matches!(err, CoreError::Frame(_)));
    }
```

- [ ] **Step 2: Run — all six pass on the existing implementation**

```bash
cargo test -p skattr-core --lib transport::frame::tests::decode_zero_length_rejected
cargo test -p skattr-core --lib transport::frame::tests::decode_oversized
cargo test -p skattr-core --lib transport::frame::tests::decode_unknown
cargo test -p skattr-core --lib transport::frame::tests::decode_ping_with
cargo test -p skattr-core --lib transport::frame::tests::decode_pong_with
cargo test -p skattr-core --lib transport::frame::tests::decode_bye_with
```

Expected: all PASS. If one fails, the decoder implementation missed the check — go fix it rather than marking the test skipped.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/transport/frame.rs
git commit -m "frame(decode): header + payload error-path coverage

Six negative tests pin the reject behaviour for zero length,
oversized length, unknown type byte 0x20, and non-empty payloads
on Ping / Pong / Bye. Every path returns CoreError::Frame."
```

---

## Task 10: Decoder — partial reads and multi-frame buffers

**Goal:** Confirm the decoder returns `Ok(None)` when more bytes are needed (both on the length prefix and on the payload), and that a single buffer containing two concatenated frames yields both in order.

**Files:**
- Modify: `crates/core/src/transport/frame.rs`

- [ ] **Step 1: Write the partial-read and concat tests**

Append inside `mod tests`:

```rust
    #[test]
    fn decode_returns_none_on_partial_length_prefix() {
        let mut buf = BytesMut::from(&[0u8, 0, 0][..]); // only 3 of 4 bytes
        let mut codec = FrameCodec::new();
        assert!(codec.decode(&mut buf).unwrap().is_none());
        assert_eq!(buf.len(), 3, "buffer must not be consumed");
    }

    #[test]
    fn decode_returns_none_on_partial_payload() {
        // length = 5 (1 type + 4 payload), type = 0x01, only 2 payload bytes.
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&5u32.to_be_bytes());
        buf.extend_from_slice(&[0x01, 0xAA, 0xBB]);
        let mut codec = FrameCodec::new();
        assert!(codec.decode(&mut buf).unwrap().is_none());
        assert_eq!(
            buf.len(),
            7,
            "buffer must not be consumed on insufficient payload"
        );
    }

    #[test]
    fn decode_two_frames_concat() {
        // Encode a Ping and a Bye into the same buffer; decode should
        // yield each in turn.
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::new();
        codec.encode(Frame::Ping, &mut buf).unwrap();
        codec.encode(Frame::Bye, &mut buf).unwrap();

        let first = codec.decode(&mut buf).unwrap().expect("first frame");
        assert!(matches!(first, Frame::Ping));
        let second = codec.decode(&mut buf).unwrap().expect("second frame");
        assert!(matches!(second, Frame::Bye));
        assert!(buf.is_empty());
    }

    #[test]
    fn decode_resumes_after_needing_more_bytes() {
        // Feed the length prefix alone, then the rest.
        let mut codec = FrameCodec::new();
        let mut full = BytesMut::new();
        codec.encode(Frame::Pong, &mut full).unwrap();

        let mut staged = BytesMut::new();
        staged.extend_from_slice(&full[..2]);
        assert!(codec.decode(&mut staged).unwrap().is_none());

        staged.extend_from_slice(&full[2..]);
        let frame = codec.decode(&mut staged).unwrap().expect("frame arrives");
        assert!(matches!(frame, Frame::Pong));
        assert!(staged.is_empty());
    }
```

- [ ] **Step 2: Run — all four must pass**

```bash
cargo test -p skattr-core --lib transport::frame::tests::decode_returns_none
cargo test -p skattr-core --lib transport::frame::tests::decode_two_frames_concat
cargo test -p skattr-core --lib transport::frame::tests::decode_resumes_after
```

Expected: all PASS on the existing implementation. A failure here means the `Ok(None)` paths in the decoder aren't leaving the buffer intact — go fix.

- [ ] **Step 3: Run the full frame test suite to confirm everything is green**

```bash
cargo test -p skattr-core --lib transport::frame
```

Expected: every frame test passes (there should be 30+ at this point).

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/transport/frame.rs
git commit -m "frame(decode): partial-read and multi-frame buffer tests

Verify the decoder returns Ok(None) without consuming bytes when
either the length prefix or payload is short, yields consecutive
frames from a buffer that holds two back-to-back, and resumes
correctly after a partial-buffer short-read."
```

---

## Task 11: Property test — 10 000-case round-trip

**Goal:** A `proptest`-driven integration test that generates arbitrary `Frame` values and asserts `decode(encode(f)) == f` for 10 000 cases.

**Files:**
- Create: `crates/core/tests/frame_proptest.rs`

- [ ] **Step 1: Create the proptest file**

Write `crates/core/tests/frame_proptest.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Property-based round-trip test for the frame codec.
//!
//! Hits the full encode → decode → encode path with 10 000 generated
//! frames per run. Payload sizes are capped at 64 KiB so the test
//! stays fast; the oversize path is covered by unit tests in
//! `transport::frame::tests`.

#![cfg(feature = "test-harness")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use bytes::BytesMut;
use proptest::prelude::*;
use skattr_core::test_exports::{Frame, FrameCodec};
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
        (any::<u16>(), "\\PC{0,256}").prop_map(|(code, message)| Frame::Error {
            code,
            message,
        }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    #[test]
    fn encode_decode_round_trip(f in arb_frame()) {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::new();
        codec.encode(f.clone(), &mut buf).unwrap();
        let decoded = codec.decode(&mut buf).unwrap().expect("frame decoded");
        // Frame does not derive PartialEq (opaque Vec<u8> payloads); Debug
        // comparison is sufficient for structural equality.
        prop_assert_eq!(format!("{f:?}"), format!("{decoded:?}"));
        prop_assert!(buf.is_empty(), "codec must consume exactly one frame");
    }
}
```

Note: `Frame` has `#[derive(Debug, Clone)]` but not `PartialEq` (because of opaque `Vec<u8>` and `String` fields). Using `format!("{:?}")` gives us structural equality without imposing `PartialEq` on the public type.

- [ ] **Step 2: Run the proptest**

```bash
cargo test -p skattr-core --test frame_proptest --features test-harness --release
```

Expected: `test encode_decode_round_trip ... ok` after a few seconds.

If any case shrinks to a failure, `proptest` writes the minimal failing input to `crates/core/proptest-regressions/frame_proptest.txt` — commit that file alongside the fix.

- [ ] **Step 3: Commit**

```bash
git add crates/core/tests/frame_proptest.rs
# If proptest-regressions/ was created, add it too:
[ -d crates/core/proptest-regressions ] && git add crates/core/proptest-regressions
git commit -m "frame: proptest round-trip (10 000 cases)

Generate arbitrary Frame values up to 64 KiB payloads and assert
decode(encode(f)) reproduces f (via Debug comparison since Frame
intentionally doesn't derive PartialEq). Oversize path is covered
by unit tests; proptest exercises logic, not boundary."
```

---

## Task 12: Fuzz target — `frame_decoder`

**Goal:** A `cargo-fuzz` harness that feeds arbitrary bytes to `FrameCodec::decode` and asserts "no panics." Register it in `fuzz/Cargo.toml`, create the target file, curate the seed corpus, and prove the build works.

**Files:**
- Modify: `crates/core/fuzz/Cargo.toml`
- Create: `crates/core/fuzz/fuzz_targets/frame_decoder.rs`
- Create: `crates/core/fuzz/corpus/frame_decoder/{ping,pong,bye,ack,mls_app_small,noise_init_empty,error_simple,oversized_length,zero_length,unknown_type,truncated_noise_init,two_frames}`

- [ ] **Step 1: Add the fuzz target to `fuzz/Cargo.toml`**

Open `crates/core/fuzz/Cargo.toml`. Change the `skattr-core` dep line to enable the test-harness feature, and append a new `[[bin]]` entry:

```toml
[package]
name = "skattr-core-fuzz"
version = "0.0.0"
publish = false
edition = "2021"

[package.metadata]
cargo-fuzz = true

[workspace]

[dependencies]
libfuzzer-sys = "0.4"
skattr-core = { path = "..", features = ["test-harness"] }
tempfile = "3"
bytes = "1"

[[bin]]
name = "vault_parser"
path = "fuzz_targets/vault_parser.rs"
test = false
doc = false
bench = false

[[bin]]
name = "frame_decoder"
path = "fuzz_targets/frame_decoder.rs"
test = false
doc = false
bench = false
```

- [ ] **Step 2: Create the fuzz target**

Write `crates/core/fuzz/fuzz_targets/frame_decoder.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Fuzz target: `FrameCodec::decode` must never panic on arbitrary bytes.

#![no_main]

use libfuzzer_sys::fuzz_target;
use skattr_core::test_exports::FrameCodec;
use tokio_util::codec::Decoder;

fuzz_target!(|data: &[u8]| {
    let mut codec = FrameCodec::new();
    let mut buf = bytes::BytesMut::from(data);
    // Drain whatever frames parse out of the buffer. Errors are fine;
    // panics or aborts are bugs.
    loop {
        match codec.decode(&mut buf) {
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(_) => break,
        }
    }
});
```

The `tokio_util::codec::Decoder` trait is re-exported at the top level of `tokio_util::codec`; no other imports needed (`bytes` is a direct dep in `fuzz/Cargo.toml`).

- [ ] **Step 3: Build the fuzz target (don't run it yet)**

```bash
cd crates/core/fuzz
cargo +nightly build --bin frame_decoder
cd ../../..
```

If `cargo +nightly` is missing, install nightly first:

```bash
rustup toolchain install nightly --profile minimal
```

Expected: `Compiling skattr-core-fuzz` then `Finished dev [unoptimized + debuginfo] target(s)`.

- [ ] **Step 4: Curate the seed corpus**

Create the corpus directory:

```bash
mkdir -p crates/core/fuzz/corpus/frame_decoder
```

Each seed is a small file containing raw bytes a real `FrameCodec::decode` input would see. Use `printf` to write exact bytes:

```bash
# Valid Ping: length=1, type=0x07
printf '\x00\x00\x00\x01\x07' > crates/core/fuzz/corpus/frame_decoder/ping

# Valid Pong: length=1, type=0x08
printf '\x00\x00\x00\x01\x08' > crates/core/fuzz/corpus/frame_decoder/pong

# Valid Bye: length=1, type=0x09
printf '\x00\x00\x00\x01\x09' > crates/core/fuzz/corpus/frame_decoder/bye

# Valid Ack: length=17, type=0x06, 16 zero bytes
printf '\x00\x00\x00\x11\x06\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00' \
    > crates/core/fuzz/corpus/frame_decoder/ack

# Valid empty NoiseInit: length=1, type=0x01
printf '\x00\x00\x00\x01\x01' > crates/core/fuzz/corpus/frame_decoder/noise_init_empty

# Valid MlsApp with 3 payload bytes: length=4, type=0x05, 0xAA 0xBB 0xCC
printf '\x00\x00\x00\x04\x05\xAA\xBB\xCC' > crates/core/fuzz/corpus/frame_decoder/mls_app_small

# Error frame with a minimal CBOR map payload (manually constructed):
# {0: 1, 1: ""} -> a2 00 01 01 60 (5 bytes)
# length = 1 + 5 = 6
# NOTE: this is a CBOR integer-keyed map, not our named-field map, so the
# decoder will reject it — perfect fuzz seed (exercises the error path).
printf '\x00\x00\x00\x06\x0A\xA2\x00\x01\x01\x60' \
    > crates/core/fuzz/corpus/frame_decoder/error_garbage_cbor

# Oversized length: length = MAX + 1 = 0x01000001
printf '\x01\x00\x00\x01' > crates/core/fuzz/corpus/frame_decoder/oversized_length

# Zero length
printf '\x00\x00\x00\x00' > crates/core/fuzz/corpus/frame_decoder/zero_length

# Unknown type byte 0x20 with empty payload: length=1
printf '\x00\x00\x00\x01\x20' > crates/core/fuzz/corpus/frame_decoder/unknown_type

# Truncated frame: length says 100, only 4 payload bytes follow
printf '\x00\x00\x00\x64\x01\x01\x02\x03\x04' > crates/core/fuzz/corpus/frame_decoder/truncated_noise_init

# Two frames concatenated: Ping + Bye
printf '\x00\x00\x00\x01\x07\x00\x00\x00\x01\x09' > crates/core/fuzz/corpus/frame_decoder/two_frames_concat
```

- [ ] **Step 5: Run the fuzz target briefly (1000 iterations) to confirm it actually runs**

```bash
cd crates/core/fuzz
cargo +nightly fuzz run frame_decoder -- -runs=1000
cd ../../..
```

Expected: libFuzzer runs with the seed corpus, reports `Done 1000 runs`, no crashes. This is a smoke test — the 1-hour nightly run is a separate CI activity (workstream 4.B will schedule it).

- [ ] **Step 6: Commit**

```bash
git add crates/core/fuzz/Cargo.toml \
        crates/core/fuzz/fuzz_targets/frame_decoder.rs \
        crates/core/fuzz/corpus/frame_decoder
git commit -m "frame: cargo-fuzz harness + 11-file seed corpus

FrameCodec::decode never panics on arbitrary input. Seed corpus
includes one valid encoded frame of each variant plus hand-crafted
boundary cases (oversized length, zero length, unknown type,
truncated payload, two-frames-concat, CBOR-shape-mismatch). Fuzz
dep now enables the test-harness feature on skattr-core so the
target can reach FrameCodec via test_exports."
```

---

## Task 13: CHANGELOG + CLAUDE.md + final gate run

**Goal:** Record the feature in CHANGELOG, touch the Repository-state paragraph in CLAUDE.md, and prove all three gates pass before the branch is considered done.

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `CLAUDE.md`

- [ ] **Step 1: Append the 1.A bullet to `CHANGELOG.md`**

Open `CHANGELOG.md`. Find the last bullet under `### Added` (currently the Phase 0.E bullet or the "Phase 0 complete" marker) and append a new bullet below it, above the `[Unreleased]` link at the bottom:

```markdown
- **Phase 1.A Frame codec:** `transport::frame::FrameCodec` implements `tokio_util::codec::Decoder` + `Encoder` for the 10 `Frame` variants. Wire format is `[u32 BE length][u8 type][payload]`, max 16 MiB. `Ping`/`Pong`/`Bye` carry empty payloads, `Ack` is 16 raw bytes, `NoiseInit`/`NoiseResp`/`MlsWelcome`/`MlsCommit`/`MlsApp` are opaque byte blobs, `Error` is a 2-field CBOR map (`code: u16`, `message: String`). New `CoreError::Frame(String)` variant. Coverage: inline unit tests (round-trip + boundary), 10 000-case `proptest`, and a `cargo-fuzz` target with 11-file seed corpus.
```

- [ ] **Step 2: Update `CLAUDE.md` Repository-state paragraph**

Open `CLAUDE.md`. Find the `## Repository state` section (first `##` after the top comment) and replace the opening sentence. Change:

```markdown
**Phase 0 is complete** — all five workstreams (0.A scaffold, 0.B
identity & crypto, 0.C Arti integration, 0.D storage layer, 0.E
documentation baseline) have shipped and are merged to master.
```

to:

```markdown
**Phase 0 is complete and Phase 1.A (frame codec) is done.** Phase 0
shipped all five workstreams (0.A scaffold, 0.B identity & crypto, 0.C
Arti integration, 0.D storage layer, 0.E documentation baseline).
Phase 1.A added `transport::frame::FrameCodec` (length-prefix + type
byte + typed payload, 10 variants, `tokio_util::codec::Decoder` + `Encoder`
traits) with unit, proptest, and cargo-fuzz coverage.
```

And add a sentence to the "Phase 1 is next" paragraph. Change:

```markdown
Phase 1 is next: MLS message exchange, outbox delivery, invite links,
and the session manager wiring that ties transport + mls + storage
together.
```

to:

```markdown
Phase 1 continues with 1.B Noise_XK handshake, then 1.C MLS 2-member
groups, 1.D invite + contact, 1.E delivery semantics, 1.F CLI
integration, 1.G message storage & search — see
`docs/superpowers/specs/2026-04-21-phase-1-decomposition.md` for the
full Phase 1 split.
```

- [ ] **Step 3: Run the full gate sequence**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features --release
```

All three must succeed. `--all-features` is needed for the proptest (it's gated on `test-harness`).

- [ ] **Step 4: Commit**

```bash
git add CHANGELOG.md CLAUDE.md
git commit -m "docs: CHANGELOG + CLAUDE.md — Phase 1.A frame codec done

Append the 1.A bullet under [Unreleased] (full wire-format summary
and coverage list). Update CLAUDE.md Repository-state opener to
'Phase 0 complete + Phase 1.A done' and point the Phase 1 forward
pointer at the decomposition spec."
```

---

## Post-plan wrap-up

Merging to master is the human's job — not automated by this plan. Signal of readiness:

- All 13 task commits on the `phase-1a-frame-codec` branch.
- Three gates (`fmt`, `clippy`, `test`) green on the final commit.
- The design spec (committed in the brainstorming session) still matches what the code does — no late divergence.

When the user merges, the standard pattern from prior phases is:

```bash
cd /home/myggiz/development/skattr
git checkout master
git merge --no-ff phase-1a-frame-codec -m "Merge branch 'phase-1a-frame-codec'"
git worktree remove /home/myggiz/development/skattr-phase-1a-frame-codec
git branch -d phase-1a-frame-codec
```

---

## Notes for the executing engineer

- **TDD discipline.** Every code change (encoder or decoder logic) lands behind a failing test. If a step says "write the test first, run it, watch it fail" — actually run it and actually watch it fail. `todo!()` panics count as failures.
- **Test visibility.** Unit tests inside `#[cfg(test)] mod tests` in `frame.rs` reach internal types directly. The proptest in `crates/core/tests/frame_proptest.rs` uses `skattr_core::test_exports::{Frame, FrameCodec}` and is gated on `--features test-harness`.
- **`Frame` PartialEq.** The enum intentionally does NOT derive `PartialEq` (opaque `Vec<u8>` and `String` fields). Round-trip comparisons use `format!("{f:?}")` string equality — good enough for structural equivalence here.
- **CBOR shape stability.** `Frame::Error`'s on-wire shape is pinned by the private `ErrorPayload` struct. Changing field names or types is a wire-break — if you need to alter it, the protocol version byte (handled in 1.B) is the right mechanism.
- **`tokio_util::codec::Decoder` contract.** Return `Ok(None)` when you need more bytes — do NOT call `src.reserve` ahead of time, the framing layer handles buffer growth. Return `Err(...)` only on unrecoverable frame-level errors; the stream fuses on error, so a `CoreError::Frame` always ends the connection.
- **Buffer discipline.** `bytes::BytesMut::split_to(n)` gives you owned bytes AND advances the buffer; use it for the length prefix, the type byte, and the payload in that order. The returned payload handle is a `BytesMut`, so `&payload[..]` gives you `&[u8]` for `copy_from_slice` and `ciborium::from_reader`.
- **Fuzz target smoke test.** Task 12 runs 1000 iterations locally — enough to catch a trivially reproducible crash. The full "1 hour clean" validation mentioned in the design spec's exit criteria is a CI concern (workstream 4.B) and not blocking for 1.A merge.
- **Unwrap in library code.** The only `unwrap` introduced is `u32::try_from(length).unwrap()` in the encoder — gated by an oversize check on the same line's predecessor. That's OK because the contract is "length fits in u32 if it's under 16 MiB" and the check enforces it. Everywhere else, use `?` with `CoreError::Frame`.
