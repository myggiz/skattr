// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Durable storage for first-contact pending Welcome re-sends (#93).
//!
//! One row per pending first contact where we are the committer/invitee and
//! the peer has not yet acknowledged the Welcome. Rows are deleted on Ack
//! (peer joined successfully) or on RemoveContact.

use crate::error::{CoreError, Result};
use crate::storage::{pool::Pool, StorageErrorKind};

/// A pending Welcome row whose `next_retry_at <= now_ms` (due for re-send).
pub struct PendingWelcomeDue {
    /// Responder identity pubkey (32 bytes).
    pub peer: [u8; 32],
    /// Genesis group id.
    pub group_id: Vec<u8>,
    /// Exact Welcome message bytes to re-send.
    pub welcome_bytes: Vec<u8>,
    /// Number of send attempts so far.
    pub attempts: i64,
}

/// Repository for the `pending_welcomes` table.
pub struct PendingWelcomeRepo<'p> {
    pool: &'p Pool,
}

impl<'p> PendingWelcomeRepo<'p> {
    /// Bind a pending-welcome repository to `pool`.
    pub fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Insert a new pending-welcome row inside an existing transaction.
    ///
    /// This is an associated function (no `&self`) so it can be called from
    /// within the `add_contact` transaction in another module.
    /// Idempotent on `peer_pubkey` via `INSERT OR IGNORE`.
    pub fn insert_in_tx(
        tx: &rusqlite::Transaction<'_>,
        peer: &[u8; 32],
        group_id: &[u8],
        welcome_bytes: &[u8],
        next_retry_at_ms: i64,
        now_ms: i64,
    ) -> Result<()> {
        tx.execute(
            "INSERT OR IGNORE INTO pending_welcomes \
             (peer_pubkey, group_id, welcome_bytes, next_retry_at, attempts, created_at) \
             VALUES (?1, ?2, ?3, ?4, 0, ?5)",
            rusqlite::params![&peer[..], group_id, welcome_bytes, next_retry_at_ms, now_ms],
        )
        .map_err(|e| {
            CoreError::Storage(StorageErrorKind::Other(format!(
                "pending_welcomes insert_in_tx: {e}"
            )))
        })?;
        Ok(())
    }

    /// Return rows whose `next_retry_at <= now_ms`, oldest first, up to `limit`.
    pub fn due(&self, now_ms: i64, limit: usize) -> Result<Vec<PendingWelcomeDue>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT peer_pubkey, group_id, welcome_bytes, attempts \
                     FROM pending_welcomes \
                     WHERE next_retry_at <= ?1 \
                     ORDER BY next_retry_at ASC LIMIT ?2",
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "pending_welcomes due prepare: {e}"
                    )))
                })?;
            let rows = stmt
                .query_map(rusqlite::params![now_ms, limit as i64], |r| {
                    let peer_bytes: Vec<u8> = r.get(0)?;
                    let group_id: Vec<u8> = r.get(1)?;
                    let welcome_bytes: Vec<u8> = r.get(2)?;
                    let attempts: i64 = r.get(3)?;
                    Ok((peer_bytes, group_id, welcome_bytes, attempts))
                })
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "pending_welcomes due query: {e}"
                    )))
                })?;
            let mut out = Vec::new();
            for row in rows {
                let (peer_bytes, group_id, welcome_bytes, attempts) = row.map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "pending_welcomes due row: {e}"
                    )))
                })?;
                if peer_bytes.len() == 32 {
                    let mut peer = [0u8; 32];
                    peer.copy_from_slice(&peer_bytes);
                    out.push(PendingWelcomeDue {
                        peer,
                        group_id,
                        welcome_bytes,
                        attempts,
                    });
                }
            }
            Ok(out)
        })
    }

    /// Increment `attempts` and push `next_retry_at` forward (backoff).
    pub fn reschedule(&self, peer: &[u8; 32], next_retry_at_ms: i64) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "UPDATE pending_welcomes \
                 SET attempts = attempts + 1, next_retry_at = ?2 \
                 WHERE peer_pubkey = ?1",
                rusqlite::params![&peer[..], next_retry_at_ms],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "pending_welcomes reschedule: {e}"
                )))
            })?;
            Ok(())
        })
    }

    /// Delete the row for `peer`. Idempotent — no error if the row is absent.
    pub fn delete(&self, peer: &[u8; 32]) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "DELETE FROM pending_welcomes WHERE peer_pubkey = ?1",
                rusqlite::params![&peer[..]],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "pending_welcomes delete: {e}"
                )))
            })?;
            Ok(())
        })
    }

    /// Returns `true` if a pending-welcome row exists for `peer` (i.e. the
    /// first-contact flow is still in progress and the peer has not yet Ack'd).
    /// Used by `send_message` to block app-frame sends while `PendingJoin` (#93).
    pub fn is_pending(&self, peer: &[u8; 32]) -> Result<bool> {
        self.pool.with(|c| {
            let count: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM pending_welcomes WHERE peer_pubkey = ?1",
                    rusqlite::params![&peer[..]],
                    |r| r.get(0),
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "pending_welcomes is_pending: {e}"
                    )))
                })?;
            Ok(count > 0)
        })
    }

    /// Total row count — used for the boot-resume log line.
    pub fn count(&self) -> Result<i64> {
        self.pool.with(|c| {
            c.query_row("SELECT COUNT(*) FROM pending_welcomes", [], |r| r.get(0))
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "pending_welcomes count: {e}"
                    )))
                })
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::storage::Pool;

    fn pool() -> Pool {
        let dir = tempfile::tempdir().unwrap();
        let seed = crate::identity::Seed::generate().unwrap();
        Pool::open(dir.path(), &seed).unwrap()
    }

    #[test]
    fn insert_due_reschedule_delete_roundtrip() {
        let p = pool();
        let repo = PendingWelcomeRepo::new(&p);
        let peer = [7u8; 32];
        p.transaction(|tx| {
            PendingWelcomeRepo::insert_in_tx(tx, &peer, &[1, 2, 3], &[9, 9, 9], 1_000, 1_000)
        })
        .unwrap();

        // Due at/after next_retry_at.
        let due = repo.due(1_000, 10).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].peer, peer);
        assert_eq!(due[0].welcome_bytes, vec![9, 9, 9]);
        assert_eq!(due[0].attempts, 0);

        // Not due before next_retry_at.
        assert!(repo.due(999, 10).unwrap().is_empty());

        // Reschedule bumps attempts + moves the due time.
        repo.reschedule(&peer, 5_000).unwrap();
        assert!(repo.due(1_000, 10).unwrap().is_empty());
        let due2 = repo.due(5_000, 10).unwrap();
        assert_eq!(due2[0].attempts, 1);

        assert_eq!(repo.count().unwrap(), 1);
        repo.delete(&peer).unwrap();
        repo.delete(&peer).unwrap(); // idempotent
        assert_eq!(repo.count().unwrap(), 0);
    }
}
