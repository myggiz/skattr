# Phase 2.C — UI bootstrap design

**Status:** draft, pending user review.
**Date:** 2026-05-01.
**Predecessor:** Phase 2.B merged 2026-05-01 (`7bf1789`).
**Parent:** `docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md` §"2.C — UI bootstrap".
**Implementation plan target:** `docs/superpowers/plans/2026-05-01-phase-2c-ui-bootstrap.md` (next step).

## Scope

Phase 2.C ships the read-only conversation MVP for Skattr: a new
`crates/ui/` Tauri 2 + SvelteKit crate that boots an in-process
`Daemon::run`, walks a first-run user through identity creation +
Tor bootstrap, and renders contacts + one open conversation with
live-append on `Event::MessageReceived`.

2.C is read-only by design. Composer (2.D), invite/add UX (2.E),
settings + mailbox CRUD UI (2.F), and packaging (2.G) are out of
scope and tracked in the umbrella decomposition.

## Locked architectural decisions inherited from the umbrella

These are binding from `2026-04-26-phase-2-ui-decomposition.md`. Not
re-litigated here.

1. Tauri 2 + SvelteKit with a transport-agnostic JS adapter
   (`crates/ui/src-svelte/src/lib/ipc/client.ts`); concrete
   `TauriTransport` behind it; no SvelteKit code imports
   `@tauri-apps/api` directly.
2. Daemon is the single source of truth — UI never opens SQLite.
3. Wire types are append-only across Phase 2.
4. Privacy-native: one bundled libre font, dark-mode-first tokens,
   no remote fonts/CDNs/images/analytics, no HTML rendering of
   message bodies.
5. 2.C is read-only — composer / invite / settings deferred.
6. Tauri main process spawns `Daemon::run` in-process; CLI keeps
   working over the existing Unix IPC socket.
7. Rust → TypeScript types generated via `ts-rs` at build time.

## Decisions locked in this brainstorm

These are binding for 2.C; downstream sub-phases inherit unless
revisited.

1. **Wizard-first first-run flow.** Tauri does not spawn `Daemon::run`
   until the wizard has produced a usable vault + passphrase. The UI
   exposes three pre-daemon Tauri commands (`vault_exists`,
   `identity_init`, `vault_unlock`); after the wizard completes,
   `daemon::start_in_process` brings the daemon up and the JS
   adapter switches to the IPC bridge. Phase 1.F's `Daemon::run` body
   is not modified.
2. **Mailbox wire surface inherited from 2.B unchanged.** 2.C does not
   declare or stub `Command::ListMailboxes` / `AddMailbox` /
   `RemoveMailbox`; it consumes them as already-real from 2.B. 2.C
   ships no mailbox UI — that lands in 2.F.
3. **Quit-on-close for 2.C only.** Closing the UI window calls
   `IpcRequest::Shutdown` and exits Tauri. This is 2.C-specific
   behaviour, replaced by hide-to-tray in 2.F. Documented in the
   2.C CHANGELOG entry so CLI users running both binaries are not
   surprised.
4. **`Subscribe` ack replays cached `TorStatusChanged` as a separate
   `Event` frame** immediately after `Ok(Subscribed)`, gated by the
   same `event_matches` predicate so only `EventFilter::All` and
   `EventFilter::TorStatus` see the replay. `CommandResult::Subscribed`
   stays a unit variant. Daemon caches latest `TorStatus` on
   `DaemonHandle::latest_tor_status` (RwLock-protected snapshot)
   updated by a tap task that subscribes to the broadcast channel
   at `Daemon::run` startup.
5. **`Command::DaemonInfo` shape:**
   `{ local_pubkey: PublicKey, current_onion: Option<String>,
   daemon_version: String, schema_version: u32 }`. `current_onion`
   is `None` until the onion is published; `daemon_version` comes
   from `env!("CARGO_PKG_VERSION")` of `skattr-core`; `schema_version`
   is read from the migrations table at startup and cached on
   `DaemonHandle`.
