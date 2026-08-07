// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Durable storage for first-contact Ack binding (#93).
//!
//! One row per consumed first-contact invite whose Welcome we have sent.
//! Maps KeyPackageRef (the join proof) to peer identity and x25519 key,
//! enabling idempotent Welcome re-sends on duplicate join attempts.

use crate::error::{CoreError, Result};
use crate::storage::{pool::Pool, StorageErrorKind};

/// A first-contact Ack record: the peer identity and x25519 key bound to a consumed invite.
pub struct FirstContactAck {
    pub peer_x25519: [u8; 32],
    pub peer_identity: [u8; 32],
}

/// Repository for the `first_contact_acks` table.
pub struct FirstContactAckRepo<'p> {
    pool: &'p Pool,
}

impl<'p> FirstContactAckRepo<'p> {
    /// Bind a first-contact-ack repository to `pool`.
    pub fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Insert a new first-contact-ack row inside an existing transaction.
    /// Idempotent on `kp_ref` via `INSERT OR IGNORE`.
    pub fn insert_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        kp_ref: &[u8; 32],
        peer_x25519: &[u8; 32],
        peer_identity: &[u8; 32],
        now_ms: i64,
    ) -> Result<()> {
        tx.execute(
            "INSERT OR IGNORE INTO first_contact_acks \
             (kp_ref, peer_x25519, peer_identity, created_at) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![&kp_ref[..], &peer_x25519[..], &peer_identity[..], now_ms],
        )
        .map_err(|e| {
            CoreError::Storage(StorageErrorKind::Other(format!(
                "first_contact_acks insert_in_tx: {e}"
            )))
        })?;
        Ok(())
    }

    /// Delete all first-contact-ack rows for `peer_identity` inside the
    /// caller's transaction. Idempotent — no error if no rows match.
    pub(crate) fn delete_by_peer_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        peer_identity: &[u8; 32],
    ) -> Result<()> {
        tx.execute(
            "DELETE FROM first_contact_acks WHERE peer_identity = ?1",
            rusqlite::params![&peer_identity[..]],
        )
        .map_err(|e| {
            CoreError::Storage(StorageErrorKind::Other(format!(
                "delete first_contact_ack: {e}"
            )))
        })?;
        Ok(())
    }

    /// Look up a first-contact Ack by KeyPackageRef.
    pub fn lookup(&self, kp_ref: &[u8; 32]) -> Result<Option<FirstContactAck>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT peer_x25519, peer_identity \
                     FROM first_contact_acks \
                     WHERE kp_ref = ?1",
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "first_contact_acks lookup prepare: {e}"
                    )))
                })?;
            let mut rows = stmt.query(rusqlite::params![&kp_ref[..]]).map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "first_contact_acks lookup query: {e}"
                )))
            })?;
            if let Some(r) = rows.next().map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "first_contact_acks lookup row: {e}"
                )))
            })? {
                let peer_x25519_bytes: Vec<u8> = r.get(0).map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "first_contact_acks lookup peer_x25519 get: {e}"
                    )))
                })?;
                let peer_identity_bytes: Vec<u8> = r.get(1).map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "first_contact_acks lookup peer_identity get: {e}"
                    )))
                })?;

                let peer_x25519: [u8; 32] = peer_x25519_bytes.try_into().map_err(|_| {
                    CoreError::Storage(StorageErrorKind::Other(
                        "first_contact_acks lookup: peer_x25519 wrong length".to_string(),
                    ))
                })?;
                let peer_identity: [u8; 32] = peer_identity_bytes.try_into().map_err(|_| {
                    CoreError::Storage(StorageErrorKind::Other(
                        "first_contact_acks lookup: peer_identity wrong length".to_string(),
                    ))
                })?;

                Ok(Some(FirstContactAck {
                    peer_x25519,
                    peer_identity,
                }))
            } else {
                Ok(None)
            }
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
    fn insert_then_lookup_roundtrips_and_is_idempotent() {
        let p = pool();
        let repo = FirstContactAckRepo::new(&p);
        let kp = [7u8; 32];
        let x = [8u8; 32];
        let id = [9u8; 32];
        p.transaction(|tx| {
            repo.insert_in_tx(tx, &kp, &x, &id, 1_000).unwrap();
            repo.insert_in_tx(tx, &kp, &x, &id, 2_000).unwrap(); // OR IGNORE: no error
            Ok(())
        })
        .unwrap();
        let got = repo.lookup(&kp).unwrap().unwrap();
        assert_eq!(got.peer_x25519, x);
        assert_eq!(got.peer_identity, id);
        assert!(repo.lookup(&[0u8; 32]).unwrap().is_none());
    }

    #[test]
    fn delete_by_peer_in_tx_removes_ack_row() {
        let p = pool();
        let repo = FirstContactAckRepo::new(&p);
        let kp = [0x11u8; 32];
        let x = [0x22u8; 32];
        let id = [0x33u8; 32];
        p.transaction(|tx| {
            repo.insert_in_tx(tx, &kp, &x, &id, 1_000).unwrap();
            Ok(())
        })
        .unwrap();
        assert!(repo.lookup(&kp).unwrap().is_some());
        p.transaction(|tx| repo.delete_by_peer_in_tx(tx, &id))
            .unwrap();
        assert!(repo.lookup(&kp).unwrap().is_none());
    }
}
