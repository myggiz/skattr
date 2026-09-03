# Outbox Delivery Truthfulness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a queued outbound message either reach the peer, reach their mailbox, or tell the user why it never will — instead of showing a clock forever.

**Architecture:** Three independent core fixes plus a durable outcome. The per-peer actor (`delivery/peer.rs`) gains a paced dialer and a deadline on its in-flight map, so a queued row causes a dial and un-wedges if never acked. A new sweeper (`delivery/outbox_sweep.rs`), sibling to `mailbox_sweeper`/`chunk_sweep`, owns queue lifecycle: it retargets aged rows onto the mailbox lane where one exists, and expires them only where no mailbox exists and the ±1h ts window makes delivery provably impossible. Two nullable columns carry the outcome so it survives a restart.

**Tech Stack:** Rust 2021 / Tokio / rusqlite (bundled, WAL) / Tauri 2 + SvelteKit 5 (runes) / vitest + Playwright.

**Spec:** `docs/superpowers/specs/2026-09-02-outbox-delivery-truthfulness-design.md`

## Global Constraints

- **No `unwrap()` / `expect()` in library code.** Use `?` and typed errors. Enforced by `clippy::unwrap_used` / `expect_used` with `-D warnings`. Test modules carry `#[allow(clippy::unwrap_used, clippy::expect_used)]` — follow the existing pattern at the top of each `mod tests`.
- **Every `.rs` file carries a licence header:** `// SPDX-License-Identifier: GPL-3.0-or-later` then `// Copyright (C) 2026 Myggiz B.V.`
- **Errors are our types, never a vendor's.** Return `crate::error::Result<T>`.
- **Model states as enums, not bool flags.**
- **Never log pubkeys, onions, or message contents at `info` or above.** Redaction by default.
- **TypeScript:** `strict`; no `any`, no `!`, no `ts-ignore`. `pnpm check` must pass at **0 errors / 0 warnings** (`--fail-on-warnings` is set).
- **`delivery`, `storage`, `mls`, `mailbox`, `transport` are `pub(crate)`.** Do not widen visibility; wrap in a public type from `daemon`/`envelope`/`contact` instead.
- **ADR 0006 (mailbox wire protocol) stays frozen.** No new frame types, no changed field meanings.
- **Local gate before any push:**
  ```bash
  . "$HOME/.cargo/env"
  cargo fmt --all -- --check
  cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings
  cargo test
  cargo clippy -p skattr-ui --all-targets -- -D warnings
  cd crates/ui/src-svelte && pnpm check && pnpm exec vitest run
  ```
- **Cargo is not on PATH.** Prefix every cargo invocation with `. "$HOME/.cargo/env" && `.
- **Run `cargo fmt --all` immediately after any scripted Rust edit.**

---

### Task 1: Un-wedge un-acked retry-tick sends (#229)

The retry tick inserts into `pending` with a dropped receiver, so nothing ever times it out. Only an `Ack` or a connection drop removes it, and the `pending.contains_key` guard then skips that row forever. Field-observed: `attempts` frozen at 1 across ~360 ticks on a healthy connection.

**Files:**
- Modify: `crates/core/src/delivery/peer.rs` (the `pending` declaration ~line 580; the retry-tick block ~lines 692–725)
- Test: `crates/core/src/delivery/peer.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: nothing later tasks depend on. Self-contained.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/core/src/delivery/peer.rs`. This drives the real actor against a peer that accepts frames and never acks, then asserts the row is retried rather than wedged.

```rust
/// #229: the retry tick inserts into `pending` with a dropped receiver, so
/// an un-acked send is never evicted and `pending.contains_key` skips that
/// row on every later tick — forever, while the connection stays healthy.
/// A peer that accepts frames and never ACKs must not permanently block
/// its own outbox row.
#[tokio::test(start_paused = true)]
async fn unacked_retry_tick_send_is_retried_not_wedged() {
    use crate::delivery::outbox::Outbox;

    let pool = std::sync::Arc::new(Pool::in_memory());
    let peer = PublicKey([0x77; 32]);
    let ob = Outbox::new(&pool);
    ob.enqueue(&peer, MessageId([0x01; 16]), b"payload", 0)
        .unwrap()
        .unwrap();

    // Peer end reads frames and NEVER sends an Ack.
    let (local, mut remote) = tokio::io::duplex(64 * 1024);
    let handle = spawn_full_actor_for_test(peer, local, pool.clone());

    // First delivery: the retry tick sends and inserts into `pending`.
    let first = read_one_frame(&mut remote).await;
    assert!(matches!(first, Frame::MlsApp(_)), "first send must go out");

    // Past the in-flight deadline, the row must be re-sent. Without the fix
    // the actor sits forever holding an entry nothing will ever remove.
    tokio::time::advance(std::time::Duration::from_secs(45)).await;
    let second = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        read_one_frame(&mut remote),
    )
    .await
    .expect("a wedged row never re-sends — #229");
    assert!(matches!(second, Frame::MlsApp(_)), "row must be retried");

    drop(handle);
}
```

If `spawn_full_actor_for_test` / `read_one_frame` helpers do not already exist in this `mod tests`, reuse whatever the neighbouring tests use — `retry_tick_picks_up_outbox_row_and_delivers` (~line 1472) already builds a full actor over a duplex and reads frames. Copy its construction verbatim rather than inventing a new harness.

- [ ] **Step 2: Run the test to verify it fails**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness unacked_retry_tick_send_is_retried_not_wedged -- --nocapture
```

Expected: FAIL — the second `read_one_frame` times out, because the row is wedged by its own `pending` entry.

- [ ] **Step 3: Give `pending` entries a deadline**

Change the `pending` map to carry the send instant. At the declaration (~line 580):

```rust
    // #229: the send instant rides along so an un-acked retry-tick send can
    // be evicted. Without it, only an `Ack` or a connection drop removes an
    // entry, and `pending.contains_key` below then skips that outbox row on
    // every later tick — forever, while the connection stays healthy.
    let mut pending: HashMap<
        MessageId,
        (
            oneshot::Sender<std::result::Result<(), ()>>,
            tokio::time::Instant,
        ),
    > = HashMap::new();
```

Update every `pending.insert(...)` to store the instant, e.g. in the `jobs` arm:

```rust
                pending.insert(job.message_id, (job.ack_tx, tokio::time::Instant::now()));
```

and in the `welcome_jobs` arm:

```rust
                pending.insert(synthetic_id, (wj.ack_tx, tokio::time::Instant::now()));
```

Update `drain_pending` and every site that removes an entry to expect a tuple (the ack sender is `.0`). Then, at the top of the retry-tick block, before reading due rows:

```rust
                // #229: evict in-flight entries the peer never acknowledged,
                // so their outbox rows become eligible again under their own
                // backoff. A row genuinely in flight is still protected for
                // the length of the deadline.
                let now_i = tokio::time::Instant::now();
                pending.retain(|_, (_, sent_at)| {
                    now_i.duration_since(*sent_at) < PENDING_INFLIGHT_DEADLINE
                });
