# Phase 2.B Mailbox Client Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the client half of the v1 mailbox protocol — long-lived poller for our mailboxes, on-demand deposit fallback for offline peers, ContactCard rotation publishing, and the AddMailbox / RemoveMailbox / RotateOnion daemon commands — without touching ADR 0006's frozen wire surface.

**Architecture:** A `MailboxClient` over a long-lived `Framed<DataStream, MailboxFrameCodec>` (one per `'mine'` mailbox; on-demand for deposits) drives the protocol. A per-mailbox `PollScheduler` actor runs Challenge→Fetch→Delete on an Idle (60 s) ↔ Active (15 s) cadence with ±25 % jitter. `DeliveryHub` gains a fallback orchestrator: after `direct_timeout_secs` (default 30 s), the orchestrator picks one of the recipient's listed mailboxes via `blake2s(message_id) % len`, deposits, and falls over sequentially to the rest on rejection. ContactCard updates ride MLS app-message channels (new `Kind::ContactCardUpdate`) so rotation reuses the existing direct→mailbox fallback path with no new transport frames. Migration 0008 adds status tracking to `mailboxes` and `target_kind` / `mailbox_id` to `outbox` with a composite unique index.

**Tech Stack:** Rust 2021, `tokio` + `tokio-util` (codec, duplex, `time::pause`), `arti-client` 0.41 (`DataStream`), `ed25519-dalek` (Fetch/Delete signing), `ciborium` (CBOR), `blake2` (deterministic mailbox pick), `rand` 0.8 (jitter), `proptest` (next_interval, idempotency), `rusqlite` 0.38 (migration + repo), `tracing` (redacted logs).

**Spec:** `docs/superpowers/specs/2026-04-30-phase-2b-mailbox-client-design.md`.

---

## Pre-flight

- [ ] **Create the worktree on a fresh branch**

Run from `/home/myggiz/development/skattr`:
```bash
. "$HOME/.cargo/env"
git worktree add -b phase-2b-mailbox-client ../skattr-phase-2b-mailbox-client master
cd ../skattr-phase-2b-mailbox-client
git status --short
git log --oneline -3
```
Expected: clean worktree, HEAD at the spec commit `8a921bc spec: Phase 2.B mailbox client + ContactCard rotation design`. All subsequent commands run from `../skattr-phase-2b-mailbox-client`.

- [ ] **Establish the baseline green build**

```bash
cd /home/myggiz/development/skattr-phase-2b-mailbox-client
. "$HOME/.cargo/env"
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```
Expected: all three green. If any fails, stop and surface — the baseline must be clean before the first behaviour change.

- [ ] **Inventory the stubs we will replace**

```bash
ls crates/core/src/mailbox
ls crates/core/src/storage/migrations
```
Expected files (stubs/migrations as of 2.A merge):
```
crates/core/src/mailbox: client.rs  mod.rs  protocol.rs  scheduler.rs
crates/core/src/storage/migrations: 0001_init.sql 0002_key_packages.sql 0003_contact_cards.sql 0004_outbox_message_id.sql 0005_contact_group_link.sql 0006_history_search.sql 0007_messages_envelope_id.sql
```
The stubs we replace: `core::mailbox::client` (full rewrite), `core::mailbox::scheduler` (rename → `poll`, full rewrite). Untouched stubs: none — `protocol` is frozen and stays as-is.

---

## Task 1: Migration 0008 — `mailboxes` status columns + `outbox` target-kind

**Files:**
- Create: `crates/core/src/storage/migrations/0008_mailbox_status_and_outbox_target_kind.sql`
- Modify: `crates/core/src/storage/migrations.rs` (extend the `MIGRATIONS` slice)
- Test: `crates/core/src/storage/migrations.rs` (`#[cfg(test)] mod tests`)

### Step 1: Write the failing test

Add to `crates/core/src/storage/migrations.rs` `mod tests`:

```rust
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
            names.iter().any(|n| n == "idx_outbox_target_message_kind_mailbox"),
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
```

### Step 2: Run the tests to verify they fail

```bash
cargo test -p skattr-core storage::migrations::tests::migration_0008
```
Expected: 3 test(s) failed (columns/index missing).

### Step 3: Create the migration SQL

Write `crates/core/src/storage/migrations/0008_mailbox_status_and_outbox_target_kind.sql`:

```sql
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz B.V.
--
-- Skattr storage schema, version 8.
-- Phase 2.B mailbox client: status tracking on `mailboxes`, target-kind
-- + mailbox FK on `outbox` for the direct→mailbox fallback path.

INSERT OR IGNORE INTO schema_version (version) VALUES (8);

ALTER TABLE mailboxes ADD COLUMN status TEXT NOT NULL DEFAULT 'unknown'
    CHECK (status IN ('unknown','reachable','unreachable',
                      'rate_limited','pending_removal','removed'));
ALTER TABLE mailboxes ADD COLUMN last_poll_at INTEGER;
ALTER TABLE mailboxes ADD COLUMN last_error_at INTEGER;
ALTER TABLE mailboxes ADD COLUMN last_error_kind TEXT;

ALTER TABLE outbox ADD COLUMN target_kind TEXT NOT NULL DEFAULT 'direct'
    CHECK (target_kind IN ('direct','mailbox'));
ALTER TABLE outbox ADD COLUMN mailbox_id INTEGER NOT NULL DEFAULT 0;

DROP INDEX IF EXISTS idx_outbox_target_message_id;
CREATE UNIQUE INDEX IF NOT EXISTS idx_outbox_target_message_kind_mailbox
    ON outbox(target, message_id, target_kind, mailbox_id);
```

### Step 4: Wire the migration into the runner

Open `crates/core/src/storage/migrations.rs` and append the new `include_str!` entry to the `MIGRATIONS` array. Pattern to follow (mirrors the 0007 entry already in place):

```rust
const MIGRATIONS: &[(&str, &str)] = &[
    // ... existing entries 0001-0007 ...
    (
        "0008_mailbox_status_and_outbox_target_kind",
        include_str!("migrations/0008_mailbox_status_and_outbox_target_kind.sql"),
    ),
];
```

### Step 5: Run the tests to verify they pass

```bash
cargo test -p skattr-core storage::migrations::tests
```
Expected: all migration tests pass (including 0001-0007 unchanged).

### Step 6: Commit

```bash
git add crates/core/src/storage/migrations.rs \
        crates/core/src/storage/migrations/0008_mailbox_status_and_outbox_target_kind.sql
git commit -m "storage: migration 0008 — mailbox status + outbox target_kind"
```

---

## Task 2: `MailboxRepo` — CRUD + status repo

**Files:**
- Create: `crates/core/src/storage/mailboxes.rs`
- Modify: `crates/core/src/storage/mod.rs` (export `MailboxRepo` `pub(crate)`)
- Test: same file (`#[cfg(test)] mod tests`)

### Step 1: Write the failing tests

Create `crates/core/src/storage/mailboxes.rs` with the test stubs first (no impl yet — write the type signatures and let the body be `todo!()`):

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Repository for the `mailboxes` table — both `'mine'` (we deposit
//! into them) and `'theirs'` (peers list them). 2.B owns status
//! transitions; 1.E's add/list helpers stay where they are for now.

use crate::error::{CoreError, Result};
use crate::storage::{Pool, StorageErrorKind};

/// One row from the `mailboxes` table.
#[derive(Debug, Clone)]
pub(crate) struct MailboxRow {
    pub id: i64,
    pub onion: String,
    pub registered_at: i64,
    pub role: String,
    pub status: MailboxStatus,
    pub last_poll_at: Option<i64>,
    pub last_error_at: Option<i64>,
    pub last_error_kind: Option<String>,
}

/// Mirrors the `mailboxes.status` CHECK constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailboxStatus {
    Unknown,
    Reachable,
    Unreachable,
    RateLimited,
    PendingRemoval,
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

pub(crate) struct MailboxRepo<'p> {
    pool: &'p Pool,
}

impl<'p> MailboxRepo<'p> {
    pub fn new(pool: &'p Pool) -> Self { Self { pool } }
    pub fn add_mine(&self, _onion: &str, _now: i64) -> Result<i64> { todo!() }
    pub fn list_mine(&self) -> Result<Vec<MailboxRow>> { todo!() }
    pub fn get(&self, _id: i64) -> Result<Option<MailboxRow>> { todo!() }
    pub fn mark_status(&self, _id: i64, _status: MailboxStatus, _now: i64) -> Result<()> { todo!() }
    pub fn mark_pending_removal(&self, _id: i64) -> Result<()> { todo!() }
    pub fn finalize_removal(&self, _id: i64) -> Result<()> { todo!() }
    pub fn touch_poll(&self, _id: i64, _now: i64) -> Result<()> { todo!() }
    pub fn record_error(&self, _id: i64, _kind: &str, _now: i64) -> Result<()> { todo!() }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        repo.mark_status(id, MailboxStatus::Reachable, 200).unwrap();
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
        assert_eq!(repo.get(id).unwrap().unwrap().status, MailboxStatus::PendingRemoval);
        repo.finalize_removal(id).unwrap();
        assert_eq!(repo.get(id).unwrap().unwrap().status, MailboxStatus::Removed);
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
            ).unwrap();
            Ok(())
        }).unwrap();
        let mine = repo.list_mine().unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].onion, "aaaa.onion");
    }
}
```

Then add `pub(crate) mod mailboxes;` to `crates/core/src/storage/mod.rs`.

### Step 2: Run tests to verify failure

```bash
cargo test -p skattr-core storage::mailboxes::tests
```
Expected: 7 test(s) failed (or panic on `todo!()`).

### Step 3: Implement each method

Replace each `todo!()` body. Reference implementation:

```rust
pub fn add_mine(&self, onion: &str, now: i64) -> Result<i64> {
    self.pool.with_mut(|c| {
        c.execute(
            "INSERT INTO mailboxes(onion, registered_at, role) \
             VALUES (?1, ?2, 'mine') \
             ON CONFLICT(onion, role) DO NOTHING",
            rusqlite::params![onion, now],
        )
        .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("add_mine insert: {e}"))))?;
        let id: i64 = c
            .query_row(
                "SELECT id FROM mailboxes WHERE onion = ?1 AND role = 'mine'",
                rusqlite::params![onion],
                |r| r.get(0),
            )
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("add_mine select: {e}"))))?;
        Ok(id)
    })
}

pub fn list_mine(&self) -> Result<Vec<MailboxRow>> {
    self.pool.with(|c| {
        let mut stmt = c
            .prepare(
                "SELECT id, onion, registered_at, role, status, \
                        last_poll_at, last_error_at, last_error_kind \
                 FROM mailboxes WHERE role = 'mine' \
                 ORDER BY registered_at, id",
            )
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("list_mine prep: {e}"))))?;
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
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("list_mine query: {e}"))))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("list_mine collect: {e}"))))
    })
}

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
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("get mailbox: {e}"))))?;
        Ok(opt)
    })
}

pub fn mark_status(&self, id: i64, status: MailboxStatus, _now: i64) -> Result<()> {
    self.pool.with_mut(|c| {
        c.execute(
            "UPDATE mailboxes SET status = ?1 WHERE id = ?2",
            rusqlite::params![status.as_sql(), id],
        )
        .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("mark_status: {e}"))))?;
        Ok(())
    })
}

pub fn mark_pending_removal(&self, id: i64) -> Result<()> {
    self.mark_status(id, MailboxStatus::PendingRemoval, 0)
}

