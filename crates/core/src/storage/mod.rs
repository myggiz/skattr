// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! SQLite persistence layer.
//!
//! Pragmas applied at connection open:
//!
//! - `foreign_keys = ON`
//! - `journal_mode = WAL`
//! - `synchronous = NORMAL`
//!
//! The DB file is wrapped at the filesystem level with `age` encryption
//! using a key derived from the identity seed (see
//! [`crate::identity::derive::INFO_STORAGE_V1`]). Migrations are
//! `include_str!`'d from `migrations/`.

pub(crate) mod backup;
pub(crate) mod contacts;
pub(crate) mod groups;
pub(crate) mod mailboxes;
pub(crate) mod messages;
pub(crate) mod migrations;
pub(crate) mod outbox;
pub(crate) mod pool;
pub(crate) mod seen_messages;

// Under `test-harness` the items need a `pub` path so `lib.rs::test_exports`
// can re-export them. The `storage` module itself is `pub(crate)`, so these
// `pub use` items are still invisible outside the crate — the effective
// visibility is capped by the module. This is intentional.
#[cfg(not(feature = "test-harness"))]
pub(crate) use contacts::ContactRepo;
#[cfg(not(feature = "test-harness"))]
pub(crate) use messages::MessageRepo;
#[cfg(not(feature = "test-harness"))]
pub(crate) use pool::Pool;

#[cfg(feature = "test-harness")]
pub use contacts::ContactRepo;
#[cfg(feature = "test-harness")]
pub use messages::MessageRepo;
#[cfg(feature = "test-harness")]
pub use pool::Pool;