```

Add the constant next to the other tick intervals (~line 547), matching the deadline the chunk-request path already uses:

```rust
/// How long a sent-but-unacknowledged message blocks its own outbox row
/// (#229). Same 30 s the chunk-request path uses. A peer that accepts a
/// frame and never ACKs must not wedge the row permanently.
const PENDING_INFLIGHT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness unacked_retry_tick_send_is_retried_not_wedged -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Run the full peer suite for regressions**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness delivery::peer
```

Expected: PASS, including `retry_tick_picks_up_outbox_row_and_delivers` and `dials must be paced by backoff`.

- [ ] **Step 6: Commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all
git add crates/core/src/delivery/peer.rs
git commit -m "fix(delivery): evict un-acked in-flight sends so their outbox row un-wedges (#229)"
```

---

### Task 2: Give the retry tick a paced dialer (#227)

The retry tick's due-rows loop does `let Some(c) = conn.as_mut() else { break; }` — with no connection it does nothing and never dials. Only the `jobs` arm and the #76 chunk block call `ensure_conn`, so a queued message waits for a connection someone else creates. The `arm_failure` half is load-bearing: today only the `jobs` arm arms `first_failure_at`, so the mailbox fallback is unreachable for a row merely sitting in the queue.

**Files:**
- Modify: `crates/core/src/delivery/peer.rs` (retry-tick block; new actor-local pacing state near `next_chunk_dial_at` ~line 617)
- Test: `crates/core/src/delivery/peer.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: `PENDING_INFLIGHT_DEADLINE` and the tuple-valued `pending` map from Task 1.
- Produces: nothing later tasks depend on.

- [ ] **Step 1: Write the failing test**

```rust
/// #227: the retry tick's due-rows loop breaks when there is no connection
/// and never dials, so a queued message waits for a connection something
/// else creates. With a dialer available, a due row must cause a dial.
#[tokio::test(start_paused = true)]
async fn retry_tick_dials_for_a_due_row_when_disconnected() {
    use crate::delivery::outbox::Outbox;

    let pool = std::sync::Arc::new(Pool::in_memory());
    let peer = PublicKey([0x88; 32]);
    Outbox::new(&pool)
        .enqueue(&peer, MessageId([0x02; 16]), b"payload", 0)
        .unwrap()
        .unwrap();

    // Actor starts with NO connection, but with a dialer that counts calls.
    let dials = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let dialer = counting_failing_dialer(dials.clone());
    let handle = spawn_full_actor_no_conn_for_test(peer, pool.clone(), dialer);

    tokio::time::advance(std::time::Duration::from_secs(5)).await;
    tokio::task::yield_now().await;

    assert!(
        dials.load(std::sync::atomic::Ordering::SeqCst) >= 1,
        "a due outbox row must cause a dial (#227); got 0"
    );
    drop(handle);
}

/// Dials must be paced. A failed Tor dial costs up to DIAL_TIMEOUT (30 s)
/// inline against a 1 s RETRY_TICK, so an unpaced dial is a storm against a
/// peer that is simply offline. Mirrors the existing assertion for the job
/// path.
#[tokio::test(start_paused = true)]
async fn retry_tick_dials_are_paced_by_backoff() {
    use crate::delivery::outbox::Outbox;

    let pool = std::sync::Arc::new(Pool::in_memory());
    let peer = PublicKey([0x89; 32]);
    Outbox::new(&pool)
        .enqueue(&peer, MessageId([0x03; 16]), b"payload", 0)
        .unwrap()
        .unwrap();

    let dials = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let dialer = counting_failing_dialer(dials.clone());
    let handle = spawn_full_actor_no_conn_for_test(peer, pool.clone(), dialer);

    // Ten minutes. The ladder is 15 s, 60 s, 300 s, 900 s (held), so a
    // correct actor issues roughly four dials, not six hundred.
    tokio::time::advance(std::time::Duration::from_secs(600)).await;
    tokio::task::yield_now().await;

    let n = dials.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        n <= 8,
        "dials must be paced by backoff, not issued per retry tick (got {n})"
    );
    drop(handle);
}
```

Build `counting_failing_dialer` and `spawn_full_actor_no_conn_for_test` from the existing helpers — the test at ~line 2779 (`per RETRY_TICK (1s) against an offline peer would be a dial storm`) already constructs a failing dialer and counts calls. Reuse its construction rather than writing a second one.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness retry_tick_dials -- --nocapture
```

Expected: `retry_tick_dials_for_a_due_row_when_disconnected` FAILS with "got 0". `retry_tick_dials_are_paced_by_backoff` passes vacuously (zero dials) — it is the guard that stops the fix from overshooting.

- [ ] **Step 3: Add the paced dial**

Alongside `next_chunk_dial_at` / `chunk_dial_step` (~line 617), add outbox-dial pacing state:

```rust
    // #227 dial pacing for queued outbox rows. Actor-local and not
    // persisted: a restarted actor gets a fresh schedule, which is correct —
    // a restart usually means conditions changed. Shares the ladder with the
    // #76 chunk dial rather than introducing a second pacing scheme.
    let mut next_outbox_dial_at: Option<tokio::time::Instant> = None;
    let mut outbox_dial_step: usize = 0;
```

In the retry tick, after computing `due` and before the send loop:

```rust
                // #227: a queued row has no live user action behind it, so
                // nothing else will ever give it a connection. Dial for it,
                // paced by backoff — a failed Tor dial burns up to
                // DIAL_TIMEOUT (30 s) inline against a 1 s tick.
                let has_direct_due = due.iter().any(|e| {
                    e.target == peer
                        && e.target_kind == crate::storage::outbox::OutboxTargetKind::Direct
                        && !pending.contains_key(&e.message_id)
                });
                if has_direct_due && conn.is_none() {
                    let ready = next_outbox_dial_at
                        .map(|t| tokio::time::Instant::now() >= t)
                        .unwrap_or(true);
                    if ready {
                        if ensure_conn::<S>(peer, &mut conn, &dialer).await {
                            next_outbox_dial_at = None;
                            outbox_dial_step = 0;
                            // Mirror the ReplaceConn arm: this is also
                            // "install a fresh connection", so idle/pong
                            // state from before the outage must not survive
                            // it and tear the new connection straight down.
                            last_traffic = tokio::time::Instant::now();
                            awaiting_pong_since = None;
                        } else {
                            // #227: arm the sustained-failure timer here too.
                            // Only the `jobs` arm armed it before, so the
                            // mailbox fallback was unreachable for a row
                            // merely sitting in the queue — which is the
                            // case that actually occurs.
                            arm_failure(&mut first_failure_at);
                            let idx = outbox_dial_step.min(CHUNK_DIAL_BACKOFF_MS.len() - 1);
                            // Deadline from now: the dial itself may have
                            // just burned up to DIAL_TIMEOUT.
                            next_outbox_dial_at = Some(
                                tokio::time::Instant::now()
                                    + std::time::Duration::from_millis(
                                        CHUNK_DIAL_BACKOFF_MS[idx],
                                    ),
                            );
                            outbox_dial_step =
                                (outbox_dial_step + 1).min(CHUNK_DIAL_BACKOFF_MS.len() - 1);
                        }
                    }
                }
```

