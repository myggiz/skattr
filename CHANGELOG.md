# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased] — targeting v0.1.17

_In-progress patch line. The version is bumped per build for tracking; entries
accumulate here and are stamped with a date when a build is cut._

### Fixed

- **`skattr-ui --version` opened the app instead of printing a version**
  (#161): there was no way to tell which build a copy of the desktop app
  was, short of launching it and digging through settings — and a script
  checking the version would hang on a window rather than answer. It now
  prints the version and exits, like the command-line tool already did.
- **A failed file save could leave a readable copy behind** (#156, #52):
  attachments are normally kept encrypted at rest and only decrypted on
  demand, but if writing the decrypted file out failed part-way through — a
  full disk, for example — the partial, readable copy could be left sitting
  next to where you asked it to go, instead of being cleaned up. If saving
  fails, the partial decrypted copy is now always removed. Opening or saving
  the same file twice at once (a double-click, say) no longer makes the two
  attempts interfere with each other.
- **Received files were invisible from the CLI — now you can see and save
  them** (#118): `skattr tail`/`export` showed a file message as an unreadable
  dump of numbers instead of a filename, so there was no way to tell what
  arrived, how big it was, or whether it had fully downloaded — and no way to
  get the file out of the app at all from the CLI. `tail`/`export` now show
  the filename, size, and whether it has fully arrived, and a new `skattr
  save-attachment <id> <path>` command saves a received file to a path you
  choose.
- **A file transfer that stalls no longer hangs around invisibly forever**
  (#149): if the sender vanished part-way through sending you a file, the
  transfer sat silently unfinished — it never appeared as failed, so there was
  nothing to retry, and the partial data stayed on disk unnoticed. After two
  weeks without progress it is now marked failed, which is what makes the
  **Retry** button appear; retrying picks up from the pieces already received
  rather than starting over. Leftover pieces from transfers the app no longer
  has any record of are cleaned up automatically. Two deliberate limits: a
  received file's data is never touched (it *is* your file — kept encrypted
  until you open or save it), and the pieces from a failed transfer are kept
  too, because they are what makes a retry cheap.
- **The app could refuse to start on a slower or busy machine, unable to open
  its own encrypted data** (#121): the identity key, message database, and
  backups are encrypted at rest with `age`, which was left to auto-pick how much
  work decryption should cost based on how fast the machine was when the file was
  *written*. Opening the same file later on a slower — or merely busier — machine
  could then be rejected as "too much work" (observed: _"Decryption would take
  around 64 seconds"_), locking the user out of their own identity, history, and
  backups. Encryption now uses a fixed, fast work factor and decryption accepts a
  fixed ceiling, so a file that was written always opens regardless of machine
  speed or load. Existing data dirs keep working unchanged — no re-key, same
  `.onion` address.

- **A file you send no longer says "Delivered" before it has actually
  transferred** (#114): the sending side showed the delivery tick as soon as the
  recipient acknowledged the *message* carrying the file's details — which
  happens before a single byte of the file moves. A transfer that stalled at the
  first chunk therefore looked complete. The sender now shows real progress
  (*"Sending 3/12"*) and only reports *"Delivered"* once the recipient confirms
  the whole file arrived; until then the tick can never read as delivered. Also
  makes stalled transfers diagnosable: the file-transfer path previously logged
  **nothing** on success at any level — a stalled transfer and a perfect one
  produced identical (empty) logs, which is why diagnosing #76 took two machines
  and disk forensics. It now logs a handful of lines per transfer on both ends,
  carrying only a transfer id and chunk counts — never filenames, keys, or onion
  addresses.
- **CLI commands that target a contact no longer fail with a broken pipe**
  (#116): `send`, `send-file`, `remove`, `tail`, `chat`, `search`, `export`, and
  `prune` resolve the contact (one IPC request) and then act (a second request),
  but the daemon's IPC connection is single-request, so the second request hit a
  closed socket (`os error 32`/`232`). Each command now opens a fresh connection
  per request. The daemon and the desktop app were unaffected; this was
  CLI-only. Co-diagnosed across a live Linux⇄Windows CLI test-bus.
- **A stuck first contact no longer says "Connecting…" forever** (#107): the
  durable Welcome re-send (added for offline delivery) replayed bytes bound to
  the original Tor connection, so after a circuit change it could never
  complete — leaving a contact stuck as "Connecting…" indefinitely. It now
  gives up cleanly after 24 hours and shows *"Couldn't connect — remove it and
  send a new invite to try again,"* so you can recover (Remove, #109 → fresh
  invite over a new connection). The contact stays un-messageable until it
  actually connects (never falsely shown as added). Automatic recovery of this
  case is a disclosed v1.1 item.
- **A never-connected first contact no longer floods the peer with undeliverable
  frames** (#108): while a first-contact Welcome was unacked, the ContactCard
  broadcast (on onion rotation or card updates) still encrypted and sent MLS
  application frames on the not-yet-joined group — which the peer could only
  reject as `WrongGroupId`. Now a single invariant is enforced everywhere: while
  a contact is pending (unacked Welcome), **no** outbound MLS application frame
  is emitted for it (text, ContactCard, or attachment), for any kind. The card
  is delivered automatically once the peer joins. No protocol change.
- **A stuck "Connecting…" contact can now be removed** (#109): an unconnected
  first-contact attempt that never completed had no delete path in the UI or
  CLI, so a poisoned attempt could not be cleared to retry. `RemoveContact` is
  now state-aware — a pending/unconnected contact is **fully wiped** (its MLS
  group, pending Welcome, first-contact acks, queued frames, messages, and
  contact row, all in one atomic transaction) so a fresh invite starts clean,
  while a connected contact still archives with its history preserved. Adds a
  Remove action + confirm dialog in the UI and a `skattr remove <contact>` CLI
  subcommand. (Groundwork for the #107/#108 first-contact reliability fixes,
  which need this escape hatch to be testable.)
- **An unconfirmed contact no longer looks successfully added** (#101): a first
  contact that hasn't completed (the peer hasn't accepted the invite) was shown
  as a normal contact, because `list_contacts` mis-reported it as `Active`. It
  now reads honestly — a de-emphasised "Connecting…" for the first two minutes,
  then "Not connected yet — they haven't accepted your invite," and messaging
  stays blocked until they connect.
- **First contact is reliable over flaky Tor** (#90): the outbound onion dial
  succeeds only ~63% of the time per attempt over real Tor (measured; an Arti
  0.44 bump does not help — #99). `add_contact`'s dial was a single attempt, so
  a ~37% transient failure blocked first contact entirely. It now retries the
  dial up to 5× (#106; compounding to ~99% — a local 2-CLI real-Tor loop went
  6/9 at 3× to 6/6 at 5×), which — combined with the #93 durable Welcome
  re-send — makes first contact land reliably.
- **Copy buttons work on Linux/Wayland** (#97): every "Copy" action (invite URL,
  pubkey, onion, history export) was a silent no-op because `navigator.clipboard`
  is restricted in the webkit2gtk webview; they now go through the Tauri
  clipboard-manager plugin.
- **First contact now recovers from a lost inviter Ack** (#93, ADR 0012): if
  the inviter already joined but its acknowledgement was dropped, a re-sent
  Welcome is idempotently re-Acked instead of rejected — the invitee's retry
  now completes rather than stalling forever. Disclosed limitation: a *lost
  first Welcome* delivered over a since-replaced circuit still does not recover
  (the re-sent Welcome carries the original connection's `h_transport` and
  cannot bind on a new circuit); the invitee stays "Connecting…" and keeps
  retrying. Full recovery of this case is v1.1 (#90 Mode A).
- **The "can't-quit" trap** (#30, #31): the *close to tray* setting now
  takes effect immediately instead of only after a restart, and the tray
  icon's right-click menu (with **Quit**) no longer flashes away — a
  left-click still toggles the window, but right-click leaves the menu open.
- **Large attachments no longer silently fail to send** (#75, ADR 0011): the
  file manifest is now encoded as a compact CBOR byte string instead of an
  integer array, which nearly doubled it and pushed attachments above
  ~15.6 MiB over the transport frame cap with no error. Files up to ~31 MiB now
  send; a send failure is also logged instead of being silently discarded.
  (Very large files above ~31 MiB still require the deferred manifest-chunking
  path.)
- **Logs are usable for field-testing** (#77): on-disk logging now defaults
  **ON** for the 0.1.x field-testing line (temporary — reverts to off before
  1.0, #86), and the code comment no longer contradicts the actual default.
  Previously-silent attachment-serve failure paths (missing chunk store, store
  read errors, failed replies) now log, so a stalled chunk transfer is
  diagnosable.
- **Received files could get stuck, or wrongly report a timeout** (#76): if the
  connection to the sender dropped while a file was arriving — common over Tor —
  the download had no way to reconnect. It would sit unfinished, or give up with
  a "request timeout" that the sender had no record of, because the requests
  never actually left your machine. While the app stays running, downloads now
  reconnect on their own, with a back-off so an offline peer isn't hammered, and
  a transfer only reports a timeout when the sender genuinely didn't answer. A
  transfer interrupted by restarting the app may still need a nudge — send that
  contact a message, or hit Retry once you're both back online.

## [v0.1.1] — 2026-06-30

A correctness/security patch release. The headline fix makes Skattr's on-disk state land in one canonical, per-user, admin-free location on every platform — fixing a blocker that prevented standard (non-admin) Windows users from onboarding at all.

### Fixed

- **Data lives in one canonical per-user directory.** State (identity vault, message DB, onion key, Tor state, logs, config) now resolves through a single source of truth to the platform-standard local data dir — `%LOCALAPPDATA%\skattr` (Windows), `~/.local/share/skattr` (Linux), `~/Library/Application Support/skattr` (macOS). This removes the previous current-directory fallback that, on a non-admin Windows install, tried to write beside the executable in `Program Files` and failed with `Access is denied (os error 5)`. The UI and CLI now share the same directory and identity.
- **Existing identities are migrated, not orphaned.** On first launch of the fixed build, an identity created by an earlier build (e.g. under `Downloads\skattr`, the Windows `VirtualStore`, or the old `net.myggiz.skattr` path) is moved into the canonical directory — preserving the onion address, message history, and Tor hidden-service key. Migration is idempotent and fail-loud, and runs on every vault-creating path (`daemon`, `init`, `restore`, and the GUI).

### Security

- **Single-daemon lock.** A pure OS advisory lock on `<data_dir>/daemon.lock` prevents a second daemon from running against the same data directory (which could corrupt the shared database or double-publish the onion service). The lock auto-releases on process death, so a hard kill never leaves a stale lock.
- **Private data directory.** The data directory is enforced to `0700` on Unix at every startup before any secret state is opened.
- **Dependency advisory.** Bumped `anyhow` to 1.0.103 to pick up the fix for RUSTSEC-2026-0190.

## [v0.1.0] — 2026-06-29

First public release of **Skattr** — a desktop-first, metadata-resistant peer-to-peer encrypted messenger. All traffic runs over Tor v3 onion services; there is no central server.

### Highlights

- **End-to-end encrypted 1:1 messaging over Tor.** Message encryption is MLS (RFC 9420 / OpenMLS); transport authentication is Noise_XK; the network layer is Tor v3 onion services via Arti. Identity is an Ed25519 keypair derived from a BIP39 seed phrase.
- **First contact via signed invite links.** Generate a link or QR code; your contact pastes it or clicks the `skattr://` deep link.
- **Offline delivery** through semi-trusted, self-hostable mailboxes. Messages and contact-card updates fall back to a mailbox automatically when a peer is unreachable.
- **File attachments**, both online (direct) and offline (via mailbox), with metadata stripping. Received attachments are **encrypted at rest** — they are decrypted only when you explicitly **Open** (to a managed cache that is wiped on exit) or **Save** (to a path you choose). Nothing is written to your Downloads folder automatically.
- **At-rest encryption** of the local message database (age) and the identity vault, plus backup and restore from your seed phrase.
- **Desktop app** (Tauri 2 + SvelteKit) for Linux, macOS (Apple Silicon), and Windows, plus a **command-line client**. Full-text message search, retention controls, and a settings panel.

### Verifying your download

Releases are signed. Verify before installing:

```bash
minisign -Vm SHA256SUMS -p minisign.pub   # maintainer key ID EEDBFDA4BF232D38
sha256sum -c SHA256SUMS
```

See `docs/install/` for per-platform instructions (Linux `.deb`/AppImage, macOS `.dmg`, Windows `.msi`).

### Known limitations — please read

This is an early release and has **not** had a third-party security audit. v0.1.0 deliberately ships **without**:

- **Multi-member groups** — conversations are strictly 1:1 (2 members).
- **Metadata-minimization defenses** — no message-size padding, send-timing jitter, or cover traffic beyond what Tor itself provides.
- **Real onion-address rotation** — `RotateOnion` republishes the current address rather than changing it.
- **Mailbox fallback for the first-contact handshake** — first contact is direct-only, so both peers must be online to complete it (ordinary messages and card updates do fall back to a mailbox).
- **Garbage collection of received attachment chunks** — they are retained, encrypted, so the data directory grows over time.
- **An encrypted vulnerability-reporting channel** — email `security@myggiz.net` in plaintext for now (a PGP key is planned after v0.1.0).

See `docs/THREAT_MODEL.md` for the full security model and disclosures.

### License

Client GPLv3; mailbox server AGPLv3. © Myggiz B.V.

## [Development history]

> The phase-by-phase sections below predate the v1.0 readiness audit that
> reshaped the project. They document the original build (Era 1, Phases 0–2.H).
> The audit-era security, offline-delivery, and attachment workstreams that
> followed are summarized in the v0.1.0 entry above rather than re-logged here;
> they are kept for historical reference.

## Phase 2.H — Windows port

### Added
- Windows support for the daemon IPC layer via Tokio Named Pipes.
- Per-platform submodules under `core::daemon::ipc::server` and
  `core::daemon::ipc::client`.
- `IpcStream`, `PeerId`, and `ENDPOINT_FILENAME` cross-platform
  aliases in `core::daemon::ipc::mod.rs`.
- `current_peer_id()` public helper that resolves to `current_uid`
  (Unix) or `current_sid` (Windows).
- Owner-SID-only DACL on the Windows Named Pipe + post-accept SID
  equality check (mirrors Unix's 0600 mode + peer_cred pattern).
- `windows-sys = "0.59"` dependency for raw Win32 SID and
  SECURITY_DESCRIPTOR FFI.
- `windows-latest` runner in both `ci.yml` (test + clippy) and
  `release.yml` (build + smoke).
- `.msi` bundle production via Tauri WiX template; `msiexec /qn`
  smoke install in CI.
- `docs/install/windows.md` with download → verify → install →
  first-run + SmartScreen walkthrough.

### Changed
- `Server::bind`'s `allowed_uid: u32` → `allowed: PeerId`.
- CLI `--socket` flag and `$SKATTR_SOCKET` env var docs note that
  on Windows the path points to the daemon's discovery file.
- CLI's default IPC endpoint filename changes from `daemon.sock`
  to the platform-specific `ENDPOINT_FILENAME` (`ipc.sock` on Unix,
  `ipc.endpoint` on Windows). Users with `--socket=/run/user/1000/skattr/daemon.sock`
  hard-coded must update.
- Workspace `unsafe_code` lint downgraded from `"forbid"` to
  `"deny"` so the single Windows-FFI module can carry an explicit
  `#![allow(unsafe_code)]`. All other crates and modules still
  reject unsafe code at compile time.
- `crates/mailbox/src/health.rs` is `cfg(unix)`-gated; the mailbox
  is not shipped on Windows.

### Deferred to Phase 5
- Authenticode code-signing (and macOS Developer ID + notarisation).
- Tauri auto-updater enablement.
- macOS Intel matrix entry.

## Phase 2.G — packaging prep (2026-05-04, superseded)

### Added

- Phase 2.G: first packaged release.
- New `core::daemon::smoke` module: `run_smoke(SmokeConfig)` initialises a throwaway vault, boots the daemon, waits for `TorStatus::Ready`, and exits 0/1 — used by CI release smoke.
- `skattr-ui --smoke-test --data-dir <tmp> [--timeout-secs N]` argv-level branch (no webview opened).
- `skattr daemon --smoke-test` developer escape hatch on the CLI.
- Linux `.deb` + AppImage bundles via `cargo tauri build`.
- macOS `.dmg` bundle (Apple Silicon only for v0.1).
- In-repo Flatpak manifest at `packaging/flatpak/net.myggiz.skattr.yml` (Flathub publication deferred).
- AppStream metainfo at `packaging/flatpak/net.myggiz.skattr.metainfo.xml`.
- `skattr://` URL scheme handler — invite paste becomes invite click. Wired via `tauri-plugin-deep-link` + `tauri-plugin-single-instance`.
- `.github/workflows/release.yml`: matrix-build (Linux + macOS), per-platform smoke gate, `SHA256SUMS` + `SHA256SUMS.minisig` (minisign), GitHub Release auto-creation on `v*` tag.
- `docs/install/{README,linux,macos}.md` — verification + install + first-run.
- `docs/build/{reproducible,flatpak}.md` — pinned-version recipe + Flatpak notes.
- Six PNG icon sizes (16/32/64/128/256/512) generated from a new `crates/ui/icons/icon.svg` source.

### Changed

- `tauri = "2"` → `tauri = "=2.11.0"` (Phase 2.G locked decision 3).
- `tauri-build = "2"` → `tauri-build = "=2.6.0"` (matched to current published version).
- `rust-toolchain.toml` gains an explicit `version = "1.95.0"` line.
- Tauri updater plugin explicitly disabled in `tauri.conf.json` (Phase 5 will enable).
- `crates/cli`: `tempfile` moved from `[dev-dependencies]` to `[dependencies]` for the `--smoke-test` escape hatch.

### Deferred to Phase 2.H

- Windows IPC port (Named Pipes + DACL peer auth) and `.msi` bundle.
- macOS Intel (`x86_64`) bundle.

### Wire format

No changes. Phase 2.G is wire-format-NEUTRAL by design; the `wire_format_append_only` snapshot test is unchanged.

## Phase 2.F — settings & history

### Added (Phase 2.F — settings & history)
- Settings panel with five sidebar-nav sections (Identity / Mailboxes /
  History / Notifications / Advanced) under `/settings/<section>/`.
- New `Command` variants: `GetConfig`, `SetConfig { patch }`,
  `ChangePassphrase { old, new }`, `SetContactMuted { contact, muted }`,
  `TailLogs { since_seq, limit }`, `GetPassphraseAuditLatest`,
  `WipeAllData` and supporting types (`ConfigSnapshot`, `ConfigPatch`,
  `NotificationMode`, `LogLevel`, `LogRecord`).
- New `Event::LogRecord` + `EventFilter::Logs` for live log tail.
- New additive fields on `ContactSummary`: `muted: bool`,
  `peer_mailboxes: Vec<String>`.
- `Vault::change_passphrase` wired through dispatch as a thin handler
  with zxcvbn ≥ 3 + length ≥ 8 + ≠-current validation. Crash-safe via
  the existing sidecar + atomic rename in `core::identity::vault`.
  (Original spec assumed a two-file rekey across an additional age-key
  file; the actual storage layout derives the SQLite age key from the
  BIP39 seed via HKDF, so only the identity vault needs rewriting.)
- Per-contact mute persisted in a new `contacts.muted` column
  (migration `0013`).
- Append-only `passphrase_audit` table (migration `0014`) surfacing
  "Last changed" in Settings → Identity.
- In-memory ring-buffer logs subsystem (`core::daemon::logs`) with a
  `tracing-subscriber` layer that strips 64-char hex pubkeys and
  `*.onion` addresses above the DEBUG level. `RingBufferLayer`
  installed in both CLI and UI subscriber stacks.
- Tauri 2 built-in tray (Show / Tor status / Unread / Quit). Click on
  tray icon toggles window visibility.
- Close button hides to tray when `ui.close_to_tray = true` (default);
  fall back to "quit on close" when tray init fails (Wayland).
  `ui.start_minimised` honoured at startup (effective on next launch
  after a SetConfig change).
- `notify-rust` desktop notifications via two new Tauri commands
  (`notify`, `focus_window_and_open_conversation`). SvelteKit-side
  dispatcher (`shouldNotify` + `buildNotification`) is focus-aware
  and respects per-contact mute.
- Cmd/Ctrl-K `SearchPalette` over 1.G's `SearchMessages`. Same
  component reused inline in Settings → History (placeholder copy
  for the inline mount in this release).
- Conversation view honours `?focus_row_id` (transient highlight via
  the searchPalette store; mark-read remains gated on bottom-of-list
  intersection — 2.D semantics preserved).
- "Delete all data and quit" Danger Zone in Settings → Advanced
  (two-step confirm; daemon shuts down + removes data_dir + exits 0).

### Changed (Phase 2.F)
- Daemon retention sweep re-reads `Config` on every tick so retention
  changes hot-apply without restart.
- `DaemonHandle` gains `config: Arc<RwLock<Config>>` and `config_path:
  PathBuf` (set via `set_config_arc` from `Daemon::run`).

### Docs (Phase 2.F)
- `docs/operations/2f-notification-smoke.md` — per-OS smoke checklist
  (notifications + tray + logs + wipe).
- `docs/operations/passphrase-recovery.md` — crash-safety explanation
  of the actual single-file rekey + lost-passphrase BIP39 fallback.
- `docs/superpowers/specs/2026-05-04-phase-2f-settings-history-design.md`.
- `docs/superpowers/plans/2026-05-04-phase-2f-settings-history.md`.

### Notes (Phase 2.F)
- The original plan's "stage-then-rename two-file ChangePassphrase
  with six kill-point integration tests" collapsed to a thin wrapper
  over `Vault::change_passphrase` after a brainstorm-vs-codebase
  reconciliation. The vault's existing atomicity coverage handles
  the same guarantees with a much smaller surface.
- `persist_logs_to_disk` toggle currently requires daemon restart
  to take effect (the `tracing_subscriber::reload` plumbing was
  thorny to wire across the layered subscriber; tracked as a
  follow-up).
- macOS / Windows click-to-focus from notifications uses a separate
  `notify-rust` API not wired in 2.F's first cut. Linux gets it via
  the XDG `conversation_id` hint.

### Added (Phase 2.E — invite & contact UX)
- Invite-generate dialog (QR + copy-link) and add-contact dialog (paste
  invite URL) accessible from the contact list.
- Inline `ContactDetailsPanel` with rename + archive (soft-delete via
  `contacts.hidden`).
- Daemon-side Welcome propagation fix: `Frame::MlsWelcome` (codec slot
  0x03, reserved since 1.A) is now load-bearing. `DeliveryHub::send_welcome`
  + a new peer-actor send/read arm + `InboundDispatch::dispatch_welcome`
  turn Bob's `AddContact` Welcome into Alice's `Group::join_from_welcome`,
  so Alice's group transitions `PendingJoin → Active`.
- Migration `0010`: `outstanding_invites` table for inviter-side PSK
  persistence.
- Migration `0011`: `contacts.hidden` for soft-delete.
- Migration `0012`: `outstanding_invites.provider_snapshot` so the
  MlsProvider's KP init key survives the create_invite → dispatch_welcome
  boundary.
- Three new `Command` variants: `RenameContact`, `RemoveContact`,
  `ListContactsWithFilter` (no new `CommandResult` or `Event` variants;
  rename / archive reuse `ContactUpdated`).
- `key_package_id` in `CommandResult::InviteCreated` is now the
  canonical MLS `KeyPackageRef` (was plain SHA-256 in 1.D — same shape
  on the wire).

### Deferred (Phase 2.E)
- **Task 2.E.5**: mailbox fallback for Welcome propagation — direct-only
  Welcome ships in 2.E; mailbox fallback deferred to avoid touching the
  2.B mailbox protocol freeze (ADR 0006).

### Added (Phase 2.D — conversation view)
- Composer (`crates/ui/src-svelte/src/lib/components/Composer.svelte`):
  Enter to send, Shift+Enter for newline, IME-safe (`event.isComposing`
  + `compositionstart` / `compositionend` gating), paste-as-plaintext
  via `event.clipboardData.getData("text/plain")` with `preventDefault()`.
  Disabled prop drives both textarea + send button; placeholder text
  reflects daemon-down / `pending_join` / `corrupt` states.
- Per-message delivery state icons via new
  `crates/ui/src-svelte/src/lib/components/DeliveryIcon.svelte` (4 states:
  pending → clock, sent/Deposited → check, delivered → check-check,
  failed → alert-triangle). Backed by 4 bundled Lucide ISC SVG glyphs
  (`lib/icons/{clock,check,check-check,alert-triangle}.svg` + LICENSE),
  loaded via Vite's `?raw` import. New `--danger` design token (7th
  colour, dark `#ef4444` / light `#dc2626`).
- Scroll-back pagination via new `Command::RecentMessages.before_id:
  Option<i64>` cursor + `paged: bool` opt-in flag (both
  `#[serde(default)]`). New `CommandResult::MessagesPage { records,
  next_before_id }` variant alongside the unchanged `Messages(Vec)`
  tuple — CLI consumers untouched. Storage method
  `MessageRepo::recent_before(group_id, before_id, limit)` is the
  sibling of `recent` with `WHERE id < ?before_id ORDER BY
  mls_generation DESC, id DESC LIMIT ?limit` (strict-less cursor).
- Frozen "Unread" separator anchored to `ContactSummary.last_read_row_id`
  at conversation-open; does not advance live as new messages arrive.
- New wire fields on `ContactSummary`: `group_state:
  Option<MlsGroupStateLabel>` (Active / PendingJoin / Corrupt; mirrors
  the three concrete `mls::state::GroupState` variants) and
  `last_read_row_id: Option<i64>` (per-group read cursor surfaced from
  `ReadStateRepo`). Composer disables on `Some(Corrupt)` /
  `Some(PendingJoin)`.
- New wire field `CommandResult::MessageSent.record:
  Option<MessageRecord>` for UI optimistic reconciliation.
  `dispatch::send_message` captures the post-encrypt `row_id` from
  `insert_in_tx` (was discarded with `let _`) and projects a
  canonical `MessageRecord` into the reply. Idempotent-retry branch
  returns `record: None`.
- Optimistic send path in the UI: `conversation.send(contact, body)`
  generates a temp `__tempId`, appends an `OptimisticMessage`
  placeholder, awaits the IPC reply, then `reconcile`s with the
  canonical record (or `markFailed` on error). Bubble icon flows:
  pending → (queued|delivered) → (deposited|delivered|failed) via
  the new `delivery` store keyed by hex `message_id`.
- Mark-read trigger: open-event AND bottom-of-list intersection.
  500 ms debounce coalesces bursts into one `Command::MarkRead`.
  Live-arrival auto-mark only fires when scrolled within 100 px of
  bottom (`isWithinBottomThreshold`).
- 5 skeleton bubbles render at the top of the list during in-flight
  `loadOlder()` calls (CSS pulse animation, gated by
  `prefers-reduced-motion`).
- New wire-format snapshot lint test
  (`crates/core/tests/wire_format_append_only.rs`): exhaustive match
  arms make adding a `Command` or `CommandResult` variant a compile
  error; sorted-list snapshot catches accidental removals or
  reshapes. Phase 2.D exit constraint enforcement.

### Fixed (Phase 2.D — caught by e2e harness)
- `routes/+page.svelte` did not call `refreshContacts()` on direct
  navigation to `/` — only the first-run completion path did, so
  re-opening the app with an unlocked vault showed an empty contact
  list until first manual refresh.
- `delivery_status_changed` events from the subscribe stream were
  silently dropped — the route's handler only routed
  `tor_status_changed` and `message_received`. UI delivery icons
  would never have advanced from live events.
- `.shell` CSS lacked `grid-template-rows: 100vh; overflow: hidden`,
  causing the conversation pane to expand to content height instead
  of viewport height. The virtualizer collapsed (rendered all rows
  at once, no scroll possible).

### Tests (Phase 2.D)
- 7 new dispatch tests (paged recent_messages + sender-side record
  projection + group_state / last_read_row_id on `list_contacts`).
- 2 new storage tests (`recent_before` cursor exclusion + orphan
  cursor handling).
- 7 new commands.rs serde tests for the new variants/fields, with
  legacy-CBOR decode coverage on every additive change.
- 53 Vitest specs total in `crates/ui/src-svelte/` (was 22 in 2.C):
  +5 DeliveryIcon, +6 Composer, +15 conversation store, +10
  delivery store, +4 tokens.css update.
- 2 new Playwright e2e specs: `composer.spec.ts` (Enter happy path
  with optimistic→delivered icon promotion + Shift+Enter newline)
  and `pagination.spec.ts` (200-msg conversation, scroll-back
  through 4 pages of 50 with cursor exhaustion).
- 1 new `#[ignore]`-gated real-Tor integration test
  (`crates/tests/src/ui_send_roundtrip.rs`) asserting
  `MessageSent.record.is_some()`, `record.row_id > 0`, and
  `last_read_row_id` cursor advance after `MarkRead`.
- `cli_two_daemons` updated to assert `MessageSent.record.is_some()`
  end-to-end.

### Known limitations (Phase 2.D)
- The `AddContact` dispatcher creates the MLS group on the consumer
  side but does not propagate the resulting Welcome message to the
  inviter. Consequence: the inviter cannot decrypt messages until
  this is wired up. Tracked as a follow-up beyond Phase 2.D's exit
  criterion. The `ui_send_roundtrip` test's module doc documents
  the gap.
- ts-rs emits `Hex16` and `PublicKey` as bare `string` (lowercase
  hex), not the tuple-struct shape (`{ "0": number[] }`) the
  original plan assumed. UI store + component code uses string
  equality and hex strings throughout.

### Added (Phase 2.C — UI bootstrap, read-only conversation MVP)
- New crate `crates/ui/` (GPLv3): Tauri 2 + SvelteKit shell with
  in-process `Daemon::run`. Two-phase Tauri command surface:
  pre-daemon (`vault_exists`, `identity_init`, `vault_unlock`),
  post-daemon (`ipc_request`, `ipc_subscribe`, `start_in_process_cmd`).
  CLI continues to attach to the daemon's existing IPC socket.
- New wire surface, additive only:
  - `Command::DaemonInfo` + `CommandResult::DaemonInfo {`
    `local_pubkey, current_onion: Option<String>, daemon_version,`
    `schema_version }`.
  - `ContactSummary` projection extensions: `unread_count: u64`,
    `last_message_preview: Option<String>` (≤80 code points,
    `Kind::Text` only), `last_ts_recv: Option<u64>` —
    all `#[serde(default)]` for forward compat.
  - `Subscribe` ack now replays the latest cached
    `Event::TorStatusChanged` immediately after `Ok(Subscribed)` for
    `EventFilter::All` and `EventFilter::TorStatus`. Cache lives on
    `DaemonHandle::latest_tor_status`, populated by a tap task
    spawned in `Daemon::run`.
- Storage helpers: `Pool::schema_version()` + `MessageRepo::latest_for_group()`.
- `dispatch::list_contacts` rewrite: populates the new fields
  (N+1 per-contact reads on `unread_count` + `latest_for_group`);
  applies `last_ts_recv DESC NULLS LAST, added_at DESC` ordering.
- `ts-rs` codegen: every wire type (29 types across 15 files)
  derives `TS` and emits to
  `crates/ui/src-svelte/src/lib/ipc/types/`. Files are gitignored
  per spec decision 13; regeneration runs on `cargo test -p skattr-core`.
- Locked design tokens (`crates/ui/src-svelte/src/lib/tokens.css`):
  6 colours, 4-step spacing, 3-step type scale, dark-mode-first
  with `prefers-color-scheme: light` override. Bundled Inter font
  (OFL 1.1, regular + medium woff2).
- Four-step first-run wizard: welcome → passphrase (zxcvbn ≥3) →
  24-word BIP39 seed type-back (case-insensitive, whitespace-tolerant,
  with red-modal "skip confirmation" escape) → Tor bootstrap.
- Read-only main shell: contact list + open conversation pane,
  live-append on `Event::MessageReceived`, virtualised message list
  via `@tanstack/svelte-virtual` (substituted for the
  unmaintained `svelte-virtual-list`).
- Tests: 16 Vitest specs + 4 Playwright e2e specs (first-run +
  unlock paths, headless Tauri mock); new
  `crates/tests/src/ui_first_run.rs` `#[ignore]`-gated real-Tor
  integration test.
- 2.C-only behaviour: closing the window quits the daemon. 2.F
  upgrades this to hide-to-tray; CLI users running the UI alongside
  should be aware.
- Mailbox wire surface inherited unchanged from 2.B; UI does not
  render mailbox state in 2.C (2.F).

### Workarounds documented (Phase 2.C)
- `esrap@1.4.9` patched via `pnpm patch` to handle TypeScript
  `EmptyStatement` nodes — known Svelte 5 + vite-plugin-svelte
  compatibility gap; track upstream.
- `app.html` CSP relaxed to allow SvelteKit's inline bootstrap
  scripts in dev/preview; production Tauri WebView enforces its
  own stricter CSP.
- `cargo deny` ignore list extended for ~16 Tauri 2 transitive
  unmaintained advisories (gtk-rs GTK3 bindings, unic-*,
  proc-macro-error). Revisit when Tauri ships a non-GTK Linux
  backend.

### Added (Phase 2.B)
- Mailbox client (`core::mailbox::client`) with long-lived per-mailbox
  `Framed` connection.
- `core::mailbox::auth` — single source of truth for the auth-digest
  helpers (hoisted from `crates/mailbox` so client + server share one
  implementation).
- Adaptive `PollScheduler` with Idle ↔ Active ↔ Unreachable cadence
  and ±25 % jitter; per-mailbox actors emit `MailboxStatusChanged`
  events on transition.
- `DeliveryHub::ensure_mailbox_fallback` — pick-one-then-retry
  orchestrator using `BLAKE2s(message_id) % mailbox_count`.
- `Command::AddMailbox`, `Command::RemoveMailbox`, `Command::RotateOnion`,
  `Command::ListMailboxes` (real handler) wired through `daemon::dispatch`.
- `Event::MailboxStatusChanged { mailbox_id, status }`,
  `Event::ContactCardReceived { contact, version }`.
- `EventFilter::Mailboxes`, `EventFilter::Delivery`.
- `Envelope::Kind::ContactCardUpdate { card: Box<ContactCard> }` for
  in-MLS card rotation.
- Migration 0008 (mailbox status + outbox target_kind/mailbox_id +
  composite unique index).
- Migration 0009 (`self_card_state` singleton).
- `core::contact::self_card::build_next_self_card` for monotonic
  version-bumped self-card publishing.
- 5 integration tests (`crates/tests/src/mailbox_*.rs`) covering
  offline delivery, mailbox failover, AddMailbox validation,
  RemoveMailbox drain, and RotateOnion republishing.
- Adversarial regression suite (5 scenarios) and logging-redaction
  guard.

### Deferred (TODOs in code)
- **Task 20.5**: `PeerConnection` direct-timeout trigger to
  `DeliveryHub::ensure_mailbox_fallback`.
- **Task 22.5**: `RemoveMailbox` final-drain ciphertexts through
  `DaemonInbound::dispatch`.
- **Task 23.5**: real HS key rotation in `Command::RotateOnion` (today
  bumps the self-card version + republishes the current onion).

## Phase 2.A — Mailbox server

`crates/mailbox/` promoted to `[lib] + [bin]` (AGPLv3). Frozen wire
surface in `core::mailbox::protocol` (ADR 0006). Server ships
transactional cap-eviction, per-connection + global token-bucket
rate limits, challenge-response auth with single-use 30 s nonces,
three background tasks (expiry, challenge sweep, metrics), local-only
UDS healthcheck, hardened systemd unit, distroless Dockerfile, and
an operator guide that targets ≤ 30 min from-fresh-VM. Test pyramid:
unit + property + fuzz + adversarial (every `ErrorCode` covered) +
24 h soak (`#[ignore]`-gated) + real-Tor smoke. The auth digest
input was switched from a CBOR map to a positional CBOR tuple after
a property-test tripwire showed `ciborium`'s serde derive emits
struct fields in declaration order rather than canonical-sorted;
the tuple form removes the ambiguity entirely.
`core::mailbox::client` and `core::mailbox::scheduler` remain stubs;
2.B picks them up.

## Phase 1.H — Hardening

Closes all 11 items surfaced in Phase 1.G review threads. No new
user-facing features; no wire-protocol breaks (additive
`DaemonErrorKind::InvalidArgument` variant + additive
`MessageRecord.row_id` field only).

### Correctness

- **#1** `ContactRepo::contact_for_group(&[u8; 32])` helper for 2-member groups;
  `daemon::dispatch::search_messages` now resolves the peer via the hit's
  `group_id` on unscoped search, fixing the bug where outgoing rows rendered
  `record.contact == local_pubkey` (`dd11b47`).
- **#2** Migration 0007 adds `messages.envelope_id BLOB` (16 bytes, shape trigger,
  `(group_id, envelope_id)` unique index). Idempotent startup backfill
  (`MessageRepo::backfill_envelope_id`, fail-closed on error, `ORDER BY id ASC`
  for deterministic dedup). `MessageRepo::insert` binds the column; UNIQUE
  violations project to `StorageErrorKind::DuplicateMessage`, mapped by the send
  path to `SendStatus::Delivered` for idempotent retries
  (`3e8fe29`, `49a2deb`, `e271971`, `df804eb`).
- **#3** Send + receive persistence is now transactional. New `Group::save_in_tx`,
  `MessageRepo::insert_in_tx`, `OutboxRepo::insert_in_tx`,
  `SeenMessagesRepo::insert_in_tx`, `delivery::receiver::receive_in_tx`.
  `daemon::dispatch::send_message` and `daemon::inbound::dispatch_for_group` run
  the full group-save + message-insert (+ outbox on send) under one
  `pool.transaction`. On `ReceiveOutcome::Rejected`, the closure returns `Err` so
  the advanced-ratchet snapshot rolls back with the rejected history row
  (`73acde1`, `84ecbd1`, `21731a6`, `ea2689d`, `0f5305f`).

### Error taxonomy

- **#4** `DaemonErrorKind::InvalidArgument { message }`; `prune_history` validation
  no longer returns `IpcError::Internal`; CLI exit code 2 for `InvalidArgument`,
  1 for everything else (`7c604dd`).
- **#5** Subsystem error sub-enums (`StorageErrorKind`, `ContactErrorKind`,
  `InviteErrorKind`, `MlsErrorKind`, `DeliveryErrorKind`, `TransportErrorKind`)
  replace `str::contains` matching in `CoreError::kind()`. Build-time grep guard
  test `kind_has_no_string_matching` prevents regression
  (`8877f82`, `46e404a`, `60fff74`, `61eacef`, `42ea815`, `cadc102`, `d743f0f`).

### IPC / API polish

- **#7** `MessageRecord.row_id: i64` is now a public field (was silently dropped in
  `project()`). Additive — no wire-format break (`4afba6c`).

### Hygiene & infra

- **#6** `daemon::clock::now_unix_seconds` replaces five duplicates (one per-module,
  one retention helper, three integration tests) (`125e6c9`).
- **#8** `MessageRepo::backfill_body_text` runs its UPDATE loop in one transaction
  (`bc5b2f6`).
- **#9** `ReceiveOutcome::New.group_id: [u8; 32]` (was `Vec<u8>`). Group creation
  uses `MlsGroup::new_with_group_id` with 32 random bytes to establish the
  invariant at creation time (`3f0286a`).
- **#10** CI cargo-deny job restored after invalid `--all-features` flag was
  dropped; job is green (`a28d60a`).
- **#11** `serial_test` dev-dep replaces a hand-rolled `Mutex` on the socket-path
  env tests (`033030d`).

### Test counts

347 lib tests (up from 328 at Phase 1.G), zero failures. `cargo deny check`
clean. `cargo clippy --workspace --all-targets --all-features -- -D warnings`
clean. `cargo fmt --check` clean.

## Phase 1.G — Message storage & search

- `storage::messages` gains `search` (FTS5 BM25 + snippet, tokenize-and-AND query escaper),
  `unread_count`, `mark_read`, `export_page`, `prune_before`, `prune_keep_last`, and a
  one-shot `backfill_body_text` startup helper. New `body_text` mirror column +
  `messages_fts` triggers keep the FTS index in lock-step with `messages`.
- `storage::read_state` (new): per-group `last_read_message_id` cursor.
- `messages` table: `mls_generation` and `ts_daemon_recv` columns persist real values
  (replacing 1.F's `0` / `ts_envelope` placeholders).
- `delivery::receiver::receive` carries `mls_generation` + `ts_daemon_recv` into a
  struct `ReceiveOutcome::New`; the `DaemonInbound` caller routes through
  `receive()` and broadcasts `Event::MessageReceived { contact, record }`.
- `daemon::commands`: new `Command::SearchMessages` / `MarkRead` / `PruneHistory` /
  `ExportHistory`, matching `CommandResult` + `SearchHitRecord` wire types,
  `EventFilter::Messages { contact }`, `Event::MessageReceived` reshaped to
  `{ contact, record }`, `DaemonErrorKind::SearchSyntax`.
- `daemon::retention` (new): hourly tokio sweep task driven by
  `[history] retention_days = 0` (default infinite).
- `Daemon::run` runs `backfill_body_text` once at startup and spawns the retention
  sweep before signalling readiness.
- `daemon::dispatch::send_message` now persists outgoing rows via `MessageRepo::insert`
  (outbox-only flow in 1.F) so sender-side history populates for export/search/tail.
- CLI: `skattr search` / `export` / `prune`; `skattr tail --follow` upgraded to
  subscribe to `Event::MessageReceived` via `EventFilter::Messages`. New CLI dep
  `time = "0.3"` for RFC3339 parsing on `skattr prune --before` / `skattr export`.
- Validation: `cargo test -p skattr-core --release --features test-harness \
  --test fts_search_p95 -- --ignored --nocapture` reports search p95 well
  under the 50 ms target over 100k synthetic rows.

## Phase 1.F — CLI integration (2026-04-23)

- Persistent `skattr daemon` owns `Pool` + `DeliveryHub` + `OnionListener` + IPC server.
- New `daemon::ipc` submodule: CBOR length-prefix codec, Unix-socket server with `0600` perms and `SO_PEERCRED`/`getpeereid` peer-cred check, per-connection state machine that lets `Subscribe` coexist with further `Execute`s (powers `skattr chat`), `IpcClient` for the CLI.
- `Daemon::run` signature changed: now takes `Config` and returns via `Ready { onion, ipc_socket }`.
- Migration 0005 adds `contacts.group_id` (plus index); `AddContact` populates it atomically.
- New wire-safe types: `ContactSummary`, `MessageRecord`, `SendStatus`, `Direction`, `Hex16`, `Hex32`, `EventFilter`, `IpcError`, `DaemonErrorKind`.
- Every CLI stub (`invite`, `add`, `contacts`, `send`, `tail`, new `chat`) now wires through IPC. `init`/`restore`/`backup` remain in-process.
- `skattr daemon` prompt moved to `/dev/tty` via `rpassword`; `--passphrase-file <path>` / `$SKATTR_PASSPHRASE_FILE` for automation.
- `skattr invite --qr` renders an ASCII QR via the `qrcode` crate.
- `skattr send --fail-on-timeout` flips the 2 s inline-wait default from "exit 0 with `status=queued`" to "exit 8".
- Integration tests: `cli_ipc_roundtrip.rs` (mocked transport), `cli_two_daemons.rs` (full invite→send flow, mocked transport; full decrypt round-trip is deferred because MLS Welcome-handoff is not yet symmetric), `cli_real_tor.rs` (`#[ignore]`-gated, real Arti).

## Phase 0 — scaffold (historical)

### Added

- Phase 0 workspace scaffold: `core`, `mailbox`, `cli`, `tests` crates.
- Module tree for protocol, transport, MLS, storage, delivery, daemon.
- Initial SQL migration (`0001_init.sql`).
- CLI subcommands (stubbed): `init`, `restore`, `daemon`, `invite`, `add`, `send`, `contacts`.
- Architecture Decision Records 0001–0003.
- `cargo-deny` and CI matrix across Linux/macOS/Windows.
- **Phase 0.B identity & crypto**: real Ed25519 keypair ops (`generate`, `public`, `sign`, `verify_strict`), BIP39 24-word mnemonic encode/decode, Argon2id (`m=64 MiB, t=3, p=4`) + XChaCha20-Poly1305 on-disk vault at `identity.vault` with AEAD-bound format version, HKDF-SHA256 helpers with domain-separated info labels.
- `skattr init` — generates identity, prints 24-word recovery phrase, writes encrypted vault.
- `skattr restore <seed>` — rebuilds identity from BIP39 phrase under a fresh passphrase.
- `Vault::change_passphrase` — decrypt-old → rewrite-new with fresh salt/nonce (crash-safe via atomic rename after Phase 0.B hardening).
- End-to-end round-trip integration test (`crates/core/tests/identity_roundtrip.rs`).
- `proptest` round-trip coverage on Seed ↔ Mnemonic (256-case default, 10k with `PROPTEST_CASES`).
- `crates/core/fuzz/vault_parser` cargo-fuzz harness asserting `Vault::open` never panics (requires nightly).
- **Phase 0.B hardening:** atomic + fsync'd vault writes (`atomic_write_vault`); `Vault::change_passphrase` now crash-safe via tempfile → rename; `IdentityKey::from_bytes` takes `Zeroizing<[u8; 32]>`; mnemonic phrase/entropy intermediates zeroized; `verify()` returns a single opaque "verification failed" error for constant-time parity; `Mnemonic::from_words` normalizes like `parse`; CLI gains `--data-dir` override and zeroizes its argv seed copy; ADR-0004 pins passphrase byte contract; added tests for signature-byte tampering, Argon2 salt/param sensitivity, and a real `from_seed` domain-separation assertion.
- **Phase 0.B cleanup:** `Vault::open` decrypts in-place via `AeadInPlace::decrypt_in_place_detached` into `Zeroizing<[u8; 32]>` — no Vec<u8> plaintext intermediate; `encrypt_identity` helper DRYs the vault-write path between `Vault::create` and `Vault::change_passphrase`; `atomic_write_vault` best-effort cleans up the `.vault.tmp` sidecar on error.
- **Phase 0.C Arti integration:** `TorRuntime::bootstrap` / `publish_onion` / `connect` / `shutdown` backed by `arti-client` 0.41 + `tor-hsservice` 0.41. HS signing key persisted at `<data_dir>/hs.key.age` encrypted under `HKDF(seed, "skattr-hs-storage-v1")`, injected into Arti's keystore via `launch_onion_service_with_hsid` (behind `experimental-api`) so `.onion` address is seed-derived and stable across restarts. `OnionListener` accepts rend requests and yields `DataStream`s via mpsc. `skattr daemon` bootstraps, publishes, prints the `.onion`, and awaits Ctrl-C. Two-daemon echo integration test (`crates/tests/src/arti_echo.rs`, `#[ignore]`-gated). ADR-0005 documents the Arti-vs-system-tor decision.
- **Phase 0.C cleanup:** `Daemon::run(data_dir, &Zeroizing<String>, ready_tx, shutdown_fut)` public wrapper owns the daemon startup flow; CLI no longer reaches into `transport::*`. Transport module + submodules + `OnionListener` + `Seed::from_storage_bytes` re-narrowed to `pub(crate)` (integration tests reach them via `skattr_core::test_exports` behind the `test-harness` feature). `inject_hs_secret` probes the Arti keymgr before inserting (no more error-string matching). `arti_echo` tempdirs chmod'd to 0700. `experimental-api` + `onion-service-cli-extra` audit comment added at the workspace dep site.
- **Phase 0.D Storage layer:** `rusqlite` + `age`-encrypted `Pool` (keyed by `HKDF(seed, "skattr-storage-v1")`, decrypt-to-plaintext-working-file at open, encrypt-back on close). Migrations runner keyed by a `schema_version` table (one migration so far: `0001_init.sql`). Seven repos — `ContactRepo`/`MessageRepo`/`MlsGroupRepo`/`OutboxRepo`/`MailboxRepo`/`SeenMessagesRepo` plus onion-address helpers on `ContactRepo`. Transactions wrapper with commit-on-Ok / rollback-on-Err. Backup archive (`tar.gz` of the three at-rest files, outer age-encrypted under `HKDF(seed, "skattr-backup-v1")`). `skattr backup <file>` + `skattr restore-backup <seed> <file>` CLI commands. Storage primitives exposed via `skattr_core::daemon::backup` (public wrappers) and `skattr_core::test_exports` (test-harness feature) so internals stay `pub(crate)`. 40+ new unit/integration tests.
- **Phase 0.E Documentation baseline:** `docs/THREAT_MODEL.md` v0 (7 adversary classes, guarantees, non-goals). `docs/OPERATIONS.md` dev-stack guide (build, test, daemon, backup/restore flows, known operational issues). `ARCHITECTURE.md` refreshed with Phase 0.B/C/D implementation state and a concrete "send one message" data-flow trace. `README.md` updated to "Phase 0 complete" with What-works / What-doesn't sections and a quickstart that covers init/daemon/backup/restore-backup.
- **Phase 0 complete.** All five workstreams shipped (0.A scaffold, 0.B identity, 0.C Arti, 0.D storage, 0.E docs). Phase 1 (MLS + delivery) up next.
- **Phase 1.A Frame codec:** `transport::frame::FrameCodec` implements `tokio_util::codec::Decoder` + `Encoder` for the 10 `Frame` variants. Wire format is `[u32 BE length][u8 type][payload]`, max 16 MiB. `Ping`/`Pong`/`Bye` carry empty payloads, `Ack` is 16 raw bytes, `NoiseInit`/`NoiseResp`/`MlsWelcome`/`MlsCommit`/`MlsApp` are opaque byte blobs, `Error` is a 2-field CBOR map (`code: u16`, `message: String`). New `CoreError::Frame(String)` variant. Coverage: inline unit tests (round-trip + boundary), 10 000-case `proptest`, and a `cargo-fuzz` target with 11-file seed corpus.
- **Phase 1.B Noise_XK handshake:** `transport::noise::handshake_initiator` + `handshake_responder` drive `snow`'s `Noise_XK_25519_ChaChaPoly_BLAKE2s` (optionally `Noise_XKpsk3_...` when an invite PSK is supplied on both sides) over any `AsyncRead + AsyncWrite + Unpin + Send` stream. 1-byte `0x01` version preamble before the first Noise frame; `Frame::NoiseInit` reused for msg1 and msg3, `Frame::NoiseResp` for msg2. Ed25519 → X25519 bridge on `IdentityKey::{noise_static_secret, noise_static_public}` via libsodium-style SHA-512 clamp + Edwards→Montgomery map (no new wire fields, no `curve25519-dalek` direct dep). `HandshakeOutcome` exposes `peer_x25519` + 32-byte `h_transport = HKDF(handshake_hash, "skattr-binding-v1")` for Phase 1.C MLS external-PSK binding. `AuthenticatedConnection<S>` is a stateful `Framed<S, FrameCodec>` + `snow::TransportState` wrapper with `&mut self` async `send`/`recv`/`close` — frame-in-frame, outer `Frame::MlsApp` on the wire, inner `Frame` at the application. Whole-handshake timeout (30 s) defends against slowloris. Error taxonomy funnels through `CoreError::Transport("handshake: ...")` with fixed strings (no key bytes, no payload bytes). Coverage: ten inline unit tests (happy paths, PSK happy/mismatch/unilateral, version, unexpected frame, stream EOF, wrong peer static, timeout via `start_paused`, round-trip) plus an integration test at `crates/core/tests/noise_handshake.rs` gated on `feature = "test-harness"`.
- **Phase 1.C MLS 2-member groups:** `mls::Group` wraps `openmls::group::MlsGroup` with a `{Active, PendingJoin, Corrupt}` state machine. `Group::create_solo` builds the single-member group; `add_member` produces a (Welcome, Commit) pair bumping epoch 0→1; `join_from_welcome` lands the invitee at epoch 1. Bidirectional `encrypt(Envelope)` / `decrypt` wrap MLS application messages. `advance_epoch` + `process_incoming_commit` cover the PCS primitive (policy wiring stays in 1.E/1.F). Persistence is checkpoint-snapshot: `MlsProvider::snapshot` ciborium-encodes the `openmls_rust_crypto::MemoryStorage` HashMap into `mls_groups.state_blob`; `load` reverses. External PSK (`h_transport` from 1.B) is registered under the identifier `b"skattr-binding-v1"`, proposed by the inviter, registered by the invitee; mismatch fails with `"mls: welcome process: ..."`. New `KeyPackage` newtype (+ `generate`, `to_bytes`, `from_bytes`, `hash`) persisted via a new `KeyPackageRepo` (migration 0002 adds a `key_packages` table with single-use `consumed` flag — enforcement deferred to 1.D). Ciphersuite locked to `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519` (IANA code-point `0x0003` — corrected from a stale `0x0001` that actually named the AES128GCM suite). Coverage: 14 `mls::group` unit tests + `mls::provider`, `mls::key_package`, `storage::key_packages` repo tests + one integration test at `crates/tests/src/mls_pair.rs` simulating Alice↔Bob exchange + restart.
- **Phase 1.D Invite & contact flow:** `invite::InviteLink` mints signed `skattr://invite/v1#id=&onion=&kp=&psk=&exp=&sig=` URLs with a fixed field order; `from_url(url, now)` parses, verifies Ed25519 over canonical CBOR of the unsigned body, checks expiry, and moves the 32-byte PSK into a `Zeroizing` guard (body copy zeroized). Single-use replay prevention reuses 1.C's `KeyPackageRepo.consumed` flag keyed by SHA-256 of the KP bytes (`record_received` / `is_consumed` / `mark_consumed`). `contact::ContactCard::{sign, verify(now)}` with canonical-CBOR body signing + expiry check; monotonic version persistence via a new `contact_cards` table (migration 0003) with `put_card` rejecting stale or equal versions and `latest_card` returning the top version. `ContactRepo::get` / `list` now hydrate `Contact.card`. `IdentityKey::sign_cbor` / `verify_cbor` helpers factor the body-signing pattern for both invite and card. QR SVG rendering via `qrcode` (feature `qr`); `render_png` removed per scope. Coverage: 14 `invite::link` unit tests, 5 `contact::card` tests, 14 `storage::contacts` tests (8 new), 1 QR test, 1 integration test `crates/tests/src/invite_roundtrip.rs`.
- **Phase 1.E Delivery semantics:** `delivery::hub::DeliveryHub<S>` routes outbound sends and post-handshake connections into per-peer `delivery::peer::PeerConnection` actor tasks. Each actor owns `Option<AuthenticatedConnection<S>>` + a `HashMap<MessageId, oneshot::Sender>` pending-ACK map + a 1 s retry tick that re-sends any `Outbox::due` row not already in pending, a 60 s keepalive (30 s pong deadline), 180 s idle close, and `PeerCtrl::ReplaceConn` for concurrent-dial races. `delivery::backoff::backoff(attempts)` doubles from 1 s, caps at 5 min, with ±25 % uniform jitter. `delivery::outbox::Outbox` wraps `storage::outbox::OutboxRepo` over `PublicKey` + `MessageId`; migration 0004 adds `message_id BLOB NOT NULL` + `UNIQUE(target, message_id)` to the `outbox` table so enqueue is idempotent and ACK is keyed. `delivery::receiver::receive` enforces a ±1 h replay window (overflow-safe via `saturating_sub`), dedups via the existing 24 h `seen_messages` index, and persists via `MessageRepo::insert`. `delivery::kill_stream::{KillSwitch, KillableStream}` under `feature = "test-harness"` supports the integration test. Inbound-MLS is injected into each actor via a `pub(crate) trait InboundDispatch`. `test_exports` gains the Phase 1.E types for integration tests. `Daemon::send` is a `pub(crate)` stub until 1.F wires the Command path. Coverage: unit tests (backoff, outbox wrapper, receiver, peer actor happy path + retry tick + keepalive, hub routing) + migration-0004 golden + integration test `crates/tests/src/delivery_kill_mid_message.rs` (`tokio::io::duplex` + `KillableStream`, kill-mid-message → reconnect → exactly-once delivery, plus ciphertext-hash pre-dedup in the test's `MlsInboundDispatch` to survive OpenMLS's replay-generation check) + `#[ignore]`-gated real-Tor smoke test `crates/tests/src/delivery_real_tor.rs`.

[Unreleased]: https://github.com/myggiz/skattr/compare/main...HEAD
