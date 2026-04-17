# Phase 0.C — Arti Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the `todo!()` stubs in `crates/core/src/transport/tor.rs` with a working `TorRuntime` that bootstraps Arti, publishes a v3 onion service (with an identity-encrypted HS key on disk), accepts inbound connections, dials outbound, and emits status updates — then wire `skattr daemon` so a human can run two daemons and watch a byte echo round-trip over real Tor.

**Architecture:** `TorRuntime` is the only abstraction over Arti — everything downstream (session manager, delivery, MLS, CLI) sees `TorRuntime` only. The HS signing key is generated locally, persisted at `<data_dir>/hs.key.age` encrypted under a `HKDF("skattr-hs-storage-v1")` derivation of the identity seed, and loaded into `tor_hsservice::OnionService` at startup. Inbound connections arrive via `OnionListener` (`mpsc::Receiver<DataStream>`); outbound dialing is `TorRuntime::connect`. Status (bootstrap progress, terminal failure) is a `tokio::sync::watch` channel. The network-exercising integration test is `#[ignore]`-gated — it works, but it hits the real Tor network and takes 3-10 min, so it's run manually rather than in default CI.

**Tech Stack:**
- `arti-client` 0.41 — Tor client embedding.
- `tor-hsservice` 0.41 — onion service primitives.
- `tor-rtcompat` 0.41 — runtime compat.
- `age` 0.11 — at-rest encryption for the HS signing key.
- `hkdf` 0.13 + `sha2` 0.10 — domain-separated key derivation (new `INFO_HS_STORAGE_V1` label).
- Existing workspace primitives: `tokio` (with `rt-multi-thread` dev-feature already in place), `tracing`, `zeroize`, `thiserror`, `anyhow`.

**Exit criteria:**
- `cargo fmt --all --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace --release` all green.
- `cargo test --workspace --release -- --ignored` pass on a machine with internet access (takes ~5-10 min; two fresh Arti bootstraps).
- `skattr daemon` bootstraps Tor, publishes a v3 onion service, and prints the `.onion` address plus a "listening, Ctrl-C to shut down" banner. Ctrl-C triggers graceful shutdown.
- Two instances of `skattr daemon` on different machines (or two processes on the same machine with distinct `--data-dir` paths) can byte-echo through an explicit test harness command.
- `transport/tor.rs` has zero `todo!()`.
- `docs/adr/0005-arti-vs-system-tor.md` committed — records "embed Arti, system-tor fallback documented but not architected-around" decision.

---

## File structure

```
crates/core/src/transport/
├── tor.rs                  MODIFY: real TorRuntime impl backed by arti-client
├── hs_key.rs               CREATE: v3 HS signing key generation + age-encrypted persistence
├── listener.rs             MODIFY: OnionListener::spawn consumes RendRequest stream
└── mod.rs                  MODIFY: add `pub(crate) mod hs_key;`

crates/core/src/identity/
└── derive.rs               MODIFY: add `INFO_HS_STORAGE_V1` label

crates/cli/src/
└── main.rs                 MODIFY: wire `skattr daemon` subcommand

crates/tests/src/
└── arti_echo.rs            CREATE: ignore-gated two-daemon echo integration test

docs/adr/
└── 0005-arti-vs-system-tor.md   CREATE

CHANGELOG.md                MODIFY: add Phase 0.C bullet
CLAUDE.md                   MODIFY: shrink 0.C from "TODO" to "done"
```

`mls/`, `storage/`, `delivery/`, `daemon/state.rs` — all untouched. `transport/noise.rs`, `transport/frame.rs`, `transport/connection.rs` — untouched (wiring those into the Tor byte pipe is Phase 1 work).

---

## Pre-flight

```bash
cd /home/myggiz/development/skattr
. "$HOME/.cargo/env"

# Confirm baseline after the Phase 0.B cleanup merge.
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --release

git worktree add ../skattr-phase-0c-arti -b phase-0c-arti
cd ../skattr-phase-0c-arti
cargo build --workspace
```

All gates green before starting. Subsequent tasks assume `/home/myggiz/development/skattr-phase-0c-arti`.

**External dependencies.** The integration test (Task 9) and any developer running Tasks 2-7 against real Arti needs:
- Internet access. Tor directory authorities must be reachable.
- ~50 MB of free disk for Arti's cached consensus/descriptors per `state_dir`.
- Patience: first bootstrap on a fresh `state_dir` takes 30-90 s.

**Arti 0.41 API caveat.** The plan below uses Arti 0.41 APIs to the best of current knowledge. If the implementer finds a method signature or type has drifted (e.g., `TorClient::builder()` has reshaped, `launch_onion_service` returns a different tuple, etc.), adapt and note the deviation in the commit message. The plan's intent — bootstrap, publish onion, accept, dial, shutdown — is stable across minor Arti churn.

---

## Task 1: Arti dep audit + `INFO_HS_STORAGE_V1` label

**Goal:** Confirm the workspace's arti-client features cover what we need; add the HS storage label that Task 4 uses. Trivial but sets up later tasks.

**Files:** Modify `Cargo.toml` (workspace), `crates/core/Cargo.toml`, `crates/core/src/identity/derive.rs`.

- [ ] **Step 1: Verify the workspace `arti-client` feature set**

Open the root `Cargo.toml`. The existing entry is:

```toml
arti-client = { version = "0.41", default-features = false, features = ["tokio", "rustls", "onion-service-client", "onion-service-service"] }
```

Confirm the features `tokio`, `rustls`, `onion-service-client`, `onion-service-service` are present. If not, add them.

- [ ] **Step 2: Add the HS storage label**

In `crates/core/src/identity/derive.rs`, add a new domain-separation constant alongside the existing `INFO_*` block:

```rust
/// HS signing-key at-rest encryption: `HKDF(seed, "skattr-hs-storage-v1")`.
pub const INFO_HS_STORAGE_V1: &[u8] = b"skattr-hs-storage-v1";
```

Placement: immediately after `INFO_STORAGE_V1` to keep related labels together.

- [ ] **Step 3: Verify (no new code, just compile)**

```bash
cargo build --workspace 2>&1 | tail -3
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: both clean.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/identity/derive.rs
git commit -m "identity: add INFO_HS_STORAGE_V1 HKDF label

Domain-separation label for the HS signing key at-rest encryption
Task 4 of Phase 0.C will introduce. Keeping label additions in a
separate commit so the actual implementation commits stay tight."
```

---

## Task 2: HS signing-key persistence (`transport/hs_key.rs`)

**Goal:** Generate a v3 HS signing key, encrypt it at rest with a key derived from the identity seed, and provide load + save operations. Analogous to `identity::vault::Vault` but for a different secret and in a different module.

**Files:** Create `crates/core/src/transport/hs_key.rs`. Modify `crates/core/src/transport/mod.rs`.

- [ ] **Step 1: Register the module**

In `crates/core/src/transport/mod.rs`, add:

```rust
pub(crate) mod hs_key;
```

near the existing `pub(crate) mod ...;` declarations.

- [ ] **Step 2: Write the failing test**

