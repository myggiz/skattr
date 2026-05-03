Phase 2.E (invite & contact UX) just merged at master `d4c9bc7`
(`Merge branch 'phase-2e-invite-contact-ux' — Phase 2.E invite &
contact UX`). Welcome propagation now works end-to-end (Bob's
AddContact emits `Frame::MlsWelcome` to Alice's hub; Alice's
`DaemonInbound::dispatch_welcome` looks up the PSK from the new
`outstanding_invites` table, restores the MlsProvider snapshot, calls
`Group::join_from_welcome`, and transitions her group from
`PendingJoin` → `Active`). Invite-generate, add-contact (paste +
scan), and the inline ContactDetailsPanel with rename/archive ship
under three new `Command` variants — `RenameContact`, `RemoveContact`,
`ListContactsWithFilter` — all additive. Migrations 0010/0011/0012
land `outstanding_invites`, `contacts.hidden`, and
`outstanding_invites.provider_snapshot` respectively.

Phase 2.F (settings & history) is the next workstream — the umbrella
spec at `docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md`
§"2.F — Settings & history" has the authoritative scope:
`Command::GetConfig` / `SetConfig`, real `ListMailboxes` /
`AddMailbox` / `RemoveMailbox` wiring against 2.B's MailboxClient,
`Command::ChangePassphrase`, settings panel UI (Identity /
Mailboxes / History / Notifications / Advanced), notification system,
tray + minimize-to-tray, and the cross-conversation search UX
surfacing 1.G's `SearchMessages`. 2.F merge waits for 2.B (already
complete on master).

**Carry-forward limitations to address in 2.F or beyond:**

- **Task 2.E.5** (mailbox fallback for Welcome) — direct-only Welcome
  ships in 2.E; mailbox fallback is deferred because it would touch
  the 2.B mailbox protocol freeze (ADR 0006). Independent follow-up;
  not required for 2.F's exit criterion but a candidate sub-task if
  scope allows.
- `ContactSummary.peer_mailboxes` projection — 2.E's
  `ContactDetailsPanel` shows "No mailboxes" placeholder; 2.F should
  add the additive `peer_mailboxes: Vec<String>` field on
  `ContactSummary` and render it from the latest `ContactCard`. The
  data is already on `contact.card.body.mailboxes` — just needs to
  be projected onto the wire type and rendered.
- `Command::ChangePassphrase` requires re-encrypting BOTH the identity
  vault (BIP39-seed-derived Argon2id + XChaCha20-Poly1305 envelope)
  AND the storage age key. Non-trivial — design the
  re-key + atomic-commit + rollback semantics carefully. A failed
  re-key must not corrupt the vault.
- Notification system: cross-platform via `notify-rust` per the
  umbrella; focus-aware (no notification when window has focus),
  per-conversation mute, modes (full / minimal / generic). The
  per-OS notification matrix needs explicit testing in the
  smoke pass.
- The `set_identity` wiring on `DaemonInbound` was added inside the
  daemon-state initialiser in 2.E; 2.F's `ChangePassphrase` path
  will need to refresh it after vault re-unlock so newly received
  Welcomes still process under the (unchanged) identity key.

**Locked decisions from Phase 2 umbrella (do not relitigate):**

1. Settings UI ships as a SvelteKit route (`routes/settings/`),
   not a modal. Five sections per the umbrella: Identity / Mailboxes
   / History / Notifications / Advanced. Section navigation TBD in
   2.F brainstorm.
2. Wire-format additive rule still binds — every `Command::SetConfig`
   field, every `ChangePassphrase` argument lands as a new field on
   an existing variant or a new variant alongside, never a reshape.
   The `wire_format_append_only` lint test catches breaking changes
   at compile time; update it in the same commit as any new variant.
3. Mailbox CRUD wire surface (`AddMailbox` / `RemoveMailbox` /
   `ListMailboxes`) is locked from 2.C; 2.F's job is to replace the
   stub handlers with real wiring against 2.B's `MailboxClient` —
   no wire change.
4. No remote fonts / images / analytics. Local-only assets. Existing
   `lint_no_remote_assets.test.ts` continues to enforce.
5. Phase 2.F merge waits on 2.B — already satisfied on master.
6. Tray + hide-to-tray replaces the current quit-on-close behaviour
   (which 2.C explicitly noted as a 2.F item).

**Topics worth pinning down in the 2.F brainstorm** (the umbrella
deferred these):

- **Settings panel layout.** Single long scrollable pane (Linear
  pattern) vs left-sidebar nav (Slack/Discord). For five sections
  with relatively few fields each, scroll-pane is simpler; sidebar
  nav scales better if Phase 3+ adds more sections.
- **Notification mode UX wording** + per-OS implementation. `full` =
  sender + body preview; `minimal` = sender only; `generic` = "New
  message". Confirm vocabulary; how does the per-conversation mute
  interact with the global mode?
- **Tray library choice.** Tauri 2 has built-in tray support
  (`tray_icon::TrayIconBuilder`); use that vs the lower-level
  `tao::tray_icon` (more control, more code). Recommendation: stick
  with Tauri 2's built-in.
- **Search UX from settings.** Global cross-conversation search via
  1.G's `SearchMessages`, presented under "History → Search". UX:
  results list with sender + snippet + timestamp; click → jump to
  the conversation at that message. Does the existing read-cursor
  semantics survive the jump?