6. **`ContactSummary` projection extensions.** Three additive fields:
   - `unread_count: u64` — derived per contact via
     `MessageRepo::unread_count(group_id)`. N+1 query is acceptable
     for 2.C (typical contact list is short); benchmark + batched
     query revisited in 2.F if needed.
   - `last_message_preview: Option<String>` — extracted from the
     latest message's `envelope.kind`: for `Kind::Text { body }`,
     the body truncated to ≤80 Unicode code points (not bytes, not
     graphemes; grapheme-aware truncation is a 2.D refinement).
     `None` for every other `Kind` variant (including
     `ContactCardUpdate`).
   - `last_ts_recv: Option<u64>` — `MAX(ts_daemon_recv)` across both
     directions (incoming + outgoing) within the contact's group;
     `None` if zero messages. Field name is umbrella-locked even
     though it is semantically "last activity"; `ts_daemon_recv`
     is populated on every row regardless of direction since 1.H.
7. **`ListContacts` ordering** locked at
   `last_ts_recv DESC NULLS LAST, added_at DESC`.
8. **Live-append on `Event::MessageReceived`.** Append the one
   incoming `MessageRecord` to the rendered list; never re-issue
   `RecentMessages` on receive.
9. **Bundled font: Inter (OFL 1.1).** Regular + medium weights as
   woff2 (~80 KiB total). Shipped in
   `crates/ui/src-svelte/src/lib/fonts/`. Alternatives (IBM Plex,
   Atkinson Hyperlegible, JetBrains Mono) considered and declined.
10. **Virtualised list library: `svelte-virtual-list`** (MIT,
    ~3 KiB). Pinned in 2.C; 2.D inherits.
11. **Wizard step granularity: 4 steps,** welcome → passphrase →
    seed-phrase reveal+confirm → Tor bootstrap. Confirmation is
    **type-back** (user types the 12 words in order; case-insensitive,
    whitespace-tolerant). A "Skip confirmation (I've written it down)"
    escape is required for accessibility but only revealed behind a
    red-modal warning.
12. **Token palette and scale.** Locked values in
    `crates/ui/src-svelte/src/lib/tokens.css`:
    ```
    --bg: #0e0f12;        --bg-elevated: #16181d;
    --text: #e8eaed;      --text-muted: #9aa0a6;
    --accent: #7aa2f7;    --danger: #f7768e;

    --s-1: 4px;  --s-2: 8px;  --s-3: 16px;  --s-4: 32px;

    --t-body: 14px / 1.5;
    --t-ui: 13px / 1.4;
    --t-display: 20px / 1.3;
    ```
    Light override via `@media (prefers-color-scheme: light)`:
    `--bg: #fafafa; --bg-elevated: #ffffff; --text: #1a1d21;
    --text-muted: #5f6368;` (accent/danger unchanged — both pass
    4.5:1 contrast on light bg).
13. **`ts-rs` codegen output is `.gitignored`.** Drift prevention is
    "if `pnpm check` reports `types.ts` differs from a fresh
    `cargo build -p skattr-ui`, fail." No commit hook on the file
    itself.

## Wire-format contract

Every change here is additive against the 2.B-merged surface. No
existing variant or field is renamed, removed, or re-typed.

### New `Command` variants

- `Command::DaemonInfo` — no payload.

### New `CommandResult` variants

- `CommandResult::DaemonInfo {`
    `local_pubkey: PublicKey,`
    `current_onion: Option<String>,`
    `daemon_version: String,`
    `schema_version: u32`
  `}`

### New fields on existing wire types

`ContactSummary` (every new field carries `#[serde(default)]` so a
future phase can append further fields without breaking 2.C-era
clients reading newer daemons; serde-derive otherwise treats absent
fields as decode errors):
- `unread_count: u64` (default `0`)
- `last_message_preview: Option<String>` (default `None`)
- `last_ts_recv: Option<u64>` (default `None`)

### Behavioural additions (no wire shape change)

- IPC server emits one `IpcResponse::Event(TorStatusChanged(cached))`
  immediately after `Ok(Subscribed)` when filter is `All` or
  `TorStatus`. Cache field added to `DaemonHandle`.
- `Command::ListContacts` ordering locked at
  `last_ts_recv DESC NULLS LAST, added_at DESC`.

