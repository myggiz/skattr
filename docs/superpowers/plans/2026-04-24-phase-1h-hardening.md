# Phase 1.H — Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close out all 11 hardening items surfaced in Phase 1.G reviews so the daemon presents a stable correctness / error-taxonomy / CI surface before Phase 2 UI work begins.

**Architecture:** Four lanes — **L1 storage correctness** (migration 0007 with `(group_id, envelope_id)` uniqueness + transactional send/receive via a new `MlsProvider::save_in_tx`), **L2 error taxonomy** (subsystem sub-enums replace `CoreError::kind()` string matching + `DaemonErrorKind::InvalidArgument`), **L3 IPC/API polish** (`ContactRepo::contact_for_group`, `MessageRecord.row_id`), and **L4 hygiene & infra** (hoist `now_unix_seconds`, fixed-width group_id on `ReceiveOutcome::New`, cargo-deny in CI already landed — verification only, `serial_test` replacing the socket-path Mutex). Tasks execute sequentially in a single worktree.

**Tech Stack:** Rust 2021, Tokio, rusqlite 0.38, OpenMLS 0.8, `thiserror`, `serial_test` (new dev-dep). See `CLAUDE.md` for the full locked-decision list.

**Predecessor:** Phase 1.G merged at `dedc206`; design spec at `docs/superpowers/specs/2026-04-24-phase-1h-hardening-design.md`.

---

## File structure

**New files:**
- `crates/core/src/storage/migrations/0007_messages_envelope_id.sql` — column + trigger + unique index
- `crates/core/src/storage/error_kind.rs` — `StorageErrorKind`
- `crates/core/src/contact/error_kind.rs` — `ContactErrorKind`
- `crates/core/src/invite/error_kind.rs` — `InviteErrorKind`
- `crates/core/src/mls/error_kind.rs` — `MlsErrorKind`
- `crates/core/src/delivery/error_kind.rs` — `DeliveryErrorKind`
- `crates/core/src/transport/error_kind.rs` — `TransportErrorKind`
- `crates/core/src/daemon/clock.rs` — `now_unix_seconds()`

**Modified files (major):**
- `crates/core/src/error.rs` — `CoreError::<Subsystem>(String)` → `CoreError::<Subsystem>(<Subsystem>ErrorKind)`, structural `kind()`
- `crates/core/src/storage/messages.rs` — `envelope_id`, `backfill_envelope_id`, `insert_in_tx`, wrap `backfill_body_text` in tx
- `crates/core/src/storage/outbox.rs` — `insert_in_tx`
- `crates/core/src/storage/migrations.rs` — register migration 0007
- `crates/core/src/storage/contacts.rs` — `contact_for_group` helper
- `crates/core/src/mls/provider.rs` + `crates/core/src/mls/group.rs` — `save_in_tx`
- `crates/core/src/delivery/receiver.rs` — `receive_in_tx`, `ReceiveOutcome::New.group_id: [u8; 32]`
- `crates/core/src/daemon/dispatch.rs` — tx-wrapped `send_message`, use `contact_for_group`, `InvalidArgument`
- `crates/core/src/daemon/inbound.rs` — tx-wrapped `dispatch_for_group`, remove local `now_unix_seconds`
- `crates/core/src/daemon/mod.rs` — wire `backfill_envelope_id` on startup
- `crates/core/src/daemon/error_kind.rs` — add `InvalidArgument { message }`
- `crates/core/src/daemon/commands.rs` — `MessageRecord.row_id`
- `crates/cli/src/main.rs` — exit code 2 on `InvalidArgument`
- `crates/cli/src/ipc/resolve_socket_path` (wherever the Mutex test lives) — `#[serial]`
- `crates/cli/Cargo.toml` — `serial_test` dev-dep
- `CHANGELOG.md`, `CLAUDE.md` — Phase 1.H paragraph

**Tasks that only touch tests:** integration tests under `crates/core/tests/` and `crates/tests/src/` for regression coverage of items #1, #3, #4.

---

## Task 1: Introduce `StorageErrorKind` and sweep `CoreError::Storage(String)` callsites

**Why first:** Task 4 needs `StorageErrorKind::DuplicateMessage` to represent the uniqueness-constraint violation; Tasks 6–9 need `StorageErrorKind::Other(String)` as the escape hatch for all pre-existing `CoreError::Storage(format!("..."))` sites. Doing this once up front avoids churn.

**Files:**
- Create: `crates/core/src/storage/error_kind.rs`
- Modify: `crates/core/src/storage/mod.rs` (re-export), `crates/core/src/error.rs`
- Modify: every file under `crates/core/src/storage/` that constructs `CoreError::Storage(...)` — enumerate with grep.
- Modify: `crates/core/src/daemon/dispatch.rs` if it constructs `CoreError::Storage(...)` directly (spot grep)

- [ ] **Step 1: Create the enum**

Create `crates/core/src/storage/error_kind.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Typed storage-layer error kinds. Replaces free-form `String` payloads
//! so `CoreError::kind()` can project via a structural match instead of
//! `str::contains`.

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StorageErrorKind {
    /// FTS5 MATCH parse/syntax error. The inner string is the raw
    /// sqlite message for logs; the projected `DaemonErrorKind` is
    /// `SearchSyntax`.
    #[error("fts5 syntax error: {0}")]
    FtsSyntax(String),

    /// `(group_id, envelope_id)` UNIQUE violation. Send path maps this
    /// to `SendStatus::Delivered` (idempotent retry); receive path
    /// never sees it thanks to the `seen_messages` pre-check.
    #[error("duplicate message")]
    DuplicateMessage,

    /// Everything else — catch-all escape hatch during the Phase 1.H
    /// refactor. Prefer adding a typed variant over populating this.
    #[error("storage: {0}")]
    Other(String),
}
```

- [ ] **Step 2: Re-export from `storage/mod.rs`**

In `crates/core/src/storage/mod.rs`, add:

```rust
mod error_kind;
pub(crate) use error_kind::StorageErrorKind;
```

