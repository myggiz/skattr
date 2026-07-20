// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Repository for `contacts` and `onion_addresses` tables.

use super::StorageErrorKind;
use crate::contact::Contact;
use crate::contact::ContactCard;
use crate::contact::ContactErrorKind;
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
                // hidden=0 on conflict: re-adding a previously-removed
                // (soft-deleted) contact must un-hide it.
                "INSERT INTO contacts (identity_pubkey, display_name, added_at) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(identity_pubkey) DO UPDATE SET display_name=excluded.display_name, hidden=0",
                rusqlite::params![
                    &contact.identity.0[..],
                    &contact.display_name,
                    contact.added_at,
                ],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("upsert contact: {e}")))
            })?;
            Ok(())
        })
    }

    /// Transactional companion to [`upsert`](Self::upsert). Inserts (or
    /// updates the display name of) a contact inside the caller's `tx` so it
    /// commits atomically with the card / genesis-group / invite-consume writes
    /// of first-contact `add_contact` (full atomicity, T2-1).
    pub(crate) fn upsert_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        contact: &Contact,
    ) -> Result<()> {
        tx.execute(
            // hidden=0 on conflict: re-adding a previously-removed (soft-deleted)
            // contact must un-hide it, or it stays invisible in the UI.
            "INSERT INTO contacts (identity_pubkey, display_name, added_at) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT(identity_pubkey) DO UPDATE SET display_name=excluded.display_name, hidden=0",
            rusqlite::params![
                &contact.identity.0[..],
                &contact.display_name,
                contact.added_at,
            ],
        )
        .map_err(|e| {
            CoreError::Storage(StorageErrorKind::Other(format!("upsert contact (tx): {e}")))
        })?;
        Ok(())
    }

    /// Look up by identity pubkey. Returns `Ok(None)` if not present.
    ///
    /// Hydrates the contact's latest card (or `None` if no card exists).
    pub fn get(&self, identity: &PublicKey) -> Result<Option<Contact>> {
        let base = self.pool.with(|c| {
            let result = c.query_row(
                "SELECT display_name, added_at, muted FROM contacts WHERE identity_pubkey = ?1",
                rusqlite::params![&identity.0[..]],
                |r| {
                    let muted_val: i64 = r.get(2).unwrap_or(0);
                    Ok(Contact {
                        identity: *identity,
                        display_name: r.get(0)?,
                        added_at: r.get(1)?,
                        card: None,
                        muted: muted_val != 0,
                    })
                },
            );
            match result {
                Ok(contact) => Ok(Some(contact)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Storage(StorageErrorKind::Other(format!(
                    "get contact: {e}"
                )))),
            }
        })?;
        let Some(mut contact) = base else {
            return Ok(None);
        };
        contact.card = self.latest_card(identity)?;
        Ok(Some(contact))
    }

    /// Enumerate all contacts, alphabetical by display name (nulls last).
    ///
    /// Hydrates each contact's latest card (or `None` if no card exists).
    pub(crate) fn list(&self) -> Result<Vec<Contact>> {
        let mut contacts: Vec<Contact> = self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT identity_pubkey, display_name, added_at, muted FROM contacts \
                     WHERE hidden = 0 \
                     ORDER BY display_name IS NULL, display_name COLLATE NOCASE",
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "prepare list contacts: {e}"
                    )))
                })?;
            let rows = stmt
                .query_map([], |r| {
                    let pub_bytes: Vec<u8> = r.get(0)?;
                    let mut arr = [0u8; 32];
                    if pub_bytes.len() == 32 {
                        arr.copy_from_slice(&pub_bytes);
                    }
                    let muted_val: i64 = r.get(3).unwrap_or(0);
                    Ok(Contact {
                        identity: PublicKey(arr),
                        display_name: r.get(1)?,
                        added_at: r.get(2)?,
                        card: None,
                        muted: muted_val != 0,
                    })
                })
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("query list contacts: {e}")))
                })?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("collect contacts: {e}")))
            })
        })?;

        // Hydrate each contact's latest_card. For small contact lists
        // (our expected scale: dozens), an N+1 query is fine. Phase 2
        // can batch via JOIN if needed.
        for contact in &mut contacts {
            contact.card = self.latest_card(&contact.identity)?;
        }
        Ok(contacts)
    }

    /// Transactional companion to [`remove`](Self::remove). Deletes by
    /// identity pubkey inside the caller's `tx`; `onion_addresses` rows
    /// cascade via the FK.
    pub(crate) fn remove_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        identity: &PublicKey,
    ) -> Result<()> {
        tx.execute(
            "DELETE FROM contacts WHERE identity_pubkey = ?1",
            rusqlite::params![&identity.0[..]],
        )
        .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("delete contact: {e}"))))?;
        Ok(())
    }

    /// Delete by identity pubkey. `onion_addresses` rows cascade via FK.
    pub(crate) fn remove(&self, identity: &PublicKey) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "DELETE FROM contacts WHERE identity_pubkey = ?1",
                rusqlite::params![&identity.0[..]],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("delete contact: {e}")))
            })?;
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
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("add onion: {e}"))))?;
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
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("demote old onions: {e}")))
            })?;
            tx.execute(
                "UPDATE onion_addresses SET is_current = 1 \
                 WHERE contact_id = (SELECT id FROM contacts WHERE identity_pubkey = ?1) \
                 AND address = ?2",
                rusqlite::params![&identity.0[..], address],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("promote new onion: {e}")))
            })?;
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
                Err(e) => Err(CoreError::Storage(StorageErrorKind::Other(format!(
                    "current_onion: {e}"
                )))),
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
        self.pool
            .transaction(|tx| Self::put_card_in_tx_inner(tx, card))
    }

    /// Transactional companion to [`put_card`](Self::put_card). Inserts a
    /// freshly-verified card inside the caller's `tx` so it commits atomically
    /// with the contact upsert / genesis-group save / invite consume of
    /// first-contact `add_contact` (full atomicity, T2-1). Keeps the same
    /// monotonic-version guard and "contact not found" rejection as `put_card`.
    pub(crate) fn put_card_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        card: &ContactCard,
    ) -> Result<()> {
        Self::put_card_in_tx_inner(tx, card)
    }

    /// Shared body for [`put_card`] and [`put_card_in_tx`]: resolve the contact
    /// row, enforce the strictly-increasing version guard, and insert the card
    /// blob. Runs entirely inside the caller's `tx`.
    fn put_card_in_tx_inner(tx: &rusqlite::Transaction<'_>, card: &ContactCard) -> Result<()> {
        let identity_bytes = card.body.identity.0;
        let version_i: i64 = i64::try_from(card.body.version).map_err(|_| {
            CoreError::Contact(ContactErrorKind::Other(
                "card: version overflows i64".into(),
            ))
        })?;

        let mut blob = Vec::new();
        ciborium::ser::into_writer(card, &mut blob).map_err(|e| {
            CoreError::Contact(ContactErrorKind::Other(format!("card: cbor encode: {e}")))
        })?;

        let verified_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // Resolve the contact's row id. If missing, fail with the
        // fixed message before touching contact_cards.
        let contact_id: i64 = match tx.query_row(
            "SELECT id FROM contacts WHERE identity_pubkey = ?1",
            rusqlite::params![&identity_bytes[..]],
            |r| r.get(0),
        ) {
            Ok(id) => id,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Err(CoreError::Contact(ContactErrorKind::NotFound));
            }
            Err(e) => {
                return Err(CoreError::Contact(ContactErrorKind::Other(format!(
                    "card: lookup: {e}"
                ))))
            }
        };

        // Compare against the stored max version; reject if not strictly greater.
        let max_version: Option<i64> = tx
            .query_row(
                "SELECT MAX(version) FROM contact_cards WHERE contact_id = ?1",
                rusqlite::params![contact_id],
                |r| r.get::<_, Option<i64>>(0),
            )
            .map_err(|e| {
                CoreError::Contact(ContactErrorKind::Other(format!(
                    "card: max-version lookup: {e}"
                )))
            })?;

        if let Some(max_v) = max_version {
            if version_i <= max_v {
                return Err(CoreError::Contact(ContactErrorKind::Other(
                    "card: stale version".into(),
                )));
            }
        }

        tx.execute(
            "INSERT INTO contact_cards (contact_id, version, card_blob, verified_at) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![contact_id, version_i, &blob, verified_at],
        )
        .map_err(|e| CoreError::Contact(ContactErrorKind::Other(format!("card: insert: {e}"))))?;
        Ok(())
    }

    /// Set the MLS group id for `identity`. Returns `CoreError::Contact`
    /// if the contact row is missing.
    pub fn set_group_id(&self, identity: &PublicKey, group_id: &[u8]) -> Result<()> {
        self.pool.with_mut(|c| {
            let changed = c
                .execute(
                    "UPDATE contacts SET group_id = ?1 WHERE identity_pubkey = ?2",
                    rusqlite::params![group_id, &identity.0[..]],
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("set group_id: {e}")))
                })?;
            if changed == 0 {
                return Err(CoreError::Contact(ContactErrorKind::NotFound));
            }
            Ok(())
        })
    }

    /// Transactional companion to [`set_group_id`](Self::set_group_id).
    /// Updates the contact's `group_id` inside the caller's `tx` so the
    /// link commits atomically with the genesis-group save and the
    /// invite consume (T2-1). Returns `CoreError::Contact(NotFound)` if
    /// the contact row is missing.
    pub(crate) fn set_group_id_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        identity: &PublicKey,
        group_id: &[u8],
    ) -> Result<()> {
        let changed = tx
            .execute(
                "UPDATE contacts SET group_id = ?1 WHERE identity_pubkey = ?2",
                rusqlite::params![group_id, &identity.0[..]],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("set group_id (tx): {e}")))
            })?;
        if changed == 0 {
            return Err(CoreError::Contact(ContactErrorKind::NotFound));
        }
        Ok(())
    }

    /// 2-member-group reverse lookup: given a group_id, return the peer's
    /// PublicKey (the member that is not us). Returns `Ok(None)` if no
    /// contact row has this group_id.
    ///
    /// Phase 1.H: scoped to 2-member groups per CLAUDE.md. Multi-member
    /// groups land in Phase 2+; this method will need a signature change
    /// at that point to return a list.
    pub(crate) fn contact_for_group(&self, group_id: &[u8; 32]) -> Result<Option<PublicKey>> {
        use rusqlite::OptionalExtension;
        self.pool.with(|c| {
            let row: Option<Vec<u8>> = c
                .query_row(
                    "SELECT identity_pubkey FROM contacts WHERE group_id = ?1 LIMIT 1",
                    rusqlite::params![&group_id[..]],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("contact_for_group: {e}")))
                })?;
            Ok(row.and_then(|bytes| {
                if bytes.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&bytes);
                    Some(PublicKey(arr))
                } else {
                    None
                }
            }))
        })
    }

    /// Read the MLS group id for `identity`. Returns `Ok(None)` if the
    /// contact row is missing; `Ok(Some(Vec::new()))` for a contact that
    /// has not yet been linked to a group (pre-`AddContact`).
    pub fn get_group_id(&self, identity: &PublicKey) -> Result<Option<Vec<u8>>> {
        self.pool.with(|c| {
            let result = c.query_row(
                "SELECT group_id FROM contacts WHERE identity_pubkey = ?1",
                rusqlite::params![&identity.0[..]],
                |r| r.get::<_, Vec<u8>>(0),
            );
            match result {
                Ok(v) => Ok(Some(v)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Storage(StorageErrorKind::Other(format!(
                    "get group_id: {e}"
                )))),
            }
        })
    }

    /// Update the local display name. `None` clears it. Returns
    /// `ContactErrorKind::NotFound` if no row matched.
    pub fn set_display_name(&self, identity: &PublicKey, name: Option<&str>) -> Result<()> {
        self.pool.with_mut(|c| {
            let changed = c
                .execute(
                    "UPDATE contacts SET display_name = ?1 WHERE identity_pubkey = ?2",
                    rusqlite::params![name, &identity.0[..]],
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("set display_name: {e}")))
                })?;
            if changed == 0 {
                return Err(CoreError::Contact(ContactErrorKind::NotFound));
            }
            Ok(())
        })
    }

    /// Set the `hidden` soft-delete bit. Idempotent. Returns
    /// `ContactErrorKind::NotFound` if no row matched.
    pub fn set_hidden(&self, identity: &PublicKey, hidden: bool) -> Result<()> {
        self.pool.with_mut(|c| {
            let changed = c
                .execute(
                    "UPDATE contacts SET hidden = ?1 WHERE identity_pubkey = ?2",
                    rusqlite::params![if hidden { 1i64 } else { 0i64 }, &identity.0[..]],
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("set hidden: {e}")))
                })?;
            if changed == 0 {
                return Err(CoreError::Contact(ContactErrorKind::NotFound));
            }
            Ok(())
        })
    }

    /// Toggle the per-contact mute flag. No-op (returns `Ok(())`) if
    /// the contact does not exist — caller is responsible for the
    /// existence check (typically a `lookup_by_pubkey` first). Matches
    /// the semantics of `set_display_name` / `set_hidden`.
    pub fn set_muted(&self, identity: &PublicKey, muted: bool) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "UPDATE contacts SET muted = ?1 WHERE identity_pubkey = ?2",
                rusqlite::params![if muted { 1i64 } else { 0i64 }, &identity.0[..]],
            )
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("set_muted: {e}"))))?;
            Ok(())
        })
    }

    /// Read the current mute flag. Returns `Ok(false)` if the contact
    /// does not exist.
    pub fn is_muted(&self, identity: &PublicKey) -> Result<bool> {
        use rusqlite::OptionalExtension;
        self.pool.with(|c| {
            let muted: Option<i64> = c
                .query_row(
                    "SELECT muted FROM contacts WHERE identity_pubkey = ?1",
                    rusqlite::params![&identity.0[..]],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("is_muted: {e}")))
                })?;
            Ok(matches!(muted, Some(v) if v != 0))
        })
    }

    /// Like `list()` but does NOT filter `hidden = 0`. Used by
    /// `Command::ListContactsWithFilter { include_hidden: true }`.
    pub(crate) fn list_all(&self) -> Result<Vec<Contact>> {
        let mut contacts: Vec<Contact> = self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT identity_pubkey, display_name, added_at, muted FROM contacts \
                     ORDER BY display_name IS NULL, display_name COLLATE NOCASE",
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("prepare list_all: {e}")))
                })?;
            let rows = stmt
                .query_map([], |r| {
                    let pub_bytes: Vec<u8> = r.get(0)?;
                    let mut arr = [0u8; 32];
                    if pub_bytes.len() == 32 {
                        arr.copy_from_slice(&pub_bytes);
                    }
                    let muted_val: i64 = r.get(3).unwrap_or(0);
                    Ok(Contact {
                        identity: PublicKey(arr),
                        display_name: r.get(1)?,
                        added_at: r.get(2)?,
                        card: None,
                        muted: muted_val != 0,
                    })
                })
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("query list_all: {e}")))
                })?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("collect list_all: {e}")))
            })
        })?;
        for contact in &mut contacts {
            contact.card = self.latest_card(&contact.identity)?;
        }
        Ok(contacts)
    }

    /// Find every contact whose hex-encoded `identity_pubkey` starts
    /// with `prefix` (case-insensitive). Empty result = no match;
    /// the caller enforces "exactly one" when a unique contact is
    /// required. Returns `CoreError::Contact` if `prefix` contains
    /// non-hex characters.
    pub fn lookup_by_prefix(&self, prefix: &str) -> Result<Vec<PublicKey>> {
        if prefix.is_empty() {
            return Err(CoreError::Contact(ContactErrorKind::Other(
                "lookup: empty prefix".into(),
            )));
        }
        if !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(CoreError::Contact(ContactErrorKind::Other(format!(
                "lookup: non-hex prefix {prefix:?}"
            ))));
        }
        let lower = prefix.to_ascii_lowercase();
        self.pool.with(|c| {
            let mut stmt = c
                .prepare("SELECT identity_pubkey FROM contacts")
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("prepare lookup: {e}")))
                })?;
            let rows = stmt
                .query_map([], |r| r.get::<_, Vec<u8>>(0))
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("query lookup: {e}")))
                })?;
            let mut out = Vec::new();
            for row in rows {
                let bytes = row.map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("row lookup: {e}")))
                })?;
                if bytes.len() != 32 {
                    continue;
                }
                let hex = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
                if hex.starts_with(&lower) {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&bytes);
                    out.push(PublicKey(arr));
                }
            }
            Ok(out)
        })
    }

    /// Resolve a peer's Noise static X25519 key to its Ed25519 identity by
    /// converting each known contact's identity via `ed25519_pub_to_x25519`
    /// and comparing in constant time. Returns `None` if no contact matches.
    ///
    /// O(contacts) per call — fine for v1.0 contact scale.
    pub(crate) fn find_by_noise_x25519(&self, x: &[u8; 32]) -> Result<Option<PublicKey>> {
        use subtle::ConstantTimeEq;
        for contact in self.list()? {
            let vk = match ed25519_dalek::VerifyingKey::from_bytes(&contact.identity.0) {
                Ok(vk) => vk,
                Err(_) => {
                    // Near-unreachable: identity bytes are validated on insert.
                    // A non-curve stored key signals storage corruption — make
                    // it observable (no key bytes logged).
                    tracing::warn!("contacts: skipping contact with non-curve stored identity");
                    continue;
                }
            };
            let cand = crate::identity::key::ed25519_pub_to_x25519(&vk);
            if cand.ct_eq(x).into() {
                return Ok(Some(contact.identity));
            }
        }
        Ok(None)
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
                        CoreError::Contact(ContactErrorKind::Other(format!(
                            "card: cbor decode: {e}"
                        )))
                    })?;
                    Ok(Some(card))
                }
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Contact(ContactErrorKind::Other(format!(
                    "card: latest: {e}"
                )))),
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
            muted: false,
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
                    .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn remove_in_tx_deletes_contact_and_cascades_onions() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let alice = sample_contact(0x05);
        repo.upsert(&alice).unwrap();
        repo.add_onion(&alice.identity, "zzzz.onion", 100).unwrap();

        pool.transaction(|tx| repo.remove_in_tx(tx, &alice.identity))
            .unwrap();

        assert!(repo.get(&alice.identity).unwrap().is_none());
        let count: i64 = pool
            .with(|c| {
                c.query_row("SELECT COUNT(*) FROM onion_addresses", [], |r| r.get(0))
                    .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))
            })
            .unwrap();
        assert_eq!(count, 0, "onion rows must cascade-delete inside the tx");
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
            matches!(
                err,
                CoreError::Contact(ContactErrorKind::Other(ref s)) if s.contains("stale version")
            ),
            "got: {err:?}"
        );

        let err = repo
            .put_card(&sample_card(11, 4))
            .expect_err("older version");
        assert!(
            matches!(
                err,
                CoreError::Contact(ContactErrorKind::Other(ref s)) if s.contains("stale version")
            ),
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
            matches!(err, CoreError::Contact(ContactErrorKind::NotFound)),
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
                    .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))
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

    #[test]
    fn get_hydrates_latest_card() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let contact = sample_contact(20);
        repo.upsert(&contact).unwrap();

        repo.put_card(&sample_card(20, 1)).unwrap();
        repo.put_card(&sample_card(20, 2)).unwrap();

        let got = repo.get(&contact.identity).unwrap().unwrap();
        let card = got.card.expect("contact.card must hydrate");
        assert_eq!(card.body.version, 2);
    }

    #[test]
    fn list_hydrates_latest_card() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let c1 = sample_contact(21);
        let c2 = sample_contact(22);
        repo.upsert(&c1).unwrap();
        repo.upsert(&c2).unwrap();

        repo.put_card(&sample_card(21, 5)).unwrap();
        // c2 has no card — get should still succeed with card=None.

        let all = repo.list().unwrap();
        let got_c1 = all.iter().find(|c| c.identity == c1.identity).unwrap();
        let got_c2 = all.iter().find(|c| c.identity == c2.identity).unwrap();
        assert_eq!(got_c1.card.as_ref().unwrap().body.version, 5);
        assert!(got_c2.card.is_none());
    }

    #[test]
    fn set_get_group_id_round_trip() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let alice = sample_contact(30);
        repo.upsert(&alice).unwrap();

        // Default group_id is empty (per migration 0005).
        assert_eq!(
            repo.get_group_id(&alice.identity).unwrap(),
            Some(Vec::new())
        );

        let gid = vec![0xAAu8; 32];
        repo.set_group_id(&alice.identity, &gid).unwrap();
        assert_eq!(repo.get_group_id(&alice.identity).unwrap(), Some(gid));
    }

    #[test]
    fn get_group_id_missing_contact_returns_none() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        assert!(repo.get_group_id(&PublicKey([0x99; 32])).unwrap().is_none());
    }

    #[test]
    fn lookup_by_prefix_returns_unique_match() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let alice = sample_contact(0x10);
        let bob = sample_contact(0x20);
        repo.upsert(&alice).unwrap();
        repo.upsert(&bob).unwrap();

        // "10" is a unique 1-byte hex prefix for alice.
        let hit = repo.lookup_by_prefix("10").unwrap();
        assert_eq!(hit, vec![alice.identity]);
    }

    #[test]
    fn lookup_by_prefix_returns_all_ambiguous_matches() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        // Two contacts both starting with 0xAB.
        let mut a = sample_contact(0xAB);
        a.identity = PublicKey([0xAB; 32]);
        let mut b = sample_contact(0xAB);
        b.identity = PublicKey({
            let mut bytes = [0xAB; 32];
            bytes[1] = 0xCD;
            bytes
        });
        repo.upsert(&a).unwrap();
        repo.upsert(&b).unwrap();

        let mut hits = repo.lookup_by_prefix("ab").unwrap();
        hits.sort();
        let mut want = vec![a.identity, b.identity];
        want.sort();
        assert_eq!(hits, want);
    }

    #[test]
    fn lookup_by_prefix_empty_returns_empty() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        repo.upsert(&sample_contact(1)).unwrap();
        // Unknown prefix -> empty result (NOT error).
        assert!(repo.lookup_by_prefix("ff00").unwrap().is_empty());
    }

    #[test]
    fn lookup_by_prefix_rejects_empty() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let err = repo.lookup_by_prefix("").expect_err("empty prefix");
        assert!(
            matches!(
                err,
                CoreError::Contact(ContactErrorKind::Other(ref s)) if s.contains("empty")
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn lookup_by_prefix_rejects_non_hex() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let err = repo.lookup_by_prefix("zz").expect_err("non-hex prefix");
        assert!(
            matches!(
                err,
                CoreError::Contact(ContactErrorKind::Other(ref s)) if s.contains("hex")
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn contact_for_group_returns_peer_for_2_member_group() {
        let pool = Pool::in_memory();
        let peer = crate::identity::PublicKey([0x42; 32]);
        let gid = [0x43u8; 32];

        let repo = ContactRepo::new(&pool);
        repo.upsert(&crate::contact::Contact {
            identity: peer,
            display_name: None,
            added_at: 0,
            card: None,
            muted: false,
        })
        .unwrap();
        repo.set_group_id(&peer, &gid).unwrap();

        let got = repo.contact_for_group(&gid).unwrap();
        assert_eq!(got, Some(peer));
    }

    #[test]
    fn contact_for_group_returns_none_for_unknown_group() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let gid = [0x44u8; 32];
        let got = repo.contact_for_group(&gid).unwrap();
        assert_eq!(got, None);
    }

    #[test]
    fn set_display_name_round_trips() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let alice = sample_contact(0x40);
        repo.upsert(&alice).unwrap();

        repo.set_display_name(&alice.identity, Some("renamed"))
            .unwrap();
        assert_eq!(
            repo.get(&alice.identity).unwrap().unwrap().display_name,
            Some("renamed".into())
        );

        repo.set_display_name(&alice.identity, None).unwrap();
        assert!(repo
            .get(&alice.identity)
            .unwrap()
            .unwrap()
            .display_name
            .is_none());
    }

    #[test]
    fn set_display_name_returns_not_found_for_missing_contact() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let err = repo
            .set_display_name(&PublicKey([0x99; 32]), Some("x"))
            .expect_err("missing contact");
        assert!(matches!(
            err,
            CoreError::Contact(ContactErrorKind::NotFound)
        ));
    }

    #[test]
    fn set_hidden_filters_list_but_keeps_list_all() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let visible = sample_contact(0x50);
        let archived = sample_contact(0x60);
        repo.upsert(&visible).unwrap();
        repo.upsert(&archived).unwrap();

        repo.set_hidden(&archived.identity, true).unwrap();

        let listed = repo.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].identity, visible.identity);

        let listed_all = repo.list_all().unwrap();
        assert_eq!(listed_all.len(), 2);
    }

    #[test]
    fn set_hidden_is_idempotent() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let alice = sample_contact(0x70);
        repo.upsert(&alice).unwrap();
        repo.set_hidden(&alice.identity, true).unwrap();
        repo.set_hidden(&alice.identity, true).unwrap();
        assert_eq!(repo.list().unwrap().len(), 0);
    }

    #[test]
    fn set_muted_toggles_and_persists() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let alice = sample_contact(0x80);
        repo.upsert(&alice).unwrap();

        assert!(!repo.is_muted(&alice.identity).unwrap());
        repo.set_muted(&alice.identity, true).unwrap();
        assert!(repo.is_muted(&alice.identity).unwrap());
        repo.set_muted(&alice.identity, false).unwrap();
        assert!(!repo.is_muted(&alice.identity).unwrap());
    }

    #[test]
    fn is_muted_returns_false_for_missing_contact() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        assert!(!repo.is_muted(&PublicKey([0xBB; 32])).unwrap());
    }

    #[test]
    fn find_by_noise_x25519_resolves_known_contact_and_rejects_stranger() {
        use crate::identity::IdentityKey;

        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);

        let alice = IdentityKey::generate().unwrap();
        let alice_pk = alice.public();
        repo.upsert(&Contact {
            identity: alice_pk,
            display_name: None,
            added_at: 0,
            card: None,
            muted: false,
        })
        .unwrap();

        let alice_x = crate::identity::key::ed25519_pub_to_x25519(
            &ed25519_dalek::VerifyingKey::from_bytes(&alice_pk.0).unwrap(),
        );
        assert_eq!(repo.find_by_noise_x25519(&alice_x).unwrap(), Some(alice_pk));

        let stranger = IdentityKey::generate().unwrap();
        let stranger_x = crate::identity::key::ed25519_pub_to_x25519(
            &ed25519_dalek::VerifyingKey::from_bytes(&stranger.public().0).unwrap(),
        );
        assert_eq!(repo.find_by_noise_x25519(&stranger_x).unwrap(), None);
    }
}
