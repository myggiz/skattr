// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Daemon struct: owns all long-lived handles.

use std::path::Path;
use std::sync::Arc;

use tokio::sync::{broadcast, oneshot};

use crate::daemon::commands::{Command, CommandResult};
use crate::daemon::config::Config;
use crate::daemon::events::Event;
use crate::daemon::logs::LogSink;
use crate::error::Result;
use crate::identity::derive::derive_storage_seed;
use crate::identity::vault::Vault;
use crate::transport::tor::{TorConfig, TorRuntime};

/// Capacity of the daemon's broadcast event channel.
///
/// Slow subscribers that lag past this count will miss events (by
/// design — the daemon must not back-pressure). UI consumers should
/// reconcile via a full `refresh` command on resubscribe.
const EVENT_CHANNEL_CAPACITY: usize = 1024;

/// Published readiness of the daemon: onion address + bound IPC path.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Ready {
    /// Full v3 onion address, without port suffix.
    pub onion: String,
    /// Path of the Unix socket the daemon is listening on.
    pub ipc_socket: std::path::PathBuf,
}

/// The running daemon.
pub struct Daemon {
    events_tx: broadcast::Sender<Event>,
}

impl Daemon {
    /// Start the daemon: unlock vault, bootstrap Tor, open storage,
    /// publish onion, spawn sender/receiver/poller tasks.
    pub async fn start(_config: Config, _passphrase: &str) -> Result<Self> {
        let (events_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        // TODO(phase-0): open Vault, TorRuntime::bootstrap, Pool::open,
        //                spawn OnionListener + Sender + MailboxScheduler.
        Ok(Self { events_tx })
    }

    /// Execute a command.
    pub async fn execute(&self, _cmd: Command) -> Result<CommandResult> {
        todo!("match command, dispatch to subsystem, return CommandResult")
    }

    /// Subscribe to the daemon's event stream.
    #[must_use]
    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.events_tx.subscribe()
    }

    /// Graceful shutdown: cancel tasks, flush outbox, stop Tor.
    pub async fn shutdown(self) -> Result<()> {
        todo!("cancel tokens, await tasks, flush storage, stop Arti")
    }
}

impl Daemon {
    /// Run the full daemon lifecycle:
    ///
    /// 1. Unlock the vault and derive the storage seed.
    /// 2. Open `Pool` (SQLite + age encryption) and run migrations.
    /// 3. Bootstrap Tor and publish the onion service.
    /// 4. Construct the `DeliveryHub` with `DaemonInbound` MLS dispatch.
    /// 5. Bind the IPC Unix socket and start the `serve` loop.
    /// 6. Signal readiness via the [`Ready`] struct (`onion` + `ipc_socket`).
    /// 7. Await the caller-supplied shutdown future.
    /// 8. Tear down: IPC server → Tor runtime → socket file (via Drop).
    ///
    /// Returns `Ok(())` after a graceful shutdown.
    pub async fn run(
        data_dir: &Path,
        passphrase: &zeroize::Zeroizing<String>,
        config: Config,
        config_path: std::path::PathBuf,
        ready: oneshot::Sender<Ready>,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        Self::run_with_sink(
            data_dir,
            passphrase,
            config,
            config_path,
            ready,
            shutdown,
            None,
        )
        .await
    }

