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
daemon              long-lived process: owns Tor runtime, storage pool, outbox, listener
                    (Phase 1 — session manager, command/event API)
  ├── delivery      send/receive/dedupe/ack, outbox retry logic (Phase 1)
  ├── contact       contacts and signed ContactCards; address rotation (Phase 1)
  ├── mailbox       client of the mailbox server (Phase 1)
  ├── invite        signed invite links, QR rendering (feature-gated) (Phase 1)
  ├── mls           MLS groups, state machine, keystore, Welcomes, Commits (Phase 1)
  ├── envelope      CBOR application payloads flowing inside MLS (Phase 1)
  ├── transport     tor + HS key + accept loop implemented; Noise/frame/connection
  │                 stubbed for Phase 1
  ├── identity      Ed25519 keypair, BIP39, Argon2id + XChaCha20-Poly1305 vault,
  │                 HKDF derivations — all done Phase 0.B
  └── storage       Pool (age-encrypted), migrations runner, 7 repos, backup — all
                    done Phase 0.D
```

**Public API boundary.** Only `daemon`, `error`, `envelope`, `invite`, `contact`, and the key types in `identity` are `pub`. Everything else is `pub(crate)`. Consumers (`cli`, `ui`) talk to the daemon through `Command` / `Event` enums.

## Data flow: sending one message

End-to-end trace of `Command::SendMessage { contact, kind: Text { ... } }`
once Phase 1 wires the message-send path in:

```
1. UI/CLI submits Command::SendMessage via the Daemon's public API.
2. Daemon looks up the contact's MLS group state (storage::groups::MlsGroupRepo).
3. mls::group wraps the Envelope as an MLS application message (AEAD).
4. storage::outbox::OutboxRepo enqueues the ciphertext with next_retry_at = now.
5. delivery::sender picks up the due entry:
     a. Peer currently online? → transport::connection::send (MLS_APP over
        Noise_XK over arti_client::DataStream, dialled via TorRuntime::connect).
     b. Peer unreachable? → mailbox::client deposits to each of the contact's
        registered mailboxes in parallel.
6. storage::messages::MessageRepo.insert records a local copy for history.
7. On ACK (direct path) or successful DEPOSIT (mailbox path):
     - outbox entry removed
     - messages.delivered_at updated
     - Daemon emits Event::DeliveryStatusChanged to subscribers.
```

The inverse (receiving):

```
1. transport::listener accepts an onion connection; Noise_XK auths the peer.
2. An MLS_APP frame arrives. mls::group decrypts → Envelope.
3. storage::seen_messages dedupes on (sender, message_id).
4. storage::messages::MessageRepo.insert persists the envelope.
5. Daemon emits Event::MessageReceived.
6. Receiver sends an ACK frame back.
```

**Current phase state.** Everything above "Phase 1 wires" is stubbed;
Phase 0 delivered the building blocks. Concretely:

- `Daemon::run` exists as the daemon entry point but does not yet
  accept `Command::SendMessage` — it just bootstraps Tor, publishes,
  and blocks on Ctrl-C.
- `mls::*` is all `todo!()` — Phase 1 work.
- `delivery::sender` is `todo!()` — Phase 1 work.
- `transport::{connection, noise, frame}` are `todo!()` — Phase 1 work.
- `storage::*` is fully implemented and unit-tested; repos are just
  not called yet.
- `transport::{tor, listener, hs_key}` are fully implemented.

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
