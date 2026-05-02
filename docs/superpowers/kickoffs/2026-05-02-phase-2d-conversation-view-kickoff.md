Phase 2.C just merged at master `07a90de` (PR #1). The read-only
conversation MVP is live: `crates/ui/` Tauri 2 + SvelteKit shell with
in-process `Daemon::run`, two-phase Tauri command surface (3 pre-daemon
wizard commands + 3 post-daemon IPC bridge commands), four-step
first-run wizard (welcome → passphrase → 24-word BIP39 type-back → Tor
bootstrap), read-only main shell with contact list + open conversation
pane, live-append on `Event::MessageReceived`. Wire surface additions:
`Command::DaemonInfo`, `ContactSummary` projection fields
(`unread_count`, `last_message_preview`, `last_ts_recv`, all
`#[serde(default)]`), and a filter-gated `TorStatusChanged` replay on
`Subscribe` ack backed by `DaemonHandle::latest_tor_status` + tap task.
ts-rs emits TS bindings for every wire type (gitignored).

Phase 2.D implements the conversation view: a composer (Enter-to-send,
Shift+Enter newline, paste-as-plaintext only), per-message delivery
state icons (clock → check → check-check → ! tracking the existing
`DeliveryStatus` enum), virtualised list pagination on scroll-to-top
with skeleton rows, a mark-read separator above the first unread, and
a mark-read cursor that advances when the bottom enters the viewport.
**No invite UX, no settings, no mailbox CRUD UI** — those land in 2.E
and 2.F. Round-trip messaging from UI to UI (paired daemons on one
machine) is the exit criterion.

The umbrella decomposition (`docs/superpowers/specs/2026-04-26-
phase-2-ui-decomposition.md`) §"2.D — Conversation view" has the
authoritative scope. The locked architectural decisions there are
binding — do not relitigate them in the brainstorm:

1. Send composer + delivery state icons + pagination + mark-read are
   the 2.D additions; everything else is deferred.
2. Wire-format additions for 2.D (additive only):
   - `Command::SendMessage` reply extended:
     `CommandResult::MessageSent { message_id, status, record:
     MessageRecord }` so the UI renders the sent bubble synchronously.
     The existing `MessageSent { message_id, status }` shape gains a
     `record` field with `#[serde(default)]` for backward decode.
   - `Command::RecentMessages` gains `before_id: Option<i64>` cursor;
     response gains `next_before_id: Option<i64>` for paged scroll-back.
3. Send path on the daemon side must persist the sender-side
   `MessageRecord` *before* the IPC reply returns, so the UI's
   optimistic render uses the same row that subsequently triggers
   `Event::MessageReceived` on the receiver. Idempotency is preserved
   by 1.H's `(group_id, envelope_id)` unique index — no duplicate
   sender rows on retry.
4. Virtualised message list keeps `@tanstack/svelte-virtual` from 2.C
   (the substitution for `svelte-virtual-list`).
5. `paste-as-plaintext only` — composer's `paste` event handler reads
   `event.clipboardData.getData("text/plain")` and inserts it; the
   default rich-paste path is preempted. No HTML, no images.
6. Composer disabled state: when daemon is down, when contact is
   removed (Phase 2.E will add the soft-delete bit), or when MLS
   group state for the active contact is `Removed | Corrupt` (those
   states already exist in `mls::state` from Phase 1.C).

Please start by invoking `superpowers:brainstorming` to refine 2.D's
internals. Topics worth pinning down (the umbrella deferred these to
per-sub-phase brainstorming):

- **Optimistic vs. wait-for-record send rendering.** Two extremes:
  (a) immediately append a placeholder bubble client-side on Enter,
  reconcile when `MessageSent.record` arrives;
  (b) wait for the IPC reply (~tens of ms), append the canonical
  `MessageRecord` then. Both are simple. (b) avoids a flicker but
  shows latency; (a) is snappier but risks UI/wire divergence on
  errors. Lock one as the 2.D default.
- **Delivery icon set + colours.** Phosphor / Lucide / Tabler /
  bundled handcrafted SVGs. The 2.C policy is "no remote CDNs, no
  HTML rendering of content" — icons must be bundled. Recommendation:
  Lucide (MIT, ~tree-shakable, ~50 KiB total for 4 icons). Lock the
  4 glyphs: pending / sent / delivered / failed.
- **Pagination cursor semantics.** `before_id: Option<i64>` returns
  rows with `id < before_id` (strict-less), oldest-first or
  newest-first? The 1.G `recent` already returns most-recent-first;
  paging upward (older) wants `id < before_id ORDER BY id DESC LIMIT`.
  Confirm + write the test.
- **Skeleton rows.** When the user scrolls to the top of a conversation
  with more history, render N placeholder bubbles for the in-flight
  page request? Or just a single spinner? Pick one and document the
  loading-state UX.
- **Mark-read trigger.** "Bottom enters viewport" via
  `IntersectionObserver` on the last visible bubble? Or "user types
  in composer" / "user clicks contact again"? Lock the trigger and
  the debounce (no `MarkRead` IPC per scroll tick).
- **Mark-read separator persistence.** The unread separator marks the
  position of the *last-read cursor when the conversation was opened*
  — not advanced live as new messages arrive, otherwise the user
  can't see what's new. Confirm + add a test.
- **Composer Unicode + IME handling.** Enter-to-send must NOT trigger
  during IME composition (Japanese/Chinese/Korean input). Use
  `compositionstart` / `compositionend` to gate. Lock the behaviour.
- **Outgoing-message bubble styling.** 2.C's `MessageBubble.svelte`
  already has the `.outgoing` class wired (right-aligned, `--accent`
  background). 2.D just exercises the existing CSS — confirm no
  changes needed.
- **`Command::SendMessage`'s pre-2.D `MessageSent { message_id,
  status }` consumers.** The CLI is the only one. After the wire
  change adds `record`, the CLI should ignore it (or surface it in
  `--verbose`). Confirm or update CLI.
- **Test plan additions.** Playwright for composer happy path (type
  → Enter → bubble appears with pending icon → mock advances state
  → check-check icon). Vitest for paste-as-plaintext sanitisation.
  Rust integration test in `crates/tests/`: paired daemons exchange
  a real message via the existing transport; UI fixture asserts the
  delivery progression.

## Context

- `docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md` §
  "2.D — Conversation view" — sketch + locked decisions. Read first.
- `docs/superpowers/specs/2026-05-01-phase-2c-ui-bootstrap-design.md`
  — what 2.C shipped; Phase 2.D inherits the IPC adapter, stores,
  components, tokens, and font.
- `docs/skattr-implementation-plan.md` Phase 2 §Workstream 2.D — the
  original detailed task list.
- `crates/core/src/daemon/{commands,events,dispatch}.rs` — current
  `Command`/`CommandResult`/`Event` surface; 2.D's additions are
  append-only on top.
- `crates/core/src/storage/messages.rs` — existing `MessageRepo::recent`
  is the pattern for the new `before_id`-cursor variant.
- `crates/ui/src-svelte/src/lib/components/{MessageBubble,VirtualMessageList}.svelte`
  — existing 2.C components 2.D extends.
- `crates/ui/src-svelte/src/lib/stores/conversation.ts` — 2.C's
  store; 2.D adds `loadOlder()` + delivery-state subscription.
- CLAUDE.md locked decisions remain binding. `crates/ui/` ships
  GPLv3 (matches `core`/`cli`/`tests`).

## Locked from the 2.C merge (do not relitigate)

- Wire-format append-only — every protocol change must extend
  existing variants with `#[serde(default)]` fields or add new ones.
