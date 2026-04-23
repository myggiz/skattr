# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository state

**Phase 0 is complete; Phase 1.A (frame codec), 1.B (Noise_XK handshake),
1.C (MLS 2-member groups), 1.D (invite & contact flow), 1.E (delivery
semantics), and 1.F (CLI integration) are done.** Phase 0 shipped all five workstreams (0.A scaffold,
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

Phase 1 continues with 1.G message storage & search — see
`docs/superpowers/specs/2026-04-21-phase-1-decomposition.md`.
The bootstrap prompt remains authoritative for
file layout, module boundaries, type signatures, and visibility rules
— match it exactly.

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

CI (per bootstrap prompt) runs fmt + clippy + test on `ubuntu-latest`, `macos-latest`, `windows-latest`.

## When extending the design

- Protocol-level changes (frame types, invite fields, handshake binding, MLS ciphersuite) need an ADR under `docs/adr/` with rationale before code.
- New dependencies need justification in the PR and must pass `cargo-deny` (license allowlist, no git deps, no banned crates).
- Every PR touching crypto, protocol, or auth requires a second reviewer.
