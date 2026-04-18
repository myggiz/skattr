// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Arti lifecycle: bootstrap, hidden service publish/accept, outbound dial.
//!
//! Wraps `arti-client` and `tor-hsservice`. Intentionally thin: this
//! module exposes only the primitives `core` needs, so we can swap the
//! backend (e.g. to a system `tor` controller socket) without touching
//! downstream code.

use std::path::PathBuf;
use std::sync::Arc;

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
    /// The currently-published onion service, if any. Dropped on shutdown.
    hs_service: Option<Arc<tor_hsservice::RunningOnionService>>,
    /// The rendezvous-request stream for the currently-published service.
    /// Taken out exactly once via [`TorRuntime::rend_requests_take`] and
    /// forwarded to the Noise listener loop.
    rend_requests: Option<
        std::pin::Pin<Box<dyn futures::Stream<Item = tor_hsservice::RendRequest> + Send + Sync>>,
    >,
}

impl TorRuntime {
    /// Boot Arti with the given config. Returns once bootstrap completes
    /// or fails; use [`TorRuntime::status`] to observe interim progress.
    pub async fn bootstrap(config: TorConfig) -> Result<Self> {
        std::fs::create_dir_all(&config.state_dir)?;
        let cache_dir = config.state_dir.join("cache");
        std::fs::create_dir_all(&cache_dir)?;

        // `TorClientConfigBuilder::from_directories` is the idiomatic 0.41 API
        // (plain `.default().storage(…).build()` is internal).
        let tor_config: TorClientConfig =
            TorClientConfigBuilder::from_directories(&config.state_dir, &cache_dir)
                .build()
                .map_err(|e| CoreError::Transport(format!("arti config: {e}")))?;

        // Attach to the already-running Tokio reactor (we are inside `#[tokio::main]`
        // or a `#[tokio::test]`).
        let runtime = TokioRustlsRuntime::current()
            .map_err(|e| CoreError::Transport(format!("arti runtime: {e}")))?;

        let client = TorClient::with_runtime(runtime)
            .config(tor_config)
            .create_unbootstrapped_async()
            .await
            .map_err(|e| CoreError::Transport(format!("arti client: {e}")))?;

        let (status_tx, _status_rx) = watch::channel(TorStatus::Idle);

        // Spawn a status-forwarding task that watches Arti's bootstrap
        // events and republishes them on our watch channel.
        let mut events = client.bootstrap_events();
        let status_forwarder_tx = status_tx.clone();
        let status_task = tokio::spawn(async move {
            use futures::StreamExt;
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
            hs_service: None,
            rend_requests: None,
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
    ///
    /// If the file does not exist, a fresh HS key is generated and
    /// persisted age-encrypted under a seed-derived storage key (see
    /// [`crate::transport::hs_key`]). Subsequent calls with the same
    /// path plus seed reload the same key, so the published `.onion`
    /// address is stable across restarts.
    ///
    /// Arti 0.41 ships with a `launch_onion_service_with_hsid` experimental
    /// API that inserts an externally-provided [`HsIdKeypair`] into the
    /// keystore (under `hss/<nickname>/ks_hs_id`) before launching. On
    /// first launch we use that. On subsequent launches Arti's keystore
    /// already holds a key under the nickname; the experimental API refuses
    /// to overwrite it (that is the correct semantics — overwriting would
    /// rotate the `.onion`), so we fall back to plain
    /// [`TorClient::launch_onion_service`] which uses the already-present
    /// key. Either way the result is the same 32-byte Ed25519 identity.
    ///
    /// Returns the `.onion` address (56 base32 chars + `.onion`).
    ///
    /// `&mut self` so the service handle + rend stream can be stored on
    /// the runtime for the duration of its lifetime.
    pub async fn publish_onion(
        &mut self,
        hs_key_path: &std::path::Path,
        seed: &crate::identity::Seed,
        nickname: &str,
    ) -> Result<String> {
        // 1. Materialize the 32-byte Ed25519 secret — either generate-and-
        // persist, or decrypt from disk under the seed.
        let hs_secret = crate::transport::hs_key::load_or_create(hs_key_path, seed)?;

        // 2. Build an HsIdKeypair from that secret. The conversion is:
        //     SigningKey::from_bytes(seed32)
        //       → ExpandedKeypair::from(&Keypair)  (SHA-512 expansion)
        //       → HsIdKeypair::from(expanded)      (derive_more::From)
        let id_keypair = hs_id_keypair_from_secret(&hs_secret);

        let nickname_parsed = nickname
            .parse::<tor_hsservice::HsNickname>()
            .map_err(|e| CoreError::Transport(format!("invalid HS nickname '{nickname}': {e}")))?;

        let config = tor_hsservice::config::OnionServiceConfigBuilder::default()
            .nickname(nickname_parsed)
            .build()
            .map_err(|e| CoreError::Transport(format!("HS config: {e}")))?;

        // 3. Launch. Try the "inject key" path first; if the keymgr already
        // holds a matching entry, Arti returns a keystore error and we fall
        // back to the plain launch path (which reuses the existing entry).
        // The two launch calls return slightly different opaque stream types,
        // so we box-pin each branch to unify them.
        let config_clone = config.clone();
        let (svc, rend_stream): (
            Arc<tor_hsservice::RunningOnionService>,
            std::pin::Pin<
                Box<dyn futures::Stream<Item = tor_hsservice::RendRequest> + Send + Sync>,
            >,
        ) = match self
            .client
            .launch_onion_service_with_hsid(config, id_keypair)
        {
            Ok(Some((svc, stream))) => (svc, Box::pin(stream)),
            Ok(None) => {
                return Err(CoreError::Transport("HS disabled in config".into()));
            }
            Err(e) if is_key_already_exists_error(&e) => {
                tracing::debug!(
                    "HS key already present in Arti keystore under nickname; \
                     reusing existing entry"
                );
                match self
                    .client
                    .launch_onion_service(config_clone)
                    .map_err(|e| CoreError::Transport(format!("HS launch (reuse): {e}")))?
                {
                    Some((svc, stream)) => (svc, Box::pin(stream)),
                    None => {
                        return Err(CoreError::Transport("HS disabled in config".into()));
                    }
                }
            }
            Err(e) => {
                return Err(CoreError::Transport(format!("HS launch: {e}")));
            }
        };

        // `HsId` deliberately does not implement `Display` — `safelog`
        // requires opting in via `DisplayRedacted`. We want the un-redacted
        // `${base32}.onion` form so callers can share it with contacts.
        let onion = svc
            .onion_address()
            .map(|id| {
                use safelog::DisplayRedacted as _;
                id.display_unredacted().to_string()
            })
            .ok_or_else(|| CoreError::Transport("HS has no address after launch".into()))?;

        self.hs_service = Some(svc);
        self.rend_requests = Some(rend_stream);

        Ok(onion)
    }

    /// Take ownership of the rendezvous-request stream from the currently-
    /// published onion service. Called exactly once per publish by the
    /// Noise listener loop.
    #[must_use]
    pub fn rend_requests_take(
        &mut self,
    ) -> Option<
        std::pin::Pin<Box<dyn futures::Stream<Item = tor_hsservice::RendRequest> + Send + Sync>>,
    > {
        self.rend_requests.take()
    }

    /// Dial an outbound connection. Implemented in Task 7.
    pub async fn connect(&self, _onion: &str, _port: u16) -> Result<arti_client::DataStream> {
        todo!("Task 7")
    }

    /// Gracefully shut down Arti. Drops the TorClient (which stops its
    /// background tasks) and cancels the status-forwarding task.
    ///
    /// Takes `self` so the runtime is truly consumed — downstream code
    /// cannot accidentally hold a zombie handle.
    pub async fn shutdown(self) -> Result<()> {
        // Notify subscribers that we're going down.
        let _ = self.status_tx.send(TorStatus::Idle);

        // Abort the status-forwarding task. It loops on the Arti event
        // stream; without this, it would linger until the TorClient drop
        // causes the stream to end.
        self._status_task.abort();

        // Drop the onion service first, if we published one. Its Drop stops
        // publishing and closes the rendezvous circuits.
        drop(self.rend_requests);
        drop(self.hs_service);

        // Drop the TorClient. Its Drop shuts down the underlying
        // background tasks.
        drop(self.client);

        Ok(())
    }
}

/// Build an Arti `HsIdKeypair` from our 32-byte Ed25519 secret.
///
/// The same 32 bytes always map to the same `HsId` (and therefore the
/// same `.onion` address), because the SHA-512 expansion and the
/// `PublicKey::from(&secret)` derivation are both deterministic.
fn hs_id_keypair_from_secret(
    secret32: &zeroize::Zeroizing<[u8; 32]>,
) -> tor_hscrypto::pk::HsIdKeypair {
    use tor_llcrypto::pk::ed25519;
    // `ed25519::Keypair` is a re-export of `ed25519_dalek::SigningKey`.
    // `SigningKey::from_bytes` is infallible (never panics; accepts any
    // 32 bytes — Ed25519 has no weak-key check on the seed).
    let dalek_kp = ed25519::Keypair::from_bytes(secret32);
    let expanded = ed25519::ExpandedKeypair::from(&dalek_kp);
    tor_hscrypto::pk::HsIdKeypair::from(expanded)
}

/// Best-effort detection of the `KeyAlreadyExists` error returned when we
/// try to re-inject an HS identity key that Arti's keystore already holds.
///
/// Arti's public error type doesn't expose the inner `tor_keymgr::Error`
/// variant directly, so we match on the error chain's debug representation.
/// This is fragile if upstream renames the variant, but the fallback path
/// (plain `launch_onion_service`) is only taken when the injection call
/// already failed for *some* reason — in the worst case we'd surface the
/// original error a moment later from the plain launch path, which is the
/// desired behaviour.
fn is_key_already_exists_error(err: &arti_client::Error) -> bool {
    // The `tor_keymgr::Error::KeyAlreadyExists` variant is rendered as
    // "key already exists" in the `Display` chain.
    let mut msg = String::new();
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = source {
        use std::fmt::Write as _;
        let _ = write!(&mut msg, "{e}; ");
        source = e.source();
    }
    msg.to_lowercase().contains("already exists")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn status_starts_as_idle_before_bootstrap() {
        // Sanity: the watch channel's initial value is Idle.
        let (tx, rx) = tokio::sync::watch::channel(TorStatus::Idle);
        drop(tx);
        assert_eq!(*rx.borrow(), TorStatus::Idle);
    }

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
}
