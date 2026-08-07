# Honest pending first-contact states — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a not-yet-confirmed first contact read as unconfirmed (never as a successful add) — by fixing `list_contacts` to report `PendingJoin` truthfully and adding two honest UI states.

**Architecture:** Core: `list_contacts` (and the `ContactAdded` result) consult the durable `pending_welcomes`/`is_pending` signal instead of the always-`Active` `Group::load` result. UI: a `pendingState(contact, nowSecs)` helper thresholds `now − added_at` into `"connecting" | "unconfirmed"`, a ~30 s ticking `now` store drives re-evaluation, `ContactRow` renders a de-emphasised two-state badge, and the conversation composer's disabled-reason becomes honest.

**Tech Stack:** Rust (skattr-core), Tauri 2 + SvelteKit (Svelte 5 runes), vitest.

## Global Constraints

- Rust: no `unwrap`/`expect` in non-test lib code; typed `CoreError`/`IpcError`; GPLv3 header on every `.rs`. Cargo not on PATH — prefix `. "$HOME/.cargo/env" &&`.
- TypeScript: `strict`; no `any`, no `!`, no `ts-ignore`; GPLv3 header on new files.
- **No new `ContactSummary` wire field** — reuse existing `group_state` + `added_at` (unix **seconds**).
- `CONNECTING_GRACE_SECS = 120` (single named constant, UI-side).
- Gates: `cargo test -p skattr-core --lib` + `cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings`; `pnpm --dir crates/ui/src-svelte check` (0 errors/warnings) + `pnpm --dir crates/ui/src-svelte exec vitest run`.
- Work only on branch `fix/101-pending-contact-ux`.

## Key existing shapes

