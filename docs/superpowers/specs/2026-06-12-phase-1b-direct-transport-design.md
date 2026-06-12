# Phase 1B — Direct P2P Transport Wiring + Regression Guardrail (Design)

**Date:** 2026-06-12
**Status:** Approved design; implementation plan to follow.
**Roadmap:** `docs/superpowers/specs/2026-06-12-v1.0-roadmap.md` (Phase 1 items 1, 2, 5).
**Audit refs:** T0-1 (no production message transport) + the latent inbound auth gap.
**Predecessor:** Phase 1A (inbound correctness) merged to `master` 2026-06-12.

---

## 1. Problem

The audit's defining finding is **"green tests, dead production path."** `Daemon::run`
bootstraps Arti and calls `publish_onion`, but it never spawns an accept loop, never runs
a Noise handshake, and never dials a peer. The two-daemon flows pass only because tests
hand-wire transport through `DeliveryHub::ingest` / `handshake_*` via the `test-harness`
`test_exports`. In production wiring, two daemons cannot exchange a message.

Concrete gaps confirmed in the code (2026-06-12):

- `Daemon::run` (`crates/core/src/daemon/state.rs`) takes `rend_requests` from the
  `TorRuntime` but never consumes them; no `OnionListener` is spawned.
- `handshake_initiator` / `handshake_responder` (`transport/noise.rs`) are called only by
  tests.
- `DeliveryHub<S>`'s per-peer actor (`delivery/peer.rs`) is purely passive: it receives a
  connection via `PeerCtrl::ReplaceConn` and never dials. No dial logic exists anywhere.
- `TorRuntime::connect(onion, port)` exists but is used only by the mailbox factory, never
  for peer-to-peer dials.
- **No reverse map** from a peer's Noise static X25519 key back to a `PublicKey`/contact
  exists. `transport/noise.rs` explicitly defers this to "the contact layer."
- `Daemon::run` is monomorphized to `arti_client::DataStream`; there is no transport
  abstraction and therefore no way to drive the assembly in CI without Tor.

## 2. Goal & scope

**Goal:** two real daemons exchange messages **in both directions** through the production
`Daemon::run` assembly, online (direct P2P over the onion transport), proven by a
CI-runnable integration test that drives the real wiring — not `test_exports`.

### In scope
1. A `Transport` abstraction with an `ArtiTransport` (production) and an in-process
   `LoopbackTransport` (test) implementation.
2. An **inbound accept loop** wired into the daemon assembly: handshake each inbound
   stream, resolve the authenticated peer, and **reject unknown/unauthenticated peers
   before `DeliveryHub::ingest`**.
3. An **outbound dialer**: on first send (or reconnect) to a peer with no live connection,
   resolve the peer's onion from their ContactCard, dial, run `handshake_initiator`, and
   install the connection on the per-peer actor.
4. A `PublicKey`-from-X25519 **reverse resolver** in the contact layer.
5. The **regression guardrail**: a fast, non-`#[ignore]` integration test that runs two
   real daemon assemblies over `LoopbackTransport` and asserts bidirectional direct
   delivery, plus an `#[ignore]`-gated real-Tor variant of the same script.

### Out of scope (deferred, with the phase that owns each)
- **Direct→mailbox fallback trigger** (Task 20.5 / T1-6) — **Phase 2.** Mailbox *receive*
  already works (Phase 1A). 1B wires only the *direct* path; the auto-trigger that deposits
  to a mailbox on sustained direct-delivery failure, and the retarget/retry correctness,
  land in Phase 2. The guardrail's offline/mailbox half is therefore also Phase 2.
- **`h_transport` ↔ MLS binding** (T1-1) — **Phase 2.** 1B uses normal identity-keyed XK
  handshakes with **no** invite PSK; injecting `AuthenticatedConnection::h_transport()` as
  the external PSK into the first MLS Commit is a separate, deliberate Phase 2 decision.
- **Real onion-key rotation** (Task 23.5), multi-member groups — unchanged deferrals.

### Non-goals
No change to the wire format, the Noise pattern, the MLS ciphersuite, or the
`Daemon::run` public signature.

## 3. Architecture

