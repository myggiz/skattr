# #109 — Remove a stuck "Connecting…" contact (design)

**Issue:** myggiz/skattr#109
**Milestone:** v0.1.2
**Status:** design approved (brainstorming 2026-07-20)
**Wire/protocol change:** none (local IPC + storage only; no ADR)

## Goal

Let a user remove an **unconnected (pending) contact** — the stuck
"Connecting…" / "Not connected yet" entry (#101) — from both the UI and the
CLI, **fully wiping its local state** so a fresh invite/add of the same peer
starts from a clean slate. Fully-connected contacts keep today's soft-archive
(hidden, history preserved).

This is the recovery/escape hatch for a stalled first contact. It does **not**
attempt to fix *why* first contact stalls — that is #107 (welcome-sweep rebuild)
and #108 (contingent finalize), each its own spec. #109 is the prerequisite that
makes those testable: it lets you wipe stale attempt state and re-invite.

## Motivation

Field-hit on v0.1.6 (2026-07-20). When first contact stalls (`Psk(KeyNotFound)`
#107, `WrongGroupId` #108), the half-formed contact cannot be removed from any
shipped surface:

- The UI has no delete/cancel action for a pending contact.
- The CLI has no `remove` subcommand at all.
- The daemon's `RemoveContact` is **soft-delete only**
  (`crates/core/src/daemon/dispatch.rs:1580` → `contacts.hidden = 1` + drop
  pending welcome; **preserves** the MLS group, messages, outbox), so even
  reaching it leaves the orphaned group that produces #108's `WrongGroupId`
  frame flood — a subsequent add of the same peer does not start clean.

## Decisions (locked in brainstorming)

1. **Hard-wipe applies to pending/unconnected contacts only.** Connected
   contacts keep the existing soft-archive. (Not a general "forget everyone"
   feature.)
2. **Selection is implicit.** One command — `RemoveContact` — branches on
   contact state in the daemon. Callers (UI/CLI) never choose soft vs hard.
3. **Scope is the consumer-side stuck contact** (the entry that appears in the
   contact list after you ran Add). Cancelling a *self-created, never-consumed
   invite* (an unused `outstanding_invites` KeyPackage) is **out of scope**.
4. **Clear-set is a full peer-keyed purge** (see below), including orphaned
   outbox frames and any optimistic local messages.
5. **UI shows a lightweight confirm dialog** before the irreversible wipe.

## Architecture / behavior

`Command::RemoveContact { contact }` becomes **state-aware** in the daemon
(`remove_contact` in `crates/core/src/daemon/dispatch.rs`):

- **Pending/unconnected** — `PendingWelcomeRepo::is_pending(&contact.0)` is true
  (the exact signal #101 uses to render "Connecting…"/"Not connected yet"):
  → **hard purge** (below).
- **Connected** — not `is_pending`:
  → **soft-delete** exactly as today (`set_hidden(true)` + drop pending welcome).
  Unchanged.

Using `is_pending` as the predicate means the user-facing rule is simply:
*if the contact isn't connected, remove wipes it; if it is, remove archives it.*
No new state concept is introduced.

### Hard-purge clear-set

All deletes run inside **one** `pool.transaction` so the purge is atomic — a
half-purge (e.g. contact gone but MLS group left behind) is exactly the
divergent state we are trying to eliminate. Every step keyed by the peer's
identity pubkey or its `group_id`:

| Table | Key | Repo method |
|---|---|---|
| `mls_groups` | `group_id` (from `ContactRepo::get_group_id`) | **new** `MlsGroupRepo::delete_in_tx` |
| `pending_welcomes` | `peer_pubkey` | **new** `PendingWelcomeRepo::delete_in_tx` (existing `delete` is non-tx) |
| `first_contact_acks` | `peer_pubkey` | **new** `FirstContactAckRepo::delete_by_peer_in_tx` |
| `outbox` | `target = peer` | **new** `OutboxRepo::delete_by_target_in_tx` |
| `messages` | `contact = peer` | **new** `MessageRepo::delete_by_contact_in_tx` |
| `contacts` (+ onions cascade) | `identity_pubkey` | existing `ContactRepo::remove` (adapt to run in-tx) |

Notes:
- `outbox` deletion clears the orphaned `MlsApp` frames the pending peer would
  otherwise keep emitting — directly removes #108's `WrongGroupId` source for
  the wiped peer.
- `messages` for a never-connected contact are at most optimistic local
  bubbles; wiping them is consistent with "remove completely."
- The MLS group is a single serialized `state_blob` row (snapshot model,
  `crates/core/src/storage/groups.rs`); deleting the row is a complete removal —
  no scattered OpenMLS key material to reconcile. Implementation must confirm
  the OpenMLS provider is snapshot-backed (rebuilt from the blob on load), so no
  separate keystore rows survive; if the provider persists key material
  elsewhere, extend the clear-set accordingly.
- `read_state` / `seen_messages`: include a peer-keyed delete only if those
  tables key on the contact; otherwise omit (they are not first-contact
  blockers). Implementation decides based on the actual schema.

If any delete in the transaction fails, the whole purge rolls back and
`RemoveContact` returns an error — no silent partial cleanup.

### Event

Add `Event::ContactRemoved(PublicKey)` to `crates/core/src/daemon/events.rs`.
Emit it after a successful **hard purge** *and* after a **soft-delete** (a
live UI should drop the row in both cases rather than re-fetch a now
hidden/absent contact via `ContactUpdated`). `ContactUpdated` remains for
mutations that keep the contact present.

## Surfaces

### CLI (`crates/cli/src/main.rs`)

New subcommand `remove <contact>`:
- Resolve `<contact>` via the existing `ListContacts` + `resolve_contact`
  path (hex prefix or nickname), mirroring `send`.
- Issue `Command::RemoveContact { contact }`.
- Print the outcome distinctly: `archived <name>` (soft) vs
  `removed <name> (local state wiped)` (hard). The daemon returns
  `CommandResult::Ok` today; to report which branch ran, either (a) return a
  richer result (e.g. `CommandResult::ContactRemoved { hard: bool }`) or
  (b) have the CLI read the contact's pending state before removal. Prefer (a)
  — a small additive result variant — so the CLI and any caller learn what
  happened.
- Honors `$SKATTR_SOCKET`, so it can operate against a running daemon (also
  useful for clearing state during our own testing).

### UI (Tauri + SvelteKit, `crates/ui/src-svelte`)

- A **Remove/Cancel** action for a pending contact, surfaced in:
  - the contact row's menu (`ContactRow.svelte`), and
  - the disabled-composer banner shown for pending contacts on `+page.svelte`
    ("They haven't accepted your invite yet — **Remove**").
- A **confirm dialog** ("Remove this pending contact? This clears the local
  invite attempt so you can start over.") — destructive styling.
- On confirm → invoke the existing `RemoveContact` IPC command (already present
  in `Command.ts` bindings).
- On `Event::ContactRemoved` → remove the contact from `stores/contacts.ts`
  and, if it was the active conversation, clear the conversation view.

The Remove action is offered for **pending** contacts here (the #109 scope).
Whether to also surface archive for connected contacts in the same menu is a UI
choice for the plan; #109 only requires the pending path.

## Error handling

- `RemoveContact` on an unknown contact returns `ContactNotFound` (match
  existing `set_contact_muted` semantics), not a silent no-op.
- `RemoveContact` is **idempotent**: removing an already-removed contact
  returns `Ok` (hard purge of a peer with no rows deletes nothing and succeeds).
- Transaction failure → typed error surfaced to the caller; nothing partially
  deleted.

## Testing (TDD)

**Daemon (`crates/core`, unit + `test-harness`):**
- `remove_contact_hard_purges_pending`: seed a pending contact (contact row +
  mls_group + pending_welcome + first_contact_ack + an outbox row + a message),
  call `RemoveContact`, assert **every** listed table has no row for the peer.
- `remove_contact_soft_deletes_connected`: a connected (Active, not pending)
  contact → still `hidden=1`, MLS group + messages **preserved** (guards the
  unchanged branch).
- `remove_contact_is_idempotent`: second call returns `Ok`, no error.
- `remove_contact_unknown_returns_not_found`.
- `fresh_add_after_purge_starts_clean`: after a hard purge, an `add_contact`
  for the same peer creates state with no leftovers (no stale group/pending
  rows collide).
- New repo delete methods each get a focused test (delete-in-tx removes only
  the targeted rows).

**CLI (`crates/tests` or cli unit):**
- `remove` subcommand resolves a contact and issues `RemoveContact`; prints the
  hard/soft outcome. Reuse existing CLI IPC test patterns.

**UI (`crates/ui`, vitest):**
- `stores/contacts.ts` handles `ContactRemoved` by dropping the contact.
- Component test: pending-contact Remove action → confirm → invokes
  `RemoveContact`; cancel does nothing.

## Out of scope

- Cancelling a self-created, never-consumed invite (unused KeyPackage).
- Hard-removing connected contacts / a general "forget everyone" action.
- The #107 (welcome-sweep per-connection rebuild) and #108 (contingent
  finalize) protocol fixes — separate specs, done next.

## Acceptance criteria

- A pending "Connecting…" contact can be removed from **both** the UI and the
  CLI.
- After removal, **no** row for that peer remains in `contacts`, `mls_groups`,
  `pending_welcomes`, `first_contact_acks`, `outbox`, or `messages`.
- A fresh invite/add of the same peer afterward starts clean (no stuck state
  from leftover rows).
- Removing a **connected** contact still soft-archives (hidden, history kept) —
  behavior unchanged.
- `RemoveContact` is idempotent and returns `ContactNotFound` for an unknown
  contact.
- Local gate green: `cargo fmt`, `clippy -D warnings`, `cargo test`,
  `pnpm check` + vitest.
