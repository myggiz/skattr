# Phase 1.B — Noise_XK Handshake Design

**Status:** Approved 2026-04-21. Sub-project 1.B of the Phase 1 decomposition (`2026-04-21-phase-1-decomposition.md`). Depends on 1.A (frame codec, merged at master `1a36eee`).

## Goal

Stand up the Noise_XK handshake so two daemons can mutually authenticate over a bidirectional byte stream (in-memory `tokio::io::duplex` for unit tests, Tor `DataStream` for integration) and obtain a post-handshake transport cipher plus a 32-byte `h_transport` that Phase 1.C will inject as an external PSK into the first MLS Commit.

## Scope

**In scope**

- `transport::noise::handshake_initiator` and `handshake_responder` driving `snow`'s `HandshakeState`.
- Ed25519 → X25519 conversion helpers on `IdentityKey` (both halves: private via SHA-512-clamp, public via Montgomery form).
- `transport::connection::AuthenticatedConnection`: a stateful `&mut self` wrapper around a `Framed<S, FrameCodec>` stream plus a `snow::TransportState`, with async `send` / `recv` / `close`.
- Version-byte preamble (single `0x01` byte) sent before the first Noise frame.
- Handshake timeout (slowloris defense).
- Unit tests over `tokio::io::duplex` (happy path + error taxonomy).
- Error taxonomy funnelled through `CoreError::Transport(String)` with a `handshake: ` prefix.

**Out of scope**

- **PSK lookup by peer identity.** The signatures accept `invite_psk: Option<&[u8; 32]>` on both sides, and both the PSK and no-PSK paths are implemented. But "given an incoming handshake, which PSK should the responder use?" is deferred to 1.D — the 1.D invite flow will ship an invite-identifier pre-handshake so the responder can pre-select the PSK. For 1.B, tests that exercise the PSK path explicitly pass the same PSK to both sides.
- MLS external-PSK binding — 1.C consumes `h_transport` but we just expose it.
- Concurrent send + recv on a single connection — 1.E will add split support if delivery needs it.
- Connection pooling, reconnection, keepalive — 1.E.
- Real-Tor integration test — extending `arti_echo` to negotiate Noise is nice-to-have but not an exit criterion for 1.B; the duplex tests validate the state machine, and 1.E will re-use the primitives over Tor.

## Locked decisions (settled during brainstorming)

| Decision | Choice |
|---|---|
| Ed25519 → X25519 bridge | (B) On-the-fly birational conversion (libsodium-style). Private half: SHA-512 clamp of Ed25519 seed. Public half: Edwards-Y → Montgomery-U. No new wire fields. |
| Msg3 on the wire | (a) Reuse `Frame::NoiseInit` for both msg1 and msg3 (direction-based, not index-based). No 1.A retrofit. |
| `AuthenticatedConnection` shape | (ii) Stateful wrapper, `&mut self` async methods. Replaces the existing stub's `mpsc::{Sender, Receiver}<Frame>` fields. |
| PSK semantics for 1.B | (β) Signatures keep `Option<&[u8; 32]>`. Both paths implemented + tested. Responder takes the PSK directly (no lookup closure) — 1.D will solve "which PSK?" via an invite-identifier pre-handshake. |
| Version preamble | One byte, value `0x01`. Sent by initiator before msg1. Responder reads and validates. Lives in `transport::connection` / the handshake functions, NOT in `FrameCodec`. |

## Architecture

All changes inside `crates/core/src/`.

```
identity/key.rs              MODIFY: add IdentityKey::noise_static_secret()
identity/key.rs              MODIFY: add IdentityKey::ed25519_pub_to_x25519 static helper
identity/derive.rs           MODIFY: (no new labels — (B) avoids a new HKDF label)
transport/noise.rs           FILL:   handshake_initiator, handshake_responder,
                                     HANDSHAKE_TIMEOUT, internal helpers for
                                     frame-level I/O during handshake
transport/connection.rs      REWRITE: AuthenticatedConnection is a stateful wrapper;
                                     drop mpsc fields; implement send/recv/close
transport/mod.rs             MODIFY: twin-arm re-export AuthenticatedConnection
                                     for test_exports (pattern from 1.A)
lib.rs                       MODIFY: add AuthenticatedConnection + handshake fns +
                                     HandshakeOutcome to test_exports
error.rs                     NO CHANGE: reuse CoreError::Transport(String) with
                                     "handshake: {detail}" prefix
```

**Tests:**

