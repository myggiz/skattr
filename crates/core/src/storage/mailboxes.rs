// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Repository for the `mailboxes` table — both `'mine'` (we deposit
//! into them) and `'theirs'` (peers list them). 2.B owns status
//! transitions; 1.E's add/list helpers stay where they are for now.

use super::StorageErrorKind;
use crate::error::{CoreError, Result};
use crate::identity::PublicKey;
use crate::storage::{ContactRepo, Pool};

/// Role of a stored mailbox record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MailboxRole {
    /// We've registered with this mailbox and poll it for inbound messages.
    Mine,
    /// Belongs to a contact; we deposit here when they're offline.
    Theirs,
}

impl MailboxRole {
    fn as_sql(self) -> &'static str {
        match self {
            MailboxRole::Mine => "mine",
            MailboxRole::Theirs => "theirs",
        }
    }

    fn from_sql(s: &str) -> Result<Self> {
        match s {
            "mine" => Ok(MailboxRole::Mine),
            "theirs" => Ok(MailboxRole::Theirs),
            other => Err(CoreError::Storage(StorageErrorKind::Other(format!(
                "unknown mailbox role: {other}"
            )))),
        }
    }
}

/// Mirrors the `mailboxes.status` CHECK constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
pub enum MailboxStatus {
    /// Status has not yet been determined.
    Unknown,
    /// Mailbox is reachable and accepting connections.
    Reachable,
    /// Mailbox could not be reached during the last poll.
    Unreachable,
    /// Mailbox responded but is enforcing a rate limit.
    RateLimited,
    /// De-registration has been requested but not yet confirmed by the server.
    PendingRemoval,
    /// De-registration confirmed; mailbox is no longer in use.
    Removed,
}

impl MailboxStatus {
    pub(crate) fn as_sql(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Reachable => "reachable",
            Self::Unreachable => "unreachable",
            Self::RateLimited => "rate_limited",
            Self::PendingRemoval => "pending_removal",
            Self::Removed => "removed",
        }
    }

    pub(crate) fn from_sql(s: &str) -> Option<Self> {
        Some(match s {
            "unknown" => Self::Unknown,
            "reachable" => Self::Reachable,
            "unreachable" => Self::Unreachable,
            "rate_limited" => Self::RateLimited,
            "pending_removal" => Self::PendingRemoval,
            "removed" => Self::Removed,
            _ => return None,
        })
    }
}

/// One row from the `mailboxes` table.
#[derive(Debug, Clone)]
pub struct MailboxRow {
    /// Primary key.
    pub id: i64,
    /// Onion service identifier (v3 format).
    pub onion: String,
    /// Unix-seconds timestamp at which this row was first inserted.
    pub registered_at: i64,
    /// Role string (`"mine"` or `"theirs"`).
    pub role: String,
    /// Current connectivity status.
    pub status: MailboxStatus,
    /// Unix-seconds timestamp of the most recent poll attempt, if any.
    pub last_poll_at: Option<i64>,
    /// Unix-seconds timestamp of the most recent error, if any.
    pub last_error_at: Option<i64>,
    /// Short error-kind string for the most recent failure, if any.
    pub last_error_kind: Option<String>,
}

/// Repository for the `mailboxes` table — both `'mine'` and `'theirs'` rows.
pub struct MailboxRepo<'p> {
    pool: &'p Pool,
}

impl<'p> MailboxRepo<'p> {
    /// Construct a `MailboxRepo` borrowing from `pool`.
    ///
    /// `pub` under `feature = "test-harness"` so the integration test
    /// crate can build mailbox-aware harnesses; `pub(crate)` otherwise.
    #[cfg(feature = "test-harness")]
    pub fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    #[cfg(not(feature = "test-harness"))]
    pub(crate) fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    // ── Phase 1.E helpers (kept for backward compat) ─────────────────────

