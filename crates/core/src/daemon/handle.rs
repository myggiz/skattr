// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic))]

//! `DaemonHandle` groups the subsystems every command handler needs.

use std::sync::{Arc, RwLock};

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::broadcast;

use crate::daemon::commands::{Command as IpcCommand, CommandResult as IpcCommandResult};
use crate::daemon::events::{Event, TorStatus};
use crate::daemon::ipc::server::CommandExecutor;
use crate::daemon::ipc::wire::IpcError;
use crate::delivery::hub::DeliveryHub;
use crate::identity::IdentityKey;
use crate::storage::Pool;

/// Shared handle to the long-lived daemon subsystems. Generic over the
/// transport stream type so the integration tests can instantiate one
/// over `tokio::io::DuplexStream` and the real daemon over a
/// Tor-anchored listener's stream type.
///
/// `identity` is wrapped in `Arc` so the handle can be cheaply cloned
/// for per-command dispatch without copying secret material.
pub struct DaemonHandle<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Encrypted SQLite pool.
    pub pool: Arc<Pool>,
    /// Per-daemon delivery router.
    pub hub: Arc<DeliveryHub<S>>,
    /// Local Ed25519 identity (used for signing ContactCards + invites).
    pub identity: Arc<IdentityKey>,
    /// Event broadcast sender. Subscribers (IPC connections, tests)
    /// get a `Receiver` via `.subscribe()`.
    pub events_tx: broadcast::Sender<Event>,
    /// Cached onion address, set by `Daemon::run` after Tor publishes.
    /// None until the daemon has finished bootstrapping.
    pub onion: Arc<RwLock<Option<String>>>,
    /// Connection factory for outbound mailbox operations (AddMailbox
    /// probe, RemoveMailbox drain, RotateOnion deposits). `None` in test
    /// helpers that don't exercise the mailbox path. `pub(crate)` —
    /// the trait itself is `pub(crate)` (production wiring lives in
    /// `daemon::run`).
    pub(crate) mailbox_factory: Option<Arc<dyn crate::mailbox::poll::MailboxConnectFactory>>,
    /// Sender side of the `PollScheduler` control channel. `None` in
    /// test helpers. `pub(crate)` — `PollerCtrl` is `pub(crate)`.
    pub(crate) poller_ctrl: Option<tokio::sync::mpsc::Sender<crate::mailbox::poll::PollerCtrl>>,
    /// Snapshot of the latest `TorStatusChanged` event the daemon
    /// emitted. Updated by a tap task spawned in `Daemon::run`. Read
    /// by the IPC server when answering a `Subscribe` ack so the UI
    /// can paint the bootstrap pill on first connect without waiting
    /// for the next live event.
    pub(crate) latest_tor_status: Arc<RwLock<Option<TorStatus>>>,
}

