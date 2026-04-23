# Phase 1.F CLI Integration — Design Spec

**Status:** Approved 2026-04-23. Scope locked by `docs/superpowers/specs/2026-04-21-phase-1-decomposition.md` §1.F.
**Depends on:** 1.A (frame codec), 1.B (Noise handshake), 1.C (MLS 2-member groups), 1.D (invite & contact), 1.E (delivery semantics), 0.D (storage).
**Exit criterion (verbatim from decomposition):** "`skattr invite` / `add` / `send` / `tail` / `contacts` wire through a real `Daemon::execute`; IPC via Unix socket; config in `~/.config/skattr/config.toml`."

## 1. Scope

In scope:

- Persistent `skattr daemon` process that owns `TorRuntime`, storage `Pool`, `DeliveryHub`, onion listener, identity, and the events broadcast channel for its entire lifetime.
- New Unix-domain-socket IPC layer inside `crates/core/src/daemon/ipc/` (server + client + wire types + codec).
- Every stateful CLI command (`invite`, `add`, `contacts`, `send`, `tail`, `chat`) becomes a thin IPC client that connects to the daemon, issues one `Command`, renders the `CommandResult`, and exits. Streaming commands (`tail --follow`, `chat`) additionally `Subscribe` to the daemon's event stream.
- Stateless/local commands (`init`, `restore`, `backup`, `restore-backup`) stay in-process and do not touch the socket.
- Migration 0005: add `group_id` to the `contacts` table so every contact row points at its MLS 2-member group.
- Real `Config::load` with XDG-aware precedence (flag > env > file > default).
- Global `--json` output flag on non-streaming commands.
- `skattr invite --qr` renders an ASCII QR to stdout via the `qrcode` crate.
- `skattr send --fail-on-timeout` flips the inline-wait timeout from "return `status=queued` and exit 0" to "exit with code 8"; useful for retry-loop scripts.
- Replace the stdin `TODO` at `crates/cli/src/main.rs:178` with an `rpassword`-style `/dev/tty` prompt; add `--passphrase-file <path>` / `$SKATTR_PASSPHRASE_FILE` for non-interactive automation.
- Integration test exercising the full flow (invite → add → send → receive) across two daemons via their IPC sockets, using the 1.E mocked-transport harness. Separate `#[ignore]`-gated real-Arti variant proves end-to-end composition.

Out of scope (owned by later sub-projects or deferred):

- Windows named-pipe IPC. 1.F returns a clear "Windows IPC not yet implemented" error on `cfg!(windows)`. Tracked as a post-Phase-1 follow-up.
- Multi-member groups / `Command::CreateGroup`. Variant stays in the enum; server answers `IpcError::UnknownCommand` until Phase 2.
- FTS5 message search — 1.G.
- Mailbox / offline delivery — Phase 2.
- TUI chat (scrollback, line editing, syntax highlighting) — Phase 2 UI.
- Daemon auto-spawn on stateful commands. Rejected during brainstorming (Q1/C).
- Config hot-reload (SIGHUP). Restart the daemon.
- Passphrase change / credential rotation — Phase 3.

## 2. Decisions locked during brainstorming

