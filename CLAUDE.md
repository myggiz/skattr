# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository state

**Phase 0 is complete; Phase 1 is complete (1.H merged 2026-04-24);
Phase 2.A (mailbox server) is complete; Phase 2.B (mailbox client +
ContactCard rotation) is complete (merged 2026-05-01); Phase 2.C
(UI bootstrap, read-only conversation MVP) is complete (merged
2026-05-02); Phase 2.D (conversation view) is complete (merged
2026-05-02); Phase 2.E (invite & contact UX) is complete (merged
2026-05-03); Phase 2.F (settings & history) is complete (merged
2026-05-04); Phase 2.G (packaging & distribution) is complete
(merged 2026-05-04) on Linux + macOS; Phase 2.H (Windows
port) is complete (merged 2026-05-06); Phase 2 is fully closed.** Phase 0 shipped all five workstreams (0.A scaffold,
0.B identity & crypto, 0.C Arti integration, 0.D storage layer, 0.E
documentation baseline). Phase 1.A added `transport::frame::FrameCodec`.
Phase 1.B added `transport::noise::handshake_{initiator,responder}`
+ the stateful `AuthenticatedConnection<S>` wrapper, plus the
Ed25519 → X25519 bridge on `IdentityKey`. Phase 1.C added `mls::Group`
(2-member only), `MlsProvider` checkpoint-snapshot persistence,
`KeyPackage` newtype + `KeyPackageRepo`, and migration 0002. Phase
1.D added `invite::InviteLink` (skattr://invite/v1# URL with
fragment-only params, canonical-CBOR Ed25519 signature, Zeroizing
PSK guard, single-use tracking via `KeyPackageRepo.consumed`),
`contact::ContactCard::{sign, verify}` with monotonic-version
persistence in a new `contact_cards` table (migration 0003), and
`IdentityKey::{sign_cbor, verify_cbor}` helpers.
Phase 1.E added `delivery::hub::DeliveryHub<S>` (per-daemon router), per-peer `delivery::peer::PeerConnection` actors (1 s retry tick, 60 s keepalive, 180 s idle close, `PeerCtrl::ReplaceConn` for concurrent-dial races), `delivery::outbox::Outbox` over `OutboxRepo` with `(target, message_id)` idempotency (migration 0004), `delivery::receiver::receive()` for ts-window + dedup + persist, a `pub(crate) trait InboundDispatch` injection point for MLS decrypt, and `delivery::kill_stream::{KillSwitch, KillableStream}` under `feature = "test-harness"`. A CI integration test (`delivery_kill_mid_message.rs`) proves kill-mid-message → reconnect → exactly-once delivery; `delivery_real_tor.rs` (`#[ignore]`-gated) exercises the same stack over real Arti.
Phase 1.F added the `skattr daemon` IPC server + `IpcClient`, expanded
`Daemon::run` to own `Pool` + `DeliveryHub` + IPC, introduced
`DaemonHandle` + `dispatch::execute_command`, migration 0005
(`contacts.group_id`), `DaemonInbound` (MLS decrypt + persist + emit
`Event::MessageReceived`), `/dev/tty` passphrase prompts (rpassword),
`--passphrase-file` automation, `--qr` invite rendering,
`--fail-on-timeout` on `send`, and three integration tests
(`cli_ipc_roundtrip`, `cli_two_daemons`, `cli_real_tor` `#[ignore]`-gated).
Phase 1.G added FTS5 wiring (triggers off a new `body_text` mirror
column, `messages_fts` recreated to reference it), persisted
`mls_generation` and `ts_daemon_recv` on `messages` (replacing 1.F's
placeholders), `MessageRepo::{search, unread_count, mark_read,
export_page, prune_before, prune_keep_last, backfill_body_text}`,
`ReadStateRepo` for per-group last-read cursors, `daemon::retention`
(hourly sweep + `[history] retention_days`), and IPC for
`SearchMessages` / `MarkRead` / `PruneHistory` / `ExportHistory` plus
`Event::MessageReceived` (reshaped to `{ contact, record }`) and
`EventFilter::Messages`. `daemon::dispatch::send_message` now persists
sender-side rows. CLI gained `search` / `export` / `prune`;
`tail --follow` subscribes to the event stream. Migration 0006 lands
the schema. The 100k-row benchmark (`crates/core/tests/fts_search_p95.rs`,
`#[ignore]`-gated) asserts search p95 < 50 ms.
Phase 1.H closes the 11 items surfaced in 1.G review threads: migration
0007 adds `messages.envelope_id` + `(group_id, envelope_id)` unique
index + idempotent startup backfill; send + receive persistence runs
under one `pool.transaction` via `Group::save_in_tx` +
`MessageRepo::insert_in_tx` + `OutboxRepo::insert_in_tx` (and
`receive_in_tx` on the inbound side). `CoreError::kind()` is a pure
structural match over six subsystem sub-enums (`StorageErrorKind`,
`ContactErrorKind`, `InviteErrorKind`, `MlsErrorKind`,
`DeliveryErrorKind`, `TransportErrorKind`); a build-time guard test
enforces zero `str::contains` in `kind()`.
`DaemonErrorKind::InvalidArgument` + CLI exit code 2 give operators a
clean signal for argument-validation errors. `MessageRecord.row_id`
surfaces the SQLite id for UI correlation;
`ContactRepo::contact_for_group` fixes unscoped-search outgoing-hit
contact resolution. `daemon::clock::now_unix_seconds` replaces five
duplicates; `ReceiveOutcome::New.group_id: [u8; 32]` (group IDs are
now generated as 32 random bytes at `create_solo`);
`backfill_body_text` runs in one transaction; the socket-path Mutex is
replaced by `serial_test`.
Phase 2.A added `crates/mailbox/` as a `[lib] + [bin]` AGPLv3 crate:
`MailboxServer::accept_loop` per-stream FSM over the shared wire
layout (length+type+CBOR; type bytes 0x82–0x8F), `Store` with
transactional cap-eviction insert, `Challenges` (single-use 30 s
nonces), `Policy` + per-conn / global token buckets, three
background tasks (expiry / challenge sweep / metrics), a UDS
healthcheck at `${data_dir}/health.sock`, and Arti glue feature-
gated as `bin`. `core::mailbox::protocol` is frozen (ADR 0006); the
auth digest input is a positional CBOR tuple after a Task 16
property tripwire revealed `ciborium`'s serde-derive non-canonical
field ordering. `core::mailbox::client` and `scheduler` stay stubs
for 2.B. Operational artefacts: `packaging/systemd/skattr-mailbox.service`,
`packaging/Dockerfile` (distroless cc + nonroot),
`docs/operations/mailbox-setup.md` (≤ 30 min target).

