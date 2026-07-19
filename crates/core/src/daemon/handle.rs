// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic))]

//! `DaemonHandle` groups the subsystems every command handler needs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::broadcast;

use crate::daemon::commands::{Command as IpcCommand, CommandResult as IpcCommandResult};
use crate::daemon::config::Config;
use crate::daemon::events::{Event, TorStatus};
use crate::daemon::ipc::server::CommandExecutor;
use crate::daemon::ipc::wire::IpcError;
use crate::daemon::logs::LogSink;
use crate::delivery::hub::DeliveryHub;
use crate::identity::IdentityKey;
use crate::storage::Pool;

/// Per-group serialization registry: maps a 32-byte MLS `group_id` to its
/// own lock. The load→encrypt/decrypt→save critical section for a group must
/// hold the group's lock so two concurrent operations can never load the same
/// on-disk ratchet snapshot and encrypt at the same generation (T1-3).
///
/// The registry uses `std::sync::Mutex` (not `tokio::sync::Mutex`) on purpose:
/// the inbound path ([`crate::daemon::inbound::DaemonInbound::dispatch_for_group`])
/// is a synchronous `fn` invoked from inside the per-peer actor's async loop, so
/// it cannot `.await` a lock; the send path's critical section
/// ([`crate::daemon::dispatch::send_message`]) is itself fully synchronous
/// (load → encrypt → `pool.transaction`) with all `.await`s only AFTER it, so a
/// blocking guard scoped to that section never crosses an await point. A single
/// `GroupLockRegistry` Arc is therefore shared verbatim across the send handle
/// and the inbound dispatcher so send + receive serialize on the SAME locks.
pub(crate) type GroupLockRegistry = Arc<Mutex<HashMap<[u8; 32], Arc<Mutex<()>>>>>;