| # | Decision | Rationale |
|---|----------|-----------|
| D1 | **Lifecycle model C**: persistent daemon for stateful commands; stateless `init`/`restore`/`backup` stay in-process. No auto-spawn. | Matches `skattr-deep-dives.md` §1.2 "one Daemon owns everything" invariant. Auto-spawn adds IPC edge cases (socket races, duplicate daemons) for marginal UX gain. |
| D2 | **Socket auth = filesystem perms `0600` + `SO_PEERCRED`/`getpeereid` check per accept**. | Belt-and-braces catches misconfigured permissions. App-layer HMAC would not defend against an attacker running as the same UID, and the threat model already trusts process memory (the daemon holds the vault passphrase plaintext). |
| D3 | **IPC wire format = CBOR body prefixed by a 4-byte big-endian u32 length; max body 1 MiB.** | Project-wide locked serialization is CBOR (`ciborium`). A second format for one surface buys nothing and costs round-trip tests and mental overhead. |
| D4 | **Passphrase for `skattr daemon` is read from `/dev/tty` at startup** (echo off). `--passphrase-file <path>` / `$SKATTR_PASSPHRASE_FILE` provide a non-interactive escape hatch. Never a plain env var. | Keeps passphrase off the CLI process memory entirely; matches `ssh-agent` / `gpg-agent` patterns. Plain env vars leak via `/proc/<pid>/environ`. |
| D5 | **Inline `SendMessage` wait = 2 s**; on timeout the handler returns `SendStatus::Queued` and later progress arrives via `Event::DeliveryStatusChanged`. | Real Arti RTT is 1–3 s; 2 s keeps the CLI snappy without forcing the caller to lie about success. |
| D6 | **Migration 0005 adds `group_id BLOB NOT NULL DEFAULT X''` to `contacts`** and indexes it. `AddContact` writes the column atomically with the contact row. | Every command that crosses "pubkey → MLS group" needs this edge. Separate join table was rejected as extra indirection; deterministic derivation was rejected as it requires retrofitting `Group::create_solo`. |
| D7 | **Two-layer error model**: stable `DaemonErrorKind` enum on the wire; rich `CoreError` stays server-side and is projected via `CoreError::kind()`. Unmapped errors become `IpcError::Internal(truncated)` with the full chain logged at `warn`. | Stops internal schema / Rust type names from leaking to CLI scripts; keeps the wire surface forward-compatible. |
| D8 | **`chat` is `Subscribe + repeated Execute` on one socket**, not a distinct protocol. | Keeps the IPC state machine small: one per-connection state flag (`subscribed: bool`) covers both modes. |
| D9 | **Command surface grows by `ListContacts` and `RecentMessages`; `CreateGroup` stays in the enum but is wire-rejected in Phase 1.** | Phase 2 reuses the variant; no churn needed. |

## 3. Architecture

```
┌───────────────────────┐                 ┌─────────────────────────────────────┐
│  skattr (CLI)         │                 │  skattr daemon                      │
│  one-shot             │  UnixStream     │  long-running                       │
│  clap dispatch        │◀──CBOR+len u32─▶│  ┌──────────────────────────┐       │
│  ipc::client::Client  │                 │  │ Daemon                   │       │
└───────────────────────┘                 │  │   Arc<Pool>              │       │
                                          │  │   Arc<DeliveryHub>       │       │
┌───────────────────────┐                 │  │   IdentityKey            │       │
│  skattr init/restore  │  in-process     │  │   TorRuntime             │       │
│  /backup              │  (no daemon)    │  │   OnionListener          │       │
└───────────────────────┘                 │  │   events: broadcast      │       │
                                          │  └──────────────────────────┘       │
                                          │                                     │
                                          │  ipc::server::Server                │
                                          │    UnixListener (0600)              │
                                          │    parent dir 0700                  │
                                          │    peer-cred check per accept       │
                                          │    task-per-connection              │
                                          └─────────────────────────────────────┘
```

Two processes communicating via one socket. One new `daemon::ipc::` submodule owns the wire surface; command dispatch lives in a new `daemon::dispatch` module that consumes an `Arc<DaemonHandle>` (a slim grouping of the subsystems each command touches).

## 4. IPC wire protocol

Frames are length-prefixed CBOR. Shared codec in `daemon::ipc::codec`:

```
┌─────────┬──────────────┐
│ len u32 │  CBOR body   │
│ (BE)    │  len bytes   │
└─────────┴──────────────┘

max body = 1 MiB    // IPC is local; Envelopes larger than this still go via DeliveryHub
```

Two wire types (in `daemon::ipc::wire`):

```rust
pub enum IpcRequest {
    Execute(Command),
    Subscribe(EventFilter),
    Shutdown,
}

pub enum IpcResponse {
    Ok(CommandResult),
    Err(IpcError),
    Event(Event),     // only after a Subscribe; zero or more before Bye
    Bye,              // terminal frame
}

pub enum EventFilter {
    All,
    Contact(PublicKey),    // MessageReceived + DeliveryStatusChanged filtered to this peer
    TorStatus,
}
```

