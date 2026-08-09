# #149 Attachment Janitor (first slice) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Auto-fail inbound attachment transfers that have stalled, and reclaim chunk directories that have no database row — without ever touching a completed attachment's chunks, which are the user's file.

**Architecture:** A new `daemon::attachment_janitor` module holds both mechanisms as one `run_once(...)` entry point, driven from the existing hourly `daemon::retention` sweep. Staleness is measured from the chunk directory's mtime (maintained free by `ChunkStore::put`'s rename), not from `created_at` — because `rearm_failed_in` does not touch `created_at`, so a creation-based policy would instantly re-fail a retried attachment.

**Tech Stack:** Rust 2021, tokio, rusqlite, tracing.

**Spec:** `docs/superpowers/specs/2026-08-09-issue-149-attachment-janitor-design.md`

**Branch:** `149-attachment-janitor` (created; spec committed as `feb2bb4`)

## Global Constraints

- **`status='complete'` is strictly out of scope for both mechanisms.** Deleting a received file is the one unacceptable failure mode. This must be a tested invariant, not an implicit one.
- **Auto-fail retains chunks.** They are #146's retry resume state. This slice deletes chunks *only* for orphans (directories with no row).
- **`STALL_GRACE = 14 days`**, `ORPHAN_GRACE = 1 hour`. Hardcoded constants with rationale, no config keys (matching the `MAX_WELCOME_AGE_MS` precedent).
- **Auto-fail transitions only via `AttachmentRepo::claim_terminal`** — preserving #38's exactly-once terminal gate. Never a bare `UPDATE`.
- **No migration, no new dependency, no wire-format change.**
- **Nothing fails silently** (#142): `info!` on reclaim/transition with counts, `warn!` on error, and the sweep continues to the next item rather than aborting the tick.
- **Tracing fields**: counts and `attachment_id` (hex) only. Never filenames or peer identity.
- **No `unwrap()`/`expect()` in library code.** `cargo clippy -D warnings` is the done-gate.
- **Every `.rs` file carries the GPLv3 SPDX header** (`// SPDX-License-Identifier: GPL-3.0-or-later` / `// Copyright (C) 2026 Myggiz B.V.`).
- **Every commit must be signed off** (`git commit -s`) — the repo enforces DCO.
- **Cargo is NOT on PATH:** prefix every cargo command with `. "$HOME/.cargo/env" && `.

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/core/src/storage/attachments.rs` | `AttachmentRepo` persistence. Gains an enumerator over every attachment id. | Modify: add `all_ids()` + test |
| `crates/core/src/daemon/attachment_janitor.rs` | **New.** Both janitor mechanisms + their tests. One `run_once()` entry point taking `now` as a parameter. | Create |
| `crates/core/src/daemon/mod.rs` | Module registration. | Modify: add `mod attachment_janitor;` |
| `crates/core/src/daemon/retention.rs` | Hourly sweep. Gains the janitor as a third step and two new params. | Modify |
| `crates/core/src/daemon/state.rs` | Four `spawn_sweep` call sites. | Modify ×4 |
| `crates/tests/src/history_sweep.rs` | Integration test calling `spawn_sweep`. | Modify: new args |
| `crates/core/src/lib.rs` | `test_exports` re-export of `spawn_sweep`. | Verify (may need no change) |
| `CLAUDE.md` | v1.0 limitations. | Modify: record auto-fail exists; failed/pending chunks still retained |

**Why a separate module rather than inlining in `retention.rs`:** the janitor's logic is worth testing without spawning a tokio task and sleeping. `run_once(pool, data_dir, events_tx, now)` is directly callable from a test. `retention.rs` stays a thin scheduler.

**Clock injection:** `run_once` takes `now: SystemTime` as a parameter rather than calling `SystemTime::now()` internally. This follows the house rule ("no I/O, clock, randomness, or env reads inside logic — take them as parameters and wire concretes up in `main`") and means tests never need to manipulate file mtimes: create a directory (mtime = real now), then pass a `now` 15 days in the future. **This is a deliberate improvement over the spec's §8**, which proposed `File::set_times`; setting mtime on a *directory* handle is not reliably portable, and injecting the clock is both simpler and more idiomatic here.

---

## Task 1: `AttachmentRepo::all_ids()`

**Files:**
- Modify: `crates/core/src/storage/attachments.rs` (add method near `list_pending_in`, add test to the existing `mod tests`)

**Interfaces:**
- Consumes: nothing new. Existing `AttachmentRepo::new(pool: &Pool)`, `insert(&self, attachment_id: &[u8;16], direction: &str, manifest: &[u8], total_chunks: i64, created_at: i64)`.
- Produces, used by Task 3: `pub fn all_ids(&self) -> Result<Vec<[u8; 16]>>` — every `attachment_id` in the table, any direction, any status.

- [ ] **Step 1: Write the failing test**

Add to the existing `mod tests` block at the bottom of `crates/core/src/storage/attachments.rs`:

```rust
    #[test]
    fn all_ids_returns_every_row_regardless_of_direction_or_status() {
        let pool = Pool::in_memory();
        let repo = AttachmentRepo::new(&pool);

        let a = [0xA1u8; 16]; // in / pending
        let b = [0xB2u8; 16]; // in / complete
        let c = [0xC3u8; 16]; // out / failed
        repo.insert(&a, "in", b"m", 1, 0).unwrap();
        repo.insert(&b, "in", b"m", 1, 0).unwrap();
        repo.insert(&c, "out", b"m", 1, 0).unwrap();
        repo.claim_terminal(&b, TerminalStatus::Complete).unwrap();
        repo.claim_terminal(&c, TerminalStatus::Failed).unwrap();

        let mut got = repo.all_ids().unwrap();
        got.sort();
        let mut want = vec![a, b, c];
        want.sort();
        assert_eq!(got, want, "all_ids must not filter by direction or status");
    }

    #[test]
    fn all_ids_is_empty_on_a_fresh_db() {
        let pool = Pool::in_memory();
        assert!(AttachmentRepo::new(&pool).all_ids().unwrap().is_empty());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib all_ids`
Expected: FAIL — `no method named all_ids found` (compile error).

- [ ] **Step 3: Implement `all_ids`**

Add immediately after `list_pending_in` in `crates/core/src/storage/attachments.rs`, matching that method's row-decoding style:

```rust
    /// Every `attachment_id` in the table — any direction, any status.
    ///
    /// Used by the janitor to distinguish a chunk directory that belongs to a
    /// known attachment from a true orphan. It deliberately does not filter:
    /// a directory whose row exists in *any* state must be spared.
    pub fn all_ids(&self) -> Result<Vec<[u8; 16]>> {
        self.pool.with(|c| {
            let mut stmt = c.prepare("SELECT attachment_id FROM attachments")?;
            let rows = stmt.query_map([], |r| {
                let aid: Vec<u8> = r.get(0)?;
                Ok(aid)
            })?;
            let mut out = Vec::new();
            for row in rows {
                let aid = row?;
                if aid.len() == 16 {
                    let mut id = [0u8; 16];
                    id.copy_from_slice(&aid);
                    out.push(id);
                }
            }
            Ok(out)
        })
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib all_ids`
Expected: PASS — 2 tests.

- [ ] **Step 5: Gate**

Run:
```bash
. "$HOME/.cargo/env" && cargo fmt --all -- --check \
  && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings
```
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/storage/attachments.rs
git commit -s -m "feat(storage): add AttachmentRepo::all_ids

Enumerates every attachment_id regardless of direction or status. The
janitor needs it to tell a chunk directory belonging to a known attachment
from a true orphan; it must not filter, because a directory whose row
exists in any state has to be spared.

Refs #149"
```

---

## Task 2: Janitor module + auto-fail (mechanism A)

**Files:**
- Create: `crates/core/src/daemon/attachment_janitor.rs`
- Modify: `crates/core/src/daemon/mod.rs` (register the module)

**Interfaces:**
- Consumes: `AttachmentRepo::{new, list_pending_in, claim_terminal}`, `TerminalStatus::Failed`, `Event::AttachmentFailed { attachment_id: Hex16, reason: String }`.
- Produces, used by Tasks 3 and 4:
  - `pub(crate) const STALL_GRACE: Duration`
  - `pub(crate) fn run_once(pool: &Pool, data_dir: &Path, events_tx: &broadcast::Sender<Event>, now: SystemTime) -> JanitorStats`
  - `pub(crate) struct JanitorStats { pub stalled: usize, pub orphans: usize }`

Task 3 fills in the orphan half of `run_once`; this task leaves `orphans` at 0.

- [ ] **Step 1: Write the failing tests**

Create `crates/core/src/daemon/attachment_janitor.rs` with the header, a test module, and nothing else yet:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Reclaims attachment state that no lane will ever finish.
//!
//! Two mechanisms, run from the hourly `daemon::retention` sweep:
//!
//! 1. **Auto-fail** — an inbound transfer whose chunk directory has not been
//!    written to for `STALL_GRACE` is claimed as `'failed'`, which makes it
//!    visible in the UI and retryable (#146) instead of silently stuck. Its
//!    chunks are **kept**: they are the resume state a retry needs.
//! 2. **Orphan sweep** — a chunk directory with no `attachments` row at all is
//!    removed once it is older than `ORPHAN_GRACE`.
//!
//! A **completed** attachment's chunks are the user's file (encrypted at rest;
//! plaintext only on demand), so `status='complete'` is out of scope for both.

use std::path::Path;
use std::time::{Duration, SystemTime};

use tokio::sync::broadcast;

use crate::daemon::events::Event;
use crate::storage::attachments::{AttachmentRepo, TerminalStatus};
use crate::storage::Pool;

/// How long an inbound transfer may go without a new chunk before it is
/// auto-failed.
///
/// Chunk deposits are made with `ttl=0`, which the mailbox resolves to its
/// `default_ttl_secs` of 7 days (`crates/mailbox/src/policy.rs`), so an offline
/// transfer can legitimately sit in flight for a week waiting for the receiver
/// to poll. 14 days is 2x that, so a transfer waiting on mailbox delivery is
/// never failed while its deposits could still arrive.
pub(crate) const STALL_GRACE: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// What one janitor pass reclaimed.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct JanitorStats {
    pub stalled: usize,
    pub orphans: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::attachments::AttachmentRepo;

    fn chunk_dir(data_dir: &Path, aid: &[u8; 16]) -> std::path::PathBuf {
        data_dir.join("attachments").join(hex::encode(aid))
    }

    /// Create a chunk directory with one chunk file in it, as `ChunkStore::put`
    /// would. Its mtime is "now" on the real filesystem clock.
    fn make_chunks(data_dir: &Path, aid: &[u8; 16]) {
        let d = chunk_dir(data_dir, aid);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("0"), b"ciphertext").unwrap();
    }

    fn events() -> broadcast::Sender<Event> {
        broadcast::channel::<Event>(16).0
    }

    /// `now` far enough in the future that anything created during the test is
    /// past both grace periods.
    fn way_later() -> SystemTime {
        SystemTime::now() + Duration::from_secs(15 * 24 * 60 * 60)
    }

    #[tokio::test]
    async fn stalled_pending_inbound_is_failed_and_its_chunks_are_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = Pool::in_memory();
        let aid = [0xA1u8; 16];
        AttachmentRepo::new(&pool)
            .insert(&aid, "in", b"m", 1, 0)
            .unwrap();
        make_chunks(tmp.path(), &aid);

        let stats = run_once(&pool, tmp.path(), &events(), way_later());

        assert_eq!(stats.stalled, 1);
        let row = AttachmentRepo::new(&pool).get(&aid).unwrap().unwrap();
        assert_eq!(row.status, "failed", "stalled transfer must be auto-failed");
        assert!(
            chunk_dir(tmp.path(), &aid).exists(),
            "chunks are retry resume state and must be kept"
        );
    }

    #[tokio::test]
    async fn pending_inside_the_grace_window_is_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = Pool::in_memory();
        let aid = [0xA2u8; 16];
        AttachmentRepo::new(&pool)
            .insert(&aid, "in", b"m", 1, 0)
            .unwrap();
        make_chunks(tmp.path(), &aid);

        // now == real now: the directory was just written, so it is fresh.
        let stats = run_once(&pool, tmp.path(), &events(), SystemTime::now());

        assert_eq!(stats.stalled, 0);
        let row = AttachmentRepo::new(&pool).get(&aid).unwrap().unwrap();
        assert_eq!(row.status, "pending");
    }

    #[tokio::test]
    async fn a_retried_attachment_is_not_immediately_refailed() {
        // The #149 regression guard. created_at is ancient (0) and
        // rearm_failed_in does not touch it, so a created_at-based policy would
        // re-fail this instantly. Fresh chunks must protect it.
        let tmp = tempfile::tempdir().unwrap();
        let pool = Pool::in_memory();
        let repo = AttachmentRepo::new(&pool);
        let aid = [0xA3u8; 16];
        repo.insert(&aid, "in", b"m", 1, 0).unwrap();
        repo.claim_terminal(&aid, TerminalStatus::Failed).unwrap();
        repo.rearm_failed_in(&aid).unwrap();
        make_chunks(tmp.path(), &aid); // retry wrote a chunk just now

        let stats = run_once(&pool, tmp.path(), &events(), SystemTime::now());

        assert_eq!(stats.stalled, 0, "a just-retried transfer must not be re-failed");
        assert_eq!(repo.get(&aid).unwrap().unwrap().status, "pending");
    }

    #[tokio::test]
    async fn completed_attachment_is_never_touched() {
        // The one unacceptable failure mode: a complete attachment's chunks are
        // the user's file.
        let tmp = tempfile::tempdir().unwrap();
        let pool = Pool::in_memory();
        let repo = AttachmentRepo::new(&pool);
        let aid = [0xC0u8; 16];
        repo.insert(&aid, "in", b"m", 1, 0).unwrap();
        repo.claim_terminal(&aid, TerminalStatus::Complete).unwrap();
        make_chunks(tmp.path(), &aid);

        let stats = run_once(&pool, tmp.path(), &events(), way_later());

        assert_eq!(stats.stalled, 0);
        assert_eq!(repo.get(&aid).unwrap().unwrap().status, "complete");
        assert!(
            chunk_dir(tmp.path(), &aid).exists(),
            "a completed attachment's chunks ARE the user's file"
        );
    }

    #[tokio::test]
    async fn pending_row_with_no_chunk_directory_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = Pool::in_memory();
        let aid = [0xA4u8; 16];
        AttachmentRepo::new(&pool)
            .insert(&aid, "in", b"m", 1, 0)
            .unwrap();
        // no make_chunks: nothing on disk to reclaim, no mtime to reason about

        let stats = run_once(&pool, tmp.path(), &events(), way_later());

        assert_eq!(stats.stalled, 0);
        assert_eq!(
            AttachmentRepo::new(&pool).get(&aid).unwrap().unwrap().status,
            "pending"
        );
    }

    #[tokio::test]
    async fn auto_fail_emits_attachment_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = Pool::in_memory();
        let aid = [0xA5u8; 16];
        AttachmentRepo::new(&pool)
            .insert(&aid, "in", b"m", 1, 0)
            .unwrap();
        make_chunks(tmp.path(), &aid);
        let tx = events();
        let mut rx = tx.subscribe();

        run_once(&pool, tmp.path(), &tx, way_later());

        match rx.try_recv() {
            Ok(Event::AttachmentFailed { attachment_id, .. }) => {
                assert_eq!(attachment_id.0, aid);
            }
            other => panic!("expected AttachmentFailed, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Register the module**

In `crates/core/src/daemon/mod.rs`, add alongside the other `mod` declarations (keep alphabetical order if the file uses it):

```rust
pub(crate) mod attachment_janitor;
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib attachment_janitor`
Expected: FAIL — `cannot find function run_once in this scope`.

- [ ] **Step 4: Implement `run_once` (auto-fail half)**

Add above the `#[cfg(test)] mod tests` block in `attachment_janitor.rs`:

```rust
/// Run one janitor pass. Returns what it reclaimed.
///
/// `now` is injected rather than read from the clock so the sweep is testable
/// without manipulating file mtimes.
///
/// Never returns `Err`: a failure on one attachment must not stop the pass, so
/// each is logged and skipped. Nothing here is silent (#142).
pub(crate) fn run_once(
    pool: &Pool,
    data_dir: &Path,
    events_tx: &broadcast::Sender<Event>,
    now: SystemTime,
) -> JanitorStats {
    let mut stats = JanitorStats::default();
    let repo = AttachmentRepo::new(pool);
    let root = data_dir.join("attachments");

    // --- Mechanism A: auto-fail stalled inbound transfers -------------------
    //
    // `list_pending_in` is already restricted to direction='in' AND
    // status='pending', so 'complete' and 'failed' rows are never considered.
    let pending = match repo.list_pending_in() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(err = %e, "janitor: listing pending inbound failed");
            Vec::new()
        }
    };

    for (aid, _manifest) in pending {
        let dir = root.join(hex::encode(aid));
        // No directory => no chunks on disk => nothing to reclaim and no
        // activity signal. Leave it alone.
        let Ok(meta) = std::fs::metadata(&dir) else {
            continue;
        };
        let Ok(mtime) = meta.modified() else {
            continue;
        };
        // `duration_since` errors when mtime is in the future (clock skew);
        // treat that as "just touched", i.e. not stalled.
        let Ok(age) = now.duration_since(mtime) else {
            continue;
        };
        if age <= STALL_GRACE {
            continue;
        }
        // Transition via claim_terminal only: it is the #38 exactly-once gate.
        // A false return means another lane already terminalised this row, and
        // that lane owns the event — stay silent.
        match repo.claim_terminal(&aid, TerminalStatus::Failed) {
            Ok(true) => {
                stats.stalled += 1;
                let _ = events_tx.send(Event::AttachmentFailed {
                    attachment_id: crate::daemon::hex::Hex16::from(aid),
                    reason: "transfer stalled".into(),
                });
            }
            Ok(false) => {}
            Err(e) => tracing::warn!(
                aid = %hex::encode(aid),
                err = %e,
                "janitor: auto-fail claim failed"
            ),
        }
    }

    if stats.stalled > 0 {
        tracing::info!(stalled = stats.stalled, "janitor: auto-failed stalled transfers");
    }
    stats
}
```

`Hex16::from(aid)` matches the existing construction at `crates/core/src/daemon/inbound.rs:891`, which is the other place `Event::AttachmentFailed` is emitted. (`Hex16` is `pub struct Hex16(pub [u8; 16])`, so the test's `attachment_id.0` field access is valid.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib attachment_janitor`
Expected: PASS — 6 tests.

- [ ] **Step 6: Gate**

Run:
```bash
. "$HOME/.cargo/env" && cargo fmt --all -- --check \
  && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings
```
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/daemon/attachment_janitor.rs crates/core/src/daemon/mod.rs
git commit -s -m "feat(daemon): auto-fail stalled inbound attachment transfers

A transfer whose sender vanished mid-flight stayed 'pending' forever, with
no auto-fail anywhere in the codebase — silently stuck, and invisible to the
user.

Staleness is measured from the chunk directory's mtime, which
ChunkStore::put maintains free via its rename. created_at cannot be used:
rearm_failed_in sets status='pending' without touching it, so a
creation-based policy would re-fail a just-retried attachment instantly.
A regression test covers exactly that case.

Chunks are deliberately KEPT — they are #146's retry resume state. The row
becomes visible and retryable rather than reclaimed.

Transitions go through claim_terminal, preserving #38's exactly-once gate.
'complete' is never considered: list_pending_in already excludes it, and a
test asserts a completed attachment's chunks survive, since those chunks are
the user's file.

Refs #149, #144, #38"
```

---

## Task 3: Orphan sweep (mechanism B)

**Files:**
- Modify: `crates/core/src/daemon/attachment_janitor.rs`

**Interfaces:**
- Consumes from Task 1: `AttachmentRepo::all_ids(&self) -> Result<Vec<[u8; 16]>>`.
- Consumes from Task 2: `run_once`, `JanitorStats`, the test helpers `chunk_dir` / `make_chunks` / `events` / `way_later`.
- Produces: `pub(crate) const ORPHAN_GRACE: Duration`, and `run_once` now populates `stats.orphans`.

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` in `attachment_janitor.rs`:

```rust
    #[tokio::test]
    async fn orphan_directory_with_no_row_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = Pool::in_memory();
        let aid = [0x0Fu8; 16];
        make_chunks(tmp.path(), &aid); // directory, but no row inserted

        let stats = run_once(&pool, tmp.path(), &events(), way_later());

        assert_eq!(stats.orphans, 1);
        assert!(!chunk_dir(tmp.path(), &aid).exists());
    }

    #[tokio::test]
    async fn directory_with_a_row_is_spared_in_every_status() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = Pool::in_memory();
        let repo = AttachmentRepo::new(&pool);

        let p = [0x11u8; 16]; // pending
        let f = [0x22u8; 16]; // failed
        let c = [0x33u8; 16]; // complete
        for (aid, dir) in [(p, "in"), (f, "in"), (c, "in")] {
            repo.insert(&aid, dir, b"m", 1, 0).unwrap();
            make_chunks(tmp.path(), &aid);
        }
        repo.claim_terminal(&f, TerminalStatus::Failed).unwrap();
        repo.claim_terminal(&c, TerminalStatus::Complete).unwrap();

        let stats = run_once(&pool, tmp.path(), &events(), way_later());

        assert_eq!(stats.orphans, 0, "a directory with a row is never an orphan");
        for aid in [p, f, c] {
            assert!(chunk_dir(tmp.path(), &aid).exists());
        }
    }

    #[tokio::test]
    async fn fresh_orphan_inside_the_grace_window_is_spared() {
        // Guards the window between create_dir_all and the row insert.
        let tmp = tempfile::tempdir().unwrap();
        let pool = Pool::in_memory();
        let aid = [0x44u8; 16];
        make_chunks(tmp.path(), &aid);

        let stats = run_once(&pool, tmp.path(), &events(), SystemTime::now());

        assert_eq!(stats.orphans, 0);
        assert!(chunk_dir(tmp.path(), &aid).exists());
    }

    #[tokio::test]
    async fn non_hex_directory_name_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = Pool::in_memory();
        let d = tmp.path().join("attachments").join("not-an-attachment-id");
        std::fs::create_dir_all(&d).unwrap();

        let stats = run_once(&pool, tmp.path(), &events(), way_later());

        assert_eq!(stats.orphans, 0);
        assert!(d.exists(), "the janitor only removes what it positively recognises");
    }

    #[tokio::test]
    async fn missing_attachments_root_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = Pool::in_memory();
        // no attachments/ directory at all — a fresh data dir
        let stats = run_once(&pool, tmp.path(), &events(), way_later());
        assert_eq!(stats, JanitorStats { stalled: 0, orphans: 0 });
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib attachment_janitor`
Expected: the four orphan tests FAIL (`stats.orphans` stays 0, directory still present); the Task 2 tests still pass.

- [ ] **Step 3: Add the constant**

Below `STALL_GRACE` in `attachment_janitor.rs`:

```rust
/// How old a chunk directory with no `attachments` row must be before it is
/// treated as an orphan.
///
/// `ChunkStore::put` calls `create_dir_all` before the row is guaranteed
/// visible to another connection; this grace closes that window rather than
/// relying on write ordering. Orphans are never urgent, so the bound is
/// generous.
pub(crate) const ORPHAN_GRACE: Duration = Duration::from_secs(60 * 60);
```

- [ ] **Step 4: Implement the orphan half**

Insert into `run_once`, after the auto-fail loop and before the `if stats.stalled > 0` logging:

```rust
    // --- Mechanism B: remove chunk directories with no row ------------------
    //
    // `all_ids` is unfiltered on purpose: a directory whose row exists in ANY
    // status must be spared, including 'complete', whose chunks are the file.
    let known: std::collections::HashSet<[u8; 16]> = match repo.all_ids() {
        Ok(v) => v.into_iter().collect(),
        Err(e) => {
            tracing::warn!(err = %e, "janitor: listing attachment ids failed; skipping orphan sweep");
            // Without the id set we cannot tell an orphan from a live
            // attachment. Deleting on a guess is unacceptable here, so skip.
            return finish(stats);
        }
    };

    match std::fs::read_dir(&root) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                // Only touch names that parse as an attachment id. Anything
                // else is not ours.
                let Ok(raw) = hex::decode(name) else { continue };
                let Ok(aid) = <[u8; 16]>::try_from(raw.as_slice()) else {
                    continue;
                };
                if known.contains(&aid) {
                    continue;
                }
                let Ok(meta) = entry.metadata() else { continue };
                if !meta.is_dir() {
                    continue;
                }
                let Ok(mtime) = meta.modified() else { continue };
                let Ok(age) = now.duration_since(mtime) else {
                    continue;
                };
                if age <= ORPHAN_GRACE {
                    continue;
                }
                match std::fs::remove_dir_all(entry.path()) {
                    Ok(()) => stats.orphans += 1,
                    Err(e) => tracing::warn!(
                        aid = %hex::encode(aid),
                        err = %e,
                        "janitor: removing orphan chunk dir failed"
                    ),
                }
            }
        }
        // A fresh data dir has no attachments/ yet. Not an error.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(err = %e, "janitor: reading attachment root failed"),
    }

    finish(stats)
```

Replace the existing tail of `run_once` (the `if stats.stalled > 0 { ... } stats`) with a call to a small helper added below `run_once`, so both exit paths log consistently:

```rust
/// Log what the pass reclaimed and hand back the stats. Quiet when idle — this
/// runs hourly.
fn finish(stats: JanitorStats) -> JanitorStats {
    if stats.stalled > 0 || stats.orphans > 0 {
        tracing::info!(
            stalled = stats.stalled,
            orphans = stats.orphans,
            "janitor: reclaimed attachment state"
        );
    }
    stats
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib attachment_janitor`
Expected: PASS — 11 tests.

- [ ] **Step 6: Gate**

Run:
```bash
. "$HOME/.cargo/env" && cargo fmt --all -- --check \
  && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings
```
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/daemon/attachment_janitor.rs
git commit -s -m "feat(daemon): reclaim orphaned attachment chunk directories

A chunk directory whose attachments row is gone was never reclaimed —
unbounded, invisible disk growth in the data dir.

The sweep spares anything it does not positively recognise: names that do
not parse as an attachment id, directories whose row exists in ANY status
(all_ids is deliberately unfiltered), and anything newer than ORPHAN_GRACE,
which covers the window between create_dir_all and the row insert. If the id
set cannot be read the sweep is skipped entirely rather than guessing —
deleting a live attachment's chunks is not an acceptable failure mode.

Refs #149"
```

---

## Task 4: Wire the janitor into the retention sweep

**Files:**
- Modify: `crates/core/src/daemon/retention.rs` (signature + third step + its 3 test call sites)
- Modify: `crates/core/src/daemon/state.rs` (4 call sites, each needing an ordering change)
- Modify: `crates/tests/src/history_sweep.rs` (1 call site)
- Verify: `crates/core/src/lib.rs:458` (`test_exports` re-export — likely unchanged)

**Interfaces:**
- Consumes from Tasks 2-3: `crate::daemon::attachment_janitor::run_once(pool, data_dir, events_tx, now)`.
- Produces: `spawn_sweep(pool, config, tick, shutdown, events_tx, data_dir)` — two new trailing parameters.

- [ ] **Step 1: Change the signature and add the third step**

In `crates/core/src/daemon/retention.rs`, extend `spawn_sweep`:

```rust
pub fn spawn_sweep(
    pool: Arc<Pool>,
    config: Arc<tokio::sync::RwLock<Config>>,
    tick: Duration,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    events_tx: tokio::sync::broadcast::Sender<crate::daemon::events::Event>,
    data_dir: std::path::PathBuf,
) -> tokio::task::JoinHandle<()> {
```

Then, inside the `tokio::select!` tick arm, after the outstanding-invite purge and before the closing brace of that arm:

```rust
                    // Attachment janitor (#149): auto-fail stalled inbound
                    // transfers and remove orphaned chunk directories.
                    let _ = crate::daemon::attachment_janitor::run_once(
                        &pool,
                        &data_dir,
                        &events_tx,
                        std::time::SystemTime::now(),
                    );
```

`run_once` logs its own outcome and cannot fail, so the return value is intentionally discarded here.

- [ ] **Step 2: Update the three call sites inside `retention.rs`'s own tests**

Each existing `spawn_sweep(...)` call in that file's `mod tests` gains two arguments. For each, add before the call:

```rust
        let tmp = tempfile::tempdir().unwrap();
        let (ev_tx, _ev_rx) = tokio::sync::broadcast::channel(16);
```

and extend the call:

```rust
        let h = spawn_sweep(
            pool.clone(),
            config_with_retention(0),
            Duration::from_millis(20),
            rx,
            ev_tx,
            tmp.path().to_path_buf(),
        );
```

(Use the same `config_with_retention(...)` argument each test already passes — only the two trailing arguments are new.)

- [ ] **Step 3: Update the four call sites in `state.rs`**

At each of the four sites, `spawn_sweep` is currently called **before** `let (events_tx, _) = broadcast::channel::<Event>(EVENT_CHANNEL_CAPACITY);`. **Move the `events_tx` channel creation above the `spawn_sweep` call**, then pass it plus the data dir:

```rust
        // Event broadcast channel — created before the sweep, which now emits
        // Event::AttachmentFailed from the attachment janitor (#149).
        let (events_tx, _) = broadcast::channel::<Event>(EVENT_CHANNEL_CAPACITY);

        // Phase 1.G: hourly retention sweep.
        let (sweep_shutdown_tx, sweep_shutdown_rx) = tokio::sync::watch::channel(false);
        let sweep_handle = crate::daemon::retention::spawn_sweep(
            pool.clone(),
            config_arc.clone(),
            std::time::Duration::from_secs(3600),
            sweep_shutdown_rx,
            events_tx.clone(),
            config.data_dir.clone(),
        );
```

Delete the now-duplicated original `let (events_tx, _) = ...` line at each site. Keep any comment that line carried by moving it with the statement.

`config` is still in scope at every one of the four sites — each builds `config_arc` from `config.clone()`, so the original is not moved — and `config.data_dir.clone()` is therefore valid directly in the call, exactly as shown above. No separate binding is needed.

- [ ] **Step 4: Update `crates/tests/src/history_sweep.rs`**

Add the two arguments the same way — a `tempfile::tempdir()` for the data dir and a throwaway broadcast channel:

```rust
    let tmp = tempfile::tempdir().unwrap();
    let (ev_tx, _ev_rx) = tokio::sync::broadcast::channel(16);
    let h = spawn_sweep(
        pool.clone(),
        config,
        Duration::from_millis(50),
        rx,
        ev_tx,
        tmp.path().to_path_buf(),
    );
```

Match the existing argument names in that file; only the two trailing arguments are new.

- [ ] **Step 5: Build and run the full suite**

Run:
```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness 2>&1 | tail -15
. "$HOME/.cargo/env" && cargo test -p skattr-tests 2>&1 | tail -15
```
Expected: all pass. `history_sweep` and the three retention tests must still pass — they are the proof the signature change did not break the existing sweep.

- [ ] **Step 6: Gate**

Run:
```bash
. "$HOME/.cargo/env" && cargo fmt --all -- --check \
  && cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings
```
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/daemon/retention.rs crates/core/src/daemon/state.rs crates/tests/src/history_sweep.rs
git commit -s -m "feat(daemon): run the attachment janitor from the hourly sweep

spawn_sweep gains events_tx and data_dir: the janitor needs the chunk-store
root, and auto-fail emits Event::AttachmentFailed so the UI reflects the
status change live rather than only on the next refresh.

All four state.rs call sites created the event channel a few lines AFTER
spawning the sweep, so the channel creation moves above the spawn.

Refs #149"
```

---

## Task 5: Docs, full gate, PR

**Files:**
- Modify: `CLAUDE.md` (the 3.C limitation bullet)

**Interfaces:** consumes the complete branch.

- [ ] **Step 1: Update the limitation in `CLAUDE.md`**

Find the bullet beginning **"3.C offline transfer is best-effort"** (it currently says a stalled inbound stays `pending` forever, "no auto-fail janitor — shared deferral with 3.B"). Replace that clause so it reads:

```markdown
- **3.C offline transfer is best-effort** — a deposited-but-never-fetched
  attachment is lost after the mailbox TTL (~7 days; the sender gets no fetch
  feedback so it never re-deposits). A stalled inbound is **no longer stuck
  forever**: the hourly janitor (#149) auto-fails an inbound transfer whose
  chunk directory has gone untouched for 14 days, which surfaces it in the UI
  as failed-and-retryable (#146) rather than silently pending. Large files
  (>10 MiB) cannot transfer while a peer is offline. All disclosed in the v1.0
  limitations.
```

Then find the bullet **"Orphaned chunks are never reclaimed"** (added by #150) and narrow it to what remains true:

```markdown
- **Chunks for failed/stalled transfers are still retained** — ⚠️ partial
  (#149). True orphans (a chunk directory with no `attachments` row) are now
  reclaimed by the hourly janitor, and a stalled transfer is auto-failed so the
  user can see and retry it. What is **not** reclaimed is the chunk data of a
  `'failed'` row: those chunks are the resume state that makes retry cheap, so
  deleting them needs an age policy and a durable failed-at signal that the
  schema does not carry (`created_at` is not touched by `rearm_failed_in`).
  #149 stays open for that half. A user who never retries keeps the partial
  file until they remove the attachment.
```

- [ ] **Step 2: Run the complete local gate**

```bash
. "$HOME/.cargo/env" \
  && cargo fmt --all -- --check \
  && cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings \
  && cargo test \
  && cargo clippy -p skattr-ui --all-targets -- -D warnings \
  && cargo deny check
```
Expected: every command exits 0. Capture the test counts for the PR body.

- [ ] **Step 3: Verify the two invariants by name**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --lib attachment_janitor 2>&1 | grep -E "completed_attachment_is_never_touched|directory_with_a_row_is_spared|a_retried_attachment_is_not_immediately_refailed|test result:"
```
Expected: those three tests present and passing. They are the safety case for this change.

- [ ] **Step 4: Commit docs**

```bash
git add CLAUDE.md
git commit -s -m "docs: record what the attachment janitor does and does not reclaim

The 3.C limitation said a stalled inbound stays pending forever; it is now
auto-failed after 14 days and becomes retryable. The orphan bullet from #150
is narrowed to what remains true: orphan directories are reclaimed, but a
failed row's chunks are deliberately retained as retry resume state, so #149
stays open for the age-GC half.

Refs #149"
```

- [ ] **Step 5: Push and open the PR**

```bash
git push -u origin 149-attachment-janitor
```

Open the PR with the gate output from Step 2 pasted in, and a body that states plainly:

- what this reclaims (orphan directories) and what it deliberately does not (chunks of failed/stalled rows),
- **`Refs #149`, NOT `Closes #149`** — the age-GC half is still outstanding,
- the mtime-vs-`created_at` reasoning, since that is the non-obvious part a reviewer needs,
- the `spawn_sweep` signature change and the channel-ordering move in `state.rs`.

CI now runs on PRs, so `check` / `ui` / `deny` plus DCO and Greptile will report on the PR itself.

---

## Self-Review

**1. Spec coverage**

| Spec section | Task |
|---|---|
| §3 mtime activity signal, retry-safety | Task 2 (impl + the `a_retried_attachment_is_not_immediately_refailed` guard) |
| §4 Mechanism A, `STALL_GRACE` = 14d, `claim_terminal` only, skip no-dir rows | Task 2 |
| §5 Mechanism B, `ORPHAN_GRACE` = 1h, `all_ids`, non-hex names spared | Tasks 1 and 3 |
| §6 wiring, `spawn_sweep` signature, `Event::AttachmentFailed`, reason string | Task 4 (+ event test in Task 2) |
| §7 observability — `info!` with counts, `warn!` on error, continue not abort | Tasks 2 and 3 (`finish` helper; every error arm warns and continues) |
| §8 tests 1-9 | Task 2 (1, 3, 4, 5) and Task 3 (2, 6, 7, 8, 9) |
| §9 acceptance — limitations documented | Task 5 |
| §10 exclusions — no migration, no config, no new dep | Enforced by Global Constraints; no task adds any |

No gaps. Spec test 1 ("complete never touched by auto-fail") is `completed_attachment_is_never_touched`; test 2 ("complete never touched by orphan sweep") is covered by `directory_with_a_row_is_spared_in_every_status`, which includes the complete case explicitly.

**2. Placeholder scan:** No TBD/TODO. Every code step carries literal code. Two conditional hedges in the first draft were resolved against the source rather than left to the implementer: `Hex16::from(aid)` is confirmed as the established construction (`daemon/inbound.rs:891`, and `Hex16` is a tuple struct with a public field so the test's `.0` access is valid), and `config` is confirmed still in scope at all four `state.rs` sites (each clones it into `config_arc`), so `config.data_dir.clone()` works inline. `tempfile` is already a workspace dev-dependency, so the tests need no new dep.

**3. Type consistency:** `run_once(&Pool, &Path, &broadcast::Sender<Event>, SystemTime) -> JanitorStats` is defined in Task 2 and used with that exact signature in Tasks 3 and 4. `JanitorStats { stalled, orphans }` field names are consistent across Tasks 2, 3 and their tests. `all_ids() -> Result<Vec<[u8; 16]>>` is defined in Task 1 and consumed in Task 3 with that type. `STALL_GRACE` / `ORPHAN_GRACE` are both `Duration`.

**Deliberate deviation from the spec:** §8 proposed setting directory mtimes with `File::set_times`. This plan injects `now: SystemTime` into `run_once` instead. Setting mtime on a *directory* handle is not reliably portable, and clock injection matches the house rule that logic takes its clock as a parameter. The tested behaviour is identical; only the mechanism differs.
