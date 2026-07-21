# #107 (Approach A) — Bounded first-contact welcome-sweep + clean-fail — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the durable welcome-sweep from retrying a stuck first contact forever; after a bounded age it marks the attempt durably `failed` and the UI surfaces "Couldn't connect — remove & re-invite" (recover via #109), instead of a permanent "Connecting…".

**Architecture:** Add a `failed` flag to `pending_welcomes`; the sweep's not-acked branch marks the row `failed` once it exceeds `MAX_WELCOME_AGE` (instead of rescheduling); `due()` skips failed rows. `list_contacts` carries a new `welcome_failed` bool; the UI adds a `failed` pending-state. No protocol/wire/ADR change.

**Tech Stack:** Rust (rusqlite 0.38, ts-rs), Tauri 2 + SvelteKit (vitest).

**Spec:** `docs/superpowers/specs/2026-07-21-issue-107-welcome-rebuild-design.md` (Approach A chosen; Approach B disclosed/deferred).

## Global Constraints

- Branch: `107-welcome-rebuild`. **No wire/protocol change, no ADR.** ADR 0009's per-connection binding is untouched (this does NOT auto-recover; it fails cleanly).
- Milestone v1.1. Local gate authoritative: `cargo fmt --all -- --check`, `cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings`, `cargo test`, `cargo clippy -p skattr-ui --all-targets -- -D warnings`, `pnpm check` + `pnpm exec vitest run`. Cargo not on PATH — prefix with `. "$HOME/.cargo/env" &&`.
- Rust: no `unwrap`/`expect` outside tests; typed `CoreError`/daemon errors; GPLv3 headers; migrations are `include_str!`'d SQL keyed by `schema_version`; model states as data.
- TS strict: no new `any`/`!`/`ts-ignore` (existing IPC `as any` bridge only where present).
- **A failed row keeps `is_pending == true`** (the row still exists) so the contact stays `PendingJoin`, never mis-rendering Active (the #101 invariant). `welcome_failed` is an *additional* signal, not a replacement for `is_pending`.
- The #109 `RemoveContact` hard-purge already deletes the `pending_welcomes` row (incl. a failed one) — do not change it; a failed contact is removable today.

**Verified facts:**
- `pending_welcomes` columns: `peer_pubkey` (PK), `group_id`, `welcome_bytes`, `next_retry_at`, `attempts`, `created_at`. Migrations run through `0018`; this adds `0019`.
- `PendingWelcomeRepo`: `due(now_ms, limit) -> Vec<PendingWelcomeDue>` (SELECT peer,group_id,welcome_bytes,attempts WHERE next_retry_at<=? ORDER BY next_retry_at LIMIT ?), `reschedule(peer, next_ms)`, `is_pending(peer) -> bool`, `delete(peer)`, `delete_in_tx` (#109).
- `run_welcome_sweep` (welcome_sweep.rs:110): for each due row → `hub.send_welcome(peer, welcome_bytes)` → await Ack (`ACK_TIMEOUT=45s`); acked → `on_welcome_acked`; else `reschedule(peer, now + welcome_backoff_ms(attempts))`. `BACKOFF_MS=[5,15,30,60]s`.
- `list_contacts` (dispatch.rs:153–175): derives `group_state` from `Group::load` then overrides `Active → PendingJoin` while `is_pending`. `ContactSummary` (commands.rs:393) has ts-rs `#[ts(export)]`.
- #101 UI: `pendingState(c, nowSecs) -> "connecting" | "unconfirmed" | null` (contacts.ts) keyed on `group_state === "pending_join"` + `added_at`; consumed in `ContactRow.svelte` (badge) and `+page.svelte` (`disabledReason`).

---

### Task 1: Storage — `failed` flag on `pending_welcomes`

**Files:**
- Create: `crates/core/src/storage/migrations/0019_pending_welcome_failed.sql`
- Modify: `crates/core/src/storage/migrations/mod.rs` (or wherever migrations are registered — follow the `0018` registration pattern)
- Modify: `crates/core/src/storage/pending_welcomes.rs` (+ its `#[cfg(test)]`)

**Interfaces — Produces:**
- `PendingWelcomeRepo::mark_failed(&self, peer: &[u8; 32]) -> Result<()>`
- `PendingWelcomeRepo::is_failed(&self, peer: &[u8; 32]) -> Result<bool>`
- `due()` skips rows where `failed = 1`; `PendingWelcomeDue` gains `created_at: i64`.

- [ ] **Step 1: Write the migration**

`0019_pending_welcome_failed.sql`:
```sql
-- SPDX-License-Identifier: GPL-3.0-or-later
-- #107: a first-contact Welcome that never Ack'd within MAX_WELCOME_AGE is
-- marked failed. The row is kept (is_pending stays true → contact stays
-- PendingJoin, never mis-rendered Active) but the sweep no longer retries it.
ALTER TABLE pending_welcomes ADD COLUMN failed INTEGER NOT NULL DEFAULT 0;
```

Register it after `0018` exactly as the existing migrations are registered (find the `include_str!("migrations/0018_…")` list and add the `0019` entry in order).

- [ ] **Step 2: Write the failing test**

In `pending_welcomes.rs` tests:
```rust
#[test]
fn mark_failed_sets_flag_and_due_skips_failed() {
    let p = pool(); // the file's existing test-pool helper
    let repo = PendingWelcomeRepo::new(&p);
    let peer = [0x11u8; 32];
    p.transaction(|tx| repo.insert_in_tx(tx, &peer, b"gid", b"welcome", 0, 100)).unwrap();
    assert!(!repo.is_failed(&peer).unwrap());
    // due() returns it (next_retry_at <= now).
    assert_eq!(repo.due(i64::MAX, 10).unwrap().len(), 1);
    repo.mark_failed(&peer).unwrap();
    assert!(repo.is_failed(&peer).unwrap());
    // is_pending stays true (row still exists) but due() skips it.
    assert!(repo.is_pending(&peer).unwrap());
    assert_eq!(repo.due(i64::MAX, 10).unwrap().len(), 0);
}
```
(Match `insert_in_tx`'s real arg order/signature — read it; the args above are illustrative.)

- [ ] **Step 3: Run it — verify it fails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core storage::pending_welcomes::tests::mark_failed_sets_flag_and_due_skips_failed`
Expected: FAIL — `mark_failed`/`is_failed` don't exist.

- [ ] **Step 4: Implement**

Add to `impl PendingWelcomeRepo`:
```rust
/// Mark a pending Welcome failed (#107): the sweep stops retrying it. The row
/// is kept so `is_pending` stays true (contact stays PendingJoin, not Active).
pub fn mark_failed(&self, peer: &[u8; 32]) -> Result<()> {
    self.pool.with_mut(|c| {
        c.execute(
            "UPDATE pending_welcomes SET failed = 1 WHERE peer_pubkey = ?1",
            rusqlite::params![&peer[..]],
        )
        .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("mark_failed: {e}"))))?;
        Ok(())
    })
}

