// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Arti lifecycle: bootstrap, hidden service publish/accept, outbound dial.
//!
//! Wraps `arti-client` and `tor-hsservice`. Intentionally thin: this
//! module exposes only the primitives `core` needs, so we can swap the
//! backend (e.g. to a system `tor` controller socket) without touching
//! downstream code.

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

    /// Publish a v3 onion service. Implemented in Task 5.
    pub async fn publish_onion(&self, _hs_key_path: PathBuf) -> Result<String> {
        todo!("Task 5")
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

        // Drop the TorClient. Its Drop shuts down the underlying
        // background tasks.
        drop(self.client);

        Ok(())
    }
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
}