impl<S> DaemonHandle<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Construct a handle from the four owned subsystems.
    ///
    /// Mailbox plumbing (`mailbox_factory`, `poller_ctrl`) is left
    /// `None` — tests that don't exercise the mailbox path use this
    /// constructor; production callers should prefer
    /// [`DaemonHandle::new_with_mailbox`].
    #[must_use]
    pub fn new(
        pool: Arc<Pool>,
        hub: Arc<DeliveryHub<S>>,
        identity: IdentityKey,
        events_tx: broadcast::Sender<Event>,
    ) -> Self {
        Self {
            pool,
            hub,
            identity: Arc::new(identity),
            events_tx,
            onion: Arc::new(RwLock::new(None)),
            mailbox_factory: None,
            poller_ctrl: None,
            latest_tor_status: Arc::new(RwLock::new(None)),
        }
    }

    /// Construct a handle with the mailbox subsystem wired in. Used by
    /// `Daemon::run` so command handlers (`AddMailbox`, `RemoveMailbox`,
    /// `RotateOnion`) can drive outbound probes and notify the
    /// `PollScheduler` of mailbox-table changes.
    ///
    /// `pub(crate)` because `MailboxConnectFactory` and `PollerCtrl`
    /// are themselves `pub(crate)`. Production wiring lives in
    /// `daemon::run`; tests can construct stub factories from inside
    /// the crate.
    #[must_use]
    pub(crate) fn new_with_mailbox(
        pool: Arc<Pool>,
        hub: Arc<DeliveryHub<S>>,
        identity: IdentityKey,
        events_tx: broadcast::Sender<Event>,
        mailbox_factory: Arc<dyn crate::mailbox::poll::MailboxConnectFactory>,
        poller_ctrl: tokio::sync::mpsc::Sender<crate::mailbox::poll::PollerCtrl>,
    ) -> Self {
        Self {
            pool,
            hub,
            identity: Arc::new(identity),
            events_tx,
            onion: Arc::new(RwLock::new(None)),
            mailbox_factory: Some(mailbox_factory),
            poller_ctrl: Some(poller_ctrl),
            latest_tor_status: Arc::new(RwLock::new(None)),
        }
    }

    /// Snapshot the latest cached `TorStatus`. Non-blocking RwLock read.
    #[must_use]
    pub fn latest_tor_status(&self) -> Option<TorStatus> {
        self.latest_tor_status
            .read()
            .ok()
            .and_then(|guard| guard.clone())
    }

    /// Replace the cached `TorStatus`. Called by the tap task spawned
    /// in `Daemon::run`. Tests may call directly.
    pub fn set_tor_status(&self, status: TorStatus) {
        if let Ok(mut guard) = self.latest_tor_status.write() {
            *guard = Some(status);
        }
    }

    /// Cache the published onion address. Called by `Daemon::run` after
    /// Tor finishes bootstrapping.
    pub fn set_onion(&self, addr: impl Into<String>) {
        if let Ok(mut guard) = self.onion.write() {
            *guard = Some(addr.into());
        }
    }

    /// Read the cached onion address. Returns `None` if Tor has not yet
    /// published (daemon still bootstrapping).
    #[must_use]
    pub fn onion(&self) -> Option<String> {
        self.onion.read().ok().and_then(|g| g.clone())
    }
}

#[async_trait::async_trait]
impl<S> CommandExecutor for DaemonHandle<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
    async fn execute(&self, cmd: IpcCommand) -> std::result::Result<IpcCommandResult, IpcError> {
        // CommandExecutor takes `&self`; dispatch::execute_command
        // takes Arc<DaemonHandle>. Build a fresh Arc by cloning the
        // subsystem handles (all Arc / Clone).
        let arc = Arc::new(self.clone_for_dispatch());
        crate::daemon::dispatch::execute_command(arc, cmd).await
    }

    fn latest_tor_status(&self) -> Option<crate::daemon::events::TorStatus> {
        DaemonHandle::latest_tor_status(self)
    }
}

impl<S> DaemonHandle<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    pub(crate) fn clone_for_dispatch(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            hub: self.hub.clone(),
            identity: self.identity.clone(),
            events_tx: self.events_tx.clone(),
            onion: self.onion.clone(),
            mailbox_factory: self.mailbox_factory.clone(),
            poller_ctrl: self.poller_ctrl.clone(),
            latest_tor_status: self.latest_tor_status.clone(),
        }
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

#[cfg(test)]
mod tor_status_cache_tests {
    use super::*;
    use crate::daemon::events::TorStatus;

    fn fake_handle() -> DaemonHandle<tokio::io::DuplexStream> {
        let (events_tx, _) = tokio::sync::broadcast::channel(16);
        let pool = Arc::new(Pool::in_memory());
        let identity = {
            let seed = crate::identity::Seed::generate().unwrap();
            crate::identity::IdentityKey::from_seed(&seed).unwrap()
        };
        let hub = Arc::new(crate::delivery::hub::DeliveryHub::new(pool.clone()));
        DaemonHandle::new(pool, hub, identity, events_tx)
    }

    #[tokio::test]
    async fn latest_tor_status_starts_none() {
        let h = fake_handle();
        assert!(h.latest_tor_status().is_none());
    }

    #[tokio::test]
    async fn set_tor_status_round_trips() {
        let h = fake_handle();
        h.set_tor_status(TorStatus::Bootstrapping(42));
        assert_eq!(h.latest_tor_status(), Some(TorStatus::Bootstrapping(42)),);
        h.set_tor_status(TorStatus::Ready);
        assert_eq!(h.latest_tor_status(), Some(TorStatus::Ready));
    }
}