(Follow the visibility conventions already used in that file — `pub(crate)` if that's what neighboring items use.)

- [ ] **Step 3: Change `CoreError::Storage(String)` to `CoreError::Storage(StorageErrorKind)`**

In `crates/core/src/error.rs`:

```rust
/// Storage / migration / serialization problem.
#[error("{0}")]
Storage(#[from] crate::storage::StorageErrorKind),
```

Note: `#[error("{0}")]` defers the outer Display to the inner enum's Display. The `#[from]` gives you ergonomic `?` from `StorageErrorKind` call sites.

Keep the `#[from] rusqlite::Error` variant as-is — SQLite errors are still a distinct `CoreError::Sqlite` variant, not storage-layer.

- [ ] **Step 4: Update `CoreError::kind()` Storage arm**

In `crates/core/src/error.rs::kind()`:

```rust
CoreError::Storage(crate::storage::StorageErrorKind::FtsSyntax(_)) =>
    Some(K::SearchSyntax),
CoreError::Storage(crate::storage::StorageErrorKind::DuplicateMessage) =>
    Some(K::StorageError), // Phase 1.H: no dedicated Daemon variant; storage-level signal only
CoreError::Storage(crate::storage::StorageErrorKind::Other(_))
| CoreError::Sqlite(_) => Some(K::StorageError),
```

Remove the old `str::contains("fts5: syntax error") || s.contains("malformed MATCH")` arm.

- [ ] **Step 5: Sweep callsites — run grep to enumerate**

Run:

```bash
grep -rn 'CoreError::Storage(' crates/core/src/
```

Every match that isn't the error variant declaration or the `#[from]` line is a construction site. Rewrite patterns:

| Before | After |
|---|---|
| `CoreError::Storage(format!("prepare backfill: {e}"))` | `CoreError::Storage(StorageErrorKind::Other(format!("prepare backfill: {e}")))` |
| `CoreError::Storage("mls: inbound: no group for peer".into())` | (that one isn't Storage — leave it; grep's mixed) |
| FTS syntax paths (look for `malformed MATCH` / `fts5: syntax error`) | `CoreError::Storage(StorageErrorKind::FtsSyntax(raw_msg))` |

Leave one TODO-comment inline where you convert FTS syntax: `// TODO(1.H): StorageErrorKind::FtsSyntax set here — no more string matching in CoreError::kind`. Remove the TODO in Step 7.

- [ ] **Step 6: Write regression test for `kind()` structural match**

In `crates/core/src/error.rs` inside `#[cfg(test)] mod tests`:

```rust
#[test]
fn storage_fts_syntax_projects_to_search_syntax() {
    use crate::daemon::error_kind::DaemonErrorKind;
    use crate::storage::StorageErrorKind;
    let e = CoreError::Storage(StorageErrorKind::FtsSyntax("near \"foo\"".into()));
    assert_eq!(e.kind(), Some(DaemonErrorKind::SearchSyntax));
}

#[test]
fn storage_duplicate_message_projects_to_storage_error() {
    use crate::daemon::error_kind::DaemonErrorKind;
    use crate::storage::StorageErrorKind;
    let e = CoreError::Storage(StorageErrorKind::DuplicateMessage);
    assert_eq!(e.kind(), Some(DaemonErrorKind::StorageError));
}

#[test]
fn storage_other_projects_to_storage_error() {
    use crate::daemon::error_kind::DaemonErrorKind;
    use crate::storage::StorageErrorKind;
    let e = CoreError::Storage(StorageErrorKind::Other("prepare failed".into()));
    assert_eq!(e.kind(), Some(DaemonErrorKind::StorageError));
}
```

- [ ] **Step 7: Run `cargo build -p skattr-core` and fix remaining breakages**

```bash
cargo build -p skattr-core
```

Expected: compile errors at every unswept Storage construction site. Fix by wrapping in `StorageErrorKind::Other(...)` unless the string matches one of the typed patterns.

- [ ] **Step 8: Run tests + clippy**

```bash
cargo test -p skattr-core --lib
cargo clippy -p skattr-core --all-targets -- -D warnings
```

All green.

- [ ] **Step 9: Commit**

```bash
git add crates/core/src/storage/error_kind.rs crates/core/src/storage/mod.rs \
        crates/core/src/error.rs crates/core/src/storage/
git commit -m "core: introduce StorageErrorKind; structural kind() for Storage

First of the Phase 1.H error-taxonomy refactor (item #5). Replaces
CoreError::Storage(String) with a typed sub-enum {FtsSyntax,
DuplicateMessage, Other} so kind() stops string-matching for FTS
errors. Other subsystems (Contact/Invite/Mls/Delivery/Transport)
follow in subsequent tasks.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Migration 0007 — `messages.envelope_id` column, shape trigger, unique index

**Files:**
- Create: `crates/core/src/storage/migrations/0007_messages_envelope_id.sql`
- Modify: `crates/core/src/storage/migrations.rs`
- Test: `crates/core/src/storage/migrations.rs::tests`

- [ ] **Step 1: Write the migration SQL**

Create `crates/core/src/storage/migrations/0007_messages_envelope_id.sql`:

```sql
-- Phase 1.H: durable (group_id, envelope_id) uniqueness.
-- The column is nullable because SQLite can't ALTER a column to NOT NULL
-- mid-life; the trigger below enforces 16-byte shape on every new INSERT,
-- and a startup-time Rust backfill populates any NULLs from pre-1.H rows
-- (see MessageRepo::backfill_envelope_id).
ALTER TABLE messages ADD COLUMN envelope_id BLOB;

CREATE TRIGGER IF NOT EXISTS messages_envelope_id_shape
BEFORE INSERT ON messages
WHEN new.envelope_id IS NULL OR length(new.envelope_id) <> 16
BEGIN
    SELECT RAISE(ABORT, 'envelope_id must be 16 bytes');
END;

-- NULLs compare distinct by default in SQLite, so pre-backfill legacy
-- rows don't collide. Once backfill runs, the constraint becomes
-- meaningful. See spec §L1.a.
CREATE UNIQUE INDEX IF NOT EXISTS messages_group_envelope_uniq
    ON messages(group_id, envelope_id);
```

- [ ] **Step 2: Register the migration**

In `crates/core/src/storage/migrations.rs`, extend `ALL_MIGRATIONS`:

```rust
Migration {
    version: 7,
    sql: include_str!("migrations/0007_messages_envelope_id.sql"),
},
```

- [ ] **Step 3: Write the migration test**

In the `#[cfg(test)] mod tests` block of `migrations.rs`:

```rust
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

    // Trigger rejects a bad insert.
    let err = conn.execute(
        "INSERT INTO messages \
         (group_id, sender, envelope_id, ts_envelope, ts_daemon_recv, \
          mls_generation, kind, body_blob, body_text) \
         VALUES (?1, ?2, ?3, 0, 0, 0, 'text', NULL, NULL)",
        rusqlite::params![[0u8; 32], [0u8; 32], [0u8; 8]], // 8 bytes — wrong shape
    );
    assert!(err.is_err(), "trigger must reject non-16-byte envelope_id");
}
```

If the `messages` column list in your tree differs from the one above (older migrations add/remove columns), adjust the INSERT column list to match — the point is to exercise the trigger on `envelope_id`.

- [ ] **Step 4: Run the test**

```bash
cargo test -p skattr-core --lib migration_0007
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/storage/migrations/0007_messages_envelope_id.sql \
        crates/core/src/storage/migrations.rs
git commit -m "storage: migration 0007 — envelope_id column + uniqueness

Adds nullable messages.envelope_id, a BEFORE-INSERT trigger enforcing
16-byte shape on new rows, and a unique index on (group_id,
envelope_id). Pre-1.H rows keep NULL envelope_id until the startup
backfill populates them (next task).

Phase 1.H item #2.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: `MessageRepo::backfill_envelope_id` + daemon-startup wiring

**Files:**
- Modify: `crates/core/src/storage/messages.rs`
- Modify: `crates/core/src/daemon/mod.rs` (or wherever `Daemon::run` lives)
- Test: `crates/core/src/storage/messages.rs::tests`

- [ ] **Step 1: Write the failing test**

In the `#[cfg(test)] mod tests` block of `messages.rs`:

```rust
#[test]
fn backfill_envelope_id_populates_null_rows_from_body_blob() {
    let pool = Pool::in_memory();
    let gid = [0x70u8; 32];
    let sender = [0x71u8; 32];

    // Insert a row with body_blob set but envelope_id NULL — simulates
    // a pre-1.H row. Bypass the trigger by inserting envelope_id as a
    // dummy 16-byte value, then NULL it out (the trigger fires on
    // INSERT, not UPDATE).
    let env = crate::envelope::Envelope {
        v: 1,
        id: crate::envelope::MessageId::generate(),
        ts: 1_700_000_000,
        reply_to: None,
        kind: crate::envelope::Kind::Text { body: "hi".into() },
    };
    let expected_id = env.id.0;

    pool.with_mut(|c| {
        c.execute(
            "INSERT INTO messages \
             (group_id, sender, envelope_id, ts_envelope, ts_daemon_recv, \
              mls_generation, kind, body_blob, body_text) \
             VALUES (?1, ?2, ?3, ?4, 0, 0, 'text', ?5, 'hi')",
            rusqlite::params![
                &gid[..],
                &sender[..],
                &[0u8; 16][..], // dummy 16 bytes to satisfy trigger
                env.ts,
                env.encode().unwrap(),
            ],
        )
        .unwrap();
        // Now NULL the envelope_id (UPDATE bypasses the BEFORE-INSERT trigger).
        c.execute(
            "UPDATE messages SET envelope_id = NULL WHERE group_id = ?1",
            rusqlite::params![&gid[..]],
        )
        .unwrap();
        Ok(())
    })
    .unwrap();

    let n = MessageRepo::new(&pool).backfill_envelope_id().unwrap();
    assert_eq!(n, 1, "exactly one row backfilled");

    let got: Vec<u8> = pool
        .with(|c| {
            c.query_row(
                "SELECT envelope_id FROM messages WHERE group_id = ?1",
                rusqlite::params![&gid[..]],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .map_err(|e| crate::error::CoreError::Storage(
                crate::storage::StorageErrorKind::Other(e.to_string())
            ))
        })
        .unwrap();
    assert_eq!(got, expected_id, "backfilled envelope_id must match body_blob");
}

#[test]
fn backfill_envelope_id_is_idempotent() {
    let pool = Pool::in_memory();
    let gid = [0x72u8; 32];
    let repo = MessageRepo::new(&pool);
    repo.insert(InsertParams {
        group_id: &gid,
        sender: &[0x73u8; 32],
        envelope: &crate::envelope::Envelope {
            v: 1,
            id: crate::envelope::MessageId::generate(),
            ts: 0,
            reply_to: None,
            kind: crate::envelope::Kind::Text { body: "a".into() },
        },
        mls_generation: 0,
        ts_daemon_recv: 0,
    })
    .unwrap();
    // Row already has envelope_id populated by insert (next task wires
    // that); backfill must do nothing.
    let n = repo.backfill_envelope_id().unwrap();
    assert_eq!(n, 0);
}
```

- [ ] **Step 2: Run test — expect "method not found"**

```bash
cargo test -p skattr-core --lib backfill_envelope_id
```

Expected: FAIL — `no method named 'backfill_envelope_id' found`.

- [ ] **Step 3: Implement `backfill_envelope_id`**

Add to `crates/core/src/storage/messages.rs` (near `backfill_body_text`):

```rust
/// One-shot startup helper: populate `envelope_id` for any row whose
/// column is NULL (pre-1.H rows). Decodes `body_blob`, extracts the
/// envelope id, writes it in place. Skips rows whose blob fails to
/// decode. Wrapped in a single transaction so all N updates commit
/// atomically. Returns the number of rows backfilled. Idempotent.
pub(crate) fn backfill_envelope_id(&self) -> Result<u64> {
    let candidates: Vec<(i64, Vec<u8>)> = self.pool.with(|c| {
        let mut stmt = c
            .prepare(
                "SELECT id, body_blob FROM messages WHERE envelope_id IS NULL",
            )
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(
                format!("prepare backfill_envelope_id: {e}"),
            )))?;
        let it = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(
                format!("query backfill_envelope_id: {e}"),
            )))?;
        let v: std::result::Result<Vec<_>, _> = it.collect();
        v.map_err(|e| CoreError::Storage(StorageErrorKind::Other(
            format!("collect backfill_envelope_id: {e}"),
        )))
    })?;

    if candidates.is_empty() {
        return Ok(0);
    }

    let mut updated = 0u64;
    self.pool.transaction(|tx| {
        for (row_id, blob) in &candidates {
            let env = match crate::envelope::Envelope::decode(blob) {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(
                        row_id = *row_id,
                        error = %e,
                        "backfill_envelope_id: skipping row whose body_blob \
                         failed to decode"
                    );
                    continue;
                }
            };
            match tx.execute(
                "UPDATE messages SET envelope_id = ?1 WHERE id = ?2",
                rusqlite::params![&env.id.0[..], row_id],
            ) {
                Ok(_) => updated += 1,
                Err(rusqlite::Error::SqliteFailure(e, _))
                    if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE =>
                {
                    // Pre-existing duplicate (group_id, envelope_id) —
                    // keep the lowest row id, delete this one.
                    tracing::warn!(
                        row_id = *row_id,
                        "backfill_envelope_id: duplicate (group_id, envelope_id) \
                         detected; deleting higher-id duplicate"
                    );
                    tx.execute(
                        "DELETE FROM messages WHERE id = ?1",
                        rusqlite::params![row_id],
                    )
                    .map_err(|e| CoreError::Storage(StorageErrorKind::Other(
                        format!("backfill dedupe delete: {e}"),
                    )))?;
                }
                Err(e) => {
                    return Err(CoreError::Storage(StorageErrorKind::Other(
                        format!("backfill UPDATE: {e}"),
                    )));
                }
            }
        }
        Ok(())
    })?;
    Ok(updated)
}
```

- [ ] **Step 4: Run the test**

```bash
cargo test -p skattr-core --lib backfill_envelope_id
```

Expected: PASS.

- [ ] **Step 5: Wire into daemon startup**

Find the spot in `Daemon::run` where `backfill_body_text` is called. Looks roughly like:

```rust
MessageRepo::new(&pool).backfill_body_text()?;
```

Add immediately after:

```rust
MessageRepo::new(&pool).backfill_envelope_id()?;
```

(If `backfill_body_text` isn't wired at startup yet, wire both — reason: migration 0006 introduced body_text the same way.)

If you can't find the call site, grep:

```bash
grep -rn 'backfill_body_text' crates/core/src/daemon/
```

- [ ] **Step 6: Run full test suite**

```bash
cargo test -p skattr-core
cargo clippy -p skattr-core --all-targets -- -D warnings
```

All green.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/storage/messages.rs crates/core/src/daemon/
git commit -m "storage: backfill_envelope_id + daemon-startup wiring

Idempotent one-shot that populates messages.envelope_id from
body_blob on any row where the 0007 migration left it NULL
(pre-1.H rows). Resolves pre-existing (group_id, envelope_id)
duplicates by keeping the lowest row id. Runs inside Daemon::run
alongside backfill_body_text.

Phase 1.H item #2.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: `MessageRepo::insert` populates `envelope_id`; duplicate maps to `DuplicateMessage`

**Files:**
- Modify: `crates/core/src/storage/messages.rs`
- Test: same file

- [ ] **Step 1: Write the failing test**

Add to `messages.rs` tests:

```rust
#[test]
fn insert_populates_envelope_id_column() {
    let pool = Pool::in_memory();
    let gid = [0x74u8; 32];
    let sender = [0x75u8; 32];
    let env = crate::envelope::Envelope {
        v: 1,
        id: crate::envelope::MessageId::generate(),
        ts: 0,
        reply_to: None,
        kind: crate::envelope::Kind::Text { body: "x".into() },
    };
    let expected = env.id.0;

    let repo = MessageRepo::new(&pool);
    repo.insert(InsertParams {
        group_id: &gid,
        sender: &sender,
        envelope: &env,
        mls_generation: 0,
        ts_daemon_recv: 0,
    })
    .unwrap();

    let got: Vec<u8> = pool
        .with(|c| {
            c.query_row(
                "SELECT envelope_id FROM messages WHERE group_id = ?1",
                rusqlite::params![&gid[..]],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .map_err(|e| crate::error::CoreError::Storage(
                crate::storage::StorageErrorKind::Other(e.to_string())
            ))
        })
        .unwrap();
    assert_eq!(got, expected);
}

#[test]
fn insert_duplicate_envelope_id_returns_duplicate_message_error() {
    use crate::error::CoreError;
    use crate::storage::StorageErrorKind;

    let pool = Pool::in_memory();
    let gid = [0x76u8; 32];
    let sender = [0x77u8; 32];
    let env = crate::envelope::Envelope {
        v: 1,
        id: crate::envelope::MessageId::generate(),
        ts: 0,
        reply_to: None,
        kind: crate::envelope::Kind::Text { body: "y".into() },
    };

    let repo = MessageRepo::new(&pool);
    repo.insert(InsertParams {
        group_id: &gid,
        sender: &sender,
        envelope: &env,
        mls_generation: 0,
        ts_daemon_recv: 0,
    })
    .unwrap();

    let err = repo
        .insert(InsertParams {
            group_id: &gid,
            sender: &sender,
            envelope: &env, // same envelope.id as above
            mls_generation: 1,
            ts_daemon_recv: 1,
        })
        .unwrap_err();

    assert!(
        matches!(err, CoreError::Storage(StorageErrorKind::DuplicateMessage)),
        "expected DuplicateMessage, got {err:?}"
    );
}
```

- [ ] **Step 2: Run — expect fail**

```bash
cargo test -p skattr-core --lib 'insert_(populates|duplicate)'
```

Expected: both FAIL (either no envelope_id bound or no typed error).

- [ ] **Step 3: Modify `MessageRepo::insert`**

Find the `insert` function in `messages.rs`. Add `envelope_id` to the INSERT. The exact SQL currently reads roughly:

```rust
"INSERT INTO messages \
 (group_id, sender, ts_envelope, ts_daemon_recv, mls_generation, \
  kind, body_blob, body_text) \
 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
```

Change to include `envelope_id`:

```rust
"INSERT INTO messages \
 (group_id, sender, envelope_id, ts_envelope, ts_daemon_recv, \
  mls_generation, kind, body_blob, body_text) \
 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
```

Bind `&params.envelope.id.0[..]` as `?3`; renumber the remaining binds.

Wrap the `c.execute` call to map the UNIQUE violation to the typed error:

```rust
match c.execute(sql, rusqlite::params![...]) {
    Ok(_) => {}
    Err(rusqlite::Error::SqliteFailure(e, _))
        if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE =>
    {
        return Err(CoreError::Storage(StorageErrorKind::DuplicateMessage));
    }
    Err(e) => {
        return Err(CoreError::Storage(StorageErrorKind::Other(
            format!("messages INSERT: {e}"),
        )));
    }
}
```

(If the existing code returns `c.last_insert_rowid()`, keep that after the match.)

- [ ] **Step 4: Run the tests**

```bash
cargo test -p skattr-core --lib 'insert_(populates|duplicate)'
```

Expected: PASS.

- [ ] **Step 5: Run the full `messages.rs` test module**

```bash
cargo test -p skattr-core --lib storage::messages
```

Fix any existing tests that broke because the INSERT column count changed (e.g., direct SQL inserts in tests must now supply 16 bytes).

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/storage/messages.rs
git commit -m "storage: MessageRepo::insert binds envelope_id; typed DuplicateMessage

insert() now binds the 16-byte envelope_id on every row. UNIQUE
constraint violations (sqlite extended code SQLITE_CONSTRAINT_UNIQUE)
project to CoreError::Storage(StorageErrorKind::DuplicateMessage),
letting callers distinguish idempotent retry from genuine storage
failure.

Phase 1.H item #2.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: Wrap `backfill_body_text` in a single transaction (item #8)

**Files:**
- Modify: `crates/core/src/storage/messages.rs::backfill_body_text`

- [ ] **Step 1: Re-read the current implementation** (already in context above).

The `pool.with_mut` block on lines ~417–438 auto-commits on every `c.execute`. We replace `with_mut` with `pool.transaction`.

- [ ] **Step 2: Rewrite the update loop**

Replace the existing `self.pool.with_mut(|c| { for ... })?;` block with:

```rust
let mut updated = 0u64;
self.pool.transaction(|tx| {
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
            tx.execute(
                "UPDATE messages SET body_text = ?1 WHERE id = ?2",
                rusqlite::params![body, id],
            )
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(
                format!("backfill UPDATE: {e}"),
            )))?;
            updated += 1;
        }
    }
    Ok(())
})?;
Ok(updated)
```

- [ ] **Step 3: Run existing tests**

```bash
cargo test -p skattr-core --lib backfill_body_text
```

Both existing tests (`backfill_body_text_decodes_legacy_text_rows_and_indexes_fts` and `backfill_body_text_is_idempotent`) must still pass.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/storage/messages.rs
git commit -m "storage: wrap backfill_body_text in a single transaction

Previously each per-row UPDATE auto-committed -> N fsyncs. Now all
updates run inside one pool.transaction -> exactly one fsync.
Matches the pattern of backfill_envelope_id (Task 3).

Phase 1.H item #8.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: `MlsProvider::save_in_tx` + `Group::save_in_tx`

**Files:**
- Modify: `crates/core/src/mls/provider.rs`
- Modify: `crates/core/src/mls/group.rs`
- Modify: `crates/core/src/storage/mls_groups.rs` (or wherever `write_snapshot` lives)
- Test: `crates/core/src/mls/group.rs::tests`

- [ ] **Step 1: Understand the current save path**

Run:

```bash
grep -rn 'fn save' crates/core/src/mls/
grep -rn 'MlsGroupRepo::\(write_snapshot\|insert\|upsert\|save\)' crates/core/src/
```

You should find `Group::save(&self, repo: &MlsGroupRepo) -> Result<()>` and the repo method that actually writes the `mls_groups` row. The OpenMLS `StorageProvider` also writes through its own internal blob — the existing `MlsProvider` already bridges this to our pool.

- [ ] **Step 2: Add `save_in_tx` on the repo**

In `crates/core/src/storage/mls_groups.rs` (or equivalent), add a variant of the existing save method that takes a `&rusqlite::Transaction` instead of using the pool. The existing method can be refactored to call the new one inside its own tx:

```rust
pub(crate) fn save_in_tx(
    &self,
    tx: &rusqlite::Transaction<'_>,
    group_id: &[u8],
    snapshot: &[u8],
    epoch: u64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO mls_groups (group_id, snapshot, epoch) VALUES (?1, ?2, ?3) \
         ON CONFLICT(group_id) DO UPDATE SET snapshot = excluded.snapshot, \
                                              epoch    = excluded.epoch",
        rusqlite::params![group_id, snapshot, i64::try_from(epoch).unwrap_or(i64::MAX)],
    )
    .map_err(|e| CoreError::Storage(StorageErrorKind::Other(
        format!("mls_groups upsert: {e}"),
    )))?;
    Ok(())
}

pub(crate) fn save(
    &self,
    group_id: &[u8],
    snapshot: &[u8],
    epoch: u64,
) -> Result<()> {
    self.pool.transaction(|tx| self.save_in_tx(tx, group_id, snapshot, epoch))
}
```

Adjust signature to match the real existing method — the essential shape is "thin transactional wrapper over save_in_tx."

- [ ] **Step 3: Add `Group::save_in_tx`**

In `crates/core/src/mls/group.rs`:

```rust
/// Transactional companion to `save`. Writes the MLS snapshot inside
/// the caller's `tx` without taking a new pool lock. Used by
/// `daemon::dispatch::send_message` and `daemon::inbound::
/// dispatch_for_group` so group state and message-row persistence
/// commit atomically.
pub(crate) fn save_in_tx(
    &self,
    repo: &MlsGroupRepo,
    tx: &rusqlite::Transaction<'_>,
) -> Result<()> {
    let snapshot = self.provider.serialize_snapshot(&self.id)?;
    repo.save_in_tx(tx, &self.id.0, &snapshot, self.epoch())
}
```

The existing `Group::save` becomes:

```rust
pub(crate) fn save(&self, repo: &MlsGroupRepo) -> Result<()> {
    let snapshot = self.provider.serialize_snapshot(&self.id)?;
    repo.save(&self.id.0, &snapshot, self.epoch())
}
```

If `MlsProvider::serialize_snapshot` doesn't exist as a pure accessor (i.e., today's code writes directly through the provider), add it — it should return the serialized bytes without side-effecting SQLite. If OpenMLS's `StorageProvider` makes this difficult, the fallback is to have `MlsProvider` expose a "flush to byte buffer" method that the caller then binds to the tx.

> **Fallback branch point:** if `MlsProvider` cannot be cleanly made to serialize-without-writing, revert to the spec's Option B (insert-before-save reorder). Document the pivot with a comment in this task's commit message.

- [ ] **Step 4: Write a test that verifies rollback**

Add to `crates/core/src/mls/group.rs::tests`:

```rust
#[test]
fn save_in_tx_rolls_back_on_abort() {
    let pool = crate::storage::Pool::in_memory();
    let repo = crate::storage::MlsGroupRepo::new(&pool);
    let seed = crate::identity::Seed::generate().unwrap();
    let id = crate::identity::IdentityKey::from_seed(&seed).unwrap();
    let group = Group::create_solo(&id, None, crate::mls::provider::MlsProvider::new())
        .unwrap();

    // Run save_in_tx inside a tx we explicitly roll back.
    let result: crate::error::Result<()> = pool.transaction(|tx| {
        group.save_in_tx(&repo, tx)?;
        // Sanity-check: the row is visible inside this tx.
        let n: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM mls_groups WHERE group_id = ?1",
                rusqlite::params![&group.id().0[..]],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "snapshot visible inside tx");
        // Force rollback via Err.
        Err(crate::error::CoreError::Storage(
            crate::storage::StorageErrorKind::Other("rollback test".into()),
        ))
    });
    assert!(result.is_err());

    // After rollback, the row must not exist.
    let n: i64 = pool
        .with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM mls_groups WHERE group_id = ?1",
                rusqlite::params![&group.id().0[..]],
                |r| r.get(0),
            )
            .map_err(|e| crate::error::CoreError::Storage(
                crate::storage::StorageErrorKind::Other(e.to_string())
            ))
        })
        .unwrap();
    assert_eq!(n, 0, "tx rollback must leave mls_groups empty");
}
```

- [ ] **Step 5: Run the test**

```bash
cargo test -p skattr-core --lib save_in_tx_rolls_back
```

Expected: PASS.

- [ ] **Step 6: Verify existing `Group::save` callers still work**

```bash
cargo test -p skattr-core
cargo clippy -p skattr-core --all-targets -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/mls/group.rs crates/core/src/mls/provider.rs \
        crates/core/src/storage/mls_groups.rs