    pub(crate) fn insert(&self, onion: &str, role: MailboxRole, registered_at: i64) -> Result<i64> {
        self.pool.with_mut(|c| {
            c.execute(
                "INSERT OR IGNORE INTO mailboxes (onion, registered_at, role) VALUES (?1, ?2, ?3)",
                rusqlite::params![onion, registered_at, role.as_sql()],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("insert mailbox: {e}")))
            })?;
            Ok(c.last_insert_rowid())
        })
    }

    pub(crate) fn list(&self, role: MailboxRole) -> Result<Vec<String>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare("SELECT onion FROM mailboxes WHERE role = ?1 ORDER BY registered_at")
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "prepare list mailboxes: {e}"
                    )))
                })?;
            let rows = stmt
                .query_map(rusqlite::params![role.as_sql()], |r| r.get::<_, String>(0))
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "query list mailboxes: {e}"
                    )))
                })?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("collect mailboxes: {e}")))
            })
        })
    }

    pub(crate) fn remove(&self, onion: &str, role: MailboxRole) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "DELETE FROM mailboxes WHERE onion = ?1 AND role = ?2",
                rusqlite::params![onion, role.as_sql()],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("delete mailbox: {e}")))
            })?;
            Ok(())
        })
    }

    // ── Phase 2.B status-tracking helpers ────────────────────────────────

    /// Insert a `role='mine'` row for `onion`, or return the existing row id
    /// if already present. Idempotent.
    pub fn add_mine(&self, onion: &str, now: i64) -> Result<i64> {
        self.pool.with_mut(|c| {
            c.execute(
                "INSERT INTO mailboxes(onion, registered_at, role) \
                 VALUES (?1, ?2, 'mine') \
                 ON CONFLICT(onion, role) DO NOTHING",
                rusqlite::params![onion, now],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("add_mine insert: {e}")))
            })?;
            let id: i64 = c
                .query_row(
                    "SELECT id FROM mailboxes WHERE onion = ?1 AND role = 'mine'",
                    rusqlite::params![onion],
                    |r| r.get(0),
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("add_mine select: {e}")))
                })?;
            Ok(id)
        })
    }

    /// Insert a `role='theirs'` row for `onion`, or return the existing row id
    /// if already present. Idempotent. Used by the delivery hub's
    /// direct→mailbox fallback orchestrator: when we discover a peer's
    /// mailbox onion (via their `ContactCard`) and need a concrete
    /// `mailbox_id` for the outbox row, we lazy-insert the row here.
    pub fn ensure_theirs(&self, onion: &str, now: i64) -> Result<i64> {
        self.pool.with_mut(|c| {
            c.execute(
                "INSERT INTO mailboxes(onion, registered_at, role) \
                 VALUES (?1, ?2, 'theirs') \
                 ON CONFLICT(onion, role) DO NOTHING",
                rusqlite::params![onion, now],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "ensure_theirs insert: {e}"
                )))
            })?;
            let id: i64 = c
                .query_row(
                    "SELECT id FROM mailboxes WHERE onion = ?1 AND role = 'theirs'",
                    rusqlite::params![onion],
                    |r| r.get(0),
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "ensure_theirs select: {e}"
                    )))
                })?;
            Ok(id)
        })
    }

    /// List all `role='mine'` rows ordered by `registered_at, id`.
    pub fn list_mine(&self) -> Result<Vec<MailboxRow>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, onion, registered_at, role, status, \
                            last_poll_at, last_error_at, last_error_kind \
                     FROM mailboxes WHERE role = 'mine' \
                     ORDER BY registered_at, id",
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("list_mine prep: {e}")))
                })?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(MailboxRow {
                        id: r.get(0)?,
                        onion: r.get(1)?,
                        registered_at: r.get(2)?,
                        role: r.get(3)?,
                        status: MailboxStatus::from_sql(&r.get::<_, String>(4)?)
                            .unwrap_or(MailboxStatus::Unknown),
                        last_poll_at: r.get(5)?,
                        last_error_at: r.get(6)?,
                        last_error_kind: r.get(7)?,
                    })
                })
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("list_mine query: {e}")))
                })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("list_mine collect: {e}")))
                })
        })
    }

    /// Fetch a single mailbox row by primary key.
    pub fn get(&self, id: i64) -> Result<Option<MailboxRow>> {
        use rusqlite::OptionalExtension;
        self.pool.with(|c| {
            let opt = c
                .query_row(
                    "SELECT id, onion, registered_at, role, status, \
                            last_poll_at, last_error_at, last_error_kind \
                     FROM mailboxes WHERE id = ?1",
                    rusqlite::params![id],
                    |r| {
                        Ok(MailboxRow {
                            id: r.get(0)?,
                            onion: r.get(1)?,
                            registered_at: r.get(2)?,
                            role: r.get(3)?,
                            status: MailboxStatus::from_sql(&r.get::<_, String>(4)?)
                                .unwrap_or(MailboxStatus::Unknown),
                            last_poll_at: r.get(5)?,
                            last_error_at: r.get(6)?,
                            last_error_kind: r.get(7)?,
                        })
                    },
                )
                .optional()
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("get mailbox: {e}")))
                })?;
            Ok(opt)
        })
    }

    /// Update the `status` column for the given row id.
    pub fn mark_status(&self, id: i64, status: MailboxStatus) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "UPDATE mailboxes SET status = ?1 WHERE id = ?2",
                rusqlite::params![status.as_sql(), id],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("mark_status: {e}")))
            })?;
            Ok(())
        })
    }

    /// Transition to `pending_removal` (first step of two-phase removal).
    pub fn mark_pending_removal(&self, id: i64) -> Result<()> {
        self.mark_status(id, MailboxStatus::PendingRemoval)
    }

    /// Transition to `removed` (second step — after the server ACKs the
    /// de-registration).
    pub fn finalize_removal(&self, id: i64) -> Result<()> {
        self.mark_status(id, MailboxStatus::Removed)
    }

    /// Return the list of mailbox onions advertised on `identity`'s latest
    /// `ContactCard`. Returns an empty `Vec` when the contact is unknown or
    /// has no card yet. Does **not** touch the `mailboxes` SQL table.
    pub fn list_for_contact(&self, identity: &PublicKey) -> Result<Vec<String>> {
        let contacts = ContactRepo::new(self.pool);
        match contacts.latest_card(identity)? {
            Some(card) => Ok(card.body.mailboxes.clone()),
            None => Ok(Vec::new()),
        }
    }

    /// Update `last_poll_at` to `now`.
    pub fn touch_poll(&self, id: i64, now: i64) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "UPDATE mailboxes SET last_poll_at = ?1 WHERE id = ?2",
                rusqlite::params![now, id],
            )
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("touch_poll: {e}"))))?;
            Ok(())
        })
    }

    /// Record the most recent connectivity error (`kind` + timestamp).
    pub fn record_error(&self, id: i64, kind: &str, now: i64) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "UPDATE mailboxes SET last_error_kind = ?1, last_error_at = ?2 WHERE id = ?3",
                rusqlite::params![kind, now, id],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("record_error: {e}")))
            })?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Phase 1.E legacy tests (kept) ────────────────────────────────────

    #[test]
    fn insert_and_list_by_role() {
        let pool = Pool::in_memory();
        let repo = MailboxRepo::new(&pool);
        repo.insert("mine-a.onion", MailboxRole::Mine, 100).unwrap();
        repo.insert("theirs-a.onion", MailboxRole::Theirs, 200)
            .unwrap();
        repo.insert("mine-b.onion", MailboxRole::Mine, 300).unwrap();

        let mine = repo.list(MailboxRole::Mine).unwrap();
        assert_eq!(mine, vec!["mine-a.onion", "mine-b.onion"]);

        let theirs = repo.list(MailboxRole::Theirs).unwrap();
        assert_eq!(theirs, vec!["theirs-a.onion"]);
    }

    #[test]
    fn insert_or_ignore_dedups_same_role() {
        let pool = Pool::in_memory();
        let repo = MailboxRepo::new(&pool);
        repo.insert("dup.onion", MailboxRole::Mine, 100).unwrap();
        repo.insert("dup.onion", MailboxRole::Mine, 200).unwrap();
        assert_eq!(repo.list(MailboxRole::Mine).unwrap().len(), 1);
    }

    #[test]
    fn remove_scoped_to_role() {
        let pool = Pool::in_memory();
        let repo = MailboxRepo::new(&pool);
        repo.insert("multi.onion", MailboxRole::Mine, 100).unwrap();
        repo.insert("multi.onion", MailboxRole::Theirs, 200)
            .unwrap();
        repo.remove("multi.onion", MailboxRole::Mine).unwrap();
        assert_eq!(repo.list(MailboxRole::Mine).unwrap().len(), 0);
        assert_eq!(repo.list(MailboxRole::Theirs).unwrap().len(), 1);
    }

    #[test]
    fn sql_role_parse_rejects_garbage() {
        assert!(MailboxRole::from_sql("bogus").is_err());
    }

    // ── Phase 2.B status-tracking tests ──────────────────────────────────

    #[test]
    fn add_mine_then_list_round_trip() {
        let pool = Pool::in_memory();
        let repo = MailboxRepo::new(&pool);
        let id = repo.add_mine("aaaa.onion", 100).unwrap();
        let rows = repo.list_mine().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].onion, "aaaa.onion");
        assert_eq!(rows[0].status, MailboxStatus::Unknown);
    }

    #[test]
    fn add_mine_is_idempotent_on_same_onion() {
        let pool = Pool::in_memory();
        let repo = MailboxRepo::new(&pool);
        let id1 = repo.add_mine("bbbb.onion", 100).unwrap();
        let id2 = repo.add_mine("bbbb.onion", 200).unwrap();
        assert_eq!(id1, id2, "second add returns the same row id");
        assert_eq!(repo.list_mine().unwrap().len(), 1);
    }

    #[test]
    fn mark_status_persists_and_is_observable() {
        let pool = Pool::in_memory();
        let repo = MailboxRepo::new(&pool);
        let id = repo.add_mine("cccc.onion", 100).unwrap();
        repo.mark_status(id, MailboxStatus::Reachable).unwrap();
        let row = repo.get(id).unwrap().unwrap();
        assert_eq!(row.status, MailboxStatus::Reachable);
    }

    #[test]
    fn touch_poll_sets_last_poll_at() {
        let pool = Pool::in_memory();
        let repo = MailboxRepo::new(&pool);
        let id = repo.add_mine("dddd.onion", 100).unwrap();
        repo.touch_poll(id, 555).unwrap();
        let row = repo.get(id).unwrap().unwrap();
        assert_eq!(row.last_poll_at, Some(555));
    }

    #[test]
    fn record_error_sets_kind_and_at() {
        let pool = Pool::in_memory();
        let repo = MailboxRepo::new(&pool);
        let id = repo.add_mine("eeee.onion", 100).unwrap();
        repo.record_error(id, "unreachable", 777).unwrap();
        let row = repo.get(id).unwrap().unwrap();
        assert_eq!(row.last_error_kind.as_deref(), Some("unreachable"));
        assert_eq!(row.last_error_at, Some(777));
    }

    #[test]
    fn pending_removal_then_finalize() {
        let pool = Pool::in_memory();
        let repo = MailboxRepo::new(&pool);
        let id = repo.add_mine("ffff.onion", 100).unwrap();
        repo.mark_pending_removal(id).unwrap();
        assert_eq!(
            repo.get(id).unwrap().unwrap().status,
            MailboxStatus::PendingRemoval
        );
        repo.finalize_removal(id).unwrap();
        assert_eq!(
            repo.get(id).unwrap().unwrap().status,
            MailboxStatus::Removed
        );
    }

    #[test]
    fn list_mine_excludes_theirs_role() {
        let pool = Pool::in_memory();
        let repo = MailboxRepo::new(&pool);
        repo.add_mine("aaaa.onion", 100).unwrap();
        // Direct insert with role='theirs' to simulate a peer's mailbox.
        pool.with_mut(|c| {
            c.execute(
                "INSERT INTO mailboxes(onion, registered_at, role) VALUES (?1, ?2, 'theirs')",
                rusqlite::params!["bbbb.onion", 100],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
        let mine = repo.list_mine().unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].onion, "aaaa.onion");
    }

    // ── ensure_theirs tests ──────────────────────────────────────────────

    #[test]
    fn ensure_theirs_inserts_new_row_with_role_theirs() {
        let pool = Pool::in_memory();
        let repo = MailboxRepo::new(&pool);
        let id = repo.ensure_theirs("peer-mb.onion", 100).unwrap();
        // Should appear in raw SQL with role='theirs' and not in list_mine.
        assert!(repo.list_mine().unwrap().is_empty());
        let onion: String = pool
            .with(|c| {
                c.query_row(
                    "SELECT onion FROM mailboxes WHERE id = ?1 AND role = 'theirs'",
                    rusqlite::params![id],
                    |r| r.get(0),
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("test query: {e}")))
                })
            })
            .unwrap();
        assert_eq!(onion, "peer-mb.onion");
    }

    #[test]
    fn ensure_theirs_is_idempotent_on_same_onion() {
        let pool = Pool::in_memory();
        let repo = MailboxRepo::new(&pool);
        let id1 = repo.ensure_theirs("peer-mb.onion", 100).unwrap();
        let id2 = repo.ensure_theirs("peer-mb.onion", 200).unwrap();
        assert_eq!(id1, id2);
    }

    #[test]
    fn ensure_theirs_does_not_collide_with_role_mine() {
        let pool = Pool::in_memory();
        let repo = MailboxRepo::new(&pool);
        let mine = repo.add_mine("dual.onion", 100).unwrap();
        let theirs = repo.ensure_theirs("dual.onion", 200).unwrap();
        assert_ne!(mine, theirs, "different rows for different roles");
    }

    // ── list_for_contact tests ────────────────────────────────────────────

    #[test]
    fn list_for_contact_returns_card_mailboxes() {
        use crate::contact::card::{ContactCard, ContactCardBody};
        use crate::contact::Contact;
        use crate::identity::{PublicKey, Signature};
        use crate::storage::ContactRepo;

        let pool = Pool::in_memory();
        let contacts = ContactRepo::new(&pool);
        let id = PublicKey([0x42; 32]);
        contacts
            .upsert(&Contact {
                identity: id,
                display_name: None,
                added_at: 0,
                card: None,
            })
            .unwrap();
        contacts
            .put_card(&ContactCard {
                body: ContactCardBody {
                    identity: id,
                    onion: "self.onion".into(),
                    mailboxes: vec!["aaaa.onion".into(), "bbbb.onion".into()],
                    version: 1,
                    expires_at: 9_999_999_999,
                },
                // Storage tests skip signature verification — see contacts.rs sample_card.
                signature: Signature([0u8; 64]),
            })
            .unwrap();

        let mailboxes = MailboxRepo::new(&pool).list_for_contact(&id).unwrap();
        assert_eq!(
            mailboxes,
            vec!["aaaa.onion".to_string(), "bbbb.onion".to_string()]
        );
    }

    #[test]
    fn list_for_contact_empty_when_no_card() {
        use crate::contact::Contact;
        use crate::identity::PublicKey;
        use crate::storage::ContactRepo;

        let pool = Pool::in_memory();
        let id = PublicKey([0x55; 32]);
        ContactRepo::new(&pool)
            .upsert(&Contact {
                identity: id,
                display_name: None,
                added_at: 0,
                card: None,
            })
            .unwrap();
        let mailboxes = MailboxRepo::new(&pool).list_for_contact(&id).unwrap();
        assert!(mailboxes.is_empty());
    }

    #[test]
    fn list_for_contact_empty_when_contact_unknown() {
        use crate::identity::PublicKey;
        let pool = Pool::in_memory();
        let mailboxes = MailboxRepo::new(&pool)
            .list_for_contact(&PublicKey([0x99; 32]))
            .unwrap();
        assert!(mailboxes.is_empty());
    }
}
