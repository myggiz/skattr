// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Arti lifecycle: bootstrap, hidden service publish/accept, outbound dial.
//!
//! Wraps `arti-client` and `tor-hsservice`. Intentionally thin: this
//! module exposes only the primitives `core` needs, so we can swap the
//! backend (e.g. to a system `tor` controller socket) without touching
//! downstream code.

use std::path::PathBuf;

use tokio::sync::watch;

use crate::error::Result;

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
    _status: watch::Sender<TorStatus>,
}

impl TorRuntime {
    /// Boot Arti with the given config. Returns when bootstrap completes
    /// or fails; use [`TorRuntime::status`] for progress updates.
    pub async fn bootstrap(_config: TorConfig) -> Result<Self> {
        todo!("construct arti_client::TorClient, bootstrap, wire status channel")
    }

    /// Observe bootstrap / runtime state.
    pub fn status(&self) -> watch::Receiver<TorStatus> {
        self._status.subscribe()
    }

    /// Publish a v3 onion service at the given key path. Returns the
    /// onion address once the descriptor is published.
    pub async fn publish_onion(&self, _hs_key_path: PathBuf) -> Result<String> {
        todo!("tor_hsservice::RunningOnionService")
    }

    /// Dial an outbound connection to `<onion>:<port>`.
    ///
    /// Returns an `AsyncRead + AsyncWrite` stream.
    pub async fn connect(
        &self,
        _onion: &str,
        _port: u16,
    ) -> Result<Box<dyn tokio::io::AsyncRead + Send + Unpin>> {
        todo!("arti client DataStream")
    }

    /// Gracefully shut down Arti: drain connections, flush, stop.
    pub async fn shutdown(self) -> Result<()> {
        todo!("stop onion service, shut down arti runtime")
    }
}
