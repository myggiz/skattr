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

const ALL_MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("migrations/0001_init.sql"),
    },
    Migration {
        version: 2,
        sql: include_str!("migrations/0002_key_packages.sql"),
    },
    Migration {
        version: 3,
        sql: include_str!("migrations/0003_contact_cards.sql"),
    },
    Migration {
        version: 4,
        sql: include_str!("migrations/0004_outbox_message_id.sql"),
    },
    Migration {
        version: 5,
        sql: include_str!("migrations/0005_contact_group_link.sql"),
    },
    Migration {
        version: 6,
        sql: include_str!("migrations/0006_history_search.sql"),
    },
    Migration {
        version: 7,
        sql: include_str!("migrations/0007_messages_envelope_id.sql"),
    },
];

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
    fn fresh_db_runs_migrations_to_latest() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        apply(&mut conn).unwrap();
        let v: u32 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        // Update this constant whenever a new migration is added.
        let expected = ALL_MIGRATIONS.iter().map(|m| m.version).max().unwrap();
        assert_eq!(v, expected);
    }

    #[test]
    fn re_applying_is_idempotent() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        apply(&mut conn).unwrap();
        apply(&mut conn).unwrap();
        apply(&mut conn).unwrap();
        // Row count in schema_version should equal the number of migrations.
        let rows: u32 = conn
            .query_row("SELECT COUNT(*) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        let expected = u32::try_from(ALL_MIGRATIONS.len()).unwrap();
        assert_eq!(rows, expected);
    }

    #[test]
    fn migration_0004_adds_message_id_column_and_unique_index() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        apply(&mut conn).unwrap();

        // message_id column present on outbox
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info('outbox')")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(std::result::Result::unwrap)
            .collect();
        assert!(
            cols.iter().any(|c| c == "message_id"),
            "migration 0004 must add message_id column; got {cols:?}"
        );

        // Unique index present
        let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'index' AND name = 'idx_outbox_target_message_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            idx_count, 1,
            "unique index idx_outbox_target_message_id must exist"
        );

        // schema_version is at latest (all migrations applied)
        let v: u32 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        let expected = ALL_MIGRATIONS.iter().map(|m| m.version).max().unwrap();
        assert_eq!(v, expected);
    }

    #[test]
    fn migration_0005_adds_group_id_column_and_index() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        apply(&mut conn).unwrap();

        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info('contacts')")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(std::result::Result::unwrap)
            .collect();
        assert!(
            cols.iter().any(|c| c == "group_id"),
            "migration 0005 must add contacts.group_id; got {cols:?}"
        );

        let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'index' AND name = 'idx_contacts_group_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx_count, 1, "idx_contacts_group_id must exist");

        let v: u32 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, ALL_MIGRATIONS.iter().map(|m| m.version).max().unwrap());
    }

    #[test]
    fn migration_0006_adds_history_search_schema() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        apply(&mut conn).unwrap();

        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info('messages')")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(std::result::Result::unwrap)
            .collect();
        for col in ["body_text", "mls_generation", "ts_daemon_recv"] {
            assert!(
                cols.iter().any(|c| c == col),
                "messages.{col} must exist; got {cols:?}"
            );
        }

        let read_state_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='table' AND name='read_state'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(read_state_exists, 1, "read_state table must exist");

        let fts_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='table' AND name='messages_fts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts_exists, 1, "messages_fts virtual table must exist");

        for idx in ["idx_messages_group_gen", "idx_messages_ts_recv"] {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master \
                     WHERE type='index' AND name=?1",
                    [idx],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "index {idx} must exist");
        }

        for trig in ["messages_ai_text", "messages_ad_text", "messages_au_text"] {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master \
                     WHERE type='trigger' AND name=?1",
                    [trig],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "trigger {trig} must exist");
        }

        let v: u32 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, ALL_MIGRATIONS.iter().map(|m| m.version).max().unwrap());
    }

    #[test]
    fn migration_0007_adds_envelope_id_column_trigger_and_unique_index() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        apply(&mut conn).unwrap();

        // Column exists on messages.
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info('messages')")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(std::result::Result::unwrap)
            .collect();
        assert!(
            cols.iter().any(|c| c == "envelope_id"),
            "messages.envelope_id must exist; got {cols:?}"
        );

        // Trigger present.
        let trig: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='trigger' AND name='messages_envelope_id_shape'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(trig, 1, "messages_envelope_id_shape trigger must exist");

        // Unique index present.
        let idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='index' AND name='messages_group_envelope_uniq'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx, 1, "messages_group_envelope_uniq index must exist");

        // Trigger rejects a bad insert (8-byte envelope_id — wrong shape).
        let err = conn.execute(
            "INSERT INTO messages \
             (group_id, sender, envelope_id, ts, kind, body_blob) \
             VALUES (?1, ?2, ?3, 0, 'text', NULL)",
            rusqlite::params![[0u8; 32], [0u8; 32], [0u8; 8]],
        );
        assert!(err.is_err(), "trigger must reject non-16-byte envelope_id");

        // A correctly-shaped insert (16-byte envelope_id) must succeed.
        conn.execute(
            "INSERT INTO messages \
             (group_id, sender, envelope_id, ts, kind, body_blob) \
             VALUES (?1, ?2, ?3, 1, 'text', NULL)",
            rusqlite::params![[0u8; 32], [0u8; 32], [0u8; 16]],
        )
        .unwrap();
    }

    #[test]
    fn migration_creates_expected_tables() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        apply(&mut conn).unwrap();
        for table in [
            "contact_cards",
            "contacts",
            "identity",
            "key_packages",
            "mailboxes",
            "messages",
            "mls_groups",
            "onion_addresses",
            "outbox",
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
