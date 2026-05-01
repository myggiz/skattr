// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Daemon struct: owns all long-lived handles.

use std::path::Path;
use std::sync::Arc;

use tokio::sync::{broadcast, oneshot};

use crate::daemon::commands::{Command, CommandResult};
use crate::daemon::config::Config;
use crate::daemon::events::Event;
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
        ready: oneshot::Sender<Ready>,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        use crate::daemon::handle::DaemonHandle;
        use crate::daemon::inbound::DaemonInbound;
        use crate::daemon::ipc::server::{current_uid, serve, Server};
        use crate::delivery::hub::DeliveryHub;
        use crate::delivery::peer::InboundDispatch;
        use crate::storage::Pool;

        std::fs::create_dir_all(data_dir)?;

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
        let (_vault2, identity) = Vault::open(&vault_path, passphrase.as_str())?;
        let (_vault3, identity_for_poller) = Vault::open(&vault_path, passphrase.as_str())?;

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

        // Phase 1.G: hourly retention sweep.
        let (sweep_shutdown_tx, sweep_shutdown_rx) = tokio::sync::watch::channel(false);
        let sweep_handle = crate::daemon::retention::spawn_sweep(
            pool.clone(),
            config.history.retention_days,
            std::time::Duration::from_secs(3600),
            sweep_shutdown_rx,
        );

        // Step 3: Tor bootstrap + onion publish.
        let cfg = TorConfig {
            state_dir: data_dir.join("arti"),
            socks_port: None,
        };
        let mut rt = TorRuntime::bootstrap(cfg).await?;

        let hs_key_path = data_dir.join("hs.key.age");
        let onion = rt
            .publish_onion(&hs_key_path, &seed, "skattr-daemon")
            .await?;

        // Step 4: event broadcast channel.
        let (events_tx, _) = broadcast::channel::<Event>(EVENT_CHANNEL_CAPACITY);

        // Step 5: DaemonInbound + DeliveryHub.
        let inbound = Arc::new(DaemonInbound::new(pool.clone(), events_tx.clone()))
            as Arc<dyn InboundDispatch>;
        // Use DataStream as the transport type parameter. The hub stores
        // per-peer actor channels; actual DataStream-backed connections are
        // injected via `hub.ingest()` from the onion-listener accept loop
        // (wired in a later phase). For Phase 1.F we only need the hub
        // constructed so IPC commands can be dispatched.
        let hub: Arc<DeliveryHub<arti_client::DataStream>> =
            Arc::new(DeliveryHub::new_with_inbound(pool.clone(), inbound.clone()));

        // Step 5.5: Mailbox connect factory + PollScheduler.
        // The factory holds a cloned `arti_client::TorClient` so
        // `AddMailbox` / `RemoveMailbox` / `RotateOnion` handlers (and
        // per-mailbox poll actors) can dial mailboxes through Arti
        // independently of the daemon's owning `TorRuntime`. The
        // scheduler is held by `Daemon::run` for the lifetime of the
        // daemon — its `Drop` aborts the supervisor on shutdown.
        let mailbox_factory: Arc<dyn crate::mailbox::poll::MailboxConnectFactory> =
            Arc::new(ArtiMailboxFactory {
                tor_client: rt.client().clone(),
            });
        let identity_arc = Arc::new(identity_for_poller);
        let scheduler = crate::mailbox::poll::PollScheduler::spawn(
            pool.clone(),
            identity_arc,
            events_tx.clone(),
            mailbox_factory.clone(),
            Some(inbound.clone()),
        );
        let poller_ctrl = scheduler.ctrl();

        // Step 6: DaemonHandle.
        let handle = DaemonHandle::<arti_client::DataStream>::new_with_mailbox(
            pool,
            hub,
            identity,
            events_tx.clone(),
            mailbox_factory,
            poller_ctrl,
        );
        handle.set_onion(onion.clone());

        // TorStatus tap: subscribe to the broadcast channel and copy
        // every TorStatusChanged into the same Arc<RwLock<…>> the
        // IpcServer reads via DaemonHandle::latest_tor_status(). Spawned
        // after DaemonHandle is built so the tap and the readers share
        // the same allocation. Held on a JoinHandle so the shutdown
        // path can abort it.
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
        let allowed_uid = current_uid();
        let ipc_server = Server::bind(&sock_path, allowed_uid)?;
        let sock_path_copy = sock_path.clone();

        let (ipc_shutdown_tx, ipc_shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        // Build the executor Arc from a clone of the handle's subsystems.
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
        // Explicitly drop the PollScheduler so its supervisor task is
        // aborted before Arti shuts down — a poll actor mid-Challenge
        // would otherwise observe a torn-down circuit.
        drop(scheduler);
        rt.shutdown().await?;
        // Server::drop removes the socket file automatically.
        Ok(())
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
            Daemon::run(&data_dir, &pw, config, ready_tx, shutdown_fut).await
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
