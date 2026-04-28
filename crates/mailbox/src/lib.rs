// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version. See LICENSE-AGPL3.

//! Skattr mailbox server library.
//!
//! Transport-agnostic core of the AGPLv3 mailbox server. The
//! `skattr-mailbox` binary is a thin wrapper that bootstraps Arti +
//! signal handling on top of this library; tests, fuzzers, and the
//! soak driver use the library directly with `tokio::io::duplex`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod auth;
pub mod background;
pub mod codec;
pub mod config;
pub mod dispatch;
pub mod error;
pub mod health;
pub(crate) mod migrations;
pub mod policy;
pub mod server;
pub mod store;

pub use config::MailboxConfig;
pub use error::{
    AuthErrorKind, ConfigErrorKind, MailboxError, MailboxErrorKind, PolicyErrorKind,
    StorageErrorKind, TransportErrorKind,
};
pub use policy::{Policy, TokenBucket};
pub use server::MailboxServer;
pub use store::Store;
