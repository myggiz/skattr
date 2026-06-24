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

    /// Compare-and-set: flip `status` from `'pending'` to `'complete'`
    /// atomically. Returns `true` iff this call performed the transition
    /// (`rows_affected == 1`). The single fire-gate for `AttachmentReceived`,
    /// so the direct (3.B) and offline (3.C) lanes cannot both emit on a
    /// simultaneous completion.
    pub fn set_status_if_pending(&self, attachment_id: &[u8; 16]) -> Result<bool> {
        self.pool.with_mut(|c| {
            let n = c
                .execute(
                    "UPDATE attachments SET status = 'complete' \
                     WHERE attachment_id = ?1 AND status = 'pending'",
                    rusqlite::params![&attachment_id[..]],
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "attachments set_status_if_pending: {e}"
                    )))
                })?;
            Ok(n == 1)
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

    /// `(attachment_id, manifest_bytes)` for every `direction='in'`,
    /// `status='pending'` attachment — the offline receiver's match set.
    pub fn list_pending_in(&self) -> Result<Vec<([u8; 16], Vec<u8>)>> {
        self.pool.with(|c| {
            let mut stmt = c.prepare(
                "SELECT attachment_id, manifest FROM attachments \
                 WHERE direction = 'in' AND status = 'pending'",
            )?;
            let rows = stmt.query_map([], |r| {
                let aid: Vec<u8> = r.get(0)?;
                let m: Vec<u8> = r.get(1)?;
                Ok((aid, m))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (aid, m) = row?;
                if aid.len() == 16 {
                    let mut id = [0u8; 16];
                    id.copy_from_slice(&aid);
                    out.push((id, m));
                }
            }
            Ok(out)
        })
    }

    /// Store the sender's pubkey (`peer`) alongside the manifest row so
    /// `finalize_offline` can populate `Event::AttachmentReceived.contact`.
    /// Silently ignored if the row does not exist.
    pub fn set_peer(&self, attachment_id: &[u8; 16], peer: &[u8; 32]) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "UPDATE attachments SET peer = ?2 WHERE attachment_id = ?1",
                rusqlite::params![&attachment_id[..], &peer[..]],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "attachments set_peer: {e}"
                )))
            })?;
            Ok(())
        })
    }

    /// Retrieve the stored sender pubkey for `attachment_id`.
    /// Returns `None` if the row is absent, the column is NULL, or the
    /// stored BLOB is not exactly 32 bytes.
    pub fn peer_for(&self, attachment_id: &[u8; 16]) -> Result<Option<[u8; 32]>> {
        self.pool.with(|c| {
            match c.query_row(
                "SELECT peer FROM attachments WHERE attachment_id = ?1",
                rusqlite::params![&attachment_id[..]],
                |r| r.get::<_, Option<Vec<u8>>>(0),
            ) {
                Ok(Some(b)) if b.len() == 32 => {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&b);
                    Ok(Some(arr))
                }
                Ok(_) => Ok(None),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Storage(StorageErrorKind::Other(format!(
                    "attachments peer_for: {e}"
                )))),
            }
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

/// One due chunk-deposit row (Phase 3.C offline sender state).
pub struct DepositDue {
    pub attachment_id: [u8; 16],
    pub chunk_index: u32,
    pub recipient: [u8; 32],
    pub attempts: u32,
}

/// Sender-side per-chunk mailbox-deposit state. Rows carry no payload; the
/// chunk bytes are read from the `ChunkStore` at deposit time.
pub struct AttachmentDepositRepo<'p> {
    pool: &'p Pool,
}