A `Transport` trait abstracts the two capabilities the daemon needs from Arti — **publish**
(obtain an onion address and a stream of inbound connections) and **dial** (open an
outbound stream to an onion). `Daemon::run` stays a thin public wrapper that constructs an
`ArtiTransport` and delegates to a generic `run_with_transport<T: Transport>` carrying the
**entire** post-bootstrap assembly. The guardrail calls `run_with_transport` with a
`LoopbackTransport`, so two literal daemon assemblies handshake and exchange messages with
no Tor and no `test_exports`.

```
Daemon::run(data_dir, passphrase, config, config_path, ready, shutdown)   // public, unchanged signature
  └─ build ArtiTransport(TorRuntime::bootstrap(...))
  └─ run_with_transport(transport, data_dir, passphrase, config, ..)      // generic; the tested seam
        ├─ (onion, inbound) = transport.publish(hs_key_path, seed, nick).await
        ├─ spawn accept loop:  inbound.recv() → handshake_responder
        │                      → find_by_noise_x25519 → (known ? hub.ingest : drop+warn)
        ├─ DeliveryHub<T::Stream>::new_with_inbound(pool, daemon_inbound)
        │     with injected OutboundDial = TransportDial(transport, identity, pool)
        ├─ IPC server, mailbox PollScheduler, retention sweep, taps   // as today
        └─ await shutdown
```

### 3.1 The `Transport` trait

```rust
// crates/core/src/transport/transport.rs  (new)

/// Fixed virtual port the onion service listens on and dialers connect to.
pub(crate) const ONION_PORT: u16 = 9735;

/// Receiver side of inbound, post-rendezvous, pre-handshake byte streams.
pub(crate) struct InboundStreams<S>(pub(crate) tokio::sync::mpsc::Receiver<S>);

/// Abstracts the daemon's transport: publish an onion + receive inbound
/// connections, and dial outbound onions. Implemented by `ArtiTransport`
/// (production) and `LoopbackTransport` (in-process, test-harness).
pub(crate) trait Transport: Send + Sync + 'static {
    type Stream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static;

    /// Publish this daemon's onion service and return its address plus the
    /// stream of accepted inbound connections.
    async fn publish(
        &self,
        hs_key_path: &std::path::Path,
        seed: &crate::identity::Seed,
        nickname: &str,
    ) -> crate::error::Result<(String, InboundStreams<Self::Stream>)>;

    /// Open an outbound connection to `onion:port`.
    async fn dial(&self, onion: &str, port: u16) -> crate::error::Result<Self::Stream>;
}
```

> Note on async-trait: the workspace targets stable Rust. If the pinned toolchain
> (`rust-toolchain.toml`, 1.95.0) supports `async fn` in traits with the needed
> object/− bounds, use it directly; otherwise use the already-present `async-trait`
> dependency pattern used elsewhere in the crate. The plan resolves this against the real
> toolchain — the trait shape above is the contract either way. `Transport` is used as a
> generic bound (`run_with_transport<T: Transport>`), not a trait object, so RPITIT is
> sufficient and no boxing of `Transport` itself is required.

### 3.2 `ArtiTransport` (production)

```rust
// crates/core/src/transport/arti_transport.rs  (new)
pub(crate) struct ArtiTransport {
    runtime: tokio::sync::Mutex<TorRuntime>,        // guards the one-time publish (&mut self)
    client: arti_client::TorClient<...>,            // cheap Clone handle for concurrent dials (&self)
}
```

- `publish`: locks `runtime`, calls `TorRuntime::publish_onion` for the address, takes
  `rend_requests` via `rend_requests_take`, spawns the existing `OnionListener`, and wraps
  its `accepted` `mpsc::Receiver<DataStream>` as `InboundStreams<DataStream>`. Called once.
- `dial`: uses the cloned `client` handle directly (`client.connect("{onion}:{port}")`) so
  concurrent dials do **not** serialize behind the publish `Mutex`. `TorRuntime::connect`
  is already `&self`; the plan extracts the same call against the held client (or exposes a
  `&self` dial on `TorRuntime` and holds the runtime in an `Arc`). Either shape avoids
  locking on the hot dial path — the plan picks whichever matches the real `TorRuntime`
  ownership of its `TorClient`.
- `Stream = arti_client::DataStream` (unchanged production stream type).

