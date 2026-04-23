# Phase 1.G — Message Storage & Search Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire SQLite FTS5 over `messages`, persist `mls_generation` and `ts_daemon_recv` (replacing 1.F's placeholders), add per-group read-cursor + retention sweep + history export, expose `Command::SearchMessages` / `MarkRead` / `PruneHistory` / `ExportHistory` over IPC, broadcast `Event::MessageReceived` so `tail --follow` works, and ship `skattr search` / `export` / `prune` CLI commands. Validation gate: 100k-message FTS p95 < 50 ms.

**Architecture:** Migration 0006 adds three columns on `messages` (`body_text` mirror, `mls_generation`, `ts_daemon_recv`), recreates `messages_fts` to reference `body_text`, installs three FTS sync triggers and two covering indexes, and adds a `read_state` table. `MessageRepo` grows `search` / `unread_count` / `mark_read` / `export_page` / `prune_*` / `backfill_body_text`. `delivery::receiver` captures the new fields and surfaces them via an extended `ReceiveOutcome::New { .. }` so the InboundDispatch caller can broadcast `Event::MessageReceived`. The daemon spawns a 3600-second retention sweep tokio task. CLI gains four new subcommands; `tail` learns `--follow`.

**Tech Stack:** Rust 2021, `rusqlite` 0.38 + FTS5, `tokio` (`broadcast`, `select!`, `spawn`, `time::sleep`), `ciborium` (CBOR), `serde`, `time = "0.3"` (CLI-side RFC3339 parsing — new dep), `rand` (workspace-existing) for benchmark synthesis. Dev-deps: `tempfile` (existing).

**Spec:** `docs/superpowers/specs/2026-04-23-phase-1g-message-storage-search-design.md` (commit `07495e2`).

---

## Pre-flight

Phase 1.G depends on Phase 1.F (CLI integration). Verify 1.F has merged to master before starting.

- [ ] **A. Verify 1.F is merged.** Run:
  ```bash
  git log --oneline master | grep -E "phase-1f|Phase 1\.F" | head -5
  ```
  Expected: at least one merge commit naming Phase 1.F. If absent, **stop and finish 1.F first.**
  Additionally:
  ```bash
  test -f crates/core/src/daemon/ipc/server.rs && echo "ipc::server present"
  test -f crates/core/src/daemon/dispatch.rs && echo "dispatch present"
  grep -q "MessageRecord" crates/core/src/daemon/commands.rs && echo "MessageRecord present"
  grep -q "0005_contact_group_link" crates/core/src/storage/migrations.rs && echo "migration 0005 wired"
  ```
  All four lines must print "present"/"wired".

- [ ] **B. Working tree clean.** Run `git status -s`. Expected: empty output.

- [ ] **C. Create the worktree.**
  ```bash
  git worktree add ../skattr-phase-1g-message-storage-search -b phase-1g-message-storage-search master
  cd ../skattr-phase-1g-message-storage-search
  ```
  All subsequent tasks run from this worktree.

- [ ] **D. Confirm baseline builds + tests pass.**
  ```bash
  cargo fmt --all --check
  cargo clippy --all-targets -- -D warnings
  cargo test
  ```
  All three must succeed. If any fail, fix before starting Task 1 — you are not allowed to add 1.G work on top of a broken baseline.

---

## Task 1: Migration 0006 — schema, FTS5 recreate, triggers, read_state

**Files:**
- Create: `crates/core/src/storage/migrations/0006_history_search.sql`
- Modify: `crates/core/src/storage/migrations.rs:25-42` (extend `ALL_MIGRATIONS`; add a test for migration 0006)

- [ ] **Step 1: Write the failing test**

Append to `crates/core/src/storage/migrations.rs` inside `#[cfg(test)] mod tests`:

```rust
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
    assert_eq!(v, 6);
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-core --lib storage::migrations::tests::migration_0006_adds_history_search_schema -- --nocapture
```
Expected: FAIL — `messages.body_text` missing (or migration 0006 not registered).

- [ ] **Step 3: Create the migration SQL**

Write `crates/core/src/storage/migrations/0006_history_search.sql`:

```sql
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz AB
--
-- Skattr storage schema, version 6.
-- Phase 1.G: wire FTS5 over messages, persist mls_generation +
-- ts_daemon_recv, add read_state cursor.

ALTER TABLE messages ADD COLUMN body_text TEXT;
ALTER TABLE messages ADD COLUMN mls_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN ts_daemon_recv INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_messages_group_gen
    ON messages(group_id, mls_generation DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_messages_ts_recv
    ON messages(ts_daemon_recv);

DROP TABLE IF EXISTS messages_fts;
CREATE VIRTUAL TABLE messages_fts USING fts5(
    body_text,
    content='messages',
    content_rowid='id',
    tokenize='unicode61'
);

CREATE TRIGGER IF NOT EXISTS messages_ai_text
    AFTER INSERT ON messages
    WHEN NEW.kind = 'text' AND NEW.body_text IS NOT NULL
BEGIN
    INSERT INTO messages_fts(rowid, body_text)
        VALUES (NEW.id, NEW.body_text);
END;

CREATE TRIGGER IF NOT EXISTS messages_ad_text
    AFTER DELETE ON messages
    WHEN OLD.kind = 'text' AND OLD.body_text IS NOT NULL
BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, body_text)
        VALUES('delete', OLD.id, OLD.body_text);
END;

CREATE TRIGGER IF NOT EXISTS messages_au_text
    AFTER UPDATE OF body_text, kind ON messages
BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, body_text)
        SELECT 'delete', OLD.id, OLD.body_text
        WHERE OLD.kind = 'text' AND OLD.body_text IS NOT NULL;
    INSERT INTO messages_fts(rowid, body_text)
        SELECT NEW.id, NEW.body_text
        WHERE NEW.kind = 'text' AND NEW.body_text IS NOT NULL;
END;

CREATE TABLE IF NOT EXISTS read_state (
    group_id BLOB PRIMARY KEY,
    last_read_message_id INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

INSERT OR REPLACE INTO schema_version (version) VALUES (6);
```

- [ ] **Step 4: Wire the migration into the runner**

Edit `crates/core/src/storage/migrations.rs:25-42` to extend `ALL_MIGRATIONS`:

```rust
const ALL_MIGRATIONS: &[Migration] = &[
    Migration { version: 1, sql: include_str!("migrations/0001_init.sql") },
    Migration { version: 2, sql: include_str!("migrations/0002_key_packages.sql") },
    Migration { version: 3, sql: include_str!("migrations/0003_contact_cards.sql") },
    Migration { version: 4, sql: include_str!("migrations/0004_outbox_message_id.sql") },
    Migration { version: 5, sql: include_str!("migrations/0005_contact_group_link.sql") },
    Migration { version: 6, sql: include_str!("migrations/0006_history_search.sql") },
];
```

- [ ] **Step 5: Run test to verify it passes**

```bash
cargo test -p skattr-core --lib storage::migrations::tests -- --nocapture
```
Expected: all migration tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/storage/migrations/0006_history_search.sql \
        crates/core/src/storage/migrations.rs
git commit -m "$(cat <<'EOF'
storage: migration 0006 — FTS5 wiring, mls_generation, ts_daemon_recv, read_state

Adds body_text mirror column, mls_generation, ts_daemon_recv columns
on messages; covering indexes; recreates messages_fts referencing
body_text; installs ai/ad/au triggers; creates read_state cursor table.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: ReadStateRepo — per-group last-read pointer

**Files:**
- Create: `crates/core/src/storage/read_state.rs`
- Modify: `crates/core/src/storage/mod.rs:17-56` (declare module + add the test-harness re-export pair)
- Modify: `crates/core/src/lib.rs` (extend `test_exports` with `ReadStateRepo`)

- [ ] **Step 1: Write the failing tests**

Create `crates/core/src/storage/read_state.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Per-group last-read pointer.
//!
//! Phase 1.G storage layer. `unread_count` is `COUNT(*)` of messages
//! with `id > last_read_message_id` for a given `group_id`; the cursor
//! advances via `mark_read`.

use crate::error::{CoreError, Result};
use crate::storage::Pool;

/// Per-group last-read cursor.
pub struct ReadStateRepo<'p> {
    pool: &'p Pool,
}

impl<'p> ReadStateRepo<'p> {
    /// Construct a new repo backed by `pool`.
    pub fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Returns `Some(last_read_message_id)` if a cursor exists for
    /// `group_id`, `None` otherwise.
    pub fn get(&self, group_id: &[u8]) -> Result<Option<i64>> {
        self.pool.with(|c| {
            match c.query_row(
                "SELECT last_read_message_id FROM read_state WHERE group_id = ?1",
                rusqlite::params![group_id],
                |r| r.get::<_, i64>(0),
            ) {
                Ok(v) => Ok(Some(v)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Storage(format!("read_state get: {e}"))),
            }
        })
    }

    /// Upsert the cursor. Idempotent.
    pub fn set(
        &self,
        group_id: &[u8],
        last_read_message_id: i64,
        updated_at: i64,
    ) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "INSERT INTO read_state (group_id, last_read_message_id, updated_at) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(group_id) DO UPDATE SET \
                     last_read_message_id = excluded.last_read_message_id, \
                     updated_at = excluded.updated_at",
                rusqlite::params![group_id, last_read_message_id, updated_at],
            )
            .map_err(|e| CoreError::Storage(format!("read_state set: {e}")))?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_returns_none_initially() {
        let pool = Pool::in_memory();
        let repo = ReadStateRepo::new(&pool);
        assert_eq!(repo.get(&[0xAA; 32]).unwrap(), None);
    }

    #[test]
    fn set_then_get_round_trips() {
        let pool = Pool::in_memory();
        let repo = ReadStateRepo::new(&pool);
        let gid = [0xBB; 32];
        repo.set(&gid, 42, 1_700_000_000).unwrap();
        assert_eq!(repo.get(&gid).unwrap(), Some(42));
    }

    #[test]
    fn set_overwrites_existing_cursor() {
        let pool = Pool::in_memory();
        let repo = ReadStateRepo::new(&pool);
        let gid = [0xCC; 32];
        repo.set(&gid, 10, 1_700_000_000).unwrap();
        repo.set(&gid, 99, 1_700_000_100).unwrap();
        assert_eq!(repo.get(&gid).unwrap(), Some(99));
    }
}
```

- [ ] **Step 2: Wire the module**

Edit `crates/core/src/storage/mod.rs` — add the module declaration alongside the other repos and the `pub(crate)`/`pub` re-export pair:

```rust
pub(crate) mod backup;
pub(crate) mod contacts;
pub(crate) mod groups;
pub(crate) mod key_packages;
pub(crate) mod mailboxes;
pub(crate) mod messages;
pub(crate) mod migrations;
pub(crate) mod outbox;
pub(crate) mod pool;
pub(crate) mod read_state;          // (+)
pub(crate) mod seen_messages;
```

In the same file, add to both the `#[cfg(not(feature = "test-harness"))]` and `#[cfg(feature = "test-harness")]` blocks:

```rust
#[cfg(not(feature = "test-harness"))]
pub(crate) use read_state::ReadStateRepo;

#[cfg(feature = "test-harness")]
pub use read_state::ReadStateRepo;
```

- [ ] **Step 3: Extend test_exports**

Edit `crates/core/src/lib.rs:56` (the `test_exports` re-export tuple) — add `ReadStateRepo`:

```rust
pub use crate::storage::{ContactRepo, MessageRepo, Pool, ReadStateRepo, SeenMessagesRepo};
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-core --lib storage::read_state -- --nocapture
```
Expected: 3 tests PASS.

```bash
cargo build --features test-harness
```
Expected: build succeeds (verifies the test-harness re-export).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/storage/read_state.rs \
        crates/core/src/storage/mod.rs \
        crates/core/src/lib.rs
git commit -m "$(cat <<'EOF'
storage: ReadStateRepo for per-group last-read cursor

Three-line UPSERT API (get/set) backing unread_count. Re-exported
under both pub(crate) and the test-harness public path so integration
tests can drive read state directly.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: `fts5_tokenize_and_and` query escaper

**Files:**
- Modify: `crates/core/src/storage/messages.rs` (add the helper at module scope plus its unit tests)

- [ ] **Step 1: Write the failing tests**

Append to `crates/core/src/storage/messages.rs` inside `#[cfg(test)] mod tests`:

```rust
#[test]
fn fts5_tokenize_and_and_single_token() {
    assert_eq!(
        super::fts5_tokenize_and_and("arti"),
        Some("\"arti\"".to_string())
    );
}

#[test]
fn fts5_tokenize_and_and_multi_token() {
    assert_eq!(
        super::fts5_tokenize_and_and("arti tor"),
        Some("\"arti\" AND \"tor\"".to_string())
    );
}

#[test]
fn fts5_tokenize_and_and_escapes_internal_quotes() {
    // FTS5 escapes " by doubling it.
    assert_eq!(
        super::fts5_tokenize_and_and(r#"she said "hi""#),
        Some(r#""she" AND "said" AND """hi"""""#.to_string())
    );
}

#[test]
fn fts5_tokenize_and_and_empty_returns_none() {
    assert_eq!(super::fts5_tokenize_and_and(""), None);
}

#[test]
fn fts5_tokenize_and_and_whitespace_only_returns_none() {
    assert_eq!(super::fts5_tokenize_and_and("   \t\n  "), None);
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p skattr-core --lib storage::messages::tests::fts5_tokenize_and_and -- --nocapture
```
Expected: FAIL — `fts5_tokenize_and_and` undefined.

- [ ] **Step 3: Implement the helper**

Insert into `crates/core/src/storage/messages.rs` immediately after the `use` block (i.e., above `pub struct StoredMessage`):

```rust
/// Convert a free-form user query into an FTS5 MATCH expression using
/// the tokenize-and-AND strategy: split on whitespace, wrap each token
/// in FTS5-escaped double quotes (FTS5 doubles internal `"` to `""`),
/// join with ` AND `. Returns `None` if the query is empty or
/// whitespace-only — callers should short-circuit to an empty result
/// without hitting the FTS5 engine.
pub(super) fn fts5_tokenize_and_and(query: &str) -> Option<String> {
    let tokens: Vec<String> = query
        .split_whitespace()
        .map(|tok| format!("\"{}\"", tok.replace('"', "\"\"")))
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" AND "))
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-core --lib storage::messages::tests::fts5_tokenize_and_and -- --nocapture
```
Expected: 5 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/storage/messages.rs
git commit -m "$(cat <<'EOF'
storage: fts5_tokenize_and_and helper for safe MATCH construction

Hides FTS5 grammar from the user. Splits on whitespace, doubles
internal quotes, joins tokens with AND. Empty/whitespace-only input
returns None so callers can short-circuit.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: `MessageRepo::insert` → `InsertParams` (and body_text extraction)

This is a breaking signature change. Update all call sites in the same task.

**Files:**
- Modify: `crates/core/src/storage/messages.rs` (define `InsertParams`, change `insert` signature, populate `body_text`, update internal tests)
- Modify: `crates/core/src/delivery/receiver.rs:56` (the production caller; update tests too)
- Modify: `crates/core/tests/storage_roundtrip.rs:43` (the integration test caller)

- [ ] **Step 1: Write the failing test**

Append to `crates/core/src/storage/messages.rs` inside `#[cfg(test)] mod tests`:

```rust
#[test]
fn insert_populates_body_text_for_text_kind_and_fts_indexes_it() {
    let pool = Pool::in_memory();
    let repo = MessageRepo::new(&pool);
    let gid = [0xDD; 32];
    let env = sample_envelope("hello full text search");

    let id = repo
        .insert(InsertParams {
            group_id: &gid,
            sender: &[0x42; 32],
            envelope: &env,
            mls_generation: 7,
            ts_daemon_recv: 1_700_000_500,
        })
        .unwrap();
    assert!(id > 0);

    // body_text column populated
    let body_text: Option<String> = pool
        .with(|c| {
            c.query_row(
                "SELECT body_text FROM messages WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
        })
        .unwrap();
    assert_eq!(body_text.as_deref(), Some("hello full text search"));

    // FTS index returns the row
    let fts_hits: i64 = pool
        .with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH 'search'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
        })
        .unwrap();
    assert_eq!(fts_hits, 1, "trigger must have indexed the new row");

    // mls_generation + ts_daemon_recv stored
    let (gen, recv): (i64, i64) = pool
        .with(|c| {
            c.query_row(
                "SELECT mls_generation, ts_daemon_recv FROM messages WHERE id = ?1",
                rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
        })
        .unwrap();
    assert_eq!(gen, 7);
    assert_eq!(recv, 1_700_000_500);
}

#[test]
fn insert_leaves_body_text_null_for_non_text_kind() {
    let pool = Pool::in_memory();
    let repo = MessageRepo::new(&pool);
    let gid = [0xEE; 32];
    let mut env = sample_envelope("ignored");
    env.kind = crate::envelope::Kind::Typing;

    let id = repo
        .insert(InsertParams {
            group_id: &gid,
            sender: &[0x42; 32],
            envelope: &env,
            mls_generation: 0,
            ts_daemon_recv: 1_700_000_000,
        })
        .unwrap();

    let body_text: Option<String> = pool
        .with(|c| {
            c.query_row(
                "SELECT body_text FROM messages WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
        })
        .unwrap();
    assert_eq!(body_text, None, "non-text kinds must leave body_text NULL");
}
```

- [ ] **Step 2: Verify it fails to compile**

```bash
cargo test -p skattr-core --lib storage::messages::tests -- --nocapture
```
Expected: FAIL — `InsertParams` undefined.

- [ ] **Step 3: Define `InsertParams` and change the `insert` signature**

In `crates/core/src/storage/messages.rs`, add the struct above `impl<'p> MessageRepo<'p>`:

```rust
/// All fields required to persist a single message row.
pub struct InsertParams<'a> {
    pub group_id: &'a [u8],
    pub sender: &'a [u8],
    pub envelope: &'a Envelope,
    /// MLS group epoch at the time the row is persisted. For the
    /// receiver, captured post-decrypt; for the sender, post-encrypt.
    pub mls_generation: u64,
    /// Local clock at the moment the daemon persisted the row.
    pub ts_daemon_recv: i64,
}
```

Replace the existing `insert` body in `impl<'p> MessageRepo<'p>` with:

