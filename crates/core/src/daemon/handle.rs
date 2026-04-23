// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic))]

//! `DaemonHandle` groups the subsystems every command handler needs.

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::broadcast;

use crate::daemon::events::Event;
use crate::delivery::hub::DeliveryHub;
use crate::identity::IdentityKey;
use crate::storage::Pool;

/// Shared handle to the long-lived daemon subsystems. Generic over the
/// transport stream type so the integration tests can instantiate one
/// over `tokio::io::DuplexStream` and the real daemon over a
/// Tor-anchored listener's stream type.
// wired up by Task 13 dispatch
#[allow(dead_code)]
pub struct DaemonHandle<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Encrypted SQLite pool.
    pub pool: Arc<Pool>,
    /// Per-daemon delivery router.
    pub hub: Arc<DeliveryHub<S>>,
    /// Local Ed25519 identity (used for signing ContactCards + invites).
    pub identity: IdentityKey,
    /// Event broadcast sender. Subscribers (IPC connections, tests)
    /// get a `Receiver` via `.subscribe()`.
    pub events_tx: broadcast::Sender<Event>,
}

impl<S> DaemonHandle<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Construct a handle from the four owned subsystems.
    #[must_use]
    pub fn new(
        pool: Arc<Pool>,
        hub: Arc<DeliveryHub<S>>,
        identity: IdentityKey,
        events_tx: broadcast::Sender<Event>,
    ) -> Self {
        Self { pool, hub, identity, events_tx }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Seed;

    #[tokio::test]
    async fn constructs_with_mock_subsystems() {
        let seed = Seed::generate().unwrap();
        let identity = IdentityKey::from_seed(&seed).unwrap();
        let pool = Arc::new(Pool::in_memory());
        let hub: Arc<DeliveryHub<tokio::io::DuplexStream>> =
            Arc::new(DeliveryHub::new(pool.clone()));
        let (events_tx, _) = broadcast::channel::<Event>(16);

        let handle = DaemonHandle::<tokio::io::DuplexStream>::new(
            pool.clone(),
            hub.clone(),
            identity,
            events_tx.clone(),
        );

        assert!(Arc::ptr_eq(&handle.pool, &pool));
        assert!(Arc::ptr_eq(&handle.hub, &hub));
    }
}
