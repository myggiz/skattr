// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Repository for `contacts` and `onion_addresses` tables.

use crate::contact::Contact;
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