Leave the existing `let Some(c) = conn.as_mut() else { break; };` in place — after a successful dial `conn` is `Some`, and after a failed one breaking is still correct.

Update the block comment at the top of the retry tick: it says the tick drives four jobs and calls the #76 dial "the deliberate exception". That is no longer true — say the tick now dials for queued rows as well, on the same ladder and for the same reason.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness retry_tick_dials -- --nocapture
```

Expected: both PASS.

- [ ] **Step 5: Run the full peer suite**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness delivery::peer
```

Expected: PASS. Watch `retry_tick_skips_mailbox_rows` in particular — `has_direct_due` must not fire on mailbox-kind rows.

- [ ] **Step 6: Commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all
git add crates/core/src/delivery/peer.rs
git commit -m "fix(delivery): dial for queued outbox rows, paced, and arm the fallback timer (#227)"
```

---

### Task 3: Persist the delivery outcome

Two nullable columns carry what cannot be derived: dismissal, and the failure *reason* (a reason computed at read time would be lost across a restart, leaving a bare "failed" with no remedy attached).

**Files:**
- Create: `crates/core/src/storage/migrations/0021_messages_delivery_outcome.sql`
- Modify: `crates/core/src/storage/migrations.rs` (append to `ALL_MIGRATIONS`, ~line 104)
- Modify: `crates/core/src/storage/messages.rs` (new repo methods)
- Test: `crates/core/src/storage/messages.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `MessageRepo::mark_failed(&self, envelope_id: &[u8; 16], reason: &str, now: i64) -> Result<bool>` — sets `failed_reason`; returns whether a row changed.
  - `MessageRepo::mark_dismissed(&self, envelope_id: &[u8; 16], now: i64) -> Result<bool>` — sets `dismissed_at`.
  - `MessageRepo::delivery_outcome(&self, envelope_id: &[u8; 16]) -> Result<(Option<i64>, Option<String>)>` — `(dismissed_at, failed_reason)`.

- [ ] **Step 1: Write the migration**

Create `crates/core/src/storage/migrations/0021_messages_delivery_outcome.sql`:

```sql
-- 0021: durable delivery outcome for outgoing messages.
--
-- dismissed_at  — the user dismissed a failed send. The row STAYS in
--                 history and in FTS; this only hides the actions and
--                 greys the bubble. Not derivable from anything else.
-- failed_reason — why the daemon gave up, stored rather than derived so
--                 the remedy survives a restart. A reason computed at
--                 read time would come back as a bare "failed".
--
-- Both nullable, mirroring the existing messages.delivered_at.
ALTER TABLE messages ADD COLUMN dismissed_at INTEGER;
ALTER TABLE messages ADD COLUMN failed_reason TEXT;
```

Register it in `crates/core/src/storage/migrations.rs`, immediately after the `0020` entry, following the existing shape exactly:

```rust
    Migration {
        version: 21,
        sql: include_str!("migrations/0021_messages_delivery_outcome.sql"),
    },
```

- [ ] **Step 2: Write the failing test**

Add to `mod tests` in `crates/core/src/storage/messages.rs`:

```rust
#[test]
fn failed_reason_and_dismissed_at_round_trip() {
    let pool = Pool::in_memory();
    let repo = MessageRepo::new(&pool);
    let eid = [0xAB; 16];
    insert_outgoing_test_row(&repo, &eid);

    // Fresh row: neither set.
    let (dismissed, reason) = repo.delivery_outcome(&eid).unwrap();
    assert_eq!(dismissed, None);
    assert_eq!(reason, None);

    // Giving up stores the reason, so the remedy survives a restart.
    assert!(repo
        .mark_failed(&eid, "this contact has no mailbox", 1_000)
        .unwrap());
    let (_, reason) = repo.delivery_outcome(&eid).unwrap();
    assert_eq!(reason.as_deref(), Some("this contact has no mailbox"));

    // Dismissal is independent and keeps the row.
    assert!(repo.mark_dismissed(&eid, 2_000).unwrap());
    let (dismissed, reason) = repo.delivery_outcome(&eid).unwrap();
    assert_eq!(dismissed, Some(2_000));
    assert_eq!(reason.as_deref(), Some("this contact has no mailbox"));
}

#[test]
fn dismiss_keeps_the_row_searchable() {
    let pool = Pool::in_memory();
    let repo = MessageRepo::new(&pool);
    let eid = [0xAC; 16];
    insert_outgoing_test_row_with_body(&repo, &eid, "findable haystack needle");

    repo.mark_dismissed(&eid, 1_000).unwrap();

    // Decision 4: Dismiss keeps the row in history AND in FTS. This is the
    // test that stops a later "tidy up" turning Dismiss into a delete.
    let hits = repo.search("needle", 10).unwrap();
    assert_eq!(hits.len(), 1, "a dismissed message must stay searchable");
}

#[test]
fn mark_failed_on_unknown_envelope_id_changes_nothing() {
    let pool = Pool::in_memory();
    let repo = MessageRepo::new(&pool);
    assert!(!repo.mark_failed(&[0xFF; 16], "nope", 1_000).unwrap());
}
```

Use whatever row-insertion and search helpers the surrounding tests in that file already use; if there is no `insert_outgoing_test_row`, build the row with the existing `InsertParams` construction that neighbouring tests use, and call the file's existing search entry point rather than inventing `search`.

- [ ] **Step 3: Run the tests to verify they fail**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness storage::messages
```

Expected: FAIL — `mark_failed` / `mark_dismissed` / `delivery_outcome` do not exist.

- [ ] **Step 4: Implement the repo methods**

In `crates/core/src/storage/messages.rs`, following the file's existing method style:

```rust
    /// Record that the daemon gave up on an outgoing message, storing the
    /// reason so the remedy survives a restart. Returns whether a row
    /// changed.
    pub(crate) fn mark_failed(
        &self,
        envelope_id: &[u8; 16],
        reason: &str,
        now: i64,
    ) -> Result<bool> {
        let conn = self.pool.conn()?;
        let n = conn.execute(
            "UPDATE messages SET failed_reason = ?2 \
             WHERE envelope_id = ?1 AND delivered_at IS NULL",
            rusqlite::params![&envelope_id[..], reason],
        )?;
        let _ = now;
        Ok(n > 0)
    }

    /// Mark a failed message dismissed. The row is KEPT — it stays in
    /// history and in FTS; this only hides the bubble's actions.
    pub(crate) fn mark_dismissed(&self, envelope_id: &[u8; 16], now: i64) -> Result<bool> {
        let conn = self.pool.conn()?;
        let n = conn.execute(
            "UPDATE messages SET dismissed_at = ?2 WHERE envelope_id = ?1",
            rusqlite::params![&envelope_id[..], now],
        )?;
        Ok(n > 0)
    }

    /// `(dismissed_at, failed_reason)` for one message.
    pub(crate) fn delivery_outcome(
        &self,
        envelope_id: &[u8; 16],
    ) -> Result<(Option<i64>, Option<String>)> {
        let conn = self.pool.conn()?;
        let row = conn
            .query_row(
                "SELECT dismissed_at, failed_reason FROM messages WHERE envelope_id = ?1",
                rusqlite::params![&envelope_id[..]],
                |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<String>>(1)?)),
            )
            .optional()?;
        Ok(row.unwrap_or((None, None)))
    }
