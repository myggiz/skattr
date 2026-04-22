// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! SQL repository for the outbox table.
//!
//! The outbox stores per-peer, per-message entries awaiting delivery.
//! Rows are keyed uniquely by `(target, message_id)` so enqueue is
//! idempotent and ACK lookup is a single index probe. Migration 0004
//! added the `message_id` column.

use crate::error::{CoreError, Result};
use crate::storage::Pool;

/// A row read back from the `outbox` table: `(id, target, payload, message_id, attempts)`.
pub type OutboxRow = (i64, Vec<u8>, Vec<u8>, [u8; 16], u32);

pub(crate) struct OutboxRepo<'p> {
    pool: &'p Pool,
}

impl<'p> OutboxRepo<'p> {
    pub(crate) fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Idempotent insert. Returns `Some(rowid)` on a fresh insert,
    /// `None` if a row with this `(target, message_id)` pair already
    /// exists. Relies on the `idx_outbox_target_message_id` unique
    /// index from migration 0004.
    pub(crate) fn insert(
        &self,
        target: &[u8],
        message_id: &[u8; 16],
        payload: &[u8],
        next_retry_at: i64,
    ) -> Result<Option<i64>> {
        self.pool.with_mut(|c| {
            let changed = c
                .execute(
                    "INSERT OR IGNORE INTO outbox \
                     (target, message_id, payload, attempts, next_retry_at) \
                     VALUES (?1, ?2, ?3, 0, ?4)",
                    rusqlite::params![target, message_id.as_slice(), payload, next_retry_at],
                )
                .map_err(|e| CoreError::Storage(format!("insert outbox: {e}")))?;
            Ok(if changed == 0 {
                None
            } else {
                Some(c.last_insert_rowid())
            })
        })
    }

    /// Fetch entries whose `next_retry_at` has passed.
    pub(crate) fn due(&self, now: i64, limit: usize) -> Result<Vec<OutboxRow>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, target, payload, message_id, attempts FROM outbox \
                     WHERE next_retry_at <= ?1 \
                     ORDER BY next_retry_at LIMIT ?2",
                )
                .map_err(|e| CoreError::Storage(format!("prepare due: {e}")))?;
            let rows = stmt
                .query_map(
                    rusqlite::params![now, i64::try_from(limit).unwrap_or(i64::MAX)],
                    |r| {
                        let id: i64 = r.get(0)?;
                        let target: Vec<u8> = r.get(1)?;
                        let payload: Vec<u8> = r.get(2)?;
                        let mid_bytes: Vec<u8> = r.get(3)?;
                        let attempts: i64 = r.get(4)?;
                        let mut mid = [0u8; 16];
                        if mid_bytes.len() == 16 {
                            mid.copy_from_slice(&mid_bytes);
                        }
                        Ok((
                            id,
                            target,
                            payload,
                            mid,
                            u32::try_from(attempts).unwrap_or(u32::MAX),
                        ))
                    },
                )
                .map_err(|e| CoreError::Storage(format!("query due: {e}")))?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| CoreError::Storage(format!("collect due: {e}")))
        })
    }

    /// Delete the outbox row for `(target, message_id)`. Returns
    /// `true` if a row was removed.
    pub(crate) fn ack_by_message_id(&self, target: &[u8], message_id: &[u8; 16]) -> Result<bool> {
        self.pool.with_mut(|c| {
            let n = c
                .execute(
                    "DELETE FROM outbox WHERE target = ?1 AND message_id = ?2",
                    rusqlite::params![target, message_id.as_slice()],
                )
                .map_err(|e| CoreError::Storage(format!("ack outbox: {e}")))?;
            Ok(n > 0)
        })
    }

    /// Increment `attempts` and set a new `next_retry_at` for a failed
    /// send.
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
    fn insert_fresh_returns_some_rowid() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        let rowid = repo
            .insert(&[0x01; 32], &[0xAA; 16], b"payload", 1000)
            .unwrap();
        assert!(rowid.expect("fresh insert returns Some(rowid)") > 0);
    }

    #[test]
    fn insert_duplicate_returns_none() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        let first = repo
            .insert(&[0x01; 32], &[0xAA; 16], b"payload", 1000)
            .unwrap();
        assert!(first.is_some(), "first insert must return Some");
        let again = repo
            .insert(&[0x01; 32], &[0xAA; 16], b"payload", 1000)
            .unwrap();
        assert!(
            again.is_none(),
            "duplicate (target, message_id) must return None"
        );
    }

    #[test]
    fn insert_same_message_id_different_targets_both_succeed() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        let a = repo.insert(&[0x01; 32], &[0xAA; 16], b"p", 1000).unwrap();
        let b = repo.insert(&[0x02; 32], &[0xAA; 16], b"p", 1000).unwrap();
        assert!(
            a.is_some() && b.is_some(),
            "unique is (target, message_id), not message_id alone"
        );
    }

    #[test]
    fn due_returns_past_with_message_id_and_skips_future() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        let rid = repo
            .insert(&[0xAA; 32], &[0x11; 16], b"past", 100)
            .unwrap()
            .unwrap();
        let _ = repo
            .insert(&[0xBB; 32], &[0x22; 16], b"future", 9999)
            .unwrap();
        let due = repo.due(500, 10).unwrap();
        assert_eq!(due.len(), 1);
        let (id, target, payload, mid, attempts) = &due[0];
        assert_eq!(*id, rid);
        assert_eq!(target.as_slice(), &[0xAA; 32]);
        assert_eq!(payload.as_slice(), b"past");
        assert_eq!(mid, &[0x11; 16]);
        assert_eq!(*attempts, 0);
    }

    #[test]
    fn ack_by_message_id_deletes_matching_row() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        repo.insert(&[0x01; 32], &[0xAA; 16], b"p", 100).unwrap();
        assert!(repo.ack_by_message_id(&[0x01; 32], &[0xAA; 16]).unwrap());
        assert_eq!(repo.due(999, 10).unwrap().len(), 0);
    }

    #[test]
    fn ack_by_message_id_returns_false_when_no_match() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        repo.insert(&[0x01; 32], &[0xAA; 16], b"p", 100).unwrap();
        assert!(!repo.ack_by_message_id(&[0x01; 32], &[0xBB; 16]).unwrap());
        assert_eq!(repo.due(999, 10).unwrap().len(), 1);
    }

    #[test]
    fn reschedule_increments_attempts_and_bumps_next_retry_at() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        let rid = repo
            .insert(&[0xCC; 32], &[0x77; 16], b"retry", 100)
            .unwrap()
            .unwrap();
        repo.reschedule(rid, 200).unwrap();
        repo.reschedule(rid, 300).unwrap();
        let due = repo.due(999, 10).unwrap();
        assert_eq!(due.len(), 1);
        let (id, _, _, _, attempts) = &due[0];
        assert_eq!(*id, rid);
        assert_eq!(*attempts, 2, "attempts must be 2 after two reschedules");
    }
}
