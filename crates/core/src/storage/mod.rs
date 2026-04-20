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

pub(crate) use pool::Pool;