git commit -m "mls: Group::save_in_tx — transactional snapshot persistence

Adds a save_in_tx variant that writes the MLS snapshot inside the
caller's rusqlite::Transaction so group state can commit atomically
with message-row and outbox inserts. Existing Group::save becomes a
thin wrapper that opens its own tx.

Prep for Phase 1.H item #3 (durability gap in send/receive paths).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: `MessageRepo::insert_in_tx` + `OutboxRepo::insert_in_tx`

**Files:**
- Modify: `crates/core/src/storage/messages.rs`
- Modify: `crates/core/src/storage/outbox.rs`

- [ ] **Step 1: Split `MessageRepo::insert` into tx + wrapper**

Add `insert_in_tx`:

```rust
pub(crate) fn insert_in_tx(
    &self,
    tx: &rusqlite::Transaction<'_>,
    params: InsertParams<'_>,
) -> Result<i64> {
    // Copy the body of the existing insert here, but bind to tx
    // instead of calling pool.with_mut. Same DuplicateMessage mapping.
}

pub fn insert(&self, params: InsertParams<'_>) -> Result<i64> {
    self.pool.transaction(|tx| self.insert_in_tx(tx, params))
}
```

- [ ] **Step 2: Same split for `OutboxRepo::insert`**

`OutboxRepo::insert` currently takes `(target, message_id, ciphertext, attempts)` and writes to `outbox` with `(target, message_id)` idempotency. Mirror the pattern:

```rust
pub(crate) fn insert_in_tx(
    &self,
    tx: &rusqlite::Transaction<'_>,
    target: &[u8],
    message_id: &[u8],
    ciphertext: &[u8],
    attempts: u32,
) -> Result<()> {
    // Move the body of the existing insert here; bind tx.
}

pub fn insert(&self, target: &[u8], message_id: &[u8],
              ciphertext: &[u8], attempts: u32) -> Result<()> {
    self.pool.transaction(|tx| {
        self.insert_in_tx(tx, target, message_id, ciphertext, attempts)
    })
}
```

- [ ] **Step 3: Run the existing test suite for both repos**

```bash
cargo test -p skattr-core --lib 'storage::(messages|outbox)'
```

All existing tests must still pass — the public API is unchanged.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/storage/messages.rs crates/core/src/storage/outbox.rs
git commit -m "storage: insert_in_tx variants for MessageRepo + OutboxRepo

Splits the INSERT bodies into tx-accepting and pool-opening halves so
daemon::dispatch::send_message and daemon::inbound::dispatch_for_group
can commit group+message+outbox together in one transaction.

Prep for Phase 1.H item #3.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: `send_message` transactional (item #3 — send path)

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs::send_message`
- Test: same file

- [ ] **Step 1: Write the failing test — rollback on insert failure**

Add to the `#[cfg(test)] mod tests` block of `dispatch.rs`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_message_rolls_back_group_save_on_duplicate_insert() {
    // Alice/Bob invite dance (copy from existing tests — the two
    // already named `send_message_*` are the template).
    let handle_a = test_handle();
    handle_a.set_onion("alice.onion".to_string());
    let CommandResult::InviteCreated { url, .. } = execute_command(
        handle_a.clone(),
        Command::CreateInvite { nickname: None, ttl_secs: Some(3600) },
    ).await.unwrap() else { panic!("expected InviteCreated") };
    let handle_b = test_handle();
    let CommandResult::ContactAdded(summary) = execute_command(
        handle_b.clone(),
        Command::AddContact { invite_url: url },
    ).await.unwrap() else { panic!("expected ContactAdded") };

    // Snapshot bob's group epoch pre-send.
    use crate::mls::{Group, GroupId};
    use crate::storage::MlsGroupRepo;
    let contact_repo = ContactRepo::new(&handle_b.pool);
    let group_id = contact_repo.get_group_id(&summary.pubkey).unwrap().unwrap();
    let group_repo = MlsGroupRepo::new(&handle_b.pool);
    let pre_epoch = Group::load(&GroupId(group_id.clone()), &group_repo)
        .unwrap()
        .unwrap()
        .epoch();

    // Pre-seed a messages row with the same envelope_id that the next
    // send will generate. Since MessageId::generate() uses OsRng, we
    // can't predict it — instead, use the IPC to send once (row 1),
    // then craft a direct duplicate insert to *force* the next send's
    // tx to fail. Simpler path: drive duplicate via the envelope we
    // already know.
    //
    // Do two sends back-to-back. Between them, capture the outbound
    // row's envelope_id from the DB and pre-seed a conflicting row in
    // a DIFFERENT group_id's slot? — no, the index is per group.
    //
    // Cleanest test: skip the dispatch layer and invoke the repo's
    // insert_in_tx twice with the same (group_id, envelope_id) inside
    // Group.save_in_tx+insert_in_tx order. Assert second call fails
    // with DuplicateMessage AND group.load's epoch is unchanged.
    // Covered by Task 4's `insert_duplicate_envelope_id_returns_...`
    // plus Task 6's `save_in_tx_rolls_back_on_abort`, but we add an
    // integration flavor here.

    // Pragmatic test: build an envelope manually, encrypt via the
    // real Group, advance bob's in-memory epoch, then attempt a tx
    // that insert_in_tx will reject via the SeenMessagesRepo-style
    // conflict. Use the real send_message flow once, then manually
    // re-issue the same envelope.id through insert_in_tx to assert
    // DuplicateMessage propagates.

    let res = execute_command(
        handle_b.clone(),
        Command::SendMessage {
            contact: summary.pubkey,
            kind: crate::envelope::Kind::Text { body: "first".into() },
        },
    ).await.unwrap();
    assert!(matches!(res, CommandResult::MessageSent { .. }));

    // At this point bob's epoch is pre_epoch + 1; a second send
    // normally gets a fresh envelope.id so would succeed — we just
    // verify the ratchet DID advance after a successful tx.
    let post_epoch = Group::load(&GroupId(group_id.clone()), &group_repo)
        .unwrap()
        .unwrap()
        .epoch();
    assert!(post_epoch > pre_epoch,
            "ratchet must advance on successful send");
}
```

> **Test rationale:** directly simulating "group.save succeeded but insert failed" is hard through the public IPC. The minimum regression this integration test locks in is **"after a successful send the on-disk epoch advanced,"** which a non-transactional implementation also satisfies. Tasks 4 and 6's unit tests already cover the rollback-on-duplicate and tx-rollback-semantics pieces individually. If you want tighter integration coverage, inject a failing `MessageRepo` via a trait object on `DaemonHandle` — but that's a larger refactor and out of scope for 1.H.

- [ ] **Step 2: Rewrite `send_message`'s persistence block**

Replace the sequence `group.save(&group_repo)? ; msg_repo.insert(...)?; outbox_repo.insert(...)?` with:

```rust
// 5. Atomic: save advanced ratchet + insert message row + enqueue outbox.
let mls_generation = group.epoch();
let ts_daemon_recv = now_ms / 1000;
let insert_result = handle.pool.transaction(|tx| {
    group.save_in_tx(&group_repo, tx)?;
    msg_repo.insert_in_tx(
        tx,
        crate::storage::messages::InsertParams {
            group_id: &group_id_bytes,
            sender: &handle.identity.public().0,
            envelope: &envelope,
            mls_generation,
            ts_daemon_recv,
        },
    )?;
    outbox_repo.insert_in_tx(tx, &contact.0, &message_id.0, &ciphertext, 0)?;
    Ok(())
});

match insert_result {
    Ok(()) => { /* continue to hub.send */ }
    Err(CoreError::Storage(crate::storage::StorageErrorKind::DuplicateMessage)) => {
        // Retry replay: the envelope was already persisted earlier.
        // Treat as Delivered — the prior attempt already owns the row.
        return Ok(CommandResult::MessageSent {
            message_id: crate::daemon::hex::Hex16::from(message_id.0),
            status: SendStatus::Delivered,
        });
    }
    Err(e) => return Err(map_err(e)),
}
```

Important: at the top of `send_message`, bring the repos into scope BEFORE the transaction so they compile inside the closure:

```rust
let msg_repo = crate::storage::MessageRepo::new(&handle.pool);
let outbox_repo = OutboxRepo::new(&handle.pool);
let group_repo = MlsGroupRepo::new(&handle.pool); // already present
```

Then the `group.save(&group_repo)?` call earlier is gone — it now happens only inside the tx closure.

- [ ] **Step 3: Run existing send-path tests**

```bash
cargo test -p skattr-core --lib send_message
```

All existing tests (`send_message_to_unknown_contact_returns_contact_not_found`, `send_message_without_group_returns_contact_not_found`, `send_message_with_real_group_yields_queued_without_transport`, `send_message_persists_post_encrypt_mls_generation_and_ts_daemon_recv`) must still pass.

- [ ] **Step 4: Run the new regression test**

```bash
cargo test -p skattr-core --lib send_message_rolls_back_group_save_on_duplicate_insert
```

Expected: PASS (ratchet advances on success).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "daemon: send_message persistence in one transaction

Wraps group.save + messages insert + outbox insert in a single
pool.transaction. On DuplicateMessage (idempotent retry), returns
SendStatus::Delivered without re-advancing the ratchet. On any
other error, the tx rolls back and the in-memory ratchet advance is
discarded — next send reloads from disk.

Phase 1.H item #3 (send path).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 9: `receive_in_tx` + `dispatch_for_group` transactional (item #3 — receive path)

**Files:**
- Modify: `crates/core/src/delivery/receiver.rs`
- Modify: `crates/core/src/daemon/inbound.rs`
- Modify: `crates/core/src/storage/seen_messages.rs` (add `mark_seen_in_tx`)
- Test: `crates/core/src/daemon/inbound.rs::tests`

- [ ] **Step 1: Inspect the current `receive` signature**

Run:

```bash
grep -n 'pub.*fn receive' crates/core/src/delivery/receiver.rs
```

It takes `(&PublicKey, &[u8], Envelope, i64, u64, i64, &SeenMessagesRepo, &MessageRepo) -> Result<ReceiveOutcome>` and calls the two repos internally.

- [ ] **Step 2: Add `SeenMessagesRepo::mark_seen_in_tx`**

Mirror the pattern from Task 7:

```rust
pub(crate) fn mark_seen_in_tx(
    &self,
    tx: &rusqlite::Transaction<'_>,
    sender: &[u8],
    message_id: &[u8],
    ts_seen: i64,
) -> Result<bool> {
    // Return true if newly inserted (fresh), false if already existed (duplicate).
    // Use INSERT OR IGNORE and check changes().
    let n = tx.execute(
        "INSERT OR IGNORE INTO seen_messages (sender, message_id, ts_seen) \
         VALUES (?1, ?2, ?3)",
        rusqlite::params![sender, message_id, ts_seen],
    )
    .map_err(|e| CoreError::Storage(StorageErrorKind::Other(
        format!("seen_messages insert: {e}"),
    )))?;
    Ok(n == 1)
}
```

Adjust to match your actual `SeenMessagesRepo` shape — the existing method already encodes dedup semantics; your job is to split it into tx + wrapper forms. Add the wrapper:

```rust
pub fn mark_seen(&self, sender: &[u8], message_id: &[u8], ts_seen: i64)
    -> Result<bool>
{
    self.pool.transaction(|tx| self.mark_seen_in_tx(tx, sender, message_id, ts_seen))
}
```

- [ ] **Step 3: Add `receive_in_tx`**

In `crates/core/src/delivery/receiver.rs`:

```rust
/// Transactional companion to `receive`. The caller holds a
/// `rusqlite::Transaction`; both seen-messages dedup and message-row
/// persistence run inside it. On success returns the same
/// `ReceiveOutcome` as `receive()`.
pub(crate) fn receive_in_tx(
    tx: &rusqlite::Transaction<'_>,
    from: &PublicKey,
    group_id: &[u8],
    envelope: Envelope,
    now_ms: i64,
    mls_generation: u64,
    ts_daemon_recv: i64,
    seen_repo: &SeenMessagesRepo,
    msg_repo: &MessageRepo,
) -> Result<ReceiveOutcome> {
    // Copy the body of existing `receive()` here, replacing
    // seen_repo.mark_seen / msg_repo.insert with their *_in_tx
    // variants. The replay-window check and dedup logic is unchanged.
}