`TorRuntime::bootstrap` remains in `Daemon::run` (the `ArtiTransport` wraps the already
bootstrapped runtime); publishing happens inside `run_with_transport` via the trait.

### 3.3 `LoopbackTransport` (test-harness)

```rust
// crates/core/src/transport/loopback.rs  (#[cfg(feature = "test-harness")])
#[derive(Clone)]
pub struct LoopbackNet(Arc<Mutex<HashMap<String, mpsc::Sender<DuplexStream>>>>);
pub struct LoopbackTransport { net: LoopbackNet, my_onion: String }
```

- A shared `LoopbackNet` registry maps fake onion addresses → an inbound-stream sender.
- `publish`: registers `my_onion` in the registry, returns `(my_onion, InboundStreams(rx))`.
- `dial(onion, _port)`: looks up the target's sender, creates a `tokio::io::duplex(BUF)`
  pair, sends one half to the target's inbound channel, returns the other half.
- `Stream = tokio::io::DuplexStream`.
- Two daemons constructed with `LoopbackTransport`s sharing one `LoopbackNet` can dial each
  other by their (fake) onion addresses exactly as over Tor.

### 3.4 Peer resolution (reverse map)

```rust
// crates/core/src/storage/contacts.rs  (add method)
impl ContactRepo<'_> {
    /// Resolve a peer's Noise static X25519 key to its Ed25519 identity by
    /// converting each known contact's identity via `ed25519_pub_to_x25519`
    /// and comparing in constant time. Returns None if no contact matches.
    pub(crate) fn find_by_noise_x25519(&self, x: &[u8; 32]) -> Result<Option<PublicKey>>;
}
```

Closes the gap the Noise module explicitly left to the contact layer. Iterating all
contacts is O(contacts) per inbound handshake — fine for v1.0's contact-scale; a comment
notes it. Comparison uses constant-time equality to avoid a timing oracle on the static
key — `subtle::ConstantTimeEq` if `subtle` is already in the dependency graph (the plan
checks), otherwise a small manual constant-time byte compare; do **not** add a new
dependency just for this.

### 3.5 Outbound dial (`OutboundDial`)

```rust
// crates/core/src/delivery/dial.rs  (new)
pub(crate) trait OutboundDial<S>: Send + Sync + 'static {
    async fn dial(&self, peer: PublicKey) -> crate::error::Result<AuthenticatedConnection<S>>;
}

pub(crate) struct TransportDial<T: Transport> {
    transport: Arc<T>,
    identity: Arc<IdentityKey>,
    pool: Arc<Pool>,
}
```

