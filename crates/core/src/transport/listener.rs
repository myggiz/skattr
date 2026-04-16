// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Accepts onion-service connections, runs Noise_XK, hands authenticated
//! connections to the session manager.

use tokio::sync::mpsc;

use crate::error::Result;
use crate::transport::connection::AuthenticatedConnection;

/// Background task that owns the inbound half of an onion service.
///
/// Each accepted stream is pushed through Noise_XK (responder side). On
/// success the resulting [`AuthenticatedConnection`] is forwarded over
/// the `accepted` channel; on failure the stream is dropped and a
/// `tracing::warn!` is emitted (no connection details logged).
pub struct OnionListener {
    /// Channel that yields new authenticated connections.
    pub accepted: mpsc::Receiver<AuthenticatedConnection>,
}

impl OnionListener {
    /// Spawn the listener. Returns the receiver side; the task runs
    /// until the sending half is dropped or [`OnionListener::shutdown`]
    /// is called.
    pub async fn spawn() -> Result<Self> {
        todo!("subscribe to Arti onion stream events, spawn Noise responder per stream")
    }

    /// Stop accepting new connections. Existing connections are not
    /// affected.
    pub async fn shutdown(self) -> Result<()> {
        todo!("signal background task to stop")
    }
}