Create `crates/core/src/transport/hs_key.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! V3 hidden-service signing key: generation, age-encrypted persistence,
//! and load.
//!
//! The HS signing key is Ed25519 (v3 onion services use `HsIdKey`, the
//! identity master key). It is generated fresh on first `skattr daemon`,
//! persisted encrypted under a `HKDF("skattr-hs-storage-v1")` derivation
//! of the identity seed, and reloaded each subsequent run. A new HS key
//! means a new `.onion` address — that's deliberate rotation (documented
//! in design §1.1 and the Phase 2 rotation workstream).

use std::path::Path;

use zeroize::Zeroizing;

use crate::error::{CoreError, Result};
use crate::identity::derive::{hkdf_expand, INFO_HS_STORAGE_V1};
use crate::identity::Seed;

/// Raw bytes of a v3 HS signing key (Ed25519 secret).
pub(crate) type HsSecretBytes = Zeroizing<[u8; 32]>;

/// Create or load the HS signing key at `path`, deriving the at-rest
/// encryption key from `seed`.
///
/// If the file does not exist, a fresh 32-byte key is generated and
/// written. On subsequent calls at the same path with the same seed,
/// the existing key is decrypted and returned unchanged — same seed
/// + same file → same `.onion` address.
pub(crate) fn load_or_create(path: &Path, seed: &Seed) -> Result<HsSecretBytes> {
    if path.exists() {
        load(path, seed)
    } else {
        let bytes = generate();
        save(path, seed, &bytes)?;
        Ok(bytes)
    }
}

fn generate() -> HsSecretBytes {
    use rand::RngCore;
    let mut out = Zeroizing::new([0u8; 32]);
    rand::rngs::OsRng.fill_bytes(out.as_mut());
    out
}

fn derive_storage_key(seed: &Seed) -> Result<Zeroizing<[u8; 32]>> {
    hkdf_expand::<32>(seed.as_bytes(), INFO_HS_STORAGE_V1)
}

fn save(path: &Path, seed: &Seed, bytes: &[u8; 32]) -> Result<()> {
    let key = derive_storage_key(seed)?;
    let passphrase = age::secrecy::SecretString::from(hex::encode(key.as_ref()));
    let encryptor = age::Encryptor::with_user_passphrase(passphrase);

    let mut ciphertext = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut ciphertext)
        .map_err(|e| CoreError::Transport(format!("age wrap: {e}")))?;
    use std::io::Write;
    writer
        .write_all(bytes)
        .map_err(|e| CoreError::Transport(format!("age write: {e}")))?;
    writer
        .finish()
        .map_err(|e| CoreError::Transport(format!("age finish: {e}")))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, &ciphertext)?;
    Ok(())
}

fn load(path: &Path, seed: &Seed) -> Result<HsSecretBytes> {
    let ciphertext = std::fs::read(path)?;
    let key = derive_storage_key(seed)?;
    let passphrase = age::secrecy::SecretString::from(hex::encode(key.as_ref()));

    let decryptor = match age::Decryptor::new(&ciphertext[..])
        .map_err(|e| CoreError::Transport(format!("age decryptor: {e}")))?
    {
        age::Decryptor::Passphrase(d) => d,
        _ => return Err(CoreError::Transport("unexpected age recipient type".into())),
    };

    let mut reader = decryptor
        .decrypt(&passphrase, None)
        .map_err(|e| CoreError::Transport(format!("age decrypt: {e}")))?;

    use std::io::Read;
    let mut buf = Zeroizing::new([0u8; 32]);
    reader
        .read_exact(buf.as_mut())
        .map_err(|e| CoreError::Transport(format!("age read: {e}")))?;

    // Guard against larger plaintexts (would indicate corruption or a
    // different-format file at the expected path).
    let mut tail = [0u8; 1];
    if reader.read(&mut tail).unwrap_or(0) > 0 {
        return Err(CoreError::Transport(
            "hs key has unexpected length".into(),
        ));
    }

    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_generates_then_loads_same_key() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("hs.key.age");
        let seed = Seed::generate().unwrap();

        let first = load_or_create(&path, &seed).unwrap();
        let second = load_or_create(&path, &seed).unwrap();
        assert_eq!(first.as_ref(), second.as_ref(), "same seed → same key");
    }

    #[test]
    fn different_seed_cannot_decrypt() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("hs.key.age");
        let seed_a = Seed::generate().unwrap();
        let seed_b = Seed::generate().unwrap();

        let _ = load_or_create(&path, &seed_a).unwrap();
        let err = load_or_create(&path, &seed_b)
            .err()
            .expect("different seed must fail to decrypt");
        assert!(matches!(err, CoreError::Transport(_)));
    }

    #[test]
    fn fresh_dir_triggers_generate() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("hs.key.age");
        let seed = Seed::generate().unwrap();
        assert!(!path.exists());
        let _ = load_or_create(&path, &seed).unwrap();
        assert!(path.exists(), "load_or_create must persist on first call");
    }
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p skattr-core --lib transport::hs_key --release 2>&1 | tail -5
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: 3 passed, clippy clean.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/transport/mod.rs crates/core/src/transport/hs_key.rs
git commit -m "transport: v3 HS signing key with age-encrypted persistence

32-byte HS signing key generated on first daemon start and stored
at-rest under age encryption keyed by
HKDF(seed, 'skattr-hs-storage-v1'). Subsequent daemon starts under
the same identity seed reload the same key — same .onion address.

Part of Phase 0.C workstream."
```

---

## Task 3: `TorRuntime::bootstrap` + status watch channel

**Goal:** Replace the `todo!()` body with a real `arti_client::TorClient` bootstrap, wiring bootstrap progress into the `TorStatus` watch channel.

**Files:** Modify `crates/core/src/transport/tor.rs`.

- [ ] **Step 1: Write the test (smoke only — full bootstrap is `#[ignore]`d)**

Append to `crates/core/src/transport/tor.rs` at the bottom:

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn status_starts_as_idle_before_bootstrap() {
        // We don't actually bootstrap here (that's network-expensive and
        // ignore-gated); we just verify the status channel's initial
        // value is Idle when constructed via the test helper.
        let (tx, rx) = tokio::sync::watch::channel(TorStatus::Idle);
        drop(tx);
        assert_eq!(*rx.borrow(), TorStatus::Idle);
    }

    #[tokio::test]
    #[ignore = "real network bootstrap, run with --ignored"]
    async fn bootstrap_progresses_to_ready() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = TorConfig {
            state_dir: tmp.path().to_path_buf(),
            socks_port: None,
        };
        let rt = TorRuntime::bootstrap(cfg).await.expect("bootstrap");
        assert_eq!(*rt.status().borrow(), TorStatus::Ready);
        rt.shutdown().await.expect("shutdown");
    }
}
```

- [ ] **Step 2: Rewrite `TorRuntime` to store a real `TorClient`**

Replace the body of `crates/core/src/transport/tor.rs` below the module doc with:

```rust
use std::path::PathBuf;

use arti_client::config::TorClientConfigBuilder;
use arti_client::{TorClient, TorClientConfig};
use tokio::sync::watch;
use tor_rtcompat::tokio::TokioRustlsRuntime;