pub fn finalize_removal(&self, id: i64) -> Result<()> {
    self.mark_status(id, MailboxStatus::Removed, 0)
}

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

pub fn record_error(&self, id: i64, kind: &str, now: i64) -> Result<()> {
    self.pool.with_mut(|c| {
        c.execute(
            "UPDATE mailboxes SET last_error_kind = ?1, last_error_at = ?2 WHERE id = ?3",
            rusqlite::params![kind, now, id],
        )
        .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("record_error: {e}"))))?;
        Ok(())
    })
}
```

### Step 4: Run the tests to verify pass

```bash
cargo test -p skattr-core storage::mailboxes::tests
cargo clippy --all-targets -- -D warnings
```
Expected: 7 passing tests + clippy clean.

### Step 5: Commit

```bash
git add crates/core/src/storage/mailboxes.rs crates/core/src/storage/mod.rs
git commit -m "storage: MailboxRepo with status transitions"
```

---

## Task 3: `OutboxRepo` extension — `target_kind` + `mailbox_id`

**Files:**
- Modify: `crates/core/src/storage/outbox.rs` (add columns to `OutboxRow`, extend insert/list, new `set_mailbox_target` mutator)
- Test: same file

### Step 1: Locate the existing repo + add failing tests

Read `crates/core/src/storage/outbox.rs` first to confirm the struct layout. Add to the test module:

```rust
#[test]
fn insert_defaults_target_kind_to_direct() {
    let pool = Pool::in_memory();
    let repo = OutboxRepo::new(&pool);
    repo.insert_direct(&[1u8; 32], &[0xAB; 16], &[7, 7, 7], 100).unwrap();
    let rows = repo.list_due(200).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].target_kind, OutboxTargetKind::Direct);
    assert_eq!(rows[0].mailbox_id, 0);
}

#[test]
fn set_mailbox_target_flips_kind_and_id() {
    let pool = Pool::in_memory();
    let repo = OutboxRepo::new(&pool);
    repo.insert_direct(&[1u8; 32], &[0xAB; 16], &[7, 7, 7], 100).unwrap();
    let id = repo.list_due(200).unwrap()[0].id;
    repo.set_mailbox_target(id, 42).unwrap();
    let row = repo.get(id).unwrap().unwrap();
    assert_eq!(row.target_kind, OutboxTargetKind::Mailbox);
    assert_eq!(row.mailbox_id, 42);
}

#[test]
fn composite_unique_index_allows_one_per_kind() {
    let pool = Pool::in_memory();
    let repo = OutboxRepo::new(&pool);
    let target = [1u8; 32];
    let msg_id = [0xAB; 16];
    repo.insert_direct(&target, &msg_id, &[7, 7, 7], 100).unwrap();
    repo.insert_for_mailbox(&target, &msg_id, 7, &[7, 7, 7], 100).unwrap();
    let rows = repo.list_due(200).unwrap();
    assert_eq!(rows.len(), 2, "direct + mailbox rows for same (target,msg) coexist");
}

#[test]
fn duplicate_direct_insert_is_idempotent() {
    let pool = Pool::in_memory();
    let repo = OutboxRepo::new(&pool);
    let target = [1u8; 32];
    let msg_id = [0xAB; 16];
    repo.insert_direct(&target, &msg_id, &[7, 7, 7], 100).unwrap();
    let rc = repo.insert_direct(&target, &msg_id, &[7, 7, 7], 100).unwrap();
    assert_eq!(rc, InsertOutcome::AlreadyPresent);
    assert_eq!(repo.list_due(200).unwrap().len(), 1);
}
```

### Step 2: Run tests to verify failure

```bash
cargo test -p skattr-core storage::outbox::tests
```
Expected: failures naming `target_kind`, `OutboxTargetKind`, `set_mailbox_target`, etc.

### Step 3: Extend the type + repo

Add to `outbox.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxTargetKind { Direct, Mailbox }

