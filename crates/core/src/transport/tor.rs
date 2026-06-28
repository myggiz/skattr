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
use crate::transport::TransportErrorKind;

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
        let mut builder = TorClientConfigBuilder::from_directories(&config.state_dir, &cache_dir);
        // Relax Arti's fs-mistrust ancestor-permission policing. Arti otherwise
        // refuses to start ("problem with filesystem permissions: Error while
        // trying to access persistent state") whenever any ancestor of the
        // state dir is group/other-writable — e.g. a umask-002 home, or `/tmp`.
        // That check targets multi-user server deployments; for a single-user
        // desktop app it only blocks startup. We already create our own dirs
        // 0700 and the message DB is age-encrypted at rest, so the protection
        // Arti is enforcing is redundant here. This is exactly the remedy Arti's
        // own error hint suggests (storage.permissions.dangerously_trust_everyone).
        builder.storage().permissions().dangerously_trust_everyone();
        let tor_config: TorClientConfig = builder.build().map_err(|e| {
            CoreError::Transport(TransportErrorKind::Other(format!("arti config: {e}")))
        })?;

        // Attach to the already-running Tokio reactor (we are inside `#[tokio::main]`
        // or a `#[tokio::test]`).
        let runtime = TokioRustlsRuntime::current().map_err(|e| {
            CoreError::Transport(TransportErrorKind::Other(format!("arti runtime: {e}")))
        })?;

        let client = TorClient::with_runtime(runtime)
            .config(tor_config)
            .create_unbootstrapped_async()
            .await
            .map_err(|e| {
                CoreError::Transport(TransportErrorKind::Other(format!("arti client: {e}")))
            })?;

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
                return Err(CoreError::Transport(TransportErrorKind::TorNotReady));
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
    /// Before launching, the seed-derived [`HsIdKeypair`] is injected into
    /// Arti's primary keystore via [`inject_hs_secret`], which probes first
    /// and skips the insert when a key is already present (the "subsequent
    /// run, same state_dir" path). Either way [`TorClient::launch_onion_service`]
    /// picks up the stored key and the resulting `.onion` address is stable.
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

        let nickname_parsed = nickname.parse::<tor_hsservice::HsNickname>().map_err(|e| {
            CoreError::Transport(TransportErrorKind::Other(format!(
                "invalid HS nickname '{nickname}': {e}"
            )))
        })?;

        let config = tor_hsservice::config::OnionServiceConfigBuilder::default()
            .nickname(nickname_parsed.clone())
            .build()
            .map_err(|e| {
                CoreError::Transport(TransportErrorKind::Other(format!("HS config: {e}")))
            })?;

        // 3. Inject the HS identity key into Arti's keystore (probe-before-
        // insert: no-op if the key is already present), then launch the
        // onion service using the stored key.
        inject_hs_secret(&self.client, &nickname_parsed, &hs_secret)?;

        let (svc, rend_stream): (
            Arc<tor_hsservice::RunningOnionService>,
            std::pin::Pin<
                Box<dyn futures::Stream<Item = tor_hsservice::RendRequest> + Send + Sync>,
            >,
        ) = match self.client.launch_onion_service(config).map_err(|e| {
            CoreError::Transport(TransportErrorKind::Other(format!("HS launch: {e}")))
        })? {
            Some((svc, stream)) => (svc, Box::pin(stream)),
            None => {
                return Err(CoreError::Transport(TransportErrorKind::Other(
                    "HS disabled in config".into(),
                )));
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
            .ok_or_else(|| {
                CoreError::Transport(TransportErrorKind::Other(
                    "HS has no address after launch".into(),
                ))
            })?;

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

    /// Dial an outbound connection to `<onion>:<port>`.
    ///
    /// Returns a `DataStream` implementing `AsyncRead + AsyncWrite`.
    /// Caller is responsible for the Noise handshake (see
    /// `transport::connection::AuthenticatedConnection`).
    pub async fn connect(&self, onion: &str, port: u16) -> Result<arti_client::DataStream> {
        let target = format!("{onion}:{port}");
        self.client.connect(target.as_str()).await.map_err(|e| {
            CoreError::Transport(TransportErrorKind::Other(format!("connect {target}: {e}")))
        })
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

/// Inject a seed-derived HS identity keypair into Arti's primary keystore,
/// using a probe-before-insert pattern to avoid depending on error-message
/// wording.
///
/// If a keypair for `nickname` is already present (the "subsequent run,
/// same state_dir" path), the existing entry is left untouched and we
/// return `Ok(())` immediately — the caller will then launch the service
/// using the stored key, yielding the same `.onion` address as the first run.
///
/// If the slot is empty, the derived keypair is inserted fresh.
fn inject_hs_secret(
    client: &TorClient<TokioRustlsRuntime>,
    nickname: &tor_hsservice::HsNickname,
    secret: &crate::transport::hs_key::HsSecretBytes,
) -> Result<()> {
    let spec = tor_hsservice::HsIdKeypairSpecifier::new(nickname.clone());

    let keymgr = client.keymgr().map_err(|e| {
        CoreError::Transport(TransportErrorKind::Other(format!(
            "keymgr unavailable: {e}"
        )))
    })?;

    // Probe the keymgr BEFORE inserting: if an HS identity keypair for
    // this nickname already exists, launch_onion_service will pick it up
    // and our seed-derived key is intentionally ignored.
    match keymgr.get::<tor_hscrypto::pk::HsIdKeypair>(&spec) {
        Ok(Some(_)) => {
            tracing::debug!(
                "HS key already present in Arti keystore under nickname; \
                 reusing existing entry"
            );
            return Ok(());
        }
        Ok(None) => { /* slot empty — proceed to insert below */ }
        Err(e) => {
            return Err(CoreError::Transport(TransportErrorKind::Other(format!(
                "keymgr get: {e}"
            ))));
        }
    }

    let kp = hs_id_keypair_from_secret(secret);
    keymgr
        .insert(kp, &spec, arti_client::KeystoreSelector::Primary, false)
        .map_err(|e| {
            CoreError::Transport(TransportErrorKind::Other(format!("keymgr insert: {e}")))
        })?;
    Ok(())
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

        // Take the rend_requests and run an accept loop that echoes bytes.
        let rend_requests = rt_a
            .rend_requests_take()
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
}