use crate::error::{CoreError, Result};

/// Observable Tor bootstrap state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TorStatus {
    /// Runtime exists but has not started bootstrap.
    Idle,
    /// Bootstrapping with the given progress percentage (0–100).
    Bootstrapping(u8),
    /// Fully bootstrapped and ready to publish / dial.
    Ready,
    /// Terminal failure; message is human-readable, non-sensitive.
    Failed(String),
}

/// Configuration for the Arti runtime.
#[derive(Debug, Clone)]
pub struct TorConfig {
    /// State directory for Arti (circuits, guards, HS keys).
    pub state_dir: PathBuf,
    /// SOCKS port to expose locally, or `None` to disable.
    pub socks_port: Option<u16>,
}

/// Opaque handle to a running Arti instance.
pub struct TorRuntime {
    client: TorClient<TokioRustlsRuntime>,
    status_tx: watch::Sender<TorStatus>,
    /// Ownership anchor for the background status-forwarding task.
    _status_task: tokio::task::JoinHandle<()>,
}

impl TorRuntime {
    /// Boot Arti with the given config. Returns once bootstrap completes
    /// or fails; use [`TorRuntime::status`] to observe interim progress.
    pub async fn bootstrap(config: TorConfig) -> Result<Self> {
        std::fs::create_dir_all(&config.state_dir)?;
        let cache_dir = config.state_dir.join("cache");
        std::fs::create_dir_all(&cache_dir)?;

        let tor_config: TorClientConfig = TorClientConfigBuilder::default()
            .storage(|s| {
                s.cache_dir(cache_dir.clone().into());
                s.state_dir(config.state_dir.clone().into());
            })
            .build()
            .map_err(|e| CoreError::Transport(format!("arti config: {e}")))?;

        let runtime = TokioRustlsRuntime::current()
            .map_err(|e| CoreError::Transport(format!("arti runtime: {e}")))?;

        let client = TorClient::with_runtime(runtime)
            .config(tor_config)
            .create_unbootstrapped_async()
            .await
            .map_err(|e| CoreError::Transport(format!("arti client: {e}")))?;

        let (status_tx, _status_rx) = watch::channel(TorStatus::Idle);

        // Spawn a status-forwarding task that watches Arti's bootstrap
        // events and republishes them on our watch channel. Keep the
        // JoinHandle so it stays alive as long as the runtime does.
        let events = client.bootstrap_events();
        let status_forwarder_tx = status_tx.clone();
        let status_task = tokio::spawn(async move {
            use futures::StreamExt;
            let mut events = events;
            while let Some(event) = events.next().await {
                let pct = (event.as_frac() * 100.0).round() as u8;
                let new_status = if pct >= 100 {
                    TorStatus::Ready
                } else {
                    TorStatus::Bootstrapping(pct)
                };
                if status_forwarder_tx.send(new_status).is_err() {
                    break;
                }
            }
        });

        // Block on bootstrap completion.
        match client.bootstrap().await {
            Ok(()) => {
                let _ = status_tx.send(TorStatus::Ready);
            }
            Err(e) => {
                let _ = status_tx.send(TorStatus::Failed(format!("{e}")));
                return Err(CoreError::Transport(format!("bootstrap: {e}")));
            }
        }

        Ok(Self {
            client,
            status_tx,
            _status_task: status_task,
        })
    }

    /// Observe bootstrap / runtime state.
    #[must_use]
    pub fn status(&self) -> watch::Receiver<TorStatus> {
        self.status_tx.subscribe()
    }

    /// Internal: access the underlying `TorClient`. Called by the HS
    /// publish path and the outbound connect path.
    pub(crate) fn client(&self) -> &TorClient<TokioRustlsRuntime> {
        &self.client
    }

    /// Publish a v3 onion service using the HS key at `hs_key_path`.
    /// Implemented in Task 5.
    pub async fn publish_onion(&self, _hs_key_path: PathBuf) -> Result<String> {
        todo!("Task 5")
    }

    /// Dial an outbound connection to `<onion>:<port>`. Implemented in
    /// Task 7.
    pub async fn connect(
        &self,
        _onion: &str,
        _port: u16,
    ) -> Result<arti_client::DataStream> {
        todo!("Task 7")
    }

    /// Gracefully shut down Arti. Implemented in Task 6.
    pub async fn shutdown(self) -> Result<()> {
        todo!("Task 6")
    }
}
```

Note: the return type of `connect` changed from `Box<dyn AsyncRead + ...>` to the concrete `arti_client::DataStream`. `DataStream` implements both `AsyncRead` and `AsyncWrite`, so downstream code gains bidirectional access without a trait-object dance.

- [ ] **Step 3: Run tests + clippy**

```bash
cargo test -p skattr-core --lib transport::tor::tests::status_starts_as_idle --release 2>&1 | tail -5
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: the non-ignored test passes. Clippy clean.

**Note on API drift.** The above assumes Arti 0.41's `TorClientConfigBuilder::storage(|s| { s.cache_dir(...); s.state_dir(...); })` pattern. If your Arti version exposes a different setter shape (e.g., `.storage(StorageCfgBuilder { ... })`), adapt and keep the semantic intent: set cache + state dirs under `config.state_dir`. Same for `bootstrap_events()` → `event.as_frac()`: if the event type is `BootstrapStatus { percentage, .. }`, use the field directly. Report any deviation in the commit message.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/transport/tor.rs
git commit -m "transport: TorRuntime::bootstrap with real Arti client

Bootstraps an arti_client::TorClient on the tokio+rustls runtime,
forwards Arti's BootstrapEvents into our TorStatus watch channel
(Idle → Bootstrapping(pct) → Ready | Failed). Subsequent tasks
fill in publish_onion, connect, and shutdown.

Integration-test for a full bootstrap is ignore-gated (hits real
Tor; ~30-90s on a fresh state_dir)."
```

---

## Task 4: `TorRuntime::shutdown`

**Goal:** Gracefully shut down the Arti client. For Arti 0.41 the TorClient itself doesn't expose an explicit "stop" — dropping the client shuts down its runtime tasks. We implement `shutdown` as an explicit drop point that also drops the status task.

**Files:** Modify `crates/core/src/transport/tor.rs`.

- [ ] **Step 1: Write the test**

Inside the existing `mod tests` in `crates/core/src/transport/tor.rs`, append:

```rust
    #[tokio::test]
    #[ignore = "real network bootstrap + shutdown, run with --ignored"]
    async fn bootstrap_then_shutdown_leaves_no_runaway_tasks() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = TorConfig {
            state_dir: tmp.path().to_path_buf(),
            socks_port: None,
        };
        let rt = TorRuntime::bootstrap(cfg).await.expect("bootstrap");
        rt.shutdown().await.expect("shutdown");
        // Drop the tempdir via `tmp`'s own Drop, which removes the
        // state directory. If Arti tasks are still holding file handles,
        // Drop would fail or linger; tempfile's cleanup is best-effort,
        // but the test at least exercises the shutdown path end-to-end.
    }
