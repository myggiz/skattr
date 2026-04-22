// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Repository for `contacts` and `onion_addresses` tables.

use crate::contact::Contact;
use crate::contact::ContactCard;
use crate::error::{CoreError, Result};
use crate::identity::PublicKey;
use crate::storage::Pool;

/// Contact CRUD operations, plus onion-address history for each contact.
pub struct ContactRepo<'p> {
    pool: &'p Pool,
}

impl<'p> ContactRepo<'p> {
    /// Construct a new `ContactRepo` backed by `pool`.
    pub fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Upsert a contact (add if new, update display name if existing).
    pub fn upsert(&self, contact: &Contact) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "INSERT INTO contacts (identity_pubkey, display_name, added_at) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(identity_pubkey) DO UPDATE SET display_name=excluded.display_name",
                rusqlite::params![
                    &contact.identity.0[..],
                    &contact.display_name,
                    contact.added_at,
                ],
            )
            .map_err(|e| CoreError::Storage(format!("upsert contact: {e}")))?;
            Ok(())
        })
    }

    /// Look up by identity pubkey. Returns `Ok(None)` if not present.
    ///
    /// Note: the `card` field is NOT loaded here — ContactCards are
    /// stored separately (Phase 1 wiring). For Phase 0.D we return
    /// contact metadata only with `card: None`.
    pub fn get(&self, identity: &PublicKey) -> Result<Option<Contact>> {
        self.pool.with(|c| {
            let result = c.query_row(
                "SELECT display_name, added_at FROM contacts WHERE identity_pubkey = ?1",
                rusqlite::params![&identity.0[..]],
                |r| {
                    Ok(Contact {
                        identity: *identity,
                        display_name: r.get(0)?,
                        added_at: r.get(1)?,
                        card: None,
                    })
                },
            );
            match result {
                Ok(contact) => Ok(Some(contact)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Storage(format!("get contact: {e}"))),
            }
        })
    }

    /// Enumerate all contacts, alphabetical by display name (nulls last).
    pub(crate) fn list(&self) -> Result<Vec<Contact>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT identity_pubkey, display_name, added_at FROM contacts \
                     ORDER BY display_name IS NULL, display_name COLLATE NOCASE",
                )
                .map_err(|e| CoreError::Storage(format!("prepare list contacts: {e}")))?;
            let rows = stmt
                .query_map([], |r| {
                    let pub_bytes: Vec<u8> = r.get(0)?;
                    let mut arr = [0u8; 32];
                    if pub_bytes.len() == 32 {
                        arr.copy_from_slice(&pub_bytes);
                    }
                    Ok(Contact {
                        identity: PublicKey(arr),
                        display_name: r.get(1)?,
                        added_at: r.get(2)?,
                        card: None,
                    })
                })
                .map_err(|e| CoreError::Storage(format!("query list contacts: {e}")))?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| CoreError::Storage(format!("collect contacts: {e}")))
        })
    }

    /// Delete by identity pubkey. `onion_addresses` rows cascade via FK.
    pub(crate) fn remove(&self, identity: &PublicKey) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "DELETE FROM contacts WHERE identity_pubkey = ?1",
                rusqlite::params![&identity.0[..]],
            )
            .map_err(|e| CoreError::Storage(format!("delete contact: {e}")))?;
            Ok(())
        })
    }

    /// Record a new onion address for a contact. Does NOT mark the old
    /// one stale — that's an explicit call to `mark_current`.
    pub(crate) fn add_onion(
        &self,
        identity: &PublicKey,
        address: &str,
        seen_at: i64,
    ) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "INSERT INTO onion_addresses (contact_id, address, seen_at, is_current) \
                 VALUES ((SELECT id FROM contacts WHERE identity_pubkey = ?1), ?2, ?3, 1)",
                rusqlite::params![&identity.0[..], address, seen_at],
            )
            .map_err(|e| CoreError::Storage(format!("add onion: {e}")))?;
            Ok(())
        })
    }

    /// Mark a specific onion address as the current one and demote
    /// others for the same contact. Use when a rotation arrives.
    pub(crate) fn mark_current(&self, identity: &PublicKey, address: &str) -> Result<()> {
        self.pool.transaction(|tx| {
            tx.execute(
                "UPDATE onion_addresses SET is_current = 0 \
                 WHERE contact_id = (SELECT id FROM contacts WHERE identity_pubkey = ?1)",
                rusqlite::params![&identity.0[..]],
            )
            .map_err(|e| CoreError::Storage(format!("demote old onions: {e}")))?;
            tx.execute(
                "UPDATE onion_addresses SET is_current = 1 \
                 WHERE contact_id = (SELECT id FROM contacts WHERE identity_pubkey = ?1) \
                 AND address = ?2",
                rusqlite::params![&identity.0[..], address],
            )
            .map_err(|e| CoreError::Storage(format!("promote new onion: {e}")))?;
            Ok(())
        })
    }

    /// Return the current onion address for a contact, if any.
    pub(crate) fn current_onion(&self, identity: &PublicKey) -> Result<Option<String>> {
        self.pool.with(|c| {
            let result = c.query_row(
                "SELECT address FROM onion_addresses \
                 WHERE contact_id = (SELECT id FROM contacts WHERE identity_pubkey = ?1) \
                 AND is_current = 1 \
                 ORDER BY seen_at DESC LIMIT 1",
                rusqlite::params![&identity.0[..]],
                |r| r.get::<_, String>(0),
            );
            match result {
                Ok(s) => Ok(Some(s)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Storage(format!("current_onion: {e}"))),
            }
        })
    }

    /// Insert a freshly-verified `ContactCard`. Rejects with
    /// `CoreError::Contact("contact: card: stale version")` if the
    /// version is not strictly greater than the latest stored card's
    /// version for the same identity. Rejects with
    /// `CoreError::Contact("contact: card: contact not found")` if
    /// the contact row doesn't exist.
    pub fn put_card(&self, card: &ContactCard) -> Result<()> {
        let identity_bytes = card.body.identity.0;
        let version_i: i64 = i64::try_from(card.body.version)
            .map_err(|_| CoreError::Contact("contact: card: version overflows i64".into()))?;

        let mut blob = Vec::new();
        ciborium::ser::into_writer(card, &mut blob)
            .map_err(|e| CoreError::Contact(format!("contact: card: cbor encode: {e}")))?;

        let verified_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        self.pool.transaction(|tx| {
            // Resolve the contact's row id. If missing, fail with the
            // fixed message before touching contact_cards.
            let contact_id: i64 = match tx.query_row(
                "SELECT id FROM contacts WHERE identity_pubkey = ?1",
                rusqlite::params![&identity_bytes[..]],
                |r| r.get(0),
            ) {
                Ok(id) => id,
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    return Err(CoreError::Contact(
                        "contact: card: contact not found".into(),
                    ));
                }
                Err(e) => return Err(CoreError::Contact(format!("contact: card: lookup: {e}"))),
            };

            // Compare against the stored max version; reject if not strictly greater.
            let max_version: Option<i64> = tx
                .query_row(
                    "SELECT MAX(version) FROM contact_cards WHERE contact_id = ?1",
                    rusqlite::params![contact_id],
                    |r| r.get::<_, Option<i64>>(0),
                )
                .map_err(|e| {
                    CoreError::Contact(format!("contact: card: max-version lookup: {e}"))
                })?;

            if let Some(max_v) = max_version {
                if version_i <= max_v {
                    return Err(CoreError::Contact("contact: card: stale version".into()));
                }
            }

            tx.execute(
                "INSERT INTO contact_cards (contact_id, version, card_blob, verified_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![contact_id, version_i, &blob, verified_at],
            )
            .map_err(|e| CoreError::Contact(format!("contact: card: insert: {e}")))?;
            Ok(())
        })
    }

    /// Return the highest-version `ContactCard` for `identity`, or
    /// `None` if no card exists (or the contact is unknown).
    pub fn latest_card(&self, identity: &PublicKey) -> Result<Option<ContactCard>> {
        self.pool.with(|c| {
            let result = c.query_row(
                "SELECT cc.card_blob FROM contact_cards cc \
                 JOIN contacts k ON cc.contact_id = k.id \
                 WHERE k.identity_pubkey = ?1 \
                 ORDER BY cc.version DESC LIMIT 1",
                rusqlite::params![&identity.0[..]],
                |r| r.get::<_, Vec<u8>>(0),
            );
            match result {
                Ok(blob) => {
                    let card: ContactCard = ciborium::de::from_reader(&blob[..]).map_err(|e| {
                        CoreError::Contact(format!("contact: card: cbor decode: {e}"))
                    })?;
                    Ok(Some(card))
                }
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Contact(format!("contact: card: latest: {e}"))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_contact(seed: u8) -> Contact {
        Contact {
            identity: PublicKey([seed; 32]),
            display_name: Some(format!("Alice-{seed}")),
            added_at: 1_700_000_000 + i64::from(seed),
            card: None,
        }
    }

    #[test]
    fn upsert_get_roundtrip() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let alice = sample_contact(1);

        repo.upsert(&alice).unwrap();
        let got = repo.get(&alice.identity).unwrap().unwrap();
        assert_eq!(got.display_name, alice.display_name);
        assert_eq!(got.added_at, alice.added_at);
    }

    #[test]
    fn get_missing_returns_none() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        assert!(repo.get(&PublicKey([0x99; 32])).unwrap().is_none());
    }

    #[test]
    fn upsert_updates_display_name_on_conflict() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let mut alice = sample_contact(2);
        repo.upsert(&alice).unwrap();
        alice.display_name = Some("Alice-renamed".into());
        repo.upsert(&alice).unwrap();
        let got = repo.get(&alice.identity).unwrap().unwrap();
        assert_eq!(got.display_name, Some("Alice-renamed".into()));
    }

    #[test]
    fn list_returns_all_contacts_sorted() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        repo.upsert(&sample_contact(3)).unwrap();
        repo.upsert(&sample_contact(1)).unwrap();
        repo.upsert(&sample_contact(2)).unwrap();
        let all = repo.list().unwrap();
        assert_eq!(all.len(), 3);
        // Sorted alphabetically: Alice-1, Alice-2, Alice-3.
        assert_eq!(all[0].display_name, Some("Alice-1".into()));
        assert_eq!(all[1].display_name, Some("Alice-2".into()));
        assert_eq!(all[2].display_name, Some("Alice-3".into()));
    }

    #[test]
    fn remove_deletes_contact_and_cascades_onions() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let alice = sample_contact(4);
        repo.upsert(&alice).unwrap();
        repo.add_onion(&alice.identity, "aaaa.onion", 100).unwrap();
        repo.remove(&alice.identity).unwrap();
        assert!(repo.get(&alice.identity).unwrap().is_none());
        // Onion rows cascaded.
        let count: i64 = pool
            .with(|c| {
                c.query_row("SELECT COUNT(*) FROM onion_addresses", [], |r| r.get(0))
                    .map_err(|e| CoreError::Storage(e.to_string()))
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    use crate::contact::card::{ContactCard, ContactCardBody};
    use crate::identity::Signature;

    fn sample_card(seed: u8, version: u64) -> ContactCard {
        ContactCard {
            body: ContactCardBody {
                identity: PublicKey([seed; 32]),
                onion: format!("onion-{seed}.onion"),
                mailboxes: Vec::new(),
                version,
                expires_at: 1_700_000_000 + i64::from(seed),
            },
            // The signature bytes are irrelevant for storage tests —
            // put_card / latest_card don't re-verify (that's the
            // caller's job via ContactCard::verify before put_card).
            signature: Signature([0u8; 64]),
        }
    }

    #[test]
    fn put_card_then_latest_card_round_trip() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let contact = sample_contact(10);
        repo.upsert(&contact).unwrap();

        let card = sample_card(10, 1);
        repo.put_card(&card).unwrap();

        let got = repo.latest_card(&contact.identity).unwrap().unwrap();
        assert_eq!(got.body.version, 1);
        assert_eq!(got.body.onion, "onion-10.onion");
    }

    #[test]
    fn put_card_rejects_stale_version() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let contact = sample_contact(11);
        repo.upsert(&contact).unwrap();

        repo.put_card(&sample_card(11, 5)).unwrap();
        let err = repo
            .put_card(&sample_card(11, 5))
            .expect_err("same version");
        assert!(
            matches!(err, CoreError::Contact(ref s) if s.contains("stale version")),
            "got: {err:?}"
        );

        let err = repo
            .put_card(&sample_card(11, 4))
            .expect_err("older version");
        assert!(
            matches!(err, CoreError::Contact(ref s) if s.contains("stale version")),
            "got: {err:?}"
        );
    }

    #[test]
    fn put_card_rejects_when_contact_absent() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        // No upsert — the contact row isn't there.
        let err = repo.put_card(&sample_card(12, 1)).expect_err("no contact");
        assert!(
            matches!(err, CoreError::Contact(ref s) if s.contains("contact not found")),
            "got: {err:?}"
        );
    }

    #[test]
    fn latest_card_missing_contact_returns_none() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        assert!(repo.latest_card(&PublicKey([0x99; 32])).unwrap().is_none());
    }

    #[test]
    fn put_card_accepts_higher_version() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let contact = sample_contact(13);
        repo.upsert(&contact).unwrap();

        repo.put_card(&sample_card(13, 1)).unwrap();
        repo.put_card(&sample_card(13, 2)).unwrap();
        repo.put_card(&sample_card(13, 10)).unwrap();

        let latest = repo.latest_card(&contact.identity).unwrap().unwrap();
        assert_eq!(latest.body.version, 10);
    }

    #[test]
    fn cascade_delete_removes_cards() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let contact = sample_contact(14);
        repo.upsert(&contact).unwrap();
        repo.put_card(&sample_card(14, 1)).unwrap();
        repo.put_card(&sample_card(14, 2)).unwrap();

        repo.remove(&contact.identity).unwrap();

        let count: i64 = pool
            .with(|c| {
                c.query_row("SELECT COUNT(*) FROM contact_cards", [], |r| r.get(0))
                    .map_err(|e| CoreError::Storage(e.to_string()))
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn onion_rotation_flow() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let alice = sample_contact(5);
        repo.upsert(&alice).unwrap();
        repo.add_onion(&alice.identity, "aaaa.onion", 100).unwrap();
        repo.add_onion(&alice.identity, "bbbb.onion", 200).unwrap();
        // Both are is_current=1 from add_onion — that's intentional;
        // mark_current demotes siblings.
        repo.mark_current(&alice.identity, "bbbb.onion").unwrap();
        assert_eq!(
            repo.current_onion(&alice.identity).unwrap(),
            Some("bbbb.onion".into())
        );
    }
}
