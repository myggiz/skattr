// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! SQL repository for the outbox table.
//!
//! The outbox stores per-peer, per-message entries awaiting delivery.
//! Rows are keyed uniquely by `(target, message_id, target_kind, mailbox_id)` so
//! enqueue is idempotent and ACK lookup is a single index probe. Migration 0004
//! added the `message_id` column; migration 0008 added `target_kind` and
//! `mailbox_id` to support mailbox-routed delivery alongside direct delivery.

use super::StorageErrorKind;
use crate::error::{CoreError, Result};
use crate::storage::Pool;

/// Whether a delivery row targets a peer directly or via a mailbox server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxTargetKind {
    Direct,
    Mailbox,
}

impl OutboxTargetKind {
    pub(crate) fn as_sql(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Mailbox => "mailbox",
        }
    }

    pub(crate) fn from_sql(s: &str) -> Self {
        match s {
            "mailbox" => Self::Mailbox,
            _ => Self::Direct,
        }
    }
}

/// Whether an `insert_direct` / `insert_for_mailbox` call created a new row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertOutcome {
    Inserted,
    AlreadyPresent,
}

/// A row read back from the `outbox` table.
#[derive(Debug, Clone)]
pub struct OutboxRow {
    pub id: i64,
    pub target: Vec<u8>,
    pub payload: Vec<u8>,
    pub message_id: [u8; 16],
    pub attempts: u32,
    pub target_kind: OutboxTargetKind,
    pub mailbox_id: i64,
}

pub(crate) struct OutboxRepo<'p> {
    pool: &'p Pool,
}

impl<'p> OutboxRepo<'p> {
    pub(crate) fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    // ── shared row mapper ────────────────────────────────────────────────────

    fn map_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<OutboxRow> {
        let id: i64 = r.get(0)?;
        let target: Vec<u8> = r.get(1)?;
        let payload: Vec<u8> = r.get(2)?;
        let mid_bytes: Vec<u8> = r.get(3)?;
        let attempts: i64 = r.get(4)?;
        let kind_str: String = r.get(5)?;
        let mailbox_id: i64 = r.get(6)?;
        let mut message_id = [0u8; 16];
        if mid_bytes.len() == 16 {
            message_id.copy_from_slice(&mid_bytes);
        }
        Ok(OutboxRow {
            id,
            target,
            payload,
            message_id,
            attempts: u32::try_from(attempts).unwrap_or(u32::MAX),
            target_kind: OutboxTargetKind::from_sql(&kind_str),
            mailbox_id,
        })
    }

    // ── inserts ──────────────────────────────────────────────────────────────