### Per-connection state machine

```
  ACCEPT
    │
    │  SO_PEERCRED/getpeereid check
    ▼
  ┌───────────────┐  Execute(cmd)      ┌───────────────┐
  │ IDLE          │───────────────────▶│ DISPATCHING   │
  │ (just opened) │                    └──────┬────────┘
  └───┬───────────┘                           │ Ok|Err
      │                                       ▼
      │  Subscribe(filter)               write response
      │                                       │
      ▼                                       ▼
  ┌───────────────┐                     ┌───────────────┐
  │ SUBSCRIBED    │  Event(..) stream  │ IDLE          │
  │ + can also    │─────────────────▶  │ (allow more   │
  │   Execute(..) │                    │   Execute)    │
  └───────┬───────┘                    └───────────────┘
          │
          │ EOF from client, or Shutdown, or daemon stop
          ▼
       write Bye, close
```

A single socket connection serves `chat` cleanly: one `Subscribe(Contact(peer))` followed by any number of `Execute(SendMessage { .. })`. The server merely tracks a `subscribed: Option<EventFilter>` flag per connection task.

### Framing rules

- Writes are always `u32_be(len) || body`.
- Reads that exceed 1 MiB close the connection with `IpcError::FrameTooLarge { got, max }`.
- Zero-length frames are a protocol error (not silent), closed with `IpcError::Codec("empty frame")`.
- Malformed CBOR returns `IpcError::Codec(<ciborium display>)` on the same connection; the server does not proactively close.

## 5. Daemon lifecycle

The current `Daemon::run` only brings up Tor + the onion listener. 1.F expands it to the full-ownership shape the deep-dives doc prescribes:

```rust
pub async fn run(
    data_dir: &Path,
    passphrase: &Zeroizing<String>,
    config: Config,                              // NEW param
    ready_tx: oneshot::Sender<Ready>,            // NEW shape: { onion, ipc_socket }
    shutdown_fut: impl Future<Output = ()> + Send,
) -> Result<(), CoreError> {
    // 1. Unlock vault; derive (identity_key, storage_seed); drop vault.
    // 2. Open encrypted Pool; run migrations (including 0005).
    // 3. Tor bootstrap + onion publish (existing).
    // 4. OnionListener, AuthenticatedConnection accept loop.
    // 5. Build Arc<DeliveryHub> wired to Pool + listener + identity; spawn its run-task.
    // 6. Create broadcast::channel::<Event>(256).
    // 7. Bind ipc::Server at config.ipc_socket_or_default(); peer-cred allow-uid = geteuid().
    //    - mkdir_p parent with mode 0700; ensure socket is 0600; remove stale file first.
    // 8. Spawn ipc_server.serve(DaemonHandle { pool, hub, identity, events_tx }).
    // 9. ready_tx.send(Ready { onion, ipc_socket }).
    // 10. shutdown_fut.await.
    // 11. Graceful teardown: abort IPC server → drain DeliveryHub → Pool drops → remove socket file.
}
```

`DaemonHandle` (new, in `daemon::handle`) is the only structure command handlers see; it deliberately exposes only what 1.F needs:

```rust
pub(crate) struct DaemonHandle {
    pub pool: Arc<Pool>,
    pub hub: Arc<DeliveryHub>,
    pub identity: IdentityKey,                      // Ed25519; Noise static key derived on demand
    pub events_tx: broadcast::Sender<Event>,
}
```

Signal handling: `skattr daemon` installs a SIGINT/SIGTERM handler that completes the `shutdown_fut`. No `--detach` mode in 1.F; running the daemon in the background is the operator's job (`systemd --user`, `nohup`, etc.). The `--detach` flag from the bootstrap prompt is deferred with a `TODO` and a spec reference.

## 6. Command surface

Full set after 1.F. New variants marked (+); existing ones are the enum already in `crates/core/src/daemon/commands.rs`.