/// Whether the pending Welcome for `peer` has been marked failed (#107).
pub fn is_failed(&self, peer: &[u8; 32]) -> Result<bool> {
    self.pool.with(|c| {
        let n: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM pending_welcomes WHERE peer_pubkey = ?1 AND failed = 1",
                rusqlite::params![&peer[..]],
                |r| r.get(0),
            )
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("is_failed: {e}"))))?;
        Ok(n > 0)
    })
}
```
(Match the file's real `with`/`with_mut` accessor names.)

Modify `due()`'s SELECT to add `AND failed = 0` and to also select `created_at`; add `created_at: i64` to `PendingWelcomeDue`:
```sql
SELECT peer_pubkey, group_id, welcome_bytes, attempts, created_at
FROM pending_welcomes
WHERE next_retry_at <= ?1 AND failed = 0
ORDER BY next_retry_at ASC LIMIT ?2
```
Update the row-mapping closure to read `created_at` (index 4) and populate the new struct field.

- [ ] **Step 5: Run it — verify it passes**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core storage::pending_welcomes`
Expected: PASS (fix any other `PendingWelcomeDue` construction sites the new field breaks — e.g. tests).

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/storage/
git commit -m "feat(#107): add failed flag to pending_welcomes (mark_failed/is_failed; due skips failed)"
```

---

### Task 2: Sweep — bounded age → mark failed instead of retry forever

**Files:**
- Modify: `crates/core/src/delivery/welcome_sweep.rs` (+ its `#[cfg(test)]`)

