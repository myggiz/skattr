// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Repository for the `mailboxes` table.

use crate::error::{CoreError, Result};
use crate::storage::Pool;

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
            other => Err(CoreError::Storage(format!(
                "unknown mailbox role: {other}"
            ))),
        }
    }
}

pub(crate) struct MailboxRepo<'p> {
    pool: &'p Pool,
}

impl<'p> MailboxRepo<'p> {
    pub(crate) fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    pub(crate) fn insert(&self, onion: &str, role: MailboxRole, registered_at: i64) -> Result<i64> {
        self.pool.with_mut(|c| {
            c.execute(
                "INSERT OR IGNORE INTO mailboxes (onion, registered_at, role) VALUES (?1, ?2, ?3)",
                rusqlite::params![onion, registered_at, role.as_sql()],
            )
            .map_err(|e| CoreError::Storage(format!("insert mailbox: {e}")))?;
            Ok(c.last_insert_rowid())
        })
    }

    pub(crate) fn list(&self, role: MailboxRole) -> Result<Vec<String>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare("SELECT onion FROM mailboxes WHERE role = ?1 ORDER BY registered_at")
                .map_err(|e| CoreError::Storage(format!("prepare list mailboxes: {e}")))?;
            let rows = stmt
                .query_map(rusqlite::params![role.as_sql()], |r| r.get::<_, String>(0))
                .map_err(|e| CoreError::Storage(format!("query list mailboxes: {e}")))?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| CoreError::Storage(format!("collect mailboxes: {e}")))
        })
    }

    pub(crate) fn remove(&self, onion: &str, role: MailboxRole) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "DELETE FROM mailboxes WHERE onion = ?1 AND role = ?2",
                rusqlite::params![onion, role.as_sql()],
            )
            .map_err(|e| CoreError::Storage(format!("delete mailbox: {e}")))?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_list_by_role() {
        let pool = Pool::in_memory();
        let repo = MailboxRepo::new(&pool);
        repo.insert("mine-a.onion", MailboxRole::Mine, 100).unwrap();
        repo.insert("theirs-a.onion", MailboxRole::Theirs, 200).unwrap();
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
        repo.insert("multi.onion", MailboxRole::Theirs, 200).unwrap();
        repo.remove("multi.onion", MailboxRole::Mine).unwrap();
        assert_eq!(repo.list(MailboxRole::Mine).unwrap().len(), 0);
        assert_eq!(repo.list(MailboxRole::Theirs).unwrap().len(), 1);
    }

    #[test]
    fn sql_role_parse_rejects_garbage() {
        assert!(MailboxRole::from_sql("bogus").is_err());
    }
}
