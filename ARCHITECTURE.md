# Architecture

A high-level tour of the codebase. Pair this with [`docs/skattr-design.md`](docs/skattr-design.md) (protocol semantics) and [`docs/skattr-deep-dives.md`](docs/skattr-deep-dives.md) (module-level detail).

## Workspace layout

```
skattr/
├── crates/
│   ├── core/        # library: protocol, transport, storage, state
│   ├── mailbox/     # AGPLv3 server binary — offline delivery
│   ├── cli/         # clap-based CLI over the daemon
│   └── tests/       # cross-crate integration tests
└── docs/            # design + ADRs + protocol spec
```

The `ui/` crate is deliberately **not** scaffolded in Phase 0. Tauri 2 + SvelteKit lands in Phase 2 once `core::daemon` exposes a stable command/event API.

## Crate dependency graph

```
                            ┌───────────────┐
                            │  crates/cli   │
                            └───────┬───────┘
                                    │
  ┌──────────────┐          ┌───────▼───────┐
  │ crates/tests │──────────▶  crates/core  │
  └──────────────┘          └───────┬───────┘
                                    │
                            ┌───────▼────────┐
                            │ crates/mailbox │  (shares core::mailbox::protocol)
                            └────────────────┘
```

- `core` depends on nothing in this workspace.
- `mailbox` depends on `core` **only** for shared wire-protocol types (`core::mailbox::protocol`). It does not pull in identity, MLS, delivery, or daemon code.
- `cli` depends on `core` to instantiate and drive a `Daemon`.
- `tests` depends on `core` and spawns daemons in-process for end-to-end tests.

## Inside `crates/core`

Modules, top-down:

```
daemon              long-lived process: owns Tor runtime, storage pool, delivery hub,
                    listener, IPC server + dispatch — done Phase 1.F
  ├── delivery      per-peer actor hub, outbox + exp-backoff retry, receiver
  │                 dedupe + ACK — done Phase 1.E
  ├── contact       contacts and signed ContactCards, monotonic-version
  │                 persistence — done Phase 1.D
  ├── mailbox       client of the mailbox server (deferred to Phase 2)
  ├── invite        signed skattr://invite/v1# links, QR rendering — done Phase 1.D
  ├── mls           2-member MLS groups, state machine, keystore, Welcome/Commit —
  │                 done Phase 1.C
  ├── envelope      CBOR application payloads flowing inside MLS — done Phase 1.A
  ├── transport     tor + HS key + accept loop, frame codec, Noise_XK handshake +
  │                 AuthenticatedConnection — done Phase 0.C, 1.A, 1.B
  ├── identity      Ed25519 keypair, BIP39, Argon2id + XChaCha20-Poly1305 vault,
  │                 HKDF derivations — done Phase 0.B
  └── storage       Pool (age-encrypted), migrations runner (through 0006), 8
                    repos (incl. read_state), backup — done Phase 0.D + extended
                    in 1.C/1.D/1.E/1.F/1.G
```

**Public API boundary.** Only `daemon`, `error`, `envelope`, `invite`, `contact`, and the key types in `identity` are `pub`. Everything else is `pub(crate)`. Consumers (`cli`, `ui`) talk to the daemon through `Command` / `Event` enums.

## "skattr send <contact> <text>" end-to-end trace

1. **CLI process** parses argv via `clap`; resolves the IPC socket path (`--socket` > `$SKATTR_SOCKET` > `$XDG_RUNTIME_DIR/skattr/daemon.sock`).
2. CLI `UnixStream::connect`s the socket; daemon's IPC server accepts, calls `SO_PEERCRED`, rejects non-matching UIDs with `IpcError::AuthDenied`.
3. CLI resolves the positional contact via `Command::ListContacts` → prefix match (hex pubkey or nickname).
4. CLI sends `Command::SendMessage { contact, kind: Kind::Text { body } }` as a length-prefixed CBOR frame.
5. Daemon's `dispatch::send_message`:
   a. reads `contacts.group_id` for the peer (migration 0005 column),
   b. loads the MLS `Group` via `Group::load(&group_id, &group_repo)`,
   c. builds an `Envelope { v, id, ts, reply_to, kind }` with a fresh `MessageId::generate()`,
   d. `Group::encrypt(&env)` → ciphertext (MLS ratchet advances once),
   e. `group.save(&group_repo)` to persist the advanced ratchet,
   f. `OutboxRepo::insert(target, message_id, ciphertext, next_retry_at)` (idempotent),
   g. `DeliveryHub::send(peer, message_id, ciphertext)` kicks the per-peer actor.
