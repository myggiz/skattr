# Honest pending first-contact states

**Date:** 2026-07-19
**Issue:** #101. Relates: #93 (PendingJoin lifecycle + the durable `pending_welcomes`
signal), #90/#99 (why first contact stalls — Arti outbound-dial flakiness).
**Area:** daemon `list_contacts` projection (correctness fix) + Tauri/SvelteKit UI.

---

## Problem

When a first contact is added but has not completed (the peer has not yet
Ack'd the Welcome), the UI presents the peer as a normal, successfully-added
contact. The user has no way to tell "connected" from "we reached their onion
once but they never joined."

**Root cause (a real bug, not just weak styling).** `list_contacts`
(`dispatch.rs:153-164`) derives `group_state` from `Group::load`:

```rust
match Group::load(&GroupId(gid), &group_repo) {
    Ok(Some(_)) => Active,        // group blob loaded
    Ok(None)    => PendingJoin,   // group blob MISSING
    Err(_)      => Corrupt,
}
```

Per the #93 pivot, `GroupState` is **not persisted** — `Group::load` always
reconstructs `Active`, and a pending first contact *has* a saved genesis group.
So `Group::load` returns `Ok(Some)` → **`Active`**. `PendingJoin` is only ever
reported when the group blob is *missing* — which is not the pending-first-contact
case. **`list_contacts` therefore mis-reports every durably-pending contact as
`Active`.** The durable truth lives in the `pending_welcomes` row (a row exists
⟺ first contact is still pending), which the send-guard already consults
(`dispatch.rs:544`, `PendingWelcomeRepo::is_pending`) — but `list_contacts` does
not. Consequently the existing UI "Connecting…" badge
(`ContactRow.svelte:36`, gated on `group_state === "pending_join"`) is **dead for
the real case**, and pending contacts render as normal contacts.

## Goals / non-goals

**Goals**
- `list_contacts` (and the `ContactAdded` result) report `PendingJoin`
  truthfully for a contact whose first contact has not completed.
- A pending contact is visually + textually distinct and never reads as a
  successful add.
- After a short grace period the state escalates from "Connecting…" to an
  honest "Not connected yet — they haven't accepted. Still trying."
- Messaging stays blocked while pending (already enforced by the send-guard).

**Non-goals**
- No terminal "failed" state — the sweeper keeps retrying indefinitely (v1.0
  decision; a truly-absent peer is indistinguishable from an offline one, so we
  never claim a definitive failure). Reaffirmed here.
- No new `ContactSummary` wire field — reuse existing `group_state` + `added_at`.
- No auto-give-up / auto-remove. The existing manual Remove already deletes the
  `pending_welcomes` row (#93 Task 6) and stops the re-send.
- No change to the hard-dial-failure path — `AddContactDialog` already surfaces
  `DeliveryTimeout` ("Couldn't reach your contact… try again").
- No change to the sweeper, the transport, or #90/#99.

## Design

### 1. Core correctness fix — `list_contacts` consults `is_pending`

In `list_contacts` (`dispatch.rs`), after computing `group_state` from
`Group::load`, override it to `PendingJoin` when the durable pending signal is
set:

```rust
// A saved genesis group always loads Active (GroupState is not persisted, #93),
// so Active here does NOT mean first contact completed. The durable truth is the
// pending_welcomes row: a row exists iff the peer has not yet Ack'd the Welcome.
let group_state = match group_state {
    Some(MlsGroupStateLabel::Active)
        if PendingWelcomeRepo::new(&handle.pool).is_pending(&c.identity)? =>
    {
        Some(MlsGroupStateLabel::PendingJoin)
    }
    other => other,
};
```

Apply the same correction to the `ContactAdded` result built in `add_contact`
(`dispatch.rs:459-468`), which currently returns `group_state: None`: since the
`pending_welcomes` row was just inserted in the same transaction, return
`Some(PendingJoin)` so the optimistic post-add UI is immediately truthful.

`is_pending` is one indexed primary-key lookup per contact; `list_contacts`
already runs several per-contact queries, so the cost is negligible. Only run
the extra lookup for contacts whose `group_state` came back `Active` (skip it
for `Corrupt`/`None`).

**No new wire field.** `ContactSummary.group_state` becomes truthful;
`ContactSummary.added_at` (unix seconds, set at add) is the "pending since" time.

### 2. UI — two honest states off existing fields

`isConnecting(c)` (`stores/contacts.ts`, `group_state === "pending_join"`) now
fires for real pending contacts. Add a derived display state keyed on elapsed
time since add:

- `elapsed = now − added_at` (seconds). Clamp negatives to 0.
- **`elapsed < CONNECTING_GRACE` (120 s) → "Connecting…"** — the grace window
  (first contact over Tor legitimately takes ~30–90 s: bootstrap + dial +
  Welcome + Ack).
- **`elapsed ≥ CONNECTING_GRACE` → "Not connected yet"** (badge), tooltip/aria
  "They haven't accepted your invite yet — still trying to reach them."

`CONNECTING_GRACE` is a named constant in the UI (single source of truth,
tunable). The two states share one derivation (a `pendingState(contact, now)`
helper → `"connecting" | "unconfirmed" | null`).

**Visual de-emphasis (both pending states):** the `ContactRow` is dimmed
(reduced opacity / muted text) and carries the badge, so a pending contact is
never mistaken for a normal one. `unconfirmed` gets a slightly stronger
treatment than `connecting` (e.g. a warning-tinted badge) without implying a
hard failure.

**Reactive clock:** a small `now` store that ticks every ~30 s (a
`readable` store) so a row re-evaluates `pendingState` and flips
Connecting → Not-connected without needing a daemon push. It only needs to be
subscribed while at least one pending contact is visible; a plain interval store
is sufficient (YAGNI — no need to gate it).

**Conversation view:** `+page.svelte` already branches on
`group_state === "pending_join"`. For a pending contact, show an honest banner
in place of the composer — "They haven't accepted your invite yet — you can't
message them until they connect." — reusing the same `pendingState` for the
Connecting vs Not-connected wording. Messaging is already blocked server-side by
the send-guard; this makes the client honest rather than letting the user type
into a void.

### 3. States summary

| state | condition | row | copy |
|---|---|---|---|
| Connecting | `pending_join` && elapsed < 120 s | dimmed + badge | "Connecting…" |
| Not connected | `pending_join` && elapsed ≥ 120 s | dimmed + warning badge | "Not connected yet" (tooltip: they haven't accepted — still trying) |
| Active | not `pending_join` | normal | — |

## Error handling / edge cases

| case | behavior |
|---|---|
| First contact completes (Ack) | `pending_welcomes` row deleted → `is_pending` false → `group_state` reported `Active` → pending display clears |
| Contact removed while pending (#93 Task 6) | row deleted → `is_pending` false → contact gone from list |
| Hard dial failure at add (`DeliveryTimeout`) | zero writes (2.A) → no contact created → existing dialog error path unchanged |
| Clock skew (`now < added_at`) | clamp elapsed to 0 → treat as Connecting |
| `is_pending` DB error in `list_contacts` | propagate as the existing `map_err` storage error (do not silently mislabel) |
| Group genuinely `Corrupt` / blob missing | unchanged — `is_pending` override only applies to an otherwise-`Active` state |

## Test plan

**Core (Rust)**
- `list_contacts` reports `pending_join` for a contact with a `pending_welcomes`
  row even though `Group::load` returns `Active` (the load-bearing correctness
  test — must fail before the fix).
- `list_contacts` reports `active` once the `pending_welcomes` row is deleted
  (simulated Ack).
- `add_contact`'s `ContactAdded` result carries `group_state: Some(PendingJoin)`.

**UI (vitest)**
- `pendingState(contact, now)` helper: `connecting` when `pending_join` &&
  elapsed < 120 s; `unconfirmed` at ≥ 120 s; `null` when `active`; clamps
  negative elapsed.
- `ContactRow` renders "Connecting…" vs "Not connected yet" per `group_state` +
  `added_at` + a mocked `now`, and applies the dimmed class.
- Conversation view shows the pending banner (not the composer) for a
  `pending_join` contact.

## Files (anticipated)

- `crates/core/src/daemon/dispatch.rs` — `list_contacts` `is_pending` override;
  `add_contact` `ContactAdded` `group_state: Some(PendingJoin)`; core tests.
- `crates/ui/src-svelte/src/lib/stores/contacts.ts` — `pendingState` helper +
  `CONNECTING_GRACE` constant (or a small `$lib/pending.ts`).
- `crates/ui/src-svelte/src/lib/stores/now.ts` (new) — ticking `now` store.
- `crates/ui/src-svelte/src/lib/components/ContactRow.svelte` — two-state badge +
  de-emphasis.
- `crates/ui/src-svelte/src/routes/+page.svelte` — pending conversation banner.
- vitest specs for the helper, `ContactRow`, and the banner.
