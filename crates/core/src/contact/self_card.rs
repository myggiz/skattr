// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Self-published `ContactCard` helpers used by `RotateOnion`,
//! `AddMailbox`, and `RemoveMailbox`. Reads + bumps the
//! `self_card_state.version` singleton inside one transaction so two
//! concurrent rotations cannot publish the same version.

use crate::contact::ContactCard;
use crate::error::{CoreError, Result};
use crate::identity::IdentityKey;
use crate::storage::{Pool, StorageErrorKind};

/// Default ContactCard TTL — 7 days. Cards published before this
/// elapses are still valid; long-form rotation gives recipients ample
/// window to fetch the new card from a mailbox before it expires.
pub(crate) const DEFAULT_TTL_SECS: u64 = 7 * 24 * 3600;

/// Atomically bump the singleton version and sign a fresh
/// `ContactCard` carrying the supplied onion + mailbox list.
///
/// Concurrency: the increment + read occur inside one `pool.transaction`
/// so concurrent callers always see strictly increasing version numbers.
pub(crate) fn build_next_self_card(
    pool: &Pool,
    signer: &IdentityKey,
    onion: String,
    mailboxes: Vec<String>,
    ttl_secs: u64,
    now: i64,
) -> Result<ContactCard> {
    let version = pool.transaction(|tx| {
        tx.execute(
            "UPDATE self_card_state SET version = version + 1 WHERE id = 1",
            [],
        )
        .map_err(|e| {
            CoreError::Storage(StorageErrorKind::Other(format!(
                "self_card: bump version: {e}"
            )))
        })?;
        let v: i64 = tx
            .query_row(
                "SELECT version FROM self_card_state WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "self_card: read version: {e}"
                )))
            })?;
        Ok(v)
    })?;

    let version_u64 = u64::try_from(version).map_err(|_| {
        CoreError::Storage(StorageErrorKind::Other(
            "self_card: version overflowed u64".into(),
        ))
    })?;

    ContactCard::sign(signer, onion, mailboxes, version_u64, ttl_secs, now)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn first_self_card_starts_at_version_1() {
        let pool = Pool::in_memory();
        let signer = IdentityKey::generate().unwrap();
        let card = build_next_self_card(
            &pool,
            &signer,
            "aaaa.onion".into(),
            vec![],
            3600,
            100,
        )
        .unwrap();
        assert_eq!(card.body.version, 1);
        assert_eq!(card.body.onion, "aaaa.onion");
        assert_eq!(card.body.expires_at, 100 + 3600);
    }

    #[test]
    fn version_monotonically_increases() {
        let pool = Pool::in_memory();
        let signer = IdentityKey::generate().unwrap();
        let v1 = build_next_self_card(&pool, &signer, "a.onion".into(), vec![], 3600, 100)
            .unwrap()
            .body
            .version;
        let v2 = build_next_self_card(&pool, &signer, "b.onion".into(), vec![], 3600, 200)
            .unwrap()
            .body
            .version;
        let v3 = build_next_self_card(&pool, &signer, "c.onion".into(), vec![], 3600, 300)
            .unwrap()
            .body
            .version;
        assert_eq!((v1, v2, v3), (1, 2, 3));
    }

    #[test]
    fn mailboxes_field_round_trips_to_signed_card() {
        let pool = Pool::in_memory();
        let signer = IdentityKey::generate().unwrap();
        let card = build_next_self_card(
            &pool,
            &signer,
            "self.onion".into(),
            vec!["mb1.onion".into(), "mb2.onion".into()],
            DEFAULT_TTL_SECS,
            100,
        )
        .unwrap();
        // Signature verifies + content matches.
        let pk = card.verify(100).unwrap();
        assert_eq!(pk, signer.public());
        assert_eq!(
            card.body.mailboxes,
            vec!["mb1.onion".to_string(), "mb2.onion".to_string()]
        );
    }
}
