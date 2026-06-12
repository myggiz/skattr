# Phase 1B — Direct P2P Transport Wiring + Guardrail — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two real daemon assemblies exchange messages in both directions through the production `Daemon::run` wiring (online, direct P2P), proven by a CI-runnable integration test that drives the real assembly — not `test_exports`.

**Architecture:** Introduce a `Transport` trait (publish onion + receive inbound streams; dial outbound) with `ArtiTransport` (production) and `LoopbackTransport` (in-process, test). `Daemon::run` stays a thin public wrapper that builds `ArtiTransport` and delegates to a generic `run_with_transport<T: Transport>` carrying the whole assembly; `run_with_transport` spawns an inbound accept loop (handshake → resolve peer → reject-unknown → `ingest`) and injects an on-demand `OutboundDial` into the `DeliveryHub` so the per-peer actor dials when it has no live connection. A new `ContactRepo::find_by_noise_x25519` reverse-resolves an authenticated peer's X25519 static key to its identity.

**Tech Stack:** Rust (stable, pinned 1.95.0), tokio, `arti-client`/`tor-hsservice` (Arti), `snow` (Noise_XK), OpenMLS, rusqlite. New tests use `tokio::io::duplex` + a loopback registry; no Tor needed for the fast guardrail.

**Spec:** `docs/superpowers/specs/2026-06-12-phase-1b-direct-transport-design.md`. Read it for rationale; this plan is the executable form.