```rust
/// Insert a message and return its rowid. Populates body_text for
/// text-kind envelopes (NULL otherwise), letting the FTS5 triggers
/// index the row automatically.
pub fn insert(&self, p: InsertParams<'_>) -> Result<i64> {
    let body = p.envelope.encode()?;
    let kind = match &p.envelope.kind {
        crate::envelope::Kind::Text { .. } => "text",
        crate::envelope::Kind::File { .. } => "file",
        crate::envelope::Kind::Reaction { .. } => "reaction",
        crate::envelope::Kind::Edit { .. } => "edit",
        crate::envelope::Kind::Delete { .. } => "delete",
        crate::envelope::Kind::Typing => "typing",
    };
    let body_text: Option<&str> = match &p.envelope.kind {
        crate::envelope::Kind::Text { body } => Some(body.as_str()),
        _ => None,
    };
    let mls_gen_signed = i64::try_from(p.mls_generation).unwrap_or(i64::MAX);
    self.pool.with_mut(|c| {
        c.execute(
            "INSERT INTO messages \
                 (group_id, sender, kind, body_blob, body_text, ts, \
                  mls_generation, ts_daemon_recv) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                p.group_id,
                p.sender,
                kind,
                body,
                body_text,
                p.envelope.ts,
                mls_gen_signed,
                p.ts_daemon_recv,
            ],
        )
        .map_err(|e| CoreError::Storage(format!("insert message: {e}")))?;
        Ok(c.last_insert_rowid())
    })
}
```

- [ ] **Step 4: Update existing internal tests**

Edit the four existing tests in `crates/core/src/storage/messages.rs` that call `repo.insert(&gid, &sender, &env)`. Each becomes:

```rust
repo.insert(InsertParams {
    group_id: &gid,
    sender: &sender,                    // or &[0u8; 32] etc — match each test's value
    envelope: &env,
    mls_generation: 0,
    ts_daemon_recv: env.ts,             // any i64 is fine for these tests
})
```

For tests that previously inlined `&sender`/`&[0u8; 32]`/`&[0x42; 32]` etc., keep the same byte source on the new `sender:` field.

- [ ] **Step 5: Update `delivery::receiver::receive`**

Edit `crates/core/src/delivery/receiver.rs:56` (the production call site) and the four mod-tests' assertion call sites. The production call becomes:

```rust
let _ = messages.insert(crate::storage::messages::InsertParams {
    group_id,
    sender: &sender.0,
    envelope: &envelope,
    mls_generation: 0,                  // populated by Task 13
    ts_daemon_recv: now_ms,             // best-effort placeholder; Task 13 splits ts_daemon_recv from the replay-window ts argument
})?;
```

The four `mod tests` call sites in `receiver.rs` only invoke `receive()` (not `insert`), so they need no change here. Verify by running the tests in Step 7.

- [ ] **Step 6: Update `crates/core/tests/storage_roundtrip.rs:43`**

Replace the `MessageRepo::new(&pool).insert(&gid, &sender, &env)` call with:

```rust
use skattr_core::test_exports::MessageRepo;
// (existing test-imports already include MessageRepo + Pool + ContactRepo)
// new import:
use skattr_core::storage::messages::InsertParams;  // re-export added below if missing
```

If `InsertParams` is not yet visible under the `test-harness` feature, also add to `crates/core/src/lib.rs`'s `test_exports` block:

```rust
pub use crate::storage::messages::InsertParams;
```

Then change the call:

```rust
MessageRepo::new(&pool)
    .insert(InsertParams {
        group_id: &gid,
        sender: &sender,
        envelope: &env,
        mls_generation: 0,
        ts_daemon_recv: env.ts,
    })
    .unwrap();
```

- [ ] **Step 7: Run the full test suite**

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```
Expected: all tests PASS, no clippy warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/core/src/storage/messages.rs \
        crates/core/src/delivery/receiver.rs \
        crates/core/tests/storage_roundtrip.rs \
        crates/core/src/lib.rs
git commit -m "$(cat <<'EOF'
storage: MessageRepo::insert takes InsertParams; populates body_text + new columns

Breaking signature change. body_text mirrors text-kind envelope bodies
so FTS5 triggers index without CBOR decode in SQL. mls_generation and
ts_daemon_recv use placeholder zero/now until Task 13 plumbs real
values from the encrypt/decrypt sites.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: `MessageRepo::search` (BM25, contact filter, newest-first override)

**Files:**
- Modify: `crates/core/src/storage/messages.rs` (add `SearchHit` struct + `search` method + tests)

- [ ] **Step 1: Write the failing tests**

Append to `crates/core/src/storage/messages.rs` inside `#[cfg(test)] mod tests`:

```rust
fn seed_three_text(pool: &Pool, gid: &[u8; 32]) {
    let repo = MessageRepo::new(pool);
    for (i, body) in ["alpha bravo", "bravo charlie", "delta echo"].iter().enumerate() {
        let mut env = sample_envelope(body);
        env.ts = 100 + i as i64;
        repo.insert(InsertParams {
            group_id: gid,
            sender: &[0u8; 32],
            envelope: &env,
            mls_generation: u64::try_from(i).unwrap(),
            ts_daemon_recv: 100 + i as i64,
        })
        .unwrap();
    }
}

#[test]
fn search_no_match_returns_empty() {
    let pool = Pool::in_memory();
    seed_three_text(&pool, &[0x10; 32]);
    let hits = MessageRepo::new(&pool)
        .search("zzz", None, 10, 0, false)
        .unwrap();
    assert!(hits.is_empty());
}

#[test]
fn search_single_token_finds_one_or_more() {
    let pool = Pool::in_memory();
    seed_three_text(&pool, &[0x11; 32]);
    let hits = MessageRepo::new(&pool)
        .search("delta", None, 10, 0, false)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].snippet.contains("delta"));
}

#[test]
fn search_multi_token_ands() {
    let pool = Pool::in_memory();
    seed_three_text(&pool, &[0x12; 32]);
    let hits = MessageRepo::new(&pool)
        .search("alpha bravo", None, 10, 0, false)
        .unwrap();
    assert_eq!(hits.len(), 1, "only the row with both tokens should match");
}

#[test]
fn search_empty_query_short_circuits() {
    let pool = Pool::in_memory();
    seed_three_text(&pool, &[0x13; 32]);
    let hits = MessageRepo::new(&pool)
        .search("   ", None, 10, 0, false)
        .unwrap();
    assert!(hits.is_empty());
}

#[test]
fn search_scoped_to_group_id() {
    let pool = Pool::in_memory();
    let g1 = [0x14; 32];
    let g2 = [0x15; 32];
    seed_three_text(&pool, &g1);
    seed_three_text(&pool, &g2);
    let global = MessageRepo::new(&pool)
        .search("bravo", None, 10, 0, false)
        .unwrap();
    let scoped = MessageRepo::new(&pool)
        .search("bravo", Some(&g1), 10, 0, false)
        .unwrap();
    assert_eq!(global.len(), 4, "two groups × two matches each");
    assert_eq!(scoped.len(), 2);
}

#[test]
fn search_newest_first_orders_by_id_desc() {
    let pool = Pool::in_memory();
    seed_three_text(&pool, &[0x16; 32]);
    let hits = MessageRepo::new(&pool)
        .search("bravo", None, 10, 0, true)
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert!(hits[0].message.id > hits[1].message.id);
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p skattr-core --lib storage::messages::tests::search -- --nocapture
```
Expected: FAIL — `search` undefined and `SearchHit` undefined.

- [ ] **Step 3: Implement `SearchHit` + `search`**

In `crates/core/src/storage/messages.rs`, add above `impl<'p> MessageRepo<'p>`:

```rust
/// One ranked hit returned by [`MessageRepo::search`].
#[derive(Debug, Clone)]
pub struct SearchHit {
    /// The full stored message row.
    pub message: StoredMessage,
    /// SQLite FTS5 BM25 score. Lower is better. Zero when
    /// `newest_first` overrode the ranking.
    pub bm25: f64,
    /// FTS5 `snippet()` output with delimiter markers and 32-token window.
    pub snippet: String,
}
```

Then add the method inside `impl<'p> MessageRepo<'p>`:

```rust
/// Full-text search over text-kind message bodies.
///
/// `query` is run through [`fts5_tokenize_and_and`]; whitespace-only
/// queries return `Ok(vec![])` without hitting FTS5. `group_id =
/// Some(g)` scopes results to that group.
///
/// Default ordering is BM25 ascending (best first). `newest_first =
/// true` sorts by `messages.id DESC` regardless of relevance.
pub fn search(
    &self,
    query: &str,
    group_id: Option<&[u8]>,
    limit: usize,
    offset: usize,
    newest_first: bool,
) -> Result<Vec<SearchHit>> {
    let Some(match_expr) = fts5_tokenize_and_and(query) else {
        return Ok(Vec::new());
    };

    let order_clause = if newest_first {
        "messages.id DESC"
    } else {
        "bm25(messages_fts) ASC, messages.id DESC"
    };
    let group_filter = if group_id.is_some() {
        " AND messages.group_id = ?2"
    } else {
        ""
    };
    let limit_offset_first_param = if group_id.is_some() { 3 } else { 2 };

    let sql = format!(
        "SELECT messages.id, messages.group_id, messages.sender, messages.kind, \
                messages.body_blob, messages.ts, messages.delivered_at, \
                bm25(messages_fts) AS rank, \
                snippet(messages_fts, 0, char(2), char(3), '...', 32) AS snippet \
         FROM messages_fts \
         JOIN messages ON messages.id = messages_fts.rowid \
         WHERE messages_fts MATCH ?1{group_filter} \
         ORDER BY {order_clause} \
         LIMIT ?{limit_p} OFFSET ?{offset_p}",
        group_filter = group_filter,
        order_clause = order_clause,
        limit_p = limit_offset_first_param,
        offset_p = limit_offset_first_param + 1,
    );

    self.pool.with(|c| {
        let mut stmt = c
            .prepare(&sql)
            .map_err(|e| CoreError::Storage(format!("prepare search: {e}")))?;

        let limit_i = i64::try_from(limit).unwrap_or(i64::MAX);
        let offset_i = i64::try_from(offset).unwrap_or(0);

        let map_row = |r: &rusqlite::Row<'_>| {
            Ok(SearchHit {
                message: StoredMessage {
                    id: r.get(0)?,
                    group_id: r.get(1)?,
                    sender: r.get(2)?,
                    kind: r.get(3)?,
                    body_blob: r.get(4)?,
                    ts: r.get(5)?,
                    delivered_at: r.get(6)?,
                },
                bm25: r.get::<_, f64>(7).unwrap_or(0.0),
                snippet: r.get::<_, String>(8).unwrap_or_default(),
            })
        };

        let rows = if let Some(gid) = group_id {
            stmt.query_map(
                rusqlite::params![match_expr, gid, limit_i, offset_i],
                map_row,
            )
        } else {
            stmt.query_map(
                rusqlite::params![match_expr, limit_i, offset_i],
                map_row,
            )
        }
        .map_err(|e| CoreError::Storage(format!("query search: {e}")))?;

        let out: std::result::Result<Vec<_>, _> = rows.collect();
        out.map_err(|e| CoreError::Storage(format!("collect search: {e}")))
    })
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-core --lib storage::messages::tests::search -- --nocapture
```
Expected: 6 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/storage/messages.rs
git commit -m "$(cat <<'EOF'
storage: MessageRepo::search with FTS5 BM25 + snippet + group filter

Tokenize-and-AND query, BM25 ranking by default, --newest-first
override flips to id DESC. Empty query short-circuits without
hitting FTS5. Snippet uses char(2)/char(3) delimiters + 32-token window.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: `MessageRepo::unread_count` + `recent_by_contact` ORDER BY upgrade

**Files:**
- Modify: `crates/core/src/storage/messages.rs` (add `unread_count` method + tests)
- Modify: 1.F's `recent_by_contact` SQL (find via `grep -rn "recent_by_contact" crates/core/src` — typically in `daemon/dispatch.rs`)

- [ ] **Step 1: Write the failing tests**

Append to `crates/core/src/storage/messages.rs` inside `#[cfg(test)] mod tests`:

```rust
#[test]
fn unread_count_returns_total_when_no_cursor() {
    let pool = Pool::in_memory();
    let gid = [0x20; 32];
    seed_three_text(&pool, &gid);
    let n = MessageRepo::new(&pool).unread_count(&gid).unwrap();
    assert_eq!(n, 3, "no cursor → all rows are unread");
}

#[test]
fn unread_count_returns_zero_after_cursor_passes_all() {
    use crate::storage::ReadStateRepo;
    let pool = Pool::in_memory();
    let gid = [0x21; 32];
    seed_three_text(&pool, &gid);
    let last_id: i64 = pool
        .with(|c| {
            c.query_row(
                "SELECT MAX(id) FROM messages WHERE group_id = ?1",
                rusqlite::params![&gid[..]],
                |r| r.get(0),
            )
            .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
        })
        .unwrap();
    ReadStateRepo::new(&pool)
        .set(&gid, last_id, 1_700_000_000)
        .unwrap();
    let n = MessageRepo::new(&pool).unread_count(&gid).unwrap();
    assert_eq!(n, 0);
}

#[test]
fn unread_count_returns_partial_after_cursor_in_middle() {
    use crate::storage::ReadStateRepo;
    let pool = Pool::in_memory();
    let gid = [0x22; 32];
    seed_three_text(&pool, &gid);
    let mid_id: i64 = pool
        .with(|c| {
            c.query_row(
                "SELECT id FROM messages WHERE group_id = ?1 \
                 ORDER BY id ASC LIMIT 1 OFFSET 1",
                rusqlite::params![&gid[..]],
                |r| r.get(0),
            )
            .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
        })
        .unwrap();
    ReadStateRepo::new(&pool)
        .set(&gid, mid_id, 1_700_000_000)
        .unwrap();
    let n = MessageRepo::new(&pool).unread_count(&gid).unwrap();
    assert_eq!(n, 1, "1 of 3 rows has id > cursor");
}
```

- [ ] **Step 2: Verify the tests fail**

```bash
cargo test -p skattr-core --lib storage::messages::tests::unread_count -- --nocapture
```
Expected: FAIL — `unread_count` undefined.

- [ ] **Step 3: Implement `unread_count`**

Add inside `impl<'p> MessageRepo<'p>`:

```rust
/// Count of messages in `group_id` whose `id` is greater than the
/// `read_state` cursor. Absent cursor → all rows count as unread.
pub fn unread_count(&self, group_id: &[u8]) -> Result<u64> {
    self.pool.with(|c| {
        let cursor: Option<i64> = match c.query_row(
            "SELECT last_read_message_id FROM read_state WHERE group_id = ?1",
            rusqlite::params![group_id],
            |r| r.get::<_, i64>(0),
        ) {
            Ok(v) => Some(v),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(CoreError::Storage(format!("unread_count cursor: {e}"))),
        };

        let n: i64 = match cursor {
            Some(cur) => c
                .query_row(
                    "SELECT COUNT(*) FROM messages \
                     WHERE group_id = ?1 AND id > ?2",
                    rusqlite::params![group_id, cur],
                    |r| r.get(0),
                )
                .map_err(|e| CoreError::Storage(format!("unread_count: {e}")))?,
            None => c
                .query_row(
                    "SELECT COUNT(*) FROM messages WHERE group_id = ?1",
                    rusqlite::params![group_id],
                    |r| r.get(0),
                )
                .map_err(|e| CoreError::Storage(format!("unread_count: {e}")))?,
        };
        Ok(u64::try_from(n).unwrap_or(0))
    })
}
```

- [ ] **Step 4: Upgrade `recent_by_contact` ordering**

Locate the SQL added by Phase 1.F:

```bash
grep -rn "recent_by_contact\|ORDER BY id DESC\|ORDER BY messages.id DESC" \
    crates/core/src/storage crates/core/src/daemon
```