pub fn receive(
    from: &PublicKey,
    group_id: &[u8],
    envelope: Envelope,
    now_ms: i64,
    mls_generation: u64,
    ts_daemon_recv: i64,
    seen_repo: &SeenMessagesRepo,
    msg_repo: &MessageRepo,
) -> Result<ReceiveOutcome> {
    seen_repo.pool().transaction(|tx| {
        receive_in_tx(tx, from, group_id, envelope, now_ms,
                      mls_generation, ts_daemon_recv, seen_repo, msg_repo)
    })
}
```

If `SeenMessagesRepo` doesn't expose `pool()` publicly, add a `pub(crate) fn pool(&self) -> &Arc<Pool>` getter — or refactor `receive()` to take the `&Pool` directly.

- [ ] **Step 4: Rewrite `DaemonInbound::dispatch_for_group` to tx-wrap save + receive_in_tx**

In `crates/core/src/daemon/inbound.rs`, replace the block:

```rust
let envelope = group.decrypt(ciphertext)?;
group.save(&group_repo)?;
// ... receive(...)
```

with:

```rust
let envelope = group.decrypt(ciphertext)?;
let msg_id = envelope.id;
let mls_generation = group.epoch();
let ts_daemon_recv = crate::daemon::clock::now_unix_seconds();
let now_ms = ts_daemon_recv.saturating_mul(1000);

let msg_repo = MessageRepo::new(&self.pool);
let seen_repo = SeenMessagesRepo::new(&self.pool);

let outcome = self.pool.transaction(|tx| {
    group.save_in_tx(&group_repo, tx)?;
    crate::delivery::receiver::receive_in_tx(
        tx, &from, group_id, envelope, now_ms,
        mls_generation, ts_daemon_recv, &seen_repo, &msg_repo,
    )
})?;

