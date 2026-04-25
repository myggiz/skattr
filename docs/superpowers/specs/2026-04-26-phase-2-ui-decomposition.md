# Phase 2 Decomposition Spec

**Status:** draft, pending user review.
**Date:** 2026-04-26.
**Predecessor:** Phase 1 complete (1.H merged 2026-04-24).
**Complements:** `docs/skattr-implementation-plan.md` §Phase 2.
**Mirrors structure of:** `docs/superpowers/specs/2026-04-21-phase-1-decomposition.md`.

## Scope

Phase 2's plan-level goal: "Users message each other even when not both online. Non-technical users can install and use the app." This doc locks the seven sub-projects that deliver that goal — five UI lane (2.C–2.F + 2.G) and two mailbox lane (2.A–2.B).

The split is by track and module layer, not by user-visible feature slice. The full Phase 2 user-visible behaviour lands only when 2.G ships installers built on top of 2.B's mailbox wiring and 2.F's settings UI.

## Sub-projects

| ID  | Name                              | Lane     | Primary modules touched                                        | Depends on        | Exit criterion                                                                                                                            |
|-----|-----------------------------------|----------|----------------------------------------------------------------|-------------------|-------------------------------------------------------------------------------------------------------------------------------------------|
| 2.A | Mailbox server                    | mailbox  | `crates/mailbox/`, new `core::mailbox::protocol`               | 1.H               | Fuzz client soak 24h clean; protocol frozen; operator can run from documented setup in ≤ 30 min                                          |
| 2.B | Mailbox client + ContactCard rotation | mailbox  | `core::mailbox::{client, poll}`, `core::contact::card`, `core::delivery::{hub, outbox}`, new `mailboxes` table | 2.A               | Offline peer receives queued messages on reconnect; rotate-onion doesn't break conversations; mailbox failover works                     |
| 2.C | UI bootstrap (read-only conversation MVP) | ui       | new `crates/ui/` (Tauri 2 + SvelteKit), additive IPC to `daemon::commands`/`events` | 1.H               | Two paired CLI daemons + UI on machine A: contact list renders, open contact, history visible, live-append on `MessageReceived`           |
| 2.D | Conversation view (send + delivery + scroll-back) | ui       | `crates/ui/src-svelte/`, additive IPC                          | 2.C               | Round-trip messaging from UI to UI; delivery states visible; pagination smooth on 10k-row history; mark-read cursor advances             |
| 2.E | Invite & contact UX               | ui       | `crates/ui/src-svelte/`, additive IPC (`RenameContact`, `RemoveContact`) | 2.D               | Two non-technical testers complete invite → add → first-message via UI alone                                                              |
| 2.F | Settings & history                | ui       | `crates/ui/src-svelte/`, additive IPC (`GetConfig`/`SetConfig`/`ChangePassphrase`/mailbox CRUD wiring) | 2.B + 2.E         | All settings round-trip; mailbox CRUD wired against 2.B; passphrase change works without data loss; notifications respect focus + mute    |
| 2.G | Packaging & distribution          | infra    | `tauri.conf.json`, new `.github/workflows/release.yml`, `docs/install/` | 2.F               | Each platform installer runs first-run wizard on fresh VM; `SHA256SUMS.minisig` verifies; install instructions cover Linux/macOS/Windows  |

## Dependency DAG

```
                                                              ┌──► 2.G
mailbox lane:  1.H ──► 2.A ──► 2.B ───────────────► 2.F ──────┤
                                                              │
ui lane:       1.H ──► 2.C ──► 2.D ──► 2.E ──► 2.F ───────────┘
                          (stub mailbox surface in 2.C; real wiring lands in 2.F)
```

## Execution order

Two parallel tracks; UI lane blocks on 2.B only at 2.F.

- **mailbox lane:** 2.A → 2.B
- **ui lane:** 2.C → 2.D → 2.E → 2.F (2.F merge waits for 2.B)
- **release:** 2.G after 2.F

The lanes run in parallel where the team has capacity. With single-stream subagent-driven development, run them serially in the order: 2.A → 2.C → 2.D → 2.E → 2.B → 2.F → 2.G. (2.B slots in before 2.F so 2.F can finalise mailbox CRUD wiring.)

## Locked architectural decisions

Approved during this brainstorm; do not relitigate in sub-project brainstorming.

