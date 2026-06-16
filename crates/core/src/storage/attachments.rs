// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Attachment transfer-state persistence (Phase 3.A).
//!
//! Tracks per-attachment manifest + status and per-chunk received bitmap.
//! Ciphertext blobs live on disk in `crate::attachment::store`; this repo
//! holds only the transfer bookkeeping.

use super::StorageErrorKind;
use crate::error::{CoreError, Result};
use crate::storage::Pool;

/// A row from the `attachments` table.
pub struct AttachmentRow {
    pub direction: String,
    pub manifest: Vec<u8>,
    pub total_chunks: i64,
    pub status: String,
    pub created_at: i64,
}

/// Persistence for attachment transfer state.
pub struct AttachmentRepo<'p> {
    pool: &'p Pool,
}

impl<'p> AttachmentRepo<'p> {
    /// Construct a new repo backed by `pool`.
    pub fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Insert a new attachment row with status `'pending'`.
    pub fn insert(
        &self,
        attachment_id: &[u8; 16],
        direction: &str,
        manifest: &[u8],
        total_chunks: i64,
        created_at: i64,
    ) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "INSERT INTO attachments \
                 (attachment_id, direction, manifest, total_chunks, status, created_at) \
                 VALUES (?1, ?2, ?3, ?4, 'pending', ?5)",
                rusqlite::params![
                    &attachment_id[..],
                    direction,
                    manifest,
                    total_chunks,
                    created_at
                ],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("attachments insert: {e}")))
            })?;
            Ok(())
        })
    }

    /// Fetch a row by id, or `None` if absent.
    pub fn get(&self, attachment_id: &[u8; 16]) -> Result<Option<AttachmentRow>> {
        self.pool.with(|c| {
            match c.query_row(
                "SELECT direction, manifest, total_chunks, status, created_at \
                 FROM attachments WHERE attachment_id = ?1",
                rusqlite::params![&attachment_id[..]],
                |r| {
                    Ok(AttachmentRow {
                        direction: r.get(0)?,
                        manifest: r.get(1)?,
                        total_chunks: r.get(2)?,
                        status: r.get(3)?,
                        created_at: r.get(4)?,
                    })
                },
            ) {
                Ok(row) => Ok(Some(row)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Storage(StorageErrorKind::Other(format!(
                    "attachments get: {e}"
                )))),
            }
        })
    }

    /// Mark a chunk index as received. Idempotent.
    pub fn mark_received(&self, attachment_id: &[u8; 16], chunk_index: u32) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "INSERT OR REPLACE INTO attachment_chunks \
                 (attachment_id, chunk_index, received) VALUES (?1, ?2, 1)",
                rusqlite::params![&attachment_id[..], chunk_index],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "attachments mark_received: {e}"
                )))
            })?;
            Ok(())
        })
    }

    /// Return the sorted set of received chunk indices.
    pub fn received_indices(&self, attachment_id: &[u8; 16]) -> Result<Vec<u32>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT chunk_index FROM attachment_chunks \
                     WHERE attachment_id = ?1 AND received = 1 ORDER BY chunk_index",
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "attachments received_indices prepare: {e}"
                    )))
                })?;
            let rows = stmt
                .query_map(rusqlite::params![&attachment_id[..]], |r| {
                    r.get::<_, i64>(0)
                })
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "attachments received_indices query: {e}"
                    )))
                })?;
            let mut out = Vec::new();
            for row in rows {
                let idx = row.map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "attachments received_indices row: {e}"
                    )))
                })?;
                let idx = u32::try_from(idx).map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "attachments received_indices range: {e}"
                    )))
                })?;
                out.push(idx);
            }
            Ok(out)
        })
    }

    /// Set the transfer status.
    pub fn set_status(&self, attachment_id: &[u8; 16], status: &str) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "UPDATE attachments SET status = ?2 WHERE attachment_id = ?1",
                rusqlite::params![&attachment_id[..], status],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "attachments set_status: {e}"
                )))
            })?;
            Ok(())
        })
    }

    /// Delete the attachment row and its chunk bitmap.
    pub fn delete(&self, attachment_id: &[u8; 16]) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "DELETE FROM attachment_chunks WHERE attachment_id = ?1",
                rusqlite::params![&attachment_id[..]],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "attachments delete chunks: {e}"
                )))
            })?;
            c.execute(
                "DELETE FROM attachments WHERE attachment_id = ?1",
                rusqlite::params![&attachment_id[..]],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("attachments delete: {e}")))
            })?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_get_and_mark_received_round_trip() {
        let pool = Pool::in_memory();
        let repo = AttachmentRepo::new(&pool);
        repo.insert(&[0x11; 16], "out", b"manifest-bytes", 3, 100)
            .unwrap();
        let row = repo.get(&[0x11; 16]).unwrap().unwrap();
        assert_eq!(row.total_chunks, 3);
        assert_eq!(row.status, "pending");
        assert_eq!(row.manifest, b"manifest-bytes");
        assert_eq!(
            repo.received_indices(&[0x11; 16]).unwrap(),
            Vec::<u32>::new()
        );
        repo.mark_received(&[0x11; 16], 0).unwrap();
        repo.mark_received(&[0x11; 16], 2).unwrap();
        repo.mark_received(&[0x11; 16], 0).unwrap(); // idempotent
        assert_eq!(repo.received_indices(&[0x11; 16]).unwrap(), vec![0, 2]);
        repo.set_status(&[0x11; 16], "complete").unwrap();
        assert_eq!(repo.get(&[0x11; 16]).unwrap().unwrap().status, "complete");
        assert!(repo.get(&[0x99; 16]).unwrap().is_none());
    }
}