- `crates/core/src/daemon/dispatch.rs:153-164` — `list_contacts` builds `group_state` from `Group::load` (`Ok(Some)→Active`, `Ok(None)→PendingJoin`, `Err→Corrupt`). `ContactSummary` also carries `added_at` (u64 secs). The contact identity in the loop is `c.identity` (`PublicKey`, `.0` is `[u8;32]`).
- `crates/core/src/daemon/dispatch.rs:459-468` — `add_contact` returns `CommandResult::ContactAdded(ContactSummary { …, group_state: None, … })`.
- `crates/core/src/storage::PendingWelcomeRepo::new(&pool).is_pending(&[u8;32]) -> Result<bool>` — the durable pending signal (already used by `send_message`'s guard at `dispatch.rs:544`).
- UI `crates/ui/src-svelte/src/lib/stores/contacts.ts` — `isConnecting(c) = c.group_state === "pending_join"`.
- UI `crates/ui/src-svelte/src/lib/components/ContactRow.svelte:36` — `{#if isConnecting(summary)}<span class="connecting-badge">Connecting…</span>{/if}`.
- UI `crates/ui/src-svelte/src/routes/+page.svelte:76-90` — `composerDisabled = group_state !== "active"`; `disabledReason` maps `pending_join → "Joining group…"`. The composer (`:215`) already renders disabled with the reason — so the "banner" is just making this reason honest, not a new component.

---

### Task 1: Core — `list_contacts` reports `PendingJoin` truthfully

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (`list_contacts` group_state; `add_contact` `ContactAdded` group_state; tests)

**Interfaces:**
- Consumes: `PendingWelcomeRepo::is_pending(&[u8;32]) -> Result<bool>`.
- Produces: `ContactSummary.group_state == Some(PendingJoin)` for a contact with a `pending_welcomes` row; `Some(Active)` once the row is gone.

- [ ] **Step 1: Write the failing test.** In `dispatch.rs` tests, after an `add_contact` that leaves a pending row, assert `list_contacts` reports `pending_join` (not `active`). Use the existing `test_handle_with_dialer` + invite flow (mirror `add_contact_persists_inviter_card_for_dialer`):

```rust
    #[tokio::test]
    async fn list_contacts_reports_pending_join_while_is_pending() {
        let handle_a = test_handle();
        handle_a.set_onion("alice.onion".to_string());
        let alice = handle_a.identity.public();
        let CommandResult::InviteCreated { url, .. } = execute_command(
            handle_a.clone(),
            Command::CreateInvite { nickname: None, ttl_secs: Some(3600) },
        ).await.unwrap() else { panic!("expected InviteCreated"); };

        let handle_b = test_handle_with_dialer();
        execute_command(handle_b.clone(), Command::AddContact { invite_url: url })
            .await.unwrap();

        // The genesis group IS saved, so Group::load is Active — but the
        // pending_welcomes row exists, so list_contacts must report PendingJoin.
        let CommandResult::Contacts(list) =
            execute_command(handle_b.clone(), Command::ListContacts).await.unwrap()
        else { panic!("expected Contacts"); };
        let entry = list.iter().find(|s| s.pubkey == alice).expect("alice listed");
        assert_eq!(
            entry.group_state,
            Some(crate::daemon::commands::MlsGroupStateLabel::PendingJoin),
            "a pending first contact must report PendingJoin, not Active"
        );

        // After the Ack (row deleted), it reports Active.
        crate::storage::PendingWelcomeRepo::new(&handle_b.pool).delete(&alice.0).unwrap();
        let CommandResult::Contacts(list2) =
            execute_command(handle_b.clone(), Command::ListContacts).await.unwrap()
        else { panic!("expected Contacts"); };
        let e2 = list2.iter().find(|s| s.pubkey == alice).expect("alice listed");
        assert_eq!(e2.group_state, Some(crate::daemon::commands::MlsGroupStateLabel::Active));
    }
```

- [ ] **Step 2: Run — expect FAIL** (reports `Active`, not `PendingJoin`).
  Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib --features test-harness list_contacts_reports_pending_join_while_is_pending`

- [ ] **Step 3: Implement the override.** In `list_contacts`, after the `Group::load` match that yields `group_state`, override an otherwise-`Active` state to `PendingJoin` when the durable pending signal is set:

```rust
        // A saved genesis group always loads Active (GroupState is not persisted,
        // #93), so Active here does NOT mean first contact completed. The durable
        // truth is the pending_welcomes row (a row exists iff the peer has not
        // yet Ack'd). Consult it only for an otherwise-Active state.
        let group_state = match group_state {
            Some(crate::daemon::commands::MlsGroupStateLabel::Active)
                if crate::storage::PendingWelcomeRepo::new(&handle.pool)
                    .is_pending(&c.identity.0)
                    .map_err(map_err)? =>
            {
                Some(crate::daemon::commands::MlsGroupStateLabel::PendingJoin)
            }
            other => other,
        };
```

  And in `add_contact`, change the returned `ContactAdded` `group_state: None` to `Some(PendingJoin)` (the `pending_welcomes` row was just inserted in the same transaction):

```rust
        group_state: Some(crate::daemon::commands::MlsGroupStateLabel::PendingJoin),
```

- [ ] **Step 4: Run — expect PASS**, and no regression in `list_contacts`/`add_contact` tests.
  Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib --features test-harness -- list_contacts add_contact`

- [ ] **Step 5: Lint + commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings
git add crates/core/src/daemon/dispatch.rs
git commit -m "fix(#101): list_contacts reports PendingJoin from is_pending (not always-Active Group::load)"
```

---

### Task 2: UI — `pendingState` helper + ticking `now` store

**Files:**
- Create: `crates/ui/src-svelte/src/lib/stores/now.ts`
- Modify: `crates/ui/src-svelte/src/lib/stores/contacts.ts`
- Test: `crates/ui/src-svelte/src/lib/stores/contacts.test.ts` (create)

**Interfaces:**
- Produces: `CONNECTING_GRACE_SECS = 120`; `pendingState(c: ContactSummary, nowSecs: number): "connecting" | "unconfirmed" | null`; `now` (a `Readable<number>` of unix **seconds**, ticking every 30 s).

- [ ] **Step 1: Write the failing test** in `contacts.test.ts`:

```ts
import { describe, it, expect } from "vitest";
import { pendingState } from "./contacts";
import type { ContactSummary } from "$lib/ipc/types";

const base = (over: Partial<ContactSummary>): ContactSummary => ({
  pubkey: "00", nickname: null, onion: "x.onion", card_version: 1n, added_at: 0n,
  unread_count: 0n, last_message_preview: null, last_ts_recv: null,
  group_state: "pending_join", last_read_row_id: null, muted: false, peer_mailboxes: [],
  ...over,
});

describe("pendingState", () => {
  it("connecting within the grace window", () => {
    expect(pendingState(base({ added_at: 1000n }), 1000 + 30)).toBe("connecting");
  });
  it("unconfirmed after the grace window", () => {
    expect(pendingState(base({ added_at: 1000n }), 1000 + 200)).toBe("unconfirmed");
  });
  it("null for an active contact", () => {
    expect(pendingState(base({ group_state: "active" }), 99999)).toBeNull();
  });
  it("clamps negative elapsed to connecting", () => {
    expect(pendingState(base({ added_at: 5000n }), 1000)).toBe("connecting");
  });
});
```

- [ ] **Step 2: Run — expect FAIL** (`pendingState` not exported).
  Run: `pnpm --dir crates/ui/src-svelte exec vitest run contacts.test.ts`

- [ ] **Step 3: Implement.** In `contacts.ts` add (near `isConnecting`):

```ts
export const CONNECTING_GRACE_SECS = 120;

/** Display state for a not-yet-confirmed first contact, or null if not pending. */
export function pendingState(
  c: ContactSummary,
  nowSecs: number,
): "connecting" | "unconfirmed" | null {
  if (c.group_state !== "pending_join") return null;
  const elapsed = Math.max(0, nowSecs - Number(c.added_at));
  return elapsed < CONNECTING_GRACE_SECS ? "connecting" : "unconfirmed";
}
```

  Create `stores/now.ts`:

```ts
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
import { readable, type Readable } from "svelte/store";

/** Unix seconds, updated every 30 s — lets pending contacts re-evaluate their
 *  display state (Connecting… → Not connected yet) without a daemon event. */
export const now: Readable<number> = readable(Math.floor(Date.now() / 1000), (set) => {
  const id = setInterval(() => set(Math.floor(Date.now() / 1000)), 30_000);
  return () => clearInterval(id);
});
```

- [ ] **Step 4: Run — expect PASS.**
  Run: `pnpm --dir crates/ui/src-svelte exec vitest run contacts.test.ts`

- [ ] **Step 5: Lint + commit**

```bash
pnpm --dir crates/ui/src-svelte check
git add crates/ui/src-svelte/src/lib/stores/now.ts crates/ui/src-svelte/src/lib/stores/contacts.ts crates/ui/src-svelte/src/lib/stores/contacts.test.ts
git commit -m "feat(#101): pendingState helper (connecting/unconfirmed by elapsed) + ticking now store"
```

---

### Task 3: UI — `ContactRow` two-state badge + de-emphasis

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/components/ContactRow.svelte`
- Test: `crates/ui/src-svelte/src/lib/components/ContactRow.test.ts` (create)

**Interfaces:**
- Consumes: `pendingState`, `now` (Task 2).

- [ ] **Step 1: Write the failing test** in `ContactRow.test.ts` — render with `group_state: "pending_join"` and an old `added_at` (unconfirmed) and assert the "Not connected yet" text + a `pending` class; and with a recent `added_at` assert "Connecting…":

```ts
import { describe, it, expect } from "vitest";
import { render } from "@testing-library/svelte";
import ContactRow from "./ContactRow.svelte";
import type { ContactSummary } from "$lib/ipc/types";

const c = (over: Partial<ContactSummary>): ContactSummary => ({
  pubkey: "aa", nickname: "Bob", onion: "x.onion", card_version: 1n, added_at: 0n,
  unread_count: 0n, last_message_preview: null, last_ts_recv: null,
  group_state: "pending_join", last_read_row_id: null, muted: false, peer_mailboxes: [],
  ...over,
});

describe("ContactRow pending states", () => {
  it("shows 'Not connected yet' for a long-pending contact", () => {
    const nowSecs = Math.floor(Date.now() / 1000);
    const { getByText } = render(ContactRow, { summary: c({ added_at: BigInt(nowSecs - 600) }) });
    expect(getByText(/not connected yet/i)).toBeTruthy();
  });
  it("shows 'Connecting…' for a fresh pending contact", () => {
    const nowSecs = Math.floor(Date.now() / 1000);
    const { getByText } = render(ContactRow, { summary: c({ added_at: BigInt(nowSecs) }) });
    expect(getByText(/connecting/i)).toBeTruthy();
  });
});
```

- [ ] **Step 2: Run — expect FAIL** (only the old "Connecting…" badge, no "Not connected yet").
  Run: `pnpm --dir crates/ui/src-svelte exec vitest run ContactRow.test.ts`

- [ ] **Step 3: Implement.** In `ContactRow.svelte` `<script>`, import and derive:

```svelte
  import { pendingState } from "$lib/stores/contacts";
  import { now } from "$lib/stores/now";
  let pstate = $derived(pendingState(summary, $now));
```

  Replace the badge block:

```svelte
      {#if pstate === "connecting"}
        <span class="pending-badge" title="First contact still connecting">Connecting…</span>
      {:else if pstate === "unconfirmed"}
        <span class="pending-badge unconfirmed" title="They haven't accepted your invite yet — still trying to reach them">Not connected yet</span>
      {/if}
```

  Add a `class:pending={pstate !== null}` to the row's root element, and styles:

```svelte
  .pending-badge { color: var(--text-muted, #888); font-size: 0.75em; font-weight: 400; }
  .pending-badge.unconfirmed { color: var(--warning, #c90); }
  .pending { opacity: 0.6; }
```

- [ ] **Step 4: Run — expect PASS.**
  Run: `pnpm --dir crates/ui/src-svelte exec vitest run ContactRow.test.ts`

- [ ] **Step 5: Lint + commit**

```bash
pnpm --dir crates/ui/src-svelte check
git add crates/ui/src-svelte/src/lib/components/ContactRow.svelte crates/ui/src-svelte/src/lib/components/ContactRow.test.ts
git commit -m "feat(#101): ContactRow two-state pending badge (Connecting / Not connected yet) + de-emphasis"
```

---

### Task 4: UI — honest conversation composer reason

**Files:**
- Modify: `crates/ui/src-svelte/src/routes/+page.svelte`

**Interfaces:**
- Consumes: `pendingState`, `now` (Task 2).

- [ ] **Step 1: Implement** — make the `pending_join` `disabledReason` honest, varying by `pendingState`. In `+page.svelte` `<script>`, import `pendingState` + `now`, then change the `disabledReason` `pending_join` arm:

```ts
        : activeSummary.group_state === "pending_join"
          ? (pendingState(activeSummary, $now) === "unconfirmed"
              ? "They haven't accepted your invite yet — you can't message them until they connect."
              : "Connecting… waiting for them to accept your invite.")
```

  (The composer at `:215` already renders disabled with `disabledReason`; this only makes the text honest — no new component.)

- [ ] **Step 2: Verify** the app builds + type-checks (no dedicated vitest for `+page`; the composer-disabled behaviour is unchanged, only the string):
  Run: `pnpm --dir crates/ui/src-svelte check && pnpm --dir crates/ui/src-svelte exec vitest run`

- [ ] **Step 3: Commit**

```bash
git add crates/ui/src-svelte/src/routes/+page.svelte
git commit -m "feat(#101): honest pending-contact reason in the conversation composer"
```

---

## After all tasks

Run the full UI gate (`pnpm check` + `pnpm exec vitest run`) + `cargo test -p skattr-core --lib` + clippy, then use superpowers:finishing-a-development-branch. Closes #101.
