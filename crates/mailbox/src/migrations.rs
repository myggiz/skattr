// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Forward-only schema-migration runner. Mirrors the `core::storage`
//! pattern: `include_str!`'d SQL files keyed by a monotonic version.

use crate::error::{MailboxError, StorageErrorKind};

struct Migration {
    version: u32,
    sql: &'static str,
}

const ALL_MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: include_str!("../migrations/0001_init.sql"),
}];

/// Apply all pending migrations in order. Idempotent.
pub(crate) fn apply(conn: &mut rusqlite::Connection) -> Result<(), MailboxError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY)",
        [],
    )?;
    let current: u32 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    for m in ALL_MIGRATIONS {
        if m.version <= current {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(m.sql)?;
        tx.execute(
            "INSERT OR REPLACE INTO schema_version (version) VALUES (?1)",
            [m.version],
        )?;
        tx.commit()?;
    }

    let final_version: u32 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_version",
        [],
        |r| r.get(0),
    )?;
    let target = ALL_MIGRATIONS.last().map(|m| m.version).unwrap_or(0);
    if final_version != target {
        return Err(MailboxError::Storage(StorageErrorKind::MigrationFailed(
            format!("expected schema {target}, got {final_version}"),
        )));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn open_in_memory() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").ok(); // memory ignores
        conn
    }

    #[test]
    fn migration_0001_creates_deposits_table() {
        let mut conn = open_in_memory();
        apply(&mut conn).unwrap();

        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info('deposits')")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for required in [
            "deposit_id",
            "recipient_hash",
            "ciphertext",
            "deposited_at",
            "expires_at",
        ] {
            assert!(cols.iter().any(|c| c == required), "missing {required}");
        }
    }

    #[test]
    fn migration_0001_creates_indexes() {
        let mut conn = open_in_memory();
        apply(&mut conn).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name IN \
                 ('idx_deposits_recipient', 'idx_deposits_expiry')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn idempotent_re_apply() {
        let mut conn = open_in_memory();
        apply(&mut conn).unwrap();
        apply(&mut conn).unwrap();
        let count: i64 = conn
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}
