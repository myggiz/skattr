// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! SQL repository for the outbox table.

use crate::error::{CoreError, Result};
use crate::storage::Pool;

/// A row read back from the `outbox` table: `(id, target, payload, attempts)`.
pub type OutboxRow = (i64, Vec<u8>, Vec<u8>, u32);

pub(crate) struct OutboxRepo<'p> {
    pool: &'p Pool,
}

impl<'p> OutboxRepo<'p> {
    pub(crate) fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Insert a new outbound entry. Returns the rowid.
    pub(crate) fn insert(&self, target: &[u8], payload: &[u8], next_retry_at: i64) -> Result<i64> {
        self.pool.with_mut(|c| {
            c.execute(
                "INSERT INTO outbox (target, payload, attempts, next_retry_at) \
                 VALUES (?1, ?2, 0, ?3)",
                rusqlite::params![target, payload, next_retry_at],
            )
            .map_err(|e| CoreError::Storage(format!("insert outbox: {e}")))?;
            Ok(c.last_insert_rowid())
        })
    }

    /// Delete by rowid (e.g. after successful ACK).
    pub(crate) fn delete(&self, id: i64) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute("DELETE FROM outbox WHERE id = ?1", rusqlite::params![id])
                .map_err(|e| CoreError::Storage(format!("delete outbox: {e}")))?;
            Ok(())
        })
    }

    /// Fetch entries whose `next_retry_at` has passed.
    pub(crate) fn due(&self, now: i64, limit: usize) -> Result<Vec<OutboxRow>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, target, payload, attempts FROM outbox \
                     WHERE next_retry_at <= ?1 \
                     ORDER BY next_retry_at LIMIT ?2",
                )
                .map_err(|e| CoreError::Storage(format!("prepare due: {e}")))?;
            let rows = stmt
                .query_map(
                    rusqlite::params![now, i64::try_from(limit).unwrap_or(i64::MAX)],
                    |r| {
                        let attempts: i64 = r.get(3)?;
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            u32::try_from(attempts).unwrap_or(u32::MAX),
                        ))
                    },
                )
                .map_err(|e| CoreError::Storage(format!("query due: {e}")))?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| CoreError::Storage(format!("collect due: {e}")))
        })
    }

    /// Increment attempts and set a new next_retry_at for a failed send.
    pub(crate) fn reschedule(&self, id: i64, next_retry_at: i64) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "UPDATE outbox SET attempts = attempts + 1, next_retry_at = ?1 WHERE id = ?2",
                rusqlite::params![next_retry_at, id],
            )
            .map_err(|e| CoreError::Storage(format!("reschedule outbox: {e}")))?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_returns_rowid() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        let id = repo.insert(&[0x01; 32], b"payload", 1000).unwrap();
        assert!(id > 0);
    }

    #[test]
    fn due_returns_past_and_skips_future() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        let id_past = repo.insert(&[0xAA; 32], b"past", 100).unwrap();
        let _id_future = repo.insert(&[0xBB; 32], b"future", 9999).unwrap();
        let due = repo.due(500, 10).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].0, id_past);
    }

    #[test]
    fn reschedule_increments_attempts() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        let id = repo.insert(&[0xCC; 32], b"retry", 100).unwrap();
        repo.reschedule(id, 200).unwrap();
        repo.reschedule(id, 300).unwrap();
        let due = repo.due(999, 10).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].3, 2, "attempts must be 2 after two reschedules");
    }

    #[test]
    fn delete_removes_row() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        let id = repo.insert(&[0xDD; 32], b"x", 100).unwrap();
        repo.delete(id).unwrap();
        assert_eq!(repo.due(999, 10).unwrap().len(), 0);
    }
}