```

Match the file's actual connection-access idiom (`self.pool.conn()?` vs a held `&Connection`) — copy it from an adjacent method rather than assuming. `mark_failed` drops `now` deliberately: the column stores the reason, not a timestamp; if the surrounding code has no use for the parameter, remove it from the signature and from the test rather than keeping a dead argument.

- [ ] **Step 5: Run the tests to verify they pass**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness storage::messages
```

Expected: PASS.

- [ ] **Step 6: Run the migration suite**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness storage::migrations
```

Expected: PASS. Several tests assert against `ALL_MIGRATIONS.len()` and the max version; they derive both, so they should follow automatically. If one hard-codes 20, update it to 21.

- [ ] **Step 7: Commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all
git add crates/core/src/storage/
git commit -m "feat(storage): migration 0021 — durable delivery outcome (dismissed_at, failed_reason)"
```

---

### Task 4: The outbox sweeper — retarget, then expire (#228)

Queue lifecycle belongs in a sweeper, not the peer actor: `full_run` has no events sender in production, `peer.rs` is already 3571 lines, and `mailbox_sweeper`/`chunk_sweep` are the established home for this shape of job.

Two rules, in order. A contact **with** a mailbox never expires — an aged direct row is retargeted onto the mailbox lane, which terminates on its own at `Deposited`. A contact **without** a mailbox has no terminal state at all, so past the ts window the row is deleted and the message marked failed.

**Files:**
- Create: `crates/core/src/delivery/outbox_sweep.rs`
- Modify: `crates/core/src/delivery/mod.rs` (declare the module)
- Modify: `crates/core/src/daemon/state.rs` (spawn + shutdown, mirroring `mailbox_sweeper_task` at ~lines 392–410 and its abort at ~587)
- Test: `crates/core/src/delivery/outbox_sweep.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: `MessageRepo::mark_failed` from Task 3. Existing: `OutboxRepo::{due, delete_by_id, set_mailbox_target}`, `MailboxRepo::list_for_contact`, `MailboxFallbackShared` (`hub.rs`, field `.events`), `crate::daemon::clock::now_unix_millis`.
- Produces: `pub(crate) async fn run_outbox_sweep(pool: &Pool, shared: &MailboxFallbackShared, now: i64, batch: usize)` and `pub(crate) const DIRECT_EXPIRY_MS: i64`.

- [ ] **Step 1: Write the failing tests**

Create `crates/core/src/delivery/outbox_sweep.rs` with the licence header, a `todo!()` body, and this test module. The two-lane pair is the point: test 2 is what stops a later simplification from expiring mailbox rows and quietly undoing the whole design.

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::daemon::events::{DeliveryStatus, Event};
    use crate::identity::PublicKey;

    /// No mailbox: direct is the only lane, and past the ts window it
    /// provably cannot deliver. The row is deleted and the message marked
    /// failed with a reason naming the actual remedy.
    #[tokio::test]
    async fn aged_row_without_mailbox_expires_with_a_reason() {
        let (pool, shared, mut events) = fixture_no_mailbox();
        let peer = PublicKey([0x11; 32]);
        let eid = seed_outgoing_message(&pool, peer, "hi", /* ts */ 0);
        seed_direct_outbox_row(&pool, peer, eid);

        // now is past DIRECT_EXPIRY_MS relative to the envelope ts.
        run_outbox_sweep(&pool, &shared, DIRECT_EXPIRY_MS + 1, 32).await;

        assert!(
            crate::storage::outbox::OutboxRepo::new(&pool)
                .due(i64::MAX, 10)
                .unwrap()
                .is_empty(),
            "an unreachable row must not be retried forever"
        );
        let (_, reason) = crate::storage::messages::MessageRepo::new(&pool)
            .delivery_outcome(&eid)
            .unwrap();
        let reason = reason.expect("failure reason must be stored, not derived");
        assert!(
            reason.contains("mailbox"),
            "the reason must name the remedy; got {reason:?}"
        );

        match events.try_recv() {
            Ok(Event::DeliveryStatusChanged { status: DeliveryStatus::Failed(_), .. }) => {}
            other => panic!("expected exactly one Failed event, got {other:?}"),
        }
        assert!(events.try_recv().is_err(), "exactly one event");
    }

    /// Decision 1: with a mailbox, a queued message NEVER expires. It is
    /// retargeted onto the mailbox lane, which terminates on its own at
    /// Deposited. Expiring it here would defeat the point of having a
    /// mailbox at all.
    #[tokio::test]
    async fn aged_row_with_mailbox_is_retargeted_never_expired() {
        let (pool, shared, _events) = fixture_with_mailbox("mb1.onion");
        let peer = PublicKey([0x22; 32]);
        let eid = seed_outgoing_message(&pool, peer, "hi", /* ts */ 0);
        let row_id = seed_direct_outbox_row(&pool, peer, eid);

        run_outbox_sweep(&pool, &shared, DIRECT_EXPIRY_MS + 1, 32).await;

        let row = crate::storage::outbox::OutboxRepo::new(&pool)
            .get(row_id)
            .unwrap()
            .expect("a mailbox contact's row must NOT be deleted");
        assert_eq!(
            row.target_kind,
            crate::storage::outbox::OutboxTargetKind::Mailbox,
            "an aged direct row for a mailbox contact must be retargeted"
        );
        let (_, reason) = crate::storage::messages::MessageRepo::new(&pool)
            .delivery_outcome(&eid)
            .unwrap();
        assert_eq!(reason, None, "a mailbox contact's message must not fail");
    }

    /// Rows inside the window are untouched on both lanes.
    #[tokio::test]
    async fn fresh_row_is_left_alone() {
        let (pool, shared, mut events) = fixture_no_mailbox();
        let peer = PublicKey([0x33; 32]);
        let eid = seed_outgoing_message(&pool, peer, "hi", /* ts */ 0);
        let row_id = seed_direct_outbox_row(&pool, peer, eid);

        run_outbox_sweep(&pool, &shared, DIRECT_EXPIRY_MS - 60_000, 32).await;

        assert!(crate::storage::outbox::OutboxRepo::new(&pool)
            .get(row_id)
            .unwrap()
            .is_some());
        assert!(events.try_recv().is_err(), "no event for a fresh row");
    }

    /// Sweeping twice must not emit a second Failed.
    #[tokio::test]
    async fn expiry_is_idempotent() {
        let (pool, shared, mut events) = fixture_no_mailbox();
        let peer = PublicKey([0x44; 32]);
        let eid = seed_outgoing_message(&pool, peer, "hi", 0);
        seed_direct_outbox_row(&pool, peer, eid);

        run_outbox_sweep(&pool, &shared, DIRECT_EXPIRY_MS + 1, 32).await;
        let _ = events.try_recv();
        run_outbox_sweep(&pool, &shared, DIRECT_EXPIRY_MS + 1, 32).await;
        assert!(events.try_recv().is_err(), "no duplicate Failed on re-sweep");
    }
}
```

