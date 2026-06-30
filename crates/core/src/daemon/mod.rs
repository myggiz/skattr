// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! The Skattr daemon.
//!
//! A [`Daemon`] owns all long-lived state: the Tor runtime, the SQLite
//! pool, active connections, the outbox worker, the mailbox poll
//! scheduler, and the MLS keystore. Downstream consumers (CLI, UI)
//! interact with it by submitting [`Command`]s and subscribing to
//! [`Event`]s — intentionally the same narrow interface for both.

pub(crate) mod accept;
pub mod backup;
pub(crate) mod clock;
pub mod commands;
pub mod config;
pub(crate) mod dispatch;
pub mod error_kind;
pub mod events;
pub(crate) mod handle;
pub mod hex;
pub(crate) mod inbound;
pub mod ipc;
pub mod logs;
pub mod paths;
pub(crate) mod retention;
pub mod smoke;
pub mod state;

pub use commands::{Command, CommandResult};
pub use config::Config;
pub use error_kind::DaemonErrorKind;
pub use events::{DeliveryStatus, Event, TorStatus};
pub use hex::{Hex16, Hex32};
pub use ipc::{IpcClient, IpcClientError};
pub use state::{Daemon, Ready};
