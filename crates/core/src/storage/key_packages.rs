// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Repository for MLS KeyPackages.
//!
//! For 1.C every row is `direction = 'ours'` (we generated the KP for a
//! peer to consume). `direction = 'theirs'` lands in Phase 2 when we
//! cache received KPs for out-of-order use. `consumed` is flipped by
//! 1.D's invite flow on successful single-use join; 1.C only persists.

use super::StorageErrorKind;
use crate::error::{CoreError, Result};
use crate::storage::Pool;

/// Repository for MLS KeyPackages.
pub struct KeyPackageRepo<'p> {
    pool: &'p Pool,
}

impl<'p> KeyPackageRepo<'p> {
    /// Construct a repo bound to the given `pool`.
    pub fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Insert a KeyPackage row. `direction` must be `"ours"` or
    /// `"theirs"` (CHECK constraint at the SQL layer).
    pub fn insert(&self, hash: &[u8; 32], bytes: &[u8], direction: &str) -> Result<()> {
        self.pool.with_mut(|c| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            c.execute(
                "INSERT INTO key_packages (kp_hash, kp_bytes, direction, consumed, created_at) \
                 VALUES (?1, ?2, ?3, 0, ?4)",
                rusqlite::params![hash, bytes, direction, now],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("insert key_package: {e}")))
            })?;
            Ok(())
        })
    }

    /// Return `(kp_bytes, consumed)` if the hash is known. `None` otherwise.
    pub fn get(&self, hash: &[u8; 32]) -> Result<Option<(Vec<u8>, bool)>> {
        self.pool.with(|c| {
            let result = c.query_row(
                "SELECT kp_bytes, consumed FROM key_packages WHERE kp_hash = ?1",
                rusqlite::params![hash],
                |r| {
                    let bytes: Vec<u8> = r.get(0)?;
                    let consumed: i64 = r.get(1)?;
                    Ok((bytes, consumed != 0))
                },
            );
            match result {
                Ok(row) => Ok(Some(row)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Storage(StorageErrorKind::Other(format!(
                    "get key_package: {e}"
                )))),
            }
        })
    }

    /// Return `(kp_bytes, consumed)` for `hash` inside a caller-owned
    /// transaction, or `None` if unknown. Used by `add_contact`'s
    /// single-use re-check so the consumed test and the consume write
    /// happen atomically (T2-1 TOCTOU close).
    pub(crate) fn get_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        hash: &[u8; 32],
    ) -> Result<Option<(Vec<u8>, bool)>> {
        let result = tx.query_row(
            "SELECT kp_bytes, consumed FROM key_packages WHERE kp_hash = ?1",
            rusqlite::params![hash],
            |r| {
                let bytes: Vec<u8> = r.get(0)?;
                let consumed: i64 = r.get(1)?;
                Ok((bytes, consumed != 0))
            },
        );
        match result {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(CoreError::Storage(StorageErrorKind::Other(format!(
                "kp: get_in_tx: {e}"
            )))),
        }
    }

    /// Mark a KeyPackage consumed inside a caller-owned transaction.
    /// Used by `dispatch_welcome_inner` to atomically consume the KP
    /// alongside the group-save and outstanding-invite delete.
    pub(crate) fn mark_consumed_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        kp_hash: &[u8; 32],
    ) -> Result<()> {
        tx.execute(
            "UPDATE key_packages SET consumed = 1 WHERE kp_hash = ?1",
            rusqlite::params![&kp_hash[..]],
        )
        .map_err(|e| {
            CoreError::Storage(StorageErrorKind::Other(format!(
                "kp: mark_consumed_in_tx: {e}"
            )))
        })?;
        Ok(())
    }

    /// Mark a KeyPackage consumed. Idempotent.
    pub fn mark_consumed(&self, hash: &[u8; 32]) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "UPDATE key_packages SET consumed = 1 WHERE kp_hash = ?1",
                rusqlite::params![hash],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("mark_consumed: {e}")))
            })?;
            Ok(())
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn insert_get_round_trip() {
        let pool = Pool::in_memory();
        let repo = KeyPackageRepo::new(&pool);
        let hash = [0xAAu8; 32];
        repo.insert(&hash, b"kp-bytes", "ours").unwrap();
        let got = repo.get(&hash).unwrap().unwrap();
        assert_eq!(got.0, b"kp-bytes");
        assert!(!got.1, "freshly inserted KP must not be consumed");
    }

    #[test]
    fn get_missing_returns_none() {
        let pool = Pool::in_memory();
        let repo = KeyPackageRepo::new(&pool);
        assert!(repo.get(&[0x99u8; 32]).unwrap().is_none());
    }

    #[test]
    fn mark_consumed_flips_flag() {
        let pool = Pool::in_memory();
        let repo = KeyPackageRepo::new(&pool);
        let hash = [0xBBu8; 32];
        repo.insert(&hash, b"kp", "ours").unwrap();
        repo.mark_consumed(&hash).unwrap();
        let (_bytes, consumed) = repo.get(&hash).unwrap().unwrap();
        assert!(consumed, "mark_consumed must flip the flag");
    }

    #[test]
    fn mark_consumed_is_idempotent() {
        let pool = Pool::in_memory();
        let repo = KeyPackageRepo::new(&pool);
        let hash = [0xCCu8; 32];
        repo.insert(&hash, b"kp", "ours").unwrap();
        repo.mark_consumed(&hash).unwrap();
        repo.mark_consumed(&hash).unwrap();
        let (_bytes, consumed) = repo.get(&hash).unwrap().unwrap();
        assert!(consumed);
    }

    #[test]
    fn direction_check_constraint_rejects_bogus_value() {
        let pool = Pool::in_memory();
        let repo = KeyPackageRepo::new(&pool);
        let hash = [0xDDu8; 32];
        let err = repo
            .insert(&hash, b"kp", "sideways")
            .expect_err("CHECK (direction IN ('ours', 'theirs')) must reject 'sideways'");
        match err {
            CoreError::Storage(crate::storage::StorageErrorKind::Other(s)) => {
                assert!(s.contains("CHECK"))
            }
            other => panic!("expected Storage(Other), got {other:?}"),
        }
    }
}