    /// Like [`Daemon::run`] but accepts a pre-constructed `LogSink` that is
    /// already installed in the subscriber stack (via `RingBufferLayer`). If
    /// `None`, a standalone sink is created (no tracing events flow into it).
    pub async fn run_with_sink(
        data_dir: &Path,
        passphrase: &zeroize::Zeroizing<String>,
        config: Config,
        config_path: std::path::PathBuf,
        ready: oneshot::Sender<Ready>,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
        log_sink: Option<LogSink>,
    ) -> Result<()> {
        use crate::storage::Pool;

        std::fs::create_dir_all(data_dir)?;

        // Single-daemon guard: hold an OS advisory lock on <data_dir>/daemon.lock
        // for the whole run. A second daemon on the same data dir fails fast
        // HERE — before the Pool is opened (`:136`) or Arti touches `arti/`/`hss`
        // (`:187`) — rather than corrupting shared SQLite / Tor state or
        // double-publishing the onion. The handle lives in `_daemon_lock` until
        // this function returns; the OS also releases it on process death
        // (incl. SIGKILL), so a hard kill leaves a cleanly re-lockable state
        // with no stale-lock reclaim needed.
        let _daemon_lock = match crate::daemon::lock::acquire(data_dir) {
            Ok(g) => g,
            Err(crate::daemon::lock::LockError::AlreadyRunning) => {
                return Err(crate::error::CoreError::DaemonAlreadyRunning);
            }
            Err(crate::daemon::lock::LockError::Io(e)) => {
                return Err(crate::error::CoreError::Io(e));
            }
        };

        // Belt-and-suspenders: the authoritative on-disk paths (vault/Pool/
        // arti/hs) already use the `data_dir` parameter, but a migrated
        // config.toml could carry a stale absolute `data_dir`. Force the
        // in-memory config to agree so config-derived paths (downloads,
        // <data_dir>/skattr.log) can't be re-pointed at the old location.
        let mut config = config;
        config.data_dir = data_dir.to_path_buf();

        // Step 1: unlock vault → identity → derive storage seed.
        // `derive_storage_seed` consumes the identity key, so we open
        // the vault a second time to get a fresh copy for DaemonHandle.
        // The mailbox subsystem also needs an owned `IdentityKey` for
        // the `PollScheduler`'s per-actor signing path; we open the
        // vault a third time so the same passphrase produces three
        // independent zeroize-on-drop handles.
        let vault_path = data_dir.join("identity.vault");
        let (_vault, identity_for_seed) = Vault::open(&vault_path, passphrase.as_str())?;
        let seed = derive_storage_seed(identity_for_seed)?;
        let backup_key = crate::identity::derive::hkdf_expand::<32>(
            seed.as_bytes(),
            crate::identity::derive::INFO_BACKUP_V1,
        )?;
        let (_vault2, identity) = Vault::open(&vault_path, passphrase.as_str())?;
        let (_vault3, identity_for_poller) = Vault::open(&vault_path, passphrase.as_str())?;
        let (_vault4, identity_for_inbound) = Vault::open(&vault_path, passphrase.as_str())?;

        // Step 2: open Pool (migrations are applied inside Pool::open).
        let pool = Arc::new(Pool::open(data_dir, &seed)?);

        // Phase 1.G: one-shot backfill for legacy text rows (idempotent).
        match crate::storage::MessageRepo::new(&pool).backfill_body_text() {
            Ok(0) => {}
            Ok(n) => tracing::info!(rows = n, "backfilled body_text for legacy rows"),
            Err(e) => {
                tracing::warn!(error = %e, "body_text backfill failed; FTS may be incomplete")
            }
        }

        // Phase 1.H: one-shot backfill for pre-1.H rows missing envelope_id.
        // Fail-closed: envelope_id backfill underpins (group_id, envelope_id)
        // uniqueness for pre-1.H rows. If this errors, continuing would leave
        // the uniqueness guarantee "advisory" for legacy rows (NULLs compare
        // distinct in SQLite's UNIQUE index). Contrast with backfill_body_text,
        // which is FTS-only and safe to warn-and-skip.
        let n = crate::storage::MessageRepo::new(&pool).backfill_envelope_id()?;
        if n > 0 {
            tracing::info!(rows = n, "backfilled envelope_id for pre-1.H rows");
        }

        // Wrap config in Arc<RwLock> for live mutation (GetConfig/SetConfig) and
        // sweep re-reads.
        let config_arc = std::sync::Arc::new(tokio::sync::RwLock::new(config.clone()));

        // Phase 1.G: hourly retention sweep.
        let (sweep_shutdown_tx, sweep_shutdown_rx) = tokio::sync::watch::channel(false);
        let sweep_handle = crate::daemon::retention::spawn_sweep(
            pool.clone(),
            config_arc.clone(),
            std::time::Duration::from_secs(3600),
            sweep_shutdown_rx,
        );

        // Step 3: event broadcast channel.
        let (events_tx, _) = broadcast::channel::<Event>(EVENT_CHANNEL_CAPACITY);

        // Step 4: Tor bootstrap. Build the mailbox factory from a cloned
        // TorClient BEFORE the runtime is moved into the ArtiTransport
        // (the transport's `shutdown` consumes the runtime).
        let cfg = TorConfig {
            state_dir: data_dir.join("arti"),
            socks_port: None,
            // Single-user desktop deployment: relax Arti's ancestor-permission
            // check so a group/other-writable home or `/tmp` ancestor doesn't
            // block startup. We own data_dir at 0700 and the DB is encrypted at
            // rest, so the check is redundant here. Not enabled by default for
            // other (e.g. test / multi-user) callers of TorConfig.
            trust_dir_permissions: true,
        };
        let rt = TorRuntime::bootstrap(cfg).await?;
        let mailbox_factory: Arc<dyn crate::mailbox::poll::MailboxConnectFactory> =
            Arc::new(ArtiMailboxFactory {
                tor_client: rt.client().clone(),
            });
        let transport = Arc::new(crate::transport::arti_transport::ArtiTransport::new(rt));

        // One identity shared (by Arc) across the dialer (initiator
        // handshakes) and the accept loop (responder handshakes); both only
        // borrow `&IdentityKey`. IdentityKey is not Clone, so we open the
        // vault a fifth time to mint an independent zeroize-on-drop handle.
        let (_vault5, identity_for_transport) = Vault::open(&vault_path, passphrase.as_str())?;
        let transport_identity = Arc::new(identity_for_transport);

        // The LogSink is resolved here so `run_with_transport` receives an
        // owned, already-installed sink.
        let resolved_sink = log_sink.unwrap_or_default();

        run_with_transport(
            transport,
            pool,
            identity,
            identity_for_poller,
            identity_for_inbound,
            transport_identity,
            seed,
            backup_key,
            data_dir,
            config,
            config_path,
            config_arc,
            events_tx,
            mailbox_factory,
            crate::mailbox::poll::PollCadence::default(),
            resolved_sink,
            sweep_shutdown_tx,
            sweep_handle,
            ready,
            shutdown,
        )
        .await
    }