impl OutboxTargetKind {
    pub(crate) fn as_sql(self) -> &'static str {
        match self { Self::Direct => "direct", Self::Mailbox => "mailbox" }
    }
    pub(crate) fn from_sql(s: &str) -> Self {
        match s { "mailbox" => Self::Mailbox, _ => Self::Direct }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertOutcome { Inserted, AlreadyPresent }
```

Extend `OutboxRow` with `target_kind: OutboxTargetKind` and `mailbox_id: i64` fields; extend every `SELECT`-mapping closure to populate them.

`insert_direct` (rename existing `insert` if needed, keep the in-tx variant) wraps:

```rust
"INSERT INTO outbox(target, message_id, payload, attempts, next_retry_at, target_kind, mailbox_id) \
 VALUES (?1, ?2, ?3, 0, ?4, 'direct', 0) \
 ON CONFLICT(target, message_id, target_kind, mailbox_id) DO NOTHING"
```
Use `c.execute(...)` and check the affected-row count: `Ok(if changes == 1 { Inserted } else { AlreadyPresent })`.

`insert_for_mailbox(&self, target, msg_id, mailbox_id, payload, next_retry_at)` is identical except the SQL hardcodes `'mailbox'` and binds `mailbox_id`.

`set_mailbox_target(&self, row_id, mailbox_id)`:

```rust
c.execute(
    "UPDATE outbox SET target_kind='mailbox', mailbox_id=?1 WHERE id=?2",
    rusqlite::params![mailbox_id, row_id],
)?;
```

`get(&self, id) -> Result<Option<OutboxRow>>` reads a single row by primary key (mirrors `MailboxRepo::get`).

### Step 4: Run tests + clippy

```bash
cargo test -p skattr-core storage::outbox
cargo clippy --all-targets -- -D warnings
```
Expected: green.

### Step 5: Commit

```bash
git add crates/core/src/storage/outbox.rs
git commit -m "storage: outbox target_kind + mailbox_id, composite unique index"
```

---

## Task 4: Move auth helpers to `core::mailbox::auth`

The `payload_digest` / `AUTH_DOMAIN` / `OP_BYTE_*` helpers live in `crates/mailbox/src/auth.rs` today. The 2.B client must compute byte-identical signing input. Refactor: move the public helpers into a new `core::mailbox::auth` module; have the mailbox crate re-import them. No wire change — only file relocation.

**Files:**
- Create: `crates/core/src/mailbox/auth.rs`
- Modify: `crates/core/src/mailbox/mod.rs` (`pub mod auth;`)
- Modify: `crates/mailbox/src/auth.rs` (delete duplicated constants/fn, `use skattr_core::mailbox::auth::{...}`)

### Step 1: Write the failing tests

Add to `crates/core/src/mailbox/auth.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Wire-frozen helpers shared by `core::mailbox::client` and
//! `crates/mailbox`. Moving these here keeps the digest construction
//! a single source of truth — see ADR 0006.

use sha2::{Digest, Sha256};

/// Domain-separation prefix.
pub const AUTH_DOMAIN: &[u8] = b"skattr-mailbox-auth-v1";
/// Operation byte for FETCH (matches `MailboxFrameKind::Fetch`).
pub const OP_BYTE_FETCH: u8 = 0x86;
/// Operation byte for DELETE.
pub const OP_BYTE_DELETE: u8 = 0x88;

/// `sha256(canonical_cbor(payload))`.
pub fn payload_digest<T: serde::Serialize>(payload: &T) -> Result<[u8; 32], String> {
    let mut buf = Vec::new();
    ciborium::into_writer(payload, &mut buf).map_err(|e| format!("auth digest: {e}"))?;
    Ok(Sha256::digest(&buf).into())
}

/// Build the full auth-string input bytes:
/// `AUTH_DOMAIN || nonce || op_byte || payload_digest`.
#[must_use]
pub fn signing_input(nonce: &[u8; 32], op_byte: u8, payload_digest: &[u8; 32]) -> Vec<u8> {
    let mut input = Vec::with_capacity(AUTH_DOMAIN.len() + 32 + 1 + 32);
    input.extend_from_slice(AUTH_DOMAIN);
    input.extend_from_slice(nonce);
    input.push(op_byte);
    input.extend_from_slice(payload_digest);
    input
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn signing_input_layout_is_stable() {
        let nonce = [0x11u8; 32];
        let digest = [0x22u8; 32];
        let out = signing_input(&nonce, OP_BYTE_FETCH, &digest);
        assert!(out.starts_with(AUTH_DOMAIN));
        assert_eq!(&out[AUTH_DOMAIN.len()..AUTH_DOMAIN.len() + 32], &nonce);
        assert_eq!(out[AUTH_DOMAIN.len() + 32], OP_BYTE_FETCH);
        assert_eq!(&out[AUTH_DOMAIN.len() + 32 + 1..], &digest);
    }

    #[test]
    fn payload_digest_round_trips() {
        let v = (1u16, [9u8; 32], [0xAAu8; 32]);
        let d = payload_digest(&v).unwrap();
        assert_eq!(d.len(), 32);
    }
}
```

Add `pub mod auth;` to `crates/core/src/mailbox/mod.rs`.

### Step 2: Run the new tests

```bash
cargo test -p skattr-core mailbox::auth
```
Expected: pass.

### Step 3: Refactor mailbox crate to import

In `crates/mailbox/src/auth.rs`:
- Delete the `AUTH_DOMAIN`, `OP_BYTE_FETCH`, `OP_BYTE_DELETE`, and `payload_digest` definitions.
- Replace usages with `use skattr_core::mailbox::auth::{AUTH_DOMAIN, OP_BYTE_FETCH, OP_BYTE_DELETE, payload_digest};`.
- The mailbox-side `payload_digest` returned `Result<[u8; 32], MailboxError>`; the moved helper returns `Result<[u8; 32], String>`. Wrap at the call site:
  ```rust
  let digest = skattr_core::mailbox::auth::payload_digest(&(...))
      .map_err(|e| MailboxError::Transport(crate::error::TransportErrorKind::EncodeFailed(e)))?;
  ```
- Update every `dispatch.rs` call site that constructed digests via `crate::auth::payload_digest`.

### Step 4: Run the full mailbox test suite

```bash
cargo test -p skattr-mailbox
cargo clippy --all-targets -- -D warnings
```
Expected: every existing test still passes (auth refactor is zero-behaviour).

### Step 5: Commit

```bash
git add crates/core/src/mailbox/auth.rs crates/core/src/mailbox/mod.rs \
        crates/mailbox/src/auth.rs crates/mailbox/src/dispatch.rs
git commit -m "mailbox: hoist auth digest helpers into core::mailbox::auth"
```

---

## Task 5: `MailboxFrameCodec` in core

Mirror the mailbox crate's codec (same wire layout) so the client can frame requests / parse responses without depending on `crates/mailbox`. Identical CBOR encoding, identical type bytes.

**Files:**
- Create: `crates/core/src/mailbox/codec.rs`
- Modify: `crates/core/src/mailbox/mod.rs` (`pub(crate) mod codec;`)

### Step 1: Failing tests

Create `codec.rs` with the same `MailboxFrame` enum and `MailboxFrameCodec` as `crates/mailbox/src/codec.rs` (copy verbatim, change visibility to `pub(crate)`, and re-license header to GPL-3.0). Tests are also a verbatim copy.

```bash
cargo test -p skattr-core mailbox::codec
```
Expected: tests fail to compile (module not yet wired) → after wiring, they pass on first run because the implementation is a copy of a known-good codec.

### Step 2: Cross-codec property test

Add a fresh test that round-trips through *both* codecs to prove byte-identical wire output:

```rust
#[test]
fn deposit_bytes_match_server_codec() {
    use skattr_core::mailbox::protocol::Deposit;
    let f_core = crate::mailbox::codec::MailboxFrame::Deposit(Deposit {
        version: 1, recipient_hash: [0xAA; 32],
        ciphertext: vec![1, 2, 3], ttl_request: 86_400,
    });
    let mut buf_core = bytes::BytesMut::new();
    crate::mailbox::codec::MailboxFrameCodec::new()
        .encode(f_core, &mut buf_core).unwrap();
    // Decoded by an *independent* server-side decoder must match.
    // (this assertion lives as a `crates/tests` integration test
    // because we can't depend on `skattr-mailbox` from `core`.)
}
```
*(Move the byte-level cross-validation into `crates/tests/src/mailbox_codec_parity.rs` as Task 6 — see below.)*

### Step 3: Commit

```bash
git add crates/core/src/mailbox/codec.rs crates/core/src/mailbox/mod.rs
git commit -m "mailbox: client-side MailboxFrameCodec mirroring server"
```

---

## Task 6: Cross-codec parity integration test

**Files:**
- Create: `crates/tests/src/mailbox_codec_parity.rs`

Write an integration test that depends on both crates:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

use bytes::BytesMut;
use skattr_core::mailbox::codec::{MailboxFrame as ClientFrame, MailboxFrameCodec as ClientCodec};
use skattr_core::mailbox::protocol::*;
use skattr_mailbox::codec::{MailboxFrame as ServerFrame, MailboxFrameCodec as ServerCodec};
use tokio_util::codec::{Decoder, Encoder};

#[test]
fn client_encoded_deposit_decodes_on_server() {
    let body = Deposit {
        version: PROTOCOL_VERSION,
        recipient_hash: [0xAA; 32],
        ciphertext: vec![1, 2, 3, 4],
        ttl_request: 86_400,
    };
    let mut buf = BytesMut::new();
    ClientCodec::new()
        .encode(ClientFrame::Deposit(body.clone()), &mut buf)
        .unwrap();
    let decoded = ServerCodec::new().decode(&mut buf).unwrap().unwrap();
    let ServerFrame::Deposit(got) = decoded else { panic!("wrong frame kind") };
    assert_eq!(got, body);
}

// Symmetric tests for Challenge, Fetch, Delete (C→S) and DepositOk,
// ChallengeNonce, FetchResponse, DeleteOk, Error (S→C).
```

Add the file to `crates/tests/src/lib.rs`. Run:

```bash
cargo test -p skattr-tests mailbox_codec_parity
git add crates/tests/src/mailbox_codec_parity.rs crates/tests/src/lib.rs
git commit -m "tests: cross-codec parity for v1 mailbox frames"
```

---

## Task 7: `MailboxClientErrorKind` + `CoreError::kind()` extension

**Files:**
- Modify: `crates/core/src/error.rs` (add `MailboxClient` variant + `MailboxClientErrorKind`)
- Modify: `crates/core/tests/error_kind_no_string_match.rs` (extend the build-time guard)

### Step 1: Failing test

Append to `crates/core/tests/error_kind_no_string_match.rs` (or wherever the 1.H guard lives):

```rust
#[test]
fn mailbox_client_kind_round_trips() {
    use skattr_core::error::{CoreError, MailboxClientErrorKind};
    let e = CoreError::MailboxClient(MailboxClientErrorKind::RateLimited);
    assert!(matches!(e.kind(), CoreError::MailboxClient(_)));
}
```

Run it; expect "no variant `MailboxClient`".

### Step 2: Extend the enum

In `crates/core/src/error.rs`:

```rust
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum MailboxClientErrorKind {
    #[error("unreachable")]                Unreachable,
    #[error("unsupported version")]        UnsupportedVersion,
    #[error("rate limited")]               RateLimited,
    #[error("recipient full")]             RecipientFull,
    #[error("invalid signature")]          InvalidSignature,
    #[error("nonce expired")]              NonceExpired,
    #[error("malformed response")]         Malformed,
    #[error("hash mismatch")]              HashMismatch,
    #[error("{0}")]                        Other(String),
}
```

Add to the `CoreError` enum:

```rust
#[error("mailbox client: {0}")]
MailboxClient(#[from] MailboxClientErrorKind),
```

Extend `CoreError::kind()` with one match arm: `Self::MailboxClient(k) => Self::MailboxClient(k.clone()),`.

### Step 3: Run

```bash
cargo test -p skattr-core
cargo clippy --all-targets -- -D warnings
git add crates/core/src/error.rs crates/core/tests/error_kind_no_string_match.rs
git commit -m "error: MailboxClientErrorKind + CoreError::kind() extension"
```

---

## Task 8: `MailboxClient::connect` + `probe`

The simplest two methods first — establish the long-lived `Framed` shape and the AddMailbox liveness probe.

**Files:**
- Modify: `crates/core/src/mailbox/client.rs` (full rewrite)
- Test: same file (in-process duplex against an inline test server)

### Step 1: Failing test

Replace the contents of `client.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Client side of the v1 mailbox protocol.

use futures::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::Framed;

use crate::error::{CoreError, MailboxClientErrorKind, Result};
use crate::mailbox::codec::{MailboxFrame, MailboxFrameCodec};
use crate::mailbox::protocol::{Challenge, ChallengeNonce, ErrorBody, ErrorCode, PROTOCOL_VERSION};

/// Single-mailbox client over a long-lived framed stream.
pub(crate) struct MailboxClient<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    onion: String,
    framed: Framed<S, MailboxFrameCodec>,
}

impl<S> MailboxClient<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    /// Wrap an already-connected stream. Production callers go through
    /// [`MailboxClient::connect`] which owns the Arti dial. Tests pass a
    /// `tokio::io::DuplexStream` directly.
    pub fn from_stream(onion: String, stream: S) -> Self {
        Self { onion, framed: Framed::new(stream, MailboxFrameCodec::new()) }
    }

    /// Onion this client is bound to.
    #[must_use]
    pub fn onion(&self) -> &str { &self.onion }

    /// Single Challenge round-trip — used by AddMailbox liveness check.
    pub async fn probe(&mut self, identity_hash: [u8; 32]) -> Result<()> {
        self.framed
            .send(MailboxFrame::Challenge(Challenge { version: PROTOCOL_VERSION, identity_hash }))
            .await
            .map_err(|_| CoreError::MailboxClient(MailboxClientErrorKind::Unreachable))?;
        match self.framed.next().await {
            Some(Ok(MailboxFrame::ChallengeNonce(_))) => Ok(()),
            Some(Ok(MailboxFrame::Error(ErrorBody { code, .. }))) => Err(CoreError::MailboxClient(map_error(code))),
            Some(Ok(_)) => Err(CoreError::MailboxClient(MailboxClientErrorKind::Malformed)),
            Some(Err(_)) | None => Err(CoreError::MailboxClient(MailboxClientErrorKind::Unreachable)),
        }
    }
}

fn map_error(code: ErrorCode) -> MailboxClientErrorKind {
    use MailboxClientErrorKind as E;
    match code {
        ErrorCode::UnsupportedVersion => E::UnsupportedVersion,
        ErrorCode::RateLimited => E::RateLimited,
        ErrorCode::RecipientFull => E::RecipientFull,
        ErrorCode::InvalidSignature => E::InvalidSignature,
        ErrorCode::NonceExpired => E::NonceExpired,
        ErrorCode::HashMismatch => E::HashMismatch,
        ErrorCode::MalformedRequest => E::Malformed,
        ErrorCode::TooLarge | ErrorCode::TtlTooLong | ErrorCode::TtlTooShort
        | ErrorCode::NotFound | ErrorCode::Internal => E::Other(format!("server error: {code:?}")),
    }
}

/// Helper used in tests + by [`crate::mailbox::poll`].
pub(crate) fn recipient_hash_from_pubkey(pk: &[u8; 32]) -> [u8; 32] {
    Sha256::digest(pk).into()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::mailbox::codec::{MailboxFrame, MailboxFrameCodec};
    use crate::mailbox::protocol::ChallengeNonce;
    use futures::SinkExt;
    use tokio::io::duplex;
    use tokio_util::codec::Framed;

    /// Spawn a tiny inline server on the duplex peer that responds to
    /// one Challenge with a fixed nonce.
    async fn inline_challenge_server(server: tokio::io::DuplexStream) {
        let mut framed = Framed::new(server, MailboxFrameCodec::new());
        if let Some(Ok(MailboxFrame::Challenge(_))) = framed.next().await {
            framed
                .send(MailboxFrame::ChallengeNonce(ChallengeNonce { nonce: [0x55; 32], issued_at: 1 }))
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn probe_succeeds_on_challenge_nonce() {
        let (a, b) = duplex(64 * 1024);
        let server = tokio::spawn(inline_challenge_server(b));
        let mut client = MailboxClient::from_stream("aaaa.onion".into(), a);
        client.probe([0xCD; 32]).await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn probe_returns_rate_limited_on_error() {
        let (a, b) = duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let mut framed = Framed::new(b, MailboxFrameCodec::new());
            let _ = framed.next().await;
            framed.send(MailboxFrame::Error(ErrorBody {
                code: ErrorCode::RateLimited, message: "slow down".into(),
            })).await.unwrap();
        });
        let mut client = MailboxClient::from_stream("a.onion".into(), a);
        let err = client.probe([0; 32]).await.unwrap_err();
        assert!(matches!(err, CoreError::MailboxClient(MailboxClientErrorKind::RateLimited)));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn probe_unreachable_on_eof() {
        let (a, b) = duplex(64);
        drop(b);
        let mut client = MailboxClient::from_stream("a.onion".into(), a);
        let err = client.probe([0; 32]).await.unwrap_err();
        assert!(matches!(err, CoreError::MailboxClient(MailboxClientErrorKind::Unreachable)));
    }
}
```

### Step 2: Run

```bash
cargo test -p skattr-core mailbox::client
cargo clippy --all-targets -- -D warnings
git add crates/core/src/mailbox/client.rs
git commit -m "mailbox: MailboxClient::probe over long-lived Framed"
```

---

## Task 9: `MailboxClient::deposit`

**Files:** `crates/core/src/mailbox/client.rs` (extend).

### Step 1: Failing test

Add to `mod tests`:

```rust
#[tokio::test]
async fn deposit_returns_deposit_id_on_ok() {
    use crate::mailbox::protocol::{Deposit, DepositOk};
    let (a, b) = duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let mut framed = Framed::new(b, MailboxFrameCodec::new());
        let req = framed.next().await.unwrap().unwrap();
        let MailboxFrame::Deposit(d) = req else { panic!("expected Deposit") };
        assert_eq!(d.recipient_hash, [0xAA; 32]);
        framed.send(MailboxFrame::DepositOk(DepositOk {
            deposit_id: [0x42; 16], expires_at: 999,
        })).await.unwrap();
    });
    let mut client = MailboxClient::from_stream("a.onion".into(), a);
    let ok = client.deposit([0xAA; 32], vec![1, 2, 3], 86_400).await.unwrap();
    assert_eq!(ok.deposit_id, [0x42; 16]);
    server.await.unwrap();
}

#[tokio::test]
async fn deposit_recipient_full_maps_kind() {
    let (a, b) = duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let mut framed = Framed::new(b, MailboxFrameCodec::new());
        let _ = framed.next().await;
        framed.send(MailboxFrame::Error(ErrorBody {
            code: ErrorCode::RecipientFull, message: "full".into(),
        })).await.unwrap();
    });
    let mut client = MailboxClient::from_stream("a.onion".into(), a);
    let err = client.deposit([0; 32], vec![1], 1).await.unwrap_err();
    assert!(matches!(err, CoreError::MailboxClient(MailboxClientErrorKind::RecipientFull)));
    server.await.unwrap();
}
```

### Step 2: Implement

Add to `impl MailboxClient`:

```rust
pub async fn deposit(
    &mut self,
    recipient_hash: [u8; 32],
    ciphertext: Vec<u8>,
    ttl_request: u32,
) -> Result<crate::mailbox::protocol::DepositOk> {
    use crate::mailbox::protocol::Deposit;
    self.framed
        .send(MailboxFrame::Deposit(Deposit {
            version: PROTOCOL_VERSION, recipient_hash, ciphertext, ttl_request,
        }))
        .await
        .map_err(|_| CoreError::MailboxClient(MailboxClientErrorKind::Unreachable))?;
    match self.framed.next().await {
        Some(Ok(MailboxFrame::DepositOk(ok))) => Ok(ok),
        Some(Ok(MailboxFrame::Error(ErrorBody { code, .. }))) => {
            Err(CoreError::MailboxClient(map_error(code)))
        }
        Some(Ok(_)) => Err(CoreError::MailboxClient(MailboxClientErrorKind::Malformed)),
        Some(Err(_)) | None => Err(CoreError::MailboxClient(MailboxClientErrorKind::Unreachable)),
    }
}
```

### Step 3: Verify + commit

```bash
cargo test -p skattr-core mailbox::client
git add crates/core/src/mailbox/client.rs
git commit -m "mailbox: MailboxClient::deposit"
```

---

## Task 10: `MailboxClient::fetch`

**Files:** `crates/core/src/mailbox/client.rs` (extend).

### Step 1: Failing test

```rust
#[tokio::test]
async fn fetch_signs_with_identity_and_returns_deposits() {
    use crate::identity::IdentityKey;
    use crate::mailbox::protocol::{
        ChallengeNonce, Fetch, FetchResponse, PendingDeposit,
    };

    let signer = IdentityKey::generate().unwrap();
    let pk: [u8; 32] = signer.public().0;

    let (a, b) = duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let mut framed = Framed::new(b, MailboxFrameCodec::new());
        // 1. Challenge
        let MailboxFrame::Challenge(_) = framed.next().await.unwrap().unwrap() else { panic!() };
        framed.send(MailboxFrame::ChallengeNonce(ChallengeNonce {
            nonce: [0x77; 32], issued_at: 1,
        })).await.unwrap();
        // 2. Fetch — verify signature
        let MailboxFrame::Fetch(f) = framed.next().await.unwrap().unwrap() else { panic!() };
        let digest = skattr_core::mailbox::auth::payload_digest(
            &(f.version, f.identity_pubkey, f.nonce),
        ).unwrap();
        let input = skattr_core::mailbox::auth::signing_input(
            &f.nonce, skattr_core::mailbox::auth::OP_BYTE_FETCH, &digest,
        );
        use ed25519_dalek::{Verifier, VerifyingKey, Signature};
        let vk = VerifyingKey::from_bytes(&f.identity_pubkey).unwrap();
        vk.verify(&input, &Signature::from_bytes(&f.signature)).unwrap();
        framed.send(MailboxFrame::FetchResponse(FetchResponse {
            deposits: vec![PendingDeposit {
                deposit_id: [0xEE; 16],
                ciphertext: vec![9, 9, 9],
                received_at: 1,
            }],
        })).await.unwrap();
    });

    let mut client = MailboxClient::from_stream("a.onion".into(), a);
    let resp = client.fetch(&signer).await.unwrap();
    assert_eq!(resp.deposits.len(), 1);
    server.await.unwrap();
}
```

### Step 2: Implement

```rust
pub async fn fetch(
    &mut self,
    identity: &crate::identity::IdentityKey,
) -> Result<crate::mailbox::protocol::FetchResponse> {
    use crate::mailbox::auth::{payload_digest, signing_input, OP_BYTE_FETCH};
    use crate::mailbox::protocol::{Fetch, FetchResponse};

    let pk: [u8; 32] = identity.public().0;
    let id_hash = recipient_hash_from_pubkey(&pk);
    let nonce = self.challenge(id_hash).await?;

    let digest = payload_digest(&(PROTOCOL_VERSION, pk, nonce))
        .map_err(|_| CoreError::MailboxClient(MailboxClientErrorKind::Other("digest encode".into())))?;
    let input = signing_input(&nonce, OP_BYTE_FETCH, &digest);
    let sig = identity.sign(&input).0;

    self.framed
        .send(MailboxFrame::Fetch(Fetch {
            version: PROTOCOL_VERSION, identity_pubkey: pk, nonce, signature: sig,
        }))
        .await
        .map_err(|_| CoreError::MailboxClient(MailboxClientErrorKind::Unreachable))?;
    match self.framed.next().await {
        Some(Ok(MailboxFrame::FetchResponse(r))) => Ok(r),
        Some(Ok(MailboxFrame::Error(ErrorBody { code, .. }))) => {
            Err(CoreError::MailboxClient(map_error(code)))
        }
        Some(Ok(_)) => Err(CoreError::MailboxClient(MailboxClientErrorKind::Malformed)),
        Some(Err(_)) | None => Err(CoreError::MailboxClient(MailboxClientErrorKind::Unreachable)),
    }
}

async fn challenge(&mut self, identity_hash: [u8; 32]) -> Result<[u8; 32]> {
    self.framed
        .send(MailboxFrame::Challenge(Challenge { version: PROTOCOL_VERSION, identity_hash }))
        .await
        .map_err(|_| CoreError::MailboxClient(MailboxClientErrorKind::Unreachable))?;
    match self.framed.next().await {
        Some(Ok(MailboxFrame::ChallengeNonce(c))) => Ok(c.nonce),
        Some(Ok(MailboxFrame::Error(ErrorBody { code, .. }))) => {
            Err(CoreError::MailboxClient(map_error(code)))
        }
        Some(Ok(_)) => Err(CoreError::MailboxClient(MailboxClientErrorKind::Malformed)),
        Some(Err(_)) | None => Err(CoreError::MailboxClient(MailboxClientErrorKind::Unreachable)),
    }
}
```

### Step 3: Verify + commit

```bash
cargo test -p skattr-core mailbox::client
git add crates/core/src/mailbox/client.rs
git commit -m "mailbox: MailboxClient::fetch with Challenge round-trip"
```

---

## Task 11: `MailboxClient::delete`

**Files:** `crates/core/src/mailbox/client.rs` (extend).

### Step 1: Failing test

```rust
#[tokio::test]
async fn delete_signs_with_deposit_ids_in_tuple() {
    use crate::identity::IdentityKey;
    use crate::mailbox::protocol::{Delete, DeleteOk, ChallengeNonce};
    let signer = IdentityKey::generate().unwrap();
    let (a, b) = duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let mut framed = Framed::new(b, MailboxFrameCodec::new());
        let _ = framed.next().await;
        framed.send(MailboxFrame::ChallengeNonce(ChallengeNonce {
            nonce: [0x88; 32], issued_at: 1,
        })).await.unwrap();
        let MailboxFrame::Delete(d) = framed.next().await.unwrap().unwrap() else { panic!() };
        let digest = skattr_core::mailbox::auth::payload_digest(
            &(d.version, d.identity_pubkey, d.nonce, d.deposit_ids.as_slice()),
        ).unwrap();
        let input = skattr_core::mailbox::auth::signing_input(
            &d.nonce, skattr_core::mailbox::auth::OP_BYTE_DELETE, &digest,
        );
        use ed25519_dalek::{Verifier, VerifyingKey, Signature};
        VerifyingKey::from_bytes(&d.identity_pubkey).unwrap()
            .verify(&input, &Signature::from_bytes(&d.signature)).unwrap();
        framed.send(MailboxFrame::DeleteOk(DeleteOk { deleted: 2, not_found: 0 }))
            .await.unwrap();
    });

    let mut client = MailboxClient::from_stream("a.onion".into(), a);
    let ok = client.delete(&signer, vec![[1; 16], [2; 16]]).await.unwrap();
    assert_eq!(ok.deleted, 2);
    server.await.unwrap();
}
```

### Step 2: Implement

```rust
pub async fn delete(
    &mut self,
    identity: &crate::identity::IdentityKey,
    deposit_ids: Vec<[u8; 16]>,
) -> Result<crate::mailbox::protocol::DeleteOk> {
    use crate::mailbox::auth::{payload_digest, signing_input, OP_BYTE_DELETE};
    use crate::mailbox::protocol::{Delete, DeleteOk};

    let pk: [u8; 32] = identity.public().0;
    let id_hash = recipient_hash_from_pubkey(&pk);
    let nonce = self.challenge(id_hash).await?;
    let digest = payload_digest(&(PROTOCOL_VERSION, pk, nonce, deposit_ids.as_slice()))
        .map_err(|_| CoreError::MailboxClient(MailboxClientErrorKind::Other("digest encode".into())))?;
    let input = signing_input(&nonce, OP_BYTE_DELETE, &digest);
    let sig = identity.sign(&input).0;

    self.framed
        .send(MailboxFrame::Delete(Delete {
            version: PROTOCOL_VERSION, identity_pubkey: pk, nonce, signature: sig, deposit_ids,
        }))
        .await
        .map_err(|_| CoreError::MailboxClient(MailboxClientErrorKind::Unreachable))?;
    match self.framed.next().await {
        Some(Ok(MailboxFrame::DeleteOk(ok))) => Ok(ok),
        Some(Ok(MailboxFrame::Error(ErrorBody { code, .. }))) => {
            Err(CoreError::MailboxClient(map_error(code)))
        }
        Some(Ok(_)) => Err(CoreError::MailboxClient(MailboxClientErrorKind::Malformed)),
        Some(Err(_)) | None => Err(CoreError::MailboxClient(MailboxClientErrorKind::Unreachable)),
    }
}
```

### Step 3: Verify + commit

```bash
cargo test -p skattr-core mailbox::client
git add crates/core/src/mailbox/client.rs
git commit -m "mailbox: MailboxClient::delete"
```

---

## Task 12: `MailboxClient::connect` over Arti

**Files:** `crates/core/src/mailbox/client.rs` (add `connect` constructor + tor-feature gating).

### Step 1: Failing test (`#[ignore]`-gated, real-Tor only)

Add to `mod tests`:

```rust
#[tokio::test]
#[ignore = "requires real Arti circuit"]
async fn connect_real_tor_round_trip() {
    // Run only with: cargo test -p skattr-core mailbox::client -- --ignored
    // Drives a localhost test mailbox spawn — see crates/tests/src/mailbox_client_real_tor.rs
    // for the harness; this here is just a smoke that the API compiles.
}
```

(The real `#[ignore]` test sits in `crates/tests/`; the in-module test is just a compile assertion.)

### Step 2: Implement `connect`

Mirror the existing `core::transport::tor::connect_onion` pattern (cf. 0.C):

```rust
impl MailboxClient<arti_client::DataStream> {
    /// Open a Tor circuit to `onion`, port 1, and wrap it in a framed
    /// codec.
    pub async fn connect(
        onion: &str,
        tor: &arti_client::TorClient<tor_rtcompat::PreferredRuntime>,
    ) -> Result<Self> {
        let target = format!("{onion}:1");
        let stream = tor
            .connect(&target as &str)
            .await
            .map_err(|_| CoreError::MailboxClient(MailboxClientErrorKind::Unreachable))?;
        Ok(Self::from_stream(onion.to_string(), stream))
    }
}
```

(Adjust the `TorClient` generic parameter to whatever 0.C exposes; if there is a `core::transport::tor::SharedTorClient` newtype, accept that instead.)

### Step 3: Build + commit

```bash
cargo check -p skattr-core --all-features
git add crates/core/src/mailbox/client.rs
git commit -m "mailbox: MailboxClient::connect over Arti circuit"
```

---

## Task 13: `next_interval` pure function + property test

**Files:**
- Modify: `crates/core/src/mailbox/scheduler.rs` → rename to `poll.rs`, full rewrite

### Step 1: Failing tests

Replace the contents of `scheduler.rs` (and rename to `poll.rs`; update `mod.rs`):

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Adaptive polling scheduler for our mailboxes.
//!
//! Per-mailbox actor with an Idle (60 s) ↔ Active (15 s) state machine.
//! ±25 % jitter per tick to break timing correlation across mailboxes.
//! Idle ceiling = 5 min (used when a mailbox is `Unreachable`).

use std::time::Duration;
use rand::Rng;

pub(crate) const ACTIVE_BASE: Duration = Duration::from_secs(15);
pub(crate) const IDLE_BASE: Duration = Duration::from_secs(60);
pub(crate) const IDLE_CEILING: Duration = Duration::from_secs(5 * 60);
pub(crate) const ACTIVE_HOLD: Duration = Duration::from_secs(5 * 60);

#[must_use]
pub(crate) fn next_interval(active: bool, unreachable: bool, rng: &mut impl Rng) -> Duration {
    let base = match (active, unreachable) {
        (_, true) => IDLE_CEILING,
        (true, false) => ACTIVE_BASE,
        (false, false) => IDLE_BASE,
    };
    let nanos = base.as_nanos() as i128;
    let jitter_range: i128 = nanos / 4;            // ±25 %
    let delta = rng.gen_range(-jitter_range..=jitter_range);
    let out = (nanos + delta).max(0) as u64;
    Duration::from_nanos(out)
}
```

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rand::{rngs::StdRng, SeedableRng};

    #[test]
    fn active_interval_within_active_band() {
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..1000 {
            let d = next_interval(true, false, &mut rng);
            assert!(d >= Duration::from_millis(11_250) && d <= Duration::from_millis(18_750),
                "active out of band: {d:?}");
        }
    }

    #[test]
    fn idle_interval_within_idle_band() {
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..1000 {
            let d = next_interval(false, false, &mut rng);
            assert!(d >= Duration::from_millis(45_000) && d <= Duration::from_millis(75_000),
                "idle out of band: {d:?}");
        }
    }

    #[test]
    fn unreachable_interval_locks_to_idle_ceiling() {
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..100 {
            let d = next_interval(false, true, &mut rng);
            assert!(d >= Duration::from_millis(225_000) && d <= Duration::from_millis(375_000));
        }
    }

    proptest::proptest! {
        #[test]
        fn never_exceeds_max(seed: u64, active: bool, unreach: bool) {
            let mut rng = StdRng::seed_from_u64(seed);
            let d = next_interval(active, unreach, &mut rng);
            assert!(d <= Duration::from_millis(375_000));
        }
    }
}
```

Add `proptest` to the dev-dependencies of `crates/core/Cargo.toml` if not already present.

### Step 2: Run + commit

```bash
cargo test -p skattr-core mailbox::poll
git rm crates/core/src/mailbox/scheduler.rs    # if rename was a copy
git add crates/core/src/mailbox/poll.rs crates/core/src/mailbox/mod.rs crates/core/Cargo.toml
git commit -m "mailbox: poll::next_interval pure function with jitter"
```

---

## Task 14: `PollScheduler` actor

**Files:**
- Modify: `crates/core/src/mailbox/poll.rs` (extend)

### Step 1: Failing tests

Add to `mod tests`:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn scheduler_drives_per_mailbox_fetch_cycle() {
    use crate::mailbox::client::MailboxClient;
    use crate::mailbox::codec::{MailboxFrame, MailboxFrameCodec};
    use crate::mailbox::protocol::{ChallengeNonce, FetchResponse, DeleteOk, PendingDeposit};
    use futures::{SinkExt, StreamExt};
    use tokio::io::duplex;
    use tokio_util::codec::Framed;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let count = Arc::new(AtomicU32::new(0));
    let count_srv = count.clone();
    let (a, b) = duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let mut framed = Framed::new(b, MailboxFrameCodec::new());
        // Challenge
        let _ = framed.next().await;
        framed.send(MailboxFrame::ChallengeNonce(ChallengeNonce {
            nonce: [0; 32], issued_at: 1
        })).await.unwrap();
        // Fetch
        let _ = framed.next().await;
        framed.send(MailboxFrame::FetchResponse(FetchResponse {
            deposits: vec![PendingDeposit {
                deposit_id: [1; 16], ciphertext: vec![9], received_at: 1
            }],
        })).await.unwrap();
        // Delete (Challenge + Delete)
        let _ = framed.next().await;
        framed.send(MailboxFrame::ChallengeNonce(ChallengeNonce {
            nonce: [1; 32], issued_at: 1
        })).await.unwrap();
        let _ = framed.next().await;
        framed.send(MailboxFrame::DeleteOk(DeleteOk { deleted: 1, not_found: 0 }))
            .await.unwrap();
        count_srv.fetch_add(1, Ordering::SeqCst);
    });

    // Run a single tick of the per-mailbox actor against this duplex.
    let signer = crate::identity::IdentityKey::generate().unwrap();
    let mut client = MailboxClient::from_stream("a.onion".into(), a);
    let dispatched = run_one_poll_tick(&mut client, &signer).await.unwrap();
    assert_eq!(dispatched.deposits.len(), 1);
    server.await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
}
```

### Step 2: Implement `run_one_poll_tick`

Extract the per-tick driver as a standalone function so unit tests don't have to spin up the full actor:

```rust
/// One Challenge → Fetch → (decrypt + persist) → Delete cycle.
/// Returns the deposits that were fetched (caller owns the
/// MLS-decrypt + persist + emit events).
pub(crate) async fn run_one_poll_tick<S>(
    client: &mut crate::mailbox::client::MailboxClient<S>,
    signer: &crate::identity::IdentityKey,
) -> crate::error::Result<crate::mailbox::protocol::FetchResponse>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    let resp = client.fetch(signer).await?;
    if !resp.deposits.is_empty() {
        let ids: Vec<[u8; 16]> = resp.deposits.iter().map(|d| d.deposit_id).collect();
        client.delete(signer, ids).await?;
    }
    Ok(resp)
}
```

(The actor wraps `run_one_poll_tick` with status updates + event emission — Task 15.)

### Step 3: Verify + commit

```bash
cargo test -p skattr-core mailbox::poll
git add crates/core/src/mailbox/poll.rs
git commit -m "mailbox: run_one_poll_tick — Challenge→Fetch→Delete cycle"
```

---

## Task 15: `PollScheduler` task surface (control + spawn)

**Files:**
- Modify: `crates/core/src/mailbox/poll.rs` (extend with actor task)

### Step 1: Failing tests

Add:

```rust
#[tokio::test]
async fn ctrl_bump_active_shortens_next_interval() {
    let (ctrl, mut rx) = tokio::sync::mpsc::channel::<PollerCtrl>(8);
    ctrl.send(PollerCtrl::BumpActive).await.unwrap();
    let recv = rx.recv().await.unwrap();
    assert!(matches!(recv, PollerCtrl::BumpActive));
}
```

(Most behaviour is exercised by Task 25's integration test; here we lock the channel API.)

### Step 2: Implement actor + spawn

```rust
use tokio::sync::mpsc;