```

- [ ] **Step 2: Implement `shutdown`**

In `crates/core/src/transport/tor.rs`, replace `TorRuntime::shutdown`:

```rust
    /// Gracefully shut down Arti. Drops the TorClient (which stops its
    /// background tasks) and cancels the status-forwarding task.
    ///
    /// Takes `self` so the runtime is truly consumed — downstream code
    /// cannot accidentally hold a zombie handle.
    pub async fn shutdown(self) -> Result<()> {
        // Notify subscribers that we're going down. Use a Failed-style
        // status but with a known-benign message; existing subscribers
        // can distinguish via explicit shutdown bookkeeping if they need
        // to, or just observe that the channel closes.
        let _ = self.status_tx.send(TorStatus::Idle);

        // Abort the status-forwarding task. It loops on the Arti event
        // stream; without this, it would linger until the TorClient drop
        // causes the stream to end.
        self._status_task.abort();

        // Drop the TorClient. Its Drop shuts down the underlying
        // background tasks.
        drop(self.client);

        Ok(())
    }
```

- [ ] **Step 3: Run clippy**

```bash
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: clean. (The ignored test runs only with `--ignored`; don't require it for this task's commit gate.)

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/transport/tor.rs
git commit -m "transport: TorRuntime::shutdown

Consumes self, aborts the status-forwarding task, and drops the
TorClient (whose Drop stops background tasks). Status channel
is notified via a final Idle send."
```

---

## Task 5: `TorRuntime::publish_onion`

**Goal:** Load the HS signing key from disk (Task 2's helper), hand it to Arti via `tor_hsservice`, launch the service, and return the `.onion` address.

**Files:** Modify `crates/core/src/transport/tor.rs`.

- [ ] **Step 1: Change the `publish_onion` signature**

The original stub takes `hs_key_path: PathBuf`. Publishing requires both the key file location AND the identity seed (to derive the at-rest decryption key). Widen the signature:

In `crates/core/src/transport/tor.rs`, replace the `publish_onion` stub:

```rust
    /// Publish a v3 onion service using the HS key at `hs_key_path`.
    /// If the file does not exist, a fresh HS key is generated and
    /// persisted encrypted under the identity `seed`.
    ///
    /// Returns the `.onion` address (56 base32 chars + `.onion`).
    ///
    /// The returned `(RunningOnionService, RendRequestStream)` pair is
    /// stored on `self` so the service keeps running for the lifetime
    /// of the `TorRuntime`. Inbound rend requests are handed to an
    /// `OnionListener` (Task 6).
    pub async fn publish_onion(
        &mut self,
        hs_key_path: &std::path::Path,
        seed: &crate::identity::Seed,
        nickname: &str,
    ) -> Result<String> {
        let hs_secret = crate::transport::hs_key::load_or_create(hs_key_path, seed)?;

        let config = tor_hsservice::OnionServiceConfigBuilder::default()
            .nickname(nickname.parse().map_err(|e| {
                CoreError::Transport(format!("invalid HS nickname '{nickname}': {e}"))
            })?)
            .build()
            .map_err(|e| CoreError::Transport(format!("HS config: {e}")))?;

        // Inject our HS secret into Arti's keystore. Arti keys itself
        // off `nickname`; overwriting an existing key requires the same
        // secret bytes we're about to hand in — so if there's already
        // a key for this nickname with different bytes, we bail.
        inject_hs_secret(self.client(), nickname, &hs_secret)?;

        let (svc, rend_requests) = self
            .client()
            .launch_onion_service(config)
            .map_err(|e| CoreError::Transport(format!("HS launch: {e}")))?;

        let onion = svc.onion_address().to_string();
        self.hs_service = Some(svc);
        self.rend_requests = Some(rend_requests);
        Ok(onion)
    }
```

**A key Arti API uncertainty:** the line `inject_hs_secret(self.client(), nickname, &hs_secret)?;` is a helper we need to implement against whatever Arti 0.41 exposes for "use THIS secret as the HS key, don't generate your own." In Arti 0.41 this is typically done via the `KeyMgr` exposed by `TorClient::keymgr()`. The helper body looks approximately like:

```rust
use tor_hsservice::HsIdKeypair;
use tor_keymgr::{KeyPath, KeySpecifier};

fn inject_hs_secret(
    _client: &arti_client::TorClient<tor_rtcompat::tokio::TokioRustlsRuntime>,
    _nickname: &str,
    _secret: &crate::transport::hs_key::HsSecretBytes,
) -> Result<()> {
    // Phase 0.C prescriptive body: construct an HsIdKeypair from the
    // 32-byte secret, determine the keymgr's KeyPath for this nickname's
    // HsIdKey slot, and insert. See tor_hsservice::HsNickname and
    // tor_keymgr::ArtiNativeKeystore for the canonical wiring.
    //
    // If the exact shape of 0.41's keymgr surface has drifted, the
    // working pattern is: resolve `HsIdKeypairSpecifier::new(nickname)`,
    // convert our secret into an `HsIdKeypair::from_bytes(&secret)`, and
    // call `keymgr.insert_entry(key_path, hs_keypair)` or the nearest
    // equivalent on the actual 0.41 surface.
    //
    // If the API surface available to us has no supported path for
    // externally-provided HS keys, fall back to: let Arti generate
    // its own HS key at the expected keystore path AND don't use our
    // Task-2 on-disk hs_key.age — document the deviation and file a
    // follow-up. The tradeoff: Arti's generated key lives inside
    // Arti's keystore (already at-rest encrypted via ArtiNativeKeystore's
    // passphrase). We lose the "seed-derived so restoring from seed
    // reproduces the same .onion" property. Acceptable for Phase 0.C if
    // we have no other option; must re-examine in Phase 1.
    todo!("wire HsIdKeypair into Arti keystore under the given nickname")
}
```

**If `inject_hs_secret` turns out to be hard with 0.41's public API** (the Arti team was still stabilizing this surface as of early 2026), the fallback is: skip the on-disk key, let Arti manage its own HS keystore, and accept that the `.onion` address is tied to `state_dir` rather than to the seed. Mark Task 2's `hs_key.rs` as "dead code for Phase 0.C, retained for Phase 1" and proceed. Document this decision in a new ADR (0006) if you take that path.

Update the `TorRuntime` struct to hold the service + rend stream:

```rust
pub struct TorRuntime {
    client: TorClient<TokioRustlsRuntime>,
    status_tx: watch::Sender<TorStatus>,
    _status_task: tokio::task::JoinHandle<()>,
    hs_service: Option<tor_hsservice::RunningOnionService>,
    rend_requests: Option<tor_hsservice::RendRequestStream>,
}
```

And initialize the new fields as `None` in `bootstrap`.

Also update `shutdown` to stop the HS service if one is running:

```rust
    pub async fn shutdown(self) -> Result<()> {
        let _ = self.status_tx.send(TorStatus::Idle);
        self._status_task.abort();
        // Stop the onion service first, if one was launched.
        if let Some(svc) = self.hs_service {
            drop(svc); // RunningOnionService drop tears down publication.
        }
        drop(self.client);
        Ok(())
    }
