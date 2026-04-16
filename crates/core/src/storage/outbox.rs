// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! SQL repository for the outbox table.
//!
//! The delivery layer ([`crate::delivery::outbox`]) wraps this with
//! higher-level semantics; this file holds only the raw SQL operations.

use crate::error::Result;

/// A row read back from the `outbox` table: `(id, target, payload, attempts)`.
pub type OutboxRow = (i64, Vec<u8>, Vec<u8>, u32);

/// Outbox storage repository.
#[derive(Debug)]
pub struct OutboxRepo {
    _private: (),
}

impl OutboxRepo {
    /// Insert a new outbound entry.
    pub fn insert(&self, _target: &[u8], _payload: &[u8], _next_retry_at: i64) -> Result<i64> {
        todo!("INSERT INTO outbox …; RETURNING id")
    }

    /// Delete by rowid.
    pub fn delete(&self, _id: i64) -> Result<()> {
        todo!("DELETE FROM outbox WHERE id = ?")
    }

    /// Fetch entries whose `next_retry_at` has passed.
    pub fn due(&self, _now: i64, _limit: usize) -> Result<Vec<OutboxRow>> {
        todo!("SELECT id, target, payload, attempts … LIMIT ?")
    }
}