- **ChangePassphrase flow safety.** Re-key both vault + storage age
  key. Atomic — both succeed, or rollback. UX: confirm-old-passphrase
  → enter-new-passphrase (with strength meter) → confirm-new →
  daemon re-keys → success or rollback. Probably wants a "verify
  current passphrase" gate before any change to avoid accidental
  lockout.
- **History retention UX.** Slider 7d / 30d / 90d / 1y / Never (the
  default `[history] retention_days = 0` already means infinite).
  Confirm presets. The existing daemon retention sweep already
  honours this; UI just edits the config.
- **Export to JSON/plaintext.** 1.G's `ExportHistory` already
  paginates messages. Settings UI surfaces a "Download" button per
  conversation + "Download all". Filename convention? Encoding?
  (Recommend: JSONL one-message-per-line, gzip-optional.)
- **Logs viewer.** "Advanced" section needs a logs viewer with
  redaction (no pubkeys / onions / message bodies above `debug`).
  Source: tracing subscriber → ring buffer → IPC stream? Or
  filesystem-tail of the rotated log file?
- **Test plan additions.** Vitest specs per settings flow.
  Playwright per major flow (passphrase change, mailbox add/remove,
  retention slider, notification toggle). Manual test plan for the
  per-OS notification matrix. Rust integration test for
  ChangePassphrase atomicity (incl. forced rollback).

## Context

- `docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md` §
  "2.F — Settings & history" — sketch + locked decisions. Read first.
- `docs/superpowers/specs/2026-05-03-phase-2e-invite-contact-ux-design.md`
  — what 2.E shipped; 2.F inherits the IPC adapter, stores, dialogs,
  components, tokens, the toast store, and the `ConfirmDialog`
  pattern.
- `docs/skattr-implementation-plan.md` Phase 2 §Workstream 2.F —
  the original detailed task list.
- `docs/skattr-design.md` — config schema, search semantics, retention
  policy.
- `crates/core/src/daemon/config.rs` — existing config struct that
  `GetConfig` / `SetConfig` will project onto the wire.
- `crates/core/src/daemon/retention.rs` — already running hourly;
  `[history] retention_days` already honoured.
- `crates/core/src/storage/messages.rs::{search, prune_before, prune_keep_last,
  export_page}` — already in place from 1.G.
- `crates/core/src/mailbox/client.rs` — real MailboxClient from 2.B
  ready to wire under `AddMailbox` / `RemoveMailbox` / `ListMailboxes`.
- `crates/ui/src-svelte/src/lib/test/tauri-mock.ts` — fixture pattern
  from 2.C/D/E; reuse + extend with new fixtures (e.g.
  `?fixture=settings-flow`).
- CLAUDE.md locked decisions remain binding. `crates/ui/` ships
  GPLv3.

## Locked from the 2.E merge (do not relitigate)

- Wire-format append-only rule — every protocol change must extend
  existing variants with `#[serde(default)]` fields or add new
  variants. The `wire_format_append_only` snapshot test
  enforces this; update the static lists alongside any addition.
- ts-rs emits `Hex16`, `PublicKey`, `MessageId` as bare lowercase hex
  `string` (NOT tuple-struct objects). Continue using `===` on hex
  strings in TS.
- `Command::ListContactsWithFilter { include_hidden: bool }` is the
  shape for "include archived contacts" — 2.F's "Show archived" UI
  in Settings → Contacts (if scoped in) consumes this.
- `ContactCard.body.mailboxes` exists already; 2.F's projection onto
  `ContactSummary.peer_mailboxes` is strictly additive.
- `MlsProvider::snapshot` / `MlsProvider::load` are the canonical
  serialise / restore for the OpenMLS keystore — 2.E used them for
  outstanding_invites; if 2.F's ChangePassphrase needs to re-key
  per-group MLS state, the same pattern applies.
- `DaemonInbound::set_identity(Arc<IdentityKey>)` exists; refresh it
  in `ChangePassphrase` after the vault re-keys.
- Tauri-mock fixture pattern (`?fixture=…` query param in
  `tauri-mock.ts`) is the way to seed e2e state. Add fixture
  branches for each settings flow.

## After brainstorming

- `superpowers:writing-plans` to author the implementation plan.
- `superpowers:using-git-worktrees` to branch off master onto a
  `phase-2f-settings-history` branch.
- `superpowers:test-driven-development` +
  `superpowers:subagent-driven-development` to execute.
- `superpowers:verification-before-completion` before the merge.

## Out of scope for 2.F

- Packaging / installers (2.G).
- Multi-member groups (Phase 3).
- Phase 2.B follow-ups: Task 20.5 (peer direct-timeout trigger),
  Task 22.5 (RemoveMailbox drain dispatch), Task 23.5 (real HS key
  rotation). Independent and tracked in CLAUDE.md.
- Task 2.E.5 (mailbox fallback for Welcome) — independent follow-up;
  may slot into 2.F if scope allows but not required for the 2.F
  exit criterion.
- Wire-format BREAKING changes — anything renaming or removing a
  Command / CommandResult / Event variant requires a separate spec.
- Avatars / reactions / replies / edits / typing indicators
  (Phase 3).
- Attachments / file send (Phase 3).
- Cover traffic / panic-wipe / duress mode (Phase 4).
- Auto-update mechanism (Phase 5).
- Code signing + notarisation (Phase 5).
- "Restore archived contact" UI — 2.E lands the data model; 2.F may
  surface this in Settings → Contacts → Archived if scope allows.
