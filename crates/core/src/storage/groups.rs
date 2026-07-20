// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Repository for MLS group state blobs.

use super::StorageErrorKind;
use crate::error::{CoreError, Result};
use crate::storage::Pool;

/// Repository for MLS group state blobs.
pub struct MlsGroupRepo<'p> {
    pool: &'p Pool,
}

impl<'p> MlsGroupRepo<'p> {
    /// Construct a repo bound to the given `pool`.
    pub fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Save or update the serialized MLS state inside the caller's
    /// transaction. Use this when the MLS snapshot must commit
    /// atomically with other row inserts (e.g. message + outbox).
    pub(crate) fn put_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        group_id: &[u8],
        state_blob: &[u8],
        epoch: u64,
    ) -> Result<()> {
        let epoch_i = i64::try_from(epoch).map_err(|_| {
            CoreError::Storage(StorageErrorKind::Other("epoch overflows i64".into()))
        })?;
        tx.execute(
            "INSERT INTO mls_groups (group_id, state_blob, epoch) VALUES (?1, ?2, ?3) \
             ON CONFLICT(group_id) DO UPDATE SET state_blob=excluded.state_blob, \
                                                 epoch=excluded.epoch",
            rusqlite::params![group_id, state_blob, epoch_i],
        )
        .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("put mls group: {e}"))))?;
        Ok(())
    }

    /// Save or update the serialized MLS state for a group.
    pub fn put(&self, group_id: &[u8], state_blob: &[u8], epoch: u64) -> Result<()> {
        self.pool
            .transaction(|tx| self.put_in_tx(tx, group_id, state_blob, epoch))
    }

    /// Load the serialized state for a group. None if unknown.
    pub fn get(&self, group_id: &[u8]) -> Result<Option<Vec<u8>>> {
        self.pool.with(|c| {
            let result = c.query_row(
                "SELECT state_blob FROM mls_groups WHERE group_id = ?1",
                rusqlite::params![group_id],
                |r| r.get::<_, Vec<u8>>(0),
            );
            match result {
                Ok(b) => Ok(Some(b)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Storage(StorageErrorKind::Other(format!(
                    "get mls group: {e}"
                )))),
            }
        })
    }

    /// Delete the serialized state for `group_id` inside the caller's tx.
    pub(crate) fn delete_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        group_id: &[u8],
    ) -> Result<()> {
        tx.execute(
            "DELETE FROM mls_groups WHERE group_id = ?1",
            rusqlite::params![group_id],
        )
        .map_err(|e| {
            CoreError::Storage(StorageErrorKind::Other(format!("delete mls group: {e}")))
        })?;
        Ok(())
    }

    /// Enumerate all known groups as `(group_id, epoch)` pairs.
    pub(crate) fn list(&self) -> Result<Vec<(Vec<u8>, u64)>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare("SELECT group_id, epoch FROM mls_groups ORDER BY id")
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("prepare list mls: {e}")))
                })?;
            let rows = stmt
                .query_map([], |r| {
                    let gid: Vec<u8> = r.get(0)?;
                    let epoch: i64 = r.get(1)?;
                    Ok((gid, u64::try_from(epoch).unwrap_or(0)))
                })
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("query list mls: {e}")))
                })?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("collect mls list: {e}")))
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_roundtrip() {
        let pool = Pool::in_memory();
        let repo = MlsGroupRepo::new(&pool);
        let gid = [0xAA; 32];
        let state = b"serialized-state-bytes";
        repo.put(&gid, state, 7).unwrap();
        let got = repo.get(&gid).unwrap().unwrap();
        assert_eq!(got, state);
    }

    #[test]
    fn put_updates_existing_row() {
        let pool = Pool::in_memory();
        let repo = MlsGroupRepo::new(&pool);
        let gid = [0xBB; 32];
        repo.put(&gid, b"state-v1", 1).unwrap();
        repo.put(&gid, b"state-v2", 2).unwrap();
        assert_eq!(repo.get(&gid).unwrap().unwrap(), b"state-v2");
        assert_eq!(repo.list().unwrap().len(), 1);
    }

    #[test]
    fn get_missing_returns_none() {
        let pool = Pool::in_memory();
        let repo = MlsGroupRepo::new(&pool);
        assert!(repo.get(&[0x99; 32]).unwrap().is_none());
    }

    #[test]
    fn delete_in_tx_removes_group_row() {
        let pool = Pool::in_memory();
        let repo = MlsGroupRepo::new(&pool);
        repo.put(b"gid-1", b"blob", 0).unwrap();
        assert!(repo.get(b"gid-1").unwrap().is_some());
        pool.transaction(|tx| repo.delete_in_tx(tx, b"gid-1"))
            .unwrap();
        assert!(repo.get(b"gid-1").unwrap().is_none());
    }

    #[test]
    fn list_returns_all_groups() {
        let pool = Pool::in_memory();
        let repo = MlsGroupRepo::new(&pool);
        repo.put(&[0x01; 32], b"a", 1).unwrap();
        repo.put(&[0x02; 32], b"b", 5).unwrap();
        repo.put(&[0x03; 32], b"c", 10).unwrap();
        let all = repo.list().unwrap();
        assert_eq!(all.len(), 3);
        // Check epochs survived the round-trip.
        assert!(all.iter().any(|(_, e)| *e == 1));
        assert!(all.iter().any(|(_, e)| *e == 5));
        assert!(all.iter().any(|(_, e)| *e == 10));
    }
}
