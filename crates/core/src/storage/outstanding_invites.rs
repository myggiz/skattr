// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Repository for `outstanding_invites` — inviter-side PSK persistence.

use zeroize::Zeroizing;

/// Tuple returned by `get_psk_and_kp_bytes`: PSK, inviter KP TLS bytes,
/// optional MlsProvider snapshot, expires_at unix seconds.
type OutstandingInviteRow = (Zeroizing<[u8; 32]>, Vec<u8>, Option<Vec<u8>>, i64);

use super::StorageErrorKind;
use crate::error::{CoreError, Result};
use crate::storage::Pool;

/// Inviter-side persistence of `(kp_hash, psk, expires_at)` so that the
/// inviter can reconstruct the PSK at Welcome-receive time.
pub struct OutstandingInviteRepo<'p> {
    pool: &'p Pool,
}

impl<'p> OutstandingInviteRepo<'p> {
    /// Construct a new repo bound to `pool`.
    pub fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Insert a row for a freshly-generated invite. Idempotent on
    /// `kp_hash` collision (overwrite is intentional — `CreateInvite`
    /// regenerates a fresh KP each time so collisions only occur on
    /// retries of the same operation).
    ///
    /// `provider_snapshot` is the ciborium-serialized `MlsProvider` state
    /// that holds the init private key for `inviter_kp`; it is required by
    /// `dispatch_welcome_inner` to process the incoming Welcome.
    pub fn put(
        &self,
        kp_hash: &[u8; 32],
        psk: &Zeroizing<[u8; 32]>,
        inviter_kp: &[u8],
        expires_at: i64,
        created_at: i64,
    ) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "INSERT OR REPLACE INTO outstanding_invites \
                 (kp_hash, psk, inviter_kp, expires_at, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    &kp_hash[..],
                    psk.as_ref(),
                    inviter_kp,
                    expires_at,
                    created_at,
                ],
            )
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("oi: put: {e}"))))?;
            Ok(())
        })
    }

    /// Like `put` but also stores the MLS provider snapshot so it can be
    /// restored when processing the incoming Welcome.
    pub fn put_with_provider(
        &self,
        kp_hash: &[u8; 32],
        psk: &Zeroizing<[u8; 32]>,
        inviter_kp: &[u8],
        provider_snapshot: &[u8],
        expires_at: i64,
        created_at: i64,
    ) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "INSERT OR REPLACE INTO outstanding_invites \
                 (kp_hash, psk, inviter_kp, provider_snapshot, expires_at, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    &kp_hash[..],
                    psk.as_ref(),
                    inviter_kp,
                    provider_snapshot,
                    expires_at,
                    created_at,
                ],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "oi: put_with_provider: {e}"
                )))
            })?;
            Ok(())
        })
    }

    /// Zeroize the PSK column then delete the row in one transaction.
    /// Calling on a missing row succeeds silently (idempotent).
    pub fn mark_consumed(&self, kp_hash: &[u8; 32]) -> Result<()> {
        self.pool.transaction(|tx| {
            tx.execute(
                "UPDATE outstanding_invites SET psk = zeroblob(32) WHERE kp_hash = ?1",
                rusqlite::params![&kp_hash[..]],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("oi: zeroize: {e}")))
            })?;
            tx.execute(
                "DELETE FROM outstanding_invites WHERE kp_hash = ?1",
                rusqlite::params![&kp_hash[..]],
            )
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("oi: delete: {e}"))))?;
            Ok(())
        })
    }

    /// Zeroize-then-delete every row whose `expires_at < now`.
    /// Returns the number of rows deleted.
    pub fn purge_expired(&self, now: i64) -> Result<u64> {
        self.pool.transaction(|tx| {
            tx.execute(
                "UPDATE outstanding_invites SET psk = zeroblob(32) WHERE expires_at < ?1",
                rusqlite::params![now],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("oi: purge zeroize: {e}")))
            })?;
            let deleted = tx
                .execute(
                    "DELETE FROM outstanding_invites WHERE expires_at < ?1",
                    rusqlite::params![now],
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("oi: purge delete: {e}")))
                })?;
            Ok(deleted as u64)
        })
    }

    /// Like `get_psk` but also returns the `inviter_kp` bytes and optional
    /// provider snapshot so the caller can:
    ///  - reconstruct the `KeyPackage` and derive its SHA-256 for `key_packages.mark_consumed_in_tx`
    ///  - restore the `MlsProvider` (with the init private key) to process the Welcome
    pub fn get_psk_and_kp_bytes(&self, kp_ref: &[u8; 32]) -> Result<Option<OutstandingInviteRow>> {
        self.pool.with(|c| {
            let result = c.query_row(
                "SELECT psk, inviter_kp, provider_snapshot, expires_at \
                 FROM outstanding_invites WHERE kp_hash = ?1",
                rusqlite::params![&kp_ref[..]],
                |r| {
                    let psk_bytes: Vec<u8> = r.get(0)?;
                    let kp_bytes: Vec<u8> = r.get(1)?;
                    let provider_snap: Option<Vec<u8>> = r.get(2)?;
                    let expires_at: i64 = r.get(3)?;
                    Ok((psk_bytes, kp_bytes, provider_snap, expires_at))
                },
            );
            match result {
                Ok((psk_bytes, kp_bytes, provider_snap, expires_at)) => {
                    if psk_bytes.len() != 32 {
                        return Err(CoreError::Storage(StorageErrorKind::Other(format!(
                            "oi: psk wrong length: {}",
                            psk_bytes.len()
                        ))));
                    }
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&psk_bytes);
                    Ok(Some((
                        Zeroizing::new(arr),
                        kp_bytes,
                        provider_snap,
                        expires_at,
                    )))
                }
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Storage(StorageErrorKind::Other(format!(
                    "oi: get_with_kp: {e}"
                )))),
            }
        })
    }

    /// Zeroize the PSK column then delete the row inside a caller-owned
    /// transaction. Used by `dispatch_welcome_inner` to consume the row
    /// atomically alongside the group-save and contact upsert.
    pub(crate) fn mark_consumed_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        kp_ref: &[u8; 32],
    ) -> Result<()> {
        tx.execute(
            "UPDATE outstanding_invites SET psk = zeroblob(32) WHERE kp_hash = ?1",
            rusqlite::params![&kp_ref[..]],
        )
        .map_err(|e| {
            CoreError::Storage(StorageErrorKind::Other(format!("oi: zeroize_in_tx: {e}")))
        })?;
        tx.execute(
            "DELETE FROM outstanding_invites WHERE kp_hash = ?1",
            rusqlite::params![&kp_ref[..]],
        )
        .map_err(|e| {
            CoreError::Storage(StorageErrorKind::Other(format!("oi: delete_in_tx: {e}")))
        })?;
        Ok(())
    }

    /// Look up the PSK + expires_at for `kp_hash`. Returns `Ok(None)`
    /// if the row is absent or has been consumed.
    pub fn get_psk(&self, kp_hash: &[u8; 32]) -> Result<Option<(Zeroizing<[u8; 32]>, i64)>> {
        self.pool.with(|c| {
            let result = c.query_row(
                "SELECT psk, expires_at FROM outstanding_invites WHERE kp_hash = ?1",
                rusqlite::params![&kp_hash[..]],
                |r| {
                    let psk_bytes: Vec<u8> = r.get(0)?;
                    let expires_at: i64 = r.get(1)?;
                    Ok((psk_bytes, expires_at))
                },
            );
            match result {
                Ok((psk_bytes, expires_at)) => {
                    if psk_bytes.len() != 32 {
                        return Err(CoreError::Storage(StorageErrorKind::Other(format!(
                            "oi: psk wrong length: {}",
                            psk_bytes.len()
                        ))));
                    }
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&psk_bytes);
                    Ok(Some((Zeroizing::new(arr), expires_at)))
                }
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Storage(StorageErrorKind::Other(format!(
                    "oi: get: {e}"
                )))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_roundtrip() {
        let pool = Pool::in_memory();
        let repo = OutstandingInviteRepo::new(&pool);

        let kp_hash = [0xAAu8; 32];
        let psk = Zeroizing::new([0xBBu8; 32]);
        let inviter_kp = vec![0xCCu8; 64];
        let expires_at = 1_700_010_000;
        let created_at = 1_700_000_000;

        repo.put(&kp_hash, &psk, &inviter_kp, expires_at, created_at)
            .unwrap();

        let (got_psk, got_exp) = repo.get_psk(&kp_hash).unwrap().unwrap();
        assert_eq!(*got_psk.as_ref(), [0xBBu8; 32]);
        assert_eq!(got_exp, expires_at);
    }

    #[test]
    fn get_psk_returns_none_for_missing_row() {
        let pool = Pool::in_memory();
        let repo = OutstandingInviteRepo::new(&pool);
        let kp_hash = [0xDDu8; 32];
        assert!(repo.get_psk(&kp_hash).unwrap().is_none());
    }

    #[test]
    fn mark_consumed_zeroizes_then_deletes() {
        let pool = Pool::in_memory();
        let repo = OutstandingInviteRepo::new(&pool);
        let kp_hash = [0x01u8; 32];
        let psk = Zeroizing::new([0xEEu8; 32]);
        repo.put(&kp_hash, &psk, &[], 0, 0).unwrap();

        repo.mark_consumed(&kp_hash).unwrap();

        // Row is gone.
        assert!(repo.get_psk(&kp_hash).unwrap().is_none());
    }

    #[test]
    fn mark_consumed_is_idempotent_on_missing_row() {
        let pool = Pool::in_memory();
        let repo = OutstandingInviteRepo::new(&pool);
        // No put — mark_consumed must succeed silently.
        repo.mark_consumed(&[0x02u8; 32]).unwrap();
    }

    #[test]
    fn purge_expired_removes_only_expired_rows() {
        let pool = Pool::in_memory();
        let repo = OutstandingInviteRepo::new(&pool);
        let now = 1_700_000_000;

        let kp1 = [0x10u8; 32]; // expired
        let kp2 = [0x20u8; 32]; // still valid
        repo.put(&kp1, &Zeroizing::new([0u8; 32]), &[], now - 1, 0)
            .unwrap();
        repo.put(&kp2, &Zeroizing::new([0u8; 32]), &[], now + 3600, 0)
            .unwrap();

        let purged = repo.purge_expired(now).unwrap();
        assert_eq!(purged, 1);

        assert!(repo.get_psk(&kp1).unwrap().is_none());
        assert!(repo.get_psk(&kp2).unwrap().is_some());
    }

    #[test]
    fn purge_expired_returns_zero_when_no_rows_expired() {
        let pool = Pool::in_memory();
        let repo = OutstandingInviteRepo::new(&pool);
        let now = 1_700_000_000;
        repo.put(
            &[0x30u8; 32],
            &Zeroizing::new([0u8; 32]),
            &[],
            now + 3600,
            0,
        )
        .unwrap();
        assert_eq!(repo.purge_expired(now).unwrap(), 0);
    }
}