#[derive(Debug)]
pub(crate) enum PollerCtrl {
    AddMailbox(i64),
    RemoveMailbox(i64),
    BumpActive,
    Shutdown,
}

pub(crate) struct PollScheduler {
    ctrl: mpsc::Sender<PollerCtrl>,
}

impl PollScheduler {
    pub fn ctrl(&self) -> mpsc::Sender<PollerCtrl> { self.ctrl.clone() }

    /// Spawn the supervisor + per-mailbox actors.
    pub fn spawn(
        pool: std::sync::Arc<crate::storage::Pool>,
        identity: std::sync::Arc<crate::identity::IdentityKey>,
        events: tokio::sync::broadcast::Sender<crate::daemon::events::Event>,
        connect_factory: std::sync::Arc<dyn ConnectFactory>,
    ) -> Self {
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<PollerCtrl>(16);
        tokio::spawn(supervisor(pool, identity, events, connect_factory, ctrl_rx));
        Self { ctrl: ctrl_tx }
    }
}

/// Connection factory used by both the poller and the fallback
/// orchestrator (Task 20). Implementations: an Arti-backed factory in
/// production; a duplex-backed factory in tests. Reused by name as
/// `MailboxConnectFactory` in Task 20 — same trait, do not duplicate.
#[async_trait::async_trait]
pub(crate) trait MailboxConnectFactory: Send + Sync + 'static {
    /// Boxed stream type chosen at the call site. Define a
    /// `pub(crate) trait MailboxStream: AsyncRead + AsyncWrite + Unpin + Send {}`
    /// alias and a blanket impl in `core::mailbox::client` to avoid
    /// repeating the trait bounds at every callsite.
    async fn connect(
        &self,
        onion: &str,
    ) -> crate::error::Result<
        crate::mailbox::client::MailboxClient<Box<dyn crate::mailbox::client::MailboxStream>>,
    >;
}
```

The supervisor task pseudocode:

```rust
async fn supervisor(
    pool: Arc<Pool>,
    identity: Arc<IdentityKey>,
    events: broadcast::Sender<Event>,
    connect_factory: Arc<dyn ConnectFactory>,
    mut rx: mpsc::Receiver<PollerCtrl>,
) {
    let mut handles: HashMap<i64, JoinHandle<()>> = HashMap::new();
    // Bootstrap: spawn one actor per existing 'mine' mailbox.
    let repo = MailboxRepo::new(&pool);
    for row in repo.list_mine().unwrap_or_default() {
        spawn_actor(row.id, row.onion.clone(), &mut handles,
                    pool.clone(), identity.clone(), events.clone(),
                    connect_factory.clone());
    }
    while let Some(ctrl) = rx.recv().await {
        match ctrl {
            PollerCtrl::AddMailbox(id) => { /* read row; spawn_actor */ }
            PollerCtrl::RemoveMailbox(id) => { handles.remove(&id).map(|h| h.abort()); }
            PollerCtrl::BumpActive => { /* fan out to per-actor channels */ }
            PollerCtrl::Shutdown => { for (_, h) in handles.drain() { h.abort(); } break; }
        }
    }
}
```

The per-mailbox actor loop:

```rust
async fn actor_loop(...) {
    let mut active_until = Instant::now() - ACTIVE_HOLD; // initially Idle
    let mut rng = rand::thread_rng();
    let mut last_status = MailboxStatus::Unknown;
    loop {
        let now = Instant::now();
        let active = now < active_until;
        let unreachable = matches!(last_status, MailboxStatus::Unreachable);
        tokio::time::sleep(next_interval(active, unreachable, &mut rng)).await;

        match connect_factory.connect(&onion).await {
            Ok(mut client) => match run_one_poll_tick(&mut client, &identity).await {
                Ok(resp) => {
                    // Mailbox produced traffic for us → bump active.
                    if !resp.deposits.is_empty() { active_until = now + ACTIVE_HOLD; }
                    repo.touch_poll(id, now_unix()).ok();
                    set_status(&repo, id, MailboxStatus::Reachable, &events, &mut last_status);
                    // Hand each deposit to the inbound MLS dispatcher.
                    for d in resp.deposits { handle_deposit(d, &events).await; }
                }
                Err(e) => record_and_maybe_status(...),
            }
            Err(e) => record_and_maybe_status(...),
        }
    }
}
```

(Concrete code lands in the implementation; this skeleton is the contract.)

### Step 3: Verify + commit

```bash
cargo check -p skattr-core
git add crates/core/src/mailbox/poll.rs
git commit -m "mailbox: PollScheduler supervisor + per-mailbox actor"
```

---

## Task 16: `Envelope::Kind::ContactCardUpdate` variant + dispatcher branch

**Files:**
- Modify: `crates/core/src/envelope/kinds.rs` (new variant)
- Modify: `crates/core/src/delivery/peer.rs` or wherever `InboundDispatch` decodes envelope kinds (search for `Kind::Text` matches)

### Step 1: Failing test

Add to `crates/core/src/envelope/kinds.rs` `mod tests` (create the module if absent):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::contact::ContactCard;
    use crate::contact::card::ContactCardBody;
    use crate::identity::{PublicKey, Signature};

    #[test]
    fn contact_card_update_round_trips_cbor() {
        let card = ContactCard {
            body: ContactCardBody {
                identity: PublicKey([7; 32]),
                onion: "aaaa.onion".into(),
                mailboxes: vec!["bbbb.onion".into()],
                version: 3,
                expires_at: 1_700_000_000,
            },
            signature: Signature([0; 64]),
        };
        let kind = Kind::ContactCardUpdate { card };
        let mut buf = Vec::new();
        ciborium::into_writer(&kind, &mut buf).unwrap();
        let back: Kind = ciborium::from_reader(&buf[..]).unwrap();
        assert!(matches!(back, Kind::ContactCardUpdate { .. }));
    }
}
```

