# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/myggiz/skattr/compare/main...HEAD
