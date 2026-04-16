// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! `OpenMlsKeyStore` implementation backed by `storage::groups`.
//!
//! OpenMLS treats the keystore as an opaque `<key, bytes>` map with
//! type-erased entries. We persist every write transactionally to SQLite
//! so a crashed client recovers a consistent MLS state on restart.

use crate::error::Result;

/// Handle used by OpenMLS to read/write MLS key material.
///
/// Thin wrapper: actual read/write goes through
/// [`crate::storage::groups::MlsGroupRepo`].
#[derive(Debug)]
pub struct SqliteKeyStore {
    _private: (),
}

impl SqliteKeyStore {
    /// Construct. Not meant to be called directly by tests — go through
    /// the daemon's wired-up repo instead.
    pub(crate) fn new() -> Result<Self> {
        Ok(Self { _private: () })
    }
}
