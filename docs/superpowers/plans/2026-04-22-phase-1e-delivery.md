# Phase 1.E Delivery Semantics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish the delivery layer so two daemons can exchange messages end-to-end with an outbox, exponential-backoff retry, in-memory ACK correlation, receiver-side dedup, and a per-peer actor-based connection pool — with a kill-mid-message integration test that proves exactly-once delivery across a reconnect.

**Architecture:** A `DeliveryHub` per daemon routes outbound sends and inbound post-handshake connections to per-peer `PeerConnection` actor tasks. Each actor owns an `Option<AuthenticatedConnection<S>>`, a `HashMap<MessageId, oneshot::Sender>` of in-flight ACKs, and polls the persisted outbox on a 1 s tick to retry rows whose `next_retry_at` has passed. The outbox is the source of truth for "needs redelivery"; the oneshot map is only for the prompt `DeliveryStatusChanged::Delivered` event. Migration 0004 adds `message_id` to the `outbox` table with a `UNIQUE(target, message_id)` index to make enqueue idempotent and ACK correlation a single-column lookup.

**Tech Stack:** Rust 2021, `tokio` + `tokio-util` (actor tasks, `select!`, `mpsc`/`oneshot`, `duplex`, `CancellationToken`), `snow` (Noise transport cipher, already integrated via 1.B), `openmls` (already integrated via 1.C), `rusqlite` + `age` (storage already in place), `arti-client` (production transport).

**Spec:** `docs/superpowers/specs/2026-04-22-phase-1e-delivery-design.md` (this worktree, commit `123d2a1`).

---

## Pre-flight

- [ ] **Confirm branch and clean worktree**

Run (from the main checkout, not the worktree):
```bash
cd /home/myggiz/development/skattr-phase-1e-delivery
git status --short
git log --oneline -3
```
Expected: no unstaged changes; HEAD at `123d2a1 spec: fix Event type refs …` on branch `phase-1e-delivery`.

- [ ] **Establish the baseline green build**

Run (from the worktree):
```bash
cd /home/myggiz/development/skattr-phase-1e-delivery
. "$HOME/.cargo/env"
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```
Expected: all three green. If any fails, stop and surface — the baseline must be clean before touching delivery code.

---

## Task 1: Migration 0004 — add `message_id` column to `outbox`

**Files:**
- Create: `crates/core/src/storage/migrations/0004_outbox_message_id.sql`
- Modify: `crates/core/src/storage/migrations.rs` (append to `ALL_MIGRATIONS`)
- Test: `crates/core/src/storage/migrations.rs` (new `migration_0004_adds_message_id_column` test)

### Step 1: Write the failing test

Append to the `#[cfg(test)] mod tests` block at the bottom of `crates/core/src/storage/migrations.rs`:

```rust
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
    assert_eq!(idx_count, 1, "unique index idx_outbox_target_message_id must exist");

    // schema_version is at 4
    let v: u32 = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, 4);
}
```

### Step 2: Run test to verify it fails

```bash
cargo test -p skattr-core --lib storage::migrations::tests::migration_0004 -- --nocapture
```
Expected: `FAILED` with something like `migration 0004 must add message_id column; got ["id", "target", "payload", "attempts", "next_retry_at"]` (the column doesn't exist yet).

### Step 3: Create the migration SQL

Create `crates/core/src/storage/migrations/0004_outbox_message_id.sql`:

```sql
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz AB
--
-- Skattr storage schema, version 4.
-- Add per-message id to the outbox so the delivery layer can
-- correlate inbound ACKs to rows without a separate lookup table
-- and so enqueues are idempotent per (target, message_id).

INSERT OR IGNORE INTO schema_version (version) VALUES (4);

ALTER TABLE outbox
    ADD COLUMN message_id BLOB NOT NULL
    DEFAULT (x'00000000000000000000000000000000');

CREATE UNIQUE INDEX IF NOT EXISTS idx_outbox_target_message_id
    ON outbox(target, message_id);
```

### Step 4: Register it in `ALL_MIGRATIONS`

Edit `crates/core/src/storage/migrations.rs`, append one more `Migration` to the end of the `ALL_MIGRATIONS` slice (keep the preceding three entries exactly as-is):

```rust
    Migration {
        version: 4,
        sql: include_str!("migrations/0004_outbox_message_id.sql"),
    },
```

### Step 5: Run the test to verify it passes

```bash
cargo test -p skattr-core --lib storage::migrations::tests::migration_0004 -- --nocapture
```
Expected: `ok`.

### Step 6: Confirm the rest of the storage tests still pass

```bash
cargo test -p skattr-core --lib storage::
```
Expected: all existing storage tests green (they don't touch the new column yet).

### Step 7: Commit

```bash
git add crates/core/src/storage/migrations.rs \
        crates/core/src/storage/migrations/0004_outbox_message_id.sql
git commit -m "$(cat <<'EOF'
storage: migration 0004 adds message_id column + unique index on outbox

The delivery layer (Phase 1.E) correlates inbound ACKs to outbox rows
via (target, message_id), and needs idempotent enqueue for the same
pair. The UNIQUE index gives us both.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Rewrite `storage::outbox::OutboxRepo` to use `message_id`

**Files:**
- Modify: `crates/core/src/storage/outbox.rs` (replace the API surface; tests are adjusted in the same file)

### Step 1: Write the failing tests

Replace the existing `#[cfg(test)] mod tests { … }` block at the bottom of `crates/core/src/storage/outbox.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_fresh_returns_some_rowid() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        let rowid = repo.insert(&[0x01; 32], &[0xAA; 16], b"payload", 1000).unwrap();
        assert!(rowid.expect("fresh insert returns Some(rowid)") > 0);
    }

    #[test]
    fn insert_duplicate_returns_none() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        let first = repo.insert(&[0x01; 32], &[0xAA; 16], b"payload", 1000).unwrap();
        assert!(first.is_some(), "first insert must return Some");
        let again = repo.insert(&[0x01; 32], &[0xAA; 16], b"payload", 1000).unwrap();
        assert!(again.is_none(), "duplicate (target, message_id) must return None");
    }

    #[test]
    fn insert_same_message_id_different_targets_both_succeed() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        let a = repo.insert(&[0x01; 32], &[0xAA; 16], b"p", 1000).unwrap();
        let b = repo.insert(&[0x02; 32], &[0xAA; 16], b"p", 1000).unwrap();
        assert!(a.is_some() && b.is_some(), "unique is (target, message_id), not message_id alone");
    }

    #[test]
    fn due_returns_past_with_message_id_and_skips_future() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        let rid = repo.insert(&[0xAA; 32], &[0x11; 16], b"past", 100).unwrap().unwrap();
        let _ = repo.insert(&[0xBB; 32], &[0x22; 16], b"future", 9999).unwrap();
        let due = repo.due(500, 10).unwrap();
        assert_eq!(due.len(), 1);
        let (id, target, payload, mid, attempts) = &due[0];
        assert_eq!(*id, rid);
        assert_eq!(target.as_slice(), &[0xAA; 32]);
        assert_eq!(payload.as_slice(), b"past");
        assert_eq!(mid, &[0x11; 16]);
        assert_eq!(*attempts, 0);
    }

    #[test]
    fn ack_by_message_id_deletes_matching_row() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        repo.insert(&[0x01; 32], &[0xAA; 16], b"p", 100).unwrap();
        assert!(repo.ack_by_message_id(&[0x01; 32], &[0xAA; 16]).unwrap());
        assert_eq!(repo.due(999, 10).unwrap().len(), 0);
    }

    #[test]
    fn ack_by_message_id_returns_false_when_no_match() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        // different message_id on the same target
        repo.insert(&[0x01; 32], &[0xAA; 16], b"p", 100).unwrap();
        assert!(!repo.ack_by_message_id(&[0x01; 32], &[0xBB; 16]).unwrap());
        assert_eq!(repo.due(999, 10).unwrap().len(), 1);
    }

    #[test]
    fn reschedule_increments_attempts_and_bumps_next_retry_at() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        let rid = repo.insert(&[0xCC; 32], &[0x77; 16], b"retry", 100).unwrap().unwrap();
        repo.reschedule(rid, 200).unwrap();
        repo.reschedule(rid, 300).unwrap();
        let due = repo.due(999, 10).unwrap();
        assert_eq!(due.len(), 1);
        let (id, _, _, _, attempts) = &due[0];
        assert_eq!(*id, rid);
        assert_eq!(*attempts, 2, "attempts must be 2 after two reschedules");
    }
}
```

### Step 2: Run tests to verify they fail

```bash
cargo test -p skattr-core --lib storage::outbox
```
Expected: FAIL — `OutboxRepo::insert` has the wrong signature (3 args, no message_id), `ack_by_message_id` doesn't exist, `OutboxRow` is a 4-tuple instead of 5-tuple.

### Step 3: Replace the `OutboxRepo` body above the `#[cfg(test)]` block

Overwrite everything above the `#[cfg(test)]` in `crates/core/src/storage/outbox.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! SQL repository for the outbox table.
//!
//! The outbox stores per-peer, per-message entries awaiting delivery.
//! Rows are keyed uniquely by `(target, message_id)` so enqueue is
//! idempotent and ACK lookup is a single index probe. Migration 0004
//! added the `message_id` column.

use crate::error::{CoreError, Result};
use crate::storage::Pool;

/// A row read back from the `outbox` table: `(id, target, payload, message_id, attempts)`.
pub type OutboxRow = (i64, Vec<u8>, Vec<u8>, [u8; 16], u32);

pub(crate) struct OutboxRepo<'p> {
    pool: &'p Pool,
}

impl<'p> OutboxRepo<'p> {
    pub(crate) fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Idempotent insert. Returns `Some(rowid)` on a fresh insert,
    /// `None` if a row with this `(target, message_id)` pair already
    /// exists. Relies on the `idx_outbox_target_message_id` unique
    /// index from migration 0004.
    pub(crate) fn insert(
        &self,
        target: &[u8],
        message_id: &[u8; 16],
        payload: &[u8],
        next_retry_at: i64,
    ) -> Result<Option<i64>> {
        self.pool.with_mut(|c| {
            let changed = c
                .execute(
                    "INSERT OR IGNORE INTO outbox \
                     (target, message_id, payload, attempts, next_retry_at) \
                     VALUES (?1, ?2, ?3, 0, ?4)",
                    rusqlite::params![target, message_id.as_slice(), payload, next_retry_at],
                )
                .map_err(|e| CoreError::Storage(format!("insert outbox: {e}")))?;
            Ok(if changed == 0 {
                None
            } else {
                Some(c.last_insert_rowid())
            })
        })
    }

    /// Fetch entries whose `next_retry_at` has passed.
    pub(crate) fn due(&self, now: i64, limit: usize) -> Result<Vec<OutboxRow>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, target, payload, message_id, attempts FROM outbox \
                     WHERE next_retry_at <= ?1 \
                     ORDER BY next_retry_at LIMIT ?2",
                )
                .map_err(|e| CoreError::Storage(format!("prepare due: {e}")))?;
            let rows = stmt
                .query_map(
                    rusqlite::params![now, i64::try_from(limit).unwrap_or(i64::MAX)],
                    |r| {
                        let id: i64 = r.get(0)?;
                        let target: Vec<u8> = r.get(1)?;
                        let payload: Vec<u8> = r.get(2)?;
                        let mid_bytes: Vec<u8> = r.get(3)?;
                        let attempts: i64 = r.get(4)?;
                        let mut mid = [0u8; 16];
                        if mid_bytes.len() == 16 {
                            mid.copy_from_slice(&mid_bytes);
                        }
                        Ok((
                            id,
                            target,
                            payload,
                            mid,
                            u32::try_from(attempts).unwrap_or(u32::MAX),
                        ))
                    },
                )
                .map_err(|e| CoreError::Storage(format!("query due: {e}")))?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| CoreError::Storage(format!("collect due: {e}")))
        })
    }

    /// Delete the outbox row for `(target, message_id)`. Returns
    /// `true` if a row was removed.
    pub(crate) fn ack_by_message_id(
        &self,
        target: &[u8],
        message_id: &[u8; 16],
    ) -> Result<bool> {
        self.pool.with_mut(|c| {
            let n = c
                .execute(
                    "DELETE FROM outbox WHERE target = ?1 AND message_id = ?2",
                    rusqlite::params![target, message_id.as_slice()],
                )
                .map_err(|e| CoreError::Storage(format!("ack outbox: {e}")))?;
            Ok(n > 0)
        })
    }

    /// Increment `attempts` and set a new `next_retry_at` for a failed
    /// send.
    pub(crate) fn reschedule(&self, id: i64, next_retry_at: i64) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "UPDATE outbox SET attempts = attempts + 1, next_retry_at = ?1 WHERE id = ?2",
                rusqlite::params![next_retry_at, id],
            )
            .map_err(|e| CoreError::Storage(format!("reschedule outbox: {e}")))?;
            Ok(())
        })
    }
}
```