/// Construct an empty [`GroupLockRegistry`].
#[must_use]
pub(crate) fn new_group_lock_registry() -> GroupLockRegistry {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Resolve (creating if absent) the per-group lock for `group_id` in `registry`.
/// The returned `Arc<Mutex<()>>` is then `.lock()`ed by the caller around the
/// load→encrypt/decrypt→save critical section.
#[must_use]
pub(crate) fn group_lock_for(registry: &GroupLockRegistry, group_id: &[u8; 32]) -> Arc<Mutex<()>> {
    // Poisoning is not a correctness concern here: the guarded data is `()`.
    let mut map = match registry.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    map.entry(*group_id)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

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
    /// Live config. Mutators take the write lock and atomic-save on
    /// success. GetConfig readers take the read lock.
    pub config: Arc<tokio::sync::RwLock<Config>>,
    /// Path of the config file on disk. Used by SetConfig's
    /// `save_to_disk` call.
    pub config_path: std::path::PathBuf,
    /// In-memory log ring buffer + live broadcast. Populated by the
    /// `RingBufferLayer` installed in the subscriber stack. The log-tap
    /// task in `Daemon::run` re-emits records onto the event bus so
    /// `EventFilter::Logs` subscribers receive live tail. `TailLogs`
    /// handlers call `snapshot()` directly for paginated snapshots.
    pub log_sink: LogSink,
    /// Per-group ratchet-serialization registry (T1-3). Shared (same Arc)
    /// with the inbound dispatcher via [`DaemonHandle::group_locks`] so send +
    /// receive on one group serialize on the same lock. See
    /// [`GroupLockRegistry`].
    pub(crate) group_locks: GroupLockRegistry,
    /// The inbound MLS dispatcher, shared with the delivery hub and accept
    /// loop. `Some` in production (`run_with_transport`), `None` in handle
    /// unit tests. Used by the RemoveMailbox drain to dispatch held deposits
    /// before finalizing removal.
    pub(crate) inbound: Option<Arc<dyn crate::delivery::peer::InboundDispatch>>,
    /// Precomputed backup key (`HKDF(seed, "skattr-backup-v1")`). Derived at
    /// boot because the root seed is consumed during identity setup and not
    /// retained. Used by `Command::ExportBackup`. `None` until
    /// `set_backup_key` is called; `export_backup_cmd` fails closed
    /// (`InvalidArgument`) if it is still `None` at call time.
    pub(crate) backup_key: Option<zeroize::Zeroizing<[u8; 32]>>,
    /// Wipe-requested signal (T3-3). Set by `wipe_all_data` so the IPC
    /// connection loop can perform the actual teardown AFTER the reply
    /// `Bye` frame is flushed, rather than relying on a fixed sleep.
    /// `Mutex<Option<PathBuf>>` allows `take_wipe_target` to genuinely
    /// consume the signal so a second caller sees `None`.
    pub(crate) wipe_target: Arc<Mutex<Option<PathBuf>>>,
    /// Wake signal for the durable first-contact Welcome sweeper (#93). Shared
    /// (same Arc) with the `welcome_sweep` task spawned in `run_with_transport`.
    /// `add_contact` calls `notify_one()` after committing the `pending_welcomes`
    /// row so the sweeper sends the first Welcome immediately instead of waiting
    /// for its next periodic tick. The sweeper is the SOLE Welcome-delivery path.
    pub(crate) welcome_nudge: Arc<tokio::sync::Notify>,
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
    ///
    /// Config defaults to `Config::defaults()` and the config path
    /// defaults to a non-existent path (tests that call `SetConfig`
    /// should use `new_with_config`).
    #[must_use]
    pub fn new(
        pool: Arc<Pool>,
        hub: Arc<DeliveryHub<S>>,
        identity: IdentityKey,
        events_tx: broadcast::Sender<Event>,
    ) -> Self {
        let config = Config::defaults().unwrap_or_else(|_| Config::fallback_for_tests());
        Self {
            pool,
            hub,
            identity: Arc::new(identity),
            events_tx,
            onion: Arc::new(RwLock::new(None)),
            mailbox_factory: None,
            poller_ctrl: None,
            latest_tor_status: Arc::new(RwLock::new(None)),
            config: Arc::new(tokio::sync::RwLock::new(config)),
            config_path: std::path::PathBuf::from("/dev/null"),
            log_sink: LogSink::default(),
            group_locks: new_group_lock_registry(),
            inbound: None,
            backup_key: None,
            wipe_target: Arc::new(Mutex::new(None)),
            welcome_nudge: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Construct a handle with an explicit config and config path.
    /// Used by tests that exercise `GetConfig` / `SetConfig`, and by
    /// `Daemon::run` (via `new_with_mailbox_and_config`).
    #[must_use]
    pub fn new_with_config(
        pool: Arc<Pool>,
        hub: Arc<DeliveryHub<S>>,
        identity: IdentityKey,
        events_tx: broadcast::Sender<Event>,
        config: Config,
        config_path: std::path::PathBuf,
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
            config: Arc::new(tokio::sync::RwLock::new(config)),
            config_path,
            log_sink: LogSink::default(),
            group_locks: new_group_lock_registry(),
            inbound: None,
            backup_key: None,
            wipe_target: Arc::new(Mutex::new(None)),
            welcome_nudge: Arc::new(tokio::sync::Notify::new()),
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
        let config = Config::defaults().unwrap_or_else(|_| Config::fallback_for_tests());
        Self {
            pool,
            hub,
            identity: Arc::new(identity),
            events_tx,
            onion: Arc::new(RwLock::new(None)),
            mailbox_factory: Some(mailbox_factory),
            poller_ctrl: Some(poller_ctrl),
            latest_tor_status: Arc::new(RwLock::new(None)),
            config: Arc::new(tokio::sync::RwLock::new(config)),
            config_path: std::path::PathBuf::from("/dev/null"),
            log_sink: LogSink::default(),
            group_locks: new_group_lock_registry(),
            inbound: None,
            backup_key: None,
            wipe_target: Arc::new(Mutex::new(None)),
            welcome_nudge: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Replace the `LogSink`. Called by `Daemon::run` after the subscriber
    /// stack is fully initialised so the `RingBufferLayer`'s sink and the
    /// handle's sink are the same allocation.
    pub fn set_log_sink(&mut self, sink: LogSink) {
        self.log_sink = sink;
    }

    /// Replace the config `Arc` and config path. Used by `Daemon::run`
    /// to inject the shared `Arc<RwLock<Config>>` that is also held by
    /// the retention sweep, ensuring `SetConfig` writes propagate to the
    /// sweep on the next tick. Call immediately after `new_with_mailbox`.
    pub(crate) fn set_config_arc(
        &mut self,
        config: Arc<tokio::sync::RwLock<Config>>,
        config_path: std::path::PathBuf,
    ) {
        self.config = config;
        self.config_path = config_path;
    }

    /// Replace the per-group lock registry with a shared one (T1-3). Called by
    /// `Daemon::run` so the send-side handle and the inbound dispatcher lock on
    /// the SAME per-group locks; without this the two would serialize on
    /// separate maps and a concurrent send/receive race would persist.
    pub(crate) fn set_group_locks(&mut self, registry: GroupLockRegistry) {
        self.group_locks = registry;
    }

    /// Inject the shared inbound MLS dispatcher. Called by `Daemon::run` so the
    /// RemoveMailbox drain can dispatch held deposits into local storage before
    /// finalizing removal.
    pub(crate) fn set_inbound(&mut self, inbound: Arc<dyn crate::delivery::peer::InboundDispatch>) {
        self.inbound = Some(inbound);
    }

    /// Store the precomputed backup key. Called by `run_with_transport` after
    /// deriving `HKDF(seed, "skattr-backup-v1")` while the seed is still in
    /// scope. Until this is called the field is `None`; `export_backup_cmd`
    /// fails closed with `InvalidArgument` if the key is absent.
    pub(crate) fn set_backup_key(&mut self, key: zeroize::Zeroizing<[u8; 32]>) {
        self.backup_key = Some(key);
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

    /// Per-group serialization lock (T1-3). The load→encrypt→save critical
    /// section in [`crate::daemon::dispatch::send_message`] (and the card-send
    /// path) MUST hold this so concurrent ops can't encrypt at the same ratchet
    /// generation. The registry is shared (same Arc) with the inbound
    /// dispatcher so send + receive serialize on the same lock.
    #[must_use]
    pub(crate) fn group_lock(&self, group_id: &[u8; 32]) -> Arc<Mutex<()>> {
        group_lock_for(&self.group_locks, group_id)
    }

    /// Clone the shared group-lock registry Arc so the inbound dispatcher can
    /// lock on the SAME per-group locks the send path uses (T1-3). Used by
    /// `Daemon::run` to wire `DaemonInbound`.
    #[must_use]
    pub(crate) fn group_locks(&self) -> GroupLockRegistry {
        self.group_locks.clone()
    }

    /// Signal that a wipe is requested (T3-3). Called by `wipe_all_data`
    /// before returning `Ok`; the IPC connection loop checks this after
    /// flushing the terminal `Bye` frame and performs the actual teardown
    /// there — no blind sleep required.
    ///
    /// Idempotent: if a signal is already set (should never happen in
    /// practice) the second call is silently ignored.
    pub(crate) fn signal_wipe(&self, data_dir: PathBuf) {
        if let Ok(mut guard) = self.wipe_target.lock() {
            if guard.is_none() {
                *guard = Some(data_dir);
            }
        }
    }

    /// Consume the wipe signal. Returns `Some(data_dir)` the first time
    /// after `signal_wipe` was called; `None` on all subsequent calls or
    /// if `signal_wipe` was never called. The signal is genuinely consumed
    /// (`take`n) so a second caller always sees `None`.
    ///
    /// Called by the `CommandExecutor::take_wipe_target` impl, which is
    /// in turn called by `handle_connection` after it writes the `Bye`
    /// frame (T3-3 flush-ordered teardown).
    #[must_use]
    pub(crate) fn take_wipe_target(&self) -> Option<PathBuf> {
        self.wipe_target.lock().ok()?.take()
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

    fn take_wipe_target(&self) -> Option<PathBuf> {
        DaemonHandle::take_wipe_target(self)
    }

    fn wipe_close(&self) {
        if let Err(e) = self.pool.close() {
            tracing::warn!(error = %e, "wipe_close: pool close failed");
        }
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
            config: self.config.clone(),
            config_path: self.config_path.clone(),
            log_sink: self.log_sink.clone(),
            group_locks: self.group_locks.clone(),
            inbound: self.inbound.clone(),
            backup_key: self
                .backup_key
                .as_deref()
                .map(|b| zeroize::Zeroizing::new(*b)),
            wipe_target: self.wipe_target.clone(),
            welcome_nudge: self.welcome_nudge.clone(),
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
