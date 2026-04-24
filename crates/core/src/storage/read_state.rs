// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Per-group last-read pointer.
//!
//! Phase 1.G storage layer. `unread_count` is `COUNT(*)` of messages
//! with `id > last_read_message_id` for a given `group_id`; the cursor
//! advances via `mark_read`.

use super::StorageErrorKind;
use crate::error::{CoreError, Result};
use crate::storage::Pool;

/// Per-group last-read cursor.
pub struct ReadStateRepo<'p> {
    pool: &'p Pool,
}

impl<'p> ReadStateRepo<'p> {
    /// Construct a new repo backed by `pool`.
    pub fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Returns `Some(last_read_message_id)` if a cursor exists for
    /// `group_id`, `None` otherwise.
    pub fn get(&self, group_id: &[u8]) -> Result<Option<i64>> {
        self.pool.with(|c| {
            match c.query_row(
                "SELECT last_read_message_id FROM read_state WHERE group_id = ?1",
                rusqlite::params![group_id],
                |r| r.get::<_, i64>(0),
            ) {
                Ok(v) => Ok(Some(v)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Storage(StorageErrorKind::Other(format!(
                    "read_state get: {e}"
                )))),
            }
        })
    }

    /// Upsert the cursor. Idempotent.
    pub fn set(&self, group_id: &[u8], last_read_message_id: i64, updated_at: i64) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "INSERT INTO read_state (group_id, last_read_message_id, updated_at) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(group_id) DO UPDATE SET \
                     last_read_message_id = excluded.last_read_message_id, \
                     updated_at = excluded.updated_at",
                rusqlite::params![group_id, last_read_message_id, updated_at],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("read_state set: {e}")))
            })?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_returns_none_initially() {
        let pool = Pool::in_memory();
        let repo = ReadStateRepo::new(&pool);
        assert_eq!(repo.get(&[0xAA; 32]).unwrap(), None);
    }

    #[test]
    fn set_then_get_round_trips() {
        let pool = Pool::in_memory();
        let repo = ReadStateRepo::new(&pool);
        let gid = [0xBB; 32];
        repo.set(&gid, 42, 1_700_000_000).unwrap();
        assert_eq!(repo.get(&gid).unwrap(), Some(42));
    }

    #[test]
    fn set_overwrites_existing_cursor() {
        let pool = Pool::in_memory();
        let repo = ReadStateRepo::new(&pool);
        let gid = [0xCC; 32];
        repo.set(&gid, 10, 1_700_000_000).unwrap();
        repo.set(&gid, 99, 1_700_000_100).unwrap();
        assert_eq!(repo.get(&gid).unwrap(), Some(99));
    }
}