```rust
pub enum Command {
    // 1.D flow
    CreateInvite { nickname: Option<String>, ttl_secs: Option<u64> },   // ttl_secs added
    AddContact   { invite_url: String },

    // 1.F additions
    ListContacts,                                                        // (+)
    RecentMessages { contact: Option<PublicKey>, limit: u32 },           // (+)

    // 1.E flow
    SendMessage { contact: PublicKey, kind: Kind },

    // reserved for Phase 2+
    CreateGroup { members: Vec<PublicKey>, name: String },

    Shutdown,
}

pub enum CommandResult {
    InviteCreated { url: String, key_package_id: Hex32, expires_at: u64 },
    ContactAdded(ContactSummary),
    Contacts(Vec<ContactSummary>),                                       // (+)
    MessageSent { message_id: Hex16, status: SendStatus },
    Messages(Vec<MessageRecord>),                                        // (+)
    Subscribed,                                                          // ack for Subscribe
    ShuttingDown,
}

pub enum SendStatus { Queued, Delivered }
pub enum Direction  { Incoming, Outgoing }

pub struct ContactSummary {
    pub pubkey: PublicKey,
    pub nickname: Option<String>,
    pub onion: String,
    pub card_version: u64,
    pub added_at: u64,
}

pub struct MessageRecord {
    pub message_id: Hex16,
    pub contact: PublicKey,
    pub direction: Direction,
    pub kind: Kind,                 // reuses envelope::Kind
    pub mls_generation: u64,
    pub ts_daemon_recv: u64,        // authoritative local-clock receive ts
    pub ts_envelope: i64,           // sender-claimed ts (display only, per CLAUDE.md)
}
```

`Hex16` / `Hex32` are thin newtypes over `[u8; 16]` / `[u8; 32]` with `Display` / `FromStr` for lowercase hex and `serde` round-trip. The CLI accepts prefix matches for `Hex32` contact pubkeys; ambiguous prefixes return `DaemonErrorKind::ContactAmbiguous { matches }`.

### Dispatch invariants per command

| Command | Dispatch path |
|---|---|
| `CreateInvite` | Generate 32-byte PSK; `KeyPackageRepo::create_fresh`; `InviteLink::generate`; return `.to_url()`. Single-use tracking already handled by `KeyPackageRepo`. |
| `AddContact` | `InviteLink::from_url` → verify signature + TTL → open MLS group via `Group::create_solo` + `add_member` + Welcome → `mark_consumed` on the inviter's KeyPackage → write `contacts` row with `group_id` in one transaction → broadcast `Event::ContactUpdated`. |
| `ListContacts` | `ContactRepo::list` → project each to `ContactSummary`. |
| `SendMessage` | Resolve contact (full pubkey or unambiguous prefix) → load MLS group via `contacts.group_id` → `Group::encrypt(Envelope { kind, ts: now() })` → `OutboxRepo::insert` → `DeliveryHub::send` with `tokio::time::timeout(2s, ..)`. |
| `RecentMessages` | `MessageRepo::recent(contact, limit)` → project to `MessageRecord`. Ordering: `(mls_generation DESC, ts_daemon_recv DESC)`. Never sorted by `ts_envelope`. |
| `Shutdown` | Write ack, trigger the daemon's shutdown future. |

## 7. MLS group ↔ contact mapping — migration 0005

```sql
-- crates/core/src/storage/migrations/0005_contact_group_link.sql
ALTER TABLE contacts ADD COLUMN group_id BLOB NOT NULL DEFAULT X'';
CREATE INDEX IF NOT EXISTS idx_contacts_group_id ON contacts(group_id);
```

- Phase 1 has no production users; the empty default is cosmetic. `AddContact` is the only code path that inserts contacts and it always writes a real `group_id`.
- Rollback plan: drop the column via a future migration; no data to preserve yet.
- Fsck helper: a `contacts::verify_group_links` function (unit-tested) that asserts every row's `group_id` resolves via `MlsGroupRepo::get`. Not wired to a user-facing command in 1.F.

## 8. Config schema and precedence