- `Subscribe` ack replay is filter-gated against `event_matches`;
  any new replay (e.g. for `MailboxStatusChanged`) follows the same
  pattern.
- `bootstrap.rs` is capped at 3 pre-daemon Tauri commands by lint
  test. New post-daemon commands go through `ipc_request`.
- Quit-on-close is a 2.C-only behaviour to be replaced by hide-to-tray
  in 2.F. 2.D should NOT touch the close handler.
- `@tanstack/svelte-virtual` is the locked virtualised list lib.
- `ts-rs` outputs are gitignored; commit hook is lint-only.
- `esrap@1.4.9` patch and dev-only CSP relaxation are documented in
  the CHANGELOG; carry them forward unchanged.

## After brainstorming

- `superpowers:writing-plans` to author the implementation plan.
- `superpowers:using-git-worktrees` to branch off master onto a
  `phase-2d-conversation-view` branch.
- `superpowers:test-driven-development` +
  `superpowers:subagent-driven-development` to execute.
- `superpowers:verification-before-completion` before the merge PR.

## Out of scope for 2.D

- Invite link generation + scan / paste UX (2.E).
- Contact rename / remove (2.E).
- Settings panel (2.F).
- Mailbox CRUD UI (2.F — wire surface from 2.B is in place).
- Notification system (2.F).
- Tray + minimize-to-tray (2.F).
- Packaging / installers (2.G).
- Multi-member groups (Phase 3).
- Phase 2.B follow-ups: Task 20.5 (peer direct-timeout trigger),
  Task 22.5 (RemoveMailbox drain dispatch), Task 23.5 (real HS key
  rotation). Independent and tracked in CLAUDE.md.
- Wire-format BREAKING changes — anything renaming or removing a
  Command / CommandResult / Event variant requires a separate spec.
- Avatars / reactions / replies / edits / typing indicators (Phase 3).
- Attachments / file send (Phase 3 — `Kind::File` is reserved but
  the UI doesn't surface it in 2.D).