`crates/core/src/identity/` is fully implemented (Ed25519, BIP39,
Argon2id + XChaCha20-Poly1305 vault, HKDF). `crates/core/src/transport/{tor,
hs_key, listener}.rs` wire `arti-client` 0.41 + `tor-hsservice` 0.41
end-to-end. `crates/core/src/storage/` has a real `rusqlite` + `age`
`Pool`, a migrations runner, seven typed repos, transactions wrapper,
and portable backup export/import. `docs/` has a v0 threat model,
an operations guide, and refreshed ARCHITECTURE.md with a
"send one message" data-flow trace.

The daemon is driven by `Daemon::run(data_dir, &Zeroizing<String>,
ready_tx, shutdown_fut)` — the CLI is a thin wrapper. `transport`
and `storage` are both `pub(crate)`; integration tests reach
internals via `skattr_core::test_exports` gated on the `test-harness`
feature. All four crates compile, `cargo clippy -D warnings` / `cargo
test` / `cargo fmt --check` are green — 77+ unit + integration tests
passing. Phase 0 exit criterion (two daemons echo bytes over Tor)
is exercised by `crates/tests/src/arti_echo.rs`, `#[ignore]`-gated
(run with `cargo test -p skattr-tests --release -- --ignored`).

Phase 2.B (mailbox client + ContactCard rotation) merged at the head
of `phase-2b-mailbox-client`. `core::mailbox::{client, codec, poll,
auth}` ship the v1-protocol client (long-lived per-`'mine'` mailbox,
on-demand for deposits), an Idle/Active/Unreachable per-mailbox
`PollScheduler` with ±25 % jitter, the `DeliveryHub` direct→mailbox
fallback (`ensure_mailbox_fallback`, pick-one + sequential failover
via BLAKE2s), and the `RotateOnion` / `AddMailbox` / `RemoveMailbox` /
`ListMailboxes` daemon commands. Migration 0008 adds status tracking
to `mailboxes` and `target_kind`/`mailbox_id` to `outbox` (composite
unique index `idx_outbox_target_message_kind_mailbox`); migration
0009 adds the `self_card_state` singleton for monotonic version
bumps. ContactCard updates ride MLS app messages as
`Envelope::Kind::ContactCardUpdate { card: Box<ContactCard> }`, so
rotation reuses the same direct→mailbox fallback path as ordinary
messages. New events: `MailboxStatusChanged`, `ContactCardReceived`;
new filters: `EventFilter::{Mailboxes, Delivery}`.