### Consumed unchanged from 2.B

`Command::AddMailbox`, `RemoveMailbox`, `ListMailboxes`,
`MailboxSummary`, `Event::MailboxStatusChanged`,
`Event::ContactCardReceived`, `EventFilter::Mailboxes`,
`EventFilter::Delivery`. 2.C does not declare or stub these. UI
rendering of mailbox state is 2.F.

### Storage surface

- New `MessageRepo` accessor: `latest_for_group(group_id: &[u8]) ->
  Result<Option<StoredMessage>>`. Single-row query
  `SELECT … FROM messages WHERE group_id = ?1 ORDER BY id DESC LIMIT 1`.
- No migration. No schema change.

## Architecture

### Crate layout

```
crates/ui/
  Cargo.toml                          # GPLv3 header on every .rs
  build.rs                            # ts-rs codegen → src-svelte/src/lib/ipc/types.ts
  tauri.conf.json
  src/                                # Rust shell
    main.rs                           # tauri::Builder, window config, app lifecycle
    bootstrap.rs                      # pre-daemon Tauri commands
    daemon.rs                         # start_in_process: Daemon::run + IpcClient
    ipc_bridge.rs                     # post-daemon Tauri commands (1 per IpcRequest)
    events.rs                         # Tauri event channel; daemon broadcast → window emit
  src-svelte/
    package.json                      # pnpm-locked; zero remote-CDN deps
    src/
      lib/
        ipc/
          client.ts                   # IpcClient interface
          tauri.ts                    # TauriTransport implementation
          types.ts                    # ts-rs generated; .gitignored
        stores/
          tor_status.ts
          contacts.ts
          conversation.ts
          daemon_info.ts
        components/
          ContactRow.svelte
          MessageBubble.svelte
          TorPill.svelte
          VirtualMessageList.svelte   # wraps svelte-virtual-list
        tokens.css
        fonts/
          inter-regular.woff2         # OFL 1.1
          inter-medium.woff2
          OFL.txt                     # license text
      routes/
        +layout.svelte
        +page.svelte                  # main shell (post-wizard)
        first-run/+page.svelte
```

### Two-phase Tauri command surface

**Pre-daemon commands** (always available, defined in
`bootstrap.rs`, restricted to three):

- `vault_exists() -> Result<bool>` — file-existence check on
  `${data_dir}/identity.vault`.
- `identity_init({ passphrase: String, mnemonic: Option<String> })
  -> Result<{ mnemonic: String }>` — when `mnemonic.is_none()`,
  generates a fresh BIP39 seed; when `mnemonic.is_some()`, treats
  the input as a restore. Either path creates the vault and
  returns the mnemonic for step 3 to render. Uses `Vault::create`
  directly. **2.C wizard only surfaces the create-new branch;**
  the restore branch is in the wire surface so 2.E/2.F can add a
  "Restore from seed" entry without an additional Tauri command.
- `vault_unlock({ passphrase: String }) -> Result<()>` — attempts
  `Vault::open` to verify passphrase before kicking the daemon.

**Post-daemon commands** (defined in `ipc_bridge.rs`):

- `ipc_request(req: IpcRequest) -> Result<IpcResponse>` — single
  generic Tauri command serving every command shape. Validates
  with the Tauri-managed `IpcClient` and returns the wire response
  verbatim. Reduces command surface to one annotation.
- `ipc_subscribe(filter: EventFilter, channel: tauri::Channel<Event>)
  -> Result<()>` — opens a long-lived `IpcClient::subscribe`
  connection; relays incoming `Event` frames to the Tauri channel
  for SvelteKit to consume.

**Lint guard:** `bootstrap.rs` is restricted to three
`#[tauri::command]` annotations. A unit test counts annotations and
fails the build if a fourth lands.

### `daemon::start_in_process` lifecycle