1. **Tauri 2 + SvelteKit, transport-agnostic JS adapter.** SvelteKit components import a typed `IpcClient` interface (`crates/ui/src-svelte/src/lib/ipc/client.ts`); the concrete transport (Tauri IPC today) sits behind that interface so a future mobile shell swaps the adapter rather than rewriting callsites. No SvelteKit code imports `@tauri-apps/api` directly.
2. **Daemon as single source of truth.** UI never opens SQLite directly. Daemon answers `ListContacts` / `RecentMessages` / `SearchMessages` / `DaemonInfo` during `TorStatus::Bootstrapping` (read paths do not gate on Tor readiness). UI shows a `TorStatus` pill, not a blocking overlay.
3. **JIT IPC evolution with wire-format contracts.** Each sub-phase's spec includes a `## Wire-format contract` section that lists every new `Command` variant, every new `CommandResult` variant, every new `Event` variant, and every additive field on existing types. Existing wire types are append-only across Phase 2 — no renames, no removed fields, no changed semantics on existing fields.
4. **Privacy-native distinctive design language.** One bundled libre font (recommendation: Inter, OFL 1.1, 2 weights ~80 KiB woff2). Dark-mode-first design tokens file (`tokens.css`) shipped in 2.C: 6 colors (`--bg`, `--bg-elevated`, `--text`, `--text-muted`, `--accent`, `--danger`), 4-step spacing scale, 3-step type scale. No external fonts/CDNs, no remote images, no analytics, no HTML rendering of message bodies.
5. **2.C bootstrap MVP = read-only conversation.** Send composer and invite/add UX deferred to 2.D and 2.E. 2.C's surface: app shell + first-run wizard + contact list + open one conversation + render `RecentMessages` + live-append `MessageReceived`.
6. **Single-process daemon model.** Tauri main process spawns `Daemon::run` in-process; the daemon keeps producing IPC over its Unix socket so the existing CLI can still attach. Closing the UI minimises to tray (added in 2.F); quit-from-tray is the only way to stop the daemon. "Attach to existing socket" mode for power users is Phase 5 territory.
7. **TS type generation via `ts-rs`.** Rust `daemon::commands` + `daemon::events` types emit TypeScript at build time; commit hook refuses commits with stale generated outputs.

## Cross-lane synchronisation

- 2.C ships a stub `Command::ListMailboxes` returning empty; 2.E surfaces the same stub in contact details; 2.F replaces both with real wiring once 2.B lands.
- 2.D's `DeliveryStatus::Deposited` variant is rendered in the UI from day one; the daemon emits it only after 2.B wires the mailbox-deposit fallback path.
- 2.B's protocol freeze gates only 2.F. Everything else is independent.

## Per-sub-project deliverables

Each sub-project produces:

1. A **design spec** at `docs/superpowers/specs/YYYY-MM-DD-phase-2<x>-<name>-design.md` (output of brainstorming).
2. An **implementation plan** at `docs/superpowers/plans/YYYY-MM-DD-phase-2<x>-<name>.md` (output of writing-plans).
3. A **merge to master** with: passing unit + integration tests on the sub-project's exit criterion, `cargo fmt --check` / `cargo clippy -D warnings` / `cargo test` green, `cargo deny check` clean, CHANGELOG bullet, CLAUDE.md status update.

## Sub-project sketches

These are seed material for each sub-project's own brainstorm — not the final design. Each sub-project's design spec is authoritative.

### 2.A — Mailbox server

**Crate split:** `crates/mailbox/` becomes a binary + library pair. `mailbox-server` binary depends on `mailbox` library so soak tests drive the library directly. Wire types live in a new `core::mailbox::protocol` (pub) module shared with 2.B.

**Storage:** plain SQLite (no `age` — the mailbox stores only ciphertext; operators need easy backup). New tables `deposits` and `challenges` per the implementation plan. No `mailbox.toml` schema changes once 2.A merges.