Three TODOs in code track follow-up work that didn't fit the 2.B
freeze: **Task 20.5** wires the per-peer direct-timeout trigger from
`PeerConnection` to `DeliveryHub::ensure_mailbox_fallback`; **Task
22.5** routes `RemoveMailbox`'s final-drain ciphertexts through
`DaemonInbound::dispatch`; **Task 23.5** is real HS key rotation —
`Command::RotateOnion` today bumps the self-card version and
republishes the current onion (the address itself does not change),
so contacts see `ContactCardReceived` with a new version but route
to the same onion until 23.5 lands.

Phase 2.C added a new `crates/ui/` crate (GPLv3): Tauri 2 +
SvelteKit shell that boots an in-process `Daemon::run`, walks
first-run users through a four-step wizard (welcome → passphrase
with zxcvbn ≥3 → 24-word BIP39 seed type-back → Tor bootstrap),
and renders a read-only contact list + open conversation with
live-append on `Event::MessageReceived`. Two-phase Tauri command
surface: pre-daemon `vault_exists` / `identity_init` / `vault_unlock`
(restricted to three by lint test), post-daemon `ipc_request` /
`ipc_subscribe` / `start_in_process_cmd` over the daemon's existing
Unix IPC socket so the CLI keeps working unchanged. New wire
surface (additive only): `Command::DaemonInfo`, `ContactSummary`
projection fields (`unread_count`, `last_message_preview`,
`last_ts_recv`, all `#[serde(default)]`), and a filter-gated
`TorStatusChanged` replay on the `Subscribe` ack backed by
`DaemonHandle::latest_tor_status` plus a tap task spawned in
`Daemon::run`. `ts-rs` emits TS bindings for every wire type into
`crates/ui/src-svelte/src/lib/ipc/types/` (gitignored per spec
decision 13; regenerated on `cargo test -p skattr-core`). Locked
design tokens (6 colours / 4-step spacing / 3-step type, dark-first
with `prefers-color-scheme: light`) and bundled Inter (OFL 1.1)
ship in `crates/ui/src-svelte/src/lib/`. Virtualised message list
uses `@tanstack/svelte-virtual` (substituted for the unmaintained
`svelte-virtual-list`). 2.C closes the window by quitting the
daemon — 2.F replaces this with hide-to-tray. Mailbox CRUD wire
surface from 2.B is consumed unchanged; UI rendering of mailbox
state lands in 2.F. Tests: 16 Vitest specs + 4 Playwright e2e
specs (first-run + unlock paths, headless Tauri mock); new
`crates/tests/src/ui_first_run.rs` `#[ignore]`-gated real-Tor
integration test.