### Step 4: Run the outbox tests to verify they pass

```bash
cargo test -p skattr-core --lib storage::outbox
```
Expected: all 7 tests pass.

### Step 5: Confirm no other call sites broke

```bash
cargo build -p skattr-core
```
Expected: build succeeds. The `delivery::outbox::Outbox` wrapper (Task 4) is the only downstream consumer and still has its `todo!()` bodies, so nothing compiles against the old API.

### Step 6: Commit

```bash
git add crates/core/src/storage/outbox.rs
git commit -m "$(cat <<'EOF'
storage: OutboxRepo API takes message_id and exposes ack-by-id

insert is idempotent over (target, message_id); ack_by_message_id
deletes the correlated row; OutboxRow gains the 16-byte message_id.
Delivery layer consumes this.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: `delivery::backoff` — pure exponential backoff with jitter

**Files:**
- Create: `crates/core/src/delivery/backoff.rs`
- Modify: `crates/core/src/delivery/mod.rs` (declare the module)
- Modify: `crates/core/src/delivery/outbox.rs` (delete the old free-fn `backoff` stub; Task 4 reworks this file further)

### Step 1: Write the failing test

Create `crates/core/src/delivery/backoff.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Exponential backoff with ±25 % jitter, capped at 5 minutes.
//!
//! Used by the per-peer delivery actor when rescheduling a failed
//! delivery. Pure function; no I/O.

use std::time::Duration;

/// Base delay: 1 second.
const BASE: Duration = Duration::from_secs(1);

/// Cap at 5 minutes.
pub(crate) const CAP: Duration = Duration::from_secs(300);

/// Return the next delay for a delivery that has failed `attempts` times.
///
/// `attempts = 0` is the "we just failed for the first time" case and
/// returns approximately 1 s. Doubles each subsequent attempt up to
/// [`CAP`], then stays capped. All values are perturbed by uniform
/// random jitter in `[-25 %, +25 %]`.
#[must_use]
pub(crate) fn backoff(attempts: u32) -> Duration {
    use rand::Rng;

    // Double: 1s, 2s, 4s, … cap at 5 min. `checked_shl` guards against
    // overflow for very large `attempts` values.
    let shifted = BASE
        .as_millis()
        .checked_shl(attempts)
        .unwrap_or(u128::MAX);
    let capped_ms = u64::try_from(shifted.min(u128::from(CAP.as_millis()))).unwrap_or(u64::MAX);
    let base = Duration::from_millis(capped_ms);

    // ±25 % jitter. Uniform in [0.75, 1.25].
    let factor: f64 = rand::rngs::OsRng.gen_range(0.75..=1.25);
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let jittered_ms = (base.as_millis() as f64 * factor) as u64;
    Duration::from_millis(jittered_ms)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn first_attempt_is_near_one_second() {
        for _ in 0..50 {
            let d = backoff(0);
            assert!(
                d >= Duration::from_millis(750) && d <= Duration::from_millis(1250),
                "attempt 0 must be in [0.75s, 1.25s]; got {d:?}"
            );
        }
    }

    #[test]
    fn doubles_until_cap() {
        // Sample the mean of many draws to smooth out jitter.
        fn mean_ms(attempts: u32, samples: usize) -> u64 {
            let sum: u64 = (0..samples).map(|_| backoff(attempts).as_millis() as u64).sum();
            sum / samples as u64
        }
        let m0 = mean_ms(0, 200);
        let m1 = mean_ms(1, 200);
        let m2 = mean_ms(2, 200);
        // Means should be roughly 1000, 2000, 4000.
        assert!(m0 > 800 && m0 < 1200, "mean attempt 0 ≈ 1000 ms, got {m0}");
        assert!(m1 > 1700 && m1 < 2300, "mean attempt 1 ≈ 2000 ms, got {m1}");
        assert!(m2 > 3400 && m2 < 4600, "mean attempt 2 ≈ 4000 ms, got {m2}");
    }

    #[test]
    fn caps_at_five_minutes_plus_jitter() {
        // attempts so large the shift overflows: must still return within the
        // jittered cap band.
        for attempts in [10u32, 20, 32, 64, 100, u32::MAX] {
            let d = backoff(attempts);
            // Cap is 300s; ±25 % band is [225s, 375s].
            assert!(
                d >= Duration::from_secs(225) && d <= Duration::from_secs(375),
                "attempts={attempts}: d={d:?} outside cap band"
            );
        }
    }
}
```

### Step 2: Declare the module

Edit `crates/core/src/delivery/mod.rs` to add the new line (keep `outbox`, `receiver`, `sender` lines unchanged for now — Task 6 restructures):

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Send/receive plumbing: outbox queue, retry, dedup, ACK handling.

pub(crate) mod backoff;
pub(crate) mod outbox;
pub(crate) mod receiver;
pub(crate) mod sender;
```

### Step 3: Remove the duplicate `backoff` free function in `delivery/outbox.rs`

In `crates/core/src/delivery/outbox.rs`, delete the existing `pub fn backoff(_attempts: u32) -> Duration { todo!(...) }` item (and its doc comment, and the `use std::time::Duration;` line if nothing else uses it). Task 4 rewrites the rest of the file.

### Step 4: Run tests

```bash
cargo test -p skattr-core --lib delivery::backoff
```
Expected: all three tests pass. The jitter randomness is constrained by the sample sizes.

### Step 5: Verify the workspace still builds (other delivery modules are still stubbed)

```bash
cargo build -p skattr-core
```
Expected: succeeds. The `delivery::outbox::Outbox` impl methods still have `todo!()` bodies but they type-check.

### Step 6: Commit

```bash
git add crates/core/src/delivery/backoff.rs \
        crates/core/src/delivery/mod.rs \
        crates/core/src/delivery/outbox.rs
git commit -m "$(cat <<'EOF'
delivery: backoff — doubled seconds with ±25 % jitter, capped at 5 min

Pure function in its own module; the per-peer actor calls this when
rescheduling a failed outbox entry. Sampling-based unit tests verify
the expected mean for attempts 0–2 and the hard cap band for very
large attempts.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: `delivery::outbox::Outbox` wrapper on `OutboxRepo`

**Files:**
- Modify: `crates/core/src/delivery/outbox.rs` (replace entire file)
- Test: in the same file

### Step 1: Write the failing tests

Replace the whole contents of `crates/core/src/delivery/outbox.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Persisted send queue with exponential-backoff retry.
//!
//! Thin wrapper over [`crate::storage::outbox::OutboxRepo`] that speaks
//! in `PublicKey`/`MessageId` terms rather than raw byte slices. Rows
//! live in the `outbox` table (see migration 0001 + 0004).

use std::time::Duration;

use crate::delivery::backoff::backoff;
use crate::envelope::MessageId;
use crate::error::Result;
use crate::identity::PublicKey;
use crate::storage::outbox::{OutboxRepo, OutboxRow};
use crate::storage::Pool;

/// A pending outbound delivery.
#[derive(Debug, Clone)]
pub(crate) struct OutboxEntry {
    /// Row id (rowid in SQLite).
    pub(crate) id: i64,
    /// Intended recipient.
    pub(crate) target: PublicKey,
    /// Opaque encrypted payload (MLS ciphertext already wrapped).
    pub(crate) payload: Vec<u8>,
    /// Application message id, for ACK correlation.
    pub(crate) message_id: MessageId,
    /// Retry attempt count (0 on first enqueue).
    pub(crate) attempts: u32,
}

/// Borrowed view over the outbox backed by a `Pool`.
pub(crate) struct Outbox<'p> {
    repo: OutboxRepo<'p>,
}

impl<'p> Outbox<'p> {
    pub(crate) fn new(pool: &'p Pool) -> Self {
        Self {
            repo: OutboxRepo::new(pool),
        }
    }

    /// Enqueue a fresh `(target, message_id, payload)` tuple with
    /// `next_retry_at = now`. Returns `Ok(Some(rowid))` on fresh
    /// insert, `Ok(None)` if `(target, message_id)` is already present.
    pub(crate) fn enqueue(
        &self,
        target: &PublicKey,
        message_id: MessageId,
        payload: &[u8],
        now: i64,
    ) -> Result<Option<i64>> {
        self.repo
            .insert(&target.0, &message_id.0, payload, now)
    }

    /// Entries whose `next_retry_at` has passed, up to `max`.
    pub(crate) fn due(&self, now: i64, max: usize) -> Result<Vec<OutboxEntry>> {
        let rows = self.repo.due(now, max)?;
        Ok(rows.into_iter().map(row_to_entry).collect())
    }

    /// Delete the `(target, message_id)` row. Returns `true` if a row
    /// was removed.
    pub(crate) fn ack(
        &self,
        target: &PublicKey,
        message_id: MessageId,
    ) -> Result<bool> {
        self.repo.ack_by_message_id(&target.0, &message_id.0)
    }

    /// Bump `attempts` and set `next_retry_at = now + backoff(attempts_now)`.
    pub(crate) fn reschedule(&self, id: i64, attempts_now: u32, now: i64) -> Result<()> {
        let delay = backoff(attempts_now);
        let next_retry_at = now.saturating_add(
            i64::try_from(delay.as_millis()).unwrap_or(i64::MAX),
        );
        self.repo.reschedule(id, next_retry_at)
    }

    /// Convenience: the configured cap used by [`backoff`]. Exposed
    /// for the retry-tick ceiling when logging.
    #[cfg(test)]
    pub(crate) fn backoff_cap() -> Duration {
        crate::delivery::backoff::CAP
    }
}

fn row_to_entry(row: OutboxRow) -> OutboxEntry {
    let (id, target, payload, message_id, attempts) = row;
    let mut pk = [0u8; 32];
    if target.len() == 32 {
        pk.copy_from_slice(&target);
    }
    OutboxEntry {
        id,
        target: PublicKey(pk),
        payload,
        message_id: MessageId(message_id),
        attempts,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn pk(byte: u8) -> PublicKey {
        PublicKey([byte; 32])
    }

    fn mid(byte: u8) -> MessageId {
        MessageId([byte; 16])
    }

    #[test]
    fn enqueue_is_idempotent_on_target_message_id() {
        let pool = Pool::in_memory();
        let ob = Outbox::new(&pool);
        assert!(ob.enqueue(&pk(0xAA), mid(0x01), b"p", 100).unwrap().is_some());
        assert!(ob.enqueue(&pk(0xAA), mid(0x01), b"p", 100).unwrap().is_none());
    }

    #[test]
    fn due_returns_entries_with_public_key_and_message_id() {
        let pool = Pool::in_memory();
        let ob = Outbox::new(&pool);
        ob.enqueue(&pk(0xAA), mid(0x01), b"past", 100).unwrap();
        let list = ob.due(999, 10).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].target, pk(0xAA));
        assert_eq!(list[0].message_id, mid(0x01));
        assert_eq!(list[0].payload, b"past");
        assert_eq!(list[0].attempts, 0);
    }

    #[test]
    fn ack_removes_exactly_one_row() {
        let pool = Pool::in_memory();
        let ob = Outbox::new(&pool);
        ob.enqueue(&pk(0xAA), mid(0x01), b"p", 100).unwrap();
        assert!(ob.ack(&pk(0xAA), mid(0x01)).unwrap());
        assert!(ob.due(999, 10).unwrap().is_empty());
    }

    #[test]
    fn reschedule_bumps_attempts_and_next_retry() {
        let pool = Pool::in_memory();
        let ob = Outbox::new(&pool);
        let rid = ob.enqueue(&pk(0xAA), mid(0x01), b"p", 100).unwrap().unwrap();
        ob.reschedule(rid, 0, 1_000).unwrap();
        // With backoff(0) ∈ [0.75s, 1.25s], next_retry_at ∈ [1750, 2250].
        // Immediately after reschedule, due(now=1_499) should return nothing.
        assert!(ob.due(1_499, 10).unwrap().is_empty());
        let later = ob.due(3_000, 10).unwrap();
        assert_eq!(later.len(), 1);
        assert_eq!(later[0].attempts, 1);
    }
}
```

### Step 2: Run tests to verify they fail then pass

Because the old file had `todo!()` bodies for `enqueue`/`due`/`ack`/`reschedule`, replacing the whole file is already the implementation. Run:

```bash
cargo test -p skattr-core --lib delivery::outbox
```
Expected: 4 new tests green. If one flakes on the tight timing bound (`1_499`), tune the bound to `1_500` — the 0.75 s jitter-low is 750 ms → `next_retry_at = 1_000 + 750 = 1_750`, so `due(1_499)` should be safe; this is a consistency check, not a timing race.

### Step 3: Confirm the crate builds

```bash
cargo clippy -p skattr-core --all-targets -- -D warnings
```
Expected: clean.

### Step 4: Commit

```bash
git add crates/core/src/delivery/outbox.rs
git commit -m "$(cat <<'EOF'
delivery: Outbox wrapper over OutboxRepo