match &outcome {
    ReceiveOutcome::New { envelope, row_id, mls_generation, ts_daemon_recv, .. } => {
        // existing event-broadcast block — outside the tx
    }
    // ... Duplicate / Rejected unchanged
}
```

Note `crate::daemon::clock::now_unix_seconds()` — Task 15 lands that module. Until then, keep the local `now_unix_seconds` and replace it in Task 15.

- [ ] **Step 5: Write the receive-side rollback test**

Add to `daemon/inbound.rs::tests`:

```rust
#[tokio::test]
async fn dispatch_for_group_rollback_leaves_group_epoch_unchanged() {
    // Alice/Bob MLS group setup — copy from the existing
    // `dispatch_emits_event_after_successful_decrypt` test (lines
    // 204–279 in inbound.rs).
    //
    // Instead of dispatching a valid ciphertext, dispatch a message
    // whose envelope.id collides with an already-seen entry (insert
    // into seen_messages directly before dispatch). The receive_in_tx
    // will return ReceiveOutcome::Duplicate — but the *prior* branch
    // tests cover Duplicate returning Ok.
    //
    // For the rollback path, feed a ciphertext that decrypts OK but
    // whose envelope carries ts far outside the ±1h window (e.g.
    // now_ms - 10h). receive_in_tx returns Rejected → dispatch_for_group
    // returns Err → the group.save_in_tx rolls back.
    //
    // Assert: alice_group's on-disk epoch did NOT advance.

    // (Full test body — paste the setup from the existing test,
    // then after the successful Add phase, encrypt an envelope with
    // ts = now_ms - 10 * 3600 * 1000 and dispatch it. Expect
    // dispatch_inner returns Err. Load the group from disk and
    // assert its epoch equals `expected_epoch` from before dispatch.)
}
```

Full boilerplate omitted here because the test setup exactly mirrors `dispatch_emits_event_after_successful_decrypt` (already in-tree at `crates/core/src/daemon/inbound.rs:204`). Copy that test, change the envelope's `ts` to `now_ms - 10 * 3600 * 1000`, and assert `Group::load(&...).epoch() == pre_epoch` after dispatch returns Err.

- [ ] **Step 6: Run the new test + existing receive tests**

```bash
cargo test -p skattr-core --lib 'dispatch_(for_group|emits|returns)'
cargo test -p skattr-core --lib 'delivery::receiver'
```

All green.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/delivery/receiver.rs crates/core/src/daemon/inbound.rs \
        crates/core/src/storage/seen_messages.rs
git commit -m "daemon: dispatch_for_group persistence in one transaction

Adds delivery::receiver::receive_in_tx (tx-accepting variant of
receive) and rewrites DaemonInbound::dispatch_for_group to wrap
Group::save_in_tx + receive_in_tx in one pool.transaction. Event
broadcast stays outside the tx — a failed subscriber does not roll
back persistence.

Phase 1.H item #3 (receive path).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 10: `ContactErrorKind` + sweep

**Files:**
- Create: `crates/core/src/contact/error_kind.rs`
- Modify: `crates/core/src/contact/mod.rs` (re-export)
- Modify: `crates/core/src/error.rs`
- Modify: every `CoreError::Contact(...)` construction site

- [ ] **Step 1: Create the enum**

`crates/core/src/contact/error_kind.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ContactErrorKind {
    #[error("contact not found")]
    NotFound,
    #[error("contact ambiguous ({matches} matches)")]
    Ambiguous { matches: u32 },
    #[error("contact: {0}")]
    Other(String),
}
```

- [ ] **Step 2: Re-export + change `CoreError::Contact`**

In `crates/core/src/contact/mod.rs`:

```rust
mod error_kind;
pub(crate) use error_kind::ContactErrorKind;
```

In `crates/core/src/error.rs`:

```rust
#[error("{0}")]
Contact(#[from] crate::contact::ContactErrorKind),
```

- [ ] **Step 3: Update `CoreError::kind()`**

Replace the two `str::contains` arms for `CoreError::Contact(_)` with:

```rust
CoreError::Contact(ContactErrorKind::NotFound) => Some(K::ContactNotFound),
CoreError::Contact(ContactErrorKind::Ambiguous { matches }) =>
    Some(K::ContactAmbiguous { matches: *matches }),
CoreError::Contact(ContactErrorKind::Other(_)) => None,
```

Remove the `extract_matches_count` helper — it's unused now.

- [ ] **Step 4: Sweep callsites**

```bash
grep -rn 'CoreError::Contact(' crates/core/src/
```

Rewrite patterns:

- `CoreError::Contact("not found".into())` → `CoreError::Contact(ContactErrorKind::NotFound)`
- `CoreError::Contact(format!("ambiguous ({n} matches)"))` → `CoreError::Contact(ContactErrorKind::Ambiguous { matches: n })`
- Everything else → `CoreError::Contact(ContactErrorKind::Other(s))`

- [ ] **Step 5: Tests + clippy**

```bash
cargo test -p skattr-core
cargo clippy -p skattr-core --all-targets -- -D warnings
```

Watch for tests that match on error strings — update them to match the typed variant.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/contact/ crates/core/src/error.rs
git commit -m "contact: ContactErrorKind — structural kind() for contact errors

Phase 1.H item #5 (second subsystem). NotFound/Ambiguous become
typed variants; kind() no longer relies on str::contains.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 11: `InviteErrorKind` + sweep

**Files:**
- Create: `crates/core/src/invite/error_kind.rs`
- Modify: `crates/core/src/invite/mod.rs`
- Modify: `crates/core/src/error.rs`
- Modify: every `CoreError::Invite(...)` construction site

- [ ] **Step 1: Create the enum**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum InviteErrorKind {
    #[error("invite expired")]
    Expired,
    #[error("invite consumed")]
    Consumed,
    #[error("invite signature invalid")]
    SignatureInvalid,
    #[error("invite: {0}")]
    Other(String),
}
```

- [ ] **Step 2: Re-export**

In `crates/core/src/invite/mod.rs`:

```rust
mod error_kind;
pub use error_kind::InviteErrorKind;
```

(InviteErrorKind is `pub` because `invite` is one of the public modules per CLAUDE.md.)

- [ ] **Step 3: Change `CoreError::Invite`**

```rust
#[error("{0}")]
Invite(#[from] crate::invite::InviteErrorKind),
```

- [ ] **Step 4: Update `CoreError::kind()`**

```rust
CoreError::Invite(InviteErrorKind::Expired) => Some(K::InviteExpired),
CoreError::Invite(InviteErrorKind::Consumed) => Some(K::InviteConsumed),
CoreError::Invite(InviteErrorKind::SignatureInvalid) => Some(K::InviteSignatureInvalid),
CoreError::Invite(InviteErrorKind::Other(_)) => None,
```

- [ ] **Step 5: Sweep callsites**

```bash
grep -rn 'CoreError::Invite(' crates/core/src/
```

- "expired" → `InviteErrorKind::Expired`
- "consumed" → `InviteErrorKind::Consumed`
- "signature" → `InviteErrorKind::SignatureInvalid`
- rest → `InviteErrorKind::Other(s)`

- [ ] **Step 6: Tests + clippy + commit**

```bash
cargo test -p skattr-core
cargo clippy -p skattr-core --all-targets -- -D warnings
git add crates/core/src/invite/ crates/core/src/error.rs
git commit -m "invite: InviteErrorKind — structural kind() for invite errors

Phase 1.H item #5 (third subsystem).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 12: `MlsErrorKind` + sweep

**Files:**
- Create: `crates/core/src/mls/error_kind.rs`
- Modify: `crates/core/src/mls/mod.rs`
- Modify: `crates/core/src/error.rs`
- Modify: every `CoreError::Mls(...)` construction site

- [ ] **Step 1: Create the enum**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MlsErrorKind {
    #[error("group corrupt")]
    GroupCorrupt,
    #[error("mls: {0}")]
    Other(String),
}
```

- [ ] **Step 2: Re-export**

In `crates/core/src/mls/mod.rs`:

```rust
mod error_kind;
pub(crate) use error_kind::MlsErrorKind;
```

- [ ] **Step 3: Change `CoreError::Mls` + `kind()` arm**

```rust
#[error("{0}")]
Mls(#[from] crate::mls::MlsErrorKind),
```

```rust
CoreError::Mls(MlsErrorKind::GroupCorrupt) => Some(K::GroupCorrupt),
CoreError::Mls(MlsErrorKind::Other(_)) => None,
```

- [ ] **Step 4: Sweep callsites**

```bash
grep -rn 'CoreError::Mls(' crates/core/src/
```

- "corrupt" → `MlsErrorKind::GroupCorrupt`
- rest → `MlsErrorKind::Other(s)`

- [ ] **Step 5: Tests + commit**

```bash
cargo test -p skattr-core
cargo clippy -p skattr-core --all-targets -- -D warnings
git add crates/core/src/mls/ crates/core/src/error.rs
git commit -m "mls: MlsErrorKind — structural kind() for mls errors

Phase 1.H item #5 (fourth subsystem).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 13: `DeliveryErrorKind` + sweep

**Files:**
- Create: `crates/core/src/delivery/error_kind.rs`
- Modify: `crates/core/src/delivery/mod.rs`
- Modify: `crates/core/src/error.rs`
- Modify: every `CoreError::Delivery(...)` construction site

- [ ] **Step 1: Create the enum**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DeliveryErrorKind {
    #[error("delivery timeout")]
    Timeout,
    #[error("delivery: {0}")]
    Other(String),
}
```

- [ ] **Step 2: Re-export + error variant**

```rust
// delivery/mod.rs
mod error_kind;
pub(crate) use error_kind::DeliveryErrorKind;

// error.rs
#[error("{0}")]
Delivery(#[from] crate::delivery::DeliveryErrorKind),
```

- [ ] **Step 3: `kind()` arm**

```rust
CoreError::Delivery(DeliveryErrorKind::Timeout) => Some(K::DeliveryTimeout),
CoreError::Delivery(DeliveryErrorKind::Other(_)) => None,
```

- [ ] **Step 4: Sweep callsites**

```bash
grep -rn 'CoreError::Delivery(' crates/core/src/
```

- "timeout" → `DeliveryErrorKind::Timeout`
- rest → `DeliveryErrorKind::Other(s)`

- [ ] **Step 5: Tests + commit**

```bash
cargo test -p skattr-core
cargo clippy -p skattr-core --all-targets -- -D warnings
git add crates/core/src/delivery/ crates/core/src/error.rs
git commit -m "delivery: DeliveryErrorKind — structural kind() for delivery errors

Phase 1.H item #5 (fifth subsystem).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 14: `TransportErrorKind` + sweep; retire string matching in `kind()`

**Files:**
- Create: `crates/core/src/transport/error_kind.rs`
- Modify: `crates/core/src/transport/mod.rs`
- Modify: `crates/core/src/error.rs`
- Modify: every `CoreError::Transport(...)` construction site

- [ ] **Step 1: Create the enum**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TransportErrorKind {
    #[error("tor not ready")]
    TorNotReady,
    #[error("transport: {0}")]
    Other(String),
}
```

- [ ] **Step 2: Re-export + error variant**

```rust
// transport/mod.rs
mod error_kind;
pub(crate) use error_kind::TransportErrorKind;

// error.rs
#[error("{0}")]
Transport(#[from] crate::transport::TransportErrorKind),
```

- [ ] **Step 3: `kind()` arm**

```rust
CoreError::Transport(TransportErrorKind::TorNotReady) => Some(K::TorNotReady),
CoreError::Transport(TransportErrorKind::Other(_)) => None,
```

- [ ] **Step 4: Sweep callsites**

```bash
grep -rn 'CoreError::Transport(' crates/core/src/
```

- "not ready" OR "bootstrap" → `TransportErrorKind::TorNotReady`
- rest → `TransportErrorKind::Other(s)`

- [ ] **Step 5: Verify `CoreError::kind()` has zero `str::contains`**

Run:

```bash
grep -n 'contains' crates/core/src/error.rs
```

Expected: NO matches. If any survive, the corresponding subsystem sweep missed a callsite — fix and commit.

- [ ] **Step 6: Add a CI-style grep guard test**

Add to `crates/core/src/error.rs::tests`:

```rust
#[test]
fn kind_has_no_string_matching() {
    // The source of `kind()` must not contain str::contains — that
    // was the review item #5 red flag. Fail the build if it creeps
    // back. We cheat by reading this file at compile time.
    const SRC: &str = include_str!("error.rs");
    // The implementation section starts at `pub fn kind`. Find it and
    // scan only until the next top-level closing brace.
    let impl_start = SRC.find("pub fn kind").expect("kind() in source");
    let tail = &SRC[impl_start..];
    assert!(
        !tail.contains(".contains("),
        "CoreError::kind() must not call str::contains — use typed sub-enums instead"
    );
}
```

- [ ] **Step 7: Tests + commit**

```bash
cargo test -p skattr-core
cargo clippy -p skattr-core --all-targets -- -D warnings
git add crates/core/src/transport/ crates/core/src/error.rs
git commit -m "transport: TransportErrorKind; retire string matching in kind()

Final subsystem in the Phase 1.H item #5 refactor. CoreError::kind()
is now a pure structural match with no str::contains. A test guard
asserts this at build time.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 15: `DaemonErrorKind::InvalidArgument` + dispatch sites + CLI exit code (item #4)

**Files:**
- Modify: `crates/core/src/daemon/error_kind.rs`
- Modify: `crates/core/src/daemon/dispatch.rs::prune_history`
- Modify: `crates/cli/src/main.rs` (or wherever CLI error-exit mapping lives)
- Test: `crates/cli/tests/` (integration)

- [ ] **Step 1: Add the variant**

In `crates/core/src/daemon/error_kind.rs`, add to the `DaemonErrorKind` enum:

```rust
/// Client-supplied arguments failed validation in the daemon.
/// Surfaces as exit code 2 in the CLI, distinct from the internal-
/// error exit code 1.
InvalidArgument { message: String },
```

If `DaemonErrorKind` derives Serialize/Deserialize/etc. (it should — it goes over IPC), nothing else needs adjusting; the wire format adds the new variant.

- [ ] **Step 2: Rewrite `prune_history`'s validation sites**

In `daemon/dispatch.rs`, replace:

```rust
return Err(IpcError::Internal(
    "PruneHistory requires exactly one of before_ts_recv or keep_last".into(),
));
```

with:

```rust
return Err(IpcError::Daemon(DaemonErrorKind::InvalidArgument {
    message: "exactly one of before_ts_recv or keep_last must be Some".into(),
}));
```

And:

```rust
IpcError::Internal("PruneHistory keep_last requires a contact".into())
```

with:

```rust
IpcError::Daemon(DaemonErrorKind::InvalidArgument {
    message: "keep_last requires a contact".into(),
})
```

- [ ] **Step 3: Update CLI exit-code mapping**

In `crates/cli/src/main.rs`, find the error-to-exit-code logic (grep for `process::exit` or `ExitCode`). Add a branch:

```rust
use skattr_core::daemon::error_kind::DaemonErrorKind;
use skattr_core::daemon::ipc::wire::IpcError;

let code = match e.downcast_ref::<IpcError>() {
    Some(IpcError::Daemon(DaemonErrorKind::InvalidArgument { message })) => {
        eprintln!("argument error: {message}");
        2
    }
    Some(IpcError::Daemon(_)) => 1,
    _ => 1,
};
std::process::exit(code);
```

Adjust to match the existing error-handling style — the essence is "InvalidArgument exits with 2; everything else with 1."

- [ ] **Step 4: Update the existing dispatch.rs unit tests**

`prune_history_rejects_both_before_and_keep_last`, `prune_history_rejects_neither`, and `prune_history_keep_last_requires_contact` currently assert `err.is_err()`. Tighten them:

```rust
#[tokio::test]
async fn prune_history_rejects_both_before_and_keep_last() {
    let handle = test_handle();
    let err = execute_command(
        handle,
        Command::PruneHistory {
            contact: None,
            before_ts_recv: Some(1),
            keep_last: Some(2),
        },
    ).await.unwrap_err();
    assert!(matches!(
        err,
        IpcError::Daemon(crate::daemon::error_kind::DaemonErrorKind::InvalidArgument { .. })
    ), "expected InvalidArgument, got {err:?}");
}
```

Apply the same tightening to the `_neither` and `keep_last_requires_contact` tests.

- [ ] **Step 5: Add a CLI exit-code integration test**

In `crates/cli/tests/` (follow the existing pattern — `cli_ipc_roundtrip.rs` etc.), add a test that runs `skattr prune --before-ts 0 --keep-last 3` (both flags) and asserts exit status 2.

If a full process-spawn test is too heavy, a narrower option is to assert in the `main.rs`-level mapping unit by calling the mapping function directly. Either is acceptable.

- [ ] **Step 6: Run tests**

```bash
cargo test -p skattr-core prune_history
cargo test -p skattr-cli
```

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/daemon/error_kind.rs \
        crates/core/src/daemon/dispatch.rs \
        crates/cli/
git commit -m "daemon: InvalidArgument error kind + CLI exit code 2

PruneHistory validation errors (\"exactly one of before/keep_last\",
\"keep_last requires a contact\") now project to
DaemonErrorKind::InvalidArgument instead of IpcError::Internal,
letting the CLI distinguish user-input errors (exit 2) from daemon
bugs (exit 1).

Phase 1.H item #4.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 16: `ContactRepo::contact_for_group` + use it in `search_messages` (item #1)

**Files:**
- Modify: `crates/core/src/storage/contacts.rs`
- Modify: `crates/core/src/daemon/dispatch.rs::search_messages`
- Test: `storage/contacts.rs::tests` + `daemon/dispatch.rs::tests`

- [ ] **Step 1: Write the repo-level failing test**

In `crates/core/src/storage/contacts.rs::tests`:

```rust
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
    }).unwrap();
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
```

- [ ] **Step 2: Run — expect fail**

```bash
cargo test -p skattr-core --lib contact_for_group
```

Expected: FAIL — method not defined.

- [ ] **Step 3: Implement `contact_for_group`**

Add to `ContactRepo` in `crates/core/src/storage/contacts.rs`:

```rust
/// 2-member-group reverse lookup: given a group_id, return the peer's
/// PublicKey (the member that is not us). Returns `Ok(None)` if no
/// contact row has this group_id.
///
/// Phase 1.H: scoped to 2-member groups per CLAUDE.md. Multi-member
/// groups land in Phase 2+; this method will need a signature change
/// at that point to return a list.
pub(crate) fn contact_for_group(&self, group_id: &[u8; 32])
    -> Result<Option<PublicKey>>
{
    self.pool.with(|c| {
        let row: Option<Vec<u8>> = c
            .query_row(
                "SELECT identity FROM contacts WHERE group_id = ?1 LIMIT 1",
                rusqlite::params![&group_id[..]],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(
                format!("contact_for_group: {e}"),
            )))?;
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
```

- [ ] **Step 4: Update `search_messages` in dispatch.rs**

In `crates/core/src/daemon/dispatch.rs::search_messages` (around line 446):

Replace:

```rust
let contact_for_record = contact.unwrap_or(sender_pk);
```

with:

```rust
let contact_for_record = match contact {
    Some(pk) => pk,
    None => {
        // Unscoped search: resolve peer via the hit's group_id so
        // outgoing rows (where sender == local identity) still
        // report the correct peer. 2-member-group scope.
        let gid_arr: [u8; 32] = h.message.group_id[..]
            .try_into()
            .unwrap_or([0u8; 32]);
        ContactRepo::new(&handle.pool)
            .contact_for_group(&gid_arr)
            .ok()
            .flatten()
            .unwrap_or(sender_pk)
    }
};
```

Note: the `unwrap_or(sender_pk)` fallback preserves behavior for rows whose group_id isn't in the contacts table (edge case — shouldn't happen in practice). The `[0u8; 32]` fallback on length mismatch is defensive; in practice `group_id` is always 32 bytes.

If `h.message.group_id` isn't directly exposed (check the `SearchHit` struct in `storage/messages.rs`), you'll need to widen it to include group_id — add it as a public field on the returned struct if it isn't already there.

- [ ] **Step 5: Write the regression test**

In `daemon/dispatch.rs::tests`:

```rust
#[tokio::test]
async fn search_messages_unscoped_resolves_outgoing_contact_via_group() {
    use crate::daemon::commands::Direction;
    let handle = test_handle();
    let my_pubkey = handle.identity.public();
    let peer = crate::identity::PublicKey([0x55; 32]);
    let gid = [0x66u8; 32];

    let cr = ContactRepo::new(&handle.pool);
    cr.upsert(&crate::contact::Contact {
        identity: peer,
        display_name: None,
        added_at: 0,
        card: None,
    }).unwrap();
    cr.set_group_id(&peer, &gid).unwrap();

    // Insert one OUTGOING row: sender == local pubkey.
    let msgs = crate::storage::MessageRepo::new(&handle.pool);
    let env = crate::envelope::Envelope {
        v: 1,
        id: crate::envelope::MessageId::generate(),
        ts: 1_700_000_000,
        reply_to: None,
        kind: crate::envelope::Kind::Text { body: "outbound hello".into() },
    };
    msgs.insert(crate::storage::messages::InsertParams {
        group_id: &gid,
        sender: &my_pubkey.0,
        envelope: &env,
        mls_generation: 1,
        ts_daemon_recv: 1_700_000_000,
    }).unwrap();

    // Unscoped search that matches the outgoing row.
    let result = execute_command(
        handle.clone(),
        Command::SearchMessages {
            query: "hello".into(),
            contact: None,
            limit: 10,
            offset: 0,
            newest_first: false,
        },
    ).await.unwrap();

    match result {
        CommandResult::SearchResults(hits) => {
            assert_eq!(hits.len(), 1);
            // Direction must still be Outgoing (sender is us).
            assert_eq!(hits[0].record.direction, Direction::Outgoing);
            // And contact MUST be the peer, not the local pubkey.
            assert_eq!(
                hits[0].record.contact, peer,
                "unscoped outgoing hit must resolve contact to peer, not self"
            );
        }
        other => panic!("expected SearchResults, got {other:?}"),
    }
}
```

- [ ] **Step 6: Run tests**

```bash
cargo test -p skattr-core --lib 'search_messages_unscoped|contact_for_group'
```

Both PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/storage/contacts.rs crates/core/src/daemon/dispatch.rs \
        crates/core/src/storage/messages.rs
git commit -m "contacts: contact_for_group; fix unscoped-search outgoing contact

Adds ContactRepo::contact_for_group(&[u8; 32]) -> Option<PublicKey>
for 2-member groups. search_messages now uses it to resolve the
peer on unscoped hits, fixing the bug where outgoing rows rendered
contact == local pubkey.

Phase 1.H item #1.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 17: Surface `MessageRecord.row_id` (item #7)

**Files:**
- Modify: `crates/core/src/daemon/commands.rs`
- Modify: `crates/core/src/daemon/dispatch.rs` (every `MessageRecord::project` call)
- Modify: `crates/core/src/daemon/inbound.rs` (event broadcast)
- Test: each dispatch handler's existing test

- [ ] **Step 1: Add `row_id` field to `MessageRecord`**

In `crates/core/src/daemon/commands.rs`, find the `MessageRecord` struct and add:

```rust
pub struct MessageRecord {
    pub row_id: i64,     // <— new: SQLite row id for UI correlation, scroll anchors
    // ... existing fields
}
```

Update `MessageRecord::project` to populate it:

```rust
impl MessageRecord {
    pub(crate) fn project(
        row_id: i64,
        env: &Envelope,
        contact: PublicKey,
        mls_generation: u64,
        ts_daemon_recv: i64,
        direction: Direction,
    ) -> Self {
        Self {
            row_id,
            contact,
            ts_envelope: env.ts,
            ts_daemon_recv: u64::try_from(ts_daemon_recv).unwrap_or(0),
            mls_generation,
            direction,
            reply_to: env.reply_to,
            kind: env.kind.clone(),
        }
    }
}
```

(Retain the existing body — the only change is adding `row_id` to the constructed struct. Match whatever fields `MessageRecord` currently has.)

- [ ] **Step 2: Update inbound.rs's `MessageRecord::project` call**

The existing call at `daemon/inbound.rs:116` already passes `*row_id` as the first argument, so no call-site change is needed there. Double-check: `grep -n 'MessageRecord::project' crates/core/src/daemon/`.

- [ ] **Step 3: Update existing tests that construct MessageRecord**

Search for any test that asserts the full shape of a `MessageRecord`:

```bash
grep -rn 'MessageRecord {' crates/core/src/ crates/tests/
```

Add the `row_id:` field to each literal. For projections, `row_id` is already populated via `::project`.

Also add a positive assertion to one existing test (e.g., `recent_messages_projects_stored_rows` at dispatch.rs:948):

```rust
assert_ne!(records[0].row_id, 0, "row_id must be the SQLite id, not a placeholder");
```

- [ ] **Step 4: Remove the `_row_id` unused-prefix if it still exists**

Grep `crates/core/src/daemon/commands.rs` for `_row_id` — the review item mentions the arg is named `_row_id`. Rename to `row_id` now that it's used.

- [ ] **Step 5: Tests + clippy + commit**

```bash
cargo test -p skattr-core
cargo clippy -p skattr-core --all-targets -- -D warnings
git add crates/core/src/daemon/commands.rs crates/core/src/daemon/dispatch.rs \
        crates/core/src/daemon/inbound.rs
git commit -m "commands: surface row_id on MessageRecord

Phase 1.H item #7. row_id was already passed into project() but
silently dropped. UI layers (Phase 2) use it for scroll anchoring,
mark_read cursor targeting, and trace correlation. No wire-format
break — additive field.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 18: `daemon::clock::now_unix_seconds` hoist (item #6)

**Files:**
- Create: `crates/core/src/daemon/clock.rs`
- Modify: `crates/core/src/daemon/mod.rs` (re-export)
- Modify: `crates/core/src/daemon/inbound.rs` (remove local fn, use helper)
- Modify: `crates/core/src/daemon/dispatch.rs::send_message` (use helper)
- Modify: integration-test copies (3 sites)
- Modify: `crates/core/src/test_exports.rs` (add feature-gated alias)

- [ ] **Step 1: Create the module**

`crates/core/src/daemon/clock.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Clock helpers for the daemon.
//!
//! Hoisted from three integration-test copies + one per-module copy in
//! `daemon/inbound.rs` + one inlined snippet in `daemon/dispatch.rs`.
//! `now_unix_seconds` saturates to 0 on any system-clock failure, so
//! callers never have to propagate a clock error.

use std::time::{SystemTime, UNIX_EPOCH};

/// Unix seconds from the system clock, saturating to 0 on error.
#[must_use]
pub(crate) fn now_unix_seconds() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    )
    .unwrap_or(0)
}
```

- [ ] **Step 2: Wire into `daemon/mod.rs`**

```rust
pub(crate) mod clock;
```

- [ ] **Step 3: Remove duplicates from `daemon/inbound.rs`**

Delete the local `fn now_unix_seconds()` (lines 160–168 in current tree). Add `use crate::daemon::clock::now_unix_seconds;` at the top. All call sites (`let ts_daemon_recv = now_unix_seconds();`) stay unchanged.

- [ ] **Step 4: Replace inlined `SystemTime::now()` in `daemon/dispatch.rs::send_message`**

At the spot where `let now_ms = SystemTime::now()...as_millis() as i64` appears, keep the ms-precision version (we need milliseconds there, not seconds), BUT after the tx block, use `now_unix_seconds()` for the `ts_daemon_recv` binding if it isn't already derived from `now_ms`. Inspecting existing code: `let ts_daemon_recv = now_ms / 1000;` is fine — leave it, but document that `now_unix_seconds()` is available when ms precision isn't needed.

- [ ] **Step 5: Replace the three integration-test copies**

Grep:

```bash
grep -rn 'fn now_unix_seconds' crates/core/tests/ crates/tests/
```

Expect three hits. In each test file, replace the local `fn now_unix_seconds` with:

```rust
use skattr_core::test_exports::now_unix_seconds;
```

- [ ] **Step 6: Expose via `test_exports`**

In `crates/core/src/test_exports.rs`:

```rust
#[cfg(feature = "test-harness")]
pub use crate::daemon::clock::now_unix_seconds;
```

If `test_exports.rs` uses a `pub use` block pattern, slot this alongside the others.

- [ ] **Step 7: Run everything**

```bash
cargo test -p skattr-core
cargo test -p skattr-tests --features skattr-core/test-harness -- --test-threads 1
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

All green.

- [ ] **Step 8: Commit**

```bash
git add crates/core/src/daemon/clock.rs crates/core/src/daemon/mod.rs \
        crates/core/src/daemon/inbound.rs crates/core/src/daemon/dispatch.rs \
        crates/core/src/test_exports.rs crates/core/tests/ crates/tests/
git commit -m "daemon: hoist now_unix_seconds into daemon::clock

Single pub(crate) helper replaces one per-module copy in
daemon::inbound and three integration-test copies. Exposed via
test_exports behind the test-harness feature.

Phase 1.H item #6.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 19: Fixed-width group_id on `ReceiveOutcome::New` (item #9)

**Files:**
- Modify: `crates/core/src/delivery/receiver.rs`
- Modify: `crates/core/src/daemon/inbound.rs` (caller)

- [ ] **Step 1: Tighten the type**

In `crates/core/src/delivery/receiver.rs`, change the enum variant:

```rust
pub enum ReceiveOutcome {
    New {
        envelope: Envelope,
        row_id: i64,
        sender: PublicKey,
        group_id: [u8; 32],   // was Vec<u8>
        mls_generation: u64,
        ts_daemon_recv: u64,
    },
    Duplicate,
    Rejected(String),
}
```

- [ ] **Step 2: Update construction inside `receive_in_tx` / `receive`**

Find the `ReceiveOutcome::New { group_id: group_id.to_vec(), ... }` construction and replace with:

```rust
let gid_arr: [u8; 32] = group_id.try_into()
    .map_err(|_| CoreError::Storage(crate::storage::StorageErrorKind::Other(
        "group_id must be 32 bytes".into(),
    )))?;
// ...
ReceiveOutcome::New {
    envelope,
    row_id,
    sender: *from,
    group_id: gid_arr,
    mls_generation,
    ts_daemon_recv: u64::try_from(ts_daemon_recv).unwrap_or(0),
}
```

`group_id: &[u8]` in the function signature stays — only the outcome's `group_id` tightens.

- [ ] **Step 3: Update the caller in `daemon/inbound.rs`**

The existing pattern:

```rust
ReceiveOutcome::New { envelope, row_id, mls_generation, ts_daemon_recv, .. } => {
```

Still works — the `..` elides `group_id`. No change needed unless a caller destructures `group_id` explicitly. Grep:

```bash
grep -rn 'ReceiveOutcome::New' crates/core/src/
```

For any match that binds `group_id`, change `group_id: Vec<u8>` bindings to `group_id: [u8; 32]`.

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-core
cargo clippy -p skattr-core --all-targets -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/delivery/receiver.rs crates/core/src/daemon/inbound.rs
git commit -m "delivery: ReceiveOutcome::New carries [u8; 32] group_id

Avoids a heap allocation on every inbound frame. Construction now
tries the 32-byte conversion and surfaces a typed storage error on
length mismatch, rather than silently padding.

Phase 1.H item #9.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 20: cargo-deny CI verification (item #10 — already landed)

**Files:** none — CI already runs cargo-deny (workflow job at `.github/workflows/ci.yml:49-57`, landed in commit `6fb715b`). This task is a verification-only pass.

- [ ] **Step 1: Confirm the job is present and clean locally**

```bash
cargo deny check --all-features
```

Expected: PASS.

- [ ] **Step 2: Confirm the CI job is required for merge on `main`/`master`**

This is a GitHub branch-protection setting, not a file change. Check at `https://github.com/<org>/skattr/settings/branches`. If `deny` isn't a required status check, add it. (Ask the user if you don't have repo-admin access — that's a one-click change only the repo owner can make.)

- [ ] **Step 3: Document as complete**

No commit. Just note in the final exit task (Task 22) that item #10 is satisfied with `6fb715b` + CI.

---

## Task 21: `serial_test` for socket-path env tests (item #11)

**Files:**
- Modify: `crates/cli/Cargo.toml`
- Modify: `crates/cli/src/ipc/` (wherever the resolve_socket_path tests live — grep)

- [ ] **Step 1: Locate the Mutex-serialized tests**

Reference commit: `80ce7c7 cli: serialize resolve_socket_path env-mutating tests with a Mutex`. Grep:

```bash
grep -rn 'resolve_socket_path' crates/cli/src/
grep -rn 'Mutex' crates/cli/src/ipc/
```

Typical shape:

```rust
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn resolve_socket_path_honors_xdg_runtime_dir() {
    let _g = ENV_LOCK.lock().unwrap();
    // ... env mutation ...
}
```

- [ ] **Step 2: Add `serial_test` to dev-dependencies**

In `crates/cli/Cargo.toml`:

```toml
[dev-dependencies]
# existing...
serial_test = "3"
```

- [ ] **Step 3: Replace Mutex with `#[serial]`**

For every affected test:

```rust
use serial_test::serial;

#[test]
#[serial(resolve_socket_path_env)]
fn resolve_socket_path_honors_xdg_runtime_dir() {
    // env mutation only — drop the let _g = ENV_LOCK.lock() line
}
```

Use a named group (`resolve_socket_path_env`) so unrelated serial tests in the crate don't force-serialize with these.

Remove the `ENV_LOCK` static entirely.

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-cli --lib ipc
```

All green. Also run twice in rapid succession (or with `--test-threads 4`) to sanity-check the serialization.

- [ ] **Step 5: `cargo deny check` covers the new dep**

```bash
cargo deny check --all-features
```

Expected: PASS. If `serial_test`'s license or advisories trip the config, add to `deny.toml` with a comment.

- [ ] **Step 6: Commit**

```bash
git add crates/cli/Cargo.toml crates/cli/src/ipc/ Cargo.lock
git commit -m "cli: serial_test replaces Mutex on socket-path env tests

Cleaner serialization of env-mutating tests. Uses a named group so
only the resolve_socket_path tests serialize with each other, not
with unrelated tests in the crate.

Phase 1.H item #11.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 22: CHANGELOG + CLAUDE.md + final verification

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add CHANGELOG entry**

At the top of `CHANGELOG.md` (above Phase 1.G's block):

```markdown
## Phase 1.H — Hardening (2026-04-?? → 2026-04-??)

Closes all 11 items surfaced in Phase 1.G review threads. No new
features; no wire-protocol breaks (additive DaemonErrorKind variant
only).

### Correctness

- **#1**: `ContactRepo::contact_for_group` + fix unscoped-search
  outgoing-contact projection in `daemon::dispatch::search_messages`.
- **#2**: Migration 0007 adds `messages.envelope_id` (16-byte BLOB,
  shape trigger, `(group_id, envelope_id)` unique index) + startup
  backfill + `MessageRepo::insert` binds it. Duplicate inserts project
  to `StorageErrorKind::DuplicateMessage` → send path maps to
  `SendStatus::Delivered`.
- **#3**: Send + receive persistence is now transactional. New
  `Group::save_in_tx`, `MessageRepo::insert_in_tx`,
  `OutboxRepo::insert_in_tx`, `delivery::receiver::receive_in_tx`.
  `daemon::dispatch::send_message` and `daemon::inbound::
  dispatch_for_group` run the full group-save + message-insert
  (+ outbox on send) under one `pool.transaction`.

### Error taxonomy

- **#4**: `DaemonErrorKind::InvalidArgument { message }`; prune
  validation no longer returns `IpcError::Internal`; CLI exit code 2
  for InvalidArgument, 1 for everything else.
- **#5**: Subsystem error sub-enums (`ContactErrorKind`,
  `InviteErrorKind`, `MlsErrorKind`, `DeliveryErrorKind`,
  `TransportErrorKind`, `StorageErrorKind`) replace string matching
  in `CoreError::kind()`; build-time grep guard prevents regression.

### IPC / API polish

- **#7**: `MessageRecord.row_id` is now a public field (was silently
  dropped in `project()`).

### Hygiene & infra

- **#6**: `daemon::clock::now_unix_seconds` replaces four duplicates.
- **#8**: `MessageRepo::backfill_body_text` runs its UPDATE loop in
  one transaction.
- **#9**: `ReceiveOutcome::New.group_id: [u8; 32]` (was `Vec<u8>`).
- **#10**: Verified — `cargo-deny` has been a CI job since `6fb715b`.
- **#11**: `serial_test` replaces the socket-path `Mutex`.
```

- [ ] **Step 2: Update `CLAUDE.md` "Repository state" paragraph**

Extend the existing state paragraph with a 1.H sub-paragraph, matching the 1.A–1.G style. Sketch (drop after the 1.G paragraph):

```markdown
Phase 1.H closes the 1.G review thread: migration 0007 adds
`messages.envelope_id` with a `(group_id, envelope_id)` unique
index + startup backfill (`MessageRepo::backfill_envelope_id`); send
+ receive persistence runs under one `pool.transaction` via
`Group::save_in_tx` + `MessageRepo::insert_in_tx` +
`OutboxRepo::insert_in_tx` (and `receive_in_tx` on the inbound
side); `CoreError::kind()` is a pure structural match over
subsystem sub-enums (`StorageErrorKind`, `ContactErrorKind`,
`InviteErrorKind`, `MlsErrorKind`, `DeliveryErrorKind`,
`TransportErrorKind`); `DaemonErrorKind::InvalidArgument` +
CLI exit code 2 for argument errors; `MessageRecord.row_id` surfaces
the SQLite row id; `daemon::clock::now_unix_seconds` replaces four
duplicates; `ReceiveOutcome::New` carries a `[u8; 32]` group_id;
`backfill_body_text` now runs in one transaction;
`serial_test` replaces the socket-path Mutex.
```

- [ ] **Step 3: Run the full verification gauntlet**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
cargo deny check --all-features
```

All green. If any fails, fix and amend the last commit (or add a follow-up commit — prefer the latter for easier review).

- [ ] **Step 4: Count down the items**

Verify each of items 1–11 is closed by pointing at a commit SHA. Use `git log --oneline --grep="Phase 1.H"` — each item should have at least one matching commit.

- [ ] **Step 5: Emit the Phase 1.I kickoff prompt**

Per the user's `feedback_phase_handoff_prompt.md` memory ("emit copy-pasteable brainstorm→spec→plan→execute prompt whenever a phase/sub-project merges"), produce a kickoff for whatever the next phase is (Phase 2 UI is the default next step per the spec). Save to `docs/superpowers/kickoffs/2026-04-??-phase-2-ui-kickoff.md` in the same format as the 1.H kickoff.

- [ ] **Step 6: Final commit**

```bash
git add CHANGELOG.md CLAUDE.md docs/superpowers/kickoffs/
git commit -m "docs: CHANGELOG + CLAUDE.md + Phase 2 kickoff for 1.H close-out

All 11 Phase 1.G review items closed. Repository state summary
extended. Phase 2 UI kickoff prompt prepared.

Phase 1.H ships.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Self-review

**Spec coverage:**

| Spec section | Tasks |
|---|---|
| L1.a — Migration 0007 + envelope_id + unique index | 2, 3, 4 |
| L1.b — Transactional send/receive | 6, 7, 8, 9 |
| L1.c — backfill_body_text in tx (item #8) | 5 |
| L2 — Subsystem error sub-enums + InvalidArgument | 1, 10, 11, 12, 13, 14, 15 |
| L3.a — contact_for_group helper (item #1) | 16 |
| L3.b — MessageRecord.row_id (item #7) | 17 |
| L4.a — daemon::clock::now_unix_seconds (item #6) | 18 |
| L4.b — ReceiveOutcome::New fixed-width (item #9) | 19 |
| L4.c — cargo-deny CI (item #10) | 20 |
| L4.d — serial_test (item #11) | 21 |
| Exit criteria / CHANGELOG / CLAUDE.md | 22 |

All 11 items covered. No spec section without a task.

**Placeholder scan:** no "TBD", "TODO" (outside the explicit temporary marker in Task 1 Step 5), "implement later", or "similar to Task N" in task bodies. The one "similar to" case (Task 9 Step 5's setup boilerplate omitted) points at an existing in-tree test and the exact diff — acceptable.

**Type-consistency sanity check:**

- `StorageErrorKind::{FtsSyntax, DuplicateMessage, Other}` — consistent Task 1 → 4 → 5 → 7.
- `save_in_tx` → takes `&rusqlite::Transaction<'_>`, returns `Result<()>` — Task 6.
- `insert_in_tx` → takes `&Transaction` + `InsertParams`, returns `Result<i64>` — Task 7, used in Task 8.
- `receive_in_tx` → takes `&Transaction` + same args as `receive` — Task 9.
- `contact_for_group(&[u8; 32]) -> Result<Option<PublicKey>>` — Task 16, consumed in same task.
- `DaemonErrorKind::InvalidArgument { message: String }` — Task 15.
- `MessageRecord.row_id: i64` — Task 17, projected via Task 1's `MessageRecord::project` signature (unchanged).

Cross-checks pass.

**Scope check:** 22 tasks, all inside the "close 1.G review thread" bounded scope. Single plan, single worktree, sequential execution. No decomposition needed.

**Known risk:** Task 6's `MlsProvider::save_in_tx` depends on OpenMLS's `StorageProvider` allowing serialize-without-write. Fallback documented inline at Task 6 Step 3 — pivot to insert-before-save if serialize-only isn't feasible.

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-04-24-phase-1h-hardening.md`. Two execution options:

**1. Subagent-Driven (recommended)** — Dispatch a fresh subagent per task, review between tasks, fast iteration. Good fit here because tasks are mostly independent (L2 subsystem sweeps 10–14 in particular) and each one terminates in a commit.

**2. Inline Execution** — Execute tasks in this session using `executing-plans`, batch execution with checkpoints.

Which approach?