Phase 2.D (conversation view) merged at the head of
`phase-2d-conversation-view`. The composer (Enter-to-send,
Shift+Enter newline, paste-as-plaintext, IME-safe), per-message
delivery state icons (clock → check → check-check → !), and
scroll-back pagination (50 rows/page, `before_id` cursor, 5
skeleton bubbles during loads) round out the conversation pane.
Wire-format additions are strictly additive: `Command::Recent
Messages` gains `before_id: Option<i64>` + `paged: bool` (both
`#[serde(default)]`); new `CommandResult::MessagesPage { records,
next_before_id }` variant alongside the unchanged `Messages(Vec)`;
`MessageSent` gains `record: Option<MessageRecord>`;
`ContactSummary` gains `group_state: Option<MlsGroupStateLabel>`
+ `last_read_row_id: Option<i64>`. New storage method
`MessageRepo::recent_before` powers pagination. The frozen "Unread"
separator anchors to `ContactSummary.last_read_row_id` at
conversation-open and never advances live. Mark-read fires on both
conversation-open and bottom-of-list intersection (debounced
500 ms, scroll-proximity ≤ 100 px). Optimistic send: UI appends a
placeholder bubble with a `__tempId`, awaits `MessageSent.record`,
reconciles in place. New wire-format snapshot test
(`crates/core/tests/wire_format_append_only.rs`) makes adding or
reshaping a `Command`/`CommandResult` variant a deliberate edit.

E2e harness work surfaced three production bugs fixed in 2.D:
`refreshContacts()` not called on direct `/` navigation;
`delivery_status_changed` events silently dropped from the
subscribe stream; `.shell` CSS missing `grid-template-rows:
100vh; overflow: hidden` causing the virtualizer to collapse.

Known 2.D limitation: the `AddContact` dispatcher creates the
MLS group only on the consumer side and does not propagate the
Welcome message to the inviter. The inviter cannot decrypt
messages from the new contact until this is wired up. Tracked as
a follow-up beyond 2.D's exit criterion; the
`ui_send_roundtrip` `#[ignore]`-gated test documents it.

Phase 2.E added invite-generate / add-contact dialogs, an inline
ContactDetailsPanel with rename + archive, and the daemon-side
Welcome-propagation fix. Migration `0010` adds an `outstanding_invites`
table for inviter-side PSK persistence; migration `0011` adds
`contacts.hidden` for soft-delete; migration `0012` adds
`outstanding_invites.provider_snapshot` so the MlsProvider's KP init
key survives the create_invite → dispatch_welcome boundary.
`Frame::MlsWelcome` (codec slot 0x03, reserved since 1.A) is now
load-bearing: `DeliveryHub::send_welcome` + a new peer-actor send/read
arm + `InboundDispatch::dispatch_welcome` turn Bob's `AddContact`
Welcome into Alice's `Group::join_from_welcome`, so Alice's group
transitions `PendingJoin → Active` and she can decrypt Bob's first
message. Wire-format is strictly additive: three new `Command`
variants (`RenameContact`, `RemoveContact`, `ListContactsWithFilter`),
no new `CommandResult` variants, no new `Event` variants (rename /
archive reuse `ContactUpdated`). The `key_package_id` returned in
`CommandResult::InviteCreated` is now the canonical MLS
`KeyPackageRef` (was plain SHA-256 in 1.D — same shape on the wire).

One TODO tracks follow-up work deferred from 2.E: **Task 2.E.5** is
mailbox fallback for Welcome propagation — direct-only Welcome ships
in 2.E; mailbox fallback is deferred because it would touch the 2.B
mailbox protocol freeze (ADR 0006).

