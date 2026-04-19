// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Schema-migration runner.
//!
//! Each migration is a `&str` of SQL (via `include_str!`), paired with a
//! monotonic version number. On open, we consult `schema_version` (a
//! single-row bookkeeping table) and run every migration whose version
//! is greater than the current one.
//!
//! This design is simpler than `refinery` or `sqlx::migrate!` and has
//! zero extra dependencies. If we ever need rollback support or
//! transactional migrations across files, revisit at Phase 1.

use crate::error::Result;

/// A single migration: a monotonic version number and its SQL text.
struct Migration {
    version: u32,
    sql: &'static str,
}

const ALL_MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: include_str!("migrations/0001_init.sql"),
}];

/// Apply all pending migrations in order. Idempotent — re-running does
/// nothing if `schema_version` is already at the latest version.
///
/// The caller opens the Connection and sets pragmas before calling us;
/// we run the migration SQL and update `schema_version`.
pub(crate) fn apply(conn: &mut rusqlite::Connection) -> Result<()> {
    // Ensure schema_version exists. 0001_init.sql creates this table
    // too; running CREATE TABLE IF NOT EXISTS here is a no-op after the
    // first migration but handles fresh databases on first open.
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

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_db_runs_migrations_to_v1() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        apply(&mut conn).unwrap();
        let v: u32 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 1);
    }

    #[test]
    fn re_applying_is_idempotent() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        apply(&mut conn).unwrap();
        apply(&mut conn).unwrap();
        apply(&mut conn).unwrap();
        // Row count in schema_version should still be 1.
        let rows: u32 = conn
            .query_row("SELECT COUNT(*) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn migration_creates_expected_tables() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        apply(&mut conn).unwrap();
        for table in [
            "identity",
            "contacts",
            "onion_addresses",
            "mls_groups",
            "messages",
            "outbox",
            "mailboxes",
            "seen_messages",
        ] {
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(exists, 1, "table {table} must exist after migration");
        }
    }
}