**Conventions (read once):**
- cargo is NOT on PATH — prefix every cargo command with `. "$HOME/.cargo/env" && `.
- Run tests/clippy with `--features test-harness` (the repo's documented test feature; clippy without it fails to build a pre-existing integration test).
- Every `.rs` file carries a GPLv3 license header — copy the header block from a sibling file into any NEW file.
- NO `unwrap()`/`expect()` in non-test (`src/`) code — use `?` and typed `CoreError` variants. Test code may use `.unwrap()`.
- Never log pubkeys, onions, ciphertext, or message bodies at `info`+; `warn!` may carry error strings / static text only.
- Commit messages end with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

---

## File map

- **Modify** `crates/core/src/storage/contacts.rs` — add `find_by_noise_x25519` (Task 1).
- **Create** `crates/core/src/transport/transport.rs` — `Transport` trait, `InboundStreams`, `ONION_PORT` (Task 2).
- **Create** `crates/core/src/transport/loopback.rs` — `LoopbackNet` + `LoopbackTransport` (Task 3, `test-harness`).
- **Create** `crates/core/src/transport/arti_transport.rs` — `ArtiTransport` (Task 4).
- **Modify** `crates/core/src/transport/mod.rs` — module decls + re-exports for the three new modules (Tasks 2–4).
- **Create** `crates/core/src/delivery/dial.rs` — `OutboundDial` trait + `TransportDial` (Task 5).
- **Modify** `crates/core/src/delivery/hub.rs` — hold + thread an injected dialer (Task 5).
- **Modify** `crates/core/src/delivery/peer.rs` — actor dials on demand when `conn` is `None` (Task 5).
- **Modify** `crates/core/src/delivery/mod.rs` — `mod dial;` + re-exports (Task 5).
- **Create** `crates/core/src/daemon/accept.rs` — `run_accept_loop` (Task 6).
- **Modify** `crates/core/src/daemon/mod.rs` — `mod accept;` (Task 6).
- **Modify** `crates/core/src/daemon/state.rs` — extract `run_with_transport<T>`; `Daemon::run`/`run_with_sink` build `ArtiTransport` + spawn accept loop + inject dialer (Task 7).
- **Modify** `crates/core/src/lib.rs` — `test_exports` adds `LoopbackTransport`, `LoopbackNet`, `run_with_transport` (Tasks 3, 7).
- **Create** `crates/tests/src/daemon_run_direct.rs` — the fast loopback guardrail + an `#[ignore]` real-Tor twin (Task 8).
- **Modify** `crates/tests/src/lib.rs` (or the test crate's module list) — register the new test module (Task 8).

---

## Task 1: `ContactRepo::find_by_noise_x25519` reverse resolver

**Why:** An inbound handshake yields the peer's Noise static X25519 key (`HandshakeOutcome.peer_x25519`), but nothing maps it back to a contact `PublicKey`. The accept loop (Task 6) needs this to authorize/attribute the connection. This is the gap `transport/noise.rs` explicitly left to the contact layer.

**Files:**
- Modify: `crates/core/src/storage/contacts.rs` (struct `ContactRepo<'p>`, impl block; `list()` exists as the query idiom to mirror)
- Test: `crates/core/src/storage/contacts.rs` (the existing `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/core/src/storage/contacts.rs`. VERIFY the existing test idiom first (how other tests build a `Pool::in_memory()` and `Contact`s) and mirror it. `noise_static_public` / `ed25519_pub_to_x25519` are `pub(crate)` in `identity::key` — reachable here. A contact's stored identity is an Ed25519 `PublicKey`; its Noise static is `ed25519_pub_to_x25519(verifying_key_of(identity))`. There is a helper for "the X25519 of an identity key" (`IdentityKey::noise_static_public`), but here we need it for a *foreign* `PublicKey` (no secret). Use the public-key conversion: `crate::identity::key::ed25519_pub_to_x25519(&VerifyingKey::from_bytes(&pk.0)?)`.

```rust
#[test]
fn find_by_noise_x25519_resolves_known_contact_and_rejects_stranger() {
    use crate::identity::IdentityKey;

    let pool = Pool::in_memory();
    let repo = ContactRepo::new(&pool);

    let alice = IdentityKey::generate().unwrap();
    let alice_pk = alice.public();
    repo.upsert(&Contact {
        identity: alice_pk,
        display_name: None,
        added_at: 0,
        card: None,
        muted: false,
    })
    .unwrap();

    // Alice's Noise static X25519 (she's a contact; we know only her public key).
    let alice_x = crate::identity::key::ed25519_pub_to_x25519(
        &ed25519_dalek::VerifyingKey::from_bytes(&alice_pk.0).unwrap(),
    );
    assert_eq!(repo.find_by_noise_x25519(&alice_x).unwrap(), Some(alice_pk));

    // A stranger's X25519 resolves to None.
    let stranger = IdentityKey::generate().unwrap();
    let stranger_x = crate::identity::key::ed25519_pub_to_x25519(
        &ed25519_dalek::VerifyingKey::from_bytes(&stranger.public().0).unwrap(),
    );
    assert_eq!(repo.find_by_noise_x25519(&stranger_x).unwrap(), None);
}
```

> **VERIFY:** the path to `ed25519_pub_to_x25519` — it's a free `pub(crate) fn` in `crates/core/src/identity/key.rs`. If it's not re-exported at `crate::identity::key::ed25519_pub_to_x25519`, either widen its visibility to `pub(crate)` at the module path the test uses, or add a small `pub(crate) fn noise_static_of_public(pk: &PublicKey) -> Result<[u8;32]>` helper next to it and call that from both the test and the new repo method (preferred — DRY, and it centralizes the `VerifyingKey::from_bytes` fallibility). If you add that helper, use it in Step 3 too.

- [ ] **Step 2: Run the test to verify it fails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib --features test-harness storage::contacts::tests::find_by_noise_x25519_resolves_known_contact_and_rejects_stranger`
Expected: FAIL to compile — `find_by_noise_x25519` does not exist yet. (Make it compile-fail, then implement.)

- [ ] **Step 3: Implement `find_by_noise_x25519`**

Add to `impl<'p> ContactRepo<'p>` in `crates/core/src/storage/contacts.rs`:

```rust
    /// Resolve a peer's Noise static X25519 key to its Ed25519 identity by
    /// converting each known contact's identity via `ed25519_pub_to_x25519`
    /// and comparing in constant time. Returns `None` if no contact matches.
    ///
    /// O(contacts) per call — fine for v1.0 contact scale.
    pub(crate) fn find_by_noise_x25519(
        &self,
        x: &[u8; 32],
    ) -> Result<Option<crate::identity::PublicKey>> {
        use subtle::ConstantTimeEq;
        for contact in self.list()? {
            let vk = match ed25519_dalek::VerifyingKey::from_bytes(&contact.identity.0) {
                Ok(vk) => vk,
                Err(_) => continue, // stored key not a valid Ed25519 point; skip
            };
            let cand = crate::identity::key::ed25519_pub_to_x25519(&vk);
            if cand.ct_eq(x).into() {
                return Ok(Some(contact.identity));
            }
        }
        Ok(None)
    }
```

> **VERIFY:** `subtle` is a workspace dep available to skattr-core (confirmed). If `ed25519_pub_to_x25519` is not reachable as `crate::identity::key::ed25519_pub_to_x25519`, use the `noise_static_of_public` helper from Step 1's note instead. `self.list()` returns `Result<Vec<Contact>>` and is the established query idiom. `contact.identity` is a `PublicKey(pub [u8; 32])`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib --features test-harness storage::contacts::tests::find_by_noise_x25519_resolves_known_contact_and_rejects_stranger`
Expected: PASS.

- [ ] **Step 5: Clippy**

Run: `. "$HOME/.cargo/env" && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings`
Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/storage/contacts.rs crates/core/src/identity/key.rs
git commit -m "feat(contacts): find_by_noise_x25519 reverse resolver

Maps an authenticated peer's Noise static X25519 key back to a contact
identity (constant-time compare over known contacts). Needed by the
inbound accept loop to authorize + attribute connections. (T0-1)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 2: `Transport` trait + `InboundStreams` + `ONION_PORT`

**Why:** The abstraction seam. `run_with_transport` (Task 7) is generic over this; `ArtiTransport` (Task 4) and `LoopbackTransport` (Task 3) implement it.

**Files:**
- Create: `crates/core/src/transport/transport.rs`
- Modify: `crates/core/src/transport/mod.rs`

- [ ] **Step 1: Create the trait module**

Create `crates/core/src/transport/transport.rs` (copy the GPLv3 header from `crates/core/src/transport/listener.rs`):

```rust
// <GPLv3 header copied from a sibling file>

//! Transport abstraction: publish an onion + receive inbound connections,
//! and dial outbound onions. Implemented by `ArtiTransport` (production) and
//! `LoopbackTransport` (in-process, test-harness). Lets `run_with_transport`
//! drive the real daemon assembly in CI without Tor.

use crate::error::Result;
use crate::identity::Seed;
use std::path::Path;
use tokio::io::{AsyncRead, AsyncWrite};

/// Fixed virtual port the onion service listens on and dialers connect to.
pub(crate) const ONION_PORT: u16 = 9735;

/// Receiver of inbound, post-rendezvous, pre-handshake byte streams.
pub(crate) struct InboundStreams<S>(pub(crate) tokio::sync::mpsc::Receiver<S>);

impl<S> InboundStreams<S> {
    pub(crate) async fn recv(&mut self) -> Option<S> {
        self.0.recv().await
    }
}

#[async_trait::async_trait]
pub(crate) trait Transport: Send + Sync + 'static {
    type Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static;

    /// Publish this daemon's onion service; return its address + the stream of
    /// accepted inbound connections. Called once at startup.
    async fn publish(
        &self,
        hs_key_path: &Path,
        seed: &Seed,
        nickname: &str,
    ) -> Result<(String, InboundStreams<Self::Stream>)>;

    /// Open an outbound connection to `onion:port`.
    async fn dial(&self, onion: &str, port: u16) -> Result<Self::Stream>;

    /// Tear down any transport-owned resources. Default: no-op.
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}
```

> **VERIFY:** `async-trait` is a workspace dep available to skattr-core (confirmed). `#[async_trait::async_trait]` with an associated `type Stream` is fine because `Transport` is used as a generic bound (`T: Transport`), never as a `dyn Transport`. If the pinned toolchain (1.95.0) cleanly supports native `async fn` in traits here, you MAY drop `async-trait` — but `async-trait` is the safe, codebase-consistent choice (the mailbox `MailboxConnectFactory` uses it). Keep `async-trait`.

- [ ] **Step 2: Register the module**

In `crates/core/src/transport/mod.rs`, add to the `mod` declarations (alphabetical with the others):

```rust
pub(crate) mod transport;
```

and add the re-export (mirror the existing `#[cfg(...)]` dual-visibility blocks):

```rust
#[cfg(not(feature = "test-harness"))]
pub(crate) use transport::{InboundStreams, Transport, ONION_PORT};
#[cfg(feature = "test-harness")]
pub use transport::{InboundStreams, Transport, ONION_PORT};
```

- [ ] **Step 3: Compile**

Run: `. "$HOME/.cargo/env" && cargo build -p skattr-core --features test-harness`
Expected: builds (an `unused`/`dead_code` warning on the new items is acceptable until later tasks use them; if `-D warnings` in clippy trips on dead code, add `#[allow(dead_code)]` to the items and remove it in the task that first uses them — but prefer landing Task 3 right after so they're used).

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/transport/transport.rs crates/core/src/transport/mod.rs
git commit -m "feat(transport): Transport trait + InboundStreams + ONION_PORT

Abstraction seam for the daemon's P2P transport (publish/dial/shutdown),
so the production assembly can run over Arti or an in-process loopback.
(T0-1)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 3: `LoopbackTransport` (in-process, test-harness)

**Why:** Lets the guardrail (Task 8) run two real daemon assemblies that dial each other over `tokio::io::duplex` streams keyed by a shared fake-onion registry — no Tor.

**Files:**
- Create: `crates/core/src/transport/loopback.rs`
- Modify: `crates/core/src/transport/mod.rs` (re-export, `test-harness`)
- Modify: `crates/core/src/lib.rs` (`test_exports`)
- Test: `crates/core/src/transport/loopback.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Create the loopback module**

Create `crates/core/src/transport/loopback.rs` (GPLv3 header; whole file gated on `test-harness`):

```rust
// <GPLv3 header>

//! In-process `Transport` for tests: a shared registry maps fake onion
//! addresses to an inbound-stream sender; `dial` makes a `duplex` pair and
//! hands one half to the target. No Tor.

#![cfg(feature = "test-harness")]

use crate::error::{CoreError, Result, TransportErrorKind};
use crate::identity::Seed;
use crate::transport::{InboundStreams, Transport, ONION_PORT};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::io::DuplexStream;
use tokio::sync::mpsc;

const DUPLEX_BUF: usize = 64 * 1024;
const INBOUND_CAP: usize = 64;

/// Shared in-process "network": fake onion → inbound sender. Clone to share
/// between two `LoopbackTransport`s so they can dial each other.
#[derive(Clone, Default)]
pub struct LoopbackNet(Arc<Mutex<HashMap<String, mpsc::Sender<DuplexStream>>>>);

impl LoopbackNet {
    pub fn new() -> Self {
        Self::default()
    }
}

pub struct LoopbackTransport {
    net: LoopbackNet,
    my_onion: String,
}

impl LoopbackTransport {
    /// Create a transport whose published onion is `my_onion` on the shared net.
    pub fn new(net: LoopbackNet, my_onion: impl Into<String>) -> Self {
        Self { net, my_onion: my_onion.into() }
    }
}

#[async_trait::async_trait]
impl Transport for LoopbackTransport {
    type Stream = DuplexStream;

    async fn publish(
        &self,
        _hs_key_path: &Path,
        _seed: &Seed,
        _nickname: &str,
    ) -> Result<(String, InboundStreams<DuplexStream>)> {
        let (tx, rx) = mpsc::channel::<DuplexStream>(INBOUND_CAP);
        self.net
            .0
            .lock()
            .map_err(|_| CoreError::Transport(TransportErrorKind::Other("loopback lock".into())))?
            .insert(self.my_onion.clone(), tx);
        Ok((self.my_onion.clone(), InboundStreams(rx)))
    }

    async fn dial(&self, onion: &str, _port: u16) -> Result<DuplexStream> {
        let sender = {
            let map = self.net.0.lock().map_err(|_| {
                CoreError::Transport(TransportErrorKind::Other("loopback lock".into()))
            })?;
            map.get(onion).cloned()
        };
        let sender = sender.ok_or_else(|| {
            CoreError::Transport(TransportErrorKind::Other("loopback: onion not published".into()))
        })?;
        let (mine, theirs) = tokio::io::duplex(DUPLEX_BUF);
        sender.send(theirs).await.map_err(|_| {
            CoreError::Transport(TransportErrorKind::Other("loopback: peer gone".into()))
        })?;
        Ok(mine)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test(flavor = "multi_thread")]
    async fn loopback_publish_then_dial_round_trips_bytes() {
        let net = LoopbackNet::new();
        let bob = LoopbackTransport::new(net.clone(), "bob.onion");
        let (_addr, mut inbound) = bob
            .publish(Path::new("/unused"), &Seed::generate().unwrap(), "bob")
            .await
            .unwrap();

        let alice = LoopbackTransport::new(net, "alice.onion");
        let mut a = alice.dial("bob.onion", ONION_PORT).await.unwrap();

        let mut b = inbound.recv().await.expect("bob accepts the inbound stream");
        a.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        b.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    }
}
```

> **VERIFY:** `CoreError`/`TransportErrorKind` import paths and the `TransportErrorKind::Other(String)` shape (used throughout `tor.rs`). `Seed::generate()` exists (used in Phase 1A tests). If `TransportErrorKind` is re-exported at `crate::transport::TransportErrorKind`, prefer that path.

- [ ] **Step 2: Register the module (test-harness only)**

In `crates/core/src/transport/mod.rs`:

```rust
#[cfg(feature = "test-harness")]
pub(crate) mod loopback;
#[cfg(feature = "test-harness")]
pub use loopback::{LoopbackNet, LoopbackTransport};
```

- [ ] **Step 3: Export from `test_exports`**

In `crates/core/src/lib.rs`, inside `pub mod test_exports`, add to the transport `pub use` line:

```rust
    pub use crate::transport::{LoopbackNet, LoopbackTransport};
```

- [ ] **Step 4: Run the loopback test**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib --features test-harness transport::loopback::tests::loopback_publish_then_dial_round_trips_bytes`
Expected: PASS.

- [ ] **Step 5: Clippy**

Run: `. "$HOME/.cargo/env" && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings`
Expected: no warnings (the Task 2 items are now used).

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/transport/loopback.rs crates/core/src/transport/mod.rs crates/core/src/lib.rs
git commit -m "feat(transport): in-process LoopbackTransport for CI testing

A test-harness Transport impl: shared fake-onion registry + duplex streams
so two daemon assemblies dial each other without Tor. (T0-1 guardrail)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 4: `ArtiTransport` (production)

**Why:** The production `Transport`: wraps the bootstrapped `TorRuntime`, publishing via `OnionListener` and dialing via the Arti client. `Daemon::run` (Task 7) constructs it.

**Files:**
- Create: `crates/core/src/transport/arti_transport.rs`
- Modify: `crates/core/src/transport/mod.rs`

- [ ] **Step 1: Create the Arti transport**

Create `crates/core/src/transport/arti_transport.rs` (GPLv3 header):

```rust
// <GPLv3 header>

//! Production `Transport` over Arti: publish via the existing `OnionListener`,
//! dial via the Arti client. Wraps an already-bootstrapped `TorRuntime`.

use crate::error::{CoreError, Result, TransportErrorKind};
use crate::identity::Seed;
use crate::transport::listener::OnionListener;
use crate::transport::tor::TorRuntime;
use crate::transport::{InboundStreams, Transport};
use std::path::Path;
use tokio::sync::Mutex;

const INBOUND_CAP: usize = 64;

pub(crate) struct ArtiTransport {
    /// Guards the one-time publish (needs `&mut TorRuntime`) and shutdown.
    runtime: Mutex<TorRuntime>,
    /// Cheap Clone handle for concurrent dials (no lock on the hot path).
    client: arti_client::TorClient<tor_rtcompat::tokio::TokioRustlsRuntime>,
}

impl ArtiTransport {
    pub(crate) fn new(runtime: TorRuntime) -> Self {
        let client = runtime.client().clone();
        Self { runtime: Mutex::new(runtime), client }
    }
}

#[async_trait::async_trait]
impl Transport for ArtiTransport {
    type Stream = arti_client::DataStream;

    async fn publish(
        &self,
        hs_key_path: &Path,
        seed: &Seed,
        nickname: &str,
    ) -> Result<(String, InboundStreams<Self::Stream>)> {
        let mut rt = self.runtime.lock().await;
        let onion = rt.publish_onion(hs_key_path, seed, nickname).await?;
        let rend = rt.rend_requests_take().ok_or_else(|| {
            CoreError::Transport(TransportErrorKind::Other(
                "publish: rend_requests already taken".into(),
            ))
        })?;
        let listener = OnionListener::spawn(rend, INBOUND_CAP);
        Ok((onion, InboundStreams(listener.into_accepted())))
    }

    async fn dial(&self, onion: &str, port: u16) -> Result<Self::Stream> {
        let target = format!("{onion}:{port}");
        self.client.connect(target.as_str()).await.map_err(|e| {
            CoreError::Transport(TransportErrorKind::Other(format!("connect {target}: {e}")))
        })
    }

    async fn shutdown(&self) -> Result<()> {
        // TorRuntime::shutdown consumes/teardown — match its real signature.
        self.runtime.lock().await.shutdown_ref().await
    }
}
```

> **VERIFY — several real-signature points:**
> 1. `TorRuntime::client(&self)` is used in `state.rs` as `rt.client().clone()`, so it exists and returns `&TorClient<TokioRustlsRuntime>`. Good.
> 2. `OnionListener` exposes its receiver via a **public field** `accepted: mpsc::Receiver<DataStream>`, NOT a method. There is no `into_accepted()`. Either (a) add a small `pub(crate) fn into_accepted(self) -> mpsc::Receiver<DataStream> { self.accepted }` to `listener.rs` (the `task` JoinHandle is kept alive by the spawned task; dropping the struct is fine since the task is detached) — **preferred**; or (b) construct `InboundStreams(listener.accepted)` and `std::mem::forget`/store the listener. Use (a): add the accessor and keep the listener's task detached. Confirm dropping `OnionListener` doesn't abort its task (it `tokio::spawn`s and stores a `JoinHandle` but never aborts on drop — so dropping is safe).
> 3. `TorRuntime::shutdown` — `state.rs` calls `rt.shutdown().await?` on an owned `rt`. Its receiver may be `self` (consuming) or `&mut self`. Since `ArtiTransport` holds the runtime behind a `Mutex` and can't move out of it, you need a `&mut self`-based teardown. Check `tor.rs`: if `shutdown(self)` consumes, add a `pub async fn shutdown_ref(&mut self) -> Result<()>` to `TorRuntime` that does the same teardown without consuming (or change the field to `Mutex<Option<TorRuntime>>` and `take()` it in `shutdown`). Pick the lower-churn option against the real `shutdown` body and name it consistently; update the call above to match.
> 4. The `TorClient` generic arg is `tor_rtcompat::tokio::TokioRustlsRuntime` (confirmed from `state.rs` `ArtiMailboxFactory`).

- [ ] **Step 2: Register the module**

In `crates/core/src/transport/mod.rs`:

```rust
pub(crate) mod arti_transport;
```

(No `test-harness` `pub` needed — `ArtiTransport` is only used inside `state.rs`. If a later step needs it re-exported, add then.)

- [ ] **Step 3: Compile**

Run: `. "$HOME/.cargo/env" && cargo build -p skattr-core --features test-harness`
Expected: builds. (`ArtiTransport` is unused until Task 7 — add `#[allow(dead_code)]` on the struct/impl if clippy `-D warnings` trips, removing it in Task 7.)

- [ ] **Step 4: Clippy**

Run: `. "$HOME/.cargo/env" && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings`
Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/transport/arti_transport.rs crates/core/src/transport/mod.rs crates/core/src/transport/listener.rs
git commit -m "feat(transport): ArtiTransport production Transport impl

Wraps the bootstrapped TorRuntime: publish via OnionListener, dial via the
Arti client (cloned handle, no lock on the dial path). (T0-1)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 5: Outbound dialer — `OutboundDial` + actor dial-on-demand

**Why:** Today the per-peer actor is passive: if it has no live `conn` it drops the job. To send to a cold peer it must dial — resolve the peer's onion from their ContactCard, dial via the `Transport`, run `handshake_initiator`, install the connection. Reconnect-on-drop falls out of the same path.

**Files:**
- Create: `crates/core/src/delivery/dial.rs`
- Modify: `crates/core/src/delivery/mod.rs`
- Modify: `crates/core/src/delivery/hub.rs` (hold `Option<Arc<dyn OutboundDial<S>>>`, thread into actor spawn)
- Modify: `crates/core/src/delivery/peer.rs` (`full_run`/`spawn` gain the dialer; dial when `conn` is `None`)
- Test: `crates/core/src/delivery/dial.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Define `OutboundDial` + `TransportDial`**

Create `crates/core/src/delivery/dial.rs` (GPLv3 header):

```rust
// <GPLv3 header>

//! On-demand outbound dialer for the per-peer delivery actor: resolve a
//! contact's onion, dial via the injected `Transport`, run the Noise
//! initiator handshake, and return an authenticated connection.

use crate::error::{CoreError, DeliveryErrorKind, Result};
use crate::identity::{IdentityKey, PublicKey};
use crate::storage::{ContactRepo, Pool};
use crate::transport::{handshake_initiator, AuthenticatedConnection, Transport, ONION_PORT};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};

#[async_trait::async_trait]
pub(crate) trait OutboundDial<S>: Send + Sync + 'static
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Establish an authenticated connection to `peer`, or error if the peer
    /// has no resolvable onion / the dial or handshake fails.
    async fn dial(&self, peer: PublicKey) -> Result<AuthenticatedConnection<S>>;
}

pub(crate) struct TransportDial<T: Transport> {
    transport: Arc<T>,
    identity: Arc<IdentityKey>,
    pool: Arc<Pool>,
}

impl<T: Transport> TransportDial<T> {
    pub(crate) fn new(transport: Arc<T>, identity: Arc<IdentityKey>, pool: Arc<Pool>) -> Self {
        Self { transport, identity, pool }
    }
}

#[async_trait::async_trait]
impl<T: Transport> OutboundDial<T::Stream> for TransportDial<T> {
    async fn dial(&self, peer: PublicKey) -> Result<AuthenticatedConnection<T::Stream>> {
        // 1. Resolve the peer's current onion from their latest ContactCard.
        let card = ContactRepo::new(&self.pool)
            .latest_card(&peer)?
            .ok_or(CoreError::Delivery(DeliveryErrorKind::NoRoute))?;
        let onion = card.body.onion.clone();

        // 2. Dial the byte stream.
        let stream = self.transport.dial(&onion, ONION_PORT).await?;

        // 3. Noise_XK initiator: we know the responder's static (their identity).
        let peer_x25519 = crate::identity::key::ed25519_pub_to_x25519(
            &ed25519_dalek::VerifyingKey::from_bytes(&peer.0)
                .map_err(|_| CoreError::Delivery(DeliveryErrorKind::NoRoute))?,
        );
        let (conn, _outcome) =
            handshake_initiator(stream, &self.identity, &peer_x25519, None).await?;
        Ok(conn)
    }
}
```

> **VERIFY:**
> - `DeliveryErrorKind` variants: pick the closest existing semantic for "no card / no route" — check `crates/core/src/delivery/error_kind.rs`. If there is no `NoRoute`, use an existing variant (e.g. `Unreachable`/`Other(String)`) rather than inventing one; do NOT add a wire-affecting variant. Use the same variant consistently in Steps 1 and the test.
> - `ContactRepo::latest_card(&PublicKey) -> Result<Option<ContactCard>>` and `ContactCard.body.onion: String` (confirmed shapes).
> - `handshake_initiator`, `AuthenticatedConnection`, `Transport`, `ONION_PORT` are reachable via `crate::transport::...` (the non-test-harness `pub(crate)` re-exports).
> - `ed25519_pub_to_x25519` path — reuse the same helper decision from Task 1.

- [ ] **Step 2: Register the module**

In `crates/core/src/delivery/mod.rs` add `pub(crate) mod dial;` and (mirroring the existing `test-harness` block) optionally `#[cfg(feature = "test-harness")] pub use dial::{OutboundDial, TransportDial};` only if a cross-crate test needs them (the guardrail does not call them directly — it goes through `run_with_transport` — so this re-export is optional; add only if Step 5's test or Task 8 needs it).

- [ ] **Step 3: Thread the dialer into the hub**

In `crates/core/src/delivery/hub.rs`:

(a) Add a field to `DeliveryHub<S>`:

```rust
    dialer: Option<Arc<dyn crate::delivery::dial::OutboundDial<S>>>,
```

(b) Update `new_inner` to take + store it, and add a constructor that sets it. Keep `new` / `new_with_inbound` working by passing `None`. Add:

```rust
    pub fn new_with_inbound_and_dialer(
        pool: Arc<Pool>,
        dispatch: Arc<dyn InboundDispatch>,
        dialer: Arc<dyn crate::delivery::dial::OutboundDial<S>>,
    ) -> Self {
        Self::new_inner(pool, Some(dispatch), None, Some(dialer))
    }
```

Update `new_inner`'s signature to `(pool, inbound, fallback, dialer)` and set `dialer` in the returned struct; update the two existing callers (`new`, `new_with_inbound`) to pass `None`.

(c) In `spawn_peer_actor`, pass `self.dialer.clone()` into `PeerConnection::spawn`:

```rust
    let _handle = PeerConnection::spawn::<S>(
        peer,
        jobs_rx,
        welcome_jobs_rx,
        ctrl_rx,
        self.pool.clone(),
        self.inbound.clone(),
        self.dialer.clone(),
    );
```

> **VERIFY:** `MailboxFallback` is the existing `fallback` field type; preserve it. The `dialer` is `Option` so `new`/`new_with_inbound` (used by current tests) keep compiling with `None`.

- [ ] **Step 4: Make the actor dial on demand**

In `crates/core/src/delivery/peer.rs`:

(a) Add the dialer param to `PeerConnection::spawn` and `full_run` (both generic over `S`):

```rust
    pub fn spawn<S>(
        peer: PublicKey,
        jobs: mpsc::Receiver<DeliveryJob>,
        welcome_jobs: mpsc::Receiver<WelcomeJob>,
        ctrl: mpsc::Receiver<PeerCtrl<S>>,
        pool: std::sync::Arc<crate::storage::Pool>,
        inbound: Option<std::sync::Arc<dyn InboundDispatch>>,
        dialer: Option<std::sync::Arc<dyn crate::delivery::dial::OutboundDial<S>>>,
    ) -> PeerHandle
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        tokio::spawn(async move {
            let _ = full_run::<S>(peer, None, jobs, welcome_jobs, ctrl, pool, inbound, dialer).await;
        })
    }
```

Thread `dialer` into `full_run`'s signature identically.

(b) Add a helper inside `full_run` that ensures a live connection, dialing if needed:

```rust
    // Returns true if a live `conn` is available after (optionally) dialing.
    async fn ensure_conn<S>(
        peer: PublicKey,
        conn: &mut Option<AuthenticatedConnection<S>>,
        dialer: &Option<std::sync::Arc<dyn crate::delivery::dial::OutboundDial<S>>>,
    ) -> bool
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        if conn.is_some() {
            return true;
        }
        let Some(d) = dialer.as_ref() else { return false };
        match d.dial(peer).await {
            Ok(c) => {
                *conn = Some(c);
                true
            }
            Err(e) => {
                tracing::warn!(error = %e, "delivery: outbound dial failed");
                false
            }
        }
    }
```

(c) In the `job = jobs.recv()` arm, replace the immediate `let Some(c) = conn.as_mut() else { ack Err; continue }` with a dial attempt first:

```rust
        job = jobs.recv() => {
            let Some(job) = job else { break; };
            if !ensure_conn::<S>(peer, &mut conn, &dialer).await {
                let _ = job.ack_tx.send(Err(()));
                continue;
            }
            let c = conn.as_mut().expect("ensure_conn guarantees Some"); // test-only expect? NO — see note
            if c.send(Frame::MlsApp(job.ciphertext)).await.is_err() {
                let _ = job.ack_tx.send(Err(()));
                conn = None;
                drain_pending(&mut pending);
                continue;
            }
            pending.insert(job.message_id, job.ack_tx);
            last_traffic = tokio::time::Instant::now();
        }
```

> **VERIFY / FIX the `expect`:** library code must not `expect`. Restructure to avoid it — e.g. have `ensure_conn` return `Option<&mut AuthenticatedConnection<S>>` is awkward with the borrow; instead inline:
> ```rust
> if !ensure_conn::<S>(peer, &mut conn, &dialer).await { let _ = job.ack_tx.send(Err(())); continue; }
> let Some(c) = conn.as_mut() else { let _ = job.ack_tx.send(Err(())); continue; };
> ```
> The second `let Some(... ) else` is unreachable-by-logic but satisfies the no-`expect` rule cleanly. Use this form.

(d) Optionally mirror the dial in the retry-tick arm so queued outbox rows also trigger a dial. Minimal for 1B: leave the retry tick as-is (it already `break`s when `conn` is None); the next `job` send will dial. Add a one-line comment noting this. (Wiring a timer-driven redial is the Phase-2 fallback work.)

> **VERIFY:** `AuthenticatedConnection` is already imported in `peer.rs`. `PeerHandle`, `DeliveryJob`, `WelcomeJob`, `Frame`, `drain_pending`, `pending`, `last_traffic` are existing locals/types — do not redefine.

- [ ] **Step 5: Write a unit test for actor dial-on-demand**

Add to `crates/core/src/delivery/dial.rs`'s `#[cfg(test)] mod tests`. This proves the actor dials when cold. Use a **stub dialer** that hands back a pre-built `AuthenticatedConnection<DuplexStream>` (built via a real handshake over duplex), so we exercise the actor's dial path without a Transport:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::delivery::hub::DeliveryHub;
    use crate::envelope::MessageId;
    use crate::transport::{handshake_responder, AuthenticatedConnection};
    use std::sync::Mutex as StdMutex;
    use tokio::io::DuplexStream;

    /// Dialer that yields one pre-made connection, then errors.
    struct OneShotDialer(StdMutex<Option<AuthenticatedConnection<DuplexStream>>>);

    #[async_trait::async_trait]
    impl OutboundDial<DuplexStream> for OneShotDialer {
        async fn dial(&self, _peer: PublicKey) -> Result<AuthenticatedConnection<DuplexStream>> {
            self.0
                .lock()
                .unwrap()
                .take()
                .ok_or(CoreError::Delivery(DeliveryErrorKind::NoRoute))
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn actor_dials_when_cold_and_delivers() {
        // Build a real authenticated duplex pair: alice (initiator side held by
        // the dialer) <-> bob (responder side reads the frame).
        let alice = IdentityKey::generate().unwrap();
        let bob = IdentityKey::generate().unwrap();
        let alice_x = crate::identity::key::ed25519_pub_to_x25519(
            &ed25519_dalek::VerifyingKey::from_bytes(&bob.public().0).unwrap(),
        );
        let (a, b) = tokio::io::duplex(64 * 1024);
        let bob_id = IdentityKey::generate().unwrap(); // placeholder; see VERIFY
        let init = tokio::spawn(async move {
            handshake_initiator(a, &alice, &alice_x, None).await.unwrap().0
        });
        let resp = tokio::spawn(async move {
            handshake_responder(b, &bob, None).await.unwrap().0
        });
        let alice_conn = init.await.unwrap();
        let mut bob_conn = resp.await.unwrap();

        // Hub with a OneShotDialer feeding alice_conn; send to bob -> actor dials.
        let pool = std::sync::Arc::new(crate::storage::Pool::in_memory());
        let dialer: Arc<dyn OutboundDial<DuplexStream>> =
            Arc::new(OneShotDialer(StdMutex::new(Some(alice_conn))));
        let hub = DeliveryHub::<DuplexStream>::new_inner_for_test(pool, None, None, Some(dialer));
        // ^ VERIFY: use whatever constructor exposes the dialer in tests; if
        //   new_with_inbound_and_dialer requires an InboundDispatch, pass a
        //   no-op one, or add a test-only constructor. See note.

        let _ack = hub.send(bob.public(), MessageId::generate(), b"hi".to_vec()).await.unwrap();
        // Bob reads exactly one MlsApp frame -> proves the actor dialed + sent.
        let frame = tokio::time::timeout(std::time::Duration::from_secs(2), bob_conn.recv())
            .await
            .expect("frame within 2s")
            .unwrap();
        assert!(frame.is_some(), "bob received the dialed-and-sent frame");
    }
}
```

> **VERIFY — this test has rough edges; make it real:**
> - The `bob_id` placeholder line is wrong — delete it; use the single `bob` identity for the responder. The initiator must target **bob's** static: `peer_x25519 = ed25519_pub_to_x25519(VerifyingKey::from_bytes(&bob.public().0))`. Fix the variable so the initiator targets bob and the responder is bob.
> - `DeliveryHub::send` requires the peer to be reachable via the dialer; the actor calls `dialer.dial(peer)` ignoring the passed `peer` (OneShotDialer ignores it), so attribution doesn't matter here.
> - Constructor: if `new_with_inbound_and_dialer` needs an `InboundDispatch`, either pass a trivial no-op impl (define a local `struct NoopInbound;` like Phase 1A's `NoopDispatch`) via a dialer-only constructor, OR add `pub(crate) fn new_with_dialer(pool, dialer)` to the hub that sets `inbound: None`. Prefer adding `new_with_dialer` (symmetric with `new`).
> - `Frame::recv()` returns `Result<Option<Frame>>`; assert it's `Some(Frame::MlsApp(_))` if you want to be precise.
> - This is the trickiest test in the plan. If the handshake-over-duplex setup fights you, model it exactly on `crates/tests/src/delivery_kill_mid_message.rs`, which already builds `AuthenticatedConnection<DuplexStream>` pairs via `handshake_initiator`/`handshake_responder`.

- [ ] **Step 6: Run the dial test + the delivery suite**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib --features test-harness delivery:: && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings`
Expected: PASS, no warnings. (Existing `delivery::peer`/`hub` tests must still pass — the `dialer: None` default preserves their behavior.)

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/delivery/dial.rs crates/core/src/delivery/mod.rs crates/core/src/delivery/hub.rs crates/core/src/delivery/peer.rs
git commit -m "feat(delivery): on-demand outbound dialer in the per-peer actor

OutboundDial + TransportDial resolve a contact's onion, dial via the
injected Transport, and run the Noise initiator handshake. The per-peer
actor dials when it has no live connection instead of dropping the job.
(T0-1 outbound)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 6: Inbound accept loop

**Why:** Consume the `InboundStreams` from `Transport::publish`, run `handshake_responder` per stream, resolve the authenticated peer via `find_by_noise_x25519`, **reject unknown peers**, and `ingest` known ones into the hub.

**Files:**
- Create: `crates/core/src/daemon/accept.rs`
- Modify: `crates/core/src/daemon/mod.rs`
- Test: `crates/core/src/daemon/accept.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Create the accept loop**

Create `crates/core/src/daemon/accept.rs` (GPLv3 header):

```rust
// <GPLv3 header>

//! Inbound accept loop: handshake each inbound stream, resolve the peer, and
//! ingest authorized connections into the DeliveryHub. Unknown peers are
//! rejected before ingest.

use crate::delivery::hub::DeliveryHub;
use crate::identity::IdentityKey;
use crate::storage::{ContactRepo, Pool};
use crate::transport::{handshake_responder, InboundStreams};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};

/// Drive the inbound stream source until it closes. Each accepted stream is
/// handshaked + resolved on its own task so a slow handshake can't stall the
/// loop. Returns when `inbound` is exhausted (transport shut down).
pub(crate) async fn run_accept_loop<S>(
    mut inbound: InboundStreams<S>,
    identity: Arc<IdentityKey>,
    pool: Arc<Pool>,
    hub: Arc<DeliveryHub<S>>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    while let Some(stream) = inbound.recv().await {
        let identity = identity.clone();
        let pool = pool.clone();
        let hub = hub.clone();
        tokio::spawn(async move {
            let (conn, outcome) = match handshake_responder(stream, &identity, None).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "accept: inbound handshake failed");
                    return;
                }
            };
            match ContactRepo::new(&pool).find_by_noise_x25519(outcome.peer_x25519()) {
                Ok(Some(peer)) => hub.ingest(peer, conn).await,
                Ok(None) => {
                    tracing::warn!("accept: rejected inbound connection from unknown peer");
                    let _ = conn.close().await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "accept: peer resolution failed");
                    let _ = conn.close().await;
                }
            }
        });
    }
}
```

> **VERIFY:**
> - `HandshakeOutcome.peer_x25519` is a **public field** `pub peer_x25519: [u8; 32]` (not a method). So use `&outcome.peer_x25519`, not `outcome.peer_x25519()`. Fix the call accordingly.
> - `find_by_noise_x25519(&[u8;32])` takes a reference — pass `&outcome.peer_x25519`.
> - `AuthenticatedConnection::close(self)` consumes — `conn.close().await` is correct in the reject/err arms (we drop the conn). In the `ingest` arm, `hub.ingest(peer, conn)` moves `conn`. Good.
> - `DeliveryHub`, `handshake_responder`, `InboundStreams` reachable via `crate::...` `pub(crate)` paths.

- [ ] **Step 2: Register the module**

In `crates/core/src/daemon/mod.rs` add `pub(crate) mod accept;` (match the existing `mod` visibility style).

- [ ] **Step 3: Write the accept-loop test**

Add to `accept.rs`'s `#[cfg(test)] mod tests`. Drive `run_accept_loop` with a hand-fed `InboundStreams` (a duplex stream where the other end runs `handshake_initiator`), prove a KNOWN peer's connection is ingested (a subsequently-sent frame is delivered) and an UNKNOWN peer is rejected (no ingest). Keep it focused — the full end-to-end is Task 8. Minimal version proving reject-unknown:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::handshake_initiator;
    use tokio::sync::mpsc;

    #[tokio::test(flavor = "multi_thread")]
    async fn accept_rejects_unknown_peer() {
        let pool = Arc::new(Pool::in_memory());           // no contacts -> everyone unknown
        let me = Arc::new(IdentityKey::generate().unwrap());
        let hub = Arc::new(DeliveryHub::<tokio::io::DuplexStream>::new(pool.clone()));

        let (tx, rx) = mpsc::channel::<tokio::io::DuplexStream>(4);
        let inbound = InboundStreams(rx);
        let loop_task = tokio::spawn(run_accept_loop(inbound, me.clone(), pool.clone(), hub.clone()));

        // A stranger dials in.
        let me_x = crate::identity::key::ed25519_pub_to_x25519(
            &ed25519_dalek::VerifyingKey::from_bytes(&me.public().0).unwrap(),
        );
        let (cli, srv) = tokio::io::duplex(64 * 1024);
        tx.send(srv).await.unwrap();
        let stranger = IdentityKey::generate().unwrap();
        // Initiator completes the handshake (responder side runs in the loop).
        let _ = handshake_initiator(cli, &stranger, &me_x, None).await;

        // Give the loop a moment; assert the stranger did NOT get a peer actor.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        // VERIFY: assert no actor for `stranger.public()` — expose a
        // `pub(crate) async fn has_peer(&self, &PublicKey) -> bool` test helper
        // on DeliveryHub, or assert via absence of any delivered frame.
        drop(tx); // close inbound -> loop ends
        let _ = loop_task.await;
    }
}
```

> **VERIFY:** `InboundStreams(rx)` is a tuple-struct constructor — reachable inside the crate. The assertion "no actor was created" needs a hook: add a tiny `pub(crate) async fn has_peer(&self, peer: &PublicKey) -> bool { self.peers.lock().await.contains_key(peer) }` to `DeliveryHub` (test-only-ish but harmless and `pub(crate)`), and assert `!hub.has_peer(&stranger.public()).await`. The "known peer is ingested" direction is fully covered by the Task 8 guardrail, so this unit test focuses on reject-unknown; if you can cheaply also assert the known-peer ingest here (seed a contact, assert `has_peer` becomes true), do so.

- [ ] **Step 4: Run the test**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib --features test-harness daemon::accept`
Expected: PASS.