Phase 2.F (settings & history) merged at the head of
`phase-2f-settings-history`. Migrations `0013` (`contacts.muted`) and
`0014` (`passphrase_audit`) land alongside seven new `Command`
variants (`GetConfig`, `SetConfig`, `ChangePassphrase`,
`SetContactMuted`, `TailLogs`, `GetPassphraseAuditLatest`,
`WipeAllData`), four new `CommandResult` variants (`Config`,
`PassphraseChanged`, `Logs`, `PassphraseAudit`), `Event::LogRecord`
+ `EventFilter::Logs`, and two additive fields on `ContactSummary`
(`muted`, `peer_mailboxes`). `ChangePassphrase` wraps the existing
`Vault::change_passphrase` (single-file atomic rewrite via
sidecar + rename) — the spec's original "stage-then-rename two-file
journal" design was simplified after discovering that the SQLite
age key is derived from the BIP39 seed via HKDF and isn't re-keyed
by passphrase changes. New core modules: `core::daemon::logs`
(in-memory ring buffer + redacting tracing layer + IPC tail) and
`core::storage::passphrase_audit`. UI side: settings sidebar
layout under `routes/settings/<section>/`, ChangePassphraseDialog,
LogsViewer, SearchPalette (Cmd/Ctrl-K modal + inline reuse),
contact mute toggle + peer_mailboxes rendering, Tauri 2 tray,
focus-aware `notify-rust` notifications, close-to-tray hide,
"Delete all data and quit" Danger Zone. The persist-logs-to-disk
toggle currently requires a daemon restart to take effect (the
`tracing_subscriber::reload` plumbing across the layered
subscriber is tracked as a follow-up). Closes Phase 2's user-facing
chrome; the next workstream is Phase 2.G (packaging).

Phase 2.G (packaging & distribution) merged at the head of
`phase-2g-packaging`. New `core::daemon::smoke` module (run_smoke
+ SmokeConfig + SmokeError); `skattr-ui --smoke-test` argv branch
that boots the daemon without opening Tauri's webview; CLI escape
hatch on `skattr daemon --smoke-test`. CI release flow at
`.github/workflows/release.yml` triggered on `v*` tags: matrix
build on ubuntu-latest + macos-latest → per-platform smoke
(`/usr/bin/skattr-ui --smoke-test` after `dpkg -i`; AppImage
extract-and-run; `.app` from mounted `.dmg`) → `SHA256SUMS` +
`SHA256SUMS.minisig` (minisign secret in repo secrets) → GitHub
Release. Bundle metadata locked: `net.myggiz.skattr` identifier,
six PNG icon sizes, `skattr://` URL scheme via
`tauri-plugin-deep-link` + `tauri-plugin-single-instance`, Tauri
updater explicitly disabled. Linux `.deb` + AppImage + Flatpak
(build-from-source); macOS `.dmg` (ARM64 only). Install docs at
`docs/install/{README,linux,macos}.md`; reproducible-build recipe
at `docs/build/reproducible.md`. Tauri Rust pinned to `=2.11.0`;
`@tauri-apps/api` matched at `2.0.0`; `rust-toolchain.toml` gains
explicit `version = "1.95.0"`. Wire-format-NEUTRAL by design — no
new `Command` / `CommandResult` / `Event` variants. **Windows is
carved out to Phase 2.H** (Named Pipes + DACL port of
`core::daemon::ipc`; `.msi` bundle); 2.H lands before any "v0.2"
tag. Maintainer prerequisite before tagging v0.1.0:
generate the real minisign keypair (placeholder at
`docs/install/minisign.pub` until then) and set GitHub Actions
secrets `MINISIGN_SECRET_KEY` + `MINISIGN_PASSWORD`; the
maintainer-only procedure is documented at
`docs/install/README-MAINTAINER-MINISIGN.md`.