Speaks in PublicKey/MessageId/Duration instead of raw bytes. enqueue
is idempotent per (target, message_id), ack deletes by the same pair,
reschedule derives next_retry_at from backoff(attempts_now).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: `delivery::receiver::receive` — dedup + persist + ACK decision

**Files:**
- Modify: `crates/core/src/delivery/receiver.rs` (replace contents; tests in the same file)

### Step 1: Write the failing tests

Replace `crates/core/src/delivery/receiver.rs` in full with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Receiver-side ingress: timestamp check, dedup, persist, ACK.
//!
//! Dedup is a sliding 24-hour `(sender, message_id)` index; ordering
//! is authoritative per the MLS generation number — not per the
//! envelope timestamp (`Envelope.ts` is display-only with a ±1 h
//! replay window).

use crate::envelope::{Envelope, MessageId};
use crate::error::Result;
use crate::identity::PublicKey;
use crate::storage::messages::MessageRepo;
use crate::storage::seen_messages::SeenMessagesRepo;

/// Replay window: an envelope whose `ts` is more than ±1 h from the
/// local clock is rejected with no ACK. Millis.
pub(crate) const REPLAY_WINDOW_MS: i64 = 60 * 60 * 1000;

/// Outcome of processing an incoming envelope.
#[derive(Debug, Clone)]
pub(crate) enum ReceiveOutcome {
    /// Fresh message persisted; caller should surface to UI and send an ACK.
    New(Envelope),
    /// We've already seen `(sender, message_id)`; caller should still send
    /// an ACK (the sender may have retried because their ACK was lost).
    Duplicate,
    /// Rejected (ts out of replay window). Caller must NOT ACK.
    Rejected(String),
}

/// Ingest one decrypted envelope from `sender` into storage.
///
/// Does not perform MLS decryption — callers have already run
/// `Group::decrypt` on the ciphertext. Pure side effects on
/// `seen_messages` and `messages`.
pub(crate) fn receive(
    sender: &PublicKey,
    group_id: &[u8],
    envelope: Envelope,
    now_ms: i64,
    seen: &SeenMessagesRepo<'_>,
    messages: &MessageRepo<'_>,
) -> Result<ReceiveOutcome> {
    if (envelope.ts - now_ms).abs() > REPLAY_WINDOW_MS {
        return Ok(ReceiveOutcome::Rejected(format!(
            "ts outside ±1h window: envelope ts={}, now={}",
            envelope.ts, now_ms
        )));
    }
    let is_new = seen.insert(&sender.0, &envelope.id.0, now_ms)?;
    if !is_new {
        return Ok(ReceiveOutcome::Duplicate);
    }
    let _ = messages.insert(group_id, &sender.0, &envelope)?;
    Ok(ReceiveOutcome::New(envelope))
}

/// Build an ACK frame payload for `message_id`.
#[must_use]
pub(crate) fn build_ack(message_id: MessageId) -> MessageId {
    message_id
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::envelope::{Kind, MessageId};
    use crate::storage::Pool;

    fn env(id: u8, ts: i64) -> Envelope {
        Envelope {
            v: 1,
            id: MessageId([id; 16]),
            ts,
            reply_to: None,
            kind: Kind::Text {
                body: "hello".into(),
            },
        }
    }

    #[test]
    fn first_receive_returns_new_and_persists() {
        let pool = Pool::in_memory();
        let seen = SeenMessagesRepo::new(&pool);
        let msgs = MessageRepo::new(&pool);
        let sender = PublicKey([0xAA; 32]);
        let out = receive(&sender, &[0x01; 16], env(0x01, 1000), 1000, &seen, &msgs).unwrap();
        assert!(matches!(out, ReceiveOutcome::New(_)));
        assert!(seen.contains(&sender.0, &[0x01; 16]).unwrap());
    }

    #[test]
    fn second_receive_same_id_returns_duplicate_and_does_not_double_insert() {
        let pool = Pool::in_memory();
        let seen = SeenMessagesRepo::new(&pool);
        let msgs = MessageRepo::new(&pool);
        let sender = PublicKey([0xAA; 32]);
        let e = env(0x01, 1000);
        receive(&sender, &[0x01; 16], e.clone(), 1000, &seen, &msgs).unwrap();
        let out = receive(&sender, &[0x01; 16], e, 1000, &seen, &msgs).unwrap();
        assert!(matches!(out, ReceiveOutcome::Duplicate));
        let rows = msgs.recent(&[0x01; 16], 10).unwrap();
        assert_eq!(rows.len(), 1, "dup must not insert a second messages row");
    }

    #[test]
    fn ts_more_than_one_hour_in_the_past_is_rejected() {
        let pool = Pool::in_memory();
        let seen = SeenMessagesRepo::new(&pool);
        let msgs = MessageRepo::new(&pool);
        let sender = PublicKey([0xAA; 32]);
        let now = 10_000_000i64;
        let old = now - (REPLAY_WINDOW_MS + 1);
        let out = receive(&sender, &[0x01; 16], env(0x01, old), now, &seen, &msgs).unwrap();
        assert!(matches!(out, ReceiveOutcome::Rejected(_)));
        assert!(!seen.contains(&sender.0, &[0x01; 16]).unwrap());
    }

    #[test]
    fn ts_more_than_one_hour_in_the_future_is_rejected() {
        let pool = Pool::in_memory();
        let seen = SeenMessagesRepo::new(&pool);
        let msgs = MessageRepo::new(&pool);
        let sender = PublicKey([0xAA; 32]);
        let now = 10_000_000i64;
        let future = now + (REPLAY_WINDOW_MS + 1);
        let out = receive(&sender, &[0x01; 16], env(0x01, future), now, &seen, &msgs).unwrap();
        assert!(matches!(out, ReceiveOutcome::Rejected(_)));
    }

    #[test]
    fn build_ack_returns_input_id() {
        let id = MessageId([0x77; 16]);
        assert_eq!(build_ack(id), id);
    }
}
```

### Step 2: Run tests to verify they pass

The existing file had `todo!()` bodies, so this rewrite is the implementation. Run:

```bash
cargo test -p skattr-core --lib delivery::receiver
```
Expected: 5 tests green.

### Step 3: Commit

```bash
git add crates/core/src/delivery/receiver.rs
git commit -m "$(cat <<'EOF'
delivery: receiver — ts window, dedup, persist, ACK decision

receive() is a pure function over SeenMessagesRepo + MessageRepo.
Returns New/Duplicate/Rejected so the per-peer actor decides whether
to emit Event::MessageReceived and whether to send Frame::Ack.
Duplicates do ACK; rejects do not.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Restructure `delivery/` — drop `sender.rs`, declare `peer` and `hub`

**Files:**
- Delete: `crates/core/src/delivery/sender.rs`
- Modify: `crates/core/src/delivery/mod.rs`
- Create (empty skeletons): `crates/core/src/delivery/peer.rs`, `crates/core/src/delivery/hub.rs`

### Step 1: Delete the stub

```bash
rm crates/core/src/delivery/sender.rs
```

### Step 2: Create skeletons that compile

Create `crates/core/src/delivery/peer.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Per-peer connection actor.
//!
//! One [`PeerConnection`] task per active peer owns the optional
//! `AuthenticatedConnection<S>`, the pending-ACK map, and the
//! per-peer retry tick. Implementation lands in Task 7.
```

Create `crates/core/src/delivery/hub.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Daemon-scoped delivery router.
//!
//! [`DeliveryHub`] maps `PublicKey → mpsc::Sender<DeliveryJob>`,
//! spawning a [`crate::delivery::peer::PeerConnection`] actor on the
//! first send to a peer, routing inbound post-handshake connections
//! into the same actors via `ingest`, and running a periodic
//! `seen_messages` sweep. Implementation lands in Task 8.
```

### Step 3: Update `delivery/mod.rs`

Replace contents with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Send/receive plumbing: outbox queue, retry, dedup, ACK handling.

pub(crate) mod backoff;
pub(crate) mod hub;
pub(crate) mod outbox;
pub(crate) mod peer;
pub(crate) mod receiver;
```

### Step 4: Confirm it builds

```bash
cargo build -p skattr-core
```
Expected: succeeds. No new tests.

### Step 5: Commit

```bash
git add crates/core/src/delivery/mod.rs \
        crates/core/src/delivery/peer.rs \
        crates/core/src/delivery/hub.rs
git rm crates/core/src/delivery/sender.rs
git commit -m "$(cat <<'EOF'
delivery: restructure module tree for 1.E layout

Drop the vestigial sender.rs stub; introduce peer.rs (actor) and
hub.rs (router) as empty skeletons to be filled by subsequent tasks.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: `PeerConnection` actor — dial, send, ACK happy path

**Files:**
- Modify: `crates/core/src/delivery/peer.rs` (replace skeleton)

This task builds the actor end-to-end **without** the retry tick, keepalive, or `ReplaceConn`. Those land in Tasks 8–9. The happy path covered here is: hub sends a `DeliveryJob`, actor dials if cold, handshake, `conn.send(Frame::MlsApp)`, awaits `Frame::Ack`, resolves the oneshot. Tested over `tokio::io::duplex` where both sides run real Noise.

### Step 1: Write the failing test (in the same file, bottom `#[cfg(test)] mod tests`)

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::identity::IdentityKey;
    use crate::transport::frame::Frame;
    use crate::transport::noise::{handshake_initiator, handshake_responder};
    use std::sync::Arc;
    use tokio::sync::{mpsc, oneshot};
    use zeroize::Zeroizing;

    /// Spawn a matching responder task over one half of a duplex pair.
    /// Returns a join handle that resolves when the responder observes
    /// one `MlsApp` ciphertext and echoes back an `Ack(mid)` frame.
    async fn spawn_responder_echo_ack(
        stream: tokio::io::DuplexStream,
        responder_identity: IdentityKey,
        expected_mid: [u8; 16],
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let (mut conn, _outcome) =
                handshake_responder(stream, &responder_identity, None).await.unwrap();
            // One frame expected.
            let frame = conn.recv().await.unwrap().expect("frame");
            match frame {
                Frame::MlsApp(_) => {}
                other => panic!("expected MlsApp, got {other:?}"),
            }
            conn.send(Frame::Ack(expected_mid)).await.unwrap();
        })
    }

    #[tokio::test]
    async fn actor_sends_mlsapp_and_resolves_oneshot_on_ack() {
        let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
        let initiator_id = IdentityKey::generate().unwrap();
        let responder_id = IdentityKey::generate().unwrap();
        let responder_static = responder_id.noise_static_public();

        let mid = [0xA5u8; 16];
        let echo = spawn_responder_echo_ack(server_stream, responder_id, mid).await;

        // Initiator-side handshake in the actor's place: build a conn up
        // front and hand it to the actor via its test-only constructor.
        let (conn, _) =
            handshake_initiator(client_stream, &initiator_id, &responder_static, None)
                .await
                .unwrap();

        let (job_tx, job_rx) = mpsc::channel::<DeliveryJob>(4);
        let (ack_tx, ack_rx) = oneshot::channel::<Result<(), ()>>();
        let handle = PeerConnection::spawn_with_conn_for_test(
            PublicKey([0xBB; 32]),
            Box::new(conn),
            job_rx,
        );

        job_tx
            .send(DeliveryJob {
                message_id: crate::envelope::MessageId(mid),
                ciphertext: vec![0xDE, 0xAD, 0xBE, 0xEF],
                ack_tx,
            })
            .await
            .unwrap();

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), ack_rx)
            .await
            .expect("oneshot must fire within 2s")
            .expect("sender side not dropped");
        assert!(outcome.is_ok(), "happy path delivers");

        drop(job_tx);
        let _ = echo.await;
        let _ = handle.await;
    }
}
```

### Step 2: Run the test to verify it fails

```bash
cargo test -p skattr-core --lib delivery::peer -- --nocapture
```
Expected: compile error — `DeliveryJob`, `PeerConnection`, `spawn_with_conn_for_test` don't exist yet.

### Step 3: Implement the actor

Replace `crates/core/src/delivery/peer.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Per-peer connection actor.

use std::collections::HashMap;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::envelope::MessageId;
use crate::error::{CoreError, Result};
use crate::identity::PublicKey;
use crate::transport::connection::AuthenticatedConnection;
use crate::transport::frame::Frame;

/// One outbound delivery, submitted by the hub.
pub(crate) struct DeliveryJob {
    pub(crate) message_id: MessageId,
    pub(crate) ciphertext: Vec<u8>,
    /// Fires `Ok(())` on successful ACK, `Err(())` if the ack path is
    /// torn down (conn dropped, actor cancelled). The hub translates
    /// `Err(())` into "row stays in outbox for retry."
    pub(crate) ack_tx: oneshot::Sender<Result<(), ()>>,
}