impl<'p> AttachmentDepositRepo<'p> {
    /// Bind a deposit-row repository to `pool`.
    pub fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Enqueue all chunks `0..total_chunks` for `attachment_id`, due at
    /// `first_due_at_ms`. Idempotent on the (attachment_id, chunk_index) PK.
    pub fn enqueue_all(
        &self,
        attachment_id: &[u8; 16],
        recipient: &[u8; 32],
        total_chunks: u32,
        first_due_at_ms: i64,
    ) -> Result<()> {
        self.pool.transaction(|tx| {
            let mut stmt = tx
                .prepare(
                    "INSERT OR IGNORE INTO attachment_deposits \
                     (attachment_id, chunk_index, recipient, attempts, next_retry_at, status) \
                     VALUES (?1, ?2, ?3, 0, ?4, 'pending')",
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "attachment_deposits enqueue_all prepare: {e}"
                    )))
                })?;
            for i in 0..total_chunks {
                stmt.execute(rusqlite::params![
                    &attachment_id[..],
                    i,
                    &recipient[..],
                    first_due_at_ms
                ])
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "attachment_deposits enqueue_all execute: {e}"
                    )))
                })?;
            }
            Ok(())
        })
    }

    /// Pending rows whose `next_retry_at <= now_ms`, oldest first.
    pub fn due(&self, now_ms: i64, limit: usize) -> Result<Vec<DepositDue>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT attachment_id, chunk_index, recipient, attempts \
                     FROM attachment_deposits \
                     WHERE status = 'pending' AND next_retry_at <= ?1 \
                     ORDER BY next_retry_at ASC LIMIT ?2",
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "attachment_deposits due prepare: {e}"
                    )))
                })?;
            let rows = stmt
                .query_map(rusqlite::params![now_ms, limit as i64], |r| {
                    let aid: Vec<u8> = r.get(0)?;
                    let recip: Vec<u8> = r.get(2)?;
                    Ok((aid, r.get::<_, i64>(1)?, recip, r.get::<_, i64>(3)?))
                })
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "attachment_deposits due query: {e}"
                    )))
                })?;
            let mut out = Vec::new();
            for row in rows {
                let (aid, idx, recip, attempts) = row.map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "attachment_deposits due row: {e}"
                    )))
                })?;
                let mut attachment_id = [0u8; 16];
                let mut recipient = [0u8; 32];
                if aid.len() == 16 && recip.len() == 32 {
                    attachment_id.copy_from_slice(&aid);
                    recipient.copy_from_slice(&recip);
                    out.push(DepositDue {
                        attachment_id,
                        chunk_index: idx as u32,
                        recipient,
                        attempts: attempts as u32,
                    });
                }
            }
            Ok(out)
        })
    }

    /// Mark one chunk row `deposited` (the chunk reached a mailbox).
    pub fn mark_deposited(&self, attachment_id: &[u8; 16], chunk_index: u32) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "UPDATE attachment_deposits SET status = 'deposited' \
                 WHERE attachment_id = ?1 AND chunk_index = ?2",
                rusqlite::params![&attachment_id[..], chunk_index],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "attachment_deposits mark_deposited: {e}"
                )))
            })?;
            Ok(())
        })
    }

    /// Bump a chunk row's `attempts` and set its next retry time (backoff).
    pub fn reschedule(
        &self,
        attachment_id: &[u8; 16],
        chunk_index: u32,
        attempts: u32,
        next_retry_at_ms: i64,
    ) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "UPDATE attachment_deposits SET attempts = ?3, next_retry_at = ?4 \
                 WHERE attachment_id = ?1 AND chunk_index = ?2",
                rusqlite::params![&attachment_id[..], chunk_index, attempts, next_retry_at_ms],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "attachment_deposits reschedule: {e}"
                )))
            })?;
            Ok(())
        })
    }

    /// True if no `pending` rows remain for the attachment (all deposited, or
    /// none were ever enqueued).
    pub fn all_deposited(&self, attachment_id: &[u8; 16]) -> Result<bool> {
        self.pool.with(|c| {
            let n: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM attachment_deposits \
                     WHERE attachment_id = ?1 AND status = 'pending'",
                    rusqlite::params![&attachment_id[..]],
                    |r| r.get(0),
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "attachment_deposits all_deposited: {e}"
                    )))
                })?;
            Ok(n == 0)
        })
    }

    /// Delete every deposit row for `attachment_id` (called once all chunks
    /// are deposited, to prune the sender's offline-delivery state).
    pub fn delete_for_attachment(&self, attachment_id: &[u8; 16]) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "DELETE FROM attachment_deposits WHERE attachment_id = ?1",
                rusqlite::params![&attachment_id[..]],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "attachment_deposits delete_for_attachment: {e}"
                )))
            })?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_status_if_pending_fires_exactly_once() {
        let pool = Pool::in_memory();
        let aid = [0x11u8; 16];
        // Insert a pending 'in' attachment row directly (schema from 0015).
        pool.with_mut(|c| {
            c.execute(
                "INSERT INTO attachments \
                 (attachment_id, direction, manifest, total_chunks, status, created_at) \
                 VALUES (?1, 'in', x'00', 1, 'pending', 0)",
                rusqlite::params![&aid[..]],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
        let repo = AttachmentRepo::new(&pool);
        assert!(repo.set_status_if_pending(&aid).unwrap(), "first call wins");
        assert!(
            !repo.set_status_if_pending(&aid).unwrap(),
            "second call loses"
        );
    }

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

    #[test]
    fn enqueue_due_mark_and_all_deposited() {
        let pool = Pool::in_memory();
        let repo = AttachmentDepositRepo::new(&pool);
        let aid = [0xAB; 16];
        let recip = [0xCD; 32];
        // Enqueue 3 chunks due at t=1000ms.
        repo.enqueue_all(&aid, &recip, 3, 1000).unwrap();
        // Not due before 1000.
        assert!(repo.due(999, 10).unwrap().is_empty());
        // Due at/after 1000.
        let due = repo.due(1000, 10).unwrap();
        assert_eq!(due.len(), 3);
        assert_eq!(due[0].recipient, recip);
        assert!(!repo.all_deposited(&aid).unwrap());
        // Mark all deposited.
        for d in &due {
            repo.mark_deposited(&aid, d.chunk_index).unwrap();
        }
        assert!(repo.all_deposited(&aid).unwrap());
        // Deposited rows are no longer due.
        assert!(repo.due(2000, 10).unwrap().is_empty());
    }

    #[test]
    fn reschedule_defers_and_bumps_attempts() {
        let pool = Pool::in_memory();
        let repo = AttachmentDepositRepo::new(&pool);
        let aid = [0x11; 16];
        repo.enqueue_all(&aid, &[0; 32], 1, 0).unwrap();
        repo.reschedule(&aid, 0, 1, 5000).unwrap();
        assert!(repo.due(4999, 10).unwrap().is_empty());
        let due = repo.due(5000, 10).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].attempts, 1);
    }

    #[test]
    fn delete_for_attachment_clears_rows() {
        let pool = Pool::in_memory();
        let repo = AttachmentDepositRepo::new(&pool);
        let aid = [0x22; 16];
        repo.enqueue_all(&aid, &[0; 32], 2, 0).unwrap();
        repo.delete_for_attachment(&aid).unwrap();
        assert!(repo.due(10_000, 10).unwrap().is_empty());
        assert!(repo.all_deposited(&aid).unwrap()); // vacuously true: no pending rows
    }
}