**Wire protocol** (CBOR over `core::transport`'s Noise_XK + frame codec; reuses existing transport rather than introducing a parallel one):

| Frame                | Body                                                          | Server response                                       |
|----------------------|---------------------------------------------------------------|-------------------------------------------------------|
| `DEPOSIT`            | `{ recipient_hash, ciphertext, ttl_request }`                 | `DEPOSIT_OK { deposit_id, expires_at }` or `DEPOSIT_REJECT { reason }` |
| `CHALLENGE`          | `{ recipient_pubkey }`                                        | `CHALLENGE_NONCE { nonce, expires_at }` (5 min TTL)   |
| `FETCH`              | `{ recipient_pubkey, signature_over_nonce }`                  | `FETCH_RESULT { deposits: Vec<{ id, ciphertext, deposited_at }> }` |
| `DELETE`             | `{ recipient_pubkey, signature_over("delete:" \|\| ids \|\| nonce), ids }` | `DELETE_OK { deleted: u32 }`                          |

**Operator-configurable caps** (defaults shown):
- Deposit max size: 1 MiB
- TTL: clamped to [1h, 30 days], default 7 days
- Per-recipient storage: 256 MiB (oldest expired first when over cap)
- Per-circuit deposits/min: 30; fetches/min: 6

**Operational artefacts:** `systemd/skattr-mailbox.service`, `Dockerfile` (distroless base), local-only metrics (no exporter), `docs/operations/mailbox-setup.md`.

**Test plan:** unit per handler, property-based round-trips, fuzz harness on the wire decoder, 24h soak (1k recipients × 100 deposits/hr), adversarial sweeps (replay-old-challenge, oversize, TTL underflow/overflow, concurrent delete races).

**Logging policy:** aggregate counters at `info`; per-request short-prefix-only at `debug`; never identity pubkeys, full hashes, or ciphertext anywhere.

### 2.B — Mailbox client + ContactCard rotation

**New core modules:**
- `core::mailbox::client` — `MailboxClient` over `core::transport::AuthenticatedConnection`. Methods: `register`, `deposit`, `fetch`, `delete`. Typed errors via `MailboxErrorKind`.
- `core::mailbox::poll` — `PollScheduler` actor. Per-mailbox state machine: `Idle (60s tick)` ↔ `Active (15s tick)`. Active triggered by recent send/receive on any contact whose card lists this mailbox. Jittered ±25%.
- `core::contact::card` extension — add `mailboxes: Vec<MailboxRef>` as an optional CBOR field on `ContactCard` (additive only — verifiers tolerate the absence to keep pre-2.B cards verifying). Sign/verify flow already covers any field shape over the canonical CBOR encoding (1.D); the new field is included in the signature for v2 cards. Bump `ContactCard.version` semantics so rotation publishes monotonically.

**Outbox extension:**
- New `outbox.target_kind` column (`'direct' | 'mailbox'`) and `outbox.mailbox_id` (FK to a new `mailboxes` table).
- `delivery::hub::DeliveryHub` adopts a fallback policy: try direct connection for N seconds (configurable, default 30s); on timeout enqueue mailbox-deposit attempts in parallel to each of the recipient's currently-known mailboxes.

**ContactCard rotation:**
- `Command::RotateOnion` (already reserved) wired up: generate fresh HS key, publish new card via deposits to all contacts' mailboxes, old onion stays listening for a configurable grace period (default 24h).
- New `Command::AddMailbox` / `Command::RemoveMailbox` register/unregister and trigger ContactCard publish.

**New events:**
- `Event::MailboxStatusChanged { mailbox_id, status: Reachable | Unreachable | Authenticated }`
- `Event::ContactCardReceived { contact, version }` (UI re-fetches contact summary)

**Test plan:** spawn-daemon-pair-plus-mailbox integration, onion-rotation pickup within one poll cycle, mailbox failover (one of two unreachable), `#[ignore]`-gated real-Tor coverage.

### 2.C — UI bootstrap

**`crates/ui/` layout:**
```
crates/ui/
  Cargo.toml                          # GPLv3 header on every .rs
  tauri.conf.json
  src/                                # Rust side
    main.rs
    commands.rs                       # one Tauri command per IPC operation
    daemon.rs                         # owns Daemon::run task
  src-svelte/                         # SvelteKit app
    package.json                      # zero remote-CDN deps; pnpm-locked
    src/
      lib/
        ipc/
          client.ts                   # IpcClient interface
          tauri.ts                    # TauriTransport implementation
          types.ts                    # ts-rs generated
        stores/                       # Svelte stores (read-only mirrors)
        components/                   # design-system components
        tokens.css                    # CSS variables
        fonts/                        # bundled libre font
      routes/
        +layout.svelte
        +page.svelte                  # contact list + conversation pane
        first-run/+page.svelte
```

**Wire-format contract for 2.C** (additive only):
- `ContactSummary` gains: `unread_count: u64`, `last_message_preview: Option<String>` (≤ 80 chars of last text body; empty for non-text), `last_ts_recv: Option<u64>`.
- `Subscribe` ack now replays the latest `Event::TorStatusChanged` on first dispatch.
- New `Command::DaemonInfo` → `CommandResult::DaemonInfo { local_pubkey: PublicKey, current_onion: String, daemon_version: String, schema_version: u32 }`.
- `Command::ListContacts` ordering: `last_ts_recv DESC NULLS LAST, added_at DESC`.
- New stub commands `Command::ListMailboxes` → `CommandResult::Mailboxes(Vec<MailboxSummary>)`, `Command::AddMailbox { onion: String }` → `CommandResult::Ok` (returns `DaemonErrorKind::Unsupported` until 2.B), `Command::RemoveMailbox { id: i64 }` → `CommandResult::Ok` (same). 2.C ships the wire shapes plus a `ListMailboxes` handler that returns an empty list; 2.B replaces handlers with real implementations. `MailboxSummary { id: i64, onion: String, status: MailboxStatus, registered_at: u64 }` is locked in 2.C and reused unchanged.

**Design tokens** (`tokens.css`, locked in 2.C, reused across 2.D–2.F):
- 6 colors: `--bg`, `--bg-elevated`, `--text`, `--text-muted`, `--accent`, `--danger`. Dark mode default; light mode overridden via `prefers-color-scheme`.
- 4-step spacing: `--s-1` (4px) → `--s-4` (32px), modular scale.
- 3-step type: `--t-body` (14px/1.5), `--t-ui` (13px/1.4), `--t-display` (20px/1.3).
- Bundled font (recommendation: Inter, OFL 1.1, regular + medium, ≈ 80 KiB woff2).

**First-run wizard** (`/first-run/`, 4 steps):
1. Welcome — what skattr does and explicitly does not protect against.
2. Passphrase create with strength meter (zxcvbn, bundled).
3. Seed phrase: show 12 words; user types them back to confirm; warn about loss.
4. Tor bootstrap with progress bar driven by `Event::TorStatusChanged`.

**Main shell:** left rail contact list (sorted by `last_ts_recv` desc; nickname + preview + unread badge), right pane conversation view (renders `RecentMessages` + live-append `MessageReceived`), top-right `TorStatus` pill. No composer in 2.C.

**Test plan:**
- Rust: `cargo test -p skattr-ui` for the Tauri command layer with mock daemon handle.
- TS: Vitest for IPC adapter; Playwright for first-run wizard happy path.
- Integration: spawn-daemon + Tauri fixture; complete first-run; verify `ListContacts` renders.

### 2.D — Conversation view

**Wire-format contract for 2.D:**
- `Command::SendMessage` reply extended: `CommandResult::MessageSent { message_id, status, record: MessageRecord }` so UI renders the sent bubble synchronously.
- `Command::RecentMessages` gains `before_id: Option<i64>` cursor; response gains `next_before_id: Option<i64>` for paged scroll-back.

**UI work:**
- Composer: textarea, Enter-to-send, Shift+Enter newline; disabled when daemon-down or contact-removed; paste-as-plaintext only.
- Message bubbles: own right-aligned with delivery state icon (clock → check → check-check → !); peer left-aligned. No avatars (Phase 3).
- Virtualised message list (svelte-virtual-list pinned in this sub-phase); pagination on scroll-to-top with skeleton rows; mark-read separator above first unread; cursor advances when bottom enters viewport.

**Test plan:** Playwright snapshot on a fixed-content conversation; pagination integration on a 10k-row seed; round-trip integration through paired UIs.

### 2.E — Invite & contact UX

**Wire-format contract for 2.E:**
- New `Command::RenameContact { contact, nickname: Option<String> }`.
- New `Command::RemoveContact { contact }` — soft-delete (mark hidden; MLS group preserved for replay safety).

**UI work:**
- Invite generate dialog: optional nickname, TTL slider, result page with URL + inline-rendered QR (via Tauri command using the `qrcode` Rust crate, returning SVG; no remote QR services) + copy-to-clipboard.
- Add-contact dialog: paste tab for `skattr://invite/v1#…`; scan tab using `getUserMedia` + bundled `jsqr` (MIT, ≈ 50 KiB). Webcam permission requested only when scan tab is opened.
- Contact details panel: pubkey short-hash with click-to-copy full, current onion, mailbox list, inline rename, remove with confirm dialog.

**Test plan:** two non-technical-tester walkthroughs (manual; matches the implementation plan exit criterion); Playwright for paste happy path; fixture-based scan path.

### 2.F — Settings & history

**Wire-format contract for 2.F:**
- New `Command::GetConfig` / `Command::SetConfig { patch: ConfigPatch }` covering `[history] retention_days`, `[delivery] direct_timeout_secs`, `[notifications] mode: full | minimal | generic`.
- `Command::ListMailboxes` / `Command::AddMailbox` / `Command::RemoveMailbox` — wire shapes locked in 2.C; 2.F replaces the stub handlers with real wiring against 2.B's `MailboxClient`. No wire change.
- New `Command::ChangePassphrase { old: String, new: String }` — re-encrypts the identity vault and the storage age key.

**UI work:** settings panel with sections — Identity (pubkey, onion, rotate, change passphrase), Mailboxes (list + add + remove), History (retention, search across all conversations using 1.G's `SearchMessages`, export to JSON/plaintext, prune controls), Notifications (mode, per-conversation mute via `notify-rust`; focus-aware), Advanced (logs viewer with redaction, version + audit info).

**Test plan:** Playwright per settings flow; `ChangePassphrase` integration (old passphrase invalidated, new opens vault, no data loss); manual cross-OS notification matrix.

### 2.G — Packaging & distribution

**Per-target deliverables:**

| Platform | Bundle             | Notes                                                                           |
|----------|--------------------|---------------------------------------------------------------------------------|
| Linux    | `.deb` + AppImage  | `tauri-bundle` produces both. Flatpak manifest committed; Flathub deferred.    |
| macOS    | `.dmg`             | Unsigned (Phase 5 adds Developer ID + notarisation). Gatekeeper warning documented. |
| Windows  | `.msi`             | Built via WiX. Unsigned (Phase 5 adds EV cert). SmartScreen warning documented.|

**CI release flow** (`.github/workflows/release.yml`, triggered on `v*` tags):
1. Matrix-build on `ubuntu-latest`, `macos-latest`, `windows-latest`.
2. Each job runs `cargo tauri build`, computes SHA-256 of bundles.
3. Final job collects bundles, generates `SHA256SUMS`, signs with `minisign` (key in repo secrets).
4. Uploads bundles + `SHA256SUMS` + `SHA256SUMS.minisig` to the GitHub Release.
5. Smoke test per platform: clean container/VM image installs the bundle and runs the binary headless via a new `--smoke-test` flag (completes first-run wizard via injected fixtures, exits 0). Failures fail the release.

**Reproducible-build groundwork** (lays groundwork for Phase 4's exit criterion):
- `Cargo.lock` committed and CI-enforced (already in place).
- Document `SOURCE_DATE_EPOCH=` derived from git commit timestamp in build instructions.
- Rust toolchain pinned via `rust-toolchain.toml`; Tauri version pinned in `Cargo.toml` + `package.json`.
- Phase 2 does not claim byte-identical reproducibility — only "inputs are pinned and the recipe is documented."

## Cross-cutting concerns

**Security review cadence** (CLAUDE.md mandate, restated):
- Every PR touching `core::transport`, `core::mls`, `core::identity`, `core::invite`, `core::mailbox::protocol` requires a second reviewer.
- Mailbox lane PRs loop in a reviewer with mailbox-protocol context for 2.A and 2.B.
- UI surfaces that handle passphrase, seed phrase, or QR scan require a UX-aware second reviewer.

**Observability** (CLAUDE.md mandate, restated):
- `tracing` Rust-side at all levels; UI logs forwarded via a Tauri command into the same subscriber.
- No identity pubkeys, onions, or message contents above `debug` level — Rust side or JS side.
- Production builds compile out `console.debug` in JS.
- No telemetry, analytics, or auto-submit crash reporting.

**IPC discipline** (locked decision 3, restated):
- Each sub-phase's spec includes a `## Wire-format contract` section.
- Existing wire types are append-only across Phase 2.
- `ts-rs`-generated TS committed alongside Rust changes; commit hook fails on drift.

**Testing pyramid** (matches implementation plan):
- Unit: every module on both Rust and TS sides.
- Property: mailbox protocol round-trips, ContactCard verify, IPC adapter framing.
- Fuzz: mailbox wire decoder; invite parser regression coverage from 1.D.
- Integration: spawn-daemon-pair tests in `crates/tests/`; spawn-daemon-plus-mailbox tests for 2.B.
- UI integration: Playwright in CI on Linux only (mac/Windows manual); Vitest for IPC adapter.
- Adversarial: malicious mailbox, malicious peer (replays old MLS), corrupted local state — each gets a regression test.

**Phase 2 exit checklist** (concatenation of sub-phase exits):
- [ ] Mailbox server soak runs 72 hours with no leaks or crashes (2.A).
- [ ] Offline user receives queued messages on reconnect; address rotation doesn't break conversations (2.B).
- [ ] Two non-technical testers complete install → invite → message via UI alone (2.C + 2.D + 2.E).
- [ ] All settings round-trip; mailbox CRUD wired (2.F).
- [ ] Each platform installer runs first-run wizard on a fresh VM; `SHA256SUMS.minisig` verifies (2.G).
- [ ] CHANGELOG entries per sub-phase, all committed.
- [ ] Mailbox operator can run a mailbox from documented setup in under 30 minutes (2.A).

## Risks and mitigations

| Risk                                                                    | Mitigation                                                                                                      |
|-------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------|
| Tauri 2 breaking changes mid-phase                                      | Pin Tauri version in `Cargo.toml` + `package.json`; upgrade only between sub-phase boundaries with a dedicated review |
| Mailbox protocol needs revision after 2.B reveals real-use issues       | 2.A's wire types are versioned (`MAILBOX_PROTOCOL_V1`); 2.B can ship a compat shim for 2.A's tail               |
| Webcam QR scan is a permission surprise on first use                    | Document in onboarding copy; webcam access requested only inside the add-contact dialog when scan tab is opened  |
| Bundling `notify-rust` per-OS quirks                                    | Each platform CI job exercises notification path in smoke test                                                  |
| `ts-rs`-generated types drift from hand-edited TS                       | Commit-hook runs the generator and refuses commits with stale outputs                                           |
| Single-process daemon model surprises CLI users when UI closes          | Closing the UI minimises to tray (added in 2.F); quit-from-tray is the only way to stop the daemon. Documented in operations guide |
| `MlsProvider`/storage churn during 2.B mailbox-deposit fallback         | Mailbox deposit and direct-send share `Group::save_in_tx` (1.H pattern); the new outbox `target_kind` column is additive |

## Out of scope for Phase 2

- Multi-member groups (Phase 3).
- Attachments / reactions / replies / edits / typing indicators (Phase 3).
- Code signing + notarisation (Phase 5; Phase 2 ships unsigned + minisign checksums per the implementation plan).
- Mobile shell (architecture leaves the door open via the IPC adapter; no implementation in Phase 2).
- Cover traffic, panic-wipe, duress mode (Phase 4).
- Auto-update mechanism (Phase 5).

## Open questions (deferred to per-sub-phase brainstorms, not blocking this decomposition)

- Bundled font choice — Inter is the default recommendation; alternatives (IBM Plex, Atkinson Hyperlegible, JetBrains Mono) defer to 2.C brainstorm.
- Mailbox-server hosting story — the implementation plan defers picking specific volunteer-run mailboxes; Phase 2 ships the server but does not publish a "use this mailbox" list. Defer to 2.A spec.
- Virtualised list library choice — recommendation `svelte-virtual-list`; alternatives defer to 2.D brainstorm.
- Daemon detach / attach-existing-socket support for power users — Phase 5 territory.

## What this doc does NOT cover

- **Sub-project internals.** Each sub-project's own design spec covers its architecture, data flow, error handling, testing strategy.
- **Phase 3+ work.** Multi-member groups, attachments, message kinds, audit, beta program — all out of scope.
- **Protocol-level changes to locked decisions in `CLAUDE.md`.** Any change requires a new ADR under `docs/adr/`.
