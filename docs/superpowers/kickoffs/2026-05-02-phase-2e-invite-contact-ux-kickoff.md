Phase 2.D just merged at master `3d1f606` (`Merge branch
'phase-2d-conversation-view' — Phase 2.D conversation view`).
The conversation view is live: composer (Enter-to-send,
Shift+Enter newline, IME-safe, paste-as-plaintext), per-message
delivery state icons (clock → check → check-check → !), scroll-back
pagination via `Command::RecentMessages.before_id` cursor returning
the new `CommandResult::MessagesPage` variant, frozen "Unread"
separator anchored to `ContactSummary.last_read_row_id` at
conversation-open, debounced mark-read on open + bottom-of-list
intersection. Wire-format additions are strictly additive (every
new field defaults via `#[serde(default)]`, every new variant
sits alongside the existing ones). New
`crates/core/tests/wire_format_append_only.rs` makes adding or
reshaping a `Command`/`CommandResult` variant a deliberate edit
via exhaustive match arms.

Three production bugs were caught during 2.D's e2e harness work:
(1) `routes/+page.svelte` did not call `refreshContacts()` on
direct navigation to `/`; (2) `delivery_status_changed` events
from the subscribe stream were silently dropped; (3) `.shell`
CSS lacked `grid-template-rows: 100vh; overflow: hidden`,
collapsing the virtualizer. All fixed in 2.D.

**Known limitation carried into 2.E:** the `AddContact` IPC
dispatcher creates the MLS group on the consumer (Bob) side but
does NOT propagate the resulting Welcome message to the inviter
(Alice). Consequence: the inviter cannot decrypt messages from
the new contact until the Welcome is delivered. Tracked as
**Task 2.E.0** below — must be wired before the new add-contact
UI is meaningful end-to-end.

Phase 2.E implements **Invite & contact UX**: invite generate
dialog (optional nickname, TTL slider, inline-rendered QR,
copy-to-clipboard), add-contact dialog (paste tab for
`skattr://invite/v1#…`; scan tab via `getUserMedia` + bundled
`jsqr`), contact details panel (pubkey short-hash with click-to-copy,
current onion, mailbox list, inline rename, remove with
confirm). Plus the Task 2.E.0 daemon-side fix to propagate
Welcome to the inviter on `AddContact`.

The umbrella decomposition spec
(`docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md`)
§"2.E — Invite & contact UX" has the authoritative scope. The
locked architectural decisions there are binding — do not
relitigate them in the brainstorm:

1. New `Command::RenameContact { contact, nickname: Option<String> }`
   and `Command::RemoveContact { contact }` (soft-delete: mark
   hidden, MLS group preserved for replay safety).
2. Invite URL scheme stays `skattr://invite/v1#…` — fragment-only
   params (no referer leaks).
3. QR rendering: bundled `qrcode` Rust crate emitting SVG via a
   Tauri command; **no remote QR services**.
4. QR scanning: bundled `jsqr` (MIT, ≈ 50 KiB); webcam permission
   is requested only when the scan tab is opened.
5. Wire-format append-only — every protocol change extends
   existing variants with `#[serde(default)]` fields or adds new
   ones. Lint test (`crates/core/tests/wire_format_append_only.rs`)
   enforces this at compile time.
6. Soft-delete semantics for `RemoveContact`: a new
   `contacts.hidden: bool` column (with a migration); the daemon
   filters hidden contacts out of `ListContacts` by default; MLS
   group state stays intact so replay/restore works. A future
   "show archived" view (Phase 3+) re-surfaces them.
7. Welcome propagation (Task 2.E.0): when `AddContact`
   constructs the new MLS group on Bob's side, the resulting
   Welcome message must be sent to Alice over the existing
   `DeliveryHub` direct/mailbox path. Alice's daemon processes
   the Welcome, transitions the group from `PendingJoin` to
   `Active`, and emits a `ContactUpdated` event so the UI
   re-fetches the summary.

Please start by invoking `superpowers:brainstorming` to refine
2.E's internals. Topics worth pinning down (the umbrella
deferred these to per-sub-phase brainstorming):

- **Welcome propagation transport.** Should the Welcome ride the
  same `DeliveryHub` plumbing as ordinary messages (with
  `Envelope::Kind::Welcome` or similar)? Or a sibling
  `Command::SendWelcome` path? Lock the wire shape — this is
  the single most consequential decision in 2.E because it
  touches the protocol surface.
- **`RenameContact` semantics.** Local-only nickname (no wire
  effect on peers), or does it propagate via a
  `ContactCard` update? Recommendation: **local-only** —
  nickname is metadata for the local user's contact list, not a
  cryptographic claim. The peer's own `ContactCard` is the
  source of truth for their identity. Confirm or revisit.
- **`RemoveContact` confirmation UX.** The remove action is
  destructive (hides history from the default view) but
  reversible via the soft-delete bit. What confirm-dialog
  copy makes the consequences clear without scaring users?
  Suggest: "Hide [nickname]? Their messages stay encrypted
  on disk; you can restore later from Settings."
- **Soft-delete migration.** Migration `0010` adds
  `contacts.hidden BOOLEAN NOT NULL DEFAULT 0`. `ListContacts`
  filters `WHERE hidden = 0` by default. Add a `Command` field
  `include_hidden: bool` (default false) to opt in for the
  future archived-view UX.
- **Invite expiry visualisation.** TTL slider in the generate
  dialog: what range (1h–7d)? What's the default? The umbrella
  says default 24h is fine; confirm.
- **QR rendering library.** The Rust `qrcode` crate (MIT/Apache)
  is the recommendation. It emits SVG strings — clean for
  inline `{@html}` in Svelte. Confirm + check it's not pulling
  in a long dep chain.
