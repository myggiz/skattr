# Skattr

> Messages, scattered.

Skattr is a desktop-first, metadata-resistant, end-to-end encrypted messenger built on Tor v3 onion services and MLS (RFC 9420). It has no phone number, no email signup, and no central account server. Identity is a keypair backed by a BIP39 seed phrase.

**Status: approaching a v1.0 release.** Skattr is a working 1:1 (two-party),
attachment-capable, Tor-only encrypted messenger: real two-daemon messaging in
both directions, first-contact via signed invite links, offline delivery through
semi-trusted mailboxes, file attachments (online and offline), and a desktop
(Tauri) app plus a CLI. The release-hardening work — honest docs, a working
download-verification chain, and a real minisign signing key — is in place;
v0.1.0 is the first signed release. See
[ARCHITECTURE.md](ARCHITECTURE.md) and [`docs/`](docs/) for the design, and
[THREAT_MODEL.md](docs/THREAT_MODEL.md) for security properties and limitations.

## What Skattr is

- **Peer-to-peer.** Clients reach each other directly via Tor onion services. No central relay.
- **Metadata-resistant.** Tor hides network-level metadata; a semi-trusted "mailbox" handles offline delivery and sees only opaque ciphertext, but it does learn a *stable* per-recipient hash plus polling/size metadata — see [THREAT_MODEL.md](docs/THREAT_MODEL.md) for the correlation caveats.
- **1:1, built on MLS.** v1.0 is two-party messaging — a 2-member [MLS](https://datatracker.ietf.org/doc/rfc9420/) group. Multi-member groups (> 2) are deferred to a later release.
- **Desktop first.** Native app via Tauri, with a CLI alongside for power users and scripting.
- **Rust, all the way down.** Tor via [Arti](https://gitlab.torproject.org/tpo/core/arti), MLS via [OpenMLS](https://openmls.tech/), transport auth via [Noise_XK](https://noiseprotocol.org/) (through `snow`).

## What Skattr isn't

- Not a feature-equivalent Signal replacement (no phone numbers, no SMS, no voice/video in v1).
- Not a low-latency chat (Tor round-trips cost seconds, not milliseconds).
- Not mobile in v1. Mobile is post-1.0 at the earliest.
- Not "anonymous" — your contacts know who you are. It's metadata-resistant, not identity-destroying.

## What works

- Create/restore a BIP39-backed identity (`skattr init` / `skattr restore`).
- At-rest encryption for identity, HS key, and the message database; backup/restore as a portable archive.
- Tor via embedded Arti: publishes a v3 onion service at a stable address (persisted across restarts and preserved by backup/restore).
- Real two-party messaging in both directions over the production transport, with per-peer retry, ACK correlation, and exactly-once delivery (CI-proven).
- First contact via signed `skattr://invite/v1#…` links (single-use) and signed `ContactCard`s.
- `Noise_XK_25519_ChaChaPoly_BLAKE2s` transport auth bound to the MLS group (`h_transport` PSK).
- Offline delivery through semi-trusted mailboxes (deposit/fetch, failover, drain on removal).
- File attachments: send/receive with metadata stripping, online (direct) and offline (mailbox), inline image preview in the UI.
- Scrolling message history with full-text search; configurable retention.
- Desktop UI (Tauri) and a `skattr` CLI.

## Limitations (v1.0)

- **Two-party only.** Multi-member groups (> 2) are deferred.
- **First contact needs both peers online at once** — first contact is direct-only (no mailbox fallback); if your contact is offline when you add them, the connection will not complete until they are online.
- **"Rotate onion address" is not yet real** — it republishes your current address with a new card version; true address rotation is planned for a later release.
- **Offline attachments are best-effort** — held by a mailbox for ~7 days and dropped if never fetched; files over 10 MiB transfer only while both peers are online.
- **Received attachments are encrypted at rest and decrypted on demand.** Completed chunks are kept as AEAD ciphertext under `<data_dir>/attachments/` indefinitely — there is no automatic GC. Use *Open* (decrypts to a temporary cache wiped on app start/exit) or *Save* (decrypt to a path you choose) to access a file. Disk usage grows with received files; a delete/retention policy is planned for v1.1.
- Not a low-latency chat (Tor round-trips cost seconds), not mobile in v1.0, and not "anonymous" — your contacts know who you are.

See [THREAT_MODEL.md](docs/THREAT_MODEL.md) for the full security model and the v1.1 deferral list, and the [disclosure decision record](docs/superpowers/specs/2026-06-23-v1.0-pull-forward-vs-disclose-decisions.md).

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