For the fixtures, copy the construction in `mailbox_sweeper.rs`'s test module verbatim — it already builds a `Pool::in_memory()`, a `StubFactory`, a `broadcast::channel::<Event>`, a `MailboxFallbackShared`, and (in `list_for_contact_returns_card_mailboxes`, `storage/mailboxes.rs:557`) a signed `ContactCard` carrying a mailbox list. `fixture_no_mailbox` is the same with a contact whose `card` is `None`.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness delivery::outbox_sweep
```

Expected: FAIL — `todo!()` panics.

- [ ] **Step 3: Implement the sweeper**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Outbox queue lifecycle: retarget aged direct rows onto the mailbox lane
//! where one exists, and expire them only where none does.
//!
//! Sibling to [`mailbox_sweeper`](crate::delivery::mailbox_sweeper) and
//! [`chunk_sweep`](crate::delivery::chunk_sweep). This is deliberately not
//! in the per-peer actor: queue lifetime is not connection work, and the
//! actor has no events sender in production.
//!
//! Two rules, in order:
//!
//! 1. **The contact advertises a mailbox** — never expire. The mailbox lane
//!    is exempt from the ±1h `ts` window by design (2.C) and terminates on
//!    its own at `Deposited`, so a queued message can wait as long as it
//!    needs to. That is the entire point of having a mailbox.
//! 2. **The contact advertises no mailbox** — direct is the only lane, and
//!    past the window the peer will certainly reject the envelope
//!    ([`receiver`](crate::delivery::receiver) enforces it). The row is
//!    deleted and the message marked failed, with a reason naming the
//!    remedy.

use crate::delivery::hub::MailboxFallbackShared;
use crate::delivery::receiver::REPLAY_WINDOW_MS;
use crate::storage::Pool;

/// Stop a margin short of the window so a message is never written to the
/// wire that the peer will certainly reject. Derived from
/// [`REPLAY_WINDOW_MS`] rather than written out, so the two cannot drift.
pub(crate) const DIRECT_EXPIRY_MS: i64 = REPLAY_WINDOW_MS - 5 * 60 * 1000;

/// Shown on the failed bubble. Names the remedy, not just the symptom.
const NO_MAILBOX_REASON: &str =
    "Not delivered — this contact has no mailbox, so messages cannot reach \
     them while they are offline.";

pub(crate) async fn run_outbox_sweep(
    pool: &Pool,
    shared: &MailboxFallbackShared,
    now: i64,
    batch: usize,
) {
    use crate::storage::outbox::{OutboxRepo, OutboxTargetKind};

    let outbox = OutboxRepo::new(pool);
    let rows = match outbox.due(now, batch) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(target: "skattr::delivery::outbox_sweep", error = %e, "due failed");
            return;
        }
    };

    let messages = crate::storage::messages::MessageRepo::new(pool);
    let mailboxes = crate::storage::mailboxes::MailboxRepo::new(pool);

    for row in rows {
        if row.target_kind != OutboxTargetKind::Direct {
            continue; // the mailbox lane owns its own retries
        }
        // The envelope ts lives on the message, joined by
        // outbox.message_id == messages.envelope_id.
        let Ok(Some(envelope_ts)) = messages.envelope_ts(&row.message_id) else {
            continue;
        };
        if now.saturating_sub(envelope_ts) < DIRECT_EXPIRY_MS {
            continue; // still inside the window
        }

        let peer = crate::identity::PublicKey(match row.target.as_slice().try_into() {
            Ok(k) => k,
            Err(_) => continue,
        });

        let has_mailbox = mailboxes
            .list_for_contact(&peer)
            .map(|m| !m.is_empty())
            .unwrap_or(false);

        if has_mailbox {
            // Rule 1. Retarget rather than no-op: leaving it assumes the
            // direct-timeout fallback will move it, and if that never
            // happens the row ages out still marked direct — the original
            // bug in a smaller window.
            if let Err(e) = crate::delivery::hub::retarget_to_mailbox(pool, &peer, &row).await {
                tracing::warn!(
                    target: "skattr::delivery::outbox_sweep",
                    error = %e,
                    "retarget to mailbox failed; leaving row for the next sweep"
                );
            }
            continue;
        }

        // Rule 2. Delete first, then record. If the process dies between
        // the two the row is gone and the message shows as unknown, which
        // is recoverable; the reverse would leave a failed message that is
        // still being retried.
        match outbox.delete_by_id(row.id) {
            Ok(true) => {}
            Ok(false) => continue, // another sweep won the race
            Err(e) => {
                tracing::warn!(target: "skattr::delivery::outbox_sweep", error = %e, "delete failed");
                continue;
            }
        }
        if let Err(e) = messages.mark_failed(&row.message_id, NO_MAILBOX_REASON) {
            tracing::warn!(target: "skattr::delivery::outbox_sweep", error = %e, "mark_failed failed");
        }
        // Redaction: no pubkey, no onion, no body.
        tracing::info!(
            target: "skattr::delivery::outbox_sweep",
            "outbox: gave up on a direct message — contact advertises no mailbox"
        );
        let _ = shared.events.send(crate::daemon::events::Event::DeliveryStatusChanged {
            message: crate::daemon::hex::Hex16::from(row.message_id),
            status: crate::daemon::events::DeliveryStatus::Failed(NO_MAILBOX_REASON.to_string()),
        });
    }
}
```

Two supporting pieces this needs:

1. `MessageRepo::envelope_ts(&self, envelope_id: &[u8; 16]) -> Result<Option<i64>>` — add it in `storage/messages.rs` next to Task 3's methods (`SELECT ts FROM messages WHERE envelope_id = ?1`, `.optional()?`).
2. `hub::retarget_to_mailbox(pool, peer, row) -> Result<()>` — `hub.rs` already does exactly this inside `run_mailbox_fallback` (find the direct row, `set_mailbox_target`). Extract that existing step into a `pub(crate)` function and call it from both places rather than writing a second copy. If extraction turns out to be more than a straight lift, call `run_mailbox_fallback` here instead and say so in a comment.

Check `Event::DeliveryStatusChanged`'s actual field names against `daemon/events.rs:74` before writing the emit — the plan assumes `message` and `status`, matching `hub.rs:595`.

Declare the module in `crates/core/src/delivery/mod.rs` alongside its siblings:

```rust
pub(crate) mod outbox_sweep;
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness delivery::outbox_sweep
```

Expected: all four PASS.

- [ ] **Step 5: Spawn it in `run_with_transport`**

In `crates/core/src/daemon/state.rs`, directly after the `mailbox_sweeper_task` block (~line 410), mirroring its shape:

```rust
    let outbox_sweep_pool = pool.clone();
    let outbox_sweep_shared = fallback_shared.clone();
    let outbox_sweep_task = tokio::spawn(async move {
        // A minute is ample: the deadline is 55 minutes, so tick precision
        // is irrelevant and a slower tick keeps the query off the hot path.
        const SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(60);
        let mut t = tokio::time::interval(SWEEP_EVERY);
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            t.tick().await;
            let now = crate::daemon::clock::now_unix_millis();
            crate::delivery::outbox_sweep::run_outbox_sweep(
                &outbox_sweep_pool,
                &outbox_sweep_shared,
                now,
                32,
            )
            .await;
        }
    });
```

