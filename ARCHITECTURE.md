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
  ├── delivery      send/receive/dedupe/ack, outbox retry logic
  ├── contact       contacts and signed ContactCards; address rotation
  ├── mailbox       client of the mailbox server
  ├── invite        signed invite links, QR rendering (feature-gated)
  ├── mls           MLS groups, state machine, keystore, Welcomes, Commits
  ├── envelope      CBOR application payloads flowing inside MLS
  ├── transport     framed Noise_XK over Arti Tor streams
  ├── identity      Ed25519 keys, BIP39 seeds, passphrase-encrypted vault
  └── storage       SQLite pool, repos, migrations
```

**Public API boundary.** Only `daemon`, `error`, `envelope`, `invite`, `contact`, and the key types in `identity` are `pub`. Everything else is `pub(crate)`. Consumers (`cli`, `ui`) talk to the daemon through `Command` / `Event` enums.

## Data flow: sending one message

This is the canonical trace a new contributor should understand.

```
1. UI / CLI submits Command::SendMessage { contact, kind }
2. Daemon enqueues an Envelope into storage::outbox
3. delivery::sender picks it up, chooses a path:
     a. Contact is currently connected? → transport::connection.send(MLS_APP)
     b. Contact unreachable? → mailbox::client.deposit() on each of their mailboxes
4. mls::group wraps the Envelope as an MLS application message (AEAD + epoch keys)
5. transport::frame encodes the MLS ciphertext as a length-prefixed frame over Noise
6. On ack (direct path) or successful deposit (mailbox path), outbox entry is removed
7. Daemon emits Event::DeliveryStatusChanged to subscribers
```

The inverse flow, receiving a message:

```
1. transport::listener accepts an onion service connection and runs Noise_XK
2. On MLS_APP frame, mls::group decrypts and yields the Envelope
3. delivery::receiver dedupes (sender, message_id), writes to storage::messages, emits ACK
4. Daemon emits Event::MessageReceived
```

## Cross-cutting: transport↔MLS binding

The Noise handshake hash is mixed into the first MLS Commit as an external PSK:

```
h_transport = HKDF(noise_handshake_hash, "skattr-binding-v1")
```

This prevents an attacker who somehow obtained MLS state from replaying it over a different Noise session. Any refactor touching either layer must preserve this invariant — it is the reason the two layers are not modeled as independent.

## State that survives restart

- `~/.local/share/skattr/identity.vault` — passphrase-encrypted identity keypair.
- `~/.local/share/skattr/skattr.sqlite` — contacts, onion addresses, MLS group state blobs, messages (with FTS5), outbox, mailbox registrations. Application-level encrypted via `age` with a seed-derived key.
- `~/.local/share/skattr/arti/` — Arti's state: circuits, hidden service keys.

The design deliberately uses two separate keypairs (identity vs. onion service — see design §1.1). Losing the onion key means changing address; losing the identity key means losing the identity.

## Where work lands by phase

| Phase | Modules that change | Exit criterion |
|-------|--------------------|----------------|
| 0 | identity, storage, transport::tor, cli (skeleton) | Two daemons echo bytes over Tor |
| 1 | transport::{noise,frame,connection,listener}, mls, envelope, invite, delivery | Two CLIs exchange E2EE messages over real Tor |
| 2 | mailbox (client + server), contact::card, contact::rotation, ui | Offline user receives queued messages |
| 3 | mls (multi-member), delivery::fanout, envelope::kinds (attachments, reactions, edits) | 50-member group passes week-long soak |
| 4 | hardening — fuzz harnesses, padding, duress mode, reproducible builds | External audit clean |
| 5 | distribution — signing, updates, docs | Public release |