```
transport/noise.rs                        #[cfg(test)] mod tests: duplex-based
                                          handshake unit tests (happy + every
                                          error branch)
crates/core/tests/noise_handshake.rs      integration test driving the pair
                                          through a real tokio::io::duplex with
                                          both sides as independent tasks
```

No new fuzz target in 1.B. A handshake fuzz harness lives in workstream 4.B.

## Key types

### `IdentityKey` additions

```rust
impl IdentityKey {
    /// Derive the X25519 static secret used by Noise_XK.
    ///
    /// Implementation: SHA-512 of the Ed25519 seed, then X25519 clamp the
    /// first 32 bytes, matching libsodium's `crypto_sign_ed25519_sk_to_curve25519`.
    /// This is the industry-standard way to bridge an Ed25519 identity to
    /// an X25519 DH key without introducing a second keypair.
    pub(crate) fn noise_static_secret(&self) -> Zeroizing<[u8; 32]>;

    /// The matching X25519 public key (Montgomery form of the Ed25519
    /// verifying key's compressed Edwards-Y).
    pub(crate) fn noise_static_public(&self) -> [u8; 32];
}

/// Convert a peer's Ed25519 verifying key (the identity key carried in
/// ContactCards and invites) into its X25519 public key for Noise DH.
///
/// Decompresses the Edwards Y-coordinate and emits the Montgomery U. The
/// mapping is a standard birational morphism between the two forms of the
/// underlying curve25519 group.
pub(crate) fn ed25519_pub_to_x25519(pk: &ed25519_dalek::VerifyingKey) -> [u8; 32];
```

`noise_static_secret` returns `Zeroizing<[u8; 32]>` so it drops safely even if the caller inadvertently clones.

### `HandshakeOutcome`

```rust
pub struct HandshakeOutcome {
    /// Peer's X25519 static public key (as received during Noise).
    /// The caller maps this back to Ed25519 identity via a ContactCard
    /// lookup: iterate known contacts, convert each stored Ed25519 pubkey
    /// to X25519 via `ed25519_pub_to_x25519`, compare. That resolver is
    /// outside 1.B's scope — `handshake_responder` returns the raw X25519
    /// pub; 1.D / 1.E wire the identity lookup.
    pub peer_x25519: [u8; 32],

    /// 32-byte transport-to-MLS binding token.
    /// `HKDF-SHA256(noise_handshake_hash, "skattr-binding-v1")`, 32 bytes
    /// of output keying material. Phase 1.C injects this as an external
    /// PSK into the first MLS Commit.
    pub h_transport: Zeroizing<[u8; 32]>,
}
```

Ed25519 resolution happens above this layer — `handshake_responder` doesn't know which contact it's talking to, just that it completed an authenticated handshake with some X25519 public key.

### Handshake entry points

```rust
/// Handshake timeout — whole handshake (3 messages + preamble) must
/// complete inside this window. Defends against slowloris.
pub const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Version byte written before the first Noise frame.
pub(crate) const PROTOCOL_VERSION: u8 = 0x01;

pub async fn handshake_initiator<S>(
    stream: S,
    identity: &IdentityKey,
    peer_static_x25519: &[u8; 32],
    invite_psk: Option<&[u8; 32]>,
) -> Result<(AuthenticatedConnection<S>, HandshakeOutcome)>
where
    S: AsyncRead + AsyncWrite + Unpin + Send;

pub async fn handshake_responder<S>(
    stream: S,
    identity: &IdentityKey,
    invite_psk: Option<&[u8; 32]>,
) -> Result<(AuthenticatedConnection<S>, HandshakeOutcome)>
where
    S: AsyncRead + AsyncWrite + Unpin + Send;
```

Both wrap the entire handshake in `tokio::time::timeout(HANDSHAKE_TIMEOUT, ...)`. Timeout surfaces as `CoreError::Transport("handshake: timeout")`.

### `AuthenticatedConnection`

```rust
pub struct AuthenticatedConnection<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    peer_x25519: [u8; 32],
    h_transport: Zeroizing<[u8; 32]>,
    framed: Framed<S, FrameCodec>,
    transport: snow::TransportState,
}

impl<S> AuthenticatedConnection<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    pub fn peer_x25519(&self) -> &[u8; 32] { &self.peer_x25519 }
    pub fn h_transport(&self) -> &[u8; 32] { &self.h_transport }

    /// Encrypt `frame`'s payload under the Noise transport cipher and
    /// send the resulting Frame::MlsApp (or the frame unchanged if it's
    /// a control frame that's already been "noise-wrapped" — in 1.B
    /// every application frame goes through this path).
    pub async fn send(&mut self, frame: Frame) -> Result<()>;

    /// Read next frame, decrypt under Noise transport cipher.
    pub async fn recv(&mut self) -> Result<Option<Frame>>;

    /// Send Bye, shut down the stream, consume self.
    pub async fn close(mut self) -> Result<()>;
}
```