**Interfaces — Consumes:** `PendingWelcomeRepo::mark_failed`, `PendingWelcomeDue::created_at` (Task 1).
**Interfaces — Produces:** `const MAX_WELCOME_AGE_MS: i64`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn sweep_marks_failed_after_max_age() {
    // Build a pool + a pending_welcomes row whose created_at is older than
    // MAX_WELCOME_AGE_MS, and a hub whose send_welcome never Acks (so the
    // not-acked branch runs). Mirror the existing welcome_sweep test harness.
    let (pool, hub, handle) = welcome_sweep_test_rig(); // adapt to the real harness
    let peer = [0x22u8; 32];
    let created_at = 0i64; // far in the past
    pool.transaction(|tx| {
        PendingWelcomeRepo::new(&pool).insert_in_tx(tx, &peer, b"gid", b"welcome", 0, created_at)
    }).unwrap();

    let now = MAX_WELCOME_AGE_MS + 1; // row age exceeds the cap
    run_welcome_sweep(&pool, &hub, &handle, now, 16).await;

    let repo = PendingWelcomeRepo::new(&pool);
    assert!(repo.is_failed(&peer).unwrap(), "over-age unacked welcome must be marked failed");
    assert!(repo.is_pending(&peer).unwrap(), "row kept so contact stays PendingJoin");
    // And the sweep no longer re-sends it: a second pass sees nothing due.
    assert_eq!(repo.due(now + 1, 16).unwrap().len(), 0);
}

#[tokio::test]
async fn sweep_reschedules_within_max_age() {
    // Same rig, but created_at recent (age < cap) and still not acked →
    // the row is rescheduled (attempts bumped), NOT failed.
    let (pool, hub, handle) = welcome_sweep_test_rig();
    let peer = [0x33u8; 32];
    let now = 10_000i64;
    pool.transaction(|tx| {
        PendingWelcomeRepo::new(&pool).insert_in_tx(tx, &peer, b"gid", b"welcome", 0, now - 1_000)
    }).unwrap();
    run_welcome_sweep(&pool, &hub, &handle, now, 16).await;
    let repo = PendingWelcomeRepo::new(&pool);
    assert!(!repo.is_failed(&peer).unwrap(), "young unacked welcome must NOT be failed yet");
    assert!(repo.is_pending(&peer).unwrap());
}
```
Use the real never-ack hub harness the existing `welcome_sweep` tests use (search the test module — e.g. an `UnreachableFactory`/stub hub whose `send_welcome` errors or never Acks). If no rig helper exists, build the two daemons/hub the same way the existing sweep test does and factor a small local helper.

- [ ] **Step 2: Run it — verify it fails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core delivery::welcome_sweep::tests::sweep_marks_failed_after_max_age`
Expected: FAIL — the row is rescheduled, not failed (no cap yet).

- [ ] **Step 3: Implement the cap**

Add near the other consts in `welcome_sweep.rs`:
```rust
/// #107: bound first-contact Welcome re-sends. A first contact that hasn't
/// Ack'd within this age is marked failed (the sweep stops; the UI surfaces
/// "couldn't connect — remove & re-invite"), instead of retrying forever — a
/// circuit-rebind (`Psk(KeyNotFound)`) is permanent and no retry count helps,
/// while a genuinely slow peer still gets up to this long. 24 h.
const MAX_WELCOME_AGE_MS: i64 = 24 * 60 * 60 * 1_000;
```

In the not-acked (`else`) branch of the `for row in due` loop, replace the unconditional `reschedule` with:
```rust
        } else if now_ms.saturating_sub(row.created_at) >= MAX_WELCOME_AGE_MS {
            tracing::warn!(
                target: "skattr::delivery::welcome_sweep",
                attempts = row.attempts,
                "welcome-sweep: first contact exceeded MAX_WELCOME_AGE; marking failed"
            );
            if let Err(e) = repo.mark_failed(&row.peer) {
                tracing::warn!(
                    target: "skattr::delivery::welcome_sweep",
                    error = %e,
                    "welcome-sweep: mark_failed failed"
                );
            }
        } else {
            let next = now_ms.saturating_add(welcome_backoff_ms(row.attempts));
            if let Err(e) = repo.reschedule(&row.peer, next) {
                tracing::warn!(
                    target: "skattr::delivery::welcome_sweep",
                    error = %e,
                    "welcome-sweep: reschedule failed"
                );
            }
        }
```

- [ ] **Step 4: Run it — verify both tests pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core delivery::welcome_sweep`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/delivery/welcome_sweep.rs
git commit -m "feat(#107): bound welcome-sweep — mark failed after MAX_WELCOME_AGE (no infinite retry)"
```