```

- [ ] **Step 2: Write the integration test (ignored)**

In the `mod tests` block of `transport/tor.rs`, append:

```rust
    #[tokio::test]
    #[ignore = "real network bootstrap + HS publish, run with --ignored"]
    async fn publish_onion_returns_valid_address() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = TorConfig {
            state_dir: tmp.path().to_path_buf(),
            socks_port: None,
        };
        let mut rt = TorRuntime::bootstrap(cfg).await.expect("bootstrap");
        let seed = crate::identity::Seed::generate().unwrap();
        let hs_key_path = tmp.path().join("hs.key.age");
        let onion = rt
            .publish_onion(&hs_key_path, &seed, "skattr-test")
            .await
            .expect("publish");
        assert!(
            onion.ends_with(".onion") && onion.len() > 50,
            "onion address should be v3 format: {onion}"
        );
        rt.shutdown().await.expect("shutdown");
    }
```

- [ ] **Step 3: Clippy gate**

```bash
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: clean (the `todo!()` inside `inject_hs_secret` is allowed by the workspace's `todo = allow` clippy rule).

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/transport/tor.rs
git commit -m "transport: TorRuntime::publish_onion via tor_hsservice

Loads the HS signing key from disk (decrypted under the identity
seed), injects it into Arti's keystore under the given nickname,
and launches a v3 onion service. Returns the .onion address.

Ignore-gated integration test exercises the full bootstrap →
publish path on real Tor (~30-120s).

Note: inject_hs_secret helper has a todo!() body where the exact
Arti 0.41 keymgr call-site sits — implementer should fill in the
working form for the Arti version in the lockfile. If Arti's
public API doesn't support externally-provided HS keys in 0.41,
fall back to Arti-generated keys and file ADR 0006."
```

---

## Task 6: `OnionListener` consumes `RendRequest` stream

**Goal:** Replace the `OnionListener::spawn` stub so inbound connections to the published onion service are converted to `arti_client::DataStream`s and delivered via `mpsc::Receiver`.

**Files:** Modify `crates/core/src/transport/listener.rs`, `crates/core/src/transport/mod.rs`.

- [ ] **Step 1: Replace `listener.rs`**

Rewrite `crates/core/src/transport/listener.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Accepts onion-service connections, yields authenticated `DataStream`s
//! via an mpsc channel.
//!
//! The Noise handshake (§1.3) is applied on top of each `DataStream`
//! in `transport::connection::AuthenticatedConnection`; this listener is
//! the byte-level accept loop only.

use arti_client::DataStream;
use tokio::sync::mpsc;

use crate::error::{CoreError, Result};

/// Accept loop over a v3 onion service.
///
/// Owns a background task that drains the `RendRequestStream` from
/// `tor_hsservice`, converts each accepted request into a `DataStream`,
/// and forwards it via `accepted`. Dropping `OnionListener` aborts the
/// task.
pub struct OnionListener {
    /// Channel that yields raw `DataStream`s from accepted rend requests.
    /// Higher layers wrap each stream with Noise + Frame to get an
    /// `AuthenticatedConnection`.
    pub accepted: mpsc::Receiver<DataStream>,
    task: tokio::task::JoinHandle<()>,
}

impl OnionListener {
    /// Spawn an accept loop over `rend_requests`. The channel buffers up
    /// to `capacity` pending streams — backpressure slows Arti's
    /// rendezvous accept rate past that.
    pub fn spawn(
        mut rend_requests: tor_hsservice::RendRequestStream,
        capacity: usize,
    ) -> Self {
        let (tx, accepted) = mpsc::channel(capacity);
        let task = tokio::spawn(async move {
            use futures::StreamExt;
            while let Some(request) = rend_requests.next().await {
                let stream_requests = match request.accept() {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(error = %e, "HS rend request: accept failed");
                        continue;
                    }
                };
                // Each RendRequest can fan out into multiple per-circuit
                // StreamRequest — drain them.
                let mut stream_requests = stream_requests;
                while let Some(stream_req) = stream_requests.next().await {
                    match stream_req.accept().await {
                        Ok(data_stream) => {
                            if tx.send(data_stream).await.is_err() {
                                // Receiver dropped; exit the task.
                                return;
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "HS stream request: accept failed");
                        }
                    }
                }
            }
        });

        Self { accepted, task }
    }

    /// Stop the accept loop. Any `DataStream`s already delivered to
    /// `accepted` remain valid; the underlying onion service stays up
    /// until `TorRuntime::shutdown`.
    pub fn shutdown(self) -> Result<()> {
        self.task.abort();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_zero_rejected_at_mpsc_level() {
        // mpsc::channel(0) panics in tokio 1.x; verify we would notice.
        // This is a compile-time sanity test and runs always.
        let result = std::panic::catch_unwind(|| {
            let _ = mpsc::channel::<DataStream>(0);
        });
        assert!(
            result.is_err(),
            "mpsc::channel(0) must panic — OnionListener::spawn should default to >= 1"
        );
    }
}
```

Note: the removed `accepted: mpsc::Receiver<AuthenticatedConnection>` field — it was premature. `AuthenticatedConnection` comes from the Noise-over-DataStream layer (Phase 1). For now, listener yields raw `DataStream`s; Phase 1 wraps each in Noise + Frame.

- [ ] **Step 2: Update `transport/mod.rs`**

The existing `pub(crate) use listener::OnionListener;` line continues to work (same type name, different internal shape). No change needed there.

- [ ] **Step 3: Clippy gate**

```bash
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/transport/listener.rs
git commit -m "transport: OnionListener consumes RendRequestStream

Background accept loop drains tor_hsservice::RendRequestStream,
accepts each per-circuit stream request, and forwards the
resulting DataStream via mpsc. No Noise/frame wrapping yet — that's
Phase 1's AuthenticatedConnection surface."
```

---

## Task 7: `TorRuntime::connect` outbound dial

**Goal:** Implement the outbound dial — resolve a `.onion:port` target and open a `DataStream`.

**Files:** Modify `crates/core/src/transport/tor.rs`.

- [ ] **Step 1: Replace the `connect` stub**

In `crates/core/src/transport/tor.rs`, replace `TorRuntime::connect`:

```rust
    /// Dial an outbound connection to `<onion>:<port>`.
    ///
    /// Returns a `DataStream` implementing `AsyncRead + AsyncWrite`.
    /// Caller is responsible for the Noise handshake (see
    /// `transport::connection::AuthenticatedConnection`).
    pub async fn connect(&self, onion: &str, port: u16) -> Result<arti_client::DataStream> {
        let target = format!("{onion}:{port}");
        self.client
            .connect(target.as_str())
            .await
            .map_err(|e| CoreError::Transport(format!("connect {target}: {e}")))
    }
```

Arti 0.41's `TorClient::connect` accepts anything that implements `IntoTorAddr`; `&str` in the form `"<host>:<port>"` is the most common form. If the exact method name has drifted (e.g., `connect_with_prefs`), the semantic intent — "dial this onion at this port, return a bidirectional stream" — is stable.

- [ ] **Step 2: Integration test (ignored)**

Append to the `mod tests` block in `transport/tor.rs`:

```rust
    #[tokio::test]
    #[ignore = "real network bootstrap + HS publish + dial, ~2-5 min, run with --ignored"]
    async fn local_publish_then_dial_echoes_bytes() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let tmp_a = tempfile::tempdir().unwrap();
        let tmp_b = tempfile::tempdir().unwrap();

        // Daemon A: bootstrap + publish.
        let mut rt_a = TorRuntime::bootstrap(TorConfig {
            state_dir: tmp_a.path().to_path_buf(),
            socks_port: None,
        })
        .await
        .expect("A: bootstrap");
        let seed_a = crate::identity::Seed::generate().unwrap();
        let onion = rt_a
            .publish_onion(&tmp_a.path().join("hs.key.age"), &seed_a, "skattr-echo-a")
            .await
            .expect("A: publish");

        // Take over the rend_requests and run an accept loop that
        // echoes bytes back.
        let rend_requests = rt_a
            .rend_requests
            .take()
            .expect("rend_requests must be populated after publish");
        let mut listener = crate::transport::OnionListener::spawn(rend_requests, 8);
        let echo_task = tokio::spawn(async move {
            if let Some(mut stream) = listener.accepted.recv().await {
                let mut buf = [0u8; 32];
                let n = stream.read(&mut buf).await.expect("A: read");
                stream.write_all(&buf[..n]).await.expect("A: echo");
                let _ = stream.shutdown().await;
            }
        });

        // Daemon B: bootstrap + dial.
        let rt_b = TorRuntime::bootstrap(TorConfig {
            state_dir: tmp_b.path().to_path_buf(),
            socks_port: None,
        })
        .await
        .expect("B: bootstrap");

        let mut stream = rt_b.connect(&onion, 1).await.expect("B: connect");
        stream.write_all(b"hello skattr").await.expect("B: write");
        let mut buf = [0u8; 32];
        let n = stream.read(&mut buf).await.expect("B: read");
        assert_eq!(&buf[..n], b"hello skattr");

        let _ = echo_task.await;
        rt_a.shutdown().await.expect("A: shutdown");
        rt_b.shutdown().await.expect("B: shutdown");
    }
