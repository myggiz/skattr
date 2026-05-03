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

mod error_kind;
pub(crate) use error_kind::StorageErrorKind;

pub(crate) mod backup;
pub(crate) mod contacts;
pub(crate) mod groups;
pub(crate) mod key_packages;
pub(crate) mod mailboxes;
pub(crate) mod messages;
pub(crate) mod migrations;
pub(crate) mod outbox;
pub(crate) mod outstanding_invites;
pub(crate) mod pool;
pub(crate) mod read_state;
pub(crate) mod seen_messages;

// Under `test-harness` the items need a `pub` path so `lib.rs::test_exports`
// can re-export them. The `storage` module itself is `pub(crate)`, so these
// `pub use` items are still invisible outside the crate — the effective
// visibility is capped by the module. This is intentional.
#[cfg(not(feature = "test-harness"))]
pub(crate) use contacts::ContactRepo;
#[cfg(not(feature = "test-harness"))]
pub(crate) use groups::MlsGroupRepo;
#[cfg(not(feature = "test-harness"))]
pub(crate) use key_packages::KeyPackageRepo;
#[cfg(not(feature = "test-harness"))]
pub(crate) use mailboxes::{MailboxRepo, MailboxRow, MailboxStatus};
#[cfg(not(feature = "test-harness"))]
pub(crate) use messages::{InsertParams, MessageRepo};
#[cfg(not(feature = "test-harness"))]
pub(crate) use outstanding_invites::OutstandingInviteRepo;
#[cfg(not(feature = "test-harness"))]
pub(crate) use pool::Pool;
#[cfg(not(feature = "test-harness"))]
pub(crate) use read_state::ReadStateRepo;
#[cfg(not(feature = "test-harness"))]
pub(crate) use seen_messages::SeenMessagesRepo;

#[cfg(feature = "test-harness")]
pub use contacts::ContactRepo;
#[cfg(feature = "test-harness")]
pub use groups::MlsGroupRepo;
#[cfg(feature = "test-harness")]
pub use key_packages::KeyPackageRepo;
#[cfg(feature = "test-harness")]
pub use mailboxes::{MailboxRepo, MailboxRow, MailboxStatus};
#[cfg(feature = "test-harness")]
pub use messages::{InsertParams, MessageRepo};
#[cfg(feature = "test-harness")]
pub use outstanding_invites::OutstandingInviteRepo;
#[cfg(feature = "test-harness")]
pub use pool::Pool;
#[cfg(feature = "test-harness")]
pub use read_state::ReadStateRepo;
#[cfg(feature = "test-harness")]
pub use seen_messages::SeenMessagesRepo;