    /// Encrypt an envelope for `peer` via the existing MLS group, persist
    /// it to the outbox, and hand it off to the delivery hub for
    /// transmission. `pub(crate)` until 1.F wires the CLI path; tests
    /// reach this via `test_exports::send`.
    #[allow(dead_code)]
    pub(crate) async fn send(
        &self,
        _peer: crate::identity::PublicKey,
        _envelope: crate::envelope::Envelope,
    ) -> crate::error::Result<tokio::sync::oneshot::Receiver<std::result::Result<(), ()>>> {
        // 1.E is scaffold-complete but Daemon::run does not yet
        // construct the hub (that wiring lands with 1.F when
        // Daemon::execute grows Command::Send). Returning an error
        // here keeps the signature stable for tests; the integration
        // test in crates/tests/src/delivery_kill_mid_message.rs
        // bypasses Daemon and drives DeliveryHub directly through
        // test_exports.
        Err(crate::error::CoreError::Delivery(
            crate::delivery::DeliveryErrorKind::Other(
                "Daemon::send requires 1.F CLI integration".into(),
            ),
        ))
    }
}

/// Generic post-bootstrap daemon assembly: publish the onion via the
/// injected [`Transport`](crate::transport::Transport), spawn the inbound
/// accept loop, inject the on-demand dialer into the [`DeliveryHub`], wire
/// the [`PollScheduler`](crate::mailbox::poll::PollScheduler), build the
/// [`DaemonHandle`], bind IPC, and await shutdown.
///
/// `Daemon::run` / `run_with_sink` are thin `ArtiTransport` wrappers around
/// this function; the two-daemon loopback guardrail (Task 8) calls it with
/// `LoopbackTransport`. Exported under `test-harness` via
/// [`crate::test_exports::run_with_transport`].
// `pub` so `lib.rs::test_exports` can re-export it for the two-daemon guardrail
// (Task 8) — a `pub use` cannot re-export a `pub(crate)` item (E0364), so this
// must be `pub`, mirroring the `pub struct Ready` pattern in this module.
// `daemon::state` is a `pub mod`, but the function takes a `pub(crate)`
// `Arc<dyn MailboxConnectFactory>` that external callers cannot construct, so
// it is uncallable from outside the crate. `private_interfaces` flags exactly
// that inert leak.
#[allow(clippy::too_many_arguments, private_interfaces)]
pub async fn run_with_transport<T>(
    transport: Arc<T>,
    pool: Arc<crate::storage::Pool>,
    identity: crate::identity::IdentityKey,
    identity_for_poller: crate::identity::IdentityKey,
    identity_for_inbound: crate::identity::IdentityKey,
    transport_identity: Arc<crate::identity::IdentityKey>,
    seed: crate::identity::Seed,
    backup_key: zeroize::Zeroizing<[u8; 32]>,
    data_dir: &Path,
    config: Config,
    config_path: std::path::PathBuf,
    config_arc: Arc<tokio::sync::RwLock<Config>>,
    events_tx: broadcast::Sender<Event>,
    mailbox_factory: Arc<dyn crate::mailbox::poll::MailboxConnectFactory>,
    poll_cadence: crate::mailbox::poll::PollCadence,
    log_sink: LogSink,
    sweep_shutdown_tx: tokio::sync::watch::Sender<bool>,
    sweep_handle: tokio::task::JoinHandle<()>,
    ready: oneshot::Sender<Ready>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()>
where
    T: crate::transport::Transport,
    // `DaemonHandle<S>: CommandExecutor` requires `S: Sync` (the IPC executor
    // is shared across threads as `Arc<dyn CommandExecutor>`). Every concrete
    // `Transport::Stream` (arti `DataStream`, loopback duplex) is `Sync`; the
    // associated-type projection just doesn't carry it, so restate it here.
    T::Stream: Sync,
{
    use crate::daemon::handle::DaemonHandle;
    use crate::daemon::inbound::DaemonInbound;
    use crate::daemon::ipc::server::{serve, Server};
    use crate::delivery::hub::DeliveryHub;
    use crate::delivery::peer::InboundDispatch;

    // Step 1: publish onion + obtain the inbound stream source. `data_dir` is
    // only borrowed here, before the `shutdown.await` boundary, so the `&Path`
    // need not outlive the `'static` shutdown future.
    let hs_key_path = data_dir.join("hs.key.age");
    let (onion, inbound_streams) = transport
        .publish(&hs_key_path, &seed, "skattr-daemon")
        .await?;

    // Step 2: DaemonInbound (MLS decrypt + persist + emit events).
    //
    // Per-group ratchet serialization (T1-3): create ONE shared lock registry
    // here and give it to both the inbound dispatcher and (below) the send-side
    // DaemonHandle, so a concurrent send + receive on the same group serialize
    // on the same lock.
    let group_locks = crate::daemon::handle::new_group_lock_registry();
    let mut inbound_impl = DaemonInbound::new(pool.clone(), events_tx.clone());
    inbound_impl.set_identity(Arc::new(identity_for_inbound));
    inbound_impl.set_group_locks(group_locks.clone());
    let chunk_store = std::sync::Arc::new(crate::attachment::store::ChunkStore::new(data_dir));
    // Clear any decrypted plaintext left in the open-cache from a previous run
    // (covers abnormal exits where the shutdown wipe never ran).
    wipe_open_cache(data_dir);
    inbound_impl.set_chunk_store(chunk_store.clone());
    inbound_impl.set_download_dir(config.resolved_download_dir());
    let inbound = Arc::new(inbound_impl) as Arc<dyn InboundDispatch>;

    // Step 3: DeliveryHub with the injected on-demand dialer. The dialer and
    // the accept loop share `transport_identity` (both only borrow it).
    let dialer = Arc::new(crate::delivery::dial::TransportDial::new(
        transport.clone(),
        transport_identity.clone(),
        pool.clone(),
    ));
    let fallback_shared = std::sync::Arc::new(crate::delivery::hub::MailboxFallbackShared {
        factory: mailbox_factory.clone(),
        events: events_tx.clone(),
        identity: transport_identity.clone(),
    });
    let hub: Arc<DeliveryHub<T::Stream>> =
        Arc::new(DeliveryHub::new_with_inbound_dialer_and_fallback(
            pool.clone(),
            inbound.clone(),
            dialer,
            fallback_shared.clone(),
            std::time::Duration::from_secs(u64::from(config.delivery.direct_timeout_secs)),
            data_dir,
            config.resolved_download_dir(),
        ));

    // Step 4: inbound accept loop — handshake (responder) + resolve + ingest.
    let accept_task = tokio::spawn(crate::daemon::accept::run_accept_loop(
        inbound_streams,
        transport_identity.clone(),
        pool.clone(),
        hub.clone(),
        inbound.clone(),
    ));

    // Step 5: PollScheduler. Held for the daemon's lifetime — its `Drop`
    // aborts the supervisor on shutdown.
    let identity_arc = Arc::new(identity_for_poller);
    let scheduler = crate::mailbox::poll::PollScheduler::spawn(
        pool.clone(),
        identity_arc,
        events_tx.clone(),
        mailbox_factory.clone(),
        Some(inbound.clone()),
        poll_cadence,
    );
    let poller_ctrl = scheduler.ctrl();

    // Mailbox-outbox sweeper: the single deposit/retry engine for offline
    // delivery — re-deposits due mailbox-kind outbox rows with failover+backoff.
    let sweeper_pool = pool.clone();
    let sweeper_shared = fallback_shared.clone();
    let mailbox_sweeper_task = tokio::spawn(async move {
        const SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(15);
        let mut t = tokio::time::interval(SWEEP_EVERY);
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            t.tick().await;
            let now = crate::daemon::clock::now_unix_millis();
            crate::delivery::mailbox_sweeper::run_mailbox_sweep(
                &sweeper_pool,
                &sweeper_shared,
                now,
                32,
            )
            .await;
        }
    });

    let chunk_sweep_pool = pool.clone();
    let chunk_sweep_shared = fallback_shared.clone();
    let chunk_sweep_store = chunk_store.clone();
    let chunk_sweep_task = tokio::spawn(async move {
        const SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(15);
        let mut t = tokio::time::interval(SWEEP_EVERY);
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            t.tick().await;
            let now = crate::daemon::clock::now_unix_millis();
            crate::delivery::chunk_sweep::run_chunk_sweep(
                &chunk_sweep_pool,
                &chunk_sweep_shared,
                &chunk_sweep_store,
                now,
                crate::attachment::CHUNK_SWEEP_BATCH,
            )
            .await;
        }
    });

    // Step 6: DaemonHandle — inject the shared config_arc so SetConfig writes
    // propagate to the retention sweep on the next tick.
    let mut handle = DaemonHandle::<T::Stream>::new_with_mailbox(
        pool.clone(),
        hub,
        identity,
        events_tx.clone(),
        mailbox_factory,
        poller_ctrl,
    );
    handle.set_config_arc(config_arc, config_path);
    handle.set_onion(onion.clone());
    handle.set_log_sink(log_sink.clone());
    // Share the SAME group-lock registry with the inbound dispatcher (T1-3) so
    // a send and a receive on one group serialize on the same lock.
    handle.set_group_locks(group_locks);
    // Give the handle the shared inbound dispatcher so the RemoveMailbox drain
    // can dispatch held deposits into local storage before finalizing removal
    // (Task 22.5).
    handle.set_inbound(inbound.clone());
    // Store the precomputed backup key (derived at boot while the seed was still
    // in scope). Command::ExportBackup reads this at runtime without needing the
    // root seed (which has already been consumed by derive_storage_seed).
    handle.set_backup_key(backup_key);

    // Log tap: forward every record from the ring buffer's broadcast channel
    // onto the daemon event bus so `EventFilter::Logs` subscribers receive
    // live tail. Terminates when the broadcast sender is dropped (process exit).
    {
        let mut log_rx = log_sink.subscribe();
        let log_events_tx = events_tx.clone();
        tokio::spawn(async move {
            loop {
                match log_rx.recv().await {
                    Ok(record) => {
                        let _ = log_events_tx.send(crate::daemon::events::Event::LogRecord(record));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    // TorStatus tap: copy every TorStatusChanged into the same Arc<RwLock<…>>
    // the IpcServer reads via DaemonHandle::latest_tor_status(). Held on a
    // JoinHandle so the shutdown path can abort it.
    let tor_status_cache_for_tap = handle.latest_tor_status.clone();
    let mut tap_rx = events_tx.subscribe();
    let tor_tap_task = tokio::spawn(async move {
        loop {
            match tap_rx.recv().await {
                Ok(crate::daemon::events::Event::TorStatusChanged(s)) => {
                    if let Ok(mut g) = tor_status_cache_for_tap.write() {
                        *g = Some(s);
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Step 7: IPC server.
    let sock_path = config.ipc_socket_or_default()?;
    let allowed = crate::daemon::ipc::current_peer_id();
    let ipc_server = Server::bind(&sock_path, allowed)?;
    let sock_path_copy = sock_path.clone();

    let (ipc_shutdown_tx, ipc_shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let executor: Arc<dyn crate::daemon::ipc::server::CommandExecutor> =
        Arc::new(handle.clone_for_dispatch());
    let ipc_events = events_tx.clone();
    let ipc_task = tokio::spawn(async move {
        serve(ipc_server, executor, ipc_events, async move {
            let _ = ipc_shutdown_rx.await;
        })
        .await;
    });

    // Step 8: signal readiness.
    let _ = ready.send(Ready {
        onion,
        ipc_socket: sock_path_copy.clone(),
    });

    // Step 9: await shutdown.
    shutdown.await;

    // Step 10: tear down.
    let _ = ipc_shutdown_tx.send(());
    let _ = ipc_task.await;
    tor_tap_task.abort();
    let _ = tor_tap_task.await;
    let _ = sweep_shutdown_tx.send(true);
    let _ = sweep_handle.await;
    // Explicitly drop the PollScheduler so its supervisor task is aborted
    // before the transport shuts down — a poll actor mid-Challenge would
    // otherwise observe a torn-down circuit.
    drop(scheduler);
    // Stop the accept loop and join it (parity with the other teardown awaits)
    // before shutting down the transport (which consumes the underlying runtime
    // for ArtiTransport), so an in-flight accept can't race the teardown.
    accept_task.abort();
    let _ = accept_task.await;
    // Stop the mailbox-outbox sweeper before tearing down the transport, for
    // parity with the accept loop (an in-flight deposit must not race teardown).
    mailbox_sweeper_task.abort();
    let _ = mailbox_sweeper_task.await;
    // Stop the chunk-sweep task with the same teardown parity.
    chunk_sweep_task.abort();
    let _ = chunk_sweep_task.await;
    transport.shutdown().await?;
    // Server::drop removes the socket file automatically.

    // At-rest encryption (2.B): close the pool deterministically through the
    // retained Arc. `close` takes `&self`, so sole ownership is NOT required —
    // background tasks still holding a clone observe a closed pool and their
    // own Drop no-ops (close is idempotent). This runs BEFORE the function
    // returns, so a clean shutdown leaves no plaintext on disk.
    if let Err(e) = pool.close() {
        tracing::warn!(error = %e, "pool close at shutdown failed");
    }
    // Remove decrypted plaintext so a clean shutdown leaves none on disk.
    wipe_open_cache(data_dir);
    Ok(())
}

/// Loopback twin of [`Daemon::run_with_sink`] for the two-daemon guardrail
/// (Task 8). Mirrors `run_with_sink` exactly except it (a) publishes over a
/// [`LoopbackTransport`](crate::transport::LoopbackTransport) on the shared
/// `net` instead of bootstrapping Tor, (b) supplies a no-op
/// [`MailboxConnectFactory`](crate::mailbox::poll::MailboxConnectFactory)
/// (no mailboxes are configured, so it is never called), and (c) has no
/// `TorRuntime`. Everything else is the production
/// [`run_with_transport`] assembly, so this exercises the real wiring.
///
/// Exported under `test-harness` via [`crate::test_exports::run_loopback`].
#[cfg(feature = "test-harness")]
#[allow(clippy::too_many_arguments)]
pub async fn run_loopback(
    data_dir: &Path,
    passphrase: &zeroize::Zeroizing<String>,
    config: Config,
    config_path: std::path::PathBuf,
    net: crate::transport::LoopbackNet,
    my_onion: String,
    ready: oneshot::Sender<Ready>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()> {
    use crate::storage::Pool;

    std::fs::create_dir_all(data_dir)?;

    // Step 1: unlock vault → identity → derive storage seed. Mirrors
    // `run_with_sink`: open the vault five times for five independent
    // zeroize-on-drop handles.
    let vault_path = data_dir.join("identity.vault");
    let (_vault, identity_for_seed) = Vault::open(&vault_path, passphrase.as_str())?;
    let seed = derive_storage_seed(identity_for_seed)?;
    let backup_key = crate::identity::derive::hkdf_expand::<32>(
        seed.as_bytes(),
        crate::identity::derive::INFO_BACKUP_V1,
    )?;
    let (_vault2, identity) = Vault::open(&vault_path, passphrase.as_str())?;
    let (_vault3, identity_for_poller) = Vault::open(&vault_path, passphrase.as_str())?;
    let (_vault4, identity_for_inbound) = Vault::open(&vault_path, passphrase.as_str())?;
    let (_vault5, identity_for_transport) = Vault::open(&vault_path, passphrase.as_str())?;

    // Step 2: open Pool (migrations applied inside Pool::open).
    let pool = Arc::new(Pool::open(data_dir, &seed)?);

    match crate::storage::MessageRepo::new(&pool).backfill_body_text() {
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "body_text backfill failed"),
    }
    let n = crate::storage::MessageRepo::new(&pool).backfill_envelope_id()?;
    if n > 0 {
        tracing::info!(rows = n, "backfilled envelope_id for pre-1.H rows");
    }

    let config_arc = std::sync::Arc::new(tokio::sync::RwLock::new(config.clone()));

    let (sweep_shutdown_tx, sweep_shutdown_rx) = tokio::sync::watch::channel(false);
    let sweep_handle = crate::daemon::retention::spawn_sweep(
        pool.clone(),
        config_arc.clone(),
        std::time::Duration::from_secs(3600),
        sweep_shutdown_rx,
    );

    let (events_tx, _) = broadcast::channel::<Event>(EVENT_CHANNEL_CAPACITY);

    // Step 3: loopback transport + no-op mailbox factory (in place of Tor).
    let transport = Arc::new(crate::transport::LoopbackTransport::new(net, my_onion));
    let mailbox_factory: Arc<dyn crate::mailbox::poll::MailboxConnectFactory> =
        Arc::new(NoopMailboxFactory);
    let transport_identity = Arc::new(identity_for_transport);

    run_with_transport(
        transport,
        pool,
        identity,
        identity_for_poller,
        identity_for_inbound,
        transport_identity,
        seed,
        backup_key,
        data_dir,
        config,
        config_path,
        config_arc,
        events_tx,
        mailbox_factory,
        crate::mailbox::poll::PollCadence::default(),
        LogSink::default(),
        sweep_shutdown_tx,
        sweep_handle,
        ready,
        shutdown,
    )
    .await
}

/// Like [`run_loopback`], but wires a REAL `MailboxConnectFactory` (an
/// in-process mailbox server, supplied by the cross-crate harness) and a
/// FAST [`PollCadence`](crate::mailbox::poll::PollCadence) so the
/// end-to-end offline-fallback guardrail (Phase 2.C Task 7) converges in
/// a few seconds instead of waiting a full 45–75 s production idle tick.
///
/// The same `factory` backs BOTH the recipient's `PollScheduler` (fetch)
/// and the sender's `MailboxFallbackShared` (deposit), exactly as
/// `run_with_transport` wires the production `ArtiMailboxFactory`. Drives
/// the unmodified production assembly: per-peer direct-timeout trigger →
/// `run_mailbox_fallback` deposit → recipient poll → `dispatch_mailbox`.
///
/// Production never reaches this entrypoint or `PollCadence::fast` — both
/// are gated on `feature = "test-harness"`.
#[cfg(feature = "test-harness")]
#[allow(clippy::too_many_arguments)]
pub async fn run_loopback_with_mailbox(
    data_dir: &Path,
    passphrase: &zeroize::Zeroizing<String>,
    config: Config,
    config_path: std::path::PathBuf,
    net: crate::transport::LoopbackNet,
    my_onion: String,
    test_factory: Arc<dyn crate::test_exports::TestMailboxFactory>,
    ready: oneshot::Sender<Ready>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()> {
    use crate::storage::Pool;

    // Bridge the public test factory to the crate-private MailboxConnectFactory
    // the daemon assembly consumes (same adapter `delivery_hub_with_mailbox`
    // uses). The test crate cannot name the private trait, so the bridge stays
    // on this side of the boundary.
    let mailbox_factory: Arc<dyn crate::mailbox::poll::MailboxConnectFactory> =
        crate::test_exports::mailbox_connect_factory_from_test(test_factory);

    std::fs::create_dir_all(data_dir)?;

    let vault_path = data_dir.join("identity.vault");
    let (_vault, identity_for_seed) = Vault::open(&vault_path, passphrase.as_str())?;
    let seed = derive_storage_seed(identity_for_seed)?;
    let backup_key = crate::identity::derive::hkdf_expand::<32>(
        seed.as_bytes(),
        crate::identity::derive::INFO_BACKUP_V1,
    )?;
    let (_vault2, identity) = Vault::open(&vault_path, passphrase.as_str())?;
    let (_vault3, identity_for_poller) = Vault::open(&vault_path, passphrase.as_str())?;
    let (_vault4, identity_for_inbound) = Vault::open(&vault_path, passphrase.as_str())?;
    let (_vault5, identity_for_transport) = Vault::open(&vault_path, passphrase.as_str())?;

    let pool = Arc::new(Pool::open(data_dir, &seed)?);

    match crate::storage::MessageRepo::new(&pool).backfill_body_text() {
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "body_text backfill failed"),
    }
    let n = crate::storage::MessageRepo::new(&pool).backfill_envelope_id()?;
    if n > 0 {
        tracing::info!(rows = n, "backfilled envelope_id for pre-1.H rows");
    }

    let config_arc = std::sync::Arc::new(tokio::sync::RwLock::new(config.clone()));

    let (sweep_shutdown_tx, sweep_shutdown_rx) = tokio::sync::watch::channel(false);
    let sweep_handle = crate::daemon::retention::spawn_sweep(
        pool.clone(),
        config_arc.clone(),
        std::time::Duration::from_secs(3600),
        sweep_shutdown_rx,
    );

    let (events_tx, _) = broadcast::channel::<Event>(EVENT_CHANNEL_CAPACITY);

    let transport = Arc::new(crate::transport::LoopbackTransport::new(net, my_onion));
    let transport_identity = Arc::new(identity_for_transport);

    run_with_transport(
        transport,
        pool,
        identity,
        identity_for_poller,
        identity_for_inbound,
        transport_identity,
        seed,
        backup_key,
        data_dir,
        config,
        config_path,
        config_arc,
        events_tx,
        mailbox_factory,
        crate::mailbox::poll::PollCadence::fast(),
        LogSink::default(),
        sweep_shutdown_tx,
        sweep_handle,
        ready,
        shutdown,
    )
    .await
}

/// Best-effort wipe of the managed attachment open-cache
/// (`<data_dir>/cache/open`). Decrypted plaintext lives here only while an
/// attachment is open; clearing it at boot + clean shutdown keeps plaintext
/// ephemeral. Failures are warned, never fatal.
pub(crate) fn wipe_open_cache(data_dir: &std::path::Path) {
    let dir = data_dir.join("cache").join("open");
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(error = %e, "open-cache wipe failed"),
    }
}

/// No-op `MailboxConnectFactory` for the loopback guardrail: no mailboxes are
/// configured in the test, so `connect` is never invoked; it returns
/// `Unreachable` to satisfy the signature.
#[cfg(feature = "test-harness")]
struct NoopMailboxFactory;

#[cfg(feature = "test-harness")]
#[async_trait::async_trait]
impl crate::mailbox::poll::MailboxConnectFactory for NoopMailboxFactory {
    async fn connect(
        &self,
        _onion: &str,
    ) -> crate::error::Result<
        crate::mailbox::client::MailboxClient<Box<dyn crate::mailbox::poll::MailboxStream>>,
    > {
        Err(crate::error::CoreError::MailboxClient(
            crate::error::MailboxClientErrorKind::Unreachable,
        ))
    }
}

/// Production `MailboxConnectFactory`: dials each mailbox onion through
/// the daemon's `arti_client::TorClient`. The factory is held by both
/// the `PollScheduler` (per-tick poll connections) and the `DaemonHandle`
/// (AddMailbox / RemoveMailbox / RotateOnion probes).
struct ArtiMailboxFactory {
    tor_client: arti_client::TorClient<tor_rtcompat::tokio::TokioRustlsRuntime>,
}

#[async_trait::async_trait]
impl crate::mailbox::poll::MailboxConnectFactory for ArtiMailboxFactory {
    async fn connect(
        &self,
        onion: &str,
    ) -> crate::error::Result<
        crate::mailbox::client::MailboxClient<Box<dyn crate::mailbox::poll::MailboxStream>>,
    > {
        let target = format!("{onion}:1");
        let stream = self
            .tor_client
            .connect(target.as_str())
            .await
            .map_err(|_| {
                crate::error::CoreError::MailboxClient(
                    crate::error::MailboxClientErrorKind::Unreachable,
                )
            })?;
        let boxed: Box<dyn crate::mailbox::poll::MailboxStream> = Box::new(stream);
        Ok(crate::mailbox::client::MailboxClient::from_stream(
            onion.to_string(),
            boxed,
        ))
    }
}

#[cfg(all(test, feature = "test-harness"))]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;
    use zeroize::Zeroizing;

    #[tokio::test]
    async fn second_daemon_on_locked_data_dir_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        // Hold the lock as if a daemon were already running.
        let _held = crate::daemon::lock::acquire(dir.path()).expect("first lock");
        // The startup guard run_with_transport uses must surface AlreadyRunning.
        let err = crate::daemon::lock::acquire(dir.path()).unwrap_err();
        assert!(matches!(
            err,
            crate::daemon::lock::LockError::AlreadyRunning
        ));
    }

    #[test]
    fn wipe_open_cache_removes_decrypted_plaintext() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp
            .path()
            .join("cache")
            .join("open")
            .join("aa")
            .join("x.bin");
        std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
        std::fs::write(&cache, b"plaintext").unwrap();
        super::wipe_open_cache(tmp.path());
        assert!(!tmp.path().join("cache").join("open").exists());
        // Idempotent: a second wipe on an absent dir is a no-op (no panic, no error).
        super::wipe_open_cache(tmp.path());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "spawns a real Arti bootstrap; run with --ignored"]
    async fn run_signals_ready_and_exits_on_shutdown() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_path_buf();
        let seed = crate::identity::Seed::generate().unwrap();
        let identity = crate::identity::IdentityKey::from_seed(&seed).unwrap();
        let pw = Zeroizing::new("a-test-passphrase-1234".to_string());
        crate::identity::Vault::create(&data_dir.join("identity.vault"), identity, pw.as_str())
            .unwrap();

        let mut config = Config::defaults().unwrap();
        config.data_dir = data_dir.clone();
        config.ipc_socket = Some(data_dir.join("ipc.sock"));

        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let shutdown_fut = async move {
            let _ = shutdown_rx.await;
        };

        // Move `data_dir` and `pw` into the spawned future so the borrows
        // are within the `'static` async block.
        let daemon_task = tokio::spawn(async move {
            Daemon::run(
                &data_dir,
                &pw,
                config,
                std::path::PathBuf::from("/dev/null"),
                ready_tx,
                shutdown_fut,
            )
            .await
        });

        let ready = tokio::time::timeout(std::time::Duration::from_secs(180), ready_rx)
            .await
            .expect("daemon becomes ready within 180 s")
            .expect("ready_tx still open");
        assert!(ready.onion.contains(".onion"));
        assert!(ready.ipc_socket.exists());

        shutdown_tx.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(30), daemon_task)
            .await
            .expect("shutdown within 30 s")
            .expect("join")
            .expect("daemon returned Ok");

        assert!(!ready.ipc_socket.exists(), "socket removed on drop");
    }
}