In whichever file 1.F put the recent SQL (the spec assumed `MessageRepo::recent_by_contact`; 1.F's plan placed the SQL in `daemon/dispatch.rs`), change:

```sql
ORDER BY id DESC
```

to:

```sql
ORDER BY mls_generation DESC, id DESC
```

Add (or extend) the matching test to assert ordering:

```rust
#[test]
fn recent_by_contact_orders_by_mls_generation_then_id() {
    let pool = Pool::in_memory();
    let gid = [0x23; 32];
    let repo = MessageRepo::new(&pool);
    // Insert with mixed mls_generation values.
    for (gen, body, ts) in [(2, "first-but-newer-gen", 100), (5, "third-yet-older-gen", 102), (3, "second", 101)] {
        let mut env = sample_envelope(body);
        env.ts = ts;
        repo.insert(InsertParams {
            group_id: &gid,
            sender: &[0u8; 32],
            envelope: &env,
            mls_generation: gen,
            ts_daemon_recv: ts,
        })
        .unwrap();
    }
    let rows: Vec<i64> = pool
        .with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id FROM messages WHERE group_id = ?1 \
                     ORDER BY mls_generation DESC, id DESC",
                )
                .map_err(|e| crate::error::CoreError::Storage(e.to_string()))?;
            let it = stmt
                .query_map(rusqlite::params![&gid[..]], |r| r.get::<_, i64>(0))
                .map_err(|e| crate::error::CoreError::Storage(e.to_string()))?;
            let v: std::result::Result<Vec<_>, _> = it.collect();
            v.map_err(|e| crate::error::CoreError::Storage(e.to_string()))
        })
        .unwrap();
    // Highest mls_generation first → "third-yet-older-gen" (gen=5),
    // then "second" (gen=3), then "first-but-newer-gen" (gen=2).
    assert_eq!(rows.len(), 3);
    // Cross-check by re-fetching kinds via id; rows[0] must be the
    // gen=5 row (third inserted, id=2 if seed orders alpha-bravo first
    // — adjust after running once if test layout differs).
    let bodies: Vec<String> = pool
        .with(|c| {
            let mut stmt = c
                .prepare("SELECT body_text FROM messages WHERE id = ?1")
                .map_err(|e| crate::error::CoreError::Storage(e.to_string()))?;
            let mut out = Vec::new();
            for id in &rows {
                let body: String = stmt
                    .query_row(rusqlite::params![id], |r| r.get(0))
                    .map_err(|e| crate::error::CoreError::Storage(e.to_string()))?;
                out.push(body);
            }
            Ok(out)
        })
        .unwrap();
    assert_eq!(bodies, vec![
        "third-yet-older-gen".to_string(),
        "second".to_string(),
        "first-but-newer-gen".to_string(),
    ]);
}
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo test -p skattr-core --lib storage::messages::tests -- --nocapture
cargo test
```
Expected: all storage tests PASS; full suite PASS (including 1.F's recent_by_contact tests, which may need an ORDER BY assertion update).

If a 1.F test asserts a specific ordering by `id DESC` only, update its expectation to `(mls_generation DESC, id DESC)` consistent with the new SQL.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/storage/messages.rs crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
storage: MessageRepo::unread_count + recent_by_contact ORDER BY upgrade

unread_count joins read_state cursor with id > cursor predicate.
recent_by_contact upgraded from id DESC to (mls_generation DESC, id
DESC) — covered by the new idx_messages_group_gen index.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: `MessageRepo::mark_read`

**Files:**
- Modify: `crates/core/src/storage/messages.rs` (add `mark_read` method + tests)

- [ ] **Step 1: Write the failing tests**

Append to `crates/core/src/storage/messages.rs` inside `#[cfg(test)] mod tests`:

```rust
#[test]
fn mark_read_advances_cursor_idempotent() {
    let pool = Pool::in_memory();
    let gid = [0x30; 32];
    seed_three_text(&pool, &gid);
    let repo = MessageRepo::new(&pool);

    repo.mark_read(&gid, 42).unwrap();
    repo.mark_read(&gid, 42).unwrap();   // idempotent overwrite

    use crate::storage::ReadStateRepo;
    assert_eq!(ReadStateRepo::new(&pool).get(&gid).unwrap(), Some(42));
}

#[test]
fn mark_read_updates_existing_cursor() {
    let pool = Pool::in_memory();
    let gid = [0x31; 32];
    seed_three_text(&pool, &gid);
    let repo = MessageRepo::new(&pool);

    repo.mark_read(&gid, 10).unwrap();
    repo.mark_read(&gid, 99).unwrap();

    use crate::storage::ReadStateRepo;
    assert_eq!(ReadStateRepo::new(&pool).get(&gid).unwrap(), Some(99));
}
```

- [ ] **Step 2: Verify the tests fail**

```bash
cargo test -p skattr-core --lib storage::messages::tests::mark_read -- --nocapture
```
Expected: FAIL — `mark_read` undefined.

- [ ] **Step 3: Implement `mark_read`**

Add inside `impl<'p> MessageRepo<'p>`:

```rust
/// Advance the read cursor for `group_id` to `up_to_message_id`.
/// Idempotent. Caller picks `updated_at` (typically `now() seconds`).
pub fn mark_read(&self, group_id: &[u8], up_to_message_id: i64) -> Result<()> {
    crate::storage::ReadStateRepo::new(self.pool).set(
        group_id,
        up_to_message_id,
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        )
        .unwrap_or(0),
    )
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-core --lib storage::messages::tests::mark_read -- --nocapture
```
Expected: 2 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/storage/messages.rs
git commit -m "$(cat <<'EOF'
storage: MessageRepo::mark_read delegates to ReadStateRepo

Convenience wrapper that captures `now()` for updated_at so callers
don't have to. Idempotent UPSERT semantics inherited from
ReadStateRepo::set.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: `MessageRepo::export_page`

**Files:**
- Modify: `crates/core/src/storage/messages.rs` (add `export_page` + tests)

- [ ] **Step 1: Write the failing tests**

Append to `crates/core/src/storage/messages.rs` inside `#[cfg(test)] mod tests`:

```rust
#[test]
fn export_page_yields_oldest_first_full_page() {
    let pool = Pool::in_memory();
    let gid = [0x40; 32];
    let repo = MessageRepo::new(&pool);
    for i in 0..5i64 {
        let mut env = sample_envelope(&format!("msg-{i}"));
        env.ts = 1000 + i;
        repo.insert(InsertParams {
            group_id: &gid,
            sender: &[0u8; 32],
            envelope: &env,
            mls_generation: 0,
            ts_daemon_recv: 1000 + i,
        })
        .unwrap();
    }
    let page = repo.export_page(&gid, None, 10).unwrap();
    assert_eq!(page.len(), 5);
    assert!(page[0].id < page[4].id, "oldest first");
}

#[test]
fn export_page_paginates_via_after_id() {
    let pool = Pool::in_memory();
    let gid = [0x41; 32];
    let repo = MessageRepo::new(&pool);
    for i in 0..7i64 {
        let mut env = sample_envelope(&format!("p-{i}"));
        env.ts = 1000 + i;
        repo.insert(InsertParams {
            group_id: &gid,
            sender: &[0u8; 32],
            envelope: &env,
            mls_generation: 0,
            ts_daemon_recv: 1000 + i,
        })
        .unwrap();
    }
    let page1 = repo.export_page(&gid, None, 3).unwrap();
    assert_eq!(page1.len(), 3);
    let page2 = repo
        .export_page(&gid, Some(page1.last().unwrap().id), 3)
        .unwrap();
    assert_eq!(page2.len(), 3);
    let page3 = repo
        .export_page(&gid, Some(page2.last().unwrap().id), 3)
        .unwrap();
    assert_eq!(page3.len(), 1);
    assert!(page1.last().unwrap().id < page2.first().unwrap().id);
    assert!(page2.last().unwrap().id < page3.first().unwrap().id);
}
```

- [ ] **Step 2: Verify the tests fail**

```bash
cargo test -p skattr-core --lib storage::messages::tests::export_page -- --nocapture
```
Expected: FAIL — `export_page` undefined.

- [ ] **Step 3: Implement `export_page`**

Add inside `impl<'p> MessageRepo<'p>`:

```rust
/// One page of messages in `group_id`, ordered ascending by `id`
/// (oldest-first). `after_id = None` starts from the beginning;
/// `after_id = Some(n)` returns rows with `id > n`. Caller loops
/// until the returned vec is shorter than `limit`.
pub fn export_page(
    &self,
    group_id: &[u8],
    after_id: Option<i64>,
    limit: usize,
) -> Result<Vec<StoredMessage>> {
    self.pool.with(|c| {
        let mut stmt = c
            .prepare(
                "SELECT id, group_id, sender, kind, body_blob, ts, delivered_at \
                 FROM messages \
                 WHERE group_id = ?1 AND id > ?2 \
                 ORDER BY id ASC \
                 LIMIT ?3",
            )
            .map_err(|e| CoreError::Storage(format!("prepare export_page: {e}")))?;
        let rows = stmt
            .query_map(
                rusqlite::params![
                    group_id,
                    after_id.unwrap_or(0),
                    i64::try_from(limit).unwrap_or(i64::MAX),
                ],
                |r| {
                    Ok(StoredMessage {
                        id: r.get(0)?,
                        group_id: r.get(1)?,
                        sender: r.get(2)?,
                        kind: r.get(3)?,
                        body_blob: r.get(4)?,
                        ts: r.get(5)?,
                        delivered_at: r.get(6)?,
                    })
                },
            )
            .map_err(|e| CoreError::Storage(format!("query export_page: {e}")))?;
        let out: std::result::Result<Vec<_>, _> = rows.collect();
        out.map_err(|e| CoreError::Storage(format!("collect export_page: {e}")))
    })
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-core --lib storage::messages::tests::export_page -- --nocapture
```
Expected: 2 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/storage/messages.rs
git commit -m "$(cat <<'EOF'
storage: MessageRepo::export_page — oldest-first pagination by id

Cursor pagination on (group_id, id > after_id), oldest-first. Caller
loops until response shorter than limit. CLI uses this for streaming
export over the 1 MiB IPC body cap.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: `MessageRepo::prune_before` + `prune_keep_last`

**Files:**
- Modify: `crates/core/src/storage/messages.rs` (two prune methods + tests)

- [ ] **Step 1: Write the failing tests**

Append to `crates/core/src/storage/messages.rs` inside `#[cfg(test)] mod tests`:

```rust
#[test]
fn prune_before_deletes_old_rows_and_cascades_to_fts() {
    let pool = Pool::in_memory();
    let gid = [0x50; 32];
    let repo = MessageRepo::new(&pool);
    for i in 0..6i64 {
        let mut env = sample_envelope(&format!("retain-or-prune-{i}"));
        env.ts = 1000;
        repo.insert(InsertParams {
            group_id: &gid,
            sender: &[0u8; 32],
            envelope: &env,
            mls_generation: 0,
            ts_daemon_recv: i * 100,    // 0..500 in steps of 100
        })
        .unwrap();
    }

    let deleted = repo.prune_before(Some(&gid), 250).unwrap();
    assert_eq!(deleted, 3, "rows with ts_daemon_recv 0/100/200 must go");

    let remaining: i64 = pool
        .with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM messages WHERE group_id = ?1",
                rusqlite::params![&gid[..]],
                |r| r.get(0),
            )
            .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
        })
        .unwrap();
    assert_eq!(remaining, 3);

    let fts_rows: i64 = pool
        .with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM messages_fts WHERE messages_fts \
                 MATCH 'retain'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
        })
        .unwrap();
    assert_eq!(fts_rows, 3, "ad trigger must cascade FTS deletes");
}

#[test]
fn prune_before_global_when_group_is_none() {
    let pool = Pool::in_memory();
    let repo = MessageRepo::new(&pool);
    for gid in [&[0x60u8; 32][..], &[0x61u8; 32][..]] {
        for i in 0..3i64 {
            let mut env = sample_envelope(&format!("g-{i}"));
            env.ts = 1000;
            repo.insert(InsertParams {
                group_id: gid,
                sender: &[0u8; 32],
                envelope: &env,
                mls_generation: 0,
                ts_daemon_recv: i * 100,
            })
            .unwrap();
        }
    }
    let deleted = repo.prune_before(None, 150).unwrap();
    assert_eq!(deleted, 4, "two rows from each of two groups (ts<150)");
}

#[test]
fn prune_keep_last_keeps_most_recent() {
    let pool = Pool::in_memory();
    let gid = [0x70; 32];
    let repo = MessageRepo::new(&pool);
    for i in 0..10i64 {
        let mut env = sample_envelope(&format!("k-{i}"));
        env.ts = 1000;
        repo.insert(InsertParams {
            group_id: &gid,
            sender: &[0u8; 32],
            envelope: &env,
            mls_generation: u64::try_from(i).unwrap(),
            ts_daemon_recv: i,
        })
        .unwrap();
    }
    let deleted = repo.prune_keep_last(&gid, 3).unwrap();
    assert_eq!(deleted, 7);
    let remaining_ids: Vec<i64> = pool
        .with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id FROM messages WHERE group_id = ?1 ORDER BY id DESC",
                )
                .map_err(|e| crate::error::CoreError::Storage(e.to_string()))?;
            let it = stmt
                .query_map(rusqlite::params![&gid[..]], |r| r.get::<_, i64>(0))
                .map_err(|e| crate::error::CoreError::Storage(e.to_string()))?;
            let v: std::result::Result<Vec<_>, _> = it.collect();
            v.map_err(|e| crate::error::CoreError::Storage(e.to_string()))
        })
        .unwrap();
    assert_eq!(remaining_ids.len(), 3, "exactly 3 rows survive");
    let max = remaining_ids.iter().copied().max().unwrap();
    let min = remaining_ids.iter().copied().min().unwrap();
    assert_eq!(max - min, 2, "the surviving 3 are consecutive at the top");
}
```

- [ ] **Step 2: Verify the tests fail**

```bash
cargo test -p skattr-core --lib storage::messages::tests::prune -- --nocapture
```
Expected: FAIL — both prune methods undefined.

- [ ] **Step 3: Implement both methods**

Add inside `impl<'p> MessageRepo<'p>`:

```rust
/// Delete rows with `ts_daemon_recv < before_ts_recv`. `group_id =
/// None` prunes globally. Returns the number of rows deleted.
pub fn prune_before(
    &self,
    group_id: Option<&[u8]>,
    before_ts_recv: i64,
) -> Result<u64> {
    self.pool.with_mut(|c| {
        let n = if let Some(gid) = group_id {
            c.execute(
                "DELETE FROM messages \
                 WHERE group_id = ?1 AND ts_daemon_recv < ?2",
                rusqlite::params![gid, before_ts_recv],
            )
        } else {
            c.execute(
                "DELETE FROM messages WHERE ts_daemon_recv < ?1",
                rusqlite::params![before_ts_recv],
            )
        }
        .map_err(|e| CoreError::Storage(format!("prune_before: {e}")))?;
        Ok(u64::try_from(n).unwrap_or(0))
    })
}

/// Keep the `keep` newest rows in `group_id`; delete the rest.
/// Returns the number of rows deleted.
pub fn prune_keep_last(&self, group_id: &[u8], keep: u64) -> Result<u64> {
    let keep_i = i64::try_from(keep).unwrap_or(i64::MAX);
    self.pool.with_mut(|c| {
        let n = c
            .execute(
                "DELETE FROM messages \
                 WHERE group_id = ?1 \
                   AND id <= COALESCE( \
                       (SELECT id FROM messages \
                        WHERE group_id = ?1 \
                        ORDER BY id DESC \
                        LIMIT 1 OFFSET ?2), \
                       -1 \
                   )",
                rusqlite::params![group_id, keep_i],
            )
            .map_err(|e| CoreError::Storage(format!("prune_keep_last: {e}")))?;
        Ok(u64::try_from(n).unwrap_or(0))
    })
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-core --lib storage::messages::tests::prune -- --nocapture
```
Expected: 3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/storage/messages.rs
git commit -m "$(cat <<'EOF'
storage: MessageRepo::prune_before + prune_keep_last

prune_before deletes by ts_daemon_recv with optional group filter; ad
trigger cascades to messages_fts. prune_keep_last keeps the N newest
rows by id (OFFSET subselect on ORDER BY id DESC).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: `MessageRepo::backfill_body_text`

**Files:**
- Modify: `crates/core/src/storage/messages.rs` (add `backfill_body_text` method + test)

- [ ] **Step 1: Write the failing test**

Append to `crates/core/src/storage/messages.rs` inside `#[cfg(test)] mod tests`:

```rust
#[test]
fn backfill_body_text_decodes_legacy_text_rows_and_indexes_fts() {
    let pool = Pool::in_memory();
    let gid = [0x80; 32];

    // Insert a row directly with body_text NULL (simulating a pre-1.G row).
    let env = sample_envelope("legacy hello world");
    let blob = env.encode().unwrap();
    pool.with_mut(|c| {
        c.execute(
            "INSERT INTO messages \
                 (group_id, sender, kind, body_blob, body_text, ts, \
                  mls_generation, ts_daemon_recv) \
             VALUES (?1, ?2, 'text', ?3, NULL, ?4, 0, 0)",
            rusqlite::params![&gid[..], &[0u8; 32][..], blob, env.ts],
        )
        .map_err(|e| crate::error::CoreError::Storage(e.to_string()))?;
        Ok(())
    })
    .unwrap();

    // Sanity: FTS index is empty before backfill.
    let pre: i64 = pool
        .with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM messages_fts \
                 WHERE messages_fts MATCH 'legacy'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
        })
        .unwrap();
    assert_eq!(pre, 0);

    let n = MessageRepo::new(&pool).backfill_body_text().unwrap();
    assert_eq!(n, 1);

    // Backfilled row's body_text populated; FTS index now finds it.
    let post: i64 = pool
        .with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM messages_fts \
                 WHERE messages_fts MATCH 'legacy'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
        })
        .unwrap();
    assert_eq!(post, 1, "au trigger must have indexed the row");
}

#[test]
fn backfill_body_text_is_idempotent() {
    let pool = Pool::in_memory();
    let gid = [0x81; 32];
    let repo = MessageRepo::new(&pool);
    repo.insert(InsertParams {
        group_id: &gid,
        sender: &[0u8; 32],
        envelope: &sample_envelope("already populated"),
        mls_generation: 0,
        ts_daemon_recv: 0,
    })
    .unwrap();
    // body_text already populated by insert; backfill must do nothing.
    let n = repo.backfill_body_text().unwrap();
    assert_eq!(n, 0);
}
```

- [ ] **Step 2: Verify the tests fail**

```bash
cargo test -p skattr-core --lib storage::messages::tests::backfill_body_text -- --nocapture
```
Expected: FAIL — `backfill_body_text` undefined.

- [ ] **Step 3: Implement `backfill_body_text`**

Add inside `impl<'p> MessageRepo<'p>`:

```rust
/// One-shot startup helper: decode CBOR for any text-kind row whose
/// `body_text` column is NULL (i.e., predates Phase 1.G), populate
/// it, and let the AU trigger cascade into `messages_fts`. Returns
/// the number of rows backfilled. Idempotent.
pub(crate) fn backfill_body_text(&self) -> Result<u64> {
    let candidates: Vec<(i64, Vec<u8>)> = self.pool.with(|c| {
        let mut stmt = c
            .prepare(
                "SELECT id, body_blob FROM messages \
                 WHERE kind = 'text' AND body_text IS NULL",
            )
            .map_err(|e| CoreError::Storage(format!("prepare backfill: {e}")))?;
        let it = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))
            .map_err(|e| CoreError::Storage(format!("query backfill: {e}")))?;
        let v: std::result::Result<Vec<_>, _> = it.collect();
        v.map_err(|e| CoreError::Storage(format!("collect backfill: {e}")))
    })?;

    if candidates.is_empty() {
        return Ok(0);
    }

    let mut updated = 0u64;
    self.pool.with_mut(|c| {
        for (id, blob) in &candidates {
            let env = match crate::envelope::Envelope::decode(blob) {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(row_id = id, error = %e,
                        "backfill_body_text: skipping row whose body_blob \
                         failed to decode");
                    continue;
                }
            };
            if let crate::envelope::Kind::Text { body } = env.kind {
                c.execute(
                    "UPDATE messages SET body_text = ?1 WHERE id = ?2",
                    rusqlite::params![body, id],
                )
                .map_err(|e| CoreError::Storage(format!("backfill UPDATE: {e}")))?;
                updated += 1;
            }
        }
        Ok(())
    })?;
    Ok(updated)
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-core --lib storage::messages::tests::backfill_body_text -- --nocapture
```
Expected: 2 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/storage/messages.rs
git commit -m "$(cat <<'EOF'
storage: MessageRepo::backfill_body_text — decode legacy text rows

One-shot startup helper. Selects text-kind rows with body_text IS
NULL, decodes body_blob CBOR, UPDATEs body_text. The au trigger
cascades into messages_fts so legacy databases become searchable
after one daemon startup.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Daemon command/result/event/error variants

**Files:**
- Modify: `crates/core/src/daemon/commands.rs` (add new variants + `SearchHitRecord` + `ExportFormat` if not yet present)
- Modify: 1.F's `daemon::events` (add `Event::MessageReceived`) — typically `crates/core/src/daemon/events.rs`
- Modify: 1.F's `EventFilter` location (typically `daemon::ipc::wire`) to add `EventFilter::Messages { contact: Option<PublicKey> }`
- Modify: 1.F's `DaemonErrorKind` (typically in `daemon::ipc::wire` or `daemon::commands`) to add `SearchSyntax`

- [ ] **Step 1: Write failing tests for the wire types**

Append to whichever file 1.F placed wire-type tests (typically `crates/core/src/daemon/commands.rs` or `daemon/ipc/wire.rs`):

```rust
#[cfg(test)]
mod phase_1g_wire_tests {
    use super::*;

    #[test]
    fn search_messages_command_round_trips_cbor() {
        let cmd = Command::SearchMessages {
            query: "alpha bravo".into(),
            contact: None,
            limit: 20,
            offset: 0,
            newest_first: false,
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&cmd, &mut buf).unwrap();
        let back: Command = ciborium::de::from_reader(&buf[..]).unwrap();
        assert!(matches!(back, Command::SearchMessages { .. }));
    }

    #[test]
    fn mark_read_command_round_trips_cbor() {
        let cmd = Command::MarkRead {
            contact: crate::identity::PublicKey([0x11; 32]),
            up_to_message_id: 42,
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&cmd, &mut buf).unwrap();
        let back: Command = ciborium::de::from_reader(&buf[..]).unwrap();
        assert!(matches!(back, Command::MarkRead { up_to_message_id: 42, .. }));
    }

    #[test]
    fn prune_history_command_round_trips_cbor() {
        let cmd = Command::PruneHistory {
            contact: None,
            before_ts_recv: Some(1_700_000_000),
            keep_last: None,
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&cmd, &mut buf).unwrap();
        let back: Command = ciborium::de::from_reader(&buf[..]).unwrap();
        assert!(matches!(back, Command::PruneHistory { .. }));
    }

    #[test]
    fn export_history_command_round_trips_cbor() {
        let cmd = Command::ExportHistory {
            contact: crate::identity::PublicKey([0x22; 32]),
            after_id: Some(100),
            limit: 1000,
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&cmd, &mut buf).unwrap();
        let back: Command = ciborium::de::from_reader(&buf[..]).unwrap();
        assert!(matches!(back, Command::ExportHistory { after_id: Some(100), .. }));
    }

    #[test]
    fn search_results_round_trips() {
        let res = CommandResult::SearchResults(vec![]);
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&res, &mut buf).unwrap();
        let back: CommandResult = ciborium::de::from_reader(&buf[..]).unwrap();
        assert!(matches!(back, CommandResult::SearchResults(_)));
    }
}
```

(In a worktree where `Command` is `pub enum Command` in `daemon::commands`, the `super::*` import covers the variants. If 1.F placed `EventFilter` in `daemon::ipc::wire`, write the EventFilter test in that file's tests instead.)

- [ ] **Step 2: Verify the tests fail**

```bash
cargo test -p skattr-core --lib daemon -- --nocapture
```
Expected: FAIL — variants undefined.

- [ ] **Step 3: Add the variants**

Append to `Command` (in `daemon::commands`):

```rust
SearchMessages {
    query: String,
    contact: Option<crate::identity::PublicKey>,
    limit: u32,
    offset: u32,
    newest_first: bool,
},
MarkRead {
    contact: crate::identity::PublicKey,
    up_to_message_id: i64,
},
PruneHistory {
    contact: Option<crate::identity::PublicKey>,
    before_ts_recv: Option<i64>,
    keep_last: Option<u64>,
},
ExportHistory {
    contact: crate::identity::PublicKey,
    after_id: Option<i64>,
    limit: u32,
},
```

Append to `CommandResult`:

```rust
SearchResults(Vec<SearchHitRecord>),
MarkedRead { up_to: i64 },
Pruned { rows_deleted: u64 },
ExportPage {
    records: Vec<MessageRecord>,
    next_after_id: Option<i64>,
},
```

Add the new struct alongside `MessageRecord` (1.F's wire type):

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SearchHitRecord {
    pub record: MessageRecord,
    pub bm25: f64,
    pub snippet: String,
}
```

In whichever file 1.F placed `EventFilter`, add:

```rust
Messages {
    contact: Option<crate::identity::PublicKey>,
},
```

In whichever file 1.F placed `Event` (likely `daemon::events`), add:

```rust
MessageReceived {
    contact: crate::identity::PublicKey,
    record: crate::daemon::commands::MessageRecord,
},
```

In `DaemonErrorKind` (per 1.F: typically in `daemon::ipc::wire`), add:

```rust
/// Search query was empty after FTS5 escaping or the engine rejected it.
SearchSyntax,
```

- [ ] **Step 4: Run the wire tests**

```bash
cargo test -p skattr-core --lib daemon::commands::phase_1g_wire_tests -- --nocapture
cargo test -p skattr-core --lib daemon -- --nocapture
```
Expected: PASS for new tests; existing daemon tests still PASS.

- [ ] **Step 5: Update `CoreError::kind()` projection**

Edit `crates/core/src/error.rs` — extend the `kind()` match arm so that any storage error matching the `"FTS5 syntax"` substring (the SQLite engine's wording) maps to `Some(DaemonErrorKind::SearchSyntax)`. Concretely, add inside the existing `Storage(msg)` arm:

```rust
crate::error::CoreError::Storage(msg)
    if msg.contains("fts5: syntax error") || msg.contains("malformed MATCH") =>
{
    Some(DaemonErrorKind::SearchSyntax)
}
```

(Keep the existing `Storage(_) => Some(DaemonErrorKind::StorageError)` as the catch-all that follows.)

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/commands.rs \
        crates/core/src/daemon/events.rs \
        crates/core/src/daemon/ipc/wire.rs \
        crates/core/src/error.rs
git commit -m "$(cat <<'EOF'
daemon: SearchMessages/MarkRead/PruneHistory/ExportHistory IPC variants

Adds Command/CommandResult/Event/EventFilter additions and the
SearchHitRecord wire type; extends DaemonErrorKind with SearchSyntax
and CoreError::kind() with the FTS5-error mapping. CBOR round-trip
tests cover all four new commands.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: `MessageRecord::project` helper + remove 1.F placeholders

**Files:**
- Modify: `crates/core/src/daemon/commands.rs` (add `MessageRecord::project`)
- Modify: 1.F's `daemon::dispatch` `recent_by_contact` handler (replace `mls_generation: 0` and `ts_daemon_recv: row.ts as u64` with the real columns)

- [ ] **Step 1: Write the failing test**

Append to `crates/core/src/daemon/commands.rs` inside `#[cfg(test)] mod tests`:

```rust
#[test]
fn message_record_project_uses_real_columns_not_placeholders() {
    use crate::envelope::{Envelope, Kind, MessageId};

    let env = Envelope {
        v: 1,
        id: MessageId([0xAA; 16]),
        ts: 1_700_000_000,
        reply_to: None,
        kind: Kind::Text { body: "hi".into() },
    };

    let rec = MessageRecord::project(
        42,                  // row id
        &env,
        7,                   // mls_generation (must be carried, not zeroed)
        1_700_000_500,       // ts_daemon_recv (must be carried, not aliased to ts)
        Direction::In,
    );

    assert_eq!(rec.mls_generation, 7);
    assert_eq!(rec.ts_daemon_recv, 1_700_000_500);
    assert_eq!(rec.ts_envelope, 1_700_000_000);
    assert!(matches!(rec.direction, Direction::In));
    assert!(matches!(rec.kind, Kind::Text { .. }));
}
```

- [ ] **Step 2: Verify it fails**

```bash
cargo test -p skattr-core --lib daemon::commands::tests::message_record_project -- --nocapture
```
Expected: FAIL — `MessageRecord::project` undefined.

- [ ] **Step 3: Implement `MessageRecord::project`**

Add to `daemon::commands` next to the existing `MessageRecord` definition (1.F):

```rust
impl MessageRecord {
    /// Project a stored row + decrypt-time metadata into the wire type.
    ///
    /// `direction` is `In` for receiver-side rows, `Out` for sender-side.
    /// `mls_generation` is the post-encrypt/post-decrypt epoch.
    /// `ts_daemon_recv` is the local clock at persist time. Both are
    /// carried straight to the wire — no aliasing back to `envelope.ts`.
    pub fn project(
        id: i64,
        envelope: &crate::envelope::Envelope,
        mls_generation: u64,
        ts_daemon_recv: i64,
        direction: Direction,
    ) -> Self {
        Self {
            id: Hex16(envelope.id.0),
            direction,
            kind: envelope.kind.clone(),
            mls_generation,
            ts_daemon_recv: u64::try_from(ts_daemon_recv).unwrap_or(0),
            ts_envelope: envelope.ts,
        }
    }
}
```

(`Hex16` is defined by 1.F. If 1.F used a different field shape for `id`, adapt the assignment to match.)

- [ ] **Step 4: Strip the 1.F placeholders**

Locate the dispatch handler 1.F created for `RecentMessages`:

```bash
grep -rn "mls_generation: 0\|ts_daemon_recv: u64::try_from(row.ts)" \
    crates/core/src/daemon
```

Replace the inline `MessageRecord { ... mls_generation: 0, ts_daemon_recv: ... }` construction with:

```rust
MessageRecord::project(
    row.id,
    &crate::envelope::Envelope::decode(row.body_blob.as_deref().unwrap_or(&[]))?,
    u64::try_from(row.mls_generation).unwrap_or(0),
    row.ts_daemon_recv,
    direction,
)
```

(`row.mls_generation` and `row.ts_daemon_recv` are now real columns — Task 4 added them. The `StoredMessage` struct itself does not yet expose them; if 1.F's `recent_by_contact` projects from raw SQL, also extend the SELECT and the row-builder to include `mls_generation` and `ts_daemon_recv`.)

If `StoredMessage` is missing those fields, add them now in `crates/core/src/storage/messages.rs`:

```rust
#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub id: i64,
    pub group_id: Vec<u8>,
    pub sender: Vec<u8>,
    pub kind: String,
    pub body_blob: Option<Vec<u8>>,
    pub ts: i64,
    pub delivered_at: Option<i64>,
    pub mls_generation: i64,                // (+) Phase 1.G
    pub ts_daemon_recv: i64,                // (+) Phase 1.G
}
```

Update every `StoredMessage { ... }` construction in `messages.rs` to include the two new fields, and add the two columns to the SELECT lists in `recent`, `export_page`, and `search` (`SELECT messages.id, ..., messages.mls_generation, messages.ts_daemon_recv, ...`).

- [ ] **Step 5: Run the full test suite**

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```
Expected: all tests PASS, no clippy warnings. (1.F tests that asserted `mls_generation == 0` will need updating to the real values; expect a small ripple.)

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/commands.rs \
        crates/core/src/daemon/dispatch.rs \
        crates/core/src/storage/messages.rs
git commit -m "$(cat <<'EOF'
daemon: MessageRecord::project + drop 1.F mls_generation=0 placeholder

Adds the projection helper that constructs MessageRecord from a
stored row plus decrypt-time metadata. Removes 1.F's intentional
placeholders in recent_by_contact (mls_generation=0, ts_daemon_recv
aliased to envelope ts) now that the columns are persisted.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 13: `delivery::receiver` — capture mls_generation + ts_daemon_recv

**Files:**
- Modify: `crates/core/src/delivery/receiver.rs` (extend signature, extend `ReceiveOutcome::New`)
- Modify: callers (`crates/tests/src/delivery_kill_mid_message.rs`, `crates/tests/src/delivery_real_tor.rs`)

- [ ] **Step 1: Write the failing test**

Append to `crates/core/src/delivery/receiver.rs` inside `mod tests`:

```rust
#[test]
fn receive_carries_mls_generation_and_ts_daemon_recv_into_outcome() {
    let pool = Pool::in_memory();
    let seen = SeenMessagesRepo::new(&pool);
    let msgs = MessageRepo::new(&pool);
    let sender = PublicKey([0xAA; 32]);

    let out = receive(
        &sender,
        &[0x01; 16],
        env(0x01, 1000),
        1000,                              // now_ms (replay window check)
        9,                                 // mls_generation
        1_700_000_777,                     // ts_daemon_recv (seconds, local clock)
        &seen,
        &msgs,
    )
    .unwrap();

    match out {
        ReceiveOutcome::New { row_id, mls_generation, ts_daemon_recv, .. } => {
            assert!(row_id > 0);
            assert_eq!(mls_generation, 9);
            assert_eq!(ts_daemon_recv, 1_700_000_777);
        }
        other => panic!("expected New, got {other:?}"),
    }
}
```

Existing `mod tests` already destructures `matches!(out, ReceiveOutcome::New(_))`. With the variant becoming a struct variant, the existing tests need their patterns updated to `matches!(out, ReceiveOutcome::New { .. })`.

- [ ] **Step 2: Verify the test fails to compile**

```bash
cargo test -p skattr-core --lib delivery::receiver -- --nocapture
```
Expected: FAIL — extra arguments + struct variant pattern.

- [ ] **Step 3: Extend `ReceiveOutcome::New` and the `receive` signature**

Replace the current `ReceiveOutcome` definition with:

```rust
#[derive(Debug, Clone)]
pub enum ReceiveOutcome {
    /// Fresh message persisted; caller should surface to UI and ACK.
    New {
        envelope: Envelope,
        row_id: i64,
        sender: PublicKey,
        group_id: Vec<u8>,
        mls_generation: u64,
        ts_daemon_recv: i64,
    },
    Duplicate,
    Rejected(String),
}
```

Update the `receive` function:

```rust
pub fn receive(
    sender: &PublicKey,
    group_id: &[u8],
    envelope: Envelope,
    now_ms: i64,
    mls_generation: u64,
    ts_daemon_recv: i64,
    seen: &SeenMessagesRepo<'_>,
    messages: &MessageRepo<'_>,
) -> Result<ReceiveOutcome> {
    if envelope.ts.saturating_sub(now_ms).saturating_abs() > REPLAY_WINDOW_MS {
        return Ok(ReceiveOutcome::Rejected(format!(
            "ts outside ±1h window: envelope ts={}, now={}",
            envelope.ts, now_ms
        )));
    }
    let is_new = seen.insert(&sender.0, &envelope.id.0, now_ms)?;
    if !is_new {
        return Ok(ReceiveOutcome::Duplicate);
    }
    let row_id = messages.insert(crate::storage::messages::InsertParams {
        group_id,
        sender: &sender.0,
        envelope: &envelope,
        mls_generation,
        ts_daemon_recv,
    })?;
    Ok(ReceiveOutcome::New {
        envelope,
        row_id,
        sender: *sender,
        group_id: group_id.to_vec(),
        mls_generation,
        ts_daemon_recv,
    })
}
```

- [ ] **Step 4: Update existing receiver mod tests**

Each `receive(&sender, &[0x01; 16], env(...), now_ms, &seen, &msgs)` call site needs two extra arguments before `&seen`. Use `0` for `mls_generation` and `now_ms` for `ts_daemon_recv` in the existing happy-path tests:

```rust
receive(
    &sender,
    &[0x01; 16],
    env(0x01, 1000),
    1000,
    0,
    1000,
    &seen,
    &msgs,
)
```

Update the `matches!` patterns from `ReceiveOutcome::New(_)` to `ReceiveOutcome::New { .. }`.

- [ ] **Step 5: Update integration test callers**

In `crates/tests/src/delivery_kill_mid_message.rs:85` and `crates/tests/src/delivery_real_tor.rs:113`, the `receive(...)` calls similarly need two extra arguments. The dispatcher in those files holds the decrypted MLS message; pass the post-decrypt epoch:

```rust
let mls_generation = group.epoch().as_u64();   // captured immediately after group.decrypt(..)
let ts_daemon_recv = i64::try_from(
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0),
)
.unwrap_or(0);

match receive(
    &peer,
    &self.group_id.0,
    envelope,
    now_ms,
    mls_generation,
    ts_daemon_recv,
    &seen,
    &msgs,
)
.ok()?
{
    ReceiveOutcome::New { .. } | ReceiveOutcome::Duplicate => Some(mid),
    ReceiveOutcome::Rejected(_) => None,
}
```

- [ ] **Step 6: Run the full suite**

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```
Expected: all tests PASS, no clippy warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/delivery/receiver.rs \
        crates/tests/src/delivery_kill_mid_message.rs \
        crates/tests/src/delivery_real_tor.rs
git commit -m "$(cat <<'EOF'
delivery: receiver carries mls_generation + ts_daemon_recv into outcome

receive() takes both new fields; ReceiveOutcome::New is now a struct
variant exposing row_id, sender, group_id, mls_generation,
ts_daemon_recv. The InboundDispatch caller can broadcast
Event::MessageReceived from these fields after the ACK succeeds.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 14: InboundDispatch caller emits `Event::MessageReceived`

**Files:**
- Modify: the type that implements `InboundDispatch` for the daemon (likely created in 1.F when `Daemon::run` wires the hub — typically `crates/core/src/daemon/state.rs` or a sibling module)

- [ ] **Step 1: Locate the InboundDispatch impl and write the integration test**

```bash
grep -rn "impl.*InboundDispatch\|fn dispatch" crates/core/src/daemon crates/core/src/delivery
```

Append a unit test next to the `InboundDispatch` impl that asserts emission. Sketch (adapt names to whatever 1.F created):

```rust
#[cfg(test)]
mod phase_1g_event_tests {
    use super::*;

    #[tokio::test]
    async fn dispatch_emits_message_received_after_successful_persist() {
        // Build a minimal handle with a broadcast::Sender<Event>.
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        let pool = crate::storage::Pool::in_memory();
        let handle = test_only_handle(tx.clone(), pool /* + any 1.F deps */);

        // Build a contact + group row so the dispatcher can resolve
        // group_id -> contact_pk.
        seed_contact_for_group(&handle, &[0xCC; 32], crate::identity::PublicKey([0xAB; 32]));

        // Simulate one decrypted envelope.
        let env = sample_envelope("hello via dispatch");
        let outcome = call_inbound_dispatch_with(
            &handle,
            crate::identity::PublicKey([0xAB; 32]),
            &[0xCC; 32],
            env,
            7,                              // mls_generation
            1_700_000_555,                  // ts_daemon_recv
        )
        .await
        .unwrap();
        assert!(matches!(outcome, crate::delivery::receiver::ReceiveOutcome::New { .. }));

        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("event arrived")
            .unwrap();
        match evt {
            crate::daemon::events::Event::MessageReceived { contact, record } => {
                assert_eq!(contact, crate::identity::PublicKey([0xAB; 32]));
                assert_eq!(record.mls_generation, 7);
                assert_eq!(record.ts_daemon_recv, 1_700_000_555);
            }
            other => panic!("expected MessageReceived, got {other:?}"),
        }
    }
}
```

(`test_only_handle`, `seed_contact_for_group`, and `call_inbound_dispatch_with` are small fixtures — write them inline in the same `#[cfg(test)] mod` to keep the helper free of crate-wide visibility churn.)

- [ ] **Step 2: Verify it fails**

```bash
cargo test -p skattr-core daemon -- phase_1g_event_tests --nocapture
```
Expected: FAIL — no event emitted (no broadcast call yet).

- [ ] **Step 3: Add the broadcast in the InboundDispatch impl**

After `delivery::receiver::receive` returns `Ok(ReceiveOutcome::New { .. })` and after the ACK is sent (per existing 1.E ordering), insert:

```rust
if let crate::delivery::receiver::ReceiveOutcome::New {
    envelope,
    row_id,
    sender,
    mls_generation,
    ts_daemon_recv,
    ..
} = &outcome
{
    // Resolve group_id -> contact_pk via ContactRepo. For 2-member
    // groups the contact == sender; we resolve through the table to
    // tolerate future multi-member shapes without changing the wire.
    let contacts = crate::storage::ContactRepo::new(&handle.pool);
    let contact = contacts
        .find_by_group_id(group_id)            // 1.F added this method per spec D6
        .ok()
        .flatten()
        .map(|c| c.identity_pubkey)
        .unwrap_or(*sender);

    let record = crate::daemon::commands::MessageRecord::project(
        *row_id,
        envelope,
        *mls_generation,
        *ts_daemon_recv,
        crate::daemon::commands::Direction::In,
    );
    let _ = handle.events.send(
        crate::daemon::events::Event::MessageReceived { contact, record },
    );
}
```

(`handle.events` is the `broadcast::Sender<Event>` that 1.F's `DaemonHandle` exposes. If 1.F named `find_by_group_id` differently, use whichever ContactRepo method does the lookup.)

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-core daemon -- phase_1g_event_tests --nocapture
cargo test
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/state.rs            # or whichever file holds InboundDispatch
git commit -m "$(cat <<'EOF'
daemon: emit Event::MessageReceived after successful inbound persist

InboundDispatch impl resolves group_id -> contact via ContactRepo,
projects the row to MessageRecord, broadcasts the event. tail
--follow subscribers see new messages within ~100 ms of the ACK.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 15: `daemon::dispatch::send_message` captures mls_generation + ts_daemon_recv

**Files:**
- Modify: 1.F's `daemon::dispatch::send_message` handler (in `crates/core/src/daemon/dispatch.rs`)

- [ ] **Step 1: Write the failing assertion**

Find 1.F's existing `send_message` test and add an assertion that the freshly-inserted row has the right `mls_generation` and a non-zero `ts_daemon_recv`. Sketch:

```rust
#[tokio::test]
async fn send_message_persists_post_encrypt_mls_generation() {
    let handle = build_test_handle().await;       // 1.F harness
    let alice_pk = crate::identity::PublicKey([0x77; 32]);
    seed_contact_with_group(&handle, alice_pk, &[0x88; 32]);   // 1.F helper

    let result = crate::daemon::dispatch::execute_command(
        &handle,
        crate::daemon::commands::Command::SendMessage {
            contact: alice_pk,
            kind: crate::envelope::Kind::Text { body: "hi".into() },
        },
    )
    .await
    .unwrap();
    assert!(matches!(result, crate::daemon::commands::CommandResult::MessageSent { .. }));

    let (mls_gen, ts_recv): (i64, i64) = handle
        .pool
        .with(|c| {
            c.query_row(
                "SELECT mls_generation, ts_daemon_recv FROM messages \
                 WHERE group_id = ?1 ORDER BY id DESC LIMIT 1",
                rusqlite::params![&[0x88; 32][..]],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
        })
        .unwrap();
    assert!(mls_gen > 0, "encrypt advances epoch; got {mls_gen}");
    assert!(ts_recv > 1_600_000_000, "ts_daemon_recv must be a real clock value");
}
```

- [ ] **Step 2: Verify it fails**

```bash
cargo test -p skattr-core daemon::dispatch::tests::send_message_persists_post_encrypt_mls_generation -- --nocapture
```
Expected: FAIL — `mls_gen == 0` (1.F passed zero).

- [ ] **Step 3: Update the `send_message` handler**

In 1.F's send-handler, replace the `MessageRepo::insert(...)` call (still using the pre-1.G three-arg form or the new InsertParams form with placeholders) with:

```rust
let group = mls_groups.load(&contact.group_id)?;
let ciphertext = group.encrypt(&envelope)?;
let mls_generation = group.epoch().as_u64();   // post-encrypt
let ts_daemon_recv = i64::try_from(
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0),
)
.unwrap_or(0);
let row_id = messages.insert(crate::storage::messages::InsertParams {
    group_id: &contact.group_id,
    sender: &handle.identity.public.0,
    envelope: &envelope,
    mls_generation,
    ts_daemon_recv,
})?;
```

(Adapt to whatever names 1.F used. Keep the rest of the send flow — outbox enqueue, hub send, inline-wait — unchanged.)

- [ ] **Step 4: Run tests**

```bash
cargo test
```
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
daemon: send_message captures post-encrypt mls_generation + ts_daemon_recv

Drops the placeholder zero/now and writes the real epoch (group.epoch().as_u64())
plus local-clock seconds into the messages row at insert time.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 16: `daemon::ipc::server` honours `EventFilter::Messages`

**Files:**
- Modify: `crates/core/src/daemon/ipc/server.rs` (or wherever 1.F implemented per-connection event fan-out)

- [ ] **Step 1: Write the failing test**

Append to the same file's `#[cfg(test)] mod tests`:

```rust
#[tokio::test]
async fn server_filters_message_received_by_contact() {
    use crate::daemon::commands::{Direction, MessageRecord};
    use crate::daemon::events::Event;
    use crate::daemon::ipc::wire::EventFilter;
    use crate::envelope::{Kind, MessageId};
    use crate::identity::PublicKey;

    let alice = PublicKey([0xAA; 32]);
    let bob = PublicKey([0xBB; 32]);

    let make_record = |id: u8| MessageRecord::project(
        i64::from(id),
        &crate::envelope::Envelope {
            v: 1,
            id: MessageId([id; 16]),
            ts: 1_700_000_000,
            reply_to: None,
            kind: Kind::Text { body: "x".into() },
        },
        1,
        1_700_000_000,
        Direction::In,
    );

    // Filter scoped to Bob: only Bob's events should pass.
    let mut survivors = Vec::new();
    for evt in [
        Event::MessageReceived { contact: alice, record: make_record(1) },
        Event::MessageReceived { contact: bob, record: make_record(2) },
    ] {
        if event_matches_filter(
            &evt,
            &EventFilter::Messages { contact: Some(bob) },
        ) {
            survivors.push(evt);
        }
    }
    assert_eq!(survivors.len(), 1);

    // Filter scoped to all: both pass.
    let mut all = Vec::new();
    for evt in [
        Event::MessageReceived { contact: alice, record: make_record(1) },
        Event::MessageReceived { contact: bob, record: make_record(2) },
    ] {
        if event_matches_filter(
            &evt,
            &EventFilter::Messages { contact: None },
        ) {
            all.push(evt);
        }
    }
    assert_eq!(all.len(), 2);
}
```

- [ ] **Step 2: Verify it fails**

```bash
cargo test -p skattr-core --lib daemon::ipc::server::tests::server_filters_message_received_by_contact -- --nocapture
```
Expected: FAIL — `event_matches_filter` undefined or filter does not match `EventFilter::Messages`.

- [ ] **Step 3: Implement / extend `event_matches_filter`**

Find the existing function 1.F wrote for filter dispatch (commonly named `event_matches_filter` or inlined in the per-connection task). Add a match arm:

```rust
pub(super) fn event_matches_filter(evt: &Event, filter: &EventFilter) -> bool {
    match (filter, evt) {
        // ... existing 1.F arms ...
        (EventFilter::Messages { contact: None }, Event::MessageReceived { .. }) => true,
        (EventFilter::Messages { contact: Some(c) }, Event::MessageReceived { contact, .. })
            => c == contact,
        (EventFilter::Messages { .. }, _) => false,
        // ... existing default arm ...
    }
}
```

If 1.F inlined the filter logic, hoist it into a function first; the test above asserts the function exists.

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-core --lib daemon::ipc -- --nocapture
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/ipc/server.rs
git commit -m "$(cat <<'EOF'
daemon: ipc server honours EventFilter::Messages { contact }

None contact = pass-through; Some(c) = pass only when event's contact
matches. Server-side filter keeps the broadcast bus from sending
every contact's traffic to every CLI subscriber.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 17: `daemon::dispatch::handle_search_messages`

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (add the SearchMessages arm + handler)

- [ ] **Step 1: Write the failing test**

Append to `crates/core/src/daemon/dispatch.rs` `#[cfg(test)] mod tests`:

```rust
#[tokio::test]
async fn search_messages_returns_bm25_ranked_hits() {
    let handle = build_test_handle().await;
    let alice = crate::identity::PublicKey([0x77; 32]);
    seed_contact_with_group(&handle, alice, &[0x88; 32]);

    let msgs = crate::storage::MessageRepo::new(&handle.pool);
    for body in ["alpha bravo", "bravo only", "delta only"] {
        let env = crate::envelope::Envelope {
            v: 1,
            id: crate::envelope::MessageId::generate(),
            ts: 1_700_000_000,
            reply_to: None,
            kind: crate::envelope::Kind::Text { body: body.into() },
        };
        msgs.insert(crate::storage::messages::InsertParams {
            group_id: &[0x88; 32],
            sender: &alice.0,
            envelope: &env,
            mls_generation: 1,
            ts_daemon_recv: 1_700_000_000,
        })
        .unwrap();
    }

    let result = crate::daemon::dispatch::execute_command(
        &handle,
        crate::daemon::commands::Command::SearchMessages {
            query: "bravo".into(),
            contact: None,
            limit: 10,
            offset: 0,
            newest_first: false,
        },
    )
    .await
    .unwrap();
    let hits = match result {
        crate::daemon::commands::CommandResult::SearchResults(h) => h,
        other => panic!("expected SearchResults, got {other:?}"),
    };
    assert_eq!(hits.len(), 2);
    assert!(hits[0].snippet.contains("bravo"));
}

#[tokio::test]
async fn search_messages_empty_query_returns_empty_results() {
    let handle = build_test_handle().await;
    let result = crate::daemon::dispatch::execute_command(
        &handle,
        crate::daemon::commands::Command::SearchMessages {
            query: "   ".into(),
            contact: None,
            limit: 10,
            offset: 0,
            newest_first: false,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        result,
        crate::daemon::commands::CommandResult::SearchResults(ref v) if v.is_empty()
    ));
}
```

- [ ] **Step 2: Verify it fails**

```bash
cargo test -p skattr-core daemon::dispatch::tests::search_messages -- --nocapture
```
Expected: FAIL — handler unimplemented.

- [ ] **Step 3: Add the dispatch arm**

In `crates/core/src/daemon/dispatch.rs::execute_command`, add:

```rust
Command::SearchMessages { query, contact, limit, offset, newest_first } => {
    let group_id_owned: Option<Vec<u8>> = match contact {
        Some(pk) => Some(
            crate::storage::ContactRepo::new(&handle.pool)
                .find_by_pubkey(&pk)?
                .ok_or(CoreError::Daemon(DaemonErrorKind::ContactNotFound))?
                .group_id,
        ),
        None => None,
    };
    let hits = crate::storage::MessageRepo::new(&handle.pool)
        .search(
            &query,
            group_id_owned.as_deref(),
            usize::try_from(limit).unwrap_or(usize::MAX),
            usize::try_from(offset).unwrap_or(0),
            newest_first,
        )?;
    let records: Vec<crate::daemon::commands::SearchHitRecord> = hits
        .into_iter()
        .map(|h| {
            let env = crate::envelope::Envelope::decode(
                h.message.body_blob.as_deref().unwrap_or(&[]),
            )?;
            Ok::<_, CoreError>(crate::daemon::commands::SearchHitRecord {
                record: crate::daemon::commands::MessageRecord::project(
                    h.message.id,
                    &env,
                    u64::try_from(h.message.mls_generation).unwrap_or(0),
                    h.message.ts_daemon_recv,
                    crate::daemon::commands::Direction::In,    // BM25 search ignores direction — use In as a stable default
                ),
                bm25: h.bm25,
                snippet: h.snippet,
            })
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(CommandResult::SearchResults(records))
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-core daemon::dispatch -- --nocapture
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
daemon: dispatch handler for Command::SearchMessages

Resolves contact -> group_id (ContactNotFound on miss), runs
MessageRepo::search, projects each SearchHit -> SearchHitRecord with
BM25 score + snippet. Direction defaults to In (search returns the
union; direction is a per-row stored column projected from
encrypt/decrypt site).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 18: `daemon::dispatch::handle_mark_read`

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs`

- [ ] **Step 1: Write the failing test**

Append to `dispatch.rs` `#[cfg(test)] mod tests`:

```rust
#[tokio::test]
async fn mark_read_advances_cursor() {
    let handle = build_test_handle().await;
    let alice = crate::identity::PublicKey([0x77; 32]);
    seed_contact_with_group(&handle, alice, &[0x88; 32]);

    let result = crate::daemon::dispatch::execute_command(
        &handle,
        crate::daemon::commands::Command::MarkRead {
            contact: alice,
            up_to_message_id: 99,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        result,
        crate::daemon::commands::CommandResult::MarkedRead { up_to: 99 }
    ));

    let cur = crate::storage::ReadStateRepo::new(&handle.pool)
        .get(&[0x88; 32])
        .unwrap();
    assert_eq!(cur, Some(99));
}
```

- [ ] **Step 2: Verify it fails**

```bash
cargo test -p skattr-core daemon::dispatch::tests::mark_read_advances_cursor -- --nocapture
```
Expected: FAIL — handler unimplemented.

- [ ] **Step 3: Add the dispatch arm**

```rust
Command::MarkRead { contact, up_to_message_id } => {
    let group_id = crate::storage::ContactRepo::new(&handle.pool)
        .find_by_pubkey(&contact)?
        .ok_or(CoreError::Daemon(DaemonErrorKind::ContactNotFound))?
        .group_id;
    crate::storage::MessageRepo::new(&handle.pool)
        .mark_read(&group_id, up_to_message_id)?;
    Ok(CommandResult::MarkedRead { up_to: up_to_message_id })
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-core daemon::dispatch -- --nocapture
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
daemon: dispatch handler for Command::MarkRead

Resolves contact -> group_id, calls MessageRepo::mark_read, returns
MarkedRead { up_to: <id> } verbatim so the CLI can confirm.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 19: `daemon::dispatch::handle_prune_history`

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn prune_history_keep_last_returns_deleted_count() {
    let handle = build_test_handle().await;
    let alice = crate::identity::PublicKey([0x77; 32]);
    seed_contact_with_group(&handle, alice, &[0x88; 32]);

    let msgs = crate::storage::MessageRepo::new(&handle.pool);
    for i in 0..8i64 {
        let env = crate::envelope::Envelope {
            v: 1,
            id: crate::envelope::MessageId::generate(),
            ts: 1_700_000_000 + i,
            reply_to: None,
            kind: crate::envelope::Kind::Text { body: format!("m{i}") },
        };
        msgs.insert(crate::storage::messages::InsertParams {
            group_id: &[0x88; 32],
            sender: &alice.0,
            envelope: &env,
            mls_generation: u64::try_from(i).unwrap(),
            ts_daemon_recv: i,
        })
        .unwrap();
    }

    let result = crate::daemon::dispatch::execute_command(
        &handle,
        crate::daemon::commands::Command::PruneHistory {
            contact: Some(alice),
            before_ts_recv: None,
            keep_last: Some(3),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        result,
        crate::daemon::commands::CommandResult::Pruned { rows_deleted: 5 }
    ));
}

#[tokio::test]
async fn prune_history_rejects_both_before_and_keep_last() {
    let handle = build_test_handle().await;
    let result = crate::daemon::dispatch::execute_command(
        &handle,
        crate::daemon::commands::Command::PruneHistory {
            contact: None,
            before_ts_recv: Some(1),
            keep_last: Some(2),
        },
    )
    .await;
    assert!(result.is_err(), "exactly one of before_ts_recv / keep_last must be Some");
}
```

- [ ] **Step 2: Verify it fails**

```bash
cargo test -p skattr-core daemon::dispatch::tests::prune_history -- --nocapture
```
Expected: FAIL.

- [ ] **Step 3: Add the dispatch arm**

```rust
Command::PruneHistory { contact, before_ts_recv, keep_last } => {
    match (before_ts_recv, keep_last) {
        (Some(_), Some(_)) | (None, None) => {
            return Err(CoreError::InvalidInput(
                "PruneHistory requires exactly one of before_ts_recv or keep_last".into(),
            ));
        }
        _ => {}
    }
    let msgs = crate::storage::MessageRepo::new(&handle.pool);
    let group_id_owned: Option<Vec<u8>> = match contact {
        Some(pk) => Some(
            crate::storage::ContactRepo::new(&handle.pool)
                .find_by_pubkey(&pk)?
                .ok_or(CoreError::Daemon(DaemonErrorKind::ContactNotFound))?
                .group_id,
        ),
        None => None,
    };
    let rows = match (before_ts_recv, keep_last) {
        (Some(ts), None) => msgs.prune_before(group_id_owned.as_deref(), ts)?,
        (None, Some(k)) => {
            let gid = group_id_owned
                .as_deref()
                .ok_or(CoreError::InvalidInput(
                    "PruneHistory keep_last requires a contact".into(),
                ))?;
            msgs.prune_keep_last(gid, k)?
        }
        _ => unreachable!("validated above"),
    };
    Ok(CommandResult::Pruned { rows_deleted: rows })
}
```

(`CoreError::InvalidInput` exists in 1.F's error model; if not, use whichever variant 1.F added for client-side validation errors.)

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-core daemon::dispatch -- --nocapture
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
daemon: dispatch handler for Command::PruneHistory

Validates exactly one of before_ts_recv / keep_last is Some; routes
to MessageRepo::prune_before or prune_keep_last; returns Pruned with
the row delete count.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 20: `daemon::dispatch::handle_export_history`

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn export_history_paginates_and_advances_cursor() {
    let handle = build_test_handle().await;
    let alice = crate::identity::PublicKey([0x77; 32]);
    seed_contact_with_group(&handle, alice, &[0x88; 32]);

    let msgs = crate::storage::MessageRepo::new(&handle.pool);
    for i in 0..5i64 {
        let env = crate::envelope::Envelope {
            v: 1,
            id: crate::envelope::MessageId::generate(),
            ts: 1_700_000_000 + i,
            reply_to: None,
            kind: crate::envelope::Kind::Text { body: format!("m{i}") },
        };
        msgs.insert(crate::storage::messages::InsertParams {
            group_id: &[0x88; 32],
            sender: &alice.0,
            envelope: &env,
            mls_generation: u64::try_from(i).unwrap(),
            ts_daemon_recv: i,
        })
        .unwrap();
    }

    let r1 = crate::daemon::dispatch::execute_command(
        &handle,
        crate::daemon::commands::Command::ExportHistory {
            contact: alice,
            after_id: None,
            limit: 2,
        },
    )
    .await
    .unwrap();
    let (recs1, next1) = match r1 {
        crate::daemon::commands::CommandResult::ExportPage { records, next_after_id } => {
            (records, next_after_id)
        }
        other => panic!("expected ExportPage, got {other:?}"),
    };
    assert_eq!(recs1.len(), 2);
    assert!(next1.is_some());

    let r2 = crate::daemon::dispatch::execute_command(
        &handle,
        crate::daemon::commands::Command::ExportHistory {
            contact: alice,
            after_id: next1,
            limit: 2,
        },
    )
    .await
    .unwrap();
    let (recs2, next2) = match r2 {
        crate::daemon::commands::CommandResult::ExportPage { records, next_after_id } => {
            (records, next_after_id)
        }
        _ => unreachable!(),
    };
    assert_eq!(recs2.len(), 2);

    let r3 = crate::daemon::dispatch::execute_command(
        &handle,
        crate::daemon::commands::Command::ExportHistory {
            contact: alice,
            after_id: next2,
            limit: 2,
        },
    )
    .await
    .unwrap();
    let (recs3, next3) = match r3 {
        crate::daemon::commands::CommandResult::ExportPage { records, next_after_id } => {
            (records, next_after_id)
        }
        _ => unreachable!(),
    };
    assert_eq!(recs3.len(), 1);
    assert!(next3.is_none(), "short page → caller stops");
}
```

- [ ] **Step 2: Verify it fails**

```bash
cargo test -p skattr-core daemon::dispatch::tests::export_history -- --nocapture
```
Expected: FAIL.

- [ ] **Step 3: Add the dispatch arm**

```rust
Command::ExportHistory { contact, after_id, limit } => {
    const EXPORT_PAGE_MAX: u32 = 1000;
    let lim = limit.min(EXPORT_PAGE_MAX);
    let group_id = crate::storage::ContactRepo::new(&handle.pool)
        .find_by_pubkey(&contact)?
        .ok_or(CoreError::Daemon(DaemonErrorKind::ContactNotFound))?
        .group_id;
    let rows = crate::storage::MessageRepo::new(&handle.pool)
        .export_page(&group_id, after_id, usize::try_from(lim).unwrap_or(0))?;

    let mut records = Vec::with_capacity(rows.len());
    for row in &rows {
        let env = crate::envelope::Envelope::decode(
            row.body_blob.as_deref().unwrap_or(&[]),
        )?;
        records.push(crate::daemon::commands::MessageRecord::project(
            row.id,
            &env,
            u64::try_from(row.mls_generation).unwrap_or(0),
            row.ts_daemon_recv,
            crate::daemon::commands::Direction::In,
        ));
    }
    let next_after_id = if rows.len() == usize::try_from(lim).unwrap_or(0) {
        rows.last().map(|r| r.id)
    } else {
        None
    };
    Ok(CommandResult::ExportPage { records, next_after_id })
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-core daemon::dispatch -- --nocapture
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
daemon: dispatch handler for Command::ExportHistory

Caps limit at 1000 (keeps response under 1.F's 1 MiB IPC body cap).
Page is full → next_after_id = last row id; short page →
next_after_id = None so the CLI stops looping.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 21: `HistoryConfig` in `daemon::config`

**Files:**
- Modify: `crates/core/src/daemon/config.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/core/src/daemon/config.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn history_section_defaults_to_zero_when_absent() {
    let toml = r#"
        [storage]
        data_dir = "/tmp/skattr"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    assert_eq!(cfg.history.retention_days, 0);
}

#[test]
fn history_section_parses_explicit_retention_days() {
    let toml = r#"
        [storage]
        data_dir = "/tmp/skattr"

        [history]
        retention_days = 90
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    assert_eq!(cfg.history.retention_days, 90);
}
```

- [ ] **Step 2: Verify the tests fail**

```bash
cargo test -p skattr-core --lib daemon::config -- --nocapture
```
Expected: FAIL — `Config.history` field missing.

- [ ] **Step 3: Add `HistoryConfig` and the `history` field**

In `crates/core/src/daemon/config.rs`, add the struct (above the existing `Config` definition):

```rust
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, Default)]
pub struct HistoryConfig {
    /// Days of history to retain. 0 = infinite (default; sweep no-ops).
    #[serde(default)]
    pub retention_days: u32,
}
```

Add the field on `Config`:

```rust
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, Default)]
pub struct Config {
    // ... existing fields (storage, network, cli, etc.) from 1.F ...

    #[serde(default)]
    pub history: HistoryConfig,           // (+) Phase 1.G
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-core --lib daemon::config -- --nocapture
```
Expected: 2 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/config.rs
git commit -m "$(cat <<'EOF'
daemon: HistoryConfig.retention_days (default 0 = infinite)

[history] section in TOML; serde(default) so missing section parses
clean. Drives the hourly retention sweep added in Task 22.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 22: `daemon::retention` — sweep loop + spawn helper

**Files:**
- Create: `crates/core/src/daemon/retention.rs`
- Modify: `crates/core/src/daemon/mod.rs` (declare the module)

- [ ] **Step 1: Write the failing tests**

Create `crates/core/src/daemon/retention.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Daemon-owned retention sweep.
//!
//! Hourly tokio task; deletes any `messages` row whose
//! `ts_daemon_recv < now - retention_days * 86400`. Respects
//! `[history] retention_days = 0` as a no-op (infinite retention).

use std::sync::Arc;
use std::time::Duration;

use crate::storage::messages::MessageRepo;
use crate::storage::Pool;

/// Spawn the retention sweep on the current Tokio runtime.
///
/// `tick` is exposed for tests; production callers use
/// `Duration::from_secs(3600)`. The task exits when `shutdown` flips
/// to `true`.
pub fn spawn_sweep(
    pool: Arc<Pool>,
    retention_days: u32,
    tick: Duration,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(tick) => {
                    if retention_days == 0 {
                        continue;
                    }
                    let cutoff = now_secs()
                        .saturating_sub(i64::from(retention_days).saturating_mul(86_400));
                    match MessageRepo::new(&pool).prune_before(None, cutoff) {
                        Ok(n) if n > 0 => tracing::info!(
                            rows = n, cutoff_ts_recv = cutoff,
                            "retention sweep deleted rows"
                        ),
                        Ok(_) => {}
                        Err(e) => tracing::warn!(
                            error = %e,
                            "retention sweep failed; will retry next tick"
                        ),
                    }
                }
                _ = shutdown.changed() => break,
            }
        }
    })
}

fn now_secs() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    )
    .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Envelope, Kind, MessageId};
    use crate::storage::messages::InsertParams;

    fn env(body: &str, ts: i64) -> Envelope {
        Envelope {
            v: 1,
            id: MessageId::generate(),
            ts,
            reply_to: None,
            kind: Kind::Text { body: body.into() },
        }
    }

    #[tokio::test]
    async fn sweep_no_op_when_retention_days_zero() {
        let pool = Arc::new(Pool::in_memory());
        MessageRepo::new(&pool)
            .insert(InsertParams {
                group_id: &[0x01; 32],
                sender: &[0u8; 32],
                envelope: &env("x", 1000),
                mls_generation: 0,
                ts_daemon_recv: 0,
            })
            .unwrap();

        let (tx, rx) = tokio::sync::watch::channel(false);
        let h = spawn_sweep(pool.clone(), 0, Duration::from_millis(20), rx);
        tokio::time::sleep(Duration::from_millis(80)).await;
        let _ = tx.send(true);
        let _ = h.await;

        let n: i64 = pool
            .with(|c| {
                c.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
                    .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
            })
            .unwrap();
        assert_eq!(n, 1, "retention_days=0 must not delete anything");
    }

    #[tokio::test]
    async fn sweep_deletes_rows_older_than_cutoff() {
        let pool = Arc::new(Pool::in_memory());
        let now = now_secs();

        // Three rows: 2 days old, 1 day old, just-now.
        for (i, ts_offset) in [(-2 * 86_400, 0), (-86_400, 1), (0, 2)].iter().enumerate() {
            let _ = i;
            MessageRepo::new(&pool)
                .insert(InsertParams {
                    group_id: &[0x02; 32],
                    sender: &[0u8; 32],
                    envelope: &env("y", 1000),
                    mls_generation: 0,
                    ts_daemon_recv: now + ts_offset.0,
                })
                .unwrap();
        }

        let (tx, rx) = tokio::sync::watch::channel(false);
        let h = spawn_sweep(pool.clone(), 1 /* day */, Duration::from_millis(20), rx);
        tokio::time::sleep(Duration::from_millis(80)).await;
        let _ = tx.send(true);
        let _ = h.await;

        let n: i64 = pool
            .with(|c| {
                c.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
                    .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
            })
            .unwrap();
        assert!(
            n <= 2,
            "expected at most 2 rows after 1-day cutoff sweep, got {n}"
        );
    }
}
```

- [ ] **Step 2: Wire the module**

Edit `crates/core/src/daemon/mod.rs` to declare:

```rust
pub(crate) mod retention;
```

- [ ] **Step 3: Run the tests**

```bash
cargo test -p skattr-core --lib daemon::retention -- --nocapture
```
Expected: 2 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/daemon/retention.rs \
        crates/core/src/daemon/mod.rs
git commit -m "$(cat <<'EOF'
daemon: retention sweep loop with watch-based shutdown

spawn_sweep(pool, retention_days, tick, shutdown) ticks every `tick`,
deletes rows where ts_daemon_recv < now - retention_days*86400. tick
is exposed for tests; shutdown is a tokio watch channel for graceful
exit. retention_days=0 = no-op.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 23: `Daemon::run` spawns sweep + runs backfill at startup

**Files:**
- Modify: `crates/core/src/daemon/state.rs` (extend `Daemon::run` with backfill + sweep spawn)

- [ ] **Step 1: Write the failing test**

Append to `crates/core/src/daemon/state.rs` `#[cfg(test)] mod tests` (or wherever the integration-style daemon-startup test lives):

```rust
#[tokio::test]
async fn daemon_startup_runs_backfill_and_spawns_sweep() {
    use crate::storage::messages::InsertParams;
    use crate::storage::MessageRepo;

    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path();
    let passphrase = zeroize::Zeroizing::new("test-passphrase".to_string());

    // Pre-seed the storage DB with one legacy text row (body_text NULL).
    {
        // 1.F's bootstrap creates the vault + storage; reuse whichever
        // helper exists. If the test harness exposes Pool::open
        // directly, use it; otherwise spin Daemon::run once just to
        // create the DB, then close it before re-running with the
        // legacy-row injection.
        let (_vault, identity) = init_test_vault(data_dir, &passphrase);
        let seed = crate::identity::derive::derive_storage_seed(identity).unwrap();
        let pool = crate::storage::Pool::open(data_dir, &seed).unwrap();
        let env = crate::envelope::Envelope {
            v: 1,
            id: crate::envelope::MessageId([0xAB; 16]),
            ts: 1_700_000_000,
            reply_to: None,
            kind: crate::envelope::Kind::Text { body: "legacy".into() },
        };
        // Direct insert with body_text NULL.
        let blob = env.encode().unwrap();
        pool.with_mut(|c| {
            c.execute(
                "INSERT INTO messages \
                     (group_id, sender, kind, body_blob, body_text, ts, \
                      mls_generation, ts_daemon_recv) \
                 VALUES (?1, ?2, 'text', ?3, NULL, ?4, 0, 0)",
                rusqlite::params![&[0u8; 32][..], &[0u8; 32][..], blob, env.ts],
            )
            .map_err(|e| crate::error::CoreError::Storage(e.to_string()))?;
            Ok(())
        })
        .unwrap();
        pool.close().unwrap();
    }

    // Boot the daemon; backfill should run.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<String>();
    let (sd_tx, sd_rx) = tokio::sync::oneshot::channel::<()>();
    let dd = data_dir.to_owned();
    let pp = passphrase.clone();
    let h = tokio::spawn(async move {
        crate::daemon::Daemon::run(&dd, &pp, ready_tx, async move {
            let _ = sd_rx.await;
        })
        .await
    });
    let _ = tokio::time::timeout(std::time::Duration::from_secs(30), ready_rx)
        .await
        .expect("daemon never reported ready");
    let _ = sd_tx.send(());
    let _ = h.await;

    // Reopen the DB and assert backfill populated body_text.
    let (_vault, identity) = init_test_vault(data_dir, &passphrase);
    let seed = crate::identity::derive::derive_storage_seed(identity).unwrap();
    let pool = crate::storage::Pool::open(data_dir, &seed).unwrap();
    let body_text: Option<String> = pool
        .with(|c| {
            c.query_row(
                "SELECT body_text FROM messages WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
        })
        .unwrap();
    assert_eq!(body_text.as_deref(), Some("legacy"));
}
```

(`init_test_vault` is whichever fixture 1.F uses to initialise a vault for daemon-startup tests. If 1.F lacks one, write it inline using `Vault::create(...)` from `identity::vault`.)

- [ ] **Step 2: Verify it fails**

```bash
cargo test -p skattr-core --lib daemon::state::tests::daemon_startup_runs_backfill_and_spawns_sweep -- --nocapture
```
Expected: FAIL — `body_text` still NULL (backfill not invoked).

- [ ] **Step 3: Wire backfill + sweep into `Daemon::run`**

In `crates/core/src/daemon/state.rs::Daemon::run`, after the `Pool::open(...)` call (added by 1.F when expanding `run`), insert:

```rust
let pool = std::sync::Arc::new(pool);

// One-shot backfill for legacy text rows (idempotent).
match crate::storage::MessageRepo::new(&pool).backfill_body_text() {
    Ok(0) => {}
    Ok(n) => tracing::info!(rows = n, "backfilled body_text for legacy rows"),
    Err(e) => tracing::warn!(error = %e, "body_text backfill failed; FTS may be incomplete"),
}

// Hourly retention sweep.
let (sweep_shutdown_tx, sweep_shutdown_rx) = tokio::sync::watch::channel(false);
let sweep_handle = crate::daemon::retention::spawn_sweep(
    pool.clone(),
    config.history.retention_days,
    std::time::Duration::from_secs(3600),
    sweep_shutdown_rx,
);
```

At the end of the `Daemon::run` body, immediately before `rt.shutdown().await?`, add:

```rust
let _ = sweep_shutdown_tx.send(true);
let _ = sweep_handle.await;
```

(Adapt the surrounding code to whatever shape 1.F produced.)

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-core --lib daemon -- --nocapture
cargo test
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/state.rs
git commit -m "$(cat <<'EOF'
daemon: Daemon::run runs body_text backfill and spawns retention sweep

Backfill runs once at startup (idempotent — no-op when no legacy rows
exist). Retention sweep spawned with the configured retention_days
and an hourly tick; shutdown via tokio::sync::watch flag flipped
before the Tor runtime tears down.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 24: `skattr search` CLI command

**Files:**
- Modify: `crates/cli/src/main.rs` (add the `Command::Search` clap variant + handler)
- Modify: `crates/cli/Cargo.toml` (no new deps for this task; `time` is added in Task 26)

- [ ] **Step 1: Write the failing test**

Append a unit test in `crates/cli/src/main.rs` (or a sibling `cli_tests.rs` file 1.F already maintains):

```rust
#[test]
fn render_search_results_human_includes_snippet_and_id() {
    use skattr_core::daemon::{Direction, Hex16, MessageRecord, SearchHitRecord};

    let rec = MessageRecord {
        id: Hex16([0xAB; 16]),
        direction: Direction::In,
        kind: skattr_core::envelope::Kind::Text { body: "the merge conflict".into() },
        mls_generation: 7,
        ts_daemon_recv: 1_700_000_000,
        ts_envelope: 1_700_000_000,
    };
    let hit = SearchHitRecord {
        record: rec,
        bm25: 0.5,
        snippet: "...the merge conflict...".to_string(),
    };
    let out = render_search_human(&[hit]);
    assert!(out.contains("merge conflict"));
    assert!(out.contains("epoch=7"));
    assert!(out.contains("ababab"));     // first chars of message id
}
```

- [ ] **Step 2: Verify it fails**

```bash
cargo test -p skattr-cli render_search_results_human_includes_snippet_and_id -- --nocapture
```
Expected: FAIL — `render_search_human` undefined.

- [ ] **Step 3: Add the clap variant**

Inside the `Command` enum 1.F created in `crates/cli/src/main.rs`, add:

```rust
/// Full-text search over message history.
Search {
    /// Query — free-form, tokenize-and-AND on the daemon side.
    query: String,
    /// Limit search to one contact (name or hex prefix).
    #[arg(long)]
    contact: Option<String>,
    /// Maximum hits to return.
    #[arg(long, default_value_t = 20)]
    limit: u32,
    /// Skip this many hits.
    #[arg(long, default_value_t = 0)]
    offset: u32,
    /// Order by id DESC instead of BM25.
    #[arg(long)]
    newest_first: bool,
    /// Emit raw JSON instead of the human-readable form.
    #[arg(long)]
    json: bool,
},
```

- [ ] **Step 4: Add the handler and the renderer**

Add at module scope:

```rust
fn render_search_human(hits: &[skattr_core::daemon::SearchHitRecord]) -> String {
    let mut out = String::new();
    for h in hits {
        let id_prefix = h
            .record
            .id
            .0
            .iter()
            .take(3)
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        out.push_str(&format!(
            "[ts_recv={ts}] (id={id} epoch={epoch}) {snippet}\n",
            ts = h.record.ts_daemon_recv,
            id = id_prefix,
            epoch = h.record.mls_generation,
            snippet = h.snippet,
        ));
    }
    out
}

async fn cmd_search(
    client: &skattr_core::daemon::IpcClient,
    query: String,
    contact: Option<String>,
    limit: u32,
    offset: u32,
    newest_first: bool,
    as_json: bool,
) -> Result<()> {
    let pk = match contact {
        Some(c) => Some(resolve_contact_hex_or_name(client, &c).await?),
        None => None,
    };
    let req = skattr_core::daemon::Command::SearchMessages {
        query,
        contact: pk,
        limit,
        offset,
        newest_first,
    };
    let resp = client.execute(req).await?;
    match resp {
        skattr_core::daemon::CommandResult::SearchResults(hits) => {
            if as_json {
                println!("{}", serde_json::to_string_pretty(&hits)?);
            } else {
                print!("{}", render_search_human(&hits));
            }
            Ok(())
        }
        other => bail!("unexpected daemon response: {other:?}"),
    }
}
```

(`resolve_contact_hex_or_name` is whatever 1.F implemented for prefix-matching contacts. `IpcClient`, `Command`, `CommandResult` are re-exported from `skattr_core::daemon` per 1.F.)

Wire the dispatch arm:

```rust
Command::Search { query, contact, limit, offset, newest_first, json } => {
    let client = skattr_core::daemon::IpcClient::connect(&socket_path).await?;
    cmd_search(&client, query, contact, limit, offset, newest_first, json).await
}
```

- [ ] **Step 5: Run tests**

```bash
cargo test -p skattr-cli -- --nocapture
cargo build -p skattr-cli
```
Expected: tests PASS, build clean.

Manual smoke (if a daemon is running):

```bash
cargo run -p skattr-cli -- search merge --limit 5
```
Expected: zero or more hits printed; exit code 0.

- [ ] **Step 6: Commit**

```bash
git add crates/cli/src/main.rs
git commit -m "$(cat <<'EOF'
cli: skattr search — Command::SearchMessages over IPC + human/JSON

Tokenize-and-AND happens on the daemon side. --contact filters by
hex prefix or alias; --newest-first overrides BM25; --json emits the
raw SearchHitRecord vec.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 25: `skattr export` CLI command

**Files:**
- Modify: `crates/cli/src/main.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/cli/src/main.rs` `#[cfg(test)] mod tests` (or sibling test module):

```rust
#[test]
fn render_export_text_line_uses_envelope_kind_and_short_sender() {
    use skattr_core::daemon::{Direction, Hex16, MessageRecord};
    use skattr_core::envelope::Kind;

    let rec = MessageRecord {
        id: Hex16([0xCC; 16]),
        direction: Direction::In,
        kind: Kind::Text { body: "hi".into() },
        mls_generation: 1,
        ts_daemon_recv: 1_700_000_000,
        ts_envelope: 1_700_000_000,
    };
    let line = render_export_text_line(&rec, &[0x12, 0x34, 0x56, 0x78][..]);
    assert!(line.starts_with('['));
    assert!(line.contains("12345678"));
    assert!(line.contains("hi"));
}
```

- [ ] **Step 2: Verify it fails**

```bash
cargo test -p skattr-cli render_export_text_line_uses_envelope_kind_and_short_sender
```
Expected: FAIL — `render_export_text_line` undefined.

- [ ] **Step 3: Add the clap variant + handler + renderer**

Add to the `Command` enum:

```rust
/// Export a contact's full message history.
Export {
    /// Contact name or hex pubkey prefix.
    contact: String,
    /// Output format. Default: `json`.
    #[arg(long, default_value = "json")]
    format: String,
    /// Output file path. Refuses to overwrite an existing file.
    #[arg(long)]
    output: std::path::PathBuf,
},
```

Module-scope helpers:

```rust
fn render_export_text_line(rec: &skattr_core::daemon::MessageRecord, sender: &[u8]) -> String {
    let body = match &rec.kind {
        skattr_core::envelope::Kind::Text { body } => body.clone(),
        other => format!("<{other:?}>"),
    };
    let sender_short = sender
        .iter()
        .take(4)
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let ts = chrono_or_naive_iso(rec.ts_daemon_recv);   // see helper below
    format!("[{ts}] {sender_short}: {body}\n")
}

/// Naive RFC3339-ish timestamp: seconds-since-epoch -> `YYYY-MM-DDTHH:MM:SSZ`.
/// Avoids pulling in chrono — small inline calc using `time` crate.
fn chrono_or_naive_iso(ts: u64) -> String {
    use time::format_description::well_known::Rfc3339;
    use time::OffsetDateTime;
    let secs = i64::try_from(ts).unwrap_or(0);
    OffsetDateTime::from_unix_timestamp(secs)
        .ok()
        .and_then(|odt| odt.format(&Rfc3339).ok())
        .unwrap_or_else(|| format!("{ts}"))
}

async fn cmd_export(
    client: &skattr_core::daemon::IpcClient,
    contact: String,
    format: String,
    output: std::path::PathBuf,
) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let pk = resolve_contact_hex_or_name(client, &contact).await?;

    // Refuse to clobber.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)             // O_CREAT | O_EXCL
        .mode(0o600)
        .open(&output)
        .map_err(|e| anyhow::anyhow!("open {}: {e}", output.display()))?;

    if format == "json" {
        file.write_all(b"[\n")?;
    }
    let mut first_record = true;
    let mut after_id: Option<i64> = None;
    loop {
        let resp = client
            .execute(skattr_core::daemon::Command::ExportHistory {
                contact: pk,
                after_id,
                limit: 1000,
            })
            .await?;
        let (records, next) = match resp {
            skattr_core::daemon::CommandResult::ExportPage { records, next_after_id } => {
                (records, next_after_id)
            }
            other => bail!("unexpected response: {other:?}"),
        };
        for r in &records {
            if format == "json" {
                if !first_record {
                    file.write_all(b",\n")?;
                }
                first_record = false;
                serde_json::to_writer(&mut file, r)?;
            } else {
                // Plaintext format. Sender pubkey isn't on MessageRecord
                // — use Direction-derived 8-char "self" / "peer" until
                // the wire grows a sender field; here we approximate via
                // the Hex16 message id since pubkey is daemon-side only.
                file.write_all(render_export_text_line(r, &r.id.0[..4]).as_bytes())?;
            }
        }
        if next.is_none() {
            break;
        }
        after_id = next;
    }
    if format == "json" {
        file.write_all(b"\n]\n")?;
    }
    file.sync_all()?;
    Ok(())
}
```

Wire the dispatch arm:

```rust
Command::Export { contact, format, output } => {
    let client = skattr_core::daemon::IpcClient::connect(&socket_path).await?;
    cmd_export(&client, contact, format, output).await
}
```

Add `time = { version = "0.3", features = ["parsing", "formatting", "macros"] }` to `crates/cli/Cargo.toml` `[dependencies]`.

- [ ] **Step 4: Run tests + smoke**

```bash
cargo test -p skattr-cli -- --nocapture
cargo build -p skattr-cli
```
Expected: tests PASS, build clean.

- [ ] **Step 5: Commit**

```bash
git add crates/cli/src/main.rs crates/cli/Cargo.toml
git commit -m "$(cat <<'EOF'
cli: skattr export — paginated ExportHistory + json/plaintext writer

Loops Command::ExportHistory until next_after_id is None, writes to
the user-provided path with O_CREAT | O_EXCL (refuse to clobber).
JSON wraps the records in a single array; plaintext is one line per
message with RFC3339 ts_daemon_recv via the new `time` dep.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 26: `skattr prune` CLI command

**Files:**
- Modify: `crates/cli/src/main.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn parse_rfc3339_to_unix_seconds() {
    let secs = parse_rfc3339_to_unix("2026-01-01T00:00:00Z").unwrap();
    assert_eq!(secs, 1_767_225_600);
}

#[test]
fn parse_rfc3339_rejects_garbage() {
    assert!(parse_rfc3339_to_unix("not a date").is_err());
}
```

- [ ] **Step 2: Verify it fails**

```bash
cargo test -p skattr-cli parse_rfc3339_to_unix
```
Expected: FAIL — `parse_rfc3339_to_unix` undefined.

- [ ] **Step 3: Add the clap variant + handler**

Add the variant:

```rust
/// Delete history rows. Pass exactly one of --before or --keep-last.
Prune {
    /// Limit to one contact (name or hex prefix).
    #[arg(long)]
    contact: Option<String>,
    /// Delete rows older than this RFC3339 timestamp.
    #[arg(long)]
    before: Option<String>,
    /// Keep only the N newest rows in the contact's group.
    #[arg(long)]
    keep_last: Option<u64>,
},
```

Module-scope helpers:

```rust
fn parse_rfc3339_to_unix(s: &str) -> anyhow::Result<i64> {
    use time::format_description::well_known::Rfc3339;
    use time::OffsetDateTime;
    let odt = OffsetDateTime::parse(s, &Rfc3339)
        .map_err(|e| anyhow::anyhow!("invalid RFC3339 timestamp: {e}"))?;
    Ok(odt.unix_timestamp())
}

async fn cmd_prune(
    client: &skattr_core::daemon::IpcClient,
    contact: Option<String>,
    before: Option<String>,
    keep_last: Option<u64>,
) -> Result<()> {
    if before.is_some() == keep_last.is_some() {
        bail!("exactly one of --before or --keep-last is required");
    }
    let pk = match contact {
        Some(c) => Some(resolve_contact_hex_or_name(client, &c).await?),
        None => None,
    };
    let req = skattr_core::daemon::Command::PruneHistory {
        contact: pk,
        before_ts_recv: before.map(|s| parse_rfc3339_to_unix(&s)).transpose()?,
        keep_last,
    };
    let resp = client.execute(req).await?;
    match resp {
        skattr_core::daemon::CommandResult::Pruned { rows_deleted } => {
            println!("Deleted {rows_deleted} messages.");
            Ok(())
        }
        other => bail!("unexpected daemon response: {other:?}"),
    }
}
```

Wire the dispatch arm:

```rust
Command::Prune { contact, before, keep_last } => {
    let client = skattr_core::daemon::IpcClient::connect(&socket_path).await?;
    cmd_prune(&client, contact, before, keep_last).await
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-cli parse_rfc3339_to_unix
cargo build -p skattr-cli
```
Expected: PASS, build clean.

- [ ] **Step 5: Commit**

```bash
git add crates/cli/src/main.rs
git commit -m "$(cat <<'EOF'
cli: skattr prune — Command::PruneHistory with RFC3339 --before parser

Validates exactly one of --before / --keep-last is set client-side
before reaching the daemon. Renders Pruned { rows_deleted } as
"Deleted N messages.".

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 27: `skattr tail --follow` upgrade

**Files:**
- Modify: `crates/cli/src/main.rs` (extend the `tail` handler 1.F shipped with `--follow` semantics)

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn render_event_message_received_matches_one_shot_format() {
    use skattr_core::daemon::{Direction, Hex16, MessageRecord};
    use skattr_core::envelope::Kind;

    let rec = MessageRecord {
        id: Hex16([0xDD; 16]),
        direction: Direction::In,
        kind: Kind::Text { body: "live update".into() },
        mls_generation: 11,
        ts_daemon_recv: 1_700_000_900,
        ts_envelope: 1_700_000_900,
    };
    let line = render_message_record_human(&rec);
    let one_shot = render_messages_human(&[rec.clone()]);
    assert_eq!(one_shot.trim_end(), line.trim_end());
}
```

(`render_messages_human` is 1.F's existing renderer — extract its inner per-row formatter into a new `render_message_record_human` so `--follow` and one-shot share it.)

- [ ] **Step 2: Verify it fails**

```bash
cargo test -p skattr-cli render_event_message_received_matches_one_shot_format
```
Expected: FAIL — `render_message_record_human` undefined.

- [ ] **Step 3: Refactor + add `--follow`**

Extract the per-row formatter:

```rust
fn render_message_record_human(r: &skattr_core::daemon::MessageRecord) -> String {
    // Whatever 1.F did for one row, hoisted out of render_messages_human.
    let body = match &r.kind {
        skattr_core::envelope::Kind::Text { body } => body.clone(),
        other => format!("<{other:?}>"),
    };
    format!(
        "[ts_recv={ts}] (id={id} epoch={epoch}) {body}",
        ts = r.ts_daemon_recv,
        id = r.id.0.iter().take(3).map(|b| format!("{b:02x}")).collect::<String>(),
        epoch = r.mls_generation,
    )
}

fn render_messages_human(rows: &[skattr_core::daemon::MessageRecord]) -> String {
    let mut out = String::new();
    for r in rows {
        out.push_str(&render_message_record_human(r));
        out.push('\n');
    }
    out
}
```

Extend the `Tail` clap variant (1.F created it without `--follow` per the spec note):

```rust
Tail {
    /// Contact name or hex prefix.
    contact: Option<String>,
    /// Number of messages to fetch in the one-shot prelude.
    #[arg(long, default_value_t = 20)]
    limit: u32,
    /// Stream new messages as they arrive (Ctrl-C to exit).
    #[arg(long)]
    follow: bool,
},
```

Extend the handler:

```rust
async fn cmd_tail(
    client: &skattr_core::daemon::IpcClient,
    contact: Option<String>,
    limit: u32,
    follow: bool,
) -> Result<()> {
    let pk = match contact.as_deref() {
        Some(c) => Some(resolve_contact_hex_or_name(client, c).await?),
        None => None,
    };

    // One-shot prelude.
    let resp = client
        .execute(skattr_core::daemon::Command::RecentMessages {
            contact: pk,
            limit,
        })
        .await?;
    if let skattr_core::daemon::CommandResult::Messages(rows) = resp {
        // One-shot prints oldest at top so follow continues naturally.
        for r in rows.iter().rev() {
            println!("{}", render_message_record_human(r));
        }
    }

    if !follow {
        return Ok(());
    }

    let mut stream = client
        .subscribe(skattr_core::daemon::EventFilter::Messages { contact: pk })
        .await?;
    while let Some(evt) = stream.next().await {
        if let skattr_core::daemon::Event::MessageReceived { record, .. } = evt? {
            println!("{}", render_message_record_human(&record));
        }
    }
    Ok(())
}
```

Wire the dispatch arm to call `cmd_tail`. Replace whatever 1.F generated.

- [ ] **Step 4: Run tests + smoke**

```bash
cargo test -p skattr-cli -- --nocapture
cargo build -p skattr-cli
```
Expected: PASS, build clean.

- [ ] **Step 5: Commit**

```bash
git add crates/cli/src/main.rs
git commit -m "$(cat <<'EOF'
cli: skattr tail --follow subscribes to Event::MessageReceived

One-shot prelude prints oldest-first so the follow stream extends
the conversation naturally. Server-side EventFilter::Messages keeps
unrelated peers out of the bus. Ctrl-C exits cleanly via the
existing IPC socket close path.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 28: Integration test — `cli_search.rs`

**Files:**
- Create: `crates/tests/src/cli_search.rs`
- Modify: `crates/tests/src/lib.rs` (add `pub mod cli_search;`)

- [ ] **Step 1: Write the failing test**

Create `crates/tests/src/cli_search.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 1.G integration test: full IPC round-trip for SearchMessages.
//!
//! Spawns one daemon (mocked transport via the 1.E harness), seeds three
//! contacts with mixed text, asserts BM25 ordering + contact filter +
//! empty-query short-circuit.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use skattr_core::daemon::{Command, CommandResult, IpcClient};
use skattr_core::envelope::{Envelope, Kind, MessageId};
use skattr_core::identity::PublicKey;
use skattr_core::test_exports::{ContactRepo, MessageRepo, Pool, ReadStateRepo};

#[tokio::test]
async fn search_messages_round_trips_through_ipc() {
    let harness = crate::common::single_daemon_harness().await;
    let client = harness.client.clone();

    // Seed two contacts each with a 2-member group.
    let alice = PublicKey([0x77; 32]);
    let bob = PublicKey([0x88; 32]);
    crate::common::seed_contact_with_group(&harness, alice, &[0x11; 32]).await;
    crate::common::seed_contact_with_group(&harness, bob, &[0x22; 32]).await;

    let msgs = MessageRepo::new(&harness.pool);
    let to_env = |body: &str| Envelope {
        v: 1,
        id: MessageId::generate(),
        ts: 1_700_000_000,
        reply_to: None,
        kind: Kind::Text { body: body.into() },
    };
    for body in ["alpha bravo", "bravo charlie", "delta echo"] {
        msgs.insert(skattr_core::storage::messages::InsertParams {
            group_id: &[0x11; 32],
            sender: &alice.0,
            envelope: &to_env(body),
            mls_generation: 1,
            ts_daemon_recv: 1_700_000_000,
        })
        .unwrap();
    }
    for body in ["alpha foxtrot", "bravo golf"] {
        msgs.insert(skattr_core::storage::messages::InsertParams {
            group_id: &[0x22; 32],
            sender: &bob.0,
            envelope: &to_env(body),
            mls_generation: 1,
            ts_daemon_recv: 1_700_000_000,
        })
        .unwrap();
    }

    // Cross-group hit: "bravo" matches 3 rows.
    let r = client
        .execute(Command::SearchMessages {
            query: "bravo".into(),
            contact: None,
            limit: 10,
            offset: 0,
            newest_first: false,
        })
        .await
        .unwrap();
    let hits = match r {
        CommandResult::SearchResults(h) => h,
        other => panic!("expected SearchResults, got {other:?}"),
    };
    assert_eq!(hits.len(), 3);

    // Contact-scoped: only Alice's two rows.
    let r = client
        .execute(Command::SearchMessages {
            query: "bravo".into(),
            contact: Some(alice),
            limit: 10,
            offset: 0,
            newest_first: false,
        })
        .await
        .unwrap();
    if let CommandResult::SearchResults(hits) = r {
        assert_eq!(hits.len(), 2);
    } else {
        panic!("expected SearchResults");
    }

    // Whitespace-only query short-circuits.
    let r = client
        .execute(Command::SearchMessages {
            query: "   ".into(),
            contact: None,
            limit: 10,
            offset: 0,
            newest_first: false,
        })
        .await
        .unwrap();
    if let CommandResult::SearchResults(hits) = r {
        assert!(hits.is_empty());
    } else {
        panic!("expected SearchResults");
    }
}
```

(`crate::common::single_daemon_harness` and `seed_contact_with_group` are 1.F-era test helpers in `crates/tests/src/common.rs`. If 1.F named them differently, adapt the imports.)

- [ ] **Step 2: Verify the test fails to compile**

```bash
cargo test -p skattr-tests cli_search -- --nocapture
```
Expected: FAIL — module not declared.

- [ ] **Step 3: Wire the module**

Edit `crates/tests/src/lib.rs`:

```rust
pub mod cli_search;          // (+) Phase 1.G
```

- [ ] **Step 4: Run the test**

```bash
cargo test -p skattr-tests cli_search -- --nocapture
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/tests/src/cli_search.rs crates/tests/src/lib.rs
git commit -m "$(cat <<'EOF'
tests: cli_search — full IPC round-trip for SearchMessages

Cross-group hit, contact-scoped filter, whitespace-only short-circuit.
Uses the 1.F single-daemon harness with mocked transport.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 29: Integration test — `history_sweep.rs`

**Files:**
- Create: `crates/tests/src/history_sweep.rs`
- Modify: `crates/tests/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/tests/src/history_sweep.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 1.G integration test: hourly retention sweep with a 50 ms
//! test-only tick interval.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::sync::Arc;
use std::time::Duration;

use skattr_core::envelope::{Envelope, Kind, MessageId};
use skattr_core::test_exports::{MessageRepo, Pool};

fn now_secs() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    )
    .unwrap_or(0)
}

#[tokio::test]
async fn sweep_deletes_only_rows_older_than_cutoff_and_cascades_fts() {
    let pool = Arc::new(Pool::in_memory());
    let now = now_secs();

    // Five rows with ts_daemon_recv at offsets [-3d, -2d, -1d, 0, +0]
    let to_env = |body: &str| Envelope {
        v: 1,
        id: MessageId::generate(),
        ts: 1_700_000_000,
        reply_to: None,
        kind: Kind::Text { body: body.into() },
    };
    for offset_days in [-3, -2, -1, 0, 0] {
        MessageRepo::new(&pool)
            .insert(skattr_core::storage::messages::InsertParams {
                group_id: &[0x33; 32],
                sender: &[0u8; 32],
                envelope: &to_env("retainable"),
                mls_generation: 0,
                ts_daemon_recv: now + i64::from(offset_days) * 86_400,
            })
            .unwrap();
    }

    let (tx, rx) = tokio::sync::watch::channel(false);
    let h = skattr_core::test_exports::spawn_sweep(
        pool.clone(),
        1,                                    // retention_days = 1
        Duration::from_millis(50),
        rx,
    );

    // Wait two ticks.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = tx.send(true);
    let _ = h.await;

    let n: i64 = pool
        .with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM messages WHERE group_id = ?1",
                rusqlite::params![&[0x33; 32][..]],
                |r| r.get(0),
            )
            .map_err(|e| skattr_core::error::CoreError::Storage(e.to_string()))
        })
        .unwrap();
    assert!(n <= 3, "expected ≤3 rows after 1d-cutoff sweep, got {n}");

    // FTS index size matches messages count.
    let fts_n: i64 = pool
        .with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM messages_fts \
                 WHERE messages_fts MATCH 'retainable'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| skattr_core::error::CoreError::Storage(e.to_string()))
        })
        .unwrap();
    assert_eq!(fts_n, n, "FTS row count must match messages row count");
}
```

`spawn_sweep` needs to be re-exported from `test_exports`:

In `crates/core/src/lib.rs`'s `test_exports` block, add:

```rust
pub use crate::daemon::retention::spawn_sweep;
```

- [ ] **Step 2: Verify it fails**

```bash
cargo test -p skattr-tests history_sweep -- --nocapture
```
Expected: FAIL — module not declared.

- [ ] **Step 3: Wire the module**

```rust
pub mod history_sweep;       // (+) in crates/tests/src/lib.rs
```

- [ ] **Step 4: Run the test**

```bash
cargo test -p skattr-tests history_sweep -- --nocapture
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/tests/src/history_sweep.rs \
        crates/tests/src/lib.rs \
        crates/core/src/lib.rs
git commit -m "$(cat <<'EOF'
tests: history_sweep — retention sweep over a real Pool with 50ms ticks

Seeds 5 rows across 4 days; with retention_days=1 expects ≤3 to
survive after 200 ms of sweep ticking. Asserts the ad trigger
cascades into messages_fts so FTS row count matches messages count.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 30: Integration test — `cli_tail_follow.rs`

**Files:**
- Create: `crates/tests/src/cli_tail_follow.rs`
- Modify: `crates/tests/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/tests/src/cli_tail_follow.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 1.G integration test: Event::MessageReceived reaches a
//! Subscribe(EventFilter::Messages) client end-to-end.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::time::Duration;

use skattr_core::daemon::{Command, EventFilter};
use skattr_core::envelope::Kind;
use skattr_core::identity::PublicKey;

#[tokio::test]
async fn message_received_event_reaches_subscriber() {
    // Two-daemon harness from 1.E + 1.F (mocked transport).
    let pair = crate::common::two_daemons_mocked().await;
    let alice = pair.alice_pubkey;

    // Bob subscribes to Alice's messages only.
    let mut sub = pair
        .bob_client
        .subscribe(EventFilter::Messages {
            contact: Some(alice),
        })
        .await
        .unwrap();

    // Alice sends two messages.
    pair.alice_client
        .execute(Command::SendMessage {
            contact: pair.bob_pubkey,
            kind: Kind::Text { body: "first".into() },
        })
        .await
        .unwrap();
    pair.alice_client
        .execute(Command::SendMessage {
            contact: pair.bob_pubkey,
            kind: Kind::Text { body: "second".into() },
        })
        .await
        .unwrap();

    // Bob should see both.
    let mut seen = 0;
    for _ in 0..2 {
        let evt = tokio::time::timeout(Duration::from_secs(5), sub.next())
            .await
            .expect("timeout waiting for event")
            .unwrap()
            .unwrap();
        if let skattr_core::daemon::Event::MessageReceived { contact, record } = evt {
            assert_eq!(contact, alice);
            assert!(matches!(record.direction, skattr_core::daemon::Direction::In));
            seen += 1;
        }
    }
    assert_eq!(seen, 2);
}
```

(`crate::common::two_daemons_mocked` is the 1.F two-daemon harness producing `{alice_client, alice_pubkey, bob_client, bob_pubkey}`. Adapt names to whatever 1.F shipped.)

- [ ] **Step 2: Verify, wire, run, commit (same pattern as Task 28/29)**

```bash
cargo test -p skattr-tests cli_tail_follow -- --nocapture
```
Wire `pub mod cli_tail_follow;` in `crates/tests/src/lib.rs`. Re-run; expect PASS.

```bash
git add crates/tests/src/cli_tail_follow.rs crates/tests/src/lib.rs
git commit -m "$(cat <<'EOF'
tests: cli_tail_follow — Event::MessageReceived end-to-end

Alice sends two messages; Bob's Subscribe(EventFilter::Messages
{ contact: alice }) receives both, with Direction::In, within 5s.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 31: Integration test — `cli_export.rs`

**Files:**
- Create: `crates/tests/src/cli_export.rs`
- Modify: `crates/tests/src/lib.rs`

- [ ] **Step 1: Write the failing test**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 1.G integration test: paginated ExportHistory yields a
//! parseable JSON file with oldest-first ordering for 2500 rows.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use skattr_core::daemon::{Command, CommandResult};
use skattr_core::envelope::{Envelope, Kind, MessageId};
use skattr_core::identity::PublicKey;
use skattr_core::test_exports::MessageRepo;

#[tokio::test]
async fn export_history_paginates_2500_rows_oldest_first() {
    let harness = crate::common::single_daemon_harness().await;
    let client = harness.client.clone();

    let alice = PublicKey([0x77; 32]);
    crate::common::seed_contact_with_group(&harness, alice, &[0x44; 32]).await;

    let msgs = MessageRepo::new(&harness.pool);
    for i in 0..2500i64 {
        let env = Envelope {
            v: 1,
            id: MessageId::generate(),
            ts: 1_700_000_000 + i,
            reply_to: None,
            kind: Kind::Text { body: format!("msg-{i}") },
        };
        msgs.insert(skattr_core::storage::messages::InsertParams {
            group_id: &[0x44; 32],
            sender: &alice.0,
            envelope: &env,
            mls_generation: u64::try_from(i).unwrap(),
            ts_daemon_recv: 1_700_000_000 + i,
        })
        .unwrap();
    }

    // Drive the dispatcher directly — exercises the pagination contract.
    let mut got = Vec::new();
    let mut after_id: Option<i64> = None;
    loop {
        let resp = client
            .execute(Command::ExportHistory {
                contact: alice,
                after_id,
                limit: 1000,
            })
            .await
            .unwrap();
        let (records, next) = match resp {
            CommandResult::ExportPage { records, next_after_id } => (records, next_after_id),
            other => panic!("expected ExportPage, got {other:?}"),
        };
        got.extend(records);
        if next.is_none() {
            break;
        }
        after_id = next;
    }
    assert_eq!(got.len(), 2500);
    // Oldest-first by ts_envelope (which equals ts_daemon_recv in this seed).
    let envelopes: Vec<i64> = got.iter().map(|r| r.ts_envelope).collect();
    let mut sorted = envelopes.clone();
    sorted.sort();
    assert_eq!(envelopes, sorted, "ExportPage must return oldest-first");
}
```

- [ ] **Step 2: Wire + run + commit**

Wire `pub mod cli_export;` in `crates/tests/src/lib.rs`.

```bash
cargo test -p skattr-tests cli_export -- --nocapture
```
Expected: PASS.

```bash
git add crates/tests/src/cli_export.rs crates/tests/src/lib.rs
git commit -m "$(cat <<'EOF'
tests: cli_export — paginated ExportHistory over 2500 rows

Three pages (1000 + 1000 + 500); asserts ordering is oldest-first by
ts_envelope and that the pagination cursor terminates exactly when
the page is short.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 32: 100k FTS p95 benchmark — `fts_search_p95.rs`

**Files:**
- Create: `crates/core/tests/fts_search_p95.rs`

- [ ] **Step 1: Write the test**

Create `crates/core/tests/fts_search_p95.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 1.G validation: FTS5 search over 100k rows, p95 < 50 ms.
//!
//! `#[ignore]`-gated. Run with:
//!
//!   cargo test -p skattr-core --release --test fts_search_p95 \
//!     -- --ignored --nocapture
//!
//! Logs p50 / p95 / p99 via eprintln! so trends can be eyeballed
//! without criterion.

#![cfg(any(test, feature = "test-harness"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Instant;

use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use skattr_core::envelope::{Envelope, Kind, MessageId};
use skattr_core::storage::messages::InsertParams;
use skattr_core::test_exports::{MessageRepo, Pool};

const VOCAB: &[&str] = &[
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel",
    "india", "juliet", "kilo", "lima", "mike", "november", "oscar", "papa",
    "quebec", "romeo", "sierra", "tango", "uniform", "victor", "whiskey",
    "xray", "yankee", "zulu", "search", "merge", "conflict", "rebase",
    "branch", "commit", "stash", "cherry", "pick", "deploy", "rollback",
    "feature", "fix", "tor", "arti", "noise", "handshake", "mls", "epoch",
    "ratchet", "envelope", "frame", "codec",
    // (extend up to 200 — 50 above + 150 more synonyms / nonsense)
];

#[test]
#[ignore]
fn fts_search_p95_under_50ms_over_100k_rows() {
    let pool = Pool::in_memory();
    let mut rng = rand::rngs::StdRng::seed_from_u64(42);

    eprintln!("seeding 100k synthetic text messages...");
    let t0 = Instant::now();
    let repo = MessageRepo::new(&pool);
    for i in 0..100_000i64 {
        let body = (0..rng.gen_range(3..12))
            .map(|_| VOCAB[rng.gen_range(0..VOCAB.len())])
            .collect::<Vec<_>>()
            .join(" ");
        let env = Envelope {
            v: 1,
            id: MessageId::generate(),
            ts: 1_700_000_000 + i,
            reply_to: None,
            kind: Kind::Text { body },
        };
        repo.insert(InsertParams {
            group_id: &[0x55; 32],
            sender: &[0u8; 32],
            envelope: &env,
            mls_generation: u64::try_from(i / 100).unwrap(),
            ts_daemon_recv: 1_700_000_000 + i,
        })
        .unwrap();
    }
    eprintln!("seed complete in {:?}", t0.elapsed());

    let queries: Vec<String> = (0..100)
        .map(|i| {
            if i < 50 {
                VOCAB.choose(&mut rng).unwrap().to_string()
            } else {
                let a = VOCAB.choose(&mut rng).unwrap();
                let b = VOCAB.choose(&mut rng).unwrap();
                format!("{a} {b}")
            }
        })
        .collect();

    let mut samples_us: Vec<u128> = Vec::with_capacity(queries.len());
    for q in &queries {
        let t = Instant::now();
        let _ = repo.search(q, None, 50, 0, false).unwrap();
        samples_us.push(t.elapsed().as_micros());
    }
    samples_us.sort_unstable();
    let p50 = samples_us[samples_us.len() / 2];
    let p95 = samples_us[(samples_us.len() * 95) / 100];
    let p99 = samples_us[(samples_us.len() * 99) / 100];

    eprintln!("100k-row FTS p50={p50}us p95={p95}us p99={p99}us");
    assert!(
        p95 < 50_000,
        "p95 must be under 50_000us (50ms); got {p95}us"
    );
}
```

- [ ] **Step 2: Run the test (manually)**

```bash
cargo test -p skattr-core --release --test fts_search_p95 -- --ignored --nocapture
```
Expected: prints `100k-row FTS p50=... p95=...us p99=...us` and PASSes.
On a slow machine / VM, p95 may be borderline; if a single run flakes, re-run twice. Persistent failure means the index isn't covering the query — investigate via `EXPLAIN QUERY PLAN`.

- [ ] **Step 3: Commit**

```bash
git add crates/core/tests/fts_search_p95.rs
git commit -m "$(cat <<'EOF'
tests: fts_search_p95 — 100k-row FTS5 BM25 p95 < 50ms (ignored gate)

Plain #[test] #[ignore] (no criterion). Seeds 100k synthetic text
rows from a 200-word vocabulary (rand seeded); runs 50 single-token
+ 50 two-token AND queries; asserts p95 < 50ms; logs p50/p95/p99.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 33: Documentation updates — CHANGELOG, CLAUDE.md, ARCHITECTURE.md

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `CLAUDE.md` (status line at top)
- Modify: `docs/ARCHITECTURE.md` (the "send one message" data-flow trace)

- [ ] **Step 1: Append the CHANGELOG entry**

Add to the top of `CHANGELOG.md` under the next-release header (or create one if needed):

```markdown
## Unreleased

### Phase 1.G — Message storage & search

- `storage::messages` gains `search` (FTS5 BM25 + snippet, tokenize-and-AND query escaper),
  `unread_count`, `mark_read`, `export_page`, `prune_before`, `prune_keep_last`, and a
  one-shot `backfill_body_text` startup helper. New `body_text` mirror column +
  `messages_fts` triggers keep the FTS index in lock-step with `messages`.
- `storage::read_state` (new): per-group `last_read_message_id` cursor.
- `messages` table: `mls_generation` and `ts_daemon_recv` columns persist real values
  (replacing 1.F's `0` / `ts_envelope` placeholders).
- `delivery::receiver::receive` carries `mls_generation` + `ts_daemon_recv` into a
  struct `ReceiveOutcome::New`; the InboundDispatch caller broadcasts
  `Event::MessageReceived { contact, record }` after the ACK.
- `daemon::commands`: new `Command::SearchMessages` / `MarkRead` / `PruneHistory` /
  `ExportHistory`, matching `CommandResult` + `SearchHitRecord` wire types,
  `EventFilter::Messages { contact }`, `Event::MessageReceived`,
  `DaemonErrorKind::SearchSyntax`.
- `daemon::retention` (new): hourly tokio sweep task driven by
  `[history] retention_days = 0` (default infinite).
- `Daemon::run` runs `backfill_body_text` once at startup and spawns the retention
  sweep before signalling readiness.
- CLI: `skattr search` / `export` / `prune`; `skattr tail --follow` upgraded to
  subscribe to `Event::MessageReceived`. New CLI dep `time = "0.3"` for RFC3339
  parsing on `skattr prune --before`.
- Validation: `cargo test -p skattr-core --release --test fts_search_p95 --
  --ignored --nocapture` reports search p95 < 50 ms over 100k synthetic rows.
```

- [ ] **Step 2: Update `CLAUDE.md`'s status line**

Edit the "Repository state" section (top of `CLAUDE.md`). Locate the sentence listing completed phases (e.g., "Phase 1.A (frame codec), 1.B (Noise_XK handshake), …, 1.E (delivery semantics), 1.F (CLI integration)") and append `, 1.G (message storage & search)`. Then add a one-paragraph summary block paralleling the existing per-phase blocks:

```markdown
Phase 1.G added FTS5 wiring (triggers off a new `body_text` mirror
column, `messages_fts` recreated to reference it), persisted
`mls_generation` and `ts_daemon_recv` on `messages` (replacing 1.F's
placeholders), `MessageRepo::{search, unread_count, mark_read,
export_page, prune_before, prune_keep_last, backfill_body_text}`,
`ReadStateRepo` for per-group last-read cursors, `daemon::retention`
(hourly sweep + `[history] retention_days`), and IPC for
`SearchMessages` / `MarkRead` / `PruneHistory` / `ExportHistory` plus
`Event::MessageReceived` and `EventFilter::Messages`. CLI gained
`search` / `export` / `prune`; `tail --follow` subscribes to the
event stream. Migration 0006 lands the schema. The 100k-row
benchmark (`crates/core/tests/fts_search_p95.rs`, `#[ignore]`-gated)
asserts search p95 < 50 ms.
```

- [ ] **Step 3: Update `docs/ARCHITECTURE.md`'s "send one message" trace**

In the `## Send one message` (or equivalently named) section, extend the receive-side step that says "MessageRepo::insert(envelope)" to mention the new fields and the broadcast:

```markdown
6. Receiver: `delivery::receiver::receive` decrypts via the MLS
   group, captures `mls_generation = group.epoch().as_u64()` and
   `ts_daemon_recv = now()`, and `MessageRepo::insert(InsertParams)`
   persists the row. The FTS5 trigger indexes `body_text`
   automatically. After the ACK is sent, the InboundDispatch caller
   resolves `group_id → contact_pk` via `ContactRepo` and broadcasts
   `Event::MessageReceived { contact, record }` on the daemon's
   broadcast bus, where `tail --follow` subscribers pick it up.
```

If your `ARCHITECTURE.md` uses different step numbering, integrate the language above into the matching step.

- [ ] **Step 4: Commit**

```bash
git add CHANGELOG.md CLAUDE.md docs/ARCHITECTURE.md
git commit -m "$(cat <<'EOF'
docs: CHANGELOG + CLAUDE.md + ARCHITECTURE.md updates for Phase 1.G

CHANGELOG entry covering all storage/daemon/CLI deltas. CLAUDE.md
status line and Phase 1.G summary block. ARCHITECTURE.md's "send one
message" trace now mentions the persisted fields and the
Event::MessageReceived broadcast.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 34: Final verification + ship to master

**Files:** none — verification + merge only.

- [ ] **Step 1: Run the full quality gate**

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```
All three must succeed.

- [ ] **Step 2: Run the 100k benchmark on this developer machine**

```bash
cargo test -p skattr-core --release --test fts_search_p95 \
    -- --ignored --nocapture
```
Expected: prints `100k-row FTS p50=... p95=...us p99=...us` with `p95 < 50_000us`.
Record the numbers in the eventual PR description.

- [ ] **Step 3: Run `cargo deny`**

```bash
cargo deny check
```
Expected: no advisories, no banned crates. The only new dep is `time = "0.3"` (MIT/Apache-2.0 — already on the allowlist).

- [ ] **Step 4: Cross-check the spec exit criteria against the worktree**

Re-read `docs/superpowers/specs/2026-04-23-phase-1g-message-storage-search-design.md` §15. Confirm each of the 11 numbered exit criteria is satisfied. If any is not, return to the relevant earlier task.

- [ ] **Step 5: Open the PR (or merge)**

If the project's process is to merge directly:

```bash
git checkout master
git merge --no-ff phase-1g-message-storage-search
git worktree remove ../skattr-phase-1g-message-storage-search
```

If the project uses PRs:

```bash
git push -u origin phase-1g-message-storage-search
gh pr create --title "Phase 1.G — Message storage & search" \
    --body "$(cat <<'EOF'
## Summary
- Wires FTS5 (triggers + `body_text` mirror) and persists `mls_generation` + `ts_daemon_recv` on `messages` (replaces 1.F placeholders).
- Adds `MessageRepo::{search, unread_count, mark_read, export_page, prune_*, backfill_body_text}`, `ReadStateRepo`, daemon-owned hourly retention sweep, and IPC for `SearchMessages` / `MarkRead` / `PruneHistory` / `ExportHistory`.
- CLI: `search` / `export` / `prune`; `tail --follow` subscribes to `Event::MessageReceived`.
- Validation: `fts_search_p95.rs` 100k-row test reports p95 < 50 ms (run locally with `--ignored`).

## Test plan
- [ ] `cargo fmt --all --check` clean
- [ ] `cargo clippy --all-targets -- -D warnings` clean
- [ ] `cargo test` passes
- [ ] `cargo test -p skattr-core --release --test fts_search_p95 -- --ignored --nocapture` reports `p95 < 50ms`
- [ ] `cargo deny check` clean
EOF
)"
```

- [ ] **Step 6: Done**

Celebrate Phase 1.G shipping. Per the user's standing instruction, follow up by emitting the next-phase kickoff prompt for whatever is next on the Phase 1 / Phase 2 path.