---

### Task 3: Daemon — surface `welcome_failed` on `ContactSummary`

**Files:**
- Modify: `crates/core/src/daemon/commands.rs` (`ContactSummary`)
- Modify: `crates/core/src/daemon/dispatch.rs` (`list_contacts` — set the field)
- Regenerate: `crates/ui/src-svelte/src/lib/ipc/types/ContactSummary.ts` (via `cargo test -p skattr-core`)
- Test: `dispatch.rs` test module

**Interfaces — Consumes:** `PendingWelcomeRepo::is_failed` (Task 1).
**Interfaces — Produces:** `ContactSummary.welcome_failed: bool`.

- [ ] **Step 1: Write the failing test**

In `dispatch.rs` tests:
```rust
#[tokio::test]
async fn list_contacts_reports_welcome_failed() {
    let handle = test_handle();
    let (pk, _gid) = seed_pending_contact(&handle, 0x44); // pending row + linked group
    // Not failed yet → welcome_failed = false.
    let s0 = list_one_contact(&handle, pk).await;
    assert!(!s0.welcome_failed);
    assert_eq!(s0.group_state, Some(MlsGroupStateLabel::PendingJoin));
    // Mark failed → welcome_failed = true, still PendingJoin.
    PendingWelcomeRepo::new(&handle.pool).mark_failed(&pk.0).unwrap();
    let s1 = list_one_contact(&handle, pk).await;
    assert!(s1.welcome_failed);
    assert_eq!(s1.group_state, Some(MlsGroupStateLabel::PendingJoin));
}
```
Build `seed_pending_contact` / `list_one_contact` from the existing #101 test helpers (the `list_contacts_reports_pending_join_while_is_pending` test already sets up a pending contact and calls `ListContacts` — mirror it).

- [ ] **Step 2: Run it — verify it fails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core daemon::dispatch::tests::list_contacts_reports_welcome_failed`
Expected: FAIL — no `welcome_failed` field.

- [ ] **Step 3: Add the field + populate it**

In `commands.rs` `ContactSummary`, add (with the existing ts-rs derive):
```rust
    /// #107: the first-contact Welcome exceeded MAX_WELCOME_AGE without an Ack.
    /// The contact is still pending (never Active) but cannot complete — the UI
    /// prompts remove + re-invite. false for any non-pending or still-retrying
    /// contact.
    pub welcome_failed: bool,
```

In `list_contacts` (dispatch.rs), after computing the `group_state` override, set the field on the summary:
```rust
        let welcome_failed = crate::storage::PendingWelcomeRepo::new(&handle.pool)
            .is_failed(&c.identity.0)
            .map_err(map_err)?;
```
and include `welcome_failed` in the `ContactSummary { … }` construction. (Find every `ContactSummary { … }` construction site — there are a few, incl. in `add_contact`'s `ContactAdded` and tests — and add the field; a freshly-added contact is `welcome_failed: false`.)

- [ ] **Step 4: Run it + regenerate bindings**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core daemon::dispatch::tests::list_contacts_reports_welcome_failed`
Expected: PASS. This run also regenerates `ContactSummary.ts` with the new field.
Then `cd crates/ui/src-svelte && pnpm check` — expect the new field present; fix any `ContactSummary` construction in UI test mocks/`tauri-mock.ts` that now needs `welcome_failed` (add `welcome_failed: false`).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/ crates/ui/src-svelte/src/lib/ipc/types/ContactSummary.ts
git commit -m "feat(#107): surface welcome_failed on ContactSummary from is_failed"
```

---

### Task 4: UI — `failed` pending-state + "couldn't connect" messaging

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/stores/contacts.ts` (`pendingState`)
- Modify: `crates/ui/src-svelte/src/lib/components/ContactRow.svelte` (badge)
- Modify: `crates/ui/src-svelte/src/routes/+page.svelte` (`disabledReason`)
- Test: `crates/ui/src-svelte/src/lib/stores/contacts.test.ts`

**Interfaces — Consumes:** `ContactSummary.welcome_failed` (Task 3).

- [ ] **Step 1: Write the failing test**