- [ ] **Step 5: Clippy**

Run: `. "$HOME/.cargo/env" && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings`
Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/accept.rs crates/core/src/daemon/mod.rs crates/core/src/delivery/hub.rs
git commit -m "feat(daemon): inbound accept loop with unknown-peer rejection

Handshake each inbound stream, resolve the peer via find_by_noise_x25519,
reject unknown peers before ingest, and ingest authorized connections.
(T0-1 inbound + auth gap)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 7: Extract `run_with_transport<T>` and rewire `Daemon::run`

**Why:** Make the whole post-bootstrap assembly generic over `Transport`, spawn the accept loop, and inject `TransportDial` — without changing `Daemon::run`'s public signature (CLI/UI unaffected). The guardrail (Task 8) drives `run_with_transport` with `LoopbackTransport`.

**Files:**
- Modify: `crates/core/src/daemon/state.rs`
- Modify: `crates/core/src/lib.rs` (`test_exports`: export `run_with_transport`)

- [ ] **Step 1: Extract the assembly into `run_with_transport`**

In `crates/core/src/daemon/state.rs`, refactor `run_with_sink` so that everything **after** `TorRuntime::bootstrap` + the mailbox factory construction moves into a new generic fn. Concretely:

`run_with_sink` keeps: vault opens, seed derivation, `Pool::open`, backfills, retention sweep spawn, `TorRuntime::bootstrap`, building the `ArtiMailboxFactory` (it needs `rt.client().clone()` — do this BEFORE moving `rt` into `ArtiTransport`), then:

