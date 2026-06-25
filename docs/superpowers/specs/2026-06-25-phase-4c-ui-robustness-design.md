# Phase 4.C — UI Robustness & Data-Safety UX — Design

**Date:** 2026-06-25
**Status:** Approved (brainstorm) — ready for implementation planning
**Depends on:** Phases 4.D + 4.B merged. The D1 disclosure baseline shipped in 4.B
(`README.md` / `THREAT_MODEL.md`).
**Sibling sub-project:** 4.A (release/CI integrity) — out of scope here.

---

## Purpose

Close the v1.0 readiness audit's UI-robustness and data-safety findings so the
desktop (Tauri) user gets honest, actionable feedback and cannot lose data to a
silent failure or an un-guarded wipe. Four independent items, all UI-/client-side
except one new daemon command:

- **A — Structured error surfacing** (audit T2-7): stop flattening typed daemon
  errors into one opaque string.
- **B — Stream-death signal** (audit T2-7): stop the event relay dying silently
  and freezing the UI.
- **C — D1 first-contact waiting state**: surface *why* first contact hasn't
  completed and give a clean retry — the client-side D1 mitigation (waiting-state
  only; **no** auto-retry).
- **D — Backup export + wipe gate** (audit T2-9, T3-3): give the GUI user a way
  to back up, gate the one-click wipe behind it, and make the wipe's exit
  deterministic.

**No wire-protocol / ADR change.** The one new IPC command (`ExportBackup`) is an
additive local command, not a change to the frozen ADR-0006 mailbox protocol or
any peer-facing frame. No multi-member, no metadata-minimization, no auto-retry.

### Audit-finding correction

The audit's **T1-2** ("`skattr.sqlite.age` never created / pool never closed")
was **already resolved in Phase 2.B** — `run_with_transport` teardown calls
`pool.close()` deterministically, so a clean shutdown produces a fresh
`skattr.sqlite.age` and crash residue is re-encrypted on boot. Item D therefore
does **not** depend on a pool re-architecture; it snapshots the *live* DB instead
of relying on the at-rest `.age` (see D1).

---

## Item A — Structured error surfacing

**Problem.** The daemon built a 12-variant `DaemonErrorKind` taxonomy
(`crates/core/src/daemon/error_kind.rs`) and maps `CoreError → DaemonErrorKind`
via `CoreError::kind()`. But the Tauri bridge throws it away:
`crates/ui/src/ipc_bridge.rs:37` wraps **every** error as
`IpcError::Internal(format!("{e}"))`. The frontend then collapses further —
`AddContactDialog.svelte:42` shows one opaque "Failed to add contact." for
expired / consumed / bad-signature invites alike.

**Design.**

1. **Bridge (`crates/ui/src/ipc_bridge.rs`).** On `client.execute(cmd)` error,
   call `CoreError::kind()` (the same projection the daemon's own `map_err`
   uses): emit `IpcResponse::Err(IpcError::Daemon(kind))` when a typed kind
   exists, and `IpcError::Internal(truncated)` only for genuinely-untyped errors.
   This makes the bridge consistent with the in-process daemon's own error
   mapping. Note: the in-process IPC path may already deliver a structured
   `IpcError` — the planning step verifies whether `client.execute` returns a
   `CoreError` (needs `.kind()`) or already an `IpcError` (pass through); the fix
   is "preserve the structured kind," whichever form it arrives in.

2. **Frontend error helper (`crates/ui/src-svelte/src/lib/ipc/errors.ts`, new).**
   `errorMessage(err: IpcError): string` maps each `DaemonErrorKind` to a
   human-readable string. Baseline mapping:
   - `InviteExpired` → "This invite link has expired."
   - `InviteConsumed` → "This invite link has already been used."
   - `InviteSignatureInvalid` → "This invite couldn't be verified — it may be corrupted or tampered with."
   - `ContactNotFound` → "Contact not found."
   - `ContactAmbiguous` → "That name matches more than one contact."
   - `DeliveryTimeout` → "Couldn't reach your contact — they may be offline."
   - `TorNotReady` → "Still connecting to Tor — try again in a moment."
   - `GroupCorrupt` → "This conversation's secure state is damaged."
   - `StorageError` → "A local storage error occurred."
   - `SearchSyntax` → "That search query isn't valid."
   - `InvalidArgument { message }` → the message (already user-facing).
   - `Unauthorized` → "Not authorized."
   - `Internal` / unknown → a generic fallback ("Something went wrong.") plus the
     truncated detail in a dev/log channel, never the raw string as the primary
     message.
   The `ts-rs`-generated `DaemonErrorKind` / `IpcError` types already cross to TS,
   so the mapping is type-checked (a `switch` over the kind discriminant).