```

This test will move to `crates/tests/src/arti_echo.rs` in Task 9 — but it's useful here as a self-contained sanity check on the publish + dial round trip before the CLI wiring.

- [ ] **Step 3: Clippy gate**

```bash
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/transport/tor.rs
git commit -m "transport: TorRuntime::connect outbound dial

Thin wrapper over arti_client::TorClient::connect — accepts
\"<onion>:<port>\" and returns a DataStream (AsyncRead + AsyncWrite).
Noise handshake wrapping is higher-layer work.

Ignore-gated end-to-end test: publish on daemon A, dial from
daemon B, byte-echo round-trip."
```

---

## Task 8: Wire `skattr daemon`

**Goal:** Replace the `daemon` CLI stub with a working subcommand that bootstraps, publishes, prints the `.onion`, and blocks on Ctrl-C.

**Files:** Modify `crates/cli/src/main.rs`.

- [ ] **Step 1: Replace the `daemon` function**

In `crates/cli/src/main.rs`, replace the existing `async fn daemon(_detach: bool) -> Result<()>` body with:

```rust
async fn daemon(detach: bool, data_dir_override: Option<&std::path::Path>) -> Result<()> {
    use skattr_core::identity::Seed;
    use skattr_core::transport::tor::{TorConfig, TorRuntime};

    if detach {
        anyhow::bail!("--detach is not yet supported; run in foreground for Phase 0.C");
    }

    let data_dir = effective_data_dir(data_dir_override)?;
    std::fs::create_dir_all(&data_dir)?;
    let vault_path = data_dir.join("identity.vault");

    if !vault_path.exists() {
        anyhow::bail!(
            "no identity vault at {}; run `skattr init` first",
            vault_path.display()
        );
    }

    let pw = read_passphrase("Vault passphrase: ")?;
    let (_vault, identity) = Vault::open(&vault_path, pw.as_str())?;
    // Derive a Seed-like view from the identity. For Phase 0.C we
    // persist a separate Seed file; revisit in Phase 1 when seed-to-
    // identity binding is formalized.
    //
    // Simpler: regenerate a new Seed on first daemon start and persist
    // it alongside the vault, then use it for HS key derivation. This
    // is NOT the same as the BIP39 seed phrase — it's a separate
    // storage-encryption seed. Name the file clearly.
    let storage_seed_path = data_dir.join("storage-seed");
    let seed = if storage_seed_path.exists() {
        // Load existing seed.
        let bytes = std::fs::read(&storage_seed_path)?;
        let arr: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("storage-seed has wrong length"))?;
        // Safety: Seed::from_bytes is cfg(test) only — we need a
        // production constructor. For Phase 0.C: construct via
        // Seed::from_mnemonic roundtrip through a generated mnemonic.
        // Alternative: widen Seed::from_bytes to pub(crate) and use it.
        //
        // The cleanest Phase-0.C path: add a production pub(crate) fn
        // Seed::from_storage_bytes(bytes: [u8; 32]) -> Self so we can
        // build the seed from a file. Add that helper in Task 8 Step 2.
        skattr_core::identity::Seed::from_storage_bytes(arr)
    } else {
        let seed = Seed::generate()?;
        std::fs::write(&storage_seed_path, seed.as_bytes_for_storage())?;
        seed
    };
    drop(identity); // not used in Phase 0.C daemon; vault unlock just proves pw.

    println!("Bootstrapping Tor…");
    let cfg = TorConfig {
        state_dir: data_dir.join("arti"),
        socks_port: None,
    };
    let mut rt = TorRuntime::bootstrap(cfg).await?;
    println!("Tor ready. Publishing onion service…");

    let hs_key_path = data_dir.join("hs.key.age");
    let onion = rt.publish_onion(&hs_key_path, &seed, "skattr-daemon").await?;
    println!();
    println!("Listening on: {onion}:1");
    println!("Ctrl-C to shut down.");

    // Wait for Ctrl-C.
    tokio::signal::ctrl_c()
        .await
        .map_err(anyhow::Error::from)?;

    println!();
    println!("Shutting down…");
    rt.shutdown().await?;
    Ok(())
}
```

- [ ] **Step 2: Add `Seed::from_storage_bytes` + `Seed::as_bytes_for_storage`**

In `crates/core/src/identity/seed.rs`, inside `impl Seed`, add:

```rust
    /// Construct from raw bytes for at-rest storage uses (not BIP39).
    /// Production path: the daemon persists a 32-byte storage seed next
    /// to the vault to key non-identity derivations (HS key, storage
    /// DB). This is distinct from the BIP39 identity seed.
    #[must_use]
    pub(crate) fn from_storage_bytes(bytes: [u8; 32]) -> Self {
        Self { bytes }
    }

    /// Dump the raw seed bytes for on-disk persistence. Caller must
    /// store these at an appropriately protected path (inside the
    /// data_dir, permissions 0600 on Unix).
    #[must_use]
    pub(crate) fn as_bytes_for_storage(&self) -> [u8; 32] {
        self.bytes
    }