- **QR scanning library.** `jsqr` (MIT, ≈ 50 KiB) is the
  recommendation. It works with `getUserMedia` + a `<canvas>`.
  Confirm + verify it's compatible with Vite's bundling.
- **Webcam permission flow.** First time the scan tab is
  opened, the browser prompts for camera permission. If denied,
  show a clear "paste an invite URL instead" affordance. Lock
  the UX — minimum surprise.
- **Contact details panel layout.** Right-side drawer, modal,
  or inline expansion of the contact list row? Recommendation:
  **inline expansion** — least new shell surface, matches the
  Apple Mail / Linear pattern.
- **Pubkey short-hash format.** First 4 + last 4 hex
  characters with copy-to-clipboard? `7aa2...f7` ≤8 chars?
  Confirm or pick another mnemonic format.
- **Test plan additions.** Vitest for invite-generate happy
  path + add-contact paste path. Playwright for the full
  invite-generate → render-QR → copy → add (paste) flow with
  daemon mock. Manual test plan for the scan path (jsdom can't
  render `getUserMedia`); fixture-based scan in Playwright is
  optional. Rust integration test in `crates/tests/` exercising
  Welcome propagation end-to-end.

## Context

- `docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md` §
  "2.E — Invite & contact UX" — sketch + locked decisions. Read
  first.
- `docs/superpowers/specs/2026-05-02-phase-2d-conversation-view-design.md`
  — what 2.D shipped; 2.E inherits the IPC adapter, stores,
  components, tokens, and the `wire_format_append_only` lint test.
- `docs/skattr-implementation-plan.md` Phase 2 §Workstream 2.E —
  the original detailed task list.
- `docs/skattr-design.md` §"Invite link format" — wire shape,
  fragment params, signature scope.
- `docs/adr/0006-mailbox-protocol-v1.md` — wire freeze 2.B
  develops against (not directly touched in 2.E, but contact
  rename/remove may affect mailbox-side logic).
- `crates/core/src/invite.rs` — existing `InviteLink::{to_url,
  from_url, sign, verify}` + canonical-CBOR signature scheme.
- `crates/core/src/contact.rs` — `ContactCard::{sign, verify}`
  + monotonic-version persistence.
- `crates/core/src/daemon/dispatch.rs::add_contact` — the
  current handler that needs extending for Welcome propagation
  (Task 2.E.0).
- `crates/ui/src-svelte/src/lib/test/tauri-mock.ts` — fixture
  pattern from 2.D's Playwright e2e; reuse + extend.
- CLAUDE.md locked decisions remain binding. `crates/ui/` ships
  GPLv3.

## Locked from the 2.D merge (do not relitigate)

- Wire-format append-only — every protocol change must extend
  existing variants with `#[serde(default)]` fields or add new
  ones. The new `wire_format_append_only` snapshot test enforces
  this; update the static lists alongside any addition.
- ts-rs emits `Hex16`, `PublicKey`, `MessageId` as bare lowercase
  hex `string` (NOT tuple-struct objects). Don't write
  byte-comparison logic in TS; use `===` on hex strings.
- `Command::RecentMessages.paged: bool` is the discriminator for
  paged vs. tuple response shape. CLI omits; UI sets `true`.
- `MessageSent.record: Option<MessageRecord>` is `None` only on
  the duplicate-retry branch — UI's optimistic placeholder
  reconciles via `__tempId`.
- `ContactSummary.group_state: MlsGroupStateLabel | null` —
  composer disables on anything other than `"active"`. 2.E adds
  `"hidden"` semantically (via filter, not a new label) — the
  contact stops appearing in the list when hidden, so the
  composer never renders for it.
- Cross-conversation race protection in `conversation.ts`:
  `markReadIfAtBottom`, `loadOlder`, and `send` all capture the
  contact at schedule/dispatch time and bail at fire/resolve
  time if the contact changed. New 2.E IPC paths
  (`RenameContact`, `RemoveContact`) follow the same idiom if
  they need any async lookup.
- Tauri mock fixture pattern (`?fixture=…` query param in
  `tauri-mock.ts`) is the way to seed e2e state. Add fixture
  branches for invite-generate / add-contact flows.
- `wire_format_append_only.rs` snapshot lists must be updated
  in the same commit as any `Command`/`CommandResult` addition.

## After brainstorming

- `superpowers:writing-plans` to author the implementation plan.
- `superpowers:using-git-worktrees` to branch off master onto a
  `phase-2e-invite-contact-ux` branch.
- `superpowers:test-driven-development` +
  `superpowers:subagent-driven-development` to execute.
- `superpowers:verification-before-completion` before the merge.

## Out of scope for 2.E

- Settings panel (2.F).
- Mailbox CRUD UI (2.F — wire surface from 2.B is in place).
- Notification system (2.F).
- Tray + minimize-to-tray (2.F).
- Packaging / installers (2.G).
- Multi-member groups (Phase 3).
- Phase 2.B follow-ups: Task 20.5 (peer direct-timeout trigger),
  Task 22.5 (RemoveMailbox drain dispatch), Task 23.5 (real HS
  key rotation). Independent and tracked in CLAUDE.md.
- Wire-format BREAKING changes — anything renaming or removing a
  Command / CommandResult / Event variant requires a separate
  spec.
- Avatars / reactions / replies / edits / typing indicators
  (Phase 3).
- Attachments / file send (Phase 3).
- Restoring hidden contacts via UI — the soft-delete bit lands
  in 2.E but the "show archived" view is deferred to Phase 3+.
  The data model stays correct so a future phase wires it up
  without migration.