Phase 2.H (Windows port) merged at the head of `phase-2h-windows-port`.
`crates/core/src/daemon/ipc/{server,client}` now have per-platform
submodules: `unix.rs` (AF_UNIX, unchanged) and `windows.rs` (Tokio
Named Pipes + owner-SID DACL + post-accept SID equality check).
New cross-platform aliases in `core::daemon::ipc::mod.rs`:
`IpcStream` (UnixStream / NamedPipeClient), `PeerId` (u32 / Vec<u8>
SID), `ENDPOINT_FILENAME` (`ipc.sock` / `ipc.endpoint`). Discovery
file at `<data_dir>\ipc.endpoint` carries the random per-daemon
pipe name `\\.\pipe\skattr-<24-hex>`. CI's `windows-latest` matrix
entry is now non-optional: `cargo test --workspace --exclude
skattr-ui --features test-harness` and `cargo clippy --workspace
--exclude skattr-ui --all-targets --all-features` both run on
`windows-latest`. The `release.yml` matrix produces a `.msi`
artifact via Tauri's WiX template; the smoke step installs via
`msiexec /qn` and runs `skattr-ui --smoke-test`. New install doc
at `docs/install/windows.md`. Workspace `unsafe_code = "deny"`
(was `"forbid"`) so the single Windows-FFI module
(`crates/core/src/daemon/ipc/server/windows.rs`) can opt in with
`#![allow(unsafe_code)]`. Wire-format-NEUTRAL — no `Command` /
`CommandResult` / `Event` variant additions. Phase 2 is now fully
closed; v0.2 can drop the "Windows deferred" disclaimer.

Phase 1 is complete (1.H merged 2026-04-24); Phase 2.A (mailbox
server) merged at the head of `phase-2a-mailbox-server`; Phase 2.B
is complete (merged 2026-05-01); Phase 2.C is complete (merged
2026-05-02); Phase 2.D is complete (merged 2026-05-02); Phase 2.E
is complete (merged 2026-05-03); Phase 2.F is complete (merged
2026-05-04); Phase 2.G is complete (merged 2026-05-04); Phase 2.H
is complete (merged 2026-05-06); Phase 2 is fully closed. See
`docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md`
for the Phase 2 decomposition,
`docs/superpowers/specs/2026-05-02-phase-2d-conversation-view-design.md`
for the 2.D internals, and
`docs/adr/0006-mailbox-protocol-v1.md` for the wire freeze 2.B
develops against. The bootstrap prompt remains
authoritative for file layout, module boundaries, type signatures,
and visibility rules — match it exactly.

## Authoritative docs (read these first)

Work is driven by four docs in `docs/`; they have clear roles — don't invent structure these files don't cover:

- `skattr-bootstrap-prompt.md` — exact Cargo workspace layout, file-by-file module tree, key types with method signatures, dependency list, initial SQL migration, success criteria for the scaffold. This is the spec for "make the project compile."
- `skattr-design.md` — protocol spec: wire framing, Noise_XK handshake, MLS binding, invite link format, mailbox threat model. Source of truth for *what the protocol does*.
- `skattr-implementation-plan.md` — phased workstreams (0 through 5) with per-phase locked decisions, exit checklists, and risks. Source of truth for *what to build in what order*.
- `skattr-deep-dives.md` — detailed design for the `core` module layout, MLS group state machine, mailbox wire protocol, and first-run UX. Consult before touching those areas.

When these docs disagree, the design doc wins for protocol semantics; the bootstrap prompt wins for initial file layout and scaffolding.

## Development process

Use the `superpowers` skills by default for every development task — they are rigid workflows, don't skip them for "simple" work:

- **Before creating, designing, or changing behavior** → `superpowers:brainstorming` (explore intent before code).
- **Multi-step tasks with a spec** → `superpowers:writing-plans`, then `superpowers:executing-plans`.
- **Writing implementation code** → `superpowers:test-driven-development`.
- **Any bug, test failure, or unexpected behavior** → `superpowers:systematic-debugging`.
- **Before claiming work complete / committing / opening a PR** → `superpowers:verification-before-completion` (run the commands, show the output, no success claims without evidence).
- **Receiving code review feedback** → `superpowers:receiving-code-review` (verify before implementing).
- **2+ independent tasks** → `superpowers:dispatching-parallel-agents`.

The `using-superpowers` skill itself enforces "invoke relevant skills BEFORE any response or action" — treat that as binding, not advisory.

## What Skattr is