`TransportDial::dial(peer)`:
1. `onion = ContactRepo::new(&pool).latest_card(&peer)?.ok_or(NoCard)?.body.onion`.
2. `stream = transport.dial(&onion, ONION_PORT).await?`.
3. `peer_x25519 = ed25519_pub_to_x25519(peer)` (the contact's identity → its Noise static).
4. `(conn, _outcome) = handshake_initiator(stream, &identity, &peer_x25519, None).await?`
   (no invite PSK — established contacts use the plain identity-keyed XK handshake).
5. Return `conn`.

The hub holds `Option<Arc<dyn OutboundDial<S>>>` and hands a clone to each spawned actor.
The actor dials on demand: when it holds a queued job (or a retry tick fires) and `conn`
is `None`, it `await`s `dialer.dial(peer)` and installs the result through the existing
`ReplaceConn`/drain path (so concurrent dials and mid-flight replacement are already
handled). This is precisely the TODO-20.5 seam, used here for **direct redial only**; the
*mailbox* fallback trigger remains Phase 2.

### 3.6 Inbound accept loop

A `pub(crate) async fn run_accept_loop<S>(inbound, identity, contacts_pool, hub)` spawned by
`run_with_transport`:

```
loop {
  stream = inbound.recv().await? ;                       // pre-handshake bytes
  spawn {
    (conn, outcome) = handshake_responder(stream, &identity, None).await? ;
    match ContactRepo::new(&pool).find_by_noise_x25519(outcome.peer_x25519)? {
      Some(peer) => hub.ingest(peer, conn).await,        // authorized
      None       => { warn!("inbound: rejected unknown peer"); drop(conn); }  // reject
    }
  }
}
```

Each accept is handled on its own task so a slow handshake cannot stall the loop. A failed
handshake logs and drops; an unknown peer logs and drops; neither persists anything.

## 4. Data flow

**Outbound (cold peer):** IPC `SendMessage` → `dispatch::send_message` persists +
`hub.send(peer, id, ciphertext)` → actor has no `conn` → `dialer.dial(peer)` (resolve
onion → dial → `handshake_initiator`) → conn installed → `Frame` sent → peer's inbound
pipeline decrypts + persists + emits `MessageReceived`. Reconnect after a drop reuses the
same on-demand dial.

**Inbound:** transport rendezvous → accept loop → `handshake_responder` → resolve peer →
`hub.ingest(peer, conn)` → actor reads frames → `DaemonInbound::dispatch` (unchanged) →
persist + `Event::MessageReceived`.

**First contact / Welcome:** after `AddContact`, both sides know each other's identity
(inviter from the consumed KeyPackage, invitee from the invite link), so the inviter's
`send_welcome` rides the same dial path: a normal identity-keyed XK handshake, then
`Frame::MlsWelcome`. No invite PSK is used on the transport in 1B.

## 5. Authorization & error handling

- **Unknown inbound peer → reject + drop + `warn!`**, before any `ingest`. This closes the
  audit's latent auth gap (previously nothing resolved or gated the inbound peer).
- **Dial failure / handshake failure / no ContactCard / onion unresolvable** → `Err`
  surfaced to the sender's delivery oneshot; the outbox/message row is retained (no loss),
  retried on a later send. Logged without leaking onion/pubkey at `info`+.
- **Concurrent dials to the same peer** → existing `ReplaceConn` close-old/keep-new logic;
  at most one live connection per peer actor.
- **Logging discipline:** no pubkeys, onions, ciphertext, or message bodies at `info`+;
  `warn!` lines carry only error strings / static text (matching Phase 1A's pattern).

## 6. Testing

- **`find_by_noise_x25519` unit test** (`storage/contacts.rs`): resolves a known contact's
  derived X25519 to its identity; returns `None` for a stranger.
- **`LoopbackTransport` unit test** (`transport/loopback.rs`): `publish` + `dial`
  round-trips bytes across a duplex stream via the shared registry.
- **Guardrail — fast, non-`#[ignore]`, no Tor** (`crates/tests/src/daemon_run_direct.rs`):
  two `run_with_transport` daemons share one `LoopbackNet`; the test drives, through each
  daemon's real IPC/dispatch surface, `invite → add → (Welcome delivered direct) →
  Alice→Bob text → Bob→Alice text`, and asserts delivery on each side via
  `Event::MessageReceived` and persisted rows. **This is the regression guardrail the
  roadmap mandates** — it exercises the production assembly, not `test_exports`.
- **`#[ignore]` real-Tor variant** (`crates/tests/src/daemon_run_direct_tor.rs` or a gated
  case): the same script over `ArtiTransport`, run locally/nightly.
- **No regressions:** `delivery_kill_mid_message` and `cli_two_daemons` keep using
  `ingest`/duplex directly and are untouched.

## 7. Exit criteria

- The fast guardrail passes in CI and proves **bidirectional direct delivery** between two
  daemon assemblies with no Tor and no `test_exports`.
- An inbound connection from an unknown peer is rejected before `ingest`.
- `Daemon::run`'s public signature is unchanged; CLI and UI build and run unmodified.
- `cargo fmt --check`, `cargo clippy --workspace --exclude skattr-ui --all-targets -- -D
  warnings`, and the full non-ignored test suite are green.

## 8. Module-visibility notes

`Transport`, `InboundStreams`, `ArtiTransport`, `LoopbackTransport`, `OutboundDial`,
`TransportDial`, `run_with_transport`, `run_accept_loop`, and `find_by_noise_x25519` are all
`pub(crate)` (or `test-harness`-gated `pub` where a test in another crate needs them — e.g.
`LoopbackTransport`, `LoopbackNet`, and `run_with_transport` must be reachable from
`crates/tests`). No widening of `core`'s public API surface (`daemon`, `identity`,
`envelope`, `invite`, `contact`, `error`) beyond what the guardrail strictly requires, and
no new wire-format types.
