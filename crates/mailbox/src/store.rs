// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! SQLite-backed deposit store.
//!
//! Single-table schema (see `migrations/0001_init.sql`). The store is
//! transactional; cap enforcement and insertion happen atomically so
//! a `RecipientFull` rejection never leaves the DB in a partial state.

use std::path::Path;
use std::sync::Mutex;

use rand::RngCore;
use rusqlite::{params, Connection, OpenFlags};

use crate::error::{MailboxError, PolicyErrorKind, StorageErrorKind};
use crate::migrations;

/// One row from `deposits` returned by [`Store::fetch`].
#[derive(Debug, Clone)]
pub struct StoredDeposit {
    /// 16-byte server-generated id for this deposit.
    pub deposit_id: [u8; 16],
    /// Opaque ciphertext blob exactly as deposited.
    pub ciphertext: Vec<u8>,
    /// Server-side timestamp (`deposited_at`) recorded when the row
    /// was inserted; reused as `received_at` in fetch responses.
    pub received_at: i64,
}

/// Deposit store handle.
#[derive(Debug)]
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// Open or create the store at `path`. Sets WAL + `synchronous=NORMAL`
    /// + `foreign_keys=ON`, then runs migrations.
    pub fn open(path: &Path) -> Result<Self, MailboxError> {
        let mut conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrations::apply(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// In-memory store for tests (unit + integration). Not gated on
    /// `#[cfg(test)]` so integration tests under `tests/` and the
    /// soak driver can construct fresh instances.
    pub fn in_memory() -> Result<Self, MailboxError> {
        let mut conn = Connection::open_in_memory()?;
        migrations::apply(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Insert a deposit, enforcing three caps atomically: the per-recipient
    /// byte cap ([`PolicyErrorKind::RecipientFull`]), the distinct-recipient
    /// count cap ([`PolicyErrorKind::RecipientLimit`], checked only for a
    /// brand-new recipient), and the global byte cap
    /// ([`PolicyErrorKind::ServerFull`]). To make room, only expired rows are
    /// evicted (oldest first); accepted non-expired rows are never evicted, so
    /// a cap that can't be satisfied by reclaiming expired space rejects.
    /// Returns the generated `deposit_id` on success.
    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        &self,
        recipient_hash: [u8; 32],
        ciphertext: Vec<u8>,
        deposited_at: i64,
        expires_at: i64,
        recipient_cap_bytes: u64,
        global_storage_cap_bytes: u64,
        max_recipients: u64,
        now: i64,
    ) -> Result<[u8; 16], MailboxError> {
        let new_len = u64::try_from(ciphertext.len()).unwrap_or(u64::MAX);
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| MailboxError::Storage(StorageErrorKind::Poisoned))?;
        let tx = conn.transaction()?;

        let existing: i64 = tx
            .query_row(
                "SELECT COALESCE(SUM(LENGTH(ciphertext)), 0) FROM deposits \
                 WHERE recipient_hash = ?1",
                params![recipient_hash.to_vec()],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let mut existing_bytes = existing as u64;

        if existing_bytes + new_len > recipient_cap_bytes {
            // Try evicting expired rows (oldest first).
            let to_free = (existing_bytes + new_len) - recipient_cap_bytes;
            evict_expired_for(&tx, recipient_hash, to_free, now)?;
            let after: i64 = tx
                .query_row(
                    "SELECT COALESCE(SUM(LENGTH(ciphertext)), 0) FROM deposits \
                     WHERE recipient_hash = ?1",
                    params![recipient_hash.to_vec()],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            existing_bytes = after as u64;
            if existing_bytes + new_len > recipient_cap_bytes {
                tx.rollback()?;
                return Err(MailboxError::Policy(PolicyErrorKind::RecipientFull));
            }
        }

        // ── recipient-count cap: only when this is a NEW recipient ──
        let recipient_rows: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM deposits WHERE recipient_hash = ?1",
                params![recipient_hash.to_vec()],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if recipient_rows == 0 {
            // O(n) scan over deposits; acceptable — only runs when this is a
            // brand-new recipient, gating the distinct-recipient count cap.
            let distinct: i64 = tx
                .query_row(
                    "SELECT COUNT(DISTINCT recipient_hash) FROM deposits",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if distinct.max(0) as u64 >= max_recipients {
                tx.rollback()?;
                return Err(MailboxError::Policy(PolicyErrorKind::RecipientLimit));
            }
        }

        // ── global byte cap: evict expired globally, then reject if still over ──
        let total: i64 = tx
            .query_row(
                "SELECT COALESCE(SUM(LENGTH(ciphertext)), 0) FROM deposits",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let mut total_bytes = total.max(0) as u64;
        if total_bytes + new_len > global_storage_cap_bytes {
            let to_free = (total_bytes + new_len) - global_storage_cap_bytes;
            evict_expired_global(&tx, to_free, now)?;
            let after: i64 = tx
                .query_row(
                    "SELECT COALESCE(SUM(LENGTH(ciphertext)), 0) FROM deposits",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            total_bytes = after.max(0) as u64;
            if total_bytes + new_len > global_storage_cap_bytes {
                tx.rollback()?;
                return Err(MailboxError::Policy(PolicyErrorKind::ServerFull));
            }
        }

        let mut id = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut id);
        tx.execute(
            "INSERT INTO deposits (deposit_id, recipient_hash, ciphertext, deposited_at, expires_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id.to_vec(),
                recipient_hash.to_vec(),
                ciphertext,
                deposited_at,
                expires_at
            ],
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Fetch all (non-expired) deposits for a recipient hash. Caller
    /// passes `now` so expiry checks use server clock.
    pub fn fetch(
        &self,
        recipient_hash: [u8; 32],
        now: i64,
    ) -> Result<Vec<StoredDeposit>, MailboxError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| MailboxError::Storage(StorageErrorKind::Poisoned))?;
        let mut stmt = conn.prepare(
            "SELECT deposit_id, ciphertext, deposited_at FROM deposits \
             WHERE recipient_hash = ?1 AND expires_at > ?2 \
             ORDER BY deposited_at ASC",
        )?;
        let rows = stmt
            .query_map(params![recipient_hash.to_vec(), now], |r| {
                let id_blob: Vec<u8> = r.get(0)?;
                let mut id = [0u8; 16];
                if id_blob.len() == 16 {
                    id.copy_from_slice(&id_blob);
                }
                Ok(StoredDeposit {
                    deposit_id: id,
                    ciphertext: r.get(1)?,
                    received_at: r.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Delete deposits by id, scoped to the given recipient. Returns
    /// `(deleted, not_found)` counts so the dispatch handler can build
    /// `DeleteOk` directly.
    pub fn delete(
        &self,
        recipient_hash: [u8; 32],
        deposit_ids: &[[u8; 16]],
    ) -> Result<(u32, u32), MailboxError> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| MailboxError::Storage(StorageErrorKind::Poisoned))?;
        let tx = conn.transaction()?;
        let mut deleted: u32 = 0;
        for id in deposit_ids {
            let n = tx.execute(
                "DELETE FROM deposits WHERE deposit_id = ?1 AND recipient_hash = ?2",
                params![id.to_vec(), recipient_hash.to_vec()],
            )?;
            deleted += u32::try_from(n).unwrap_or(0);
        }
        tx.commit()?;
        let not_found = u32::try_from(deposit_ids.len())
            .unwrap_or(u32::MAX)
            .saturating_sub(deleted);
        Ok((deleted, not_found))
    }

    /// Expire all rows whose `expires_at < now`. Returns the number
    /// removed.
    pub fn expire_sweep(&self, now: i64) -> Result<u64, MailboxError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| MailboxError::Storage(StorageErrorKind::Poisoned))?;
        let n = conn.execute("DELETE FROM deposits WHERE expires_at < ?1", params![now])?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// Total bytes stored across all recipients. Used by the metrics
    /// tick.
    pub fn storage_bytes(&self) -> Result<u64, MailboxError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| MailboxError::Storage(StorageErrorKind::Poisoned))?;
        let total: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(LENGTH(ciphertext)), 0) FROM deposits",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        Ok(total.max(0) as u64)
    }
}

fn evict_expired_for(
    tx: &rusqlite::Transaction<'_>,
    recipient_hash: [u8; 32],
    target_bytes: u64,
    now: i64,
) -> Result<(), MailboxError> {
    // Oldest expired first; stop when freed enough or list exhausted.
    let mut stmt = tx.prepare(
        "SELECT deposit_id, LENGTH(ciphertext) FROM deposits \
         WHERE recipient_hash = ?1 AND expires_at < ?2 \
         ORDER BY deposited_at ASC",
    )?;
    let candidates: Vec<(Vec<u8>, i64)> = stmt
        .query_map(params![recipient_hash.to_vec(), now], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    let mut freed: u64 = 0;
    for (id, bytes) in candidates {
        tx.execute("DELETE FROM deposits WHERE deposit_id = ?1", params![id])?;
        freed = freed.saturating_add(u64::try_from(bytes).unwrap_or(0));
        if freed >= target_bytes {
            break;
        }
    }
    let _ = freed; // silence warning when target_bytes == 0
    Ok::<(), MailboxError>(())
}

fn evict_expired_global(
    tx: &rusqlite::Transaction<'_>,
    target_bytes: u64,
    now: i64,
) -> Result<(), MailboxError> {
    let mut stmt = tx.prepare(
        "SELECT deposit_id, LENGTH(ciphertext) FROM deposits \
         WHERE expires_at < ?1 ORDER BY deposited_at ASC",
    )?;
    let candidates: Vec<(Vec<u8>, i64)> = stmt
        .query_map(params![now], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    let mut freed: u64 = 0;
    for (id, bytes) in candidates {
        tx.execute("DELETE FROM deposits WHERE deposit_id = ?1", params![id])?;
        freed = freed.saturating_add(u64::try_from(bytes).unwrap_or(0));
        if freed >= target_bytes {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const REC_A: [u8; 32] = [0x11; 32];
    const REC_B: [u8; 32] = [0x22; 32];
    const ONE_GB: u64 = 1 << 30;

    #[test]
    fn insert_and_fetch_round_trip() {
        let s = Store::in_memory().unwrap();
        let id = s
            .insert(REC_A, vec![1, 2, 3], 100, 200, ONE_GB, ONE_GB, 100, 50)
            .unwrap();
        let rows = s.fetch(REC_A, 150).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].deposit_id, id);
        assert_eq!(rows[0].ciphertext, vec![1, 2, 3]);
        assert_eq!(rows[0].received_at, 100);
    }

    #[test]
    fn fetch_skips_expired_rows() {
        let s = Store::in_memory().unwrap();
        s.insert(REC_A, vec![9], 100, 110, ONE_GB, ONE_GB, 100, 50)
            .unwrap();
        assert_eq!(s.fetch(REC_A, 200).unwrap().len(), 0);
    }

    #[test]
    fn fetch_is_per_recipient() {
        let s = Store::in_memory().unwrap();
        s.insert(REC_A, vec![1], 100, 999_999, ONE_GB, ONE_GB, 100, 50)
            .unwrap();
        s.insert(REC_B, vec![2], 100, 999_999, ONE_GB, ONE_GB, 100, 50)
            .unwrap();
        assert_eq!(s.fetch(REC_A, 150).unwrap().len(), 1);
    }

    #[test]
    fn delete_returns_counts_and_is_recipient_scoped() {
        let s = Store::in_memory().unwrap();
        let id_a = s
            .insert(REC_A, vec![1; 4], 100, 200, ONE_GB, ONE_GB, 100, 50)
            .unwrap();
        let id_b = s
            .insert(REC_B, vec![2; 4], 100, 200, ONE_GB, ONE_GB, 100, 50)
            .unwrap();
        // Try to delete id_b with REC_A's hash: should not match, count as not_found.
        let (deleted, not_found) = s.delete(REC_A, &[id_a, id_b]).unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(not_found, 1);
    }

    #[test]
    fn expire_sweep_removes_only_expired() {
        let s = Store::in_memory().unwrap();
        s.insert(REC_A, vec![1], 100, 110, ONE_GB, ONE_GB, 100, 50)
            .unwrap();
        s.insert(REC_A, vec![2], 100, 999_999, ONE_GB, ONE_GB, 100, 50)
            .unwrap();
        let n = s.expire_sweep(200).unwrap();
        assert_eq!(n, 1);
        assert_eq!(s.fetch(REC_A, 200).unwrap().len(), 1);
    }

    #[test]
    fn cap_overflow_returns_recipient_full_when_no_evictable_rows() {
        let s = Store::in_memory().unwrap();
        // Two existing non-expired deposits filling the 8-byte cap.
        s.insert(REC_A, vec![1; 4], 100, 999_999, 8, ONE_GB, 100, 50)
            .unwrap();
        s.insert(REC_A, vec![2; 4], 100, 999_999, 8, ONE_GB, 100, 50)
            .unwrap();
        let err = s
            .insert(REC_A, vec![3; 4], 200, 999_999, 8, ONE_GB, 100, 50)
            .expect_err("must reject");
        assert!(matches!(
            err,
            MailboxError::Policy(PolicyErrorKind::RecipientFull)
        ));
    }

    #[test]
    fn cap_overflow_evicts_expired_rows_first() {
        let s = Store::in_memory().unwrap();
        // First deposit expires at 110; cap = 8 bytes.
        s.insert(REC_A, vec![1; 4], 100, 110, 8, ONE_GB, 100, 50)
            .unwrap();
        s.insert(REC_A, vec![2; 4], 100, 999_999, 8, ONE_GB, 100, 50)
            .unwrap();
        // now=200: first row is expired and gets evicted to make room.
        s.insert(REC_A, vec![3; 4], 200, 999_999, 8, ONE_GB, 100, 200)
            .unwrap();
        let rows = s.fetch(REC_A, 250).unwrap();
        assert_eq!(rows.len(), 2);
        // Surviving rows: the second (pending) and the third (just inserted).
    }

    #[test]
    fn storage_bytes_tracks_inserts_and_deletes() {
        let s = Store::in_memory().unwrap();
        assert_eq!(s.storage_bytes().unwrap(), 0);
        let id = s
            .insert(REC_A, vec![1; 100], 100, 999_999, ONE_GB, ONE_GB, 100, 50)
            .unwrap();
        assert_eq!(s.storage_bytes().unwrap(), 100);
        s.delete(REC_A, &[id]).unwrap();
        assert_eq!(s.storage_bytes().unwrap(), 0);
    }

    #[test]
    fn global_cap_rejects_after_evicting_expired() {
        let s = Store::in_memory().unwrap();
        // global cap = 8 bytes; recipient cap huge so only the global cap bites.
        s.insert(REC_A, vec![1; 4], 100, 110, ONE_GB, 8, 100, 50)
            .unwrap();
        s.insert(REC_B, vec![2; 4], 100, 999_999, ONE_GB, 8, 100, 50)
            .unwrap();
        // now=200: REC_A's row is expired → evicted globally to make room.
        s.insert(REC_A, vec![3; 4], 200, 999_999, ONE_GB, 8, 100, 200)
            .unwrap();
        // No expired rows left to evict → ServerFull.
        let err = s
            .insert(REC_A, vec![4; 4], 300, 999_999, ONE_GB, 8, 100, 300)
            .expect_err("must reject");
        assert!(matches!(
            err,
            MailboxError::Policy(PolicyErrorKind::ServerFull)
        ));
    }

    #[test]
    fn recipient_count_cap_rejects_new_recipient() {
        let s = Store::in_memory().unwrap();
        // max_recipients = 1: REC_A allowed; REC_B (new distinct recipient) rejected;
        // a second deposit to EXISTING REC_A still allowed.
        s.insert(REC_A, vec![1], 100, 999_999, ONE_GB, ONE_GB, 1, 50)
            .unwrap();
        let err = s
            .insert(REC_B, vec![2], 100, 999_999, ONE_GB, ONE_GB, 1, 50)
            .expect_err("new recipient must be rejected at the limit");
        assert!(matches!(
            err,
            MailboxError::Policy(PolicyErrorKind::RecipientLimit)
        ));
        s.insert(REC_A, vec![3], 100, 999_999, ONE_GB, ONE_GB, 1, 50)
            .unwrap();
    }
}