```
1. Read data_dir from Tauri config (default: app_data_dir()/skattr).
2. Construct Config::defaults() with data_dir + ipc_socket pinned
   to a UI-namespaced path (${data_dir}/ipc.sock — same as the
   CLI; the daemon's IPC socket is shared).
3. Build (ready_tx, ready_rx) and (shutdown_tx, shutdown_rx).
4. tokio::spawn Daemon::run(&data_dir, &passphrase, config,
                            ready_tx, shutdown_fut).
5. Await ready_rx with a 180s timeout (matches Phase 1.F test).
6. Open IpcClient::connect(ready.ipc_socket).
7. Store {handle: JoinHandle, client: IpcClient, shutdown_tx,
         ready: Ready} in tauri::State.
```

The `passphrase` arrives at the Tauri command as a `String` and is
wrapped in `zeroize::Zeroizing` before being moved into the
spawned task. It is zeroized on drop after `Daemon::run` returns
— same lifecycle as the CLI.

### `Daemon` cache extension for `Subscribe` replay

Locked decision 4 requires the IPC server to replay the latest
`TorStatusChanged`. Implementation:

1. Add `latest_tor_status: parking_lot::RwLock<Option<TorStatus>>`
   to `DaemonHandle`.
2. At `Daemon::run` startup, after the broadcast channel is
   created, spawn a tap task that subscribes to `events_rx`,
   filters for `Event::TorStatusChanged(s)`, and writes `Some(s)`
   into the cache.
3. In `ipc::server::handle_connection`, after writing
   `Ok(Subscribed)`, read the cache and (if `Some(status)` and the
   filter passes `event_matches`) write one
   `IpcResponse::Event(TorStatusChanged(status))` frame.

The cache read is non-blocking (RwLock read guard). The tap task
adds one extra subscriber; broadcast capacity (1024) is unchanged.

## First-run flow

```
SvelteKit boot → vault_exists()
  ├─ false → /first-run
  │    Step 1: welcome (info + threat-model link)
  │    Step 2: passphrase create (zxcvbn ≥3); calls identity_init();
  │             receives mnemonic for step 3.
  │    Step 3: seed-phrase reveal + type-back confirm
  │             (case-insensitive, whitespace-tolerant; "Skip
  │             confirmation" escape behind red-modal warning).
  │    Step 4: kicks daemon::start_in_process; subscribes
  │             EventFilter::TorStatus; renders progress bar driven
  │             by TorStatusChanged replay + live events.
  │    On TorStatus::Ready → navigate to /
  └─ true → unlock screen
       Calls vault_unlock(); on success kicks
       daemon::start_in_process; on Subscribe ack the cached
       TorStatus paints the pill, ListContacts fetches the contact
       list.
```

## Main shell (post-wizard)

- **Left rail — contact list.** One `ContactRow` per
  `ContactSummary`. Renders nickname (or `pubkey[..8]` short-hash
  if `nickname.is_none()`), `last_message_preview`, `last_ts_recv`
  formatted relative ("3m", "2h", "yesterday", "Apr 28"), unread
  badge if `unread_count > 0`. Selecting a row sets the
  `conversation` store's active contact and issues
  `RecentMessages { contact: Some(active), limit: 200 }`.
- **Right pane — open conversation.** Renders the active contact's
  most recent 200 messages via `VirtualMessageList`. Live-appends
  one `MessageRecord` per `Event::MessageReceived` whose `contact`
  matches the active selection. Outgoing rows from this user
  (Phase 2.D will hydrate them from the send path) are absent in
  2.C since there is no composer.
- **Top-right — `TorPill`.** Renders `TorStatus`:
  - `Ready` → green dot, label "Tor connected".
  - `Bootstrapping(pct)` → spinner + "Connecting (NN%)".
  - `Failed(msg)` → red dot, click for details.
  - `Idle` → grey dot, "Disconnected".
- **No composer, no invite UX, no settings, no tray.**

## Test plan

### Rust (`cargo test -p skattr-ui`)

- `bootstrap` unit tests: `vault_exists` true/false; `identity_init`
  creates a vault file with the passphrase; `vault_unlock` rejects
  wrong passphrase.
- `daemon::start_in_process` test (mock `Daemon::run` for fast
  path): `Ready` arrives → `IpcClient` round-trips
  `Command::DaemonInfo`. Real-`Daemon::run` integration test
  `#[ignore]`-gated (matches 1.F precedent).