```rust
    let mailbox_factory: Arc<dyn crate::mailbox::poll::MailboxConnectFactory> =
        Arc::new(ArtiMailboxFactory { tor_client: rt.client().clone() });
    let transport = Arc::new(crate::transport::arti_transport::ArtiTransport::new(rt));
    // One identity shared (by Arc) across the dialer (initiator handshakes) and
    // the accept loop (responder handshakes); both only need `&IdentityKey`.
    // Open the vault once more for it (the established multi-open pattern).
    let (_vault5, identity_for_transport) = Vault::open(&vault_path, passphrase.as_str())?;
    let transport_identity = Arc::new(identity_for_transport);
    run_with_transport(
        transport,
        pool,
        identity,
        identity_for_poller,
        identity_for_inbound,
        transport_identity,
        seed,
        &data_dir_owned,
        config,
        config_path,
        config_arc,
        events_tx,
        mailbox_factory,
        resolved_sink,
        sweep_shutdown_tx,
        sweep_handle,
        ready,
        shutdown,
    )
    .await
```

Define:

```rust
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_with_transport<T>(
    transport: Arc<T>,
    pool: Arc<Pool>,
    identity: IdentityKey,
    identity_for_poller: IdentityKey,
    identity_for_inbound: IdentityKey,
    transport_identity: Arc<IdentityKey>,
    seed: crate::identity::Seed,
    data_dir: &Path,
    config: Config,
    config_path: std::path::PathBuf,
    config_arc: Arc<tokio::sync::RwLock<Config>>,
    events_tx: broadcast::Sender<Event>,
    mailbox_factory: Arc<dyn crate::mailbox::poll::MailboxConnectFactory>,
    log_sink: LogSink,
    sweep_shutdown_tx: tokio::sync::watch::Sender<bool>,
    sweep_handle: tokio::task::JoinHandle<()>,
    ready: oneshot::Sender<Ready>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()>
where
    T: crate::transport::Transport,
{
    // 1. Publish onion + inbound stream source via the transport.
    let hs_key_path = data_dir.join("hs.key.age");
    let (onion, inbound_streams) = transport.publish(&hs_key_path, &seed, "skattr-daemon").await?;

    // 2. DaemonInbound.
    let inbound_impl = DaemonInbound::new(pool.clone(), events_tx.clone());
    inbound_impl.set_identity(Arc::new(identity_for_inbound));
    let inbound = Arc::new(inbound_impl) as Arc<dyn InboundDispatch>;

    // 3. Hub with injected on-demand dialer (shares `transport_identity`).
    let dialer = Arc::new(crate::delivery::dial::TransportDial::new(
        transport.clone(),
        transport_identity.clone(),
        pool.clone(),
    ));
    let hub: Arc<DeliveryHub<T::Stream>> = Arc::new(
        DeliveryHub::new_with_inbound_and_dialer(pool.clone(), inbound.clone(), dialer),
    );

    // 4. Inbound accept loop (shares `transport_identity`).
    let accept_task = tokio::spawn(crate::daemon::accept::run_accept_loop(
        inbound_streams,
        transport_identity.clone(),
        pool.clone(),
        hub.clone(),
    ));

    // 5. PollScheduler, DaemonHandle, IPC, taps — as in the original body, using
    //    `hub` (now DeliveryHub<T::Stream>) and `mailbox_factory`.
    //    ... (move the existing Step 5.5 / 6 / 7 / 8 code here verbatim) ...

    // 8. signal readiness, await shutdown.
    // 9. teardown: ipc shutdown, tor_tap abort, sweep shutdown, drop(scheduler),
    //    accept_task.abort(), transport.shutdown().await?.
    Ok(())
}
```