### Step 2: Add the variant

In `kinds.rs`:

```rust
/// Self-published ContactCard (rotation, mailbox-list change).
ContactCardUpdate {
    /// Signed card carrying the new onion + mailbox list.
    card: crate::contact::ContactCard,
},
```

### Step 3: Branch the inbound dispatcher

Locate the place where MLS-decrypted envelopes are persisted (1.G's `MessageRepo::insert_in_tx` call site). Wrap the existing path in a `match envelope.kind { ... }` and add:

```rust
Kind::ContactCardUpdate { card } => {
    // Verify the embedded signature against the sender's identity.
    let sender_pk = card.verify(now_unix_seconds())?;
    // Cross-check: card.body.identity must equal the MLS sender.
    if sender_pk != peer_identity {
        return Err(CoreError::Contact(ContactErrorKind::Other(
            "card-update: sender mismatch".into(),
        )));
    }
    let repo = ContactRepo::new(&pool);
    repo.put_card(&card)?;
    let _ = events.send(Event::ContactCardReceived {
        contact: sender_pk,
        version: card.body.version,
    });
}
```

(`Event::ContactCardReceived` lands in Task 19.)

### Step 4: Verify + commit

```bash
cargo test -p skattr-core envelope::kinds
git add crates/core/src/envelope/kinds.rs crates/core/src/delivery/peer.rs
git commit -m "envelope: Kind::ContactCardUpdate + inbound dispatch"
```

---

## Task 17: ContactCard self-publish helper

**Files:**
- Create: `crates/core/src/contact/self_card.rs`
- Modify: `crates/core/src/contact/mod.rs` (`pub(crate) mod self_card;`)

### Step 1: Failing tests

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Self-published ContactCard helpers used by RotateOnion / AddMailbox /
//! RemoveMailbox.

use crate::contact::ContactCard;
use crate::error::Result;
use crate::identity::IdentityKey;
use crate::storage::Pool;

/// Build the next self-card with `version = previous + 1`.
pub(crate) fn build_next_self_card(
    pool: &Pool,
    signer: &IdentityKey,
    onion: String,
    mailboxes: Vec<String>,
    ttl_secs: u64,
    now: i64,
) -> Result<ContactCard> { todo!() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_self_card_starts_at_version_1() {
        let pool = Pool::in_memory();
        let signer = IdentityKey::generate().unwrap();
        let card = build_next_self_card(&pool, &signer,
            "aaaa.onion".into(), vec![], 3600, 100).unwrap();
        assert_eq!(card.body.version, 1);
    }

    #[test]
    fn second_self_card_bumps_version() {
        let pool = Pool::in_memory();
        let signer = IdentityKey::generate().unwrap();
        let _ = build_next_self_card(&pool, &signer, "aaaa.onion".into(), vec![], 3600, 100).unwrap();
        // Persist via SelfCardRepo (Task 18) — for now poke directly
        // into a self_card_versions singleton table or KV.
        let card2 = build_next_self_card(&pool, &signer, "bbbb.onion".into(), vec![], 3600, 200).unwrap();
        assert_eq!(card2.body.version, 2);
    }
}
```

### Step 2: Decide where the version counter lives

Two choices:
- **A.** Reuse the existing `identity` table — add a `self_card_version INTEGER NOT NULL DEFAULT 0` column in 0008 (early — already shipped).
- **B.** Add a tiny `self_card_state(version INTEGER)` singleton table in a 0009 migration.

Pick **A** but defer the schema change: store the version in a singleton row of `mailboxes`-adjacent state. Actually simplest: persist last self-card in `contacts`-style table with `identity_pubkey = me`. Look at what 1.D / 1.F / 1.G did; the design says "ContactRepo::put_self_card (or equivalent) — locally persist."

Decision for the plan: extend migration 0008 retroactively? **No** — migrations are append-only. Add migration 0009 `self_card_version.sql`:

```sql
INSERT OR IGNORE INTO schema_version (version) VALUES (9);
CREATE TABLE IF NOT EXISTS self_card_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    version INTEGER NOT NULL DEFAULT 0
);
INSERT OR IGNORE INTO self_card_state (id, version) VALUES (1, 0);
```

Wire migration 0009 into the runner (mirror Task 1 step 4). Implement `build_next_self_card` to read+increment that singleton.

### Step 3: Verify + commit

```bash
cargo test -p skattr-core contact::self_card
git add crates/core/src/contact/self_card.rs crates/core/src/contact/mod.rs \
        crates/core/src/storage/migrations/0009_self_card_state.sql \
        crates/core/src/storage/migrations.rs