And in teardown, beside the other aborts (~line 587):

```rust
    outbox_sweep_task.abort();
    let _ = outbox_sweep_task.await;
```

- [ ] **Step 6: Verify the whole core suite**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all
git add crates/core/src/delivery/ crates/core/src/daemon/state.rs crates/core/src/storage/messages.rs
git commit -m "feat(delivery): outbox_sweep — retarget to mailbox, expire only when there is none (#228)"
```

---

### Task 5: Carry the outcome to the wire

**Files:**
- Modify: `crates/core/src/daemon/commands.rs` (`MessageRecord` ~line 471; new builder after `project` ~line 530)
- Modify: `crates/core/src/daemon/dispatch.rs:905`, `:1171` and `crates/core/src/daemon/ipc/server/mod.rs:442` (the read-from-storage projection sites)
- Test: `crates/core/src/daemon/commands.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: `MessageRepo::delivery_outcome` from Task 3.
- Produces: `MessageRecord.dismissed_at: Option<u64>`, `MessageRecord.failed_reason: Option<String>`, and `MessageRecord::with_persisted_status(self, dismissed_at: Option<i64>, failed_reason: Option<String>) -> Self`. The regenerated `MessageRecord.ts` gains both fields — Task 7 consumes them.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn with_persisted_status_attaches_outcome_without_touching_project() {
    let rec = sample_outgoing_record(); // existing helper in this mod
    assert_eq!(rec.dismissed_at, None);
    assert_eq!(rec.failed_reason, None);

    let rec = rec.with_persisted_status(Some(1_700), Some("no mailbox".into()));
    assert_eq!(rec.dismissed_at, Some(1_700));
    assert_eq!(rec.failed_reason.as_deref(), Some("no mailbox"));
}
```

- [ ] **Step 2: Run it to verify it fails**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness with_persisted_status
```

Expected: FAIL — no such fields or method.

- [ ] **Step 3: Add the fields and the builder**

To `MessageRecord`, after `delivered_at`:

```rust
    /// When the user dismissed a failed send (unix millis). The row is
    /// KEPT — dismissal only hides the bubble's actions and greys it.
    /// Sender-side only.
    pub dismissed_at: Option<u64>,
    /// Why the daemon gave up, if it did. Stored rather than derived so
    /// the remedy survives a restart. Sender-side only.
    pub failed_reason: Option<String>,
```

Set both to `None` inside `project`, then add:

```rust
    /// Attach persisted delivery outcome to a projection read from storage.
    /// Live-emit call sites never have these and do not call it.
    #[must_use]
    pub fn with_persisted_status(
        mut self,
        dismissed_at: Option<i64>,
        failed_reason: Option<String>,
    ) -> Self {
        self.dismissed_at = dismissed_at.and_then(|t| u64::try_from(t).ok());
        self.failed_reason = failed_reason;
        self
    }
```

At each of the three read-from-storage sites, look up the outcome and chain the builder, e.g.:

```rust
        let (dismissed_at, failed_reason) = messages.delivery_outcome(&envelope.id.0)?;
        records.push(
            MessageRecord::project(/* … unchanged … */)
                .with_persisted_status(dismissed_at, failed_reason),
        );
```

Leave `inbound.rs:343`, `dispatch.rs:734` and `dispatch.rs:1017` untouched — those are live emits that can never have an outcome yet.

If the per-row `delivery_outcome` call shows up as an N+1 on the list path (`dispatch.rs:1171` paginates history), fold the two columns into that function's existing row query instead of calling per row. Prefer the join; only fall back to per-row if the existing query shape makes that genuinely awkward.

- [ ] **Step 4: Run it to verify it passes**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness with_persisted_status
```

Expected: PASS.

- [ ] **Step 5: Regenerate the TypeScript bindings and confirm the new fields**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness export_bindings
grep -n "dismissed_at\|failed_reason" crates/ui/src-svelte/src/lib/ipc/types/MessageRecord.ts
```

Expected: both fields present. ts-rs export is driven by `#[ts(export)]` and runs as part of the test suite; if that grep is empty, run the full `cargo test -p skattr-core` and re-check.

- [ ] **Step 6: Commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all
git add crates/core/src/daemon/ crates/ui/src-svelte/src/lib/ipc/types/
git commit -m "feat(ipc): carry dismissed_at and failed_reason on MessageRecord"
```

---

### Task 6: `Command::DismissMessage`

Additive, append-only, local IPC — the same shape as 4.C's `ExportBackup`. Resend needs no command: it is `send()` with the original body, which the UI already has.

**Files:**
- Modify: `crates/core/src/daemon/commands.rs` (add the variant after `RetryAttachment` ~line 369)
- Modify: `crates/core/src/daemon/dispatch.rs` (dispatch arm ~line 83; handler beside `handle_remove_mailbox`)
- Test: `crates/core/src/daemon/dispatch.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: `MessageRepo::mark_dismissed` (Task 3).
- Produces: `Command::DismissMessage { message_id: Hex16 }` and the generated `Command.ts` variant, consumed by Task 7.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn dismiss_message_marks_the_row_and_keeps_it() {
    let (handle, _tmp) = test_handle().await; // existing helper in this mod
    let eid = seed_failed_outgoing_message(&handle).await;

    let result = dispatch(
        handle.clone(),
        Command::DismissMessage { message_id: Hex16::from(eid) },
    )
    .await
    .unwrap();
    assert!(matches!(result, CommandResult::Ok));

    // Decision 4: the row is KEPT.
    let (dismissed, _) = MessageRepo::new(&handle.pool).delivery_outcome(&eid).unwrap();
    assert!(dismissed.is_some(), "dismissal must be recorded");
    assert!(
        message_row_exists(&handle.pool, &eid),
        "Dismiss keeps the row — it is not a delete"
    );
}
```

Use whichever success variant `CommandResult` already has for side-effect-only commands — check what `handle_remove_mailbox` returns and match it rather than assuming `CommandResult::Ok` exists.

- [ ] **Step 2: Run it to verify it fails**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness dismiss_message
```

Expected: FAIL — no such variant.

- [ ] **Step 3: Add the command and handler**

In `commands.rs`, after `RetryAttachment`:

```rust
    /// Mark a failed outgoing message dismissed. The row is KEPT — it
    /// stays in history and in FTS; this only hides the bubble's actions.
    DismissMessage {
        /// 16-byte envelope id of the message to dismiss.
        message_id: crate::daemon::hex::Hex16,
    },
```

In `dispatch.rs`, the arm:

```rust
        Command::DismissMessage { message_id } => dismiss_message(&handle, message_id).await,
```

and the handler, following the style of the neighbouring ones:

```rust
async fn dismiss_message(
    handle: &DaemonHandle,
    message_id: crate::daemon::hex::Hex16,
) -> Result<CommandResult, IpcError> {
    let now = crate::daemon::clock::now_unix_millis();
    crate::storage::messages::MessageRepo::new(&handle.pool)
        .mark_dismissed(&message_id.0, now)
        .map_err(|e| {
            // Log the category only — never the error's payload (4.D item 1).
            tracing::warn!(kind = %e.kind_str(), "dismiss_message failed");
            IpcError::Internal
        })?;
    Ok(CommandResult::Ok)
}
```