A Rust, desktop-first, metadata-resistant P2P encrypted messenger. All traffic goes over Tor v3 onion services (via Arti). Message encryption is MLS (RFC 9420) via OpenMLS. Transport auth is Noise_XK via `snow`. Identity is an Ed25519 keypair backed by a BIP39 seed phrase. No central server; mailboxes exist only for offline delivery and are semi-trusted. Licensed GPLv3 (client) / AGPLv3 (mailbox server). Owned by Myggiz AB (Sweden).

## Locked technical decisions (do not casually revisit)

These are decided and changing them has cascading consequences. Full rationale lives in the design doc and implementation plan's "Decisions to lock" tables.

- **Edition / toolchain:** Rust 2021, stable, pinned via `rust-toolchain.toml`.
- **Async runtime:** Tokio (Arti requires it).
- **Tor:** Arti (`arti-client` + `tor-hsservice`). Fallback to shelling out to system `tor` is documented in workstream 0.C but **not** something to architect around unless Arti blocks you.
- **Noise pattern:** `Noise_XK_25519_ChaChaPoly_BLAKE2s` (via `snow`). Identity keys are the Noise static keys — **distinct from onion service keys** (see design §1.1).
- **MLS ciphersuite:** `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519`. Note: the design doc mentions a 256-bit variant in prose; the bootstrap prompt and Phase 1 decision lock the 128-bit variant — use the 128-bit one.
- **Crypto libraries:** RustCrypto (`ed25519-dalek`, `x25519-dalek`, `chacha20poly1305`). Argon2id params: `m=64MiB, t=3, p=4`.
- **Seed phrase:** BIP39. Derivation path: `seed → HKDF("skattr-identity-v1") → ed25519 seed → keypair`. Domain-separate every HKDF use.
- **Wire serialization:** CBOR via `ciborium`. Config: TOML.
- **Storage:** `rusqlite` (bundled) with WAL mode + app-level encryption via `age`. Migrations are `include_str!`'d SQL keyed by a `schema_version` table.
- **Errors:** `thiserror` in libraries, `anyhow` in binaries. **No `unwrap()` / `expect()` in library code** — use `?` and typed errors.
- **Logging:** `tracing` + `tracing-subscriber`. Never log pubkeys, onions, or message contents at `info` level or higher; redaction by default.
- **Invite URI scheme:** `skattr://invite/v1#...` (fragment-based to avoid referer leaks).
- **Transport↔MLS binding:** `h_transport = HKDF(noise_handshake_hash, "skattr-binding-v1")` is injected as external PSK into the first MLS Commit. Preserve this binding when refactoring either layer.

## Workspace layout (target)

A Cargo workspace with these crates (see bootstrap prompt for the full tree):

- `crates/core/` — the library where almost all logic lives (identity, transport, mls, envelope, invite, contact, mailbox client, delivery, storage, daemon). Licensed GPLv3.
- `crates/mailbox/` — standalone server binary. Shares `core::mailbox::protocol` types. Licensed **AGPLv3**.
- `crates/cli/` — thin `clap`-based binary wrapping `core::Daemon`. Licensed GPLv3.
- `crates/tests/` — integration tests (spawn daemon pairs, wait for Tor bootstrap).
- `crates/ui/` — **reserved, do not scaffold in Phase 0**. Tauri 2 + SvelteKit lands in Phase 2.

## Module visibility discipline

In `core`, only these are public API: `daemon`, `identity` (key types only), `envelope`, `invite`, `contact`, `error`. Everything else is `pub(crate)`. If something inside `transport/`, `mls/`, `mailbox/`, `delivery/`, or `storage/` needs to be exposed, wrap it in a public type from one of the approved modules rather than widening visibility.

## Non-obvious hard constraints