In `contacts.test.ts`:
```ts
test("pendingState returns 'failed' when welcome_failed, regardless of elapsed", () => {
  const c = { group_state: "pending_join", added_at: 0, welcome_failed: true } as any;
  expect(pendingState(c, 5)).toBe("failed");        // even within the connecting grace
  expect(pendingState(c, 10_000)).toBe("failed");
});
test("pendingState unaffected when welcome_failed is false", () => {
  const c = { group_state: "pending_join", added_at: 0, welcome_failed: false } as any;
  expect(pendingState(c, 5)).toBe("connecting");
});
```

- [ ] **Step 2: Run it — verify it fails**

Run: `cd crates/ui/src-svelte && pnpm exec vitest run src/lib/stores/contacts.test.ts`
Expected: FAIL — `pendingState` returns "connecting", type has no `failed`.

- [ ] **Step 3: Implement the `failed` arm**

In `contacts.ts`, widen the return type and add the arm (failed takes precedence):
```ts
export function pendingState(
  c: ContactSummary,
  nowSecs: number,
): "connecting" | "unconfirmed" | "failed" | null {
  if (c.group_state !== "pending_join") return null;
  if (c.welcome_failed) return "failed";
  const elapsed = Math.max(0, nowSecs - Number(c.added_at));
  return elapsed < CONNECTING_GRACE_SECS ? "connecting" : "unconfirmed";
}
```

- [ ] **Step 4: Run it — verify it passes**

Run: `cd crates/ui/src-svelte && pnpm exec vitest run src/lib/stores/contacts.test.ts`
Expected: PASS.

- [ ] **Step 5: Wire the UI messaging**

- `ContactRow.svelte`: add a `failed` branch to the pending badge (beside `connecting`/`unconfirmed`) — e.g. a badge "Couldn't connect" with the `unconfirmed`/warning styling (reuse the existing `.pending-badge.unconfirmed` class or add a `.failed` modifier). Keep `class:pending={pstate !== null}` (a failed contact is still de-emphasised/pending).
- `+page.svelte`: add a `failed` arm to `disabledReason` for `group_state === "pending_join"` → *"Couldn't connect to this contact. Remove it and send a new invite to try again."* (The #109 Remove action is already shown for pending contacts, so no new control is needed — just the message.)

- [ ] **Step 6: Gate (full UI)**

Run: `cd crates/ui/src-svelte && pnpm check && pnpm exec vitest run` then `. "$HOME/.cargo/env" && cargo clippy -p skattr-ui --all-targets -- -D warnings`
Expected: 0 errors/warnings, vitest green, clippy clean.

- [ ] **Step 7: Commit**

```bash
git add crates/ui/src-svelte/src/lib/stores/contacts.ts crates/ui/src-svelte/src/lib/components/ContactRow.svelte crates/ui/src-svelte/src/routes/+page.svelte crates/ui/src-svelte/src/lib/stores/contacts.test.ts
git commit -m "feat(#107): UI failed pending-state — couldn't-connect message + remove/re-invite"
```

---

### Final: whole-branch gate

- [ ] Full authoritative local gate:
  - `. "$HOME/.cargo/env" && cargo fmt --all -- --check`
  - `cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings`
  - `cargo test`
  - `cargo clippy -p skattr-ui --all-targets -- -D warnings`
  - `cd crates/ui/src-svelte && pnpm check && pnpm exec vitest run`
  - `cargo deny check`
- [ ] Open PR referencing `Closes #107`; note "Approach A (bounded-fail); Approach B (auto-rebuild) disclosed/deferred in the spec"; babysit CodeRabbit.
- [ ] Follow-up: add a v1.1 threat-model/limitations disclosure line — first-contact lost-Welcome does not auto-recover; a stuck attempt fails after 24 h and is removable + re-invitable (Approach B deferred). (Docs task — file or fold into the next docs pass.)

## Self-review notes (coverage)

- Bound the sweep (stop infinite retry) → Task 2 (`MAX_WELCOME_AGE_MS`, mark_failed on over-age). ✅
- Failed modeled durably, row kept, never mis-rendered Active → Task 1 (`failed` col, `is_pending` still true) + Task 3 (`welcome_failed` alongside `PendingJoin`). ✅
- UI "couldn't connect — remove & re-invite" → Task 4. ✅
- #109 clears a failed contact → unchanged (hard-purge deletes the row incl. failed); noted, no code. ✅
- Fresh add after removal binds → existing first-contact behavior; not re-tested here (out of this change's surface). ✅
- No ADR/protocol change → migration + repo + sweep cap + a bool field + UI. ✅
