// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! # Skattr core library
//!
//! `skattr-core` is the protocol library that powers the Skattr CLI, the
//! forthcoming Tauri UI, and the integration test crate.
//!
//! ## Module layout
//!
//! - [`identity`]: long-term Ed25519 identity keys, BIP39 seed phrases,
//!   passphrase-encrypted on-disk vaults.
//! - [`transport`]: framed Noise_XK over Tor v3 onion services, via Arti.
//! - [`mls`]: OpenMLS integration, group state machine, keystore bridge.
//! - [`envelope`]: CBOR application payloads carried inside MLS.
//! - [`invite`]: signed invite links + optional QR rendering.
//! - [`contact`]: contacts, signed `ContactCard`s, address rotation.
//! - [`mailbox`]: client of the mailbox server for offline delivery.
//! - [`delivery`]: outbox, retry, dedup, ACK handling.
//! - [`storage`]: SQLite persistence with migrations.
//! - [`daemon`]: top-level process that owns all long-lived handles.
//!
//! ## Public API boundary
//!
//! Only a handful of modules are part of the stable public API. Everything
//! else is `pub(crate)` to keep the surface small and auditable. See
//! `ARCHITECTURE.md` at the workspace root for the full rationale.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod contact;
pub mod daemon;
pub mod envelope;
pub mod error;
pub mod identity;
pub mod invite;
pub mod prelude;

pub(crate) mod delivery;
pub(crate) mod mailbox;
pub(crate) mod mls;
pub(crate) mod storage;
pub mod transport;

pub use error::{CoreError, Result};
