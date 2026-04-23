// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Daemon struct: owns all long-lived handles.

use std::path::Path;

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
    /// Run the Phase 0.C daemon: unlock the vault, derive the storage
    /// seed, bootstrap Tor, publish the onion service, signal readiness
    /// with the `.onion` address, then await a caller-supplied shutdown
    /// future. Returns `Ok(())` after a graceful shutdown.
    ///
    /// `ready` fires as soon as the onion is published — the caller can
    /// print the banner while this future continues to hold the runtime.
    ///
    /// This is the public entry point the CLI calls; subsequent phases
    /// extend it with the MLS session manager, outbox, mailbox poller,
    /// etc.
    pub async fn run(
        data_dir: &Path,
        passphrase: &zeroize::Zeroizing<String>,
        ready: oneshot::Sender<String>,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let vault_path = data_dir.join("identity.vault");
        let (_vault, identity) = Vault::open(&vault_path, passphrase.as_str())?;
        let seed = derive_storage_seed(identity)?;

        let cfg = TorConfig {
            state_dir: data_dir.join("arti"),
            socks_port: None,
        };
        let mut rt = TorRuntime::bootstrap(cfg).await?;

        let hs_key_path = data_dir.join("hs.key.age");
        let onion = rt
            .publish_onion(&hs_key_path, &seed, "skattr-daemon")
            .await?;

        // If the receiver was dropped, there's no reader — that's fine,
        // proceed to listen until shutdown.
        let _ = ready.send(onion);

        shutdown.await;
        rt.shutdown().await?;
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
            "Daemon::send requires 1.F CLI integration".into(),
        ))
    }
}