```toml
# ~/.config/skattr/config.toml — all keys optional

[daemon]
data_dir   = "~/.local/share/skattr"                    # default: dirs::data_dir().join("skattr")
ipc_socket = "/run/user/1000/skattr/daemon.sock"        # default: ${XDG_RUNTIME_DIR:-$TMPDIR}/skattr/daemon.sock

[cli]
default_contact = "ab12cd34..."                         # hex Ed25519 pubkey; used if send/tail omit <contact>
```

Precedence: **CLI flag > env (`SKATTR_DATA_DIR`, `SKATTR_SOCKET`) > config file > built-in default.**

`Config::load(path: Option<&Path>)`:
- `None` → try `$XDG_CONFIG_HOME/skattr/config.toml`, then `$HOME/.config/skattr/config.toml`, then return defaults. Absence is not an error.
- `Some(p)` and `p` missing → hard error with the attempted path.
- Invalid TOML or unknown keys → hard error with line/column from `toml`. Unknown keys fail loud in 1.F; if future compat becomes a problem we add `#[serde(deny_unknown_fields = false)]` with an ADR.

Socket path rules:
- Parent directory created with mode `0700` if absent.
- Socket itself bound with mode `0600` immediately after bind.
- Stale socket file (from a crashed prior daemon) is unlinked first; if binding still fails the daemon aborts with a clear error naming the path and the conflicting PID when discoverable via `fuser`-style probe (best-effort; no hard dependency on `psutil`).

## 9. Error model

```rust
// daemon::ipc::wire
pub enum IpcError {
    AuthDenied,                               // peer-cred mismatch; should be unreachable
    Codec(String),                            // CBOR decode failure; server keeps connection open
    FrameTooLarge { got: u32, max: u32 },
    UnknownCommand,                           // forward-compat (e.g., CreateGroup pre-Phase-2)
    VaultNotReady,                            // daemon still booting; retry
    Daemon(DaemonErrorKind),                  // typed library error
    Internal(String),                         // last resort; truncated; full detail stays in logs
}

pub enum DaemonErrorKind {
    ContactNotFound,
    ContactAmbiguous { matches: u32 },
    InviteExpired,
    InviteConsumed,
    InviteSignatureInvalid,
    GroupCorrupt,
    DeliveryTimeout,                          // inline wait expired; not a permanent failure
    TorNotReady,
    StorageError,
}
```

`CoreError::kind(&self) -> Option<DaemonErrorKind>` is the projection adapter. Unmapped variants become `IpcError::Internal(display_truncated_at_256_bytes)`; the full `CoreError` (with source chain, CBOR dump, SQL context) is recorded server-side at `tracing::warn!` with connection id and command name but never any pubkeys, message contents, or passphrase state.

## 10. CLI exit codes

Stable, scriptable:

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Generic / unexpected failure |
| 2 | Usage error (clap) |
| 3 | Daemon not running / socket unreachable |
| 4 | `AuthDenied` (peer-cred mismatch) |
| 5 | Vault lock / wrong passphrase (local `init`-family commands) |
| 6 | Contact not found or ambiguous |
| 7 | Invite expired, consumed, or signature invalid |
| 8 | Delivery timeout on `send` (hit when user passed `--fail-on-timeout`; absent flag = exit 0 with `status=queued`) |

Human output maps each `IpcError` variant to a one-line explanation. `--json` instead emits `{ "error": { "kind": "...", "detail": "..." } }` with the same exit code.

## 11. File layout

**New files (all carry GPLv3 headers per CLAUDE.md):**

```
crates/core/src/daemon/ipc/
    mod.rs                     # module declarations + internal re-exports
    wire.rs                    # IpcRequest, IpcResponse, IpcError, DaemonErrorKind, EventFilter
    codec.rs                   # u32-BE length prefix + CBOR encode/decode
    server.rs                  # Server::{bind, serve}; per-conn task; peer-cred helper
    client.rs                  # IpcClient::{connect, execute, subscribe}
crates/core/src/daemon/
    handle.rs                  # DaemonHandle Arc-shareable subsystem grouping
    dispatch.rs                # execute_command(&DaemonHandle, Command) -> Result<CommandResult, CoreError>
crates/core/src/storage/migrations/
    0005_contact_group_link.sql
crates/tests/src/
    cli_ipc_roundtrip.rs       # single-daemon IPC codec + auth + dispatch coverage
    cli_two_daemons.rs         # full invite→add→send→receive flow; mocked transport
    cli_real_tor.rs            # #[ignore]-gated real-Arti variant
```

