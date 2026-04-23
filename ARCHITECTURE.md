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
                    listener (Daemon::run shipped; Command/Event wiring is 1.F)
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
  └── storage       Pool (age-encrypted), migrations runner (through 0004), 7
                    repos, backup — done Phase 0.D + extended in 1.C/1.D/1.E
```

**Public API boundary.** Only `daemon`, `error`, `envelope`, `invite`, `contact`, and the key types in `identity` are `pub`. Everything else is `pub(crate)`. Consumers (`cli`, `ui`) talk to the daemon through `Command` / `Event` enums.

## Data flow: sending one message

End-to-end trace of the 1.E delivery stack. The `Command::SendMessage`
CLI wiring is still 1.F, but tests drive the same path today via
`test_exports::DeliveryHub`:

```
1. Caller encrypts an Envelope (MessageId, ts, kind) via the peer's
   MLS group (mls::group::Group::encrypt → ciphertext). The MLS
   ratchet advances exactly once per message.
2. storage::outbox::OutboxRepo.insert persists (target, message_id,
   ciphertext) with next_retry_at = now. INSERT OR IGNORE keeps the
   call idempotent over (target, message_id).
3. delivery::hub::DeliveryHub.send routes to the per-peer
   delivery::peer::PeerConnection actor, spawning one on first use.
4. The actor owns an Option<AuthenticatedConnection<S>>:
     - If None, dial via transport::tor::TorRuntime::connect and
       run transport::noise::handshake_initiator to populate it.
     - conn.send(Frame::MlsApp(ciphertext)) — frame-in-frame through
       the Noise transport cipher.
5. Actor registers a oneshot in pending_acks[MessageId] and waits.
6. On inbound Frame::Ack(id), actor resolves the oneshot,
   OutboxRepo.ack_by_message_id drops the row, and the Daemon emits
   Event::DeliveryStatusChanged { status: Delivered }.
7. On conn error / kill-mid-message, pending_acks are drained with
   Err(()); the outbox row stays. The actor's 1 s retry tick picks
   it up, redials if needed, re-sends the same ciphertext (no
   re-encrypt), and reschedules with exponential backoff (1s → 5min
   cap, ±25% jitter).
```

The inverse (receiving):

```
1. transport::listener accepts an onion connection; the post-handshake
   AuthenticatedConnection is handed to DeliveryHub::ingest.
2. The per-peer actor's select! arm on conn.recv() observes a
   Frame::MlsApp and passes the ciphertext to the injected
   InboundDispatch (mls::group::Group::decrypt → Envelope).
3. delivery::receiver::receive enforces a ±1 h ts window, checks
   storage::seen_messages::SeenMessagesRepo for (sender,
   message_id), and on fresh insert calls
   storage::messages::MessageRepo.insert.
4. Actor replies with Frame::Ack(message_id) whether the receive
   was New or Duplicate (duplicate ACK lets the sender's retry
   loop clear its outbox row).
5. Daemon emits Event::MessageReceived.
```

**Current phase state.**

- `Daemon::run` bootstraps Tor, publishes the onion, and blocks on
  shutdown. `Daemon::send` is a `pub(crate)` stub until 1.F wires
  `Command::Send` into the hub.
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
- `storage::*` is fully implemented (migrations through 0004).

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
| 1 | mls, delivery, transport::{noise, frame, connection}, daemon::state session manager | Two CLI users exchange E2EE messages over real Tor |
| 2 | mailbox (client + server), contact::{card, rotation}, ui | Offline user receives queued messages; Tauri UI shell |
| 3 | mls (multi-member), delivery::fanout, envelope::kinds (attachments, reactions, edits) | 50-member group passes week-long soak |
| 4 | hardening — fuzz harnesses, padding, duress mode, reproducible builds | External audit clean |
| 5 | distribution — signing, updates, docs | Public release |
