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
    Migration {
        version: 8,
        sql: include_str!("migrations/0008_mailbox_status_and_outbox_target_kind.sql"),
    },
    Migration {
        version: 9,
        sql: include_str!("migrations/0009_self_card_state.sql"),
    },
    Migration {
        version: 10,
        sql: include_str!("migrations/0010_outstanding_invites.sql"),
    },
    Migration {
        version: 11,
        sql: include_str!("migrations/0011_contacts_hidden.sql"),
    },
    Migration {
        version: 12,
        sql: include_str!("migrations/0012_outstanding_invites_provider.sql"),
    },
    Migration {
        version: 13,
        sql: include_str!("migrations/0013_contacts_muted.sql"),
    },
    Migration {
        version: 14,
        sql: include_str!("migrations/0014_passphrase_audit.sql"),
    },
    Migration {
        version: 15,
        sql: include_str!("migrations/0015_attachments.sql"),
    },
    Migration {
        version: 16,
        sql: include_str!("migrations/0016_attachment_deposits.sql"),
    },
    Migration {
        version: 17,
        sql: include_str!("migrations/0017_pending_welcomes.sql"),
    },
    Migration {
        version: 18,
        sql: include_str!("migrations/0018_first_contact_acks.sql"),
    },
    Migration {
        version: 19,
        sql: include_str!("migrations/0019_pending_welcome_failed.sql"),
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

    // Propagate a read/conversion failure rather than masking it as 0 — a
    // hidden error must not bypass the downgrade guard below by looking like a
    // fresh DB. (`CREATE TABLE IF NOT EXISTS` above guarantees the table exists,
    // and `COALESCE(MAX(version), 0)` yields 0 for an empty table, so the happy
    // path still reads 0 on a genuinely fresh DB.)
    let current: u32 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |r| r.get(0),
        )
        .map_err(|e| {
            crate::error::CoreError::Storage(crate::storage::StorageErrorKind::Other(format!(
                "read schema_version: {e}"
            )))
        })?;

    // Schema-downgrade guard: refuse to operate on a DB written by a newer
    // binary. Silently running an older binary against a future schema risks
    // corrupting data we don't understand. Projects to StorageError on the wire.
    let max_known = ALL_MIGRATIONS.iter().map(|m| m.version).max().unwrap_or(0);
    if current > max_known {
        return Err(crate::error::CoreError::Storage(
            crate::storage::StorageErrorKind::SchemaTooNew {
                found: current,
                max_known,
            },
        ));
    }

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
    use crate::storage::pool::Pool;

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

        // Unique index present (migration 0008 replaced the original 0004 index
        // with a wider composite index; check that some unique outbox index exists)
        let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'index' AND tbl_name = 'outbox' AND name LIKE 'idx_outbox_%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            idx_count >= 1,
            "at least one outbox index must exist after migrations"
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
    fn migration_0008_adds_mailbox_status_columns() {
        let pool = Pool::in_memory();
        pool.with(|c| {
            let cols: Vec<String> = c
                .prepare("PRAGMA table_info(mailboxes)")
                .unwrap()
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            for want in ["status", "last_poll_at", "last_error_at", "last_error_kind"] {
                assert!(cols.iter().any(|c| c == want), "missing column: {want}");
            }
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn migration_0008_adds_outbox_target_kind_columns() {
        let pool = Pool::in_memory();
        pool.with(|c| {
            let cols: Vec<String> = c
                .prepare("PRAGMA table_info(outbox)")
                .unwrap()
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            for want in ["target_kind", "mailbox_id"] {
                assert!(cols.iter().any(|c| c == want), "missing column: {want}");
            }
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn migration_0008_replaces_outbox_unique_index() {
        let pool = Pool::in_memory();
        pool.with(|c| {
            let names: Vec<String> = c
                .prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='outbox'")
                .unwrap()
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            assert!(
                names
                    .iter()
                    .any(|n| n == "idx_outbox_target_message_kind_mailbox"),
                "expected new composite unique index, got: {names:?}"
            );
            assert!(
                !names.iter().any(|n| n == "idx_outbox_target_message_id"),
                "old unique index must be dropped"
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn migration_0009_creates_self_card_state_singleton() {
        let pool = Pool::in_memory();
        pool.with(|c| {
            let v: i64 = c
                .query_row(
                    "SELECT version FROM self_card_state WHERE id = 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(v, 0);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn migration_0013_adds_contacts_muted_column() {
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
            cols.iter().any(|c| c == "muted"),
            "migration 0013 must add contacts.muted; got {cols:?}"
        );
        // Default for existing rows must be 0
        conn.execute(
            "INSERT INTO contacts (identity_pubkey, display_name, added_at, group_id, hidden) \
             VALUES (?1, NULL, 0, X'', 0)",
            rusqlite::params![[0u8; 32]],
        )
        .unwrap();
        let muted: i64 = conn
            .query_row(
                "SELECT muted FROM contacts WHERE identity_pubkey = ?1",
                rusqlite::params![[0u8; 32]],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(muted, 0);
    }

    #[test]
    fn migration_0014_creates_passphrase_audit_table() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        apply(&mut conn).unwrap();

        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='table' AND name='passphrase_audit'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1, "passphrase_audit table must exist");

        // CHECK constraint enforces the three valid values.
        let bad = conn.execute(
            "INSERT INTO passphrase_audit (ts_unix, outcome) VALUES (1, 'bogus')",
            [],
        );
        assert!(bad.is_err(), "CHECK constraint must reject invalid outcome");

        for outcome in ["changed", "rolled_back", "recovered"] {
            conn.execute(
                "INSERT INTO passphrase_audit (ts_unix, outcome) VALUES (?1, ?2)",
                rusqlite::params![1_700_000_000_i64, outcome],
            )
            .unwrap();
        }

        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM passphrase_audit", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 3);
    }

    #[test]
    fn refuses_db_newer_than_binary() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        apply(&mut conn).unwrap(); // bring to latest known
        let max = ALL_MIGRATIONS.iter().map(|m| m.version).max().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO schema_version (version) VALUES (?1)",
            [max + 1],
        )
        .unwrap();
        let err = apply(&mut conn).unwrap_err();
        assert!(matches!(
            err,
            crate::error::CoreError::Storage(crate::storage::StorageErrorKind::SchemaTooNew { .. })
        ));
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
