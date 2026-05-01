# Phase 2.C — UI bootstrap (read-only conversation MVP) kickoff prompt

> **Usage:** Paste the fenced block below as the first message of a
> fresh Claude Code session. Keep the surrounding meta-text out of
> the paste — only the fenced block is the prompt itself.

---

```
Phase 2.B just merged at master `7bf1789`. The mailbox client +
ContactCard rotation are live: `core::mailbox::{client, codec, poll,
auth}`, `DeliveryHub::ensure_mailbox_fallback`, `Command::AddMailbox`/
`RemoveMailbox`/`RotateOnion`/`ListMailboxes`, two new events
(`MailboxStatusChanged`, `ContactCardReceived`) and two new filters
(`Mailboxes`, `Delivery`). 34 commits, all checks green.

Three TODOs were deferred from 2.B (do NOT block on these for 2.C —
they're independent): **Task 20.5** (PeerConnection direct-timeout
trigger to fallback orchestrator), **Task 22.5** (RemoveMailbox
final-drain through DaemonInbound), **Task 23.5** (real HS key
rotation in `Command::RotateOnion`; today bumps version + republishes
current onion).

Phase 2.C implements the UI bootstrap: a read-only conversation MVP
in a new `crates/ui/` crate built on **Tauri 2 + SvelteKit**. App
shell, first-run wizard (welcome → passphrase → seed phrase → Tor
bootstrap), contact list rendered from `Command::ListContacts`, one
open conversation rendered from `Command::RecentMessages`, and live-
append on `Event::MessageReceived`. **No composer, no invite UX, no
settings panel** — those land in 2.D / 2.E / 2.F respectively.

The umbrella decomposition (`docs/superpowers/specs/2026-04-26-
phase-2-ui-decomposition.md`) §"2.C — UI bootstrap" has the
authoritative scope. The locked architectural decisions there are
binding — do not relitigate them in the brainstorm:

1. Tauri 2 + SvelteKit with a transport-agnostic JS adapter
   (`crates/ui/src-svelte/src/lib/ipc/client.ts` interface; concrete
   `TauriTransport` behind it; no SvelteKit code imports
   `@tauri-apps/api` directly).
2. Daemon is the single source of truth — UI never opens SQLite.
3. IPC wire types are append-only across Phase 2 (additive only).
4. Privacy-native design: one bundled libre font (Inter is the
   default recommendation), dark-mode-first tokens, no remote
   fonts/CDNs/images/analytics, no HTML-rendered message bodies.
5. 2.C ships read-only — composer is 2.D, invite is 2.E,
   settings/mailbox-CRUD wiring is 2.F.
6. Tauri main process spawns `Daemon::run` in-process; CLI keeps
   working over the existing IPC socket.
7. Rust → TypeScript types generated via `ts-rs` at build time;
   commit hook refuses commits with stale generated outputs.

Please start by invoking `superpowers:brainstorming` to refine 2.C's
internals. Topics worth pinning down (these are the open questions
the umbrella spec deferred to per-sub-phase brainstorming):

- **Bundled font choice.** Inter is the working default (OFL 1.1,
  regular + medium, ≈80 KiB woff2). Alternatives: IBM Plex Sans /
  Atkinson Hyperlegible / JetBrains Mono. Trade-offs: Inter is
  battle-tested, Atkinson is accessibility-first, Plex carries
  brand neutrality, JBM reads as a dev tool.
- **Virtualised message list.** `svelte-virtual-list` is the working
  recommendation. Alternatives: hand-rolled `IntersectionObserver`
  pagination; `@virtual-scroll/core`. Decision criterion: handles
  10k-row history smoothly, no remote deps, MIT/BSD-compatible.
- **First-run wizard step granularity.** Four steps per umbrella:
  welcome / passphrase / seed phrase / Tor bootstrap. Should
  passphrase + seed phrase be separate screens or combined? What
  does the seed-phrase confirmation UX look like (type-back vs.
  click-to-confirm)?
- **`tokens.css` exact values.** 6 colors (`--bg`, `--bg-elevated`,
  `--text`, `--text-muted`, `--accent`, `--danger`), 4-step spacing
  (`--s-1` 4px → `--s-4` 32px), 3-step type (`--t-body`, `--t-ui`,
  `--t-display`). Lock the hex values + spacing scale.
- **`Command::DaemonInfo` shape.** Umbrella says `{ local_pubkey,
  current_onion, daemon_version, schema_version }`. Confirm this
  is the minimum the UI needs at first paint.
- **`ContactSummary` projection extensions.** Add `unread_count: u64`,
  `last_message_preview: Option<String>` (≤80 chars text body),
  `last_ts_recv: Option<u64>`. Confirm field types + nullability.
- **`Command::ListContacts` ordering.** Locked at `last_ts_recv DESC
  NULLS LAST, added_at DESC` per umbrella. Confirm + write the test.
- **Live-append vs full re-fetch on `MessageReceived`.** Append the
  one new record to the rendered list (cheap, no flicker) vs.
  re-issuing `RecentMessages` (simple, slow on long histories). The
  former is the obvious choice; the brainstorm should lock it.
- **`Subscribe` ack replay.** Umbrella says the ack now replays the
  latest `TorStatusChanged` so the UI can paint the bootstrap pill
  on first connect. Confirm wire shape + tests.
- **Stub mailbox commands — replace immediately or keep stubbed?**
  The umbrella anticipated `Command::ListMailboxes`/`AddMailbox`/
  `RemoveMailbox` would be 2.C stubs that 2.F replaces. **2.B
  already landed real handlers** for all three, so the stubs are
  unnecessary — 2.C's UI can render real mailbox data from day one.
  Confirm the spec absorbs this: the 2.C settings panel for
  Mailboxes is still 2.F territory, but the wire surface is already
  done.
- **`Daemon::run` in-process integration.** Tauri's main process
  spawns a `Daemon::run` task, holds the resulting IPC socket path,
  and the JS adapter dials it. The existing CLI keeps working
  because the daemon's IPC socket is unchanged. Pin down: where
  does the data_dir / passphrase come from on first run?
  (Answer: the wizard's passphrase step; the daemon doesn't start
  until the wizard completes.)
- **Tray / window-close behaviour.** Umbrella says minimise-to-tray
  is added in 2.F. For 2.C, what does close-button do — quit the
  daemon, or just hide the window? Pick one and document it as the
  2.C-only behaviour to be revisited in 2.F.
- **Test plan layers.** Rust: `cargo test -p skattr-ui` for the
  Tauri command layer with a mock daemon handle. TS: Vitest for the
  IPC adapter + Playwright for first-run wizard happy path.
  Integration: spawn-daemon + Tauri fixture; complete first-run;
  verify `ListContacts` renders.

## Context

- `docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md` §
  "2.C — UI bootstrap" — sketch + locked decisions. Read first.
- `docs/skattr-implementation-plan.md` Phase 2 §Workstream 2.C — the
  original detailed task list.
- `docs/superpowers/specs/2026-04-30-phase-2b-mailbox-client-design.md`
  — what 2.B shipped; the wire-format contract section names every
  Command/Event variant 2.B added that 2.C may surface.
- `crates/core/src/daemon/{commands,events}.rs` — current
  `Command`/`CommandResult`/`Event` surface; 2.C's additions are
  append-only on top.
- `crates/core/src/daemon/ipc/wire.rs` — `EventFilter` + framing.
- `crates/core/src/daemon/ipc/server.rs` — IPC server entry point;
  the Tauri main process invokes the same shape from in-process.
- `crates/cli/src/main.rs` — reference `IpcClient` consumer; the
  TS adapter follows the same conceptual contract over Tauri IPC.
- CLAUDE.md locked decisions remain binding. `crates/ui/` ships
  GPLv3 (matches `core`/`cli`/`tests`).

## Locked from the 2.B merge (do not relitigate)

- v1 mailbox wire surface is frozen — every protocol change
  requires v2.
- `EventFilter` evolution is append-only — adding new variants OK,
  changing semantics of existing ones requires v2.
- `Command::ListMailboxes` returns real rows now (2.C does not need
  the planned stub).
- `Event::MailboxStatusChanged` and `Event::ContactCardReceived`
  exist; UI may consume them but rendering them is 2.F scope.

## After brainstorming

- `superpowers:writing-plans` to author the implementation plan.
- `superpowers:using-git-worktrees` to branch off master onto a
  `phase-2c-ui-bootstrap` branch.
- `superpowers:test-driven-development` +
  `superpowers:subagent-driven-development` to execute.
- `superpowers:verification-before-completion` before the merge PR.

## Out of scope for 2.C

- Composer / send button (2.D).
- Invite link generation + scan / paste UX (2.E).
- Settings panel (2.F).
- Mailbox CRUD UI (2.F — wire surface from 2.B is already in place).
- Notification system (2.F).
- Tray + minimize-to-tray (2.F).
- Packaging / installers (2.G).
- Multi-member groups (Phase 3).
- Phase 2.B follow-ups: Task 20.5 (peer direct-timeout trigger),
  Task 22.5 (RemoveMailbox drain dispatch), Task 23.5 (real HS key
  rotation). These are independent and tracked in CLAUDE.md.
- Wire-format BREAKING changes — anything that renames or removes a
  Command / CommandResult / Event variant requires a separate spec.
```