Match the file's actual error-mapping idiom for `IpcError` — copy it from an adjacent handler. The redaction requirement is real: log the category, not the error.

- [ ] **Step 4: Run it to verify it passes**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness dismiss_message
```

Expected: PASS.

- [ ] **Step 5: Confirm the binding regenerated**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness
grep -n "dismiss_message" crates/ui/src-svelte/src/lib/ipc/types/Command.ts
```

Expected: the variant is present.

- [ ] **Step 6: Commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all
git add crates/core/src/daemon/ crates/ui/src-svelte/src/lib/ipc/types/
git commit -m "feat(ipc): Command::DismissMessage — keeps the row, hides the actions"
```

---

### Task 7: The failed and dismissed bubble

The failed *look* already exists end to end — `DeliveryIcon` renders `alert-triangle` for `"failed"`, `deliveryToIconStatus` already maps `{Failed: string}`, and #197 deliberately made the icon shape carry the state so it survives a light theme. This task makes it reachable and adds the two actions.

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/stores/conversation.ts` (hydration ~line 143–150; new `dismiss` action)
- Modify: `crates/ui/src-svelte/src/lib/components/MessageBubble.svelte`
- Test: `crates/ui/src-svelte/src/lib/components/MessageBubble.test.ts`, `crates/ui/src-svelte/src/lib/stores/conversation.test.ts`

**Interfaces:**
- Consumes: `MessageRecord.dismissed_at`, `MessageRecord.failed_reason` (Task 5); `Command::DismissMessage` (Task 6).
- Produces: nothing later tasks depend on.

- [ ] **Step 1: Write the failing tests**

In `conversation.test.ts`:

```ts
it("seeds Failed from a record's failed_reason on load", () => {
  const rec = makeRecord({
    direction: "outgoing",
    delivered_at: null,
    failed_reason: "Not delivered — this contact has no mailbox.",
  });
  hydrateDeliveryFromRecords([rec]);
  const s = statusForMessageHex(hex16ToString(rec.message_id));
  expect(s).toEqual({ Failed: "Not delivered — this contact has no mailbox." });
});

it("prefers delivered over a stale failed_reason", () => {
  const rec = makeRecord({
    direction: "outgoing",
    delivered_at: 1700n,
    failed_reason: "stale",
  });
  hydrateDeliveryFromRecords([rec]);
  expect(statusForMessageHex(hex16ToString(rec.message_id))).toBe("Delivered");
});
```

In `MessageBubble.test.ts`:

```ts
it("renders the failure reason and both actions on a failed message", () => {
  const rec = makeOutgoing({ failed_reason: "Not delivered — no mailbox." });
  render(MessageBubble, { props: { record: rec, grouped: false } });

  expect(screen.getByText(/no mailbox/i)).toBeInTheDocument();
  expect(screen.getByRole("button", { name: /resend/i })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: /dismiss/i })).toBeInTheDocument();
});

it("a dismissed message keeps its text but offers no actions", () => {
  const rec = makeOutgoing({
    failed_reason: "Not delivered — no mailbox.",
    dismissed_at: 1700n,
  });
  render(MessageBubble, { props: { record: rec, grouped: false } });

  expect(screen.getByText(rec.kind.body)).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: /resend/i })).toBeNull();
  expect(screen.queryByRole("button", { name: /dismiss/i })).toBeNull();
});
```

Match the file's existing render/helper conventions (`makeOutgoing`, `makeRecord`) rather than introducing new ones.

- [ ] **Step 2: Run them to verify they fail**

```bash
cd crates/ui/src-svelte && pnpm exec vitest run MessageBubble conversation
```

Expected: FAIL.

- [ ] **Step 3: Extend the hydration**

In `conversation.ts`, the existing loop at ~line 147 already seeds `Delivered` from `delivered_at`. Extend it — delivered wins, because an ack is ground truth and a `failed_reason` could be stale from an earlier attempt:

```ts
    if (r.direction === "outgoing") {
      const hex = hex16ToString(r.message_id);
      if (r.delivered_at !== null && r.delivered_at !== undefined) {
        recordDeliveryStatus(hex, "Delivered");
      } else if (r.failed_reason !== null && r.failed_reason !== undefined) {
        // Durable: the daemon stored the reason when it gave up, so the
        // remedy survives a restart rather than coming back as a bare clock.
        recordDeliveryStatus(hex, { Failed: r.failed_reason });
      }
    }
```

Add the dismiss action:

```ts
export async function dismiss(messageId: Hex16): Promise<void> {
  await ipcClient.request({ cmd: "dismiss_message", message_id: messageId });
  conversation.update((s) => ({
    ...s,
    messages: s.messages.map((m) =>
      m.message_id === messageId ? { ...m, dismissed_at: BigInt(Date.now()) } : m,
    ),
  }));
}
```

Check the generated `Command.ts` for the exact request shape and field naming before writing the call.

- [ ] **Step 4: Add the actions to the bubble**

In `MessageBubble.svelte`, alongside the existing `iconStatus` derivation:

```svelte
  let dismissed = $derived(record.dismissed_at !== null && record.dismissed_at !== undefined);
  let failureReason = $derived(
    !dismissed && iconStatus === "failed" ? (record.failed_reason ?? null) : null,
  );
```

and in the markup, after `.meta`:

```svelte
    {#if failureReason}
      <p class="failure">{failureReason}</p>
      <div class="failure-actions">
        <button type="button" onclick={() => resend(record)}>Resend</button>
        <button type="button" onclick={() => dismiss(record.message_id)}>Dismiss</button>
      </div>
    {/if}
```

`resend` calls the existing `send(record.contact, body)` — the resent message is a genuinely new one and appends at the bottom, per decision 5. Add `class:dismissed` to the bubble and grey it via an opacity rule using existing tokens; do not introduce a new colour token.

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cd crates/ui/src-svelte && pnpm exec vitest run MessageBubble conversation
```

Expected: PASS.

- [ ] **Step 6: Run the full UI gate**

```bash
cd crates/ui/src-svelte && pnpm check && pnpm exec vitest run
```

Expected: `pnpm check` at 0 errors / 0 warnings (`--fail-on-warnings` is set — a new a11y warning fails the build). Full vitest green.

- [ ] **Step 7: Commit**

```bash
git add crates/ui/src-svelte/src/
git commit -m "feat(ui): failed bubble shows the reason with Resend and Dismiss"
```

---

### Task 8: Failed file attachments

`Kind::File` messages ride the same outbox, so without this a failed file bubble is the one place left showing an eternal clock. Dismiss only — re-sending a file needs the original path, which may no longer exist, and inventing a recovery story for that is out of scope (spec §7).

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/components/FileAttachmentBubble.svelte`
- Test: `crates/ui/src-svelte/src/lib/components/FileAttachmentBubble.test.ts`