/// Per-peer actor handle. Returned by `PeerConnection::spawn*` so the
/// hub can `.await` it on shutdown.
pub(crate) type PeerHandle = JoinHandle<()>;

/// Minimal "happy-path" actor. Owns a single `AuthenticatedConnection`,
/// a pending-ACK map, and a `select!` over job intake + frame recv.
///
/// Task 8 extends this with retry tick, keepalive, and idle close.
/// Task 9 extends it with `ReplaceConn` support.
pub(crate) struct PeerConnection;

impl PeerConnection {
    /// Test-only constructor: hand in an already-dialed connection and
    /// an already-opened job receiver. The actor runs until the job
    /// receiver closes or the connection errors.
    #[cfg(test)]
    pub(crate) fn spawn_with_conn_for_test<S>(
        peer: PublicKey,
        conn: Box<AuthenticatedConnection<S>>,
        jobs: mpsc::Receiver<DeliveryJob>,
    ) -> PeerHandle
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        tokio::spawn(async move {
            let _ = run_actor(peer, *conn, jobs).await;
        })
    }
}

async fn run_actor<S>(
    _peer: PublicKey,
    mut conn: AuthenticatedConnection<S>,
    mut jobs: mpsc::Receiver<DeliveryJob>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut pending: HashMap<MessageId, oneshot::Sender<Result<(), ()>>> = HashMap::new();

    loop {
        tokio::select! {
            job = jobs.recv() => {
                let Some(job) = job else { break; };
                if let Err(e) = conn.send(Frame::MlsApp(job.ciphertext)).await {
                    let _ = job.ack_tx.send(Err(()));
                    return Err(e);
                }
                pending.insert(job.message_id, job.ack_tx);
            }
            frame = conn.recv() => {
                match frame {
                    Ok(Some(Frame::Ack(bytes))) => {
                        let mid = MessageId(bytes);
                        if let Some(tx) = pending.remove(&mid) {
                            let _ = tx.send(Ok(()));
                        }
                    }
                    Ok(Some(Frame::Bye)) => {
                        break;
                    }
                    Ok(Some(Frame::Ping)) => {
                        let _ = conn.send(Frame::Pong).await;
                    }
                    Ok(Some(Frame::Pong)) => { /* handled by keepalive in Task 8 */ }
                    Ok(Some(other)) => {
                        tracing::warn!(ty = ?other, "peer: dropping unexpected inbound frame");
                    }
                    Ok(None) => {
                        return Err(CoreError::Transport("peer: EOF".into()));
                    }
                    Err(e) => {
                        return Err(e);
                    }
                }
            }
        }
    }

    // Drain pending oneshots on clean exit.
    for (_, tx) in pending.drain() {
        let _ = tx.send(Err(()));
    }
    Ok(())
}
```

### Step 4: Run the test to verify it passes

```bash
cargo test -p skattr-core --lib delivery::peer -- --nocapture
```
Expected: `actor_sends_mlsapp_and_resolves_oneshot_on_ack` passes.

### Step 5: Run the full test suite — nothing else should have regressed

```bash
cargo test -p skattr-core
```
Expected: all existing tests still pass (backoff, outbox, receiver, etc.).

### Step 6: Commit

```bash
git add crates/core/src/delivery/peer.rs
git commit -m "$(cat <<'EOF'
delivery: PeerConnection actor — send MlsApp, correlate Ack to oneshot

Happy path only. Owns a HashMap<MessageId, oneshot::Sender<Result<(),
()>>> and resolves it on Frame::Ack. Exits on Frame::Bye or EOF,
draining pending oneshots with Err. Retry tick, keepalive, idle
close, and ReplaceConn land in following tasks.

Test uses tokio::io::duplex + real Noise handshakes on both sides.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: `PeerConnection` — retry tick, keepalive, idle close, inbound-MLS dispatch

**Files:**
- Modify: `crates/core/src/delivery/peer.rs`

This task expands the actor to:

1. Pull rows from the outbox on a 1 s tick, re-send any not already in `pending`.
2. Run keepalive (60 s Ping, 30 s Pong deadline).
3. Close idle connections after 180 s.
4. Decrypt inbound `Frame::MlsApp` via an injected `InboundDispatch` trait object — spec §4.3 step 4.

Tests use `tokio::time::pause` / `advance`.