**Modified files:**

```
crates/cli/src/main.rs                # delete stubs; wire clap subcommands to IpcClient
crates/cli/Cargo.toml                 # + qrcode = "0.14", rpassword = "7"
crates/core/src/daemon/state.rs       # expand Daemon::run; implement execute/send
crates/core/src/daemon/commands.rs    # add ListContacts, RecentMessages, result variants, ContactSummary/MessageRecord/Hex16/Hex32
crates/core/src/daemon/config.rs      # real Config::load + XDG precedence + socket-path defaults
crates/core/src/daemon/mod.rs         # re-export Command/CommandResult/IpcClient as pub API
crates/core/src/storage/contacts.rs   # group_id column; lookup_by_prefix(prefix_hex: &str)
crates/core/src/storage/messages.rs   # recent_by_contact + MessageRecord projection
crates/core/src/storage/mod.rs        # include_str! migration 0005
crates/core/src/error.rs              # CoreError::kind() -> Option<DaemonErrorKind>
crates/core/src/lib.rs                # extend test_exports with IpcClient/Server/wire types
CHANGELOG.md                          # 1.F entry
CLAUDE.md                             # "Phase 1.F done" status line
docs/ARCHITECTURE.md                  # refresh "send one message" trace with IPC hop
```

**New dependencies:**

- `crates/core`: none. `tokio::net::UnixStream::peer_cred()` is stable on Linux + macOS. `dirs` is already a dep from Phase 0.C.
- `crates/cli`: `qrcode = "0.14"` (ASCII QR via `render::unicode::Dense1x2`), `rpassword = "7"` (`/dev/tty` passphrase prompt, replacing the current stdin `TODO` at `main.rs:178`).

Both new crates are MIT-OR-Apache-2.0; no `cargo-deny` additions needed.

## 12. Testing strategy

### Unit tests (co-located `#[cfg(test)]` modules)

- **`codec.rs`** — round-trip every `IpcRequest` and `IpcResponse` variant; oversize-frame rejection (`FrameTooLarge`); malformed-CBOR rejection (`Codec`); zero-length frame rejection.
- **`wire.rs`** — a deliberately-older `Command` deserializer reading a newer-variant frame returns `IpcError::UnknownCommand` rather than panicking (forward-compat contract).
- **`server.rs`** — `check_peer_uid(ucred: UCred, expected: u32)` helper unit-tested in isolation so the real accept loop stays trivial.
- **`client.rs`** — `connect()` on a missing socket file returns `IpcError::DaemonNotRunning` (maps to CLI exit 3).
- **`dispatch.rs`** — each `Command` variant dispatched against an in-memory `Pool` and a mocked `DeliveryHub` (1.E pattern); assert correct `CommandResult` and emitted `Event`s.
- **`config.rs`** — TOML round-trip; precedence matrix; explicit-missing-path error; unknown-key rejection.
- **CLI (`cli/src/main.rs`)** — extract `parse_args(argv: &[&str]) -> ParsedInvocation` so arg-to-`Command` translation is unit-testable without spawning subprocesses.

### Integration tests

1. **`cli_ipc_roundtrip.rs`** — boot one `Daemon` with a `tempdir` socket path; drive every `Command` variant through `IpcClient`; assert expected `CommandResult`s and `Event`s arrive on a `Subscribe` stream.
2. **`cli_two_daemons.rs`** — adapts the 1.E `delivery_kill_mid_message.rs` harness. Two daemons A + B with independent sockets. Flow: A.`CreateInvite` → B.`AddContact(url)` → A's delivery hub accepts the Welcome → A.`SendMessage(b_pubkey, "hello")` → assert `MessageSent { Delivered }` → B's `Subscribe(Contact(A))` stream yields `Event::MessageReceived` → B.`RecentMessages(contact=A, limit=10)` returns the plaintext. No real Tor; uses mocked transport.
3. **`cli_real_tor.rs`** (`#[ignore]`-gated) — same script as #2 but over real Arti. Counts toward the Phase 1 exit criterion "Two users on different networks exchange messages via CLI" together with `delivery_real_tor.rs`.