3. **Call sites.** `AddContactDialog.svelte` and the generic command-failure path
   (`client.ts` `unwrapOk` / the toast surface) use `errorMessage(...)` instead
   of the opaque collapse. Other dialogs that surface command errors adopt it
   where a typed kind improves the message (kept minimal — the add-contact flow
   is the audit's named example).

**Verification.** Frontend unit tests: `errorMessage` returns the right string
per kind; `AddContactDialog` renders the specific message for an
`InviteExpired` / `InviteConsumed` / `InviteSignatureInvalid` response (mocked
IPC). A Rust test on the bridge: a typed `CoreError` round-trips to
`IpcError::Daemon(expected_kind)` rather than `Internal`.

---

## Item B — Stream-death signal

**Problem.** `crates/ui/src/events.rs` spawns a relay loop
`while let Ok(ev) = client.next_event().await { … }` (line ~39). On the first
`Err` (IPC socket closed / daemon gone) it `break`s **silently** — no signal to
SvelteKit. The UI keeps its last state but receives no further messages,
delivery, or Tor-status updates: a frozen-but-not-obviously-broken UI until a
manual reload. The wire already has an unused `IpcResponse::Bye` terminal frame.

**Design.**

1. **Relay (`crates/ui/src/events.rs`).** When `next_event()` returns `Err` (or a
   `Bye`), the relay emits a **terminal marker** to the frontend before the task
   exits — a dedicated Tauri event `ipc:stream-closed` (app-global, not on the
   per-subscription `Channel<Event>`, so it survives the channel teardown). The
   marker carries a short reason string for logging.

2. **Connection store (`crates/ui/src-svelte/src/lib/stores/connection.ts`,
   new).** Tracks `'live' | 'reconnecting' | 'dead'`. On `ipc:stream-closed` it
   flips to `'reconnecting'` and triggers a **re-subscribe**: re-invoke
   `ipc_subscribe` with the same `EventFilter`, with bounded exponential backoff
   (e.g. 0.5 s → 1 s → 2 s → 4 s, cap ~8 s; after N failed attempts → `'dead'`).
   On a successful re-subscribe → `'live'`. The daemon's existing
   subscribe-replay re-delivers state the UI missed while disconnected.

3. **Banner.** An unobtrusive top banner shows in `'reconnecting'`
   ("Reconnecting to the app service…") and `'dead'` ("Disconnected — retry"
   with a manual retry button). Hidden in `'live'`. Wired once at the app shell
   (`+layout.svelte` / `+page.svelte`).

**Verification.** Frontend unit tests: the connection store transitions
`live → reconnecting → live` on a stream-closed event followed by a successful
re-subscribe, and `→ dead` after exhausting retries; the banner renders per
state. A Rust test (or controller-verified) that the relay emits the
`ipc:stream-closed` event on a closed/errored stream rather than exiting
silently.

---

## Item C — D1 first-contact waiting state (waiting-state only)

**Problem.** `add_contact` (`crates/core/src/daemon/dispatch.rs:287-436`)
**dials the inviter first** (mandatory, before any writes — the 2.A T2-1
atomicity invariant). So an offline inviter surfaces as a **dial failure**: the
add fails, nothing is written, and the invite is **not** consumed. The Welcome
send afterward is non-blocking; its failure surfaces only via a
`DeliveryStatusChanged` event. There is no UI state explaining a stalled first
contact.

**Design — two complementary, fully client-side pieces (no new persisted /
cross-peer / wire state):**

1. **Offline-at-add → clear error + clean retry in `AddContactDialog`.** When
   `add_contact` fails because the peer is unreachable, the dialog shows a
   first-contact-specific message — *"Couldn't reach your contact. First contact
   needs both of you online at the same time. Try again when they're back
   online."* — and **stays open** so the user can re-submit. Because the dial
   failed, the invite was never consumed, so re-submitting the same link is a
   clean retry. This is the "manual retry" the D1 mitigation calls for.
   - **Dependency on item A:** this requires `add_contact`'s dial-failure path to
     surface a recognizable `DaemonErrorKind` (e.g. `DeliveryTimeout` or
     `TorNotReady`) rather than an untyped error. The planning step checks the
     current dial-failure error; if untyped, map it to the closest existing kind
     (no new variant unless clearly warranted). This is the only daemon-side
     touch in item C and rides with item A's error work.

2. **Post-add "Connecting…" badge.** After a *successful* add, while the Welcome
   is in flight (`group_state == PendingJoin`), the contact row/header shows a
   "Connecting…" indicator. It resolves to active automatically when first
   contact completes — driven by the existing `DeliveryStatusChanged`
   (Welcome delivered) / `ContactUpdated` events; no retry needed there because
   the dial already succeeded. Derived purely from existing `ContactSummary`
   state (`group_state`) + events.

**Explicitly NOT in scope (per the D1 guardrail):** joiner-side background
auto-retry of the Welcome; any persisted "first-contact pending" intent; any
mailbox fallback for the Welcome. The waiting state + manual re-submit is the
whole of item C.

**Verification.** Frontend unit/e2e: an `add_contact` failure with a
peer-unreachable kind renders the first-contact message and keeps the dialog
open (mocked IPC); a `PendingJoin` contact shows the "Connecting…" badge and
clears it on a `ContactUpdated`/delivery event.

---

## Item D — Backup export + wipe gate + completion signal

**Problem.** Settings → Advanced has a one-click "Delete all data and quit"
(`routes/settings/advanced/+page.svelte:181-209`) behind a two-stage confirm, but
**no backup gate**; backup is **CLI-only** (`skattr backup`,
`crates/cli/src/main.rs:540-563`) — the GUI user can wipe everything but can't
back up. The wipe handler (`dispatch.rs:1679-1714`) also uses a fragile fixed
`sleep(150ms)` to let the reply flush before `process::exit` (audit T3-3).

**Design.**

**D1 — New daemon command `Command::ExportBackup { dest_path: String }`.** A
live, daemon-side export (the daemon is already unlocked, so no passphrase
re-prompt):
- Derive the backup key from the **in-memory identity**
  (`derive_storage_seed(identity)` → `HKDF(seed, "skattr-backup-v1")`).
- Produce a **consistent snapshot of the live DB**: `wal_checkpoint(TRUNCATE)`
  then `VACUUM INTO <temp>` (a consistent plaintext copy even with WAL active) —
  do **not** rely on the at-rest `.age` (stale between clean shutdowns).
- Reuse `storage::backup::export_backup`'s tar-gz + age-encrypt logic, refactored
  (or a sibling `export_backup_from_live`) to take the DB **snapshot path** for
  the DB component while still bundling the current `identity.vault` +
  `hs.key.age`. Write atomically to `dest_path`; **delete the temp plaintext
  snapshot immediately** (it lives in the 0700 data dir from 4.D and is no new
  exposure — the working `skattr.sqlite` is already plaintext on disk during
  operation).
- Returns `CommandResult::Ok` (reusing the existing variant) / a typed error
  (`StorageError` / `InvalidArgument` for a bad path). `Command::ExportBackup` is
  an **additive** local-IPC variant — the IPC command set is **append-only** (per
  `crates/core/tests/wire_format_append_only.rs`), so adding it requires updating
  that snapshot test (the exhaustive `command_variant_tag` match arm + the
  `expected_command_variant_set` list, `"export_backup"`) in the same commit.
  This is the local CLI/UI↔daemon IPC, **not** the frozen ADR-0006 mailbox *peer*
  protocol — no peer-facing frame changes.

**D2 — Settings → Advanced "Export backup…".** A new action using the Tauri
dialog **save-file** picker (the `dialog` plugin is already a dependency from
3.D) → calls `ExportBackup { dest_path }` → success/failure toast via item A's
`errorMessage`. Surfaces near (but above) the danger zone.

**D3 — Gate the wipe.** The first confirm dialog becomes three-way:
**[Export backup first]** / **[Continue without backup]** / **[Cancel]**.
"Export backup first" runs the picker + `ExportBackup`; on success it advances to
the existing final ("Are you absolutely sure?") confirm. Backup is prominent and
one-click but **not mandatory** (a user who genuinely wants to wipe without
backup can).

**D4 — Wipe completion signal (T3-3).** Replace the fixed `sleep(150ms)` in
`wipe_all_data` with a deterministic flush: send the `Ok` reply, await a oneshot
that the IPC writer signals once the reply is actually flushed to the socket,
*then* drop the handle, `remove_dir_all`, and `exit(0)`. Removes the
reply-flush-vs-exit race. (Scope check at planning: if wiring a flush-confirmation
oneshot through the IPC writer proves to reach beyond a contained change, fall
back to awaiting the writer's flush at the dispatch boundary; do not redesign the
IPC layer for this.)

**Verification.** Rust: `ExportBackup` on a running in-process daemon writes an
archive that `import`/restore round-trips (or at least decrypts under the
backup key and contains the three members) and that the temp snapshot is removed;
the wipe completion path signals deterministically (no sleep) in a unit/harness
test. Frontend: the export action invokes `ExportBackup` with the picked path and
toasts on result; the wipe gate offers the three choices and only reaches the
final confirm after a successful export (or an explicit "continue without
backup").

---

## File structure (where changes land)

**Rust (`crates/core`, `crates/ui/src`):**
- `crates/ui/src/ipc_bridge.rs` — preserve structured `IpcError` (A).
- `crates/ui/src/events.rs` — emit `ipc:stream-closed` on relay death (B).
- `crates/core/src/daemon/commands.rs` — `Command::ExportBackup` variant (D).
- `crates/core/src/daemon/dispatch.rs` — `export_backup` handler (D); typed
  dial-failure error for add_contact if needed (C/A); wipe completion signal (D4).
- `crates/core/src/storage/backup.rs` — `export_backup_from_live` (snapshot
  variant) (D).

**Frontend (`crates/ui/src-svelte`):**
- `lib/ipc/errors.ts` (new) — `errorMessage(IpcError)` (A).
- `lib/stores/connection.ts` (new) — connection-state store + re-subscribe (B).
- `lib/components/AddContactDialog.svelte` — typed errors + first-contact message
  (A, C).
- contact list/header component — "Connecting…" badge (C).
- app shell (`+layout`/`+page`) — reconnect banner (B).
- `routes/settings/advanced/+page.svelte` — Export backup action + three-way wipe
  gate (D).
- `lib/stores/config.ts` (or a new `backup.ts`) — `exportBackup(path)` IPC
  wrapper (D).

Each item is independently reviewable; the natural task order is **A → B → C → D**
(C depends on A's typed errors; D is the largest and self-contained).

---

## Non-goals

- **Joiner auto-retry of the Welcome** and any persisted first-contact intent
  (D1 guardrail — waiting-state only).
- **Mandatory backup before wipe** (offered, not forced).
- **GUI restore** from a backup archive (export only this round; restore stays
  CLI `restore-backup`, documented in 4.B's passphrase-recovery guide).
- **Mailbox fallback for the first-contact Welcome** (protocol work, v1.1).
- **Pool re-architecture** (T1-2 already fixed in 2.B).
- Any change to the frozen ADR-0006 mailbox wire protocol or peer-facing frames.

---

## Risks

- **Live-DB snapshot consistency.** `VACUUM INTO` after a `wal_checkpoint` gives a
  consistent copy; the planning step confirms it works under an active WAL pool
  and that the snapshot + the two at-rest files restore cleanly. Fallback: the
  SQLite online-backup API. Either way the temp plaintext is deleted promptly.
- **Bridge error-shape mismatch.** Whether `client.execute` surfaces a
  `CoreError` or an already-mapped `IpcError` decides the exact one-line fix;
  resolved by reading the in-process client at planning time.
- **Re-subscribe storms.** Bounded exponential backoff + a `dead` terminal state
  prevent a hot reconnect loop if the daemon is genuinely down.
- **Wipe completion-signal scope.** Guard-railed in D4: keep the change contained
  to the dispatch/IPC-writer boundary; do not redesign IPC.