6. `PeerConnection` actor either uses its live `AuthenticatedConnection<DataStream>` or dials the peer's cached onion, completes `Noise_XK` handshake, sends a length-prefixed `FrameType::MlsApp` frame.
7. Remote peer's `OnionListener` accepts the stream; post-handshake, the stream enters the remote `DeliveryHub`. `delivery::receiver::receive` decrypts via the MLS group, captures `mls_generation = group.epoch()` and `ts_daemon_recv = now_unix_seconds()`, and `MessageRepo::insert(InsertParams)` persists the row. The FTS5 `au`/`ai` triggers index `body_text` automatically. After persist, `DaemonInbound` projects a `MessageRecord` and broadcasts `Event::MessageReceived { contact, record }` on the daemon's broadcast bus, where `tail --follow` subscribers (with `EventFilter::Messages { contact }`) pick it up.
8. Remote CLI running `skattr tail --follow` or `skattr chat` receives the event frame and prints the plaintext.
9. Remote `PeerConnection` sends back `FrameType::Ack { message_id }`; the sender's oneshot resolves, local `dispatch::send_message`'s `tokio::time::timeout(2s, ..)` completes with `SendStatus::Delivered`, `CommandResult::MessageSent { status: Delivered }` is written to the CLI's IPC socket.
10. CLI prints `<message_id>  delivered` and exits 0.

**Current phase state.**

- `Daemon::run` takes `Config`, bootstraps Tor, publishes the onion,
  starts the IPC server, and signals `Ready { onion, ipc_socket }`.
  All `Command`/`Event` variants are wired through `dispatch::execute_command`.
- `mls::group` covers 2-member groups (create, add, encrypt/decrypt,
  persist). Group chat >2 members is Phase 3.
- `delivery::{hub, peer, outbox, receiver, backoff}` are fully
  wired; kill-mid-message → reconnect → exactly-once delivery is
  exercised by a CI integration test.
- `transport::{frame, noise, connection, listener, tor, hs_key}`
  are fully wired.
- `invite::InviteLink` + `contact::ContactCard` parse/verify/sign/
  persist with single-use enforcement on KeyPackages.
- `mailbox::client` is `todo!()` — offline delivery is Phase 2.
- `storage::*` is fully implemented (migrations through 0006).
  `MessageRepo` provides FTS5 search, unread/mark-read, export
  pagination, and retention pruning; `ReadStateRepo` tracks per-group
  last-read cursors. The daemon runs `backfill_body_text` once at
  startup and an hourly `daemon::retention` sweep driven by
  `[history] retention_days`.

## Cross-cutting: transport↔MLS binding

The Noise handshake hash is mixed into the first MLS Commit as an external PSK:

```
h_transport = HKDF(noise_handshake_hash, "skattr-binding-v1")
```

This prevents an attacker who somehow obtained MLS state from replaying it over a different Noise session. Any refactor touching either layer must preserve this invariant — it is the reason the two layers are not modeled as independent.

## State that survives restart

- `~/.local/share/skattr/identity.vault` — passphrase-encrypted
  Ed25519 identity seed (Argon2id + XChaCha20-Poly1305).
- `~/.local/share/skattr/hs.key.age` — age-encrypted v3 HS signing
  key. Key: `HKDF(storage_seed, "skattr-hs-storage-v1")`.
- `~/.local/share/skattr/skattr.sqlite.age` — age-encrypted SQLite
  database (contacts, messages, groups, outbox, mailboxes,
  seen_messages, schema_version). Key: `HKDF(storage_seed,
  "skattr-storage-v1")`. While the daemon is running, a plaintext
  `skattr.sqlite` sidecar exists; it's re-encrypted and removed on
  clean shutdown.
- `~/.local/share/skattr/arti/` — Arti's state directory
  (circuits, guards, HS keystore). Mode 0700.

The design deliberately uses two separate keypairs (identity vs. onion service — see design §1.1). Losing the onion key means changing address; losing the identity key means losing the identity.

## Where work lands by phase

| Phase | Modules that change | Exit criterion |
|-------|--------------------|----------------|
| 0.A ✅ | Workspace scaffold | Scaffold compiles, clippy clean, tests empty |
| 0.B ✅ | identity | skattr init/restore work, vault round-trips |
| 0.C ✅ | transport::{tor, hs_key, listener} | Two daemons echo bytes over Tor (integration test) |
| 0.D ✅ | storage, daemon::backup | Pool + 7 repos + backup/restore-backup CLI |
| 0.E ✅ | docs | THREAT_MODEL.md, OPERATIONS.md, ARCHITECTURE.md refresh, README update |
| 1.A–1.E ✅ | mls, delivery, transport::{noise, frame, connection} | Kill-mid-message → reconnect → exactly-once delivery (CI) |
| 1.F ✅ | daemon::{ipc, dispatch}, cli | `skattr send` through IPC; `cli_ipc_roundtrip` + `cli_two_daemons` CI |
| 1.G ✅ | storage::messages (FTS5), storage::read_state, daemon::retention, cli::{search,export,prune} | Full-text search p95 < 50 ms over 100k rows; history retention + export |
| 2 | mailbox (client + server), contact::{card, rotation}, ui | Offline user receives queued messages; Tauri UI shell |
| 3 | mls (multi-member), delivery::fanout, envelope::kinds (attachments, reactions, edits) | 50-member group passes week-long soak |
| 4 | hardening — fuzz harnesses, padding, duress mode, reproducible builds | External audit clean |
| 5 | distribution — signing, updates, docs | Public release |
