// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Dedup table for received messages: `(sender, message_id)` with TTL sweep.
//!
//! Receiver-side dedup uses a sliding 24-hour window. We insert
//! `(sender, message_id, now)` on every successful receive and query
//! "contains(sender, message_id)" on each incoming envelope before
//! surfacing it to the UI. `sweep_older_than(cutoff)` is called
//! periodically to garbage-collect rows outside the window.

use super::StorageErrorKind;
use crate::error::{CoreError, Result};
use crate::storage::Pool;

/// Repository for tracking which `(sender, message_id)` pairs have been seen,
/// for receiver-side deduplication with a sliding 24-hour TTL window.
pub struct SeenMessagesRepo<'p> {
    pool: &'p Pool,
}

impl<'p> SeenMessagesRepo<'p> {
    /// Create a new `SeenMessagesRepo` backed by the given `Pool`.
    pub fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Mark a message as seen inside the caller's transaction. Returns
    /// `true` if this is new (insert succeeded) or `false` if we've
    /// already seen it (PRIMARY KEY conflict). Use this when the seen-row
    /// must commit atomically with other rows (e.g. MLS snapshot +
    /// messages insert in one transaction).
    pub(crate) fn insert_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        sender: &[u8],
        message_id: &[u8],
        seen_at: i64,
    ) -> Result<bool> {
        let changed = tx
            .execute(
                "INSERT OR IGNORE INTO seen_messages (sender, message_id, seen_at) \
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![sender, message_id, seen_at],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("insert seen: {e}")))
            })?;
        Ok(changed > 0)
    }

    /// Mark a message as seen. Returns `true` if this is new (insert
    /// succeeded) or `false` if we've already seen it (PRIMARY KEY
    /// conflict).
    pub fn insert(&self, sender: &[u8], message_id: &[u8], seen_at: i64) -> Result<bool> {
        self.pool
            .transaction(|tx| self.insert_in_tx(tx, sender, message_id, seen_at))
    }

    /// Borrow the underlying pool. Used by `delivery::receiver::receive`
    /// to open a transaction that wraps both the seen-messages insert and
    /// the messages insert atomically.
    pub(crate) fn pool(&self) -> &Pool {
        self.pool
    }

    /// Has this (sender, message_id) been seen?
    pub fn contains(&self, sender: &[u8], message_id: &[u8]) -> Result<bool> {
        self.pool.with(|c| {
            let count: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM seen_messages WHERE sender = ?1 AND message_id = ?2",
                    rusqlite::params![sender, message_id],
                    |r| r.get(0),
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("contains seen: {e}")))
                })?;
            Ok(count > 0)
        })
    }

    /// Delete rows with `seen_at < cutoff`. Returns the number removed.
    pub fn sweep_older_than(&self, cutoff: i64) -> Result<u64> {
        self.pool.with_mut(|c| {
            let n = c
                .execute(
                    "DELETE FROM seen_messages WHERE seen_at < ?1",
                    rusqlite::params![cutoff],
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("sweep seen: {e}")))
                })?;
            Ok(n as u64)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_is_idempotent() {
        let pool = Pool::in_memory();
        let repo = SeenMessagesRepo::new(&pool);
        let sender = [0xAA; 32];
        let mid = [0x01; 16];
        assert!(
            repo.insert(&sender, &mid, 100).unwrap(),
            "first insert is new"
        );
        assert!(
            !repo.insert(&sender, &mid, 200).unwrap(),
            "second insert is dup"
        );
    }

    #[test]
    fn contains_after_insert() {
        let pool = Pool::in_memory();
        let repo = SeenMessagesRepo::new(&pool);
        let sender = [0xBB; 32];
        let mid = [0x02; 16];
        assert!(!repo.contains(&sender, &mid).unwrap());
        repo.insert(&sender, &mid, 100).unwrap();
        assert!(repo.contains(&sender, &mid).unwrap());
    }

    #[test]
    fn sweep_removes_old_rows_only() {
        let pool = Pool::in_memory();
        let repo = SeenMessagesRepo::new(&pool);
        repo.insert(&[0x01; 32], &[0xAA; 16], 100).unwrap();
        repo.insert(&[0x02; 32], &[0xBB; 16], 500).unwrap();
        repo.insert(&[0x03; 32], &[0xCC; 16], 900).unwrap();

        let removed = repo.sweep_older_than(600).unwrap();
        assert_eq!(removed, 2);
        assert!(!repo.contains(&[0x01; 32], &[0xAA; 16]).unwrap());
        assert!(!repo.contains(&[0x02; 32], &[0xBB; 16]).unwrap());
        assert!(repo.contains(&[0x03; 32], &[0xCC; 16]).unwrap());
    }
}