**Interfaces:**
- Consumes: Task 5's record fields, Task 7's `dismiss`.
- Produces: nothing.

- [ ] **Step 1: Write the failing test**

```ts
it("an outgoing file that failed to send shows the reason and Dismiss, but no Resend", () => {
  const rec = makeOutgoingFile({ failed_reason: "Not delivered — no mailbox." });
  render(FileAttachmentBubble, { props: { record: rec } });

  expect(screen.getByText(/no mailbox/i)).toBeInTheDocument();
  expect(screen.getByRole("button", { name: /dismiss/i })).toBeInTheDocument();
  // Resend needs the original path, which may be gone (spec §7).
  expect(screen.queryByRole("button", { name: /resend/i })).toBeNull();
});
```

- [ ] **Step 2: Run it to verify it fails**

```bash
cd crates/ui/src-svelte && pnpm exec vitest run FileAttachmentBubble
```

Expected: FAIL.

- [ ] **Step 3: Add the failed branch**

Mirror Task 7's derivation and markup, minus the Resend button. Reuse the same wording and the same `dismiss` import; do not duplicate the reason text as a literal.

- [ ] **Step 4: Run it to verify it passes**

```bash
cd crates/ui/src-svelte && pnpm exec vitest run FileAttachmentBubble
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src-svelte/src/lib/components/
git commit -m "feat(ui): failed outgoing attachments show the reason and Dismiss"
```

---

### Task 9: The guardrail that would have caught this

Per the audit's defining rule, behaviour is proven through the real `Daemon::run` (`run_with_transport`) assembly over loopback — not via `test_exports`. No such test exists for a queued message surviving an outage, which is exactly why the field bug shipped.

**Files:**
- Modify: `crates/tests/src/` — add beside the existing loopback guardrails (`two_daemons_exchange_messages_both_directions_over_loopback`, `offline_peer_receives_via_mailbox_fallback`), reusing `crates/tests/src/loopback_harness.rs`
- Test: the new file is the test

**Interfaces:**
- Consumes: Tasks 1, 2 and 4.
- Produces: nothing.

- [ ] **Step 1: Write the failing guardrail**

```rust
/// #227/#229: a message queued while the peer is unreachable must deliver
/// on its own when the peer returns — with NO new user action and NO
/// inbound dial from the peer. Field-observed failure: six messages sat
/// queued for 23 hours and moved only when the peer dialled in.
#[tokio::test]
async fn queued_message_delivers_when_peer_returns_over_loopback() {
    let mut h = LoopbackHarness::pair().await;

    // Peer unreachable: the send lands in the outbox rather than the wire.
    h.partition_b().await;
    h.send_from_a("queued while you were away").await;
    assert!(h.outbox_len_a().await >= 1, "message must be queued");

    // Peer returns. Nothing else happens — no new send, no dial from B.
    h.heal_b().await;

    h.wait_for_message_on_b("queued while you were away", Duration::from_secs(90))
        .await
        .expect("a queued message must deliver once the peer is reachable again");
    assert_eq!(h.outbox_len_a().await, 0, "the row must be acked and removed");
}
```

Build `partition_b` / `heal_b` from whatever `LoopbackTransport` already exposes for controlling reachability; if it has no such control, add the smallest one that makes a dial fail and then succeed, and keep it in the harness rather than the test. Model the rest on `offline_peer_receives_via_mailbox_fallback`, which already does an offline-then-online sequence.

- [ ] **Step 2: Run it to verify it fails on the pre-fix code**

```bash
. "$HOME/.cargo/env" && git stash && cargo test -p skattr-tests queued_message_delivers_when_peer_returns; git stash pop
```

Expected: FAIL before the Task 1/2 fixes — this is the mutation check that proves the guardrail has teeth. If it passes on stashed code, the harness is not actually exercising the retry path; fix the test, not the assertion.

- [ ] **Step 3: Run it against the fixes**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-tests queued_message_delivers_when_peer_returns
```

Expected: PASS.

- [ ] **Step 4: Run the full integration suite**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-tests
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all
git add crates/tests/src/
git commit -m "test(guardrail): a queued message delivers when the peer returns (#227, #229)"
```

---

### Task 10: Full gate, changelog, PR

**Files:**
- Modify: `CHANGELOG.md`, `Cargo.toml` (`workspace.package.version`), `crates/ui/tauri.conf.json` (`version`), `Cargo.lock`

- [ ] **Step 1: Run the complete local gate**

```bash
. "$HOME/.cargo/env"
cargo fmt --all -- --check
cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings
cargo test
cargo clippy -p skattr-ui --all-targets -- -D warnings
cd crates/ui/src-svelte && pnpm check && pnpm exec vitest run && cd ../../..
cargo deny check
```

Every command must pass. Paste the output — no success claims without it (`superpowers:verification-before-completion`).

- [ ] **Step 2: Bump the version**

Patch-bump per build: `0.1.24 → 0.1.25` in `Cargo.toml` and `crates/ui/tauri.conf.json`, then `. "$HOME/.cargo/env" && cargo check` to refresh `Cargo.lock`.

- [ ] **Step 3: Add the changelog entry**

Under a new `0.1.25` heading, listing the issues this closes: `#227`, `#228`, `#229`.

- [ ] **Step 4: Commit and open the PR**

```bash
git add -A
git commit -m "chore(release): bump version 0.1.24 → 0.1.25"
git push -u origin <branch>
gh pr create --title "fix(delivery): queued messages deliver, or say why they cannot (#227, #228, #229)" --body "Closes #227
Closes #228
Closes #229

<summary + the field evidence from the spec>"
```

- [ ] **Step 5: Babysit Greptile**

Verify each finding against the code before applying it; reject false positives with evidence; resolve all threads before merging. Read the **check conclusion**, not the review state — a clean run lands as a green `Greptile Review` check with no formal approval, so `reviewDecision` stays empty.

- [ ] **Step 6: Field-verify on the Linux/Windows pair**

Build with `cargo tauri build` (**not** `cargo build` — a bare build produces a binary that loads `devUrl` and cannot render, #183). Confirm on the real pair: a message sent while the other machine is offline either delivers when it returns, or turns into a failed bubble naming the mailbox remedy.

Note the existing six stuck rows are 23 hours past the window and will surface as failed — that is the correct outcome, not a regression.

---

## Notes for the executor

- **The two-lane pair in Task 4 is the heart of this.** `aged_row_with_mailbox_is_retargeted_never_expired` is not a nice-to-have: it is what stops a later simplification from expiring mailbox rows and quietly undoing the design. Where a contact advertises a mailbox, a queued message must never expire — the mailbox lane terminates on its own at `Deposited`.
- **Do not add a give-up deadline shorter than the window.** A 5–10 minute deadline is a pollable presence oracle (send, wait, repeat, and you have an occupancy log), and Skattr has no presence signal by design (#196). Coarseness is the mitigation.
- **Do not "fix" #228 by re-stamping the envelope on retry.** It works, and it makes the mailbox redundant. Rejected in the spec with reasons.
- **jsdom performs no layout.** The vitest tests in Tasks 7 and 8 assert content and actions, not positioning. Do not read them as proof anything is visible on screen; that is #230's territory.