git commit -m "contact: build_next_self_card + self_card_state singleton"
```

---

## Task 18: `DeliveryStatus` enum + `Event::DeliveryStatusChanged`

**Files:**
- Modify: `crates/core/src/daemon/events.rs`

### Step 1: Failing test

Existing `events.rs` round-trip tests can serve as the model. Add:

```rust
#[test]
fn delivery_status_changed_round_trips() {
    let e = Event::DeliveryStatusChanged {
        message_id: MessageId([0xAB; 16]),
        status: DeliveryStatus::Deposited,
    };
    let json = serde_json::to_string(&e).unwrap();
    let back: Event = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, Event::DeliveryStatusChanged { .. }));
}
```

### Step 2: Add types

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus { Queued, Sent, Deposited, Acked, Failed }

// In Event:
DeliveryStatusChanged { message_id: MessageId, status: DeliveryStatus },
```

### Step 3: Verify + commit

```bash
cargo test -p skattr-core daemon::events
git add crates/core/src/daemon/events.rs
git commit -m "events: DeliveryStatus + DeliveryStatusChanged"
```

---

## Task 19: `Event::MailboxStatusChanged` + `Event::ContactCardReceived` + EventFilter

**Files:** `crates/core/src/daemon/events.rs`.

### Step 1: Failing test

```rust
#[test]
fn mailbox_status_changed_round_trips() { /* analogous */ }
#[test]
fn contact_card_received_round_trips() { /* analogous */ }
#[test]
fn event_filter_includes_new_filters() {
    assert!(EventFilter::Mailboxes.matches(&Event::MailboxStatusChanged {
        mailbox_id: 1, status: MailboxStatus::Reachable,
    }));
    assert!(EventFilter::Delivery.matches(&Event::DeliveryStatusChanged {
        message_id: MessageId([0; 16]), status: DeliveryStatus::Acked,
    }));
}
```

### Step 2: Implement

Re-export `MailboxStatus` from `crate::storage::mailboxes`. Add:

```rust
MailboxStatusChanged { mailbox_id: i64, status: MailboxStatus },
ContactCardReceived  { contact: PublicKey, version: u64 },
```

Add `Mailboxes`, `Delivery` arms to `EventFilter` and extend `EventFilter::matches`.

### Step 3: Commit

```bash
cargo test -p skattr-core daemon::events
git add crates/core/src/daemon/events.rs
git commit -m "events: MailboxStatusChanged + ContactCardReceived + filters"
```

---

## Task 20: `DeliveryHub::ensure_mailbox_fallback`

**Files:**
- Modify: `crates/core/src/delivery/hub.rs`
- Modify: `crates/core/src/delivery/peer.rs` (signal fallback after `direct_timeout_secs`)

### Step 1: Failing test

Add to `crates/core/src/delivery/hub.rs` `mod tests`:

```rust
#[tokio::test]
async fn ensure_mailbox_fallback_picks_one_then_retries() {
    use crate::storage::mailboxes::MailboxRepo;
    use crate::storage::outbox::{OutboxRepo, OutboxTargetKind};
    let pool = Arc::new(Pool::in_memory());
    let hub: DeliveryHub<tokio::io::DuplexStream> = DeliveryHub::new(pool.clone());

    let peer = PublicKey([42; 32]);
    let msg_id = MessageId([0xCD; 16]);

    // Seed contact + a card with two mailboxes.
    seed_contact_with_card(&pool, peer, &["aaaa.onion", "bbbb.onion"]);

    // Insert a direct outbox row first (1.E persists this on send).
    OutboxRepo::new(&pool).insert_direct(&peer.0, &msg_id.0, &[1, 2, 3], 0).unwrap();

    // Inject a fake MailboxClient factory that fails the first onion
    // and succeeds on the second. Assert the outbox row flips to
    // mailbox + the second mailbox_id, then is deleted on DepositOk.
    // (Uses a `dyn MailboxConnectFactory` that the hub takes.)
}
```

### Step 2: Implement the orchestrator