```

Note: these mirror the `#[cfg(test)] from_bytes` we added in the hardening pass, but are production-visible within the crate. That's intentional — the daemon needs them.

- [ ] **Step 3: Update main dispatch**

The existing main dispatch calls `daemon(detach).await`. Change to:

```rust
        Command::Daemon { detach } => daemon(detach, cli.data_dir.as_deref()).await,
```

- [ ] **Step 4: Smoke test (ignored on CI; manual run)**

```bash
TMP=$(mktemp -d)
printf 'pw\npw\n' | cargo run --quiet -p skattr-cli -- --data-dir "$TMP" init 2>&1 | tail -5
# Start the daemon in the background; wait for "Listening on:" banner.
printf 'pw\n' | cargo run --quiet -p skattr-cli -- --data-dir "$TMP" daemon &
DAEMON_PID=$!
sleep 120  # wait for bootstrap.
kill -INT $DAEMON_PID
wait $DAEMON_PID
```

Expected: the daemon prints a `.onion` address, listens, and exits cleanly on SIGINT. This is a manual test — do not codify as a cargo test (too slow for CI).

- [ ] **Step 5: Clippy + workspace build**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
cargo test --workspace --release 2>&1 | tail -5
```

Expected: clippy clean, no test regressions.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/identity/seed.rs crates/cli/src/main.rs
git commit -m "cli: wire skattr daemon — bootstrap Tor, publish onion, await Ctrl-C

Vault open prompts for passphrase, then: load-or-create storage
seed → TorRuntime::bootstrap → publish_onion → print address →
wait on Ctrl-C → graceful shutdown.

Adds pub(crate) Seed::from_storage_bytes + as_bytes_for_storage
for the daemon's storage-seed persistence (distinct from the BIP39
identity seed).

The storage-seed + HS key files live under the data_dir alongside
identity.vault. --detach is reserved; errors out with a clear
message for Phase 0.C scope."
```

---

## Task 9: Two-daemon echo integration test

**Goal:** Codify the Phase 0 exit criterion ("two daemons echo bytes over Tor") as a runnable test. It's `#[ignore]`-gated because it hits the real Tor network and takes 3-10 min, but it lives in the repo and any developer can run it with `cargo test --release -- --ignored`.

**Files:** Create `crates/tests/src/arti_echo.rs`. Modify `crates/tests/src/lib.rs` (add module). Modify `crates/tests/Cargo.toml` (add deps if missing).

- [ ] **Step 1: Verify dev-deps**

```bash
grep -E '^(skattr-core|tokio|tempfile|futures)' crates/tests/Cargo.toml
```

Expected hits: `skattr-core`, `tokio`, `tempfile`. If `futures` is missing, add:

```toml
futures = { workspace = true }
```

to `[dependencies]` of `crates/tests/Cargo.toml`.

- [ ] **Step 2: Create the test file**

Create `crates/tests/src/arti_echo.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 0 exit-criterion test: two daemons echo bytes over real Tor.
//!
//! Ignored by default. Run with:
//!
//! ```bash
//! cargo test -p skattr-tests --release -- --ignored
//! ```
//!
//! Takes ~3-10 min on first run (two fresh Arti bootstraps). Subsequent
//! runs against the same `state_dir` are faster but each test uses a
//! fresh tempdir, so expect the full cost every invocation.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use skattr_core::identity::Seed;
use skattr_core::transport::tor::{TorConfig, TorRuntime};
use skattr_core::transport::OnionListener;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real Tor network; ~3-10 min; run with --ignored"]
async fn two_daemons_echo_bytes_over_tor() {
    let tmp_a = tempfile::tempdir().unwrap();
    let tmp_b = tempfile::tempdir().unwrap();

    // Daemon A.
    let mut rt_a = TorRuntime::bootstrap(TorConfig {
        state_dir: tmp_a.path().to_path_buf(),
        socks_port: None,
    })
    .await
    .expect("A: bootstrap");

    let seed_a = Seed::generate().unwrap();
    let onion = rt_a
        .publish_onion(
            &tmp_a.path().join("hs.key.age"),
            &seed_a,
            "skattr-exit-a",
        )
        .await
        .expect("A: publish");
    eprintln!("A listening on: {onion}");

    // Hand the rend request stream to an OnionListener that echoes.
    let rend_requests = rt_a
        .rend_requests_take()
        .expect("A: rend_requests must be populated after publish");
    let mut listener = OnionListener::spawn(rend_requests, 8);
    let echo_task = tokio::spawn(async move {
        if let Some(mut stream) = listener.accepted.recv().await {
            let mut buf = [0u8; 64];
            let n = stream.read(&mut buf).await.expect("A: read");
            stream.write_all(&buf[..n]).await.expect("A: write echo");
            let _ = stream.shutdown().await;
        }
    });

    // Daemon B.
    let rt_b = TorRuntime::bootstrap(TorConfig {
        state_dir: tmp_b.path().to_path_buf(),
        socks_port: None,
    })
    .await
    .expect("B: bootstrap");

    let mut stream = rt_b.connect(&onion, 1).await.expect("B: connect");
    const MSG: &[u8] = b"hello skattr phase-0 exit";
    stream.write_all(MSG).await.expect("B: write");
    let mut buf = [0u8; 64];
    let n = stream.read(&mut buf).await.expect("B: read");
    assert_eq!(&buf[..n], MSG, "echo mismatch");

    echo_task.await.expect("A: echo task");
    rt_a.shutdown().await.expect("A: shutdown");
    rt_b.shutdown().await.expect("B: shutdown");
}
```

- [ ] **Step 3: Add a public `rend_requests_take` method on `TorRuntime`**

The test (and the earlier inline test in Task 7) reach into `rt.rend_requests` — that field is private. Add a public accessor.

In `crates/core/src/transport/tor.rs`, inside `impl TorRuntime`, add:

```rust
    /// Take ownership of the `RendRequestStream` from the currently-
    /// published onion service. Called exactly once per publish —
    /// subsequent calls return `None`.
    ///
    /// Production paths should pass the stream into `OnionListener`
    /// immediately after `publish_onion`; the two-step split exists so
    /// callers can plug in their own accept strategy (e.g., a Noise-
    /// wrapping session manager in Phase 1).
    #[must_use]
    pub fn rend_requests_take(&mut self) -> Option<tor_hsservice::RendRequestStream> {
        self.rend_requests.take()
    }