**Send/recv semantics for 1.B:** `send(frame)` serialises `frame` via `FrameCodec::encode` into a byte buffer, encrypts the buffer with `snow::TransportState::write_message`, then sends those ciphertext bytes wrapped in a single `Frame::MlsApp(ciphertext)` on the wire. `recv` inverts: read one `MlsApp` frame, decrypt to cleartext bytes, decode as the inner `Frame`, return.

This "frame-in-frame" composition is deliberate. The outer `MlsApp` wrapper is what the wire sees (opaque to any observer); the inner `Frame` is what the application handles. For 1.B's tests we only exercise a couple of inner variants (Ping, Bye, Error); 1.E will layer outbound sequencing on top.

Alternative considered: encrypt raw bytes directly into `Frame::MlsApp(bytes)` and let the caller decide payload shape. Rejected for 1.B because consumers expect to send `Frame` values; doing the double-framing in `AuthenticatedConnection::send` keeps the public API symmetric with `FrameCodec`.

## Handshake wire flow

Per design doc §2, with (a) Frame-reuse applied:

```
initiator                                     responder
───────────────────────────────────────────────────────
write: [0x01]                    →
(version preamble)

Frame::NoiseInit(msg1: -e)       →
                                              read version, validate = 0x01
                                              read Frame::NoiseInit → snow.read_message(msg1)
                                 ←            Frame::NoiseResp(msg2: -e, ee, s, es)
read Frame::NoiseResp
→ snow.read_message(msg2)
→ snow.write_message(msg3)

Frame::NoiseInit(msg3: -s, se,   →
  psk3 iff Some)
                                              read Frame::NoiseInit → snow.read_message(msg3)
                                              → get_handshake_hash → h_transport
→ get_handshake_hash                          → into_transport_mode()
→ h_transport
→ into_transport_mode()

────────── handshake complete ──────────
(both sides now have AuthenticatedConnection wrapping the stream)
```

The PSK, when provided, is set on the `snow::Builder` via `.psk(3, psk_bytes)` BEFORE `into_stateless()` / `.build_handshake_state()`. Both sides must have the same PSK for msg3 to decrypt. If initiator has PSK and responder doesn't (or vice versa), msg3's AEAD tag fails → `CoreError::Transport("handshake: authentication failed")`.

## Error surface

All handshake errors funnel through `CoreError::Transport(String)`. Messages are fixed strings plus optional low-detail context so logs don't leak keys or bytes:

| Condition | Surfaced as |
|---|---|
| First byte not `PROTOCOL_VERSION` | `"handshake: unsupported version: {byte:#04x}"` |
| `snow::Error::Decrypt` during `read_message` | `"handshake: authentication failed"` |
| Frame codec error inside handshake | `"handshake: malformed: {detail}"` (detail from `CoreError::Frame`) |
| Unexpected frame type (e.g. NoiseResp when expecting NoiseInit) | `"handshake: malformed: unexpected frame type 0x{:02X}"` |
| Stream EOF mid-handshake | `"handshake: stream closed"` |
| `tokio::time::timeout` elapsed | `"handshake: timeout"` |
| Builder error (shouldn't happen in practice; indicates programmer bug) | `"handshake: builder: {detail}"` |

The plan's "UnknownPeer" category is implied by the responder's caller rejecting peer_x25519 after a successful handshake (contact lookup returns nothing). That's not inside `handshake_responder`; it's the caller's responsibility. We document this clearly.

Replay is NOT addressed in 1.B; see the plan's workstream 4 (hardening) and the threat model's A2 residual-exposure note.

## Testing strategy

### Unit tests (in `transport/noise.rs`)

All run over `tokio::io::duplex(N)` — half buffer, both ends in the same process as independent tasks spawned via `tokio::spawn` or `tokio::join!`.

1. **happy_path_no_psk** — initiator + responder both succeed; both `h_transport` values are byte-equal; both `peer_x25519` values match the opposite identity's `noise_static_public`.
2. **happy_path_with_psk** — same, but both sides pass the same 32-byte PSK.
3. **psk_mismatch_fails** — initiator PSK = `[0xAA; 32]`, responder PSK = `[0xBB; 32]`; both handshakes return `CoreError::Transport("handshake: authentication failed")`.
4. **responder_without_psk_when_initiator_has_one** — should fail the same way; confirms that unilateral PSK is detected.
5. **wrong_peer_static** — initiator targets a random X25519 pubkey, not the responder's real one. Responder's `read_message(msg3)` fails auth (X25519 shared secret mismatch).
6. **malformed_first_frame** — initiator sends garbage bytes instead of the version preamble + NoiseInit. Responder returns `"handshake: malformed: ..."` or `"handshake: unsupported version: ..."` depending on exactly where the corruption lands.
7. **wrong_version_byte** — initiator writes `0x02` instead of `0x01` preamble; responder returns `"handshake: unsupported version"`.
8. **timeout** — one side writes the version byte and then stops. The other side's whole-handshake timer (`HANDSHAKE_TIMEOUT`, scaled down to 100 ms for the test via a cfg override or direct `tokio::time::timeout` at the call site) fires.
9. **h_transport_is_hkdf_of_handshake_hash** — capture the snow handshake hash directly in the test (there's a `get_handshake_hash` on `snow::TransportState`) and verify `h_transport == HKDF-SHA256(handshake_hash, INFO_TRANSPORT_BINDING_V1)` for 32 output bytes.
10. **send_recv_round_trip_post_handshake** — after successful handshake, initiator `send(Frame::Ping)`, responder `recv()` returns `Frame::Ping`. Confirms the Noise transport cipher is wired through `send`/`recv` correctly.

### Integration test (`crates/core/tests/noise_handshake.rs`)

Gated `#[cfg(feature = "test-harness")]`. One test: run both halves concurrently via `tokio::join!`, assert the `h_transport` bytes match and the post-handshake round-trip of a small Frame works.

### No fuzz target in 1.B

Deferred to workstream 4.B. Existing proptest infrastructure from 1.A does not naturally extend to a stateful handshake.

## Dependencies

All already in `crates/core/Cargo.toml`:

- `snow = "0.9"` (line 52 per the exploration report)
- `hkdf = "0.12"`
- `sha2 = "0.10"` (for HKDF and for Ed25519 → X25519 secret conversion)
- `x25519-dalek = "2"` (for `PublicKey::from(&StaticSecret)` and Montgomery types if used)
- `ed25519-dalek = "2"` (already; for `VerifyingKey` decompression)
- `curve25519-dalek = "4"` — only needed for `CompressedEdwardsY::decompress().to_montgomery()`. Check if already present; add if not (it's a transitive dep of ed25519-dalek and x25519-dalek so should be accessible).
- `zeroize = "1"`
- `tokio-util` with `codec` feature (already present from 1.A)
- `tokio` with `time`, `macros`, `io-util`, `rt-multi-thread` features for timeout + test infra

No new third-party crypto. No hand-rolled primitives.

## Risks

- **Birational map pinning.** Getting the Ed25519 → X25519 conversion wrong is subtle and silently breaks interop. Mitigation: a unit test that hard-codes a known-good Ed25519 seed and its expected X25519 public key (cribbed from a libsodium test vector), asserting bitwise equality.
- **`snow` API drift.** `snow` 0.9 is stable but API names may shift. Mitigation: pin the version in `Cargo.toml`; if snow bumps, the test suite catches regressions immediately.
- **`tokio::io::duplex` resource sizing.** A 0-byte or too-small buffer will deadlock the handshake. Use at least 8 KiB.
- **PSK path partial coverage.** Because 1.B doesn't solve "which PSK for an unknown initiator," the PSK unit tests hard-code the PSK on both sides. This leaves the "mismatched-lookup" failure mode untested until 1.D. Acknowledged and tracked in 1.D's spec.

## Exit criteria

1. All unit tests in `transport/noise.rs` pass.
2. The integration test in `crates/core/tests/noise_handshake.rs` passes under `--features test-harness`.
3. `cargo fmt --check` / `cargo clippy --all-features -- -D warnings` / `cargo test --workspace --all-features --release` all green.
4. `HandshakeOutcome.h_transport` is `HKDF-SHA256(handshake_hash, INFO_TRANSPORT_BINDING_V1)` for 32 output bytes — verified by unit test 9 above.
5. CHANGELOG bullet and CLAUDE.md Repository-state paragraph updated with "Phase 1.B complete."
6. No new fuzz target, no PSK-lookup implementation, no Tor-level integration test (these are explicitly out of scope).