```rust
// `MailboxConnectFactory` is the shared trait introduced in Task 15;
// the orchestrator takes the same `Arc<dyn MailboxConnectFactory>` the
// PollScheduler does so production and tests inject the same factory.

impl<S> DeliveryHub<S> where S: AsyncRead + AsyncWrite + Unpin + Send + 'static {
    pub async fn ensure_mailbox_fallback(
        &self,
        peer: PublicKey,
        msg_id: MessageId,
        ciphertext: Vec<u8>,
    ) {
        let pool = self.pool.clone();
        let factory = self.mailbox_factory.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            let card = match ContactRepo::new(&pool).latest_card(&peer).ok().flatten() {
                Some(c) => c,
                None => return,         // no mailboxes — outbox row stays direct
            };
            let mailboxes = card.body.mailboxes;
            if mailboxes.is_empty() { return; }

            // Pick-one: blake2s(message_id) % len.
            let primary = pick_index(&msg_id.0, mailboxes.len());
            let order: Vec<usize> = (0..mailboxes.len()).cycle()
                .skip(primary).take(mailboxes.len()).collect();

            for idx in order {
                let onion = &mailboxes[idx];
                let mailbox_id = match MailboxRepo::new(&pool).id_for_theirs(onion) {
                    Ok(Some(id)) => id,
                    _ => continue,
                };
                if let Err(e) = OutboxRepo::new(&pool)
                    .set_mailbox_target(/* row id */, mailbox_id) { continue; }

                match factory.connect(onion).await {
                    Ok(mut client) => {
                        let recipient_hash = sha256_pubkey(&peer.0);
                        match client.deposit(recipient_hash, ciphertext.clone(), 86_400).await {
                            Ok(_) => {
                                OutboxRepo::new(&pool)
                                    .delete_for(&peer.0, &msg_id.0).ok();
                                let _ = events.send(Event::DeliveryStatusChanged {
                                    message_id: msg_id,
                                    status: DeliveryStatus::Deposited,
                                });
                                return;
                            }
                            Err(_) => continue,        // try next mailbox
                        }
                    }
                    Err(_) => continue,
                }
            }
            // All mailboxes rejected — leave outbox row for backoff retry.
        });
    }
}

fn pick_index(msg_id: &[u8; 16], len: usize) -> usize {
    use blake2::{Blake2s256, Digest};
    let h = Blake2s256::digest(msg_id);
    let n = u64::from_le_bytes(h[0..8].try_into().unwrap()) as usize;
    n % len
}
```

### Step 3: Wire `PeerConnection` to fire the orchestrator

In `delivery::peer`, after `direct_timeout_secs` of unsuccessful direct delivery for a job, call `hub.ensure_mailbox_fallback(peer, msg_id, ct)`. Direct retries continue in parallel; whichever path resolves first emits `DeliveryStatusChanged`.

### Step 4: Verify + commit

```bash
cargo test -p skattr-core delivery::hub
git add crates/core/src/delivery/hub.rs crates/core/src/delivery/peer.rs
git commit -m "delivery: hub.ensure_mailbox_fallback + pick-one-then-retry"
```

---

## Task 21: `Command::AddMailbox` — validate-then-publish

**Files:**
- Modify: `crates/core/src/daemon/commands.rs` (add variant)
- Modify: `crates/core/src/daemon/dispatch.rs` (new handler)

### Step 1: Failing tests

Add to `crates/tests/src/cli_ipc_roundtrip.rs` (or a fresh `add_mailbox_command.rs` integration test):

```rust
#[tokio::test]
async fn add_mailbox_unreachable_returns_invalid_argument() {
    // Spawn daemon, drive Command::AddMailbox { onion: "unreachable.onion" }.
    // Assert CommandResult::Err(DaemonErrorKind::InvalidArgument(_))
    //  with reason == "unreachable".
}

#[tokio::test]
async fn add_mailbox_reachable_inserts_row_and_bumps_card_version() {
    // Spawn daemon + in-process MailboxServer over duplex.
    // Inject a connect-factory that returns a duplex peer.
    // Drive AddMailbox; assert row + card.version increment.
}
```

### Step 2: Add the command + handler

```rust
// commands.rs
AddMailbox { onion: String },

// dispatch.rs
async fn handle_add_mailbox(...) -> Result<CommandResult, DaemonError> {
    let mut client = MailboxClient::connect(&onion, &tor).await
        .map_err(|_| DaemonErrorKind::InvalidArgument("unreachable".into()))?;
    let id_hash = sha256_pubkey(&identity.public().0);
    client.probe(id_hash).await.map_err(|e| match e {
        CoreError::MailboxClient(MailboxClientErrorKind::UnsupportedVersion) =>
            DaemonErrorKind::InvalidArgument("unsupported_version".into()),
        CoreError::MailboxClient(MailboxClientErrorKind::RateLimited) =>
            DaemonErrorKind::InvalidArgument("rate_limited".into()),
        CoreError::MailboxClient(MailboxClientErrorKind::Malformed) =>
            DaemonErrorKind::InvalidArgument("malformed_response".into()),
        _ => DaemonErrorKind::InvalidArgument("other".into()),
    })?;

    let id = MailboxRepo::new(&pool).add_mine(&onion, now_unix_seconds())?;
    MailboxRepo::new(&pool).mark_status(id, MailboxStatus::Reachable, now_unix_seconds())?;
    poller.send(PollerCtrl::AddMailbox(id)).await.ok();

    publish_self_card_update(&pool, &identity, &delivery_hub).await?;
    Ok(CommandResult::Ok)
}
```

`publish_self_card_update` builds the next self-card (Task 17), wraps it as `Envelope::Kind::ContactCardUpdate`, and fans out via `delivery_hub.send(...)` to each contact whose `latest_card.body.mailboxes` is non-empty (or who has a current direct connection).

### Step 3: Verify + commit

```bash
cargo test -p skattr-core daemon::dispatch
cargo test -p skattr-tests add_mailbox
git add crates/core/src/daemon/commands.rs crates/core/src/daemon/dispatch.rs
git commit -m "daemon: AddMailbox validate-then-publish"
```

---

## Task 22: `Command::RemoveMailbox` — drain-then-drop

**Files:**
- Modify: `crates/core/src/daemon/commands.rs` (variant)
- Modify: `crates/core/src/daemon/dispatch.rs` (handler)

### Step 1: Failing test

```rust
#[tokio::test]
async fn remove_mailbox_drains_pending_then_finalizes() {
    // Pre-deposit a message to the mailbox; remove the mailbox;
    // assert the deposit arrives at the daemon BEFORE the row flips
    // to status='removed'.
}
```

### Step 2: Implement handler

```rust
async fn handle_remove_mailbox(id: i64, ...) -> Result<CommandResult, DaemonError> {
    let row = MailboxRepo::new(&pool).get(id)?
        .ok_or(DaemonErrorKind::InvalidArgument("not_found".into()))?;
    MailboxRepo::new(&pool).mark_pending_removal(id)?;
    events.send(Event::MailboxStatusChanged { mailbox_id: id, status: MailboxStatus::PendingRemoval })
        .ok();

    // Final drain: one Challenge → Fetch → Delete cycle. On any error,
    // log + proceed (we still finalize the row to avoid getting stuck
    // on a misbehaving mailbox).
    if let Ok(mut client) = MailboxClient::connect(&row.onion, &tor).await {
        if let Ok(resp) = run_one_poll_tick(&mut client, &identity).await {
            // Each deposit goes through the standard inbound dispatcher
            // (handled by the broadcast receiver loop).
        }
    }

    poller.send(PollerCtrl::RemoveMailbox(id)).await.ok();
    MailboxRepo::new(&pool).finalize_removal(id)?;
    events.send(Event::MailboxStatusChanged { mailbox_id: id, status: MailboxStatus::Removed })
        .ok();
    publish_self_card_update(&pool, &identity, &delivery_hub).await?;
    Ok(CommandResult::Ok)
}
```

### Step 3: Verify + commit

```bash
cargo test -p skattr-core daemon::dispatch
cargo test -p skattr-tests remove_mailbox
git add crates/core/src/daemon/dispatch.rs crates/core/src/daemon/commands.rs
git commit -m "daemon: RemoveMailbox drain-then-drop + republish"
```

---

## Task 23: `Command::RotateOnion`

**Files:**
- Modify: `crates/core/src/daemon/commands.rs` (variant)
- Modify: `crates/core/src/daemon/dispatch.rs` (handler)
- Modify: wherever the daemon owns the running `OnionService` (likely `core::transport::listener` and/or `daemon::run`)

### Step 1: Failing test

```rust
#[tokio::test]
async fn rotate_onion_publishes_new_card_and_keeps_old_listening_briefly() {
    use tokio::time::{pause, advance, Duration};
    pause();
    // Spawn paired daemons. Alice rotates. Assert:
    // 1. Alice's self-card version bumps.
    // 2. Bob receives ContactCardReceived with the new onion + new version.
    // 3. Old onion is still listening at t = rotate_grace_secs - 1s.
    // 4. Old onion is shut down at t = rotate_grace_secs + 1s.
    advance(Duration::from_secs(rotate_grace_secs() - 1)).await;
    // ... assert alive ...
    advance(Duration::from_secs(2)).await;
    // ... assert shut down ...
}
```

### Step 2: Implement handler

```rust
async fn handle_rotate_onion(...) -> Result<CommandResult, DaemonError> {
    // 1. Spin up a new OnionService task with a freshly-generated key.
    let new_handle = transport::listener::spawn_new_onion(&data_dir, &arti).await?;
    let new_onion = new_handle.onion_string();

    // 2. Schedule old onion shutdown in `rotate_grace_secs`.
    let old_handle = current_onion_handle.swap(new_handle.clone());
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(rotate_grace_secs())).await;
        old_handle.abort();
    });

    // 3. Build + persist the new self-card.
    let mailboxes = MailboxRepo::new(&pool)
        .list_mine()?
        .into_iter()
        .filter(|r| r.status == MailboxStatus::Reachable)
        .map(|r| r.onion).collect();
    let card = build_next_self_card(&pool, &identity, new_onion, mailboxes,
                                    7 * 24 * 3600, now_unix_seconds())?;

    // 4. Fan-out via MLS app message + DeliveryHub direct→mailbox fallback.
    publish_card_to_all_contacts(&pool, &identity, &delivery_hub, card).await?;

    Ok(CommandResult::Ok)
}
```

`publish_card_to_all_contacts` iterates `ContactRepo::list()`, encrypts an `Envelope::Kind::ContactCardUpdate { card }` per pairwise group via `Group::encrypt`, hands the ciphertext to `DeliveryHub::send`. Direct→mailbox fallback handles offline peers.

### Step 3: Verify + commit

```bash
cargo test -p skattr-tests rotate_onion
git add crates/core/src/daemon/dispatch.rs crates/core/src/daemon/commands.rs \
        crates/core/src/transport/listener.rs
git commit -m "daemon: RotateOnion with grace-period old-onion teardown"
```

---

## Task 24: `Command::ListMailboxes` — replace 2.C stub

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (replace stub handler)

### Step 1: Failing test

```rust
#[tokio::test]
async fn list_mailboxes_returns_real_rows_after_add() {
    let daemon = spawn_daemon_test().await;
    daemon.send(Command::AddMailbox { onion: "aaaa.onion".into() }).await.unwrap();
    let resp = daemon.send(Command::ListMailboxes).await.unwrap();
    let CommandResult::Mailboxes(rows) = resp else { panic!() };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].onion, "aaaa.onion");
    assert!(matches!(rows[0].status, MailboxStatus::Reachable | MailboxStatus::Unknown));
}
```

### Step 2: Implement

Replace the 2.C stub body:

