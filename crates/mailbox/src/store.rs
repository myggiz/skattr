// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! SQLite-backed deposit store.
//!
//! One table, intentionally:
//!
//! ```sql
//! CREATE TABLE deposits (
//!   deposit_id      BLOB PRIMARY KEY,
//!   recipient_hash  BLOB NOT NULL,
//!   ciphertext      BLOB NOT NULL,
//!   deposited_at    INTEGER NOT NULL,
//!   expires_at      INTEGER NOT NULL
//! );
//! ```
//!
//! The server never stores who deposited what — only the recipient
//! hash. See the design doc §3.2 for the rationale.

use anyhow::Result;

/// One deposit returned by [`Store::fetch`]: `(ciphertext, received_at, deposit_id)`.
pub type StoredDeposit = (Vec<u8>, i64, [u8; 16]);

/// Deposit store handle.
#[derive(Debug)]
pub struct Store {
    _private: (),
}

impl Store {
    /// Open or create the store at the given path.
    pub fn open(_path: &std::path::Path) -> Result<Self> {
        // TODO(phase-2): open rusqlite::Connection, CREATE TABLE IF NOT
        // EXISTS, set pragmas.
        Ok(Self { _private: () })
    }

    /// Insert a deposit, returning its generated id.
    pub fn insert(
        &self,
        _recipient_hash: [u8; 32],
        _ciphertext: Vec<u8>,
        _expires_at: i64,
    ) -> Result<[u8; 16]> {
        todo!("Phase 2")
    }

    /// Fetch all deposits for a recipient hash.
    pub fn fetch(&self, _recipient_hash: [u8; 32]) -> Result<Vec<StoredDeposit>> {
        todo!("Phase 2")
    }

    /// Delete deposits by id.
    pub fn delete(&self, _deposit_ids: &[[u8; 16]]) -> Result<()> {
        todo!("Phase 2")
    }

    /// Sweep expired deposits. Call periodically.
    pub fn expire(&self, _now: i64) -> Result<u64> {
        todo!("Phase 2")
    }
}