- **Every `.rs` file must carry a license header comment.** GPLv3 for `core`/`cli`/`tests`, AGPLv3 for `mailbox`.
- **All secret types zeroize.** Wrap keys, seeds, passphrases, derived key material in `Zeroizing` or implement `ZeroizeOnDrop`. No raw `[u8; 32]` secrets sitting on the stack un-zeroed.
- **No custom crypto.** No hand-rolled Noise patterns, no "small tweaks" to MLS, no hand-rolled AEAD. Where the design doc says "use X," use X.
- **MLS state is fragile.** Treat MLS storage like a database: transactions, WAL, explicit recovery paths. A single bad write can brick a group — see deep-dives Part 2 for the state machine (`Active`, `PendingJoin`, `PendingCommit`, `CatchingUp`, `Removed`, `Corrupt`).
- **Timestamps are display-only.** Authoritative ordering comes from MLS generation numbers, not `Envelope.ts`. Validate `ts` within ±1h for replay resistance; don't sort by it.
- **Invite KeyPackages are single-use.** Mark consumed on first successful use; reject on second.
- **The scaffold must pass `cargo clippy -D warnings`** and `cargo test` (even with `todo!()`-stubbed bodies) before being considered done.
- **Workspace-level `dead_code = "allow"` and `unused_imports = "allow"` are intentional during Phase 0** (see `Cargo.toml` comment). Most `pub(crate)` items and re-exports are legitimately dead until Phase 1 wires call sites. Remove these allows at the start of Phase 1, not before.
- **Use `todo!()`, never `unimplemented!()`** in stub bodies — workspace lint warns on `unimplemented` and CI's `-D warnings` turns it into an error.

## Dep version gotchas

- `rusqlite` is pinned at 0.38 (not latest) — `arti-client 0.41`'s `tor-dirmgr` transitively requires `>=0.36,<0.39`. Bumping breaks the `links = "sqlite3"` uniqueness rule. Revisit when arti bumps.
- OpenMLS triplet version numbers don't line up: `openmls = 0.8`, `openmls_traits = 0.5`, `openmls_rust_crypto = 0.5`. Don't "match" them.
- `[u8; N>32]` fields (signatures, 64-byte keys) need `#[serde(with = "serde_big_array::BigArray")]` — serde derive doesn't cover arrays longer than 32.
- `ciborium::ser::Error` / `de::Error` are generic over `W::Error`/`R::Error`; `#[from]` is fragile. Use `.map_err(|e| CoreError::CborEncode(e.to_string()))`.

## Commands

**Cargo isn't on system PATH** — prefix with `. "$HOME/.cargo/env" &&` or add `~/.cargo/bin` to your shell. rustup was installed at the user level during bootstrap.

Scaffold is in place and builds clean (`cargo build` / `cargo clippy -D warnings` / `cargo test` all green as of bootstrap).

```bash
cargo build                          # build all crates
cargo test                           # run all tests
cargo test -p core identity          # run tests for one module
cargo test --test handshake          # run a single integration test
cargo clippy --all-targets -- -D warnings
cargo fmt --all
cargo deny check                     # licenses, advisories, sources (config in deny.toml)
cargo audit                          # advisory scan

# CLI (once implemented)
cargo run -p cli -- init             # generate identity + seed phrase
cargo run -p cli -- restore <seed>   # rebuild identity from seed
cargo run -p cli -- daemon           # start Tor, publish onion, accept connections
cargo run -p cli -- invite [--qr]
cargo run -p cli -- add <link>
cargo run -p cli -- send <contact> <text>
```

CI runs fmt + clippy + test on `ubuntu-latest`, `macos-latest`, and
`windows-latest`, plus a dedicated `ui` job on `ubuntu-latest` for
the Tauri 2 + SvelteKit crate. macOS x86_64 is still deferred —
`macos-latest` is Apple Silicon only.

## When extending the design

- Protocol-level changes (frame types, invite fields, handshake binding, MLS ciphersuite) need an ADR under `docs/adr/` with rationale before code.
- New dependencies need justification in the PR and must pass `cargo-deny` (license allowlist, no git deps, no banned crates).
- Every PR touching crypto, protocol, or auth requires a second reviewer.