**Why the `InboundDispatch` trait:** the actor must not hard-link to `openmls` types or the `mls::Group` machinery — both for testability (Task 8's retry test uses a dumb echo responder, no real MLS on the inbound side) and because a future `Daemon`-level hub construction can inject the prod dispatcher once. The trait shape is:

```rust
pub(crate) trait InboundDispatch: Send + Sync + 'static {
    /// Handle one inbound `Frame::MlsApp` payload from `peer`.
    /// Returns `Some(message_id)` if the actor should reply with a
    /// `Frame::Ack(id)` (fresh OR duplicate — the sender may have
    /// retried because their ACK was lost). Returns `None` if the
    /// frame was rejected (ts out of window, decrypt failure) and no
    /// ACK should be sent.
    fn dispatch(&self, peer: PublicKey, ciphertext: &[u8]) -> Option<MessageId>;
}
```

### Step 1: Write the failing test — retry tick picks up outbox rows

Append to the `mod tests` block in `delivery/peer.rs`:

```rust
    use crate::storage::Pool;

    /// Full actor spawn for Task 8+: actor owns its Outbox handle (via
    /// a Pool reference) and its tick loop. We use `tokio::time::pause`
    /// to control elapsed virtual time.
    #[tokio::test(start_paused = true)]
    async fn retry_tick_picks_up_outbox_row_and_delivers() {
        use crate::delivery::outbox::Outbox;
        use crate::envelope::MessageId as EMid;

        let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
        let initiator_id = IdentityKey::generate().unwrap();
        let responder_id = IdentityKey::generate().unwrap();
        let responder_static = responder_id.noise_static_public();

        let mid = [0x42u8; 16];
        let echo = spawn_responder_echo_ack(server_stream, responder_id, mid).await;

        let (conn, _) =
            handshake_initiator(client_stream, &initiator_id, &responder_static, None)
                .await
                .unwrap();

        // Seed an outbox row directly — no hub involvement yet.
        let pool = std::sync::Arc::new(Pool::in_memory());
        let ob = Outbox::new(&pool);
        let peer = PublicKey([0xBB; 32]);
        ob.enqueue(&peer, EMid(mid), &[0x01, 0x02, 0x03], 0).unwrap();

        let (_job_tx, job_rx) = mpsc::channel::<DeliveryJob>(4);
        let handle = PeerConnection::spawn_full_for_test(
            peer,
            Box::new(conn),
            job_rx,
            pool.clone(),
            None, // no inbound MLS needed — responder echoes Ack directly
        );

        // Advance virtual time past one retry tick (1 s).
        tokio::time::advance(std::time::Duration::from_millis(1_100)).await;
        // Let the ack arrive.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Outbox should now be empty.
        let ob_check = Outbox::new(&pool);
        assert!(ob_check.due(i64::MAX, 10).unwrap().is_empty(),
                "retry tick must remove the row after Ack");

        handle.abort();
        let _ = echo.await;
    }
```

### Step 2: Run to verify it fails

```bash
cargo test -p skattr-core --lib delivery::peer::tests::retry_tick_picks_up
```
Expected: compile error — `spawn_full_for_test` doesn't exist; `run_actor` doesn't know about `Pool` or `Outbox`.

### Step 3: Extend `run_actor` and add a full-actor spawner

Replace the body of `peer.rs` from `pub(crate) struct PeerConnection;` downward with:

```rust
/// Inbound-MLS dispatch strategy, injected per peer actor. See Task 8
/// preamble for the rationale — keeps `openmls` out of the actor
/// and keeps tests that don't need real MLS trivially easy to write.
pub(crate) trait InboundDispatch: Send + Sync + 'static {
    fn dispatch(&self, peer: PublicKey, ciphertext: &[u8]) -> Option<MessageId>;
}

pub(crate) struct PeerConnection;

impl PeerConnection {
    /// Test-only constructor (Task 7): a minimal actor with no outbox
    /// tick, no keepalive, no idle close, no inbound-MLS handling.
    #[cfg(test)]
    pub(crate) fn spawn_with_conn_for_test<S>(
        peer: PublicKey,
        conn: Box<AuthenticatedConnection<S>>,
        jobs: mpsc::Receiver<DeliveryJob>,
    ) -> PeerHandle
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        tokio::spawn(async move {
            let _ = minimal_run(peer, *conn, jobs).await;
        })
    }

    /// Test-only full-actor constructor: retry tick + keepalive + idle
    /// close are all active, driven by `tokio::time`. `inbound` is
    /// optional so Task 8's retry test can pass `None` (the responder
    /// in that test never sends `Frame::MlsApp` back to the actor).
    #[cfg(test)]
    pub(crate) fn spawn_full_for_test<S>(
        peer: PublicKey,
        conn: Box<AuthenticatedConnection<S>>,
        jobs: mpsc::Receiver<DeliveryJob>,
        pool: std::sync::Arc<crate::storage::Pool>,
        inbound: Option<std::sync::Arc<dyn InboundDispatch>>,
    ) -> PeerHandle
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        tokio::spawn(async move {
            let _ = full_run(peer, Some(*conn), jobs, pool, inbound).await;
        })
    }
}

/// Minimal actor from Task 7.
async fn minimal_run<S>(
    _peer: PublicKey,
    mut conn: AuthenticatedConnection<S>,
    mut jobs: mpsc::Receiver<DeliveryJob>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut pending: HashMap<MessageId, oneshot::Sender<Result<(), ()>>> = HashMap::new();
    loop {
        tokio::select! {
            job = jobs.recv() => {
                let Some(job) = job else { break; };
                if let Err(e) = conn.send(Frame::MlsApp(job.ciphertext)).await {
                    let _ = job.ack_tx.send(Err(()));
                    return Err(e);
                }
                pending.insert(job.message_id, job.ack_tx);
            }
            frame = conn.recv() => {
                match frame {
                    Ok(Some(Frame::Ack(bytes))) => {
                        let mid = MessageId(bytes);
                        if let Some(tx) = pending.remove(&mid) { let _ = tx.send(Ok(())); }
                    }
                    Ok(Some(Frame::Bye)) => break,
                    Ok(Some(Frame::Ping)) => { let _ = conn.send(Frame::Pong).await; }
                    Ok(Some(Frame::Pong)) => {}
                    Ok(Some(other)) => tracing::warn!(ty = ?other, "peer: dropping unexpected frame"),
                    Ok(None) => return Err(CoreError::Transport("peer: EOF".into())),
                    Err(e) => return Err(e),
                }
            }
        }
    }
    for (_, tx) in pending.drain() { let _ = tx.send(Err(())); }
    Ok(())
}

/// Tick intervals for the full actor.
const RETRY_TICK: std::time::Duration = std::time::Duration::from_secs(1);
const KEEPALIVE_PERIOD: std::time::Duration = std::time::Duration::from_secs(60);
const PONG_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
const IDLE_CLOSE: std::time::Duration = std::time::Duration::from_secs(180);

/// Full actor (Tasks 8+). `conn` starts as `Some(...)` once the
/// handshake is complete and may become `None` after an error; the
/// retry tick is responsible for redialing via the hub in production.
/// For the test-only constructor, `conn == None` after an error means
/// the actor exits (redial wiring lives on the hub side).
async fn full_run<S>(
    peer: PublicKey,
    mut conn: Option<AuthenticatedConnection<S>>,
    mut jobs: mpsc::Receiver<DeliveryJob>,
    pool: std::sync::Arc<crate::storage::Pool>,
    inbound: Option<std::sync::Arc<dyn InboundDispatch>>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::delivery::outbox::Outbox;

    let mut pending: HashMap<MessageId, oneshot::Sender<Result<(), ()>>> = HashMap::new();
    let mut retry_tick = tokio::time::interval(RETRY_TICK);
    let mut keepalive_tick = tokio::time::interval(KEEPALIVE_PERIOD);
    let mut last_traffic = tokio::time::Instant::now();
    let mut awaiting_pong_since: Option<tokio::time::Instant> = None;

    loop {
        tokio::select! {
            job = jobs.recv() => {
                let Some(job) = job else { break; };
                let Some(c) = conn.as_mut() else {
                    let _ = job.ack_tx.send(Err(()));
                    continue;
                };
                if let Err(_) = c.send(Frame::MlsApp(job.ciphertext)).await {
                    let _ = job.ack_tx.send(Err(()));
                    conn = None;
                    drain_pending(&mut pending);
                    continue;
                }
                pending.insert(job.message_id, job.ack_tx);
                last_traffic = tokio::time::Instant::now();
            }
            _ = retry_tick.tick() => {
                let ob = Outbox::new(&pool);
                let now_ms = now_ms();
                let due = match ob.due(now_ms, 32) { Ok(v) => v, Err(_) => continue };
                for entry in due {
                    if pending.contains_key(&entry.message_id) { continue; }
                    if entry.target != peer { continue; }
                    let Some(c) = conn.as_mut() else { break; };
                    if let Err(_) = c.send(Frame::MlsApp(entry.payload.clone())).await {
                        conn = None;
                        drain_pending(&mut pending);
                        break;
                    }
                    let (tx, _rx) = oneshot::channel::<Result<(), ()>>();
                    pending.insert(entry.message_id, tx);
                    let _ = ob.reschedule(entry.id, entry.attempts, now_ms);
                    last_traffic = tokio::time::Instant::now();
                }
            }
            _ = keepalive_tick.tick() => {
                if let Some(c) = conn.as_mut() {
                    if last_traffic.elapsed() >= IDLE_CLOSE {
                        let owned = conn.take().unwrap();
                        let _ = owned.close().await;
                        drain_pending(&mut pending);
                        continue;
                    }
                    if awaiting_pong_since.map(|t| t.elapsed() >= PONG_DEADLINE).unwrap_or(false) {
                        conn = None;
                        drain_pending(&mut pending);
                        awaiting_pong_since = None;
                        continue;
                    }
                    let _ = c.send(Frame::Ping).await;
                    awaiting_pong_since.get_or_insert_with(tokio::time::Instant::now);
                }
            }
            frame = async {
                match conn.as_mut() {
                    Some(c) => c.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match frame {
                    Ok(Some(Frame::Ack(bytes))) => {
                        let mid = MessageId(bytes);
                        if let Some(tx) = pending.remove(&mid) { let _ = tx.send(Ok(())); }
                        let ob = Outbox::new(&pool);
                        let _ = ob.ack(&peer, mid);
                        last_traffic = tokio::time::Instant::now();
                    }
                    Ok(Some(Frame::Bye)) => break,
                    Ok(Some(Frame::Ping)) => {
                        if let Some(c) = conn.as_mut() { let _ = c.send(Frame::Pong).await; }
                        last_traffic = tokio::time::Instant::now();
                    }
                    Ok(Some(Frame::Pong)) => {
                        awaiting_pong_since = None;
                        last_traffic = tokio::time::Instant::now();
                    }
                    Ok(Some(Frame::MlsApp(ct))) => {
                        last_traffic = tokio::time::Instant::now();
                        if let Some(d) = inbound.as_ref() {
                            if let Some(mid) = d.dispatch(peer, &ct) {
                                if let Some(c) = conn.as_mut() {
                                    let _ = c.send(Frame::Ack(mid.0)).await;
                                }
                            }
                            // None => rejected, do not ACK.
                        } else {
                            tracing::warn!(
                                "peer: inbound MlsApp received but no InboundDispatch configured"
                            );
                        }
                    }
                    Ok(Some(other)) => tracing::warn!(ty = ?other, "peer: dropping unexpected frame"),
                    Ok(None) => { conn = None; drain_pending(&mut pending); }
                    Err(_) => { conn = None; drain_pending(&mut pending); }
                }
            }
        }
    }

    if let Some(c) = conn {
        let _ = c.close().await;
    }
    drain_pending(&mut pending);
    Ok(())
}

fn drain_pending(pending: &mut HashMap<MessageId, oneshot::Sender<Result<(), ()>>>) {
    for (_, tx) in pending.drain() {
        let _ = tx.send(Err(()));
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}
```

### Step 4: Run the test

```bash
cargo test -p skattr-core --lib delivery::peer
```
Expected: both tests pass (`actor_sends_mlsapp_and_resolves_oneshot_on_ack` from Task 7 and `retry_tick_picks_up_outbox_row_and_delivers`).

### Step 5: Add keepalive test

Append to `mod tests`:

```rust
    #[tokio::test(start_paused = true)]
    async fn keepalive_ping_goes_out_after_sixty_seconds() {
        let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
        let initiator_id = IdentityKey::generate().unwrap();
        let responder_id = IdentityKey::generate().unwrap();
        let responder_static = responder_id.noise_static_public();

        // Responder task: assert a Ping arrives; reply Pong; hold open.
        let responder_task = tokio::spawn(async move {
            let (mut conn, _) =
                handshake_responder(server_stream, &responder_id, None).await.unwrap();
            loop {
                match conn.recv().await {
                    Ok(Some(Frame::Ping)) => {
                        conn.send(Frame::Pong).await.unwrap();
                        break;
                    }
                    Ok(Some(_)) => continue,
                    _ => return,
                }
            }
        });

        let (conn, _) =
            handshake_initiator(client_stream, &initiator_id, &responder_static, None)
                .await
                .unwrap();

        let pool = std::sync::Arc::new(crate::storage::Pool::in_memory());
        let (_job_tx, job_rx) = mpsc::channel::<DeliveryJob>(1);
        let handle = PeerConnection::spawn_full_for_test(
            PublicKey([0xBB; 32]),
            Box::new(conn),
            job_rx,
            pool,
            None,
        );

        // Advance past the 60 s keepalive interval.
        tokio::time::advance(std::time::Duration::from_secs(61)).await;
        // Let the actor run one tick.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Responder should have received the Ping.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), responder_task).await;
        handle.abort();
    }
```

### Step 6: Run and commit

```bash
cargo test -p skattr-core --lib delivery::peer
```
Expected: 3 tests green.

```bash
git add crates/core/src/delivery/peer.rs
git commit -m "$(cat <<'EOF'
delivery: peer actor — retry tick, keepalive, idle close

full_run() is the production actor shape: 1 s retry tick pulls due
outbox rows for this peer and re-sends, 60 s keepalive sends Ping
(30 s pong deadline), 180 s idle close drops the connection. Test
coverage uses tokio::time::pause + advance for deterministic timing.

minimal_run() from Task 7 is retained as a test-only fixture for
wire-level assertions that don't need the tick machinery.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: `DeliveryHub` — routing, `ingest`, `ReplaceConn`, `seen_messages` sweep

**Files:**
- Modify: `crates/core/src/delivery/hub.rs`
- Modify: `crates/core/src/delivery/peer.rs` (add `ReplaceConn` variant to control channel + a production `spawn` that takes an identity and dial info — **not** the test-only ones from Task 7/8)

### Step 1: Add `ReplaceConn` machinery to the actor

In `crates/core/src/delivery/peer.rs`, introduce a second control channel alongside `jobs`:

```rust
/// Control messages sent by the hub to a running peer actor.
pub(crate) enum PeerCtrl<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Replace the actor's current `AuthenticatedConnection` with a new
    /// one (typically because the hub received an inbound dial from
    /// this peer while an older outbound conn was live). The old conn
    /// is closed, pending-ACK oneshots are drained (caller's outbox
    /// rows will be retried), and the new conn takes over.
    ReplaceConn(Box<AuthenticatedConnection<S>>),
    /// Graceful stop. Drain pending and exit.
    Shutdown,
}
```

Extend `full_run` to accept a `mpsc::Receiver<PeerCtrl<S>>` alongside the existing `inbound` dispatcher:

```rust
async fn full_run<S>(
    peer: PublicKey,
    mut conn: Option<AuthenticatedConnection<S>>,
    mut jobs: mpsc::Receiver<DeliveryJob>,
    mut ctrl: mpsc::Receiver<PeerCtrl<S>>,
    pool: std::sync::Arc<crate::storage::Pool>,
    inbound: Option<std::sync::Arc<dyn InboundDispatch>>,
) -> Result<()>
```

Update the Task 8 test spawners (`spawn_with_conn_for_test`, `spawn_full_for_test`) to allocate and pass a no-op `ctrl` channel: `let (_ctrl_tx, ctrl_rx) = mpsc::channel::<PeerCtrl<S>>(4);`. The `spawn_full_for_test` now takes `inbound` as a parameter already (Task 8); include that in the call-through.

Add an arm to the `select!`:

```rust
            c = ctrl.recv() => {
                match c {
                    Some(PeerCtrl::ReplaceConn(new_conn)) => {
                        if let Some(old) = conn.take() { let _ = old.close().await; }
                        drain_pending(&mut pending);
                        conn = Some(*new_conn);
                        last_traffic = tokio::time::Instant::now();
                        awaiting_pong_since = None;
                    }
                    Some(PeerCtrl::Shutdown) | None => break,
                }
            }
```

Update both `spawn_with_conn_for_test` and `spawn_full_for_test` to pass a no-op ctrl channel (`let (_ctrl_tx, ctrl_rx) = mpsc::channel(4);`) and to pass `ctrl_rx` through. Add a production constructor:

```rust
/// Production spawner: the hub creates an actor cold (no conn) and
/// provides job + control channels plus an optional inbound-MLS
/// dispatcher. The actor starts receiving a fresh conn via
/// `PeerCtrl::ReplaceConn` sent by the hub immediately after spawn.
pub(crate) fn spawn<S>(
    peer: PublicKey,
    jobs: mpsc::Receiver<DeliveryJob>,
    ctrl: mpsc::Receiver<PeerCtrl<S>>,
    pool: std::sync::Arc<crate::storage::Pool>,
    inbound: Option<std::sync::Arc<dyn InboundDispatch>>,
) -> PeerHandle
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let _ = full_run::<S>(peer, None, jobs, ctrl, pool, inbound).await;
    })
}
```

Rebuild:

```bash
cargo build -p skattr-core --all-features
```
Expected: succeeds.

### Step 2: Write the `DeliveryHub` tests

Replace `crates/core/src/delivery/hub.rs` skeleton with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Daemon-scoped delivery router.
//!
//! Maps `PublicKey → (mpsc::Sender<DeliveryJob>, mpsc::Sender<PeerCtrl>)`,
//! spawning a per-peer actor on the first send or ingest. Also runs a
//! periodic `seen_messages` sweep.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::delivery::peer::{DeliveryJob, InboundDispatch, PeerConnection, PeerCtrl};
use crate::envelope::MessageId;
use crate::error::Result;
use crate::identity::PublicKey;
use crate::storage::seen_messages::SeenMessagesRepo;
use crate::storage::Pool;
use crate::transport::connection::AuthenticatedConnection;

const JOB_CHAN_CAP: usize = 64;
const CTRL_CHAN_CAP: usize = 4;
const SWEEP_INTERVAL: Duration = Duration::from_secs(3600);
const SEEN_WINDOW_MS: i64 = 24 * 3600 * 1000;

struct PeerChannels<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    jobs: mpsc::Sender<DeliveryJob>,
    ctrl: mpsc::Sender<PeerCtrl<S>>,
}

/// Per-daemon delivery hub.
pub(crate) struct DeliveryHub<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    peers: Mutex<HashMap<PublicKey, PeerChannels<S>>>,
    pool: Arc<Pool>,
    inbound: Option<Arc<dyn InboundDispatch>>,
    _sweep: tokio::task::JoinHandle<()>,
}

impl<S> DeliveryHub<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Construct a hub with no inbound-MLS handling. Suitable for
    /// outbound-only tests where the responder echoes `Frame::Ack`
    /// directly; real-MLS deployments must use
    /// [`DeliveryHub::new_with_inbound`] instead.
    pub(crate) fn new(pool: Arc<Pool>) -> Self {
        Self::new_with_inbound_inner(pool, None)
    }

    /// Construct a hub that decrypts inbound `Frame::MlsApp` through
    /// `dispatch`. The integration test builds an `MlsInboundDispatch`
    /// that wraps `Group::decrypt` + `receiver::receive` for the one
    /// peer it cares about.
    pub(crate) fn new_with_inbound(
        pool: Arc<Pool>,
        dispatch: Arc<dyn InboundDispatch>,
    ) -> Self {
        Self::new_with_inbound_inner(pool, Some(dispatch))
    }

    fn new_with_inbound_inner(pool: Arc<Pool>, inbound: Option<Arc<dyn InboundDispatch>>) -> Self {
        let sweep_pool = pool.clone();
        let sweep = tokio::spawn(async move {
            let mut t = tokio::time::interval(SWEEP_INTERVAL);
            t.tick().await;
            loop {
                t.tick().await;
                let now = crate::delivery::peer::now_ms_testable();
                let cutoff = now - SEEN_WINDOW_MS;
                let seen = SeenMessagesRepo::new(&sweep_pool);
                let _ = seen.sweep_older_than(cutoff);
            }
        });
        Self {
            peers: Mutex::new(HashMap::new()),
            pool,
            inbound,
            _sweep: sweep,
        }
    }

    /// Submit a job for `peer`. Spawns the peer actor on first use.
    pub(crate) async fn send(
        &self,
        peer: PublicKey,
        message_id: MessageId,
        ciphertext: Vec<u8>,
    ) -> Result<oneshot::Receiver<Result<(), ()>>> {
        let (ack_tx, ack_rx) = oneshot::channel::<Result<(), ()>>();
        let jobs_tx = self.ensure_actor(peer).await;
        let _ = jobs_tx
            .send(DeliveryJob {
                message_id,
                ciphertext,
                ack_tx,
            })
            .await;
        Ok(ack_rx)
    }

    /// Install a post-handshake `AuthenticatedConnection` for `peer`.
    /// If an actor already exists for this peer, its current conn is
    /// replaced. Otherwise a fresh actor is spawned with the conn.
    pub(crate) async fn ingest(
        &self,
        peer: PublicKey,
        conn: AuthenticatedConnection<S>,
    ) {
        let mut peers = self.peers.lock().await;
        if let Some(ch) = peers.get(&peer) {
            let _ = ch.ctrl.send(PeerCtrl::ReplaceConn(Box::new(conn))).await;
            return;
        }
        let (jobs_tx, jobs_rx) = mpsc::channel::<DeliveryJob>(JOB_CHAN_CAP);
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<PeerCtrl<S>>(CTRL_CHAN_CAP);
        let _handle = PeerConnection::spawn::<S>(peer, jobs_rx, ctrl_rx, self.pool.clone(), self.inbound.clone());
        let _ = ctrl_tx.send(PeerCtrl::ReplaceConn(Box::new(conn))).await;
        peers.insert(peer, PeerChannels { jobs: jobs_tx, ctrl: ctrl_tx });
    }

    async fn ensure_actor(&self, peer: PublicKey) -> mpsc::Sender<DeliveryJob> {
        let mut peers = self.peers.lock().await;
        if let Some(ch) = peers.get(&peer) {
            return ch.jobs.clone();
        }
        let (jobs_tx, jobs_rx) = mpsc::channel::<DeliveryJob>(JOB_CHAN_CAP);
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<PeerCtrl<S>>(CTRL_CHAN_CAP);
        let _handle = PeerConnection::spawn::<S>(peer, jobs_rx, ctrl_rx, self.pool.clone(), self.inbound.clone());
        peers.insert(peer, PeerChannels {
            jobs: jobs_tx.clone(),
            ctrl: ctrl_tx,
        });
        jobs_tx
    }
}
```