```rust
async fn handle_list_mailboxes(pool: &Pool) -> Result<CommandResult, DaemonError> {
    let rows = MailboxRepo::new(pool).list_mine()?;
    let summaries = rows.into_iter().map(|r| MailboxSummary {
        id: r.id, onion: r.onion, status: r.status,
        registered_at: r.registered_at as u64,
    }).collect();
    Ok(CommandResult::Mailboxes(summaries))
}
```

### Step 3: Verify + commit

```bash
cargo test -p skattr-core
cargo test -p skattr-tests list_mailboxes
git add crates/core/src/daemon/dispatch.rs
git commit -m "daemon: ListMailboxes reads MailboxRepo"
```

---

## Task 25: Integration test — `mailbox_offline_delivery`

**Files:**
- Create: `crates/tests/src/mailbox_offline_delivery.rs`
- Modify: `crates/tests/src/lib.rs`

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Alice → offline Bob via mailbox; Bob comes online; ACK reaches Alice.

#[tokio::test]
async fn alice_sends_to_offline_bob_via_mailbox_and_receives_ack() {
    // 1. Spin up an in-process MailboxServer over a tokio::io::duplex pair
    //    (matches Phase 2.A's pattern).
    // 2. Spawn Alice's daemon. Alice's contact list contains Bob with a
    //    ContactCard that lists the test mailbox onion.
    // 3. Spawn Bob's daemon, but leave Bob's onion listener offline.
    // 4. Drive Command::SendMessage(bob, "hello") on Alice's IPC.
    // 5. After direct_timeout_secs (mock-clocked via tokio::time::pause),
    //    assert Alice's outbox row flipped target_kind='mailbox' and emitted
    //    DeliveryStatusChanged{Deposited}.
    // 6. Bring Bob's daemon online; subscribe to PollScheduler tick.
    // 7. Poll cycle fetches the deposit; Bob persists; emits MessageReceived.
    // 8. Bob initiates direct connection to Alice (now both online), sends
    //    Frame::Ack(msg_id).
    // 9. Alice's row flips to Acked; emits DeliveryStatusChanged{Acked}.
    //
    // Inject the test mailbox into both daemons via an
    // `IntoMailboxFactory(server_duplex)` test-harness type so Alice's
    // DeliveryHub deposits to it and Bob's PollScheduler fetches from it.
}
```

Verify + commit:

```bash
cargo test -p skattr-tests mailbox_offline_delivery
git add crates/tests/src/mailbox_offline_delivery.rs crates/tests/src/lib.rs
git commit -m "tests: mailbox_offline_delivery — full deposit→fetch→ack cycle"
```

---

## Task 26: Integration test — `mailbox_failover`

**Files:** `crates/tests/src/mailbox_failover.rs`.

```rust
// Two test mailboxes. The first returns ErrorCode::RateLimited on Deposit;
// the second accepts. Assert the orchestrator falls over and DepositOk
// arrives via the second.
```

Same shape as Task 25 — register two mailboxes on Bob's contact card. Verify + commit:

```bash
cargo test -p skattr-tests mailbox_failover
git add crates/tests/src/mailbox_failover.rs crates/tests/src/lib.rs
git commit -m "tests: mailbox_failover — rate-limited primary, second accepts"
```

---

## Task 27: Integration test — `rotate_onion_during_offline`

**Files:** `crates/tests/src/rotate_onion_during_offline.rs`.

```rust
// Alice rotates while Bob is offline. ContactCardUpdate envelope queues
// in Alice's outbox → mailbox fallback after direct_timeout_secs.
// Bob comes online, polls the mailbox, decrypts the card-update,
// emits ContactCardReceived, future Alice→Bob direct messages route to
// the new onion. Old onion shut down at rotate_grace_secs.
```

Use `tokio::time::pause` to advance to past `rotate_grace_secs` and assert the old listener is no longer accepting.

```bash
cargo test -p skattr-tests rotate_onion_during_offline
git add crates/tests/src/rotate_onion_during_offline.rs crates/tests/src/lib.rs
git commit -m "tests: rotate_onion_during_offline — full rotation arc"
```

---

## Task 28: Integration test — `add_mailbox_validates`

**Files:** `crates/tests/src/add_mailbox_validates.rs`.

```rust
#[tokio::test] async fn unreachable_onion_rejected_with_invalid_argument() { /* … */ }
#[tokio::test] async fn reachable_onion_inserts_and_publishes_card_update() { /* … */ }
```

```bash
cargo test -p skattr-tests add_mailbox_validates
git commit -m "tests: add_mailbox_validates — both happy + unhappy paths"
```

---

## Task 29: Integration test — `remove_mailbox_drains`

**Files:** `crates/tests/src/remove_mailbox_drains.rs`.

```rust
// Pre-deposit one message; remove the mailbox; assert the deposit
// is delivered + persisted before the row flips to 'removed'.
```

```bash
cargo test -p skattr-tests remove_mailbox_drains
git commit -m "tests: remove_mailbox_drains — final-drain semantics"
```

---

## Task 30: Adversarial regression suite

**Files:** `crates/core/tests/mailbox_client_adversarial.rs`.

Five sub-tests in one file:

```rust
#[tokio::test] async fn malicious_mailbox_internal_on_every_fetch() { /* status flips Unreachable after 5 */ }
#[tokio::test] async fn malicious_replay_old_nonce()                { /* expect re-Challenge + retry */ }
#[tokio::test] async fn malicious_returns_arbitrary_ciphertext()    { /* decrypt rejects, deposit_id still deleted */ }
#[tokio::test] async fn malformed_cbor_dropped()                    { /* MailboxClientErrorKind::Malformed */ }
#[tokio::test] async fn concurrent_rotate_and_add_mailbox_keeps_version_monotonic() { /* version invariants */ }
```

Each test uses the in-process duplex pattern. The fifth test queues two commands rapid-fire and asserts the persisted self-card versions are strictly monotonic.

```bash
cargo test -p skattr-core --test mailbox_client_adversarial
git add crates/core/tests/mailbox_client_adversarial.rs
git commit -m "tests: mailbox client adversarial regression"
```

---

## Task 31: Logging-redaction unit test

**Files:** `crates/core/tests/mailbox_client_logging_redaction.rs`.

Mirror the 2.A pattern: a `tracing` test subscriber captures every line emitted by `core::mailbox::client`, `core::mailbox::poll`, and the new `daemon::dispatch::handle_*_mailbox` functions during a representative session. Assertions:

- No 56+1-char `*.onion` strings at level >= INFO.
- No 64-char hex strings at level >= INFO (would be a hash or pubkey).
- No `MessageId` 32-char hex strings at level >= INFO.
- No ciphertext byte sequences at level >= INFO.

```rust
#[tokio::test]
async fn info_level_logs_redact_onions_pubkeys_and_ids() {
    use tracing_subscriber::layer::SubscriberExt;
    let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let layer = CapturingLayer::new(captured.clone());
    let subscriber = tracing_subscriber::registry().with(layer);
    tracing::subscriber::with_default(subscriber, || { /* drive a session */ });
    for line in captured.lock().unwrap().iter() {
        assert!(!line.contains(".onion"), "leaked onion: {line}");
        assert!(!line.matches(|c: char| c.is_ascii_hexdigit()).count().ge(&64),
                "long hex run looks like a key: {line}");
    }
}
```

```bash
cargo test -p skattr-core --test mailbox_client_logging_redaction
git add crates/core/tests/mailbox_client_logging_redaction.rs
git commit -m "tests: mailbox client logging-redaction guard"
```

---

## Task 32: `#[ignore]`-gated real-Tor scenario

**Files:** `crates/tests/src/mailbox_client_real_tor.rs`.

Spawns the `skattr-mailbox` binary, two daemons, drives offline-delivery + rotation. Manual run before merge:

```rust
#[tokio::test]
#[ignore = "requires real Arti circuits + spawned skattr-mailbox binary"]
async fn real_tor_offline_delivery_plus_rotation() {
    // 1. tokio::process::Command::new("./target/release/skattr-mailbox") ...
    // 2. Wait for healthcheck on the UDS at $TMPDIR/health.sock.
    // 3. Spawn two daemons over real onion services (matches 1.E's pattern).
    // 4. Drive offline-delivery; assert ack arrives within 30s of Bob coming online.
    // 5. Rotate Alice; assert Bob picks up ContactCardReceived; Alice's old
    //    onion stops accepting new connections after rotate_grace_secs.
}
```

Run only with: `cargo test -p skattr-tests --release -- --ignored mailbox_client_real_tor`.

```bash
git add crates/tests/src/mailbox_client_real_tor.rs crates/tests/src/lib.rs
git commit -m "tests: #[ignore]-gated real-Tor scenario for 2.B"
```

---

## Task 33: CLAUDE.md status update + CHANGELOG

**Files:**
- Modify: `/home/myggiz/development/skattr/CLAUDE.md` (Repository state section)
- Modify: `CHANGELOG.md` (or equivalent — locate via `ls *.md`)

### Step 1: Update CLAUDE.md

Edit the "Repository state" paragraph. Append, after the Phase 2.A section:

```markdown
Phase 2.B (mailbox client + ContactCard rotation) merged at the head
of `phase-2b-mailbox-client`. `core::mailbox::{client, codec, poll}`
ships the v1-protocol client (long-lived per-`'mine'` mailbox, on-demand
for deposits), an Idle/Active/Unreachable per-mailbox `PollScheduler`
with ±25 % jitter, the `DeliveryHub` direct→mailbox fallback (pick-one
+ sequential failover), and the `RotateOnion` / `AddMailbox` /
`RemoveMailbox` daemon commands. Migration 0008 adds status tracking
to `mailboxes` and `target_kind`/`mailbox_id` to `outbox` (composite
unique index); migration 0009 adds the `self_card_state` singleton.
ContactCard updates ride MLS app messages as
`Envelope::Kind::ContactCardUpdate`, so rotation reuses the same
fallback path as ordinary messages.

The next workstream is Phase 2.F (settings & history UI; depends on
both 2.B and 2.E) — see `docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md`.
```

### Step 2: Add CHANGELOG entry

```markdown
## Unreleased

### Added (Phase 2.B)
- Mailbox client (`core::mailbox::client`) with long-lived per-mailbox
  `Framed` connection.
- Adaptive `PollScheduler` with Idle ↔ Active ↔ Unreachable cadence.
- `DeliveryHub` direct→mailbox fallback (pick-one-then-retry).
- `Command::AddMailbox`, `Command::RemoveMailbox`, `Command::RotateOnion`.
- `Event::MailboxStatusChanged`, `Event::ContactCardReceived`,
  `Event::DeliveryStatusChanged`.
- `Envelope::Kind::ContactCardUpdate` for in-MLS card rotation.
- Migrations 0008 (mailbox status + outbox target_kind) and 0009
  (self_card_state singleton).
```

### Step 3: Run full validation

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo deny check
```
Expected: every command green.

### Step 4: Commit

```bash
git add CLAUDE.md CHANGELOG.md
git commit -m "docs: Phase 2.B status update + CHANGELOG entry"
```

---

## Final verification

```bash
cd /home/myggiz/development/skattr-phase-2b-mailbox-client
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo deny check
git log --oneline master..HEAD | wc -l         # ~33 commits expected
```

Manual real-Tor smoke (skim before requesting merge):

```bash
cargo build --release
cargo test -p skattr-tests --release -- --ignored mailbox_client_real_tor
```

When all green, follow `superpowers:finishing-a-development-branch` to open the merge PR.
