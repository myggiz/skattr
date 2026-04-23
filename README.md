# Skattr

> Messages, scattered.

Skattr is a desktop-first, metadata-resistant, end-to-end encrypted messenger built on Tor v3 onion services and MLS (RFC 9420). It has no phone number, no email signup, and no central account server. Identity is a keypair backed by a BIP39 seed phrase.

**Status: Phase 1 on-line messaging almost complete.** Phase 0
delivered identity, at-rest encryption, Arti integration, and
storage. Phase 1.A–1.E have since landed the wire-format codec,
Noise_XK handshake, MLS 2-member groups, invite & contact flow,
and the delivery layer (per-peer actor connection pool, outbox
with exponential-backoff retry, ACK correlation, receiver dedup).
Phase 1.F (CLI integration — `skattr send`, `tail`, `contacts`)
and Phase 1.G (message history + full-text search) are the final
Phase 1 steps before end-to-end chat is user-visible. See
[ARCHITECTURE.md](ARCHITECTURE.md) and [`docs/`](docs/) for the
full design.

## What Skattr is

- **Peer-to-peer.** Clients reach each other directly via Tor onion services. No central relay.
- **Metadata-resistant.** Tor hides network-level metadata; a semi-trusted "mailbox" handles offline delivery and learns only that *someone* has pending ciphertext for *some* identity hash.
- **Group-ready.** Built on [MLS](https://datatracker.ietf.org/doc/rfc9420/). 1:1 is a 2-member group; groups scale to ~50 members in v1.
- **Desktop first.** Native app via Tauri (arriving in Phase 2). A CLI ships alongside for power users and scripting.
- **Rust, all the way down.** Tor via [Arti](https://gitlab.torproject.org/tpo/core/arti), MLS via [OpenMLS](https://openmls.tech/), transport auth via [Noise_XK](https://noiseprotocol.org/) (through `snow`).

## What Skattr isn't

- Not a feature-equivalent Signal replacement (no phone numbers, no SMS, no voice/video in v1).
- Not a low-latency chat (Tor round-trips cost seconds, not milliseconds).
- Not mobile in v1. Mobile is post-1.0 at the earliest.
- Not "anonymous" — your contacts know who you are. It's metadata-resistant, not identity-destroying.

## What works now (through Phase 1.E)

- Create and restore a BIP39-backed identity (`skattr init` /
  `skattr restore`).
- Encrypted at-rest storage for identity, HS key, message database.
- Bootstrap Tor via embedded Arti, publish a v3 onion service with
  a seed-derived address.
- Byte-level inbound accept loop (`OnionListener`).
- Backup / restore of the full state as a portable archive.
- Wire-format frame codec (16 MiB cap, 10 frame types).
- `Noise_XK_25519_ChaChaPoly_BLAKE2s` handshake with optional `psk3`
  for first-contact, producing an `AuthenticatedConnection<S>` with
  the `h_transport` binding exposed.
- MLS 2-member groups: create, add-member (Welcome + Commit),
  encrypt/decrypt, state persists across restart via
  checkpoint-snapshot.
- Signed invite links (`skattr://invite/v1#…`) with canonical-CBOR
  Ed25519 signatures, Zeroizing PSK guard, and single-use tracking.
- Signed `ContactCard`s with monotonic-version persistence.
- Delivery layer: per-peer `PeerConnection` actor with 1 s retry
  tick, 60 s keepalive, 180 s idle close, and `ReplaceConn` for
  concurrent-dial races. Kill-mid-message → reconnect → exactly-once
  delivery is proven by a CI integration test.

## What doesn't work yet

- `skattr send <contact> "hello"` on the CLI (Phase 1.F — CLI
  integration is the missing glue between the delivery hub and
  user-visible commands).
- Scrolling message history / full-text search (Phase 1.G).
- Offline delivery via mailbox server (Phase 2).
- Desktop UI (Phase 2 — Tauri).
- Group chat beyond 2 members (Phase 3).

## Quickstart (desktop, Linux/macOS)

Requirements: Rust stable (see `rust-toolchain.toml`), a C toolchain,
and internet access on first run for Arti's consensus download.

```bash
git clone https://github.com/myggiz/skattr
cd skattr
cargo build --workspace --release

# Generate an identity. Record the 24-word seed phrase on screen.
cargo run -p skattr-cli --release -- init

# Start the daemon. Prints the .onion address once Tor is ready.
# Ctrl-C to stop.
cargo run -p skattr-cli --release -- daemon

# Back up everything (identity + HS key + DB) to a single file.
cargo run -p skattr-cli --release -- backup ~/skattr-backup.age

# Full recovery from seed phrase + backup on a clean machine:
cargo run -p skattr-cli --release -- restore-backup "word1 ... word24" ~/skattr-backup.age
```

See [`docs/OPERATIONS.md`](docs/OPERATIONS.md) for the full
developer guide.

## Layout

See [ARCHITECTURE.md](ARCHITECTURE.md) for the crate dependency diagram and data-flow walkthrough.

- `crates/core/` — protocol library (GPLv3).
- `crates/mailbox/` — mailbox server binary (AGPLv3).
- `crates/cli/` — `skattr` command-line client (GPLv3).
- `crates/tests/` — cross-crate integration tests.
- `docs/` — design docs, ADRs, protocol spec.

## Security

Report vulnerabilities privately — see [SECURITY.md](SECURITY.md). Do **not** open public issues for security problems.

## License

- **`core`, `cli`, `tests`** — [GNU General Public License v3.0](LICENSE-GPL3).
- **`mailbox`** — [GNU Affero General Public License v3.0](LICENSE-AGPL3). If you run a public mailbox, the AGPL's network-use clause applies.

Copyright © Myggiz AB. See [`docs/adr/0001-license.md`](docs/adr/0001-license.md) for the rationale.