- `ipc_bridge` round-trip: every Tauri command carries the same
  CBOR payload as the equivalent direct `IpcClient` call.
- Lint: `bootstrap.rs` exposes ≤ 3 `#[tauri::command]` annotations.

### TypeScript

- Vitest: `IpcClient` interface contract + `TauriTransport` framing
  (mock `@tauri-apps/api/core::invoke`).
- Vitest snapshot: `tokens.css` exact bytes (regression guard).
- Playwright (headed, Linux CI): first-run wizard happy path
  (welcome → passphrase → seed type-back → Tor pill green → empty
  contact list).
- Playwright: existing-vault unlock path.
- Playwright: type-back rejects wrong word; "Skip" escape requires
  explicit modal confirmation.

### Integration (`crates/tests/`)

- New `ui_first_run.rs`: spawns a Tauri-app fixture + real
  `Daemon::run` over an isolated `data_dir`; drives wizard via
  Tauri's `invoke` API; asserts `ListContacts` returns `[]`.
  `#[ignore]`-gated (real Tor bootstrap).

## Risks and mitigations

| Risk | Mitigation |
|------|------------|
| `ts-rs` codegen missing on first `cargo build` breaks `pnpm dev` | `pnpm dev` precheck verifies `types.ts` exists; prints actionable error pointing to `cargo build -p skattr-ui` |
| Tauri 2 IPC channel back-pressure under event burst | `EVENT_CHANNEL_CAPACITY=1024` matches `Daemon::run`; lagged subscribers reconcile via full `ListContacts` refresh |
| Quit-on-close (2.C-only) confuses CLI users running the UI alongside | Documented in 2.C CHANGELOG; 2.F upgrades to hide-to-tray |
| Two paths into Tauri commands (pre/post daemon) accumulate inconsistencies | `bootstrap.rs` restricted to three commands; lint test counts `#[tauri::command]` annotations |
| `unread_count` N+1 query on huge contact lists | Benchmark deferred; 2.C contact list is short; revisit in 2.F if needed |
| Wizard's seed type-back too rigid for accessibility | "Skip confirmation" escape with red-modal copy + `tracing` log; never disabled |
| Tap task lag drops the latest `TorStatus` snapshot before a Subscribe lands | Tap uses bounded `broadcast::Receiver`; if it lags, the cache simply stays at the previous snapshot — UI re-asks via the next `TorStatusChanged` live event |
| `latest_for_group` SQL plan picks a wrong index on huge groups | Existing `messages` rows are indexed by `(group_id, id)` from migration 0001; query is `LIMIT 1` so cost is constant |

## Out of scope for 2.C

- Composer / send button (2.D).
- Invite link generation, QR scan/render, add-contact dialog (2.E).
- Settings panel, mailbox CRUD UI, notifications, tray, hide-on-close (2.F).
- Packaging / installers (2.G).
- Phase 2.B follow-ups (Tasks 20.5, 22.5, 23.5 — independent,
  tracked in `CLAUDE.md`).
- Multi-member groups (Phase 3).
- Wire-format BREAKING changes — anything renaming or removing an
  existing `Command` / `CommandResult` / `Event` variant requires
  a separate spec.
- "Restore from seed phrase" wizard branch — wire surface ready
  via `identity_init`'s `mnemonic` arg; the SvelteKit wizard does
  not surface a restore entry until 2.E or 2.F.

## Exit criterion

- `cargo build` / `cargo clippy -D warnings` / `cargo test` /
  `cargo fmt --check` / `cargo deny check` all green across the
  workspace.
- `pnpm test` (Vitest) green; `pnpm playwright test` (headed on
  Linux) green for first-run + unlock paths.
- Two paired CLI daemons + UI on machine A: contact list renders;
  open one contact; history visible; a new `Event::MessageReceived`
  appears in the right pane without a refetch.
- CLI continues to work over the same `${data_dir}/ipc.sock` while
  the UI is open.
- `CHANGELOG.md` entry; `CLAUDE.md` "Repository state" updated to
  reflect 2.C.