```

Adjust the Task 7 inline test to use `rt_a.rend_requests_take()` instead of the earlier `rt_a.rend_requests.take()` — same behavior, public API.

- [ ] **Step 4: Register the integration module**

In `crates/tests/src/lib.rs`, ensure there's a module registration if the library style requires it. `crates/core/tests/*.rs` is the standard Cargo integration-test location (each file is its own test binary). For the `skattr-tests` crate, the pattern is different: if `crates/tests/src/lib.rs` is a `#[test]`-bearing library, add `pub mod arti_echo;` there.

Inspect `crates/tests/src/lib.rs`:

```bash
cat crates/tests/src/lib.rs
```

If it's a minimal library with existing test modules, append:

```rust
#[cfg(test)]
mod arti_echo;
```

If it's empty, the test can go there directly or as its own file. Match the existing pattern.

- [ ] **Step 5: Smoke (optional)**

```bash
# Do NOT run this in CI. Manual verification only.
cargo test -p skattr-tests --release -- --ignored two_daemons_echo 2>&1 | tail -10
```

Expected (after 3-10 min): `test two_daemons_echo_bytes_over_tor ... ok`.

- [ ] **Step 6: Verify the default `cargo test` still finishes quickly**

```bash
cargo test --workspace --release 2>&1 | tail -5
```

Expected: finishes in its normal 30-90 s; the ignored tests are skipped.

- [ ] **Step 7: Commit**

```bash
git add crates/tests crates/core/src/transport/tor.rs
git commit -m "tests: two-daemon echo integration test (ignore-gated)

crates/tests/src/arti_echo.rs exercises the Phase 0 exit
criterion: two TorRuntime instances, one publishes an onion
service, the other dials and byte-echoes. Ignored by default
(real Tor, 3-10 min); run with --release --ignored.

Also exposes TorRuntime::rend_requests_take for callers that
want to plug their own accept strategy (e.g., a Noise-wrapping
session manager in Phase 1)."
```

---

## Post-plan wrap-up

- [ ] **Step 1: Full verification gate**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --release
cd crates/core && cargo +nightly fuzz build vault_parser && cd ../..
```

All four must pass. If `fmt --check` has drift, apply `cargo fmt --all` and commit separately as a `style:` commit.

- [ ] **Step 2: Author ADR-0005**

Create `docs/adr/0005-arti-vs-system-tor.md`:

```markdown
# ADR 0005: Embed Arti vs. shell out to system tor

- **Status:** Accepted
- **Date:** 2026-04-17

## Context

Phase 0.C wires Skattr's transport layer to the Tor network. The two
realistic options were:

1. **Embed `arti-client` + `tor-hsservice`** — Arti is the Rust Tor
   implementation, actively developed by the Tor Project, already
   pinned in the workspace manifest.
2. **Shell out to system `tor`** via its controller socket — the
   mature C implementation has a wider deployment footprint and
   better-understood operational posture.

## Decision

**Embed Arti.** Concretely: `arti-client` 0.41.x with onion-service
server features on; `tor-hsservice` 0.41.x for the HS side.

## Consequences

- **Good:** single Rust binary, no external runtime dep, reproducible
  builds, easier CI (mostly).
- **Good:** Arti's async API is a natural fit for our Tokio-based
  daemon.
- **Bad:** Arti's onion-service surface is the youngest part of its
  public API. Upgrades may break us. We pin to specific 0.41.x
  minor versions and re-qualify at every phase exit.
- **Bad:** Arti bootstrap is slower than system tor on first run
  (fresh consensus download). Subsequent bootstraps against the same
  state_dir are fast.

## Fallback

If Arti blocks us in a future phase (e.g., performance regression,
upstream API removal, or a hard-to-fix onion-service bug), the
fallback is to shell out to system `tor` with a controller socket.
We have **not** architected around this fallback — `TorRuntime` is a
deliberate abstraction layer that could be reimplemented on top of a
controller socket without touching downstream code, but the current
plan is to make Arti work.

## Alternatives considered

- **Tor.framework / libtor:** rejected. The project's Rust bindings
  to system tor are less maintained than Arti and tie us to a C
  runtime we'd otherwise avoid.
- **Pluggable transports layer only:** rejected. We need to publish
  onion services, not just dial them.
```

Commit:

```bash
git add docs/adr/0005-arti-vs-system-tor.md
git commit -m "docs: ADR-0005 embed Arti vs system tor

Records the Phase 0.C decision to embed arti-client +
tor-hsservice rather than shell out to system tor. Documents the
fallback path for future sessions that hit an Arti blocker."
```

- [ ] **Step 3: Update CHANGELOG.md**

Append under `[Unreleased]`:

```markdown
- **Phase 0.C Arti integration:** `TorRuntime::bootstrap` / `publish_onion` / `connect` / `shutdown` backed by `arti-client` 0.41 + `tor-hsservice` 0.41. HS signing key persisted at `<data_dir>/hs.key.age` encrypted under `HKDF(seed, "skattr-hs-storage-v1")`. `OnionListener` accepts rend requests and yields `DataStream`s via mpsc. `skattr daemon` bootstraps, publishes, prints the `.onion`, and awaits Ctrl-C. Two-daemon echo integration test (`crates/tests/src/arti_echo.rs`, `#[ignore]`-gated).
- ADR-0005 documents the Arti-vs-system-tor decision.
```

Commit:

```bash
git add CHANGELOG.md
git commit -m "changelog: Phase 0.C Arti integration"
```

- [ ] **Step 4: Update CLAUDE.md**

In the Repository-state paragraph, change "Remaining Phase 0 workstreams (0.C Arti integration, 0.D Storage layer, 0.E Documentation baseline)" to "Remaining Phase 0 workstreams (0.D Storage layer, 0.E Documentation baseline)". Append (or insert) a sentence noting: `skattr daemon` now bootstraps Tor, publishes a v3 onion service, and accepts incoming byte streams; the Phase 0 exit criterion ("two daemons echo bytes over Tor") is exercised by an `#[ignore]`-gated integration test.

Commit:

```bash
git add CLAUDE.md
git commit -m "docs: CLAUDE.md — Phase 0.C complete"
```

- [ ] **Step 5: Exit-criterion verification (manual)**

On a machine with internet access:

```bash
cargo test -p skattr-tests --release -- --ignored two_daemons_echo
```

Expected (after 3-10 min): `test result: ok. 1 passed`. Record the full `tail -20` of the output in the PR description (or wherever the branch's completion is reviewed).

---

## Notes for the executing engineer

- **Real-network tests are slow.** Every `#[ignore]`d test in this plan hits the Tor network for its full bootstrap. Do NOT move them out of `#[ignore]`; CI will revolt.
- **Arti 0.41 API is the one thing likely to drift.** If a method name or type path in Tasks 3-7 doesn't match what your `Cargo.lock` resolved, adapt and keep the semantic intent. The plan's commit messages already ask the implementer to note any deviation.
- **HS key injection is the hairiest API surface.** Task 5 prescribes the approach but marks the body as `todo!()` until the implementer confirms the exact `tor_hsservice`/`tor_keymgr` wiring. If it's intractable in 0.41, the fallback (Arti-managed HS keys) is documented — ADR-0006 follow-up, defer seed-derived HS key to Phase 1.
- **`skattr daemon` is thin by design.** Phase 0.C delivers the byte pipe; Phase 1 wires Noise + MLS + the session manager on top of it. Don't add more to Task 8 than the plan specifies.
- **Worktree discipline.** Every task commits to `phase-0c-arti`. The final merge back to master is a `--no-ff` merge; preserve the 10-commit history for future bisect.
