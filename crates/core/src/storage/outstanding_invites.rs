// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Repository for `outstanding_invites` — inviter-side PSK persistence.

use zeroize::Zeroizing;

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
                    &psk.as_ref()[..],
                    inviter_kp,
                    expires_at,
                    created_at,
                ],
            )
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("oi: put: {e}"))))?;
            Ok(())
        })
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
}