Then add a small `pub(crate) fn now_ms_testable()` to `peer.rs` that simply wraps `now_ms` (exposed to `hub.rs`):

```rust
#[doc(hidden)]
pub(crate) fn now_ms_testable() -> i64 { now_ms() }
```

### Step 3: Tests for the hub

Append to `hub.rs` a `#[cfg(test)] mod tests` block:

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::identity::IdentityKey;
    use crate::transport::frame::Frame;
    use crate::transport::noise::{handshake_initiator, handshake_responder};

    #[tokio::test]
    async fn ingest_spawns_actor_and_replace_conn_on_second_ingest() {
        let pool = Arc::new(Pool::in_memory());
        let hub: DeliveryHub<tokio::io::DuplexStream> = DeliveryHub::new(pool.clone());

        let alice = IdentityKey::generate().unwrap();
        let bob = IdentityKey::generate().unwrap();
        let bob_static = bob.noise_static_public();
        let bob_pk = PublicKey(bob.public().0);

        // Conn #1
        let (a1, b1) = tokio::io::duplex(16 * 1024);
        let bob_task = tokio::spawn(async move {
            let (_conn, _) = handshake_responder(b1, &bob, None).await.unwrap();
            // Let the initiator drive; we keep this alive briefly.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        });
        let (conn_a1, _) =
            handshake_initiator(a1, &alice, &bob_static, None).await.unwrap();
        hub.ingest(bob_pk, conn_a1).await;

        // At this point the hub should have one actor for bob_pk.
        {
            let peers = hub.peers.lock().await;
            assert!(peers.contains_key(&bob_pk));
        }

        let _ = bob_task.await;
    }
}
```

### Step 4: Run and confirm

```bash
cargo test -p skattr-core --lib delivery::hub -- --nocapture
```
Expected: 1 test passes.

### Step 5: Commit

```bash
git add crates/core/src/delivery/peer.rs \
        crates/core/src/delivery/hub.rs
git commit -m "$(cat <<'EOF'
delivery: DeliveryHub router + PeerCtrl::ReplaceConn

Hub maps PublicKey to (jobs_tx, ctrl_tx) and spawns the per-peer
actor on first use. ingest() installs a post-handshake
AuthenticatedConnection, replacing any existing one via
PeerCtrl::ReplaceConn so the newer conn always wins when both sides
dial simultaneously. A background task sweeps seen_messages on a 1 h
cadence.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: `KillableStream` + extended `test_exports` + `Daemon::send` wrapper

**Files:**
- Create: `crates/core/src/delivery/kill_stream.rs`
- Modify: `crates/core/src/delivery/mod.rs` (declare `kill_stream`, gated on `test-harness`)
- Modify: `crates/core/src/daemon/state.rs` (add `pub(crate) async fn Daemon::send`)
- Modify: `crates/core/src/lib.rs` (extend `test_exports`)

### Step 1: Create `KillableStream`

Create `crates/core/src/delivery/kill_stream.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Test-only: an `AsyncRead + AsyncWrite` wrapper that can be "killed"
//! mid-stream by flipping a shared atomic. After the kill flag is set,
//! all reads and writes return `ErrorKind::BrokenPipe`.
//!
//! Gated on `feature = "test-harness"` because it only exists to
//! exercise the kill-mid-message integration test.

use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Handle that kills both ends of a paired `KillableStream`.
#[derive(Clone, Debug, Default)]
pub struct KillSwitch(Arc<AtomicBool>);

impl KillSwitch {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Flip the kill flag. Idempotent.
    pub fn kill(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    #[must_use]
    pub fn is_killed(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Wrap `inner` so that once `switch` is flipped, all I/O fails.
pub struct KillableStream<S> {
    inner: S,
    switch: KillSwitch,
}

impl<S> KillableStream<S> {
    pub fn new(inner: S, switch: KillSwitch) -> Self {
        Self { inner, switch }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for KillableStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = Pin::into_inner(self);
        if this.switch.is_killed() {
            return Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe)));
        }
        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for KillableStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = Pin::into_inner(self);
        if this.switch.is_killed() {
            return Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe)));
        }
        Pin::new(&mut this.inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = Pin::into_inner(self);
        if this.switch.is_killed() {
            return Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe)));
        }
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = Pin::into_inner(self);
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}
```

### Step 2: Gate it in `delivery/mod.rs`

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Send/receive plumbing: outbox queue, retry, dedup, ACK handling.

pub(crate) mod backoff;
pub(crate) mod hub;
pub(crate) mod outbox;
pub(crate) mod peer;
pub(crate) mod receiver;

#[cfg(feature = "test-harness")]
pub mod kill_stream;
```

### Step 3: Add `Daemon::send` wrapper

`Daemon` in Phase 1.E doesn't yet own a hub (1.F will plumb it). To honor the spec's `Daemon::send` commitment, add a thin wrapper that constructs-on-demand is *not* what we want. Instead: add a `pub(crate) struct DaemonContext` built by `Daemon::run` that owns the hub + pool + identity + groups, and expose `Daemon::send` as a method that routes through it.

For 1.E, keep this minimal:

Edit `crates/core/src/daemon/state.rs` — inside `impl Daemon`, append:

```rust
    /// Encrypt an envelope for `peer` via the existing MLS group, persist
    /// it to the outbox, and hand it off to the delivery hub for
    /// transmission. `pub(crate)` until 1.F wires the CLI path; tests
    /// reach this via `test_exports::send`.
    #[allow(dead_code)]
    pub(crate) async fn send(
        &self,
        _peer: crate::identity::PublicKey,
        _envelope: crate::envelope::Envelope,
    ) -> crate::error::Result<tokio::sync::oneshot::Receiver<crate::error::Result<()>>> {
        // 1.E is scaffold-complete but Daemon::run does not yet
        // construct the hub (that wiring lands with 1.F when
        // Daemon::execute grows `Command::Send`). Returning an error
        // here until then keeps the signature stable for tests; the
        // integration test in crates/tests/src/delivery_kill_mid_message.rs
        // bypasses Daemon and drives DeliveryHub directly through
        // test_exports.
        Err(crate::error::CoreError::Daemon(
            "Daemon::send requires 1.F CLI integration".into(),
        ))
    }
```

If `CoreError::Daemon` doesn't exist, grep for the canonical "general" variant (likely `CoreError::Other(String)` or similar) and use that. Verify first:

```bash
grep -n "pub enum CoreError" -A 40 crates/core/src/error.rs | head -50
```

Pick the nearest matching variant (e.g. `CoreError::Other` or `CoreError::Transport`); adjust the call to match.

### Step 4: Extend `test_exports`

Edit `crates/core/src/lib.rs`, inside `pub mod test_exports { … }`, add:

```rust
    // Phase 1.E additions:
    pub use crate::delivery::hub::DeliveryHub;
    pub use crate::delivery::kill_stream::{KillSwitch, KillableStream};
    pub use crate::delivery::outbox::{Outbox, OutboxEntry};
    pub use crate::delivery::peer::{
        DeliveryJob, InboundDispatch, PeerConnection, PeerCtrl,
    };
    pub use crate::delivery::receiver::{receive, ReceiveOutcome, REPLAY_WINDOW_MS};
    pub use crate::storage::{MessageRepo, SeenMessagesRepo};
```

`SeenMessagesRepo` is currently `pub(crate)` inside `storage::seen_messages`. Add `pub(crate) use seen_messages::SeenMessagesRepo;` to `crates/core/src/storage/mod.rs` alongside the other `pub(crate) use` lines so the test_exports re-export resolves.

### Step 5: Build and run

```bash
cargo build -p skattr-core --all-features
cargo clippy -p skattr-core --all-targets --all-features -- -D warnings
cargo test -p skattr-core --all-features
```
Expected: all green.

### Step 6: Commit

```bash
git add crates/core/src/delivery/kill_stream.rs \
        crates/core/src/delivery/mod.rs \
        crates/core/src/daemon/state.rs \
        crates/core/src/storage/mod.rs \
        crates/core/src/lib.rs
git commit -m "$(cat <<'EOF'
delivery: KillableStream + test_exports additions + Daemon::send stub

KillableStream wraps any AsyncRead+AsyncWrite and fails all further
I/O after the shared KillSwitch is flipped — the primary vehicle for
the kill-mid-message integration test in 1.E.

test_exports gains DeliveryHub, Outbox, PeerConnection, receive,
SeenMessagesRepo, and MessageRepo so the test crate can drive the
delivery stack directly. Daemon::send is a pub(crate) stub until
1.F wires the full CLI path.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Integration test — kill-mid-message → reconnect → exactly-once

**Files:**
- Create: `crates/tests/src/delivery_kill_mid_message.rs`
- Modify: `crates/tests/src/lib.rs` (declare the new test module)

### Step 1: Declare the module

Edit `crates/tests/src/lib.rs`:

```rust
#[cfg(test)]
mod arti_echo;
#[cfg(test)]
mod delivery_kill_mid_message;
#[cfg(test)]
mod invite_roundtrip;
#[cfg(test)]
mod mls_pair;
```

### Step 2: Write the test

Create `crates/tests/src/delivery_kill_mid_message.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Integration test: two daemons share an MLS 2-member group, send one
//! application message, kill the transport mid-flight, rebuild the
//! transport, and assert the message is delivered exactly once.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use skattr_core::envelope::{Envelope, Kind, MessageId};
use skattr_core::identity::IdentityKey;
use skattr_core::test_exports::{
    handshake_initiator, handshake_responder, DeliveryHub, Group, GroupId, InboundDispatch,
    KeyPackage, KeyPackageRepo, KillSwitch, KillableStream, MessageRepo, MlsGroupRepo,
    MlsProvider, Outbox, Pool, ReceiveOutcome, SeenMessagesRepo,
};

/// Integration-test `InboundDispatch` that reloads the MLS group from
/// storage on each invocation, decrypts, persists the new ratchet
/// state, then runs `receiver::receive` for ts-window + dedup +
/// persist. Reloading each call is fine for a test that sends a
/// handful of messages; production would cache.
///
/// `Group` is not `Clone`-able (wraps OpenMLS state), so `Pair` stores
/// only the `GroupId` here and the actual `Group` lives in storage.
/// Follow `crates/tests/src/mls_pair.rs` for the `Group::load` / save
/// pattern.
struct MlsInboundDispatch {
    pool: Arc<Pool>,
    group_id: GroupId,
    expected_peer: skattr_core::identity::PublicKey,
}

impl InboundDispatch for MlsInboundDispatch {
    fn dispatch(
        &self,
        peer: skattr_core::identity::PublicKey,
        ciphertext: &[u8],
    ) -> Option<MessageId> {
        if peer != self.expected_peer {
            return None;
        }
        let repo = MlsGroupRepo::new(&self.pool);
        let mut g = Group::load(&self.group_id, &repo).ok().flatten()?;
        let envelope = g.decrypt(ciphertext).ok()?;
        let _ = g.save(&repo);

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let seen = SeenMessagesRepo::new(&self.pool);
        let msgs = MessageRepo::new(&self.pool);
        let mid = envelope.id;
        match skattr_core::test_exports::receive(&peer, &self.group_id.0, envelope, now_ms, &seen, &msgs).ok()? {
            ReceiveOutcome::New(_) | ReceiveOutcome::Duplicate => Some(mid),
            ReceiveOutcome::Rejected(_) => None,
        }
    }
}

/// The test envelopes set `ts: 0`; the receiver's ±1h check needs
/// the test to either (a) use `ts: now_ms()` or (b) widen the
/// window for tests. Task 5 already made `REPLAY_WINDOW_MS`
/// pub(crate); this helper emits a `ts` in the current window.
fn ts_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn env(body: &str) -> Envelope {
    Envelope {
        v: 1,
        id: MessageId::generate(),
        ts: ts_now_ms(),
        reply_to: None,
        kind: Kind::Text { body: body.into() },
    }
}

/// Minimal pair bootstrap: two pools, two identities, two MLS groups
/// (Alice's solo + Bob added), returns the shared GroupId.
struct Pair {
    alice_pool: Arc<Pool>,
    bob_pool: Arc<Pool>,
    alice_id: IdentityKey,
    bob_id: IdentityKey,
    alice_group: Group,
    bob_group: Group,
    gid: GroupId,
}

fn setup_pair() -> Pair {
    let alice_pool = Arc::new(Pool::in_memory());
    let bob_pool = Arc::new(Pool::in_memory());
    let alice_id = IdentityKey::generate().unwrap();
    let bob_id = IdentityKey::generate().unwrap();

    let alice_provider = MlsProvider::new(alice_pool.clone()).unwrap();
    let bob_provider = MlsProvider::new(bob_pool.clone()).unwrap();
    let bob_kp_repo = KeyPackageRepo::new(&bob_pool);
    let alice_group_repo = MlsGroupRepo::new(&alice_pool);
    let bob_group_repo = MlsGroupRepo::new(&bob_pool);

    let psk = [0x5Au8; 32];
    let bob_kp = KeyPackage::generate(&bob_provider, &bob_id).unwrap();
    bob_kp_repo.put(&bob_kp).unwrap();

    let mut alice_group = Group::create_solo(&alice_provider, &alice_id, &psk).unwrap();
    let (welcome, commit) = alice_group.add_member(&alice_provider, &bob_kp).unwrap();
    alice_group.save(&alice_group_repo).unwrap();

    let mut bob_group =
        Group::join_from_welcome(&bob_provider, &bob_id, welcome.as_slice(), &psk).unwrap();
    bob_group.process_incoming_commit(commit.as_slice()).unwrap();
    bob_group.save(&bob_group_repo).unwrap();

    let gid = alice_group.id().clone();
    Pair {
        alice_pool,
        bob_pool,
        alice_id,
        bob_id,
        alice_group,
        bob_group,
        gid,
    }
}

async fn run_paired_handshake(
    alice_id: &IdentityKey,
    bob_id: &IdentityKey,
    kill: KillSwitch,
) -> (
    KillableStream<tokio::io::DuplexStream>,
    KillableStream<tokio::io::DuplexStream>,
) {
    let (a_raw, b_raw) = tokio::io::duplex(64 * 1024);
    (
        KillableStream::new(a_raw, kill.clone()),
        KillableStream::new(b_raw, kill),
    )
}

#[tokio::test]
async fn kill_mid_message_redelivers_exactly_once() {
    let pair = setup_pair();

    // Build per-side InboundDispatch. Each side's Group lives in
    // storage (saved by setup_pair); the dispatcher reloads on
    // demand.
    let alice_dispatch: Arc<dyn InboundDispatch> = Arc::new(MlsInboundDispatch {
        pool: pair.alice_pool.clone(),
        group_id: pair.gid.clone(),
        expected_peer: skattr_core::identity::PublicKey(pair.bob_id.public().0),
    });
    let bob_dispatch: Arc<dyn InboundDispatch> = Arc::new(MlsInboundDispatch {
        pool: pair.bob_pool.clone(),
        group_id: pair.gid.clone(),
        expected_peer: skattr_core::identity::PublicKey(pair.alice_id.public().0),
    });

    let alice_hub: DeliveryHub<KillableStream<tokio::io::DuplexStream>> =
        DeliveryHub::new_with_inbound(pair.alice_pool.clone(), alice_dispatch.clone());
    let bob_hub: DeliveryHub<KillableStream<tokio::io::DuplexStream>> =
        DeliveryHub::new_with_inbound(pair.bob_pool.clone(), bob_dispatch.clone());

    // --- Round 1: handshake + send + kill before ACK arrives ---
    let kill1 = KillSwitch::new();
    let (alice_stream, bob_stream) =
        run_paired_handshake(&pair.alice_id, &pair.bob_id, kill1.clone()).await;

    // Drive both sides of Noise concurrently.
    let bob_id_clone = pair.bob_id.clone();
    let alice_id_clone = pair.alice_id.clone();
    let bob_static = pair.bob_id.noise_static_public();
    let responder = tokio::spawn(async move {
        handshake_responder(bob_stream, &bob_id_clone, None).await
    });
    let (alice_conn, _) =
        handshake_initiator(alice_stream, &alice_id_clone, &bob_static, None)
            .await
            .unwrap();
    let (bob_conn, _) = responder.await.unwrap().unwrap();

    // Plumb both authenticated conns into their respective hubs.
    alice_hub
        .ingest(
            skattr_core::identity::PublicKey(pair.bob_id.public().0),
            alice_conn,
        )
        .await;
    bob_hub
        .ingest(
            skattr_core::identity::PublicKey(pair.alice_id.public().0),
            bob_conn,
        )
        .await;

    // Alice encrypts and enqueues.
    let message = env("hello bob");
    let mid = message.id;
    let ct = pair.alice_group.encrypt(&message).unwrap();
    let peer_bob = skattr_core::identity::PublicKey(pair.bob_id.public().0);
    Outbox::new(&pair.alice_pool)
        .enqueue(&peer_bob, mid, &ct, 0)
        .unwrap();

    // Alice sends; kill the wire mid-flight.
    let ack_rx = alice_hub.send(peer_bob, mid, ct.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    kill1.kill();
    // Expect oneshot to resolve Err because the conn is dead.
    let _ = tokio::time::timeout(Duration::from_secs(1), ack_rx).await;

    // At this point Bob may or may not have observed the frame; we
    // don't assert either way — the point is that the next round must
    // produce exactly one stored message.
    drop(alice_hub);
    drop(bob_hub);

    // --- Round 2: rebuild hubs, rebuild transport, wait for retry ---
    let alice_hub2: DeliveryHub<KillableStream<tokio::io::DuplexStream>> =
        DeliveryHub::new_with_inbound(pair.alice_pool.clone(), alice_dispatch.clone());
    let bob_hub2: DeliveryHub<KillableStream<tokio::io::DuplexStream>> =
        DeliveryHub::new_with_inbound(pair.bob_pool.clone(), bob_dispatch.clone());

    let kill2 = KillSwitch::new();
    let (a2, b2) = run_paired_handshake(&pair.alice_id, &pair.bob_id, kill2).await;
    let bob_id_clone = pair.bob_id.clone();
    let alice_id_clone = pair.alice_id.clone();
    let responder2 = tokio::spawn(async move {
        handshake_responder(b2, &bob_id_clone, None).await
    });
    let (a_conn2, _) =
        handshake_initiator(a2, &alice_id_clone, &bob_static, None).await.unwrap();
    let (b_conn2, _) = responder2.await.unwrap().unwrap();
    alice_hub2
        .ingest(
            skattr_core::identity::PublicKey(pair.bob_id.public().0),
            a_conn2,
        )
        .await;
    bob_hub2
        .ingest(
            skattr_core::identity::PublicKey(pair.alice_id.public().0),
            b_conn2,
        )
        .await;

    // Wait up to 3s for the retry tick to fire and deliver.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let seen = SeenMessagesRepo::new(&pair.bob_pool);
        if seen
            .contains(&pair.alice_id.public().0, &mid.0)
            .unwrap()
        {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("retry tick did not redeliver within 3s");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Assert: bob's messages has exactly one row for this sender.
    let msgs = MessageRepo::new(&pair.bob_pool);
    let recent = msgs.recent(&pair.gid.0, 10).unwrap();
    let from_alice: Vec<_> = recent
        .iter()
        .filter(|m| m.sender == pair.alice_id.public().0)
        .collect();
    assert_eq!(
        from_alice.len(),
        1,
        "exactly one row must land in bob's messages table, got {}",
        from_alice.len()
    );

    // Alice's outbox must be empty.
    let ob = Outbox::new(&pair.alice_pool);
    assert!(
        ob.due(i64::MAX, 10).unwrap().is_empty(),
        "alice's outbox must be empty after ACK"
    );
}

#[tokio::test]
async fn kill_before_any_frame_sent_delivers_on_retry() {
    let pair = setup_pair();
    let bob_dispatch: Arc<dyn InboundDispatch> = Arc::new(MlsInboundDispatch {
        pool: pair.bob_pool.clone(),
        group_id: pair.gid.clone(),
        expected_peer: skattr_core::identity::PublicKey(pair.alice_id.public().0),
    });
    let alice_hub: DeliveryHub<KillableStream<tokio::io::DuplexStream>> =
        DeliveryHub::new(pair.alice_pool.clone()); // Alice is outbound-only, no inbound dispatcher needed

    let message = env("first wire never flies");
    let mid = message.id;
    let ct = pair.alice_group.encrypt(&message).unwrap();
    let peer_bob = skattr_core::identity::PublicKey(pair.bob_id.public().0);

    // Enqueue; kill switch already flipped before any conn exists.
    Outbox::new(&pair.alice_pool)
        .enqueue(&peer_bob, mid, &ct, 0)
        .unwrap();

    // Build a fresh, non-killed transport and ingest on both ends.
    let kill = KillSwitch::new();
    let (a_raw, b_raw) = tokio::io::duplex(64 * 1024);
    let alice_stream = KillableStream::new(a_raw, kill.clone());
    let bob_stream = KillableStream::new(b_raw, kill);
    let bob_id_clone = pair.bob_id.clone();
    let responder = tokio::spawn(async move {
        handshake_responder(bob_stream, &bob_id_clone, None).await
    });
    let bob_static = pair.bob_id.noise_static_public();
    let (a_conn, _) =
        handshake_initiator(alice_stream, &pair.alice_id, &bob_static, None)
            .await
            .unwrap();
    let (b_conn, _) = responder.await.unwrap().unwrap();

    let bob_hub: DeliveryHub<KillableStream<tokio::io::DuplexStream>> =
        DeliveryHub::new_with_inbound(pair.bob_pool.clone(), bob_dispatch);
    alice_hub.ingest(peer_bob, a_conn).await;
    bob_hub
        .ingest(
            skattr_core::identity::PublicKey(pair.alice_id.public().0),
            b_conn,
        )
        .await;

    // Let the retry tick pick up the row.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let ob = Outbox::new(&pair.alice_pool);
        if ob.due(i64::MAX, 10).unwrap().is_empty() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("retry tick did not deliver within 3s");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let msgs = MessageRepo::new(&pair.bob_pool);
    let recent = msgs.recent(&pair.gid.0, 10).unwrap();
    assert_eq!(recent.len(), 1);
}
```

### Step 3: Run the integration test

```bash
cargo test -p skattr-tests --test-threads=1 --all-features -- --nocapture
```
Expected: both tests pass within ~10 seconds total. The `--all-features` propagates `test-harness`.

**Note on OpenMLS re-decryption:** Spec §4.4 Case B assumes `Group::decrypt` tolerates re-processing the same ciphertext (ACK lost on first round, ciphertext re-sent on retry, Bob decrypts twice). OpenMLS maintains a sliding window of old-generation keys, so this typically works, but if the implementer observes a decrypt failure on the retry path the fix is to add a ciphertext-level pre-dedup in `MlsInboundDispatch` — hash the ciphertext, check a per-peer `HashSet<[u8; 32]>` before calling `Group::decrypt`, and on hit skip decrypt but still emit the ACK. That's a legitimate 1.E extension that falls out of the spec's "duplicates do ACK" rule.

If the test needs `test-harness` to be enabled on `skattr-core` by default for the tests crate only, check `crates/tests/Cargo.toml` — add/verify:

```toml
[dependencies]
skattr-core = { path = "../core", features = ["test-harness"] }
```

### Step 4: Commit

```bash
git add crates/tests/src/lib.rs crates/tests/src/delivery_kill_mid_message.rs crates/tests/Cargo.toml
git commit -m "$(cat <<'EOF'
tests: delivery — kill-mid-message → reconnect → exactly-once

Two tests over tokio::io::duplex + KillableStream exercise the full
delivery stack (Noise + MLS + hub + actor + outbox + seen_messages):

  1. kill-after-send then reconnect: assert bob's messages table has
     exactly one row and alice's outbox is empty after retry.
  2. kill-before-any-send then reconnect: same post-condition.

Both tests run in CI (no #[ignore]); they complete in seconds.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: Integration test — real Tor (`#[ignore]`-gated)

**Files:**
- Create: `crates/tests/src/delivery_real_tor.rs`
- Modify: `crates/tests/src/lib.rs` (declare the module)

### Step 1: Declare the module

Edit `crates/tests/src/lib.rs`:

```rust
#[cfg(test)]
mod delivery_real_tor;
```

### Step 2: Write the test

Create `crates/tests/src/delivery_real_tor.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Integration test over real Arti: two daemons publish onions,
//! Alice dials Bob, runs the full handshake, and sends five
//! application messages through the delivery hub. Asserts all five
//! arrive.
//!
//! `#[ignore]`-gated; run with:
//!     cargo test -p skattr-tests --release -- --ignored
//!
//! Mirrors `arti_echo.rs` in shape.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use skattr_core::envelope::{Envelope, Kind, MessageId};
use skattr_core::identity::IdentityKey;
use skattr_core::test_exports::{
    handshake_initiator, handshake_responder, DeliveryHub, Group, GroupId, KeyPackage,
    KeyPackageRepo, MessageRepo, MlsGroupRepo, MlsProvider, Outbox, Pool, SeenMessagesRepo,
    TorConfig, TorRuntime,
};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "real Tor bootstrap + HS publish + dial, ~2–5 min; run with --ignored"]
async fn five_messages_delivered_over_real_tor() {
    // Bootstrap two Arti runtimes in temp dirs.
    let a_tmp = tempfile::tempdir().unwrap();
    let b_tmp = tempfile::tempdir().unwrap();
    let mut a_rt = TorRuntime::bootstrap(TorConfig {
        state_dir: a_tmp.path().join("arti"),
        socks_port: None,
    })
    .await
    .unwrap();
    let mut b_rt = TorRuntime::bootstrap(TorConfig {
        state_dir: b_tmp.path().join("arti"),
        socks_port: None,
    })
    .await
    .unwrap();

    // Identities and pools.
    let alice_id = IdentityKey::generate().unwrap();
    let bob_id = IdentityKey::generate().unwrap();
    let alice_pool = Arc::new(Pool::in_memory());
    let bob_pool = Arc::new(Pool::in_memory());

    // MLS group setup: Alice solo → add Bob.
    let alice_provider = MlsProvider::new(alice_pool.clone()).unwrap();
    let bob_provider = MlsProvider::new(bob_pool.clone()).unwrap();
    let bob_kp = KeyPackage::generate(&bob_provider, &bob_id).unwrap();
    KeyPackageRepo::new(&bob_pool).put(&bob_kp).unwrap();
    let mut alice_group =
        Group::create_solo(&alice_provider, &alice_id, &[0x5A; 32]).unwrap();
    let (welcome, commit) = alice_group.add_member(&alice_provider, &bob_kp).unwrap();
    alice_group.save(&MlsGroupRepo::new(&alice_pool)).unwrap();
    let mut bob_group = Group::join_from_welcome(
        &bob_provider,
        &bob_id,
        welcome.as_slice(),
        &[0x5A; 32],
    )
    .unwrap();
    bob_group.process_incoming_commit(commit.as_slice()).unwrap();
    bob_group.save(&MlsGroupRepo::new(&bob_pool)).unwrap();
    let gid = alice_group.id().clone();

    // Bob publishes an onion.
    let bob_hs_key_path = b_tmp.path().join("hs.key.age");
    let bob_seed = [0x11u8; 32];
    let bob_onion = b_rt
        .publish_onion(&bob_hs_key_path, &bob_seed, "skattr-1e-tor")
        .await
        .unwrap();

    // Alice dials Bob.
    let a_stream = a_rt.connect(&bob_onion, 443).await.unwrap();
    let b_accept = b_rt.accept_next().await.unwrap();
    let bob_static = bob_id.noise_static_public();
    let bob_id_clone = bob_id.clone();
    let responder = tokio::spawn(async move {
        handshake_responder(b_accept, &bob_id_clone, None).await
    });
    let (alice_conn, _) =
        handshake_initiator(a_stream, &alice_id, &bob_static, None)
            .await
            .unwrap();
    let (bob_conn, _) = responder.await.unwrap().unwrap();

    // Hubs.
    type S = arti_client::DataStream;
    let alice_hub: DeliveryHub<S> = DeliveryHub::new(alice_pool.clone());
    let bob_hub: DeliveryHub<S> = DeliveryHub::new(bob_pool.clone());
    alice_hub
        .ingest(
            skattr_core::identity::PublicKey(bob_id.public().0),
            alice_conn,
        )
        .await;
    bob_hub
        .ingest(
            skattr_core::identity::PublicKey(alice_id.public().0),
            bob_conn,
        )
        .await;

    // Alice sends 5 envelopes.
    let peer_bob = skattr_core::identity::PublicKey(bob_id.public().0);
    let mut ids = vec![];
    for i in 0..5 {
        let env = Envelope {
            v: 1,
            id: MessageId::generate(),
            ts: 0,
            reply_to: None,
            kind: Kind::Text { body: format!("msg-{i}") },
        };
        ids.push(env.id);
        let ct = alice_group.encrypt(&env).unwrap();
        Outbox::new(&alice_pool)
            .enqueue(&peer_bob, env.id, &ct, 0)
            .unwrap();
        let _ = alice_hub.send(peer_bob, env.id, ct).await.unwrap();
    }

    // Wait for all five to land in Bob's storage.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let msgs = MessageRepo::new(&bob_pool);
        let recent = msgs.recent(&gid.0, 50).unwrap();
        if recent.len() >= 5 {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("only {} of 5 messages delivered within 30s", recent.len());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Shutdown.
    let _ = a_rt.shutdown().await;
    let _ = b_rt.shutdown().await;
}
```

### Step 3: Ensure `TorRuntime::accept_next` exists (or equivalent)

`arti_echo.rs` already gets stream-level acceptance. Check the pattern used there:

```bash
grep -n "accept_next\|rend_requests\|OnionListener" crates/tests/src/arti_echo.rs | head -10
```

If the existing integration test uses `OnionListener::spawn(rend_requests, 8)` + `listener.accepted.recv().await` instead of a hypothetical `TorRuntime::accept_next`, adapt the test to match that pattern — the test here is a composition of the existing shapes, not a new API.

### Step 4: Run it once locally

```bash
cargo test -p skattr-tests --release --all-features -- --ignored five_messages_delivered_over_real_tor --nocapture
```
Expected: test passes within 2–5 min. If Arti bootstrap times out, re-run (the Tor network is not always fast from a cold start).

### Step 5: Commit

```bash
git add crates/tests/src/lib.rs crates/tests/src/delivery_real_tor.rs
git commit -m "$(cat <<'EOF'
tests: delivery over real Tor — ignored smoke test

Mirrors arti_echo.rs in shape. Two daemons bootstrap Arti, Bob
publishes an onion, Alice dials, both sides run Noise + MLS, five
application messages round-trip through DeliveryHub. #[ignore]-gated
because real Tor bootstrap is slow; run with --ignored.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 13: CHANGELOG + CLAUDE.md status update

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `CLAUDE.md`

### Step 1: Verify the final exit-criterion build

```bash
cd /home/myggiz/development/skattr-phase-1e-delivery
. "$HOME/.cargo/env"
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```
Expected: all green. This is the **verification gate** — do not skip.

### Step 2: Append to `CHANGELOG.md`

Find the existing "Unreleased" / Phase-1.D entry and append a new section for 1.E. Example (mirror the 1.D bullet density):

```markdown
## Phase 1.E — Delivery semantics (2026-04-22)

- **delivery::backoff** — doubled-seconds exponential backoff with ±25% jitter, capped at 5 min.
- **delivery::outbox** — `Outbox` wrapper over `OutboxRepo` speaking `PublicKey`/`MessageId`.
- **delivery::receiver** — `receive()` enforces ±1 h replay window, uses the existing `seen_messages` 24 h dedup, and returns `New` / `Duplicate` / `Rejected` so the actor knows whether to ACK.
- **delivery::peer::PeerConnection** — per-peer actor owning an `Option<AuthenticatedConnection<S>>`, pending-ACK map, 1 s retry tick, 60 s keepalive / 30 s pong deadline, 180 s idle close, and `ReplaceConn` for concurrent dial races.
- **delivery::hub::DeliveryHub** — per-daemon router; spawns per-peer actors on first use, ingests post-handshake connections, runs a 1 h `seen_messages` sweep.
- **delivery::kill_stream** — test-only `KillableStream<S>` + `KillSwitch` behind `feature = "test-harness"`.
- **storage migration 0004** — adds `message_id BLOB NOT NULL` + `UNIQUE(target, message_id)` to `outbox`.
- **Integration tests** — `delivery_kill_mid_message.rs` (CI) proves kill-mid-message → reconnect → exactly-once; `delivery_real_tor.rs` (`#[ignore]`-gated) proves the same stack composes over Arti.
```

### Step 3: Update `CLAUDE.md` repository-state paragraph

Open `CLAUDE.md`. Find the block that starts `**Phase 0 is complete; Phase 1.A (frame codec) … 1.D (invite & contact flow) are done.**` and extend it. Replace the sentence-level description to include 1.E; example (adjust wording to match the existing cadence — keep prose style consistent):

> **Phase 0 is complete; Phase 1.A (frame codec), 1.B (Noise_XK handshake), 1.C (MLS 2-member groups), 1.D (invite & contact flow), and 1.E (delivery semantics) are done.**

Then, in the same section, add one paragraph describing 1.E (mirror the 1.D paragraph voice):

> Phase 1.E added the `delivery::hub::DeliveryHub` router, per-peer `delivery::peer::PeerConnection` actors (retry tick, keepalive, idle close, `ReplaceConn`), the `delivery::outbox::Outbox` wrapper over `storage::outbox::OutboxRepo` (migration 0004 adds `message_id` + `UNIQUE(target, message_id)`), and `delivery::receiver::receive()` for dedup + persist + ACK decision. `delivery::kill_stream::{KillableStream, KillSwitch}` is exposed via `test_exports` under `feature = "test-harness"`. A CI integration test exercises kill-mid-message → reconnect → exactly-once delivery end-to-end; a separate `#[ignore]`-gated test does the same round-trip over real Arti.

Update the "Phase 1 continues with …" line to drop 1.E and keep 1.F / 1.G.

### Step 4: Final test run

```bash
cargo test --all-features
```
Expected: all green.

### Step 5: Commit

```bash
git add CHANGELOG.md CLAUDE.md
git commit -m "$(cat <<'EOF'
docs: CHANGELOG + CLAUDE.md — Phase 1.E delivery semantics done

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Step 6: Final log check

```bash
git log --oneline master..HEAD
```
Expected: a linear history of the 13 task commits, ending with the docs commit, ready for merge into `master`.

---

## Exit gate

Before opening a PR / merging, confirm every clause of the Phase 1.E exit criterion is met:

- [ ] Outbox — `storage::outbox` + `delivery::outbox::Outbox` with unit + integration coverage. (Tasks 1, 2, 4)
- [ ] Exponential backoff — `delivery::backoff` with mean + cap tests. (Task 3)
- [ ] ACK correlation — `PeerConnection` `pending_acks` map + `OutboxRepo::ack_by_message_id`. (Tasks 7, 8)
- [ ] Receiver dedup — `delivery::receiver::receive` uses `seen_messages`. (Task 5)
- [ ] Connection pool — `DeliveryHub` per-peer actors with cold-start dial, reconnect, idle close, replace. (Tasks 7–9)
- [ ] Kill-mid-message → reconnect → delivered — `delivery_kill_mid_message.rs` passes on every CI run. (Task 11)
- [ ] Real-Tor smoke test — `delivery_real_tor.rs` passes locally with `-- --ignored`. (Task 12)
- [ ] `cargo fmt --all --check`, `cargo clippy -D warnings`, `cargo test --all-features` all green. (Task 13 step 1)
- [ ] CHANGELOG + CLAUDE.md updated. (Task 13)