CI runs #1 + #2 and must stay under 60 s per run. #3 runs only via `cargo test -p skattr-tests --release -- --ignored`.

## 13. Security notes (implementation-level)

- Socket path lives in `$XDG_RUNTIME_DIR/skattr/` (tmpfs on Linux, cleared on logout) when available; falls back to `$TMPDIR/skattr/` with the same `0700` parent and `0600` socket.
- Every accepted connection calls `tokio::net::UnixStream::peer_cred()`; UID mismatch returns `IpcError::AuthDenied` and closes. Logged at `warn` with connection id and offending peer UID. No pubkeys, message contents, or passphrase state ever cross the log level `info`.
- No passphrase or key material crosses the IPC boundary in either direction. `SendMessage { kind: Kind::Text { body } }` carries application-layer plaintext by design; MLS/Noise/vault keys stay in-process on the daemon.
- The stable `DaemonErrorKind` enum is the only error surface CLI clients see; `IpcError::Internal(String)` is a 256-byte-truncated placeholder and the rich `CoreError` (with source chain, CBOR dumps, SQL context) stays server-side.
- `ts_envelope` is display-only (CLAUDE.md constraint). `tail` renders both `ts_envelope` and `ts_daemon_recv` but ordering is `(mls_generation DESC, ts_daemon_recv DESC)`.
- `--passphrase-file` opens the file, reads into a `Zeroizing<String>`, trims exactly one trailing `\n`, closes. `$SKATTR_PASSPHRASE_FILE` points to the *path*, never the passphrase itself.
- CLI never touches the vault directly: `init`/`restore`/`backup` open the vault in-process, but those commands run before any daemon exists. Once a daemon is up, no CLI command can read or modify the vault file.

## 14. Open questions / follow-ups

Not blocking 1.F; tracked here so the implementation plan can skip re-deciding them.

- **Windows named-pipe IPC.** Post-Phase-1 follow-up. Needs separate auth story (no `SO_PEERCRED`); likely token-file + perms.
- **`skattr daemon --detach`.** The bootstrap prompt prescribes it but 1.F ships without. `TODO` comment in the daemon subcommand handler points here.
- **Socket path collision detection via `/proc/net/unix`.** Best-effort PID hint on bind failure; treat as a UX polish item, not a correctness concern.
- **Config hot-reload.** Deferred; no SIGHUP handler in 1.F.
- **Structured-log JSON export.** Separate concern from `--json` CLI output. Use `tracing-subscriber`'s existing JSON layer if a user passes `RUST_LOG_FORMAT=json`; not 1.F scope.
- **`skattr invite --qr --file <path>`.** ASCII-only in 1.F. PNG output is a trivial future addition.
- **Alias management (`skattr contacts rename`, etc.).** Phase 2 UX work; 1.F ships without nickname editing.

## 15. Phase 1.F exit criteria (verifiable)

All of the following must be green before this sub-project merges to master:

1. `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` all pass on Linux and macOS CI.
2. `cargo test -p skattr-tests cli_two_daemons` exercises the full invite → add → send → receive flow end-to-end.
3. `cargo test -p skattr-tests cli_real_tor -- --ignored` passes on at least one developer machine (not CI-gated).
4. Running `skattr daemon` on one shell and `skattr invite && skattr send <pk-prefix> hello && skattr tail` on another produces the message on both sides.
5. Unix socket is created at `$XDG_RUNTIME_DIR/skattr/daemon.sock` with mode `0600`, parent `0700`, and is removed on clean shutdown.
6. `cargo deny check` passes with no new advisories, no banned crates, no license exceptions for `qrcode`/`rpassword`.
7. CHANGELOG.md, CLAUDE.md status line, and `docs/ARCHITECTURE.md`'s "send one message" trace are updated.