> **VERIFY — the identity-juggling is the crux.** The original opens the vault **four times** to get four owned `IdentityKey`s (`identity_for_seed`, `identity`, `identity_for_poller`, `identity_for_inbound`) because `IdentityKey` is not `Clone`. The transport now needs an identity for BOTH the dialer (initiator handshakes) and the accept loop (responder handshakes) — both only borrow `&IdentityKey`, so this plan opens the vault **one** additional time (`identity_for_transport`), wraps it in `Arc<IdentityKey>`, and shares that single `Arc` across the dialer + accept loop (see Step 1's `transport_identity`). Keep `identity` / `identity_for_poller` / `identity_for_inbound` as their existing owned opens (the `DaemonHandle` / `PollScheduler` / `DaemonInbound` APIs take owned `IdentityKey`). If you later find any of those APIs accept `Arc<IdentityKey>`, collapsing the extra opens is a fine cleanup — but it is NOT required for 1B. Keep it correct and zeroizing.
> - Capture `data_dir` as an owned `PathBuf` (`data_dir_owned`) before the move, since the original takes `data_dir: &Path`.
> - Everything from the original "Step 5.5" through "Step 10" moves into `run_with_transport` **verbatim**, except: the hub is now `DeliveryHub<T::Stream>` (was `<arti_client::DataStream>`); `DaemonHandle::<arti_client::DataStream>` becomes `DaemonHandle::<T::Stream>`; and teardown replaces `rt.shutdown().await?` with `transport.shutdown().await?` plus `accept_task.abort()`.
> - `Daemon::run` (the public wrapper) is unchanged in signature; it calls `run_with_sink`, which now ends by delegating to `run_with_transport`. Confirm `Daemon::run`'s existing body.

- [ ] **Step 2: Export `run_with_transport` for the guardrail**

In `crates/core/src/lib.rs` `test_exports`, add:

```rust
    pub use crate::daemon::state::run_with_transport;
```

(`run_with_transport` must be at least `pub(crate)`; the `test_exports` re-export makes it reachable from `crates/tests` under `test-harness`.)

- [ ] **Step 3: Build + run the existing daemon/integration tests**

Run:
```bash
. "$HOME/.cargo/env" && \
cargo build -p skattr-core --features test-harness && \
cargo test -p skattr-core --features test-harness daemon:: && \
cargo test -p skattr-tests
```
Expected: builds; existing daemon tests + the (mostly `#[ignore]`d) integration tests still pass. The public `Daemon::run` path is behavior-equivalent **plus** it now accepts inbound connections and can dial — existing non-Tor tests that don't exercise transport are unaffected.

- [ ] **Step 4: Confirm CLI + UI still build (public API unchanged)**

Run: `. "$HOME/.cargo/env" && cargo build -p skattr-cli`
Expected: builds (proves `Daemon::run`'s signature is unchanged). (Skip `skattr-ui` — it's excluded from the core workspace test/clippy gates.)

- [ ] **Step 5: Clippy**

Run: `. "$HOME/.cargo/env" && cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings`
Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/state.rs crates/core/src/lib.rs
git commit -m "feat(daemon): generic run_with_transport assembly; wire accept loop + dialer

Extract the post-bootstrap daemon assembly into run_with_transport<T: Transport>;
Daemon::run stays a thin ArtiTransport wrapper (signature unchanged). The
assembly now spawns the inbound accept loop and injects the on-demand dialer,
so two daemons exchange messages directly over the production wiring. (T0-1)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 8: The regression guardrail (fast loopback + #[ignore] real-Tor)

**Why:** The roadmap's cross-cutting principle — prove messaging works through the **real** `run_with_transport` assembly, not `test_exports` wiring. Two daemons over `LoopbackTransport` exchange messages both directions in CI; an `#[ignore]` twin runs the same script over Arti.

**Files:**
- Create: `crates/tests/src/daemon_run_direct.rs`
- Modify: the `crates/tests` module list (e.g. `crates/tests/src/lib.rs`) to include it.

- [ ] **Step 1: Write the fast loopback guardrail**

Create `crates/tests/src/daemon_run_direct.rs` (GPLv3 header). Model the daemon lifecycle + IPC interaction on `crates/tests/src/cli_two_daemons.rs` (read it for the exact init/unlock/IpcClient idiom), but spawn each daemon via `test_exports::run_with_transport` with a `LoopbackTransport` over a shared `LoopbackNet`:

```rust
// <GPLv3 header>

//! Phase 1B guardrail: two real daemon assemblies (run_with_transport) over an
//! in-process LoopbackTransport exchange messages in both directions — driving
//! the production wiring, not test_exports hand-wiring.

use skattr_core::test_exports::{LoopbackNet, LoopbackTransport, run_with_transport, /* + IpcClient, etc. */};

#[tokio::test(flavor = "multi_thread")]
async fn two_daemons_exchange_messages_both_directions_over_loopback() {
    // 1. Two temp data dirs; init each identity vault (mirror cli_two_daemons'
    //    identity_init/vault setup).
    // 2. Shared LoopbackNet; alice publishes "alice.onion", bob "bob.onion".
    //    Each side's ContactCard.onion MUST equal the loopback onion so the
    //    dialer resolves to the right registry entry.
    // 3. Spawn run_with_transport(LoopbackTransport::new(net.clone(), "<onion>"), ..)
    //    per daemon (build the non-transport args the way run_with_sink does:
    //    Pool, identities, Config with a unique ipc socket path + data dir,
    //    events_tx, a no-op/loopback mailbox factory, etc.). Await each Ready.
    // 4. IpcClient to each socket. Alice creates an invite; Bob adds it.
    //    The inviter's Welcome rides the dialer (direct) -> Bob's group goes Active.
    // 5. Alice sends "hello-bob"; assert Bob emits MessageReceived / persists it.
    // 6. Bob sends "hello-alice"; assert Alice receives it.
    // 7. Shut both down.
}
```

> **VERIFY — this is the integration capstone; build it concretely:**
> - **Mailbox factory for loopback:** `run_with_transport` requires an `Arc<dyn MailboxConnectFactory>`. The guardrail has no mailboxes, so provide a no-op factory whose `connect` returns `Err(MailboxClientErrorKind::Unreachable)` (it's never called — no contact has a mailbox). Define it in the test, or add a `test_exports` no-op factory. The `PollScheduler` will simply have nothing to poll.
> - **ContactCard onion must match the loopback onion.** When Bob adds Alice's invite, Alice's ContactCard must carry `onion = "alice.onion"` (the string Alice published on the `LoopbackNet`). Confirm how the onion gets into the card during invite/add (the daemon sets its own onion via `handle.set_onion(onion)` and publishes its card); ensure both daemons' published onions equal their loopback addresses. This is the single most important wiring detail — if the card onion ≠ the registry key, the dialer's `Transport::dial` returns "onion not published" and delivery fails.
> - **Args plumbing:** `run_with_transport` takes many args that `run_with_sink` builds (config_arc, sweep handle, log sink, events_tx, identities). Either (a) call the public `Daemon::run` — but that forces ArtiTransport (Tor); so you MUST call `run_with_transport` directly with a `LoopbackTransport`, which means the test reconstructs the same arg set `run_with_sink` builds. To keep the test small, consider adding a thin `test_exports::run_loopback(data_dir, passphrase, config, net, my_onion, ready, shutdown)` helper in `lib.rs` that does the vault-open/pool/sweep/factory setup (a loopback twin of `run_with_sink`) and calls `run_with_transport`. **Strongly prefer adding this helper** — it keeps the guardrail readable and is itself part of the tested production path (it differs from `run_with_sink` only in the transport + mailbox factory). Name it and export it under `test-harness`.
> - **Driving invite/add/send over IPC:** reuse `cli_two_daemons`' exact command sequence (`Command::CreateInvite`, `Command::AddContact`, `Command::SendMessage`) and event subscription (`EventFilter::Messages`) for assertions. Confirm the real command/result variant names against `crates/core/src/daemon/commands.rs` / wire module.
> - This test is non-`#[ignore]` and must be deterministic. Use bounded `tokio::time::timeout`s around each await (invite/add/Welcome/send) so a wiring bug fails fast rather than hanging CI.

- [ ] **Step 2: Run the guardrail**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-tests --features <whatever the test crate uses to reach test_exports> two_daemons_exchange_messages_both_directions_over_loopback -- --nocapture`
Expected: PASS — both directions deliver. (Determine the test crate's feature flag from how `delivery_kill_mid_message` builds; it imports `test_exports`, so the same flag applies.)

- [ ] **Step 3: Add the #[ignore] real-Tor twin**

In the same file, add `#[tokio::test] #[ignore = "requires Tor"] async fn two_daemons_exchange_messages_over_real_tor()` that runs the **same script** but spawns each daemon via the public `Daemon::run` (ArtiTransport). Reuse `cli_real_tor.rs`'s Tor-bring-up idiom. This is the production-fidelity check, run locally/nightly.

- [ ] **Step 4: Run the ignored test locally if Tor is available (optional but recommended)**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-tests --release -- --ignored two_daemons_exchange_messages_over_real_tor`
Expected: PASS (slow). If no Tor/network in this environment, note it's deferred to a networked run and do NOT block the task on it.

- [ ] **Step 5: Commit**

```bash
git add crates/tests/src/daemon_run_direct.rs crates/tests/src/lib.rs crates/core/src/lib.rs
git commit -m "test(guardrail): two daemons exchange messages over the real assembly

Fast loopback guardrail drives run_with_transport end-to-end (invite -> add ->
bidirectional send) with no Tor and no test_exports hand-wiring, plus an
#[ignore] real-Tor twin. This is the roadmap's regression guardrail. (T0-1)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Final verification

- [ ] **Run the full gates**

Run:
```bash
. "$HOME/.cargo/env" && \
cargo fmt --all -- --check && \
cargo test -p skattr-core --features test-harness && \
cargo test -p skattr-tests && \
cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings && \
cargo build -p skattr-cli
```
Expected: all green; CLI builds (public API intact).

- [ ] **Confirm the five behaviors by reading the diff**
  - `find_by_noise_x25519` resolves known peers, rejects strangers (Task 1).
  - `Transport` has Arti + Loopback impls; loopback round-trips bytes (Tasks 2–4).
  - The per-peer actor dials on demand when cold (Task 5).
  - The accept loop rejects unknown peers before `ingest` (Task 6).
  - Two `run_with_transport` daemons exchange messages **both directions** over loopback (Tasks 7–8).

---

## Spec coverage (self-review)

| Spec section | Covered by | Notes |
|---|---|---|
| §3.1 `Transport` trait | Task 2 | Full. |
| §3.2 `ArtiTransport` | Task 4 | Full; verifies real `TorRuntime::shutdown`/`client` shapes. |
| §3.3 `LoopbackTransport` | Task 3 | Full, with unit test. |
| §3.4 `find_by_noise_x25519` | Task 1 | Full, constant-time. |
| §3.5 `OutboundDial` / actor dial | Task 5 | Full; retry-tick redial left minimal (Phase-2 fallback owns timer redial). |
| §3.6 inbound accept loop + reject-unknown | Task 6 | Full. |
| §3 `run_with_transport` extraction, `Daemon::run` wrapper | Task 7 | Full; public signature unchanged. |
| §6 guardrail (fast loopback + #[ignore] Tor) | Task 8 | Full. |
| §2 out-of-scope (mailbox fallback trigger, h_transport) | — | Deferred to Phase 2 by design; no invite PSK on the transport in 1B. |
| §8 visibility discipline | All tasks | New items `pub(crate)` / `test-harness` `pub`; no public-API widening. |

The trickiest tasks are **5** (actor dial path + hub threading) and **7** (assembly extraction + identity juggling); both carry explicit VERIFY notes pinning the real signatures gathered from the code.
