// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Daemon struct: owns all long-lived handles.

use tokio::sync::broadcast;

use crate::daemon::commands::{Command, CommandResult};
use crate::daemon::config::Config;
use crate::daemon::events::Event;
use crate::error::Result;

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