    /// Idempotent direct-delivery insert inside the caller's transaction.
    /// Returns `Some(rowid)` on a fresh insert, `None` on duplicate.
    ///
    /// Use this when the outbox row must commit atomically with other
    /// rows (e.g. MLS snapshot + message in one transaction).
    pub(crate) fn insert_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        target: &[u8],
        message_id: &[u8; 16],
        payload: &[u8],
        next_retry_at: i64,
    ) -> Result<Option<i64>> {
        let changed = tx
            .execute(
                "INSERT INTO outbox \
                 (target, message_id, payload, attempts, next_retry_at, target_kind, mailbox_id) \
                 VALUES (?1, ?2, ?3, 0, ?4, 'direct', 0) \
                 ON CONFLICT(target, message_id, target_kind, mailbox_id) DO NOTHING",
                rusqlite::params![target, message_id.as_slice(), payload, next_retry_at],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("insert outbox: {e}")))
            })?;
        Ok(if changed == 0 {
            None
        } else {
            Some(tx.last_insert_rowid())
        })
    }

    /// Idempotent direct-delivery insert. Returns `InsertOutcome::Inserted`
    /// on a fresh insert or `InsertOutcome::AlreadyPresent` on duplicate.
    /// Relies on the composite unique index from migration 0008.
    pub(crate) fn insert_direct(
        &self,
        target: &[u8],
        message_id: &[u8; 16],
        payload: &[u8],
        next_retry_at: i64,
    ) -> Result<InsertOutcome> {
        self.pool.with_mut(|c| {
            let changed = c
                .execute(
                    "INSERT INTO outbox \
                     (target, message_id, payload, attempts, next_retry_at, target_kind, mailbox_id) \
                     VALUES (?1, ?2, ?3, 0, ?4, 'direct', 0) \
                     ON CONFLICT(target, message_id, target_kind, mailbox_id) DO NOTHING",
                    rusqlite::params![target, message_id.as_slice(), payload, next_retry_at],
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "insert_direct outbox: {e}"
                    )))
                })?;
            Ok(if changed == 1 {
                InsertOutcome::Inserted
            } else {
                InsertOutcome::AlreadyPresent
            })
        })
    }

    /// Shim keeping the old `insert` name functional for callers that have
    /// not yet been migrated to `insert_direct`. Returns `Some(rowid)` on a
    /// fresh insert, `None` on duplicate — matching the pre-2.B contract.
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
                    "INSERT INTO outbox \
                     (target, message_id, payload, attempts, next_retry_at, target_kind, mailbox_id) \
                     VALUES (?1, ?2, ?3, 0, ?4, 'direct', 0) \
                     ON CONFLICT(target, message_id, target_kind, mailbox_id) DO NOTHING",
                    rusqlite::params![target, message_id.as_slice(), payload, next_retry_at],
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("insert outbox: {e}")))
                })?;
            Ok(if changed == 0 {
                None
            } else {
                Some(c.last_insert_rowid())
            })
        })
    }

    /// Insert a mailbox-routed delivery row. The composite unique index makes
    /// this idempotent per `(target, message_id, mailbox_id)`.
    pub(crate) fn insert_for_mailbox(
        &self,
        target: &[u8],
        message_id: &[u8; 16],
        mailbox_id: i64,
        payload: &[u8],
        next_retry_at: i64,
    ) -> Result<InsertOutcome> {
        self.pool.with_mut(|c| {
            let changed = c
                .execute(
                    "INSERT INTO outbox \
                     (target, message_id, payload, attempts, next_retry_at, target_kind, mailbox_id) \
                     VALUES (?1, ?2, ?3, 0, ?4, 'mailbox', ?5) \
                     ON CONFLICT(target, message_id, target_kind, mailbox_id) DO NOTHING",
                    rusqlite::params![
                        target,
                        message_id.as_slice(),
                        payload,
                        next_retry_at,
                        mailbox_id
                    ],
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "insert_for_mailbox outbox: {e}"
                    )))
                })?;
            Ok(if changed == 1 {
                InsertOutcome::Inserted
            } else {
                InsertOutcome::AlreadyPresent
            })
        })
    }

    // ── queries ──────────────────────────────────────────────────────────────

    /// Fetch a single row by primary key.
    pub(crate) fn get(&self, id: i64) -> Result<Option<OutboxRow>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, target, payload, message_id, attempts, target_kind, mailbox_id \
                     FROM outbox WHERE id = ?1",
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("prepare get: {e}")))
                })?;
            let mut rows = stmt
                .query_map(rusqlite::params![id], Self::map_row)
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("query get: {e}")))
                })?;
            match rows.next() {
                None => Ok(None),
                Some(r) => r.map(Some).map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("row get: {e}")))
                }),
            }
        })
    }

    /// Fetch entries whose `next_retry_at` has passed.
    pub(crate) fn list_due(&self, now: i64) -> Result<Vec<OutboxRow>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, target, payload, message_id, attempts, target_kind, mailbox_id \
                     FROM outbox WHERE next_retry_at <= ?1 ORDER BY next_retry_at",
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("prepare list_due: {e}")))
                })?;
            let rows = stmt
                .query_map(rusqlite::params![now], Self::map_row)
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("query list_due: {e}")))
                })?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("collect list_due: {e}")))
            })
        })
    }

    /// Fetch entries whose `next_retry_at` has passed (legacy name, forwards to `list_due`).
    pub(crate) fn due(&self, now: i64, limit: usize) -> Result<Vec<OutboxRow>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, target, payload, message_id, attempts, target_kind, mailbox_id \
                     FROM outbox WHERE next_retry_at <= ?1 ORDER BY next_retry_at LIMIT ?2",
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("prepare due: {e}")))
                })?;
            let rows = stmt
                .query_map(
                    rusqlite::params![now, i64::try_from(limit).unwrap_or(i64::MAX)],
                    Self::map_row,
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("query due: {e}")))
                })?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("collect due: {e}")))
            })
        })
    }

    /// Due rows with `target_kind='mailbox'`, oldest first, up to `limit`.
    ///
    /// `now` and the `next_retry_at` column are in milliseconds — the same
    /// domain as [`due`]/[`list_due`] (per-peer retry tick + sweeper).
    pub(crate) fn due_mailbox(&self, now: i64, limit: usize) -> Result<Vec<OutboxRow>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, target, payload, message_id, attempts, target_kind, mailbox_id \
                     FROM outbox WHERE next_retry_at <= ?1 AND target_kind='mailbox' \
                     ORDER BY next_retry_at LIMIT ?2",
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("prepare due_mailbox: {e}")))
                })?;
            let rows = stmt
                .query_map(
                    rusqlite::params![now, i64::try_from(limit).unwrap_or(i64::MAX)],
                    Self::map_row,
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("query due_mailbox: {e}")))
                })?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("collect due_mailbox: {e}")))
            })
        })
    }

    // ── mutations ─────────────────────────────────────────────────────────────

    /// Look up the row id for an existing direct-delivery row keyed by
    /// `(target, message_id, target_kind='direct')`. Returns `Ok(None)` when
    /// no such row exists. Used by the delivery hub's fallback orchestrator
    /// to find the row it should retarget to a mailbox.
    pub(crate) fn find_direct_id(
        &self,
        target: &[u8],
        message_id: &[u8; 16],
    ) -> Result<Option<i64>> {
        use rusqlite::OptionalExtension;
        self.pool.with(|c| {
            c.query_row(
                "SELECT id FROM outbox \
                 WHERE target = ?1 AND message_id = ?2 AND target_kind = 'direct' \
                 LIMIT 1",
                rusqlite::params![target, message_id.as_slice()],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("find_direct_id: {e}")))
            })
        })
    }

    /// Delete all outbox rows for `target` inside the caller's transaction.
    /// Idempotent — no error if no rows match.
    pub(crate) fn delete_by_target_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        target: &[u8],
    ) -> Result<()> {
        tx.execute(
            "DELETE FROM outbox WHERE target = ?1",
            rusqlite::params![target],
        )
        .map_err(|e| {
            CoreError::Storage(StorageErrorKind::Other(format!(
                "delete outbox by target: {e}"
            )))
        })?;
        Ok(())
    }

    /// Delete a single outbox row by id. Returns `true` if a row was removed.
    /// Used by the fallback orchestrator after a successful mailbox deposit
    /// (the row's message has been "delivered" by the mailbox).
    pub(crate) fn delete_by_id(&self, row_id: i64) -> Result<bool> {
        self.pool.with_mut(|c| {
            let n = c
                .execute(
                    "DELETE FROM outbox WHERE id = ?1",
                    rusqlite::params![row_id],
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("delete_by_id: {e}")))
                })?;
            Ok(n > 0)
        })
    }

    /// Flip an existing direct row to mailbox delivery. Called by the delivery
    /// hub when it determines the peer should be reached via a mailbox server.
    ///
    /// Returns an error if no row with `row_id` exists — a stale or already-
    /// acked id must be a hard error so the orchestrator doesn't proceed with
    /// a phantom row.
    pub(crate) fn set_mailbox_target(&self, row_id: i64, mailbox_id: i64) -> Result<()> {
        self.pool.with_mut(|c| {
            let n = c
                .execute(
                    "UPDATE outbox SET target_kind='mailbox', mailbox_id=?1 WHERE id=?2",
                    rusqlite::params![mailbox_id, row_id],
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("set_mailbox_target: {e}")))
                })?;
            if n == 0 {
                return Err(CoreError::Storage(StorageErrorKind::Other(format!(
                    "set_mailbox_target: no row with id={row_id}"
                ))));
            }
            Ok(())
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
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("ack outbox: {e}")))
                })?;
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
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("reschedule outbox: {e}")))
            })?;
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
        let row = &due[0];
        assert_eq!(row.id, rid);
        assert_eq!(row.target.as_slice(), &[0xAA; 32]);
        assert_eq!(row.payload.as_slice(), b"past");
        assert_eq!(row.message_id, [0x11; 16]);
        assert_eq!(row.attempts, 0);
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
        let row = &due[0];
        assert_eq!(row.id, rid);
        assert_eq!(row.attempts, 2, "attempts must be 2 after two reschedules");
    }

    // ── new Task-3 tests ─────────────────────────────────────────────────────

    #[test]
    fn insert_defaults_target_kind_to_direct() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        repo.insert_direct(&[1u8; 32], &[0xAB; 16], &[7, 7, 7], 100)
            .unwrap();
        let rows = repo.list_due(200).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target_kind, OutboxTargetKind::Direct);
        assert_eq!(rows[0].mailbox_id, 0);
    }

    #[test]
    fn set_mailbox_target_flips_kind_and_id() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        repo.insert_direct(&[1u8; 32], &[0xAB; 16], &[7, 7, 7], 100)
            .unwrap();
        let id = repo.list_due(200).unwrap()[0].id;
        repo.set_mailbox_target(id, 42).unwrap();
        let row = repo.get(id).unwrap().unwrap();
        assert_eq!(row.target_kind, OutboxTargetKind::Mailbox);
        assert_eq!(row.mailbox_id, 42);
    }

    #[test]
    fn composite_unique_index_allows_one_per_kind() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        let target = [1u8; 32];
        let msg_id = [0xAB; 16];
        repo.insert_direct(&target, &msg_id, &[7, 7, 7], 100)
            .unwrap();
        repo.insert_for_mailbox(&target, &msg_id, 7, &[7, 7, 7], 100)
            .unwrap();
        let rows = repo.list_due(200).unwrap();
        assert_eq!(
            rows.len(),
            2,
            "direct + mailbox rows for same (target,msg) coexist"
        );
    }

    #[test]
    fn set_mailbox_target_errors_on_missing_row() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        let err = repo
            .set_mailbox_target(999, 42)
            .expect_err("must reject missing id");
        match err {
            CoreError::Storage(StorageErrorKind::Other(s)) => {
                assert!(s.contains("no row with id=999"), "got: {s}");
            }
            other => panic!("expected Storage(Other), got {other:?}"),
        }
    }

    #[test]
    fn delete_by_target_in_tx_removes_all_target_rows() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        let target = [0x55u8; 32];
        repo.insert(&target, &[0xA1; 16], b"p1", 100).unwrap();
        repo.insert(&target, &[0xA2; 16], b"p2", 100).unwrap();
        // A different target — must survive.
        repo.insert(&[0x66u8; 32], &[0xA3; 16], b"p3", 100)
            .unwrap();
        pool.transaction(|tx| repo.delete_by_target_in_tx(tx, &target))
            .unwrap();
        let remaining = repo.due(999, 10).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].target.as_slice(), &[0x66u8; 32]);
    }

    #[test]
    fn due_mailbox_returns_only_mailbox_kind() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        repo.insert_direct(&[1u8; 32], &[0xAA; 16], b"d", 100)
            .unwrap();
        repo.insert_for_mailbox(&[1u8; 32], &[0xBB; 16], 9, b"m", 100)
            .unwrap();
        let rows = repo.due_mailbox(200, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target_kind, OutboxTargetKind::Mailbox);
        assert_eq!(rows[0].mailbox_id, 9);
    }

    #[test]
    fn duplicate_direct_insert_is_idempotent() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        let target = [1u8; 32];
        let msg_id = [0xAB; 16];
        repo.insert_direct(&target, &msg_id, &[7, 7, 7], 100)
            .unwrap();
        let rc = repo
            .insert_direct(&target, &msg_id, &[7, 7, 7], 100)
            .unwrap();
        assert_eq!(rc, InsertOutcome::AlreadyPresent);
        assert_eq!(repo.list_due(200).unwrap().len(), 1);
    }
}
