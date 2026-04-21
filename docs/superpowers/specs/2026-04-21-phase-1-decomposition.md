# Phase 1 Decomposition Spec

**Status:** Approved 2026-04-21. Supersedes no prior doc; complements `docs/skattr-implementation-plan.md` §Phase 1.

## Scope

Phase 1's plan-level goal: "Two online users on different networks exchange end-to-end encrypted messages via CLI." The implementation plan splits Phase 1 into seven workstreams (1.A–1.G). This doc locks that split as seven **independent sub-projects**, each with its own spec → plan → subagent-driven execution cycle.

The split is by module layer (each sub-project ships a finished module), not by user-visible feature slice. End-to-end user-visible behaviour lands only when 1.F (CLI integration) completes.

## Sub-projects

| ID  | Name                      | Primary modules touched                             | Depends on     | Exit criterion                                                                                                  |
|-----|---------------------------|-----------------------------------------------------|----------------|-----------------------------------------------------------------------------------------------------------------|
| 1.A | Frame codec               | `transport/frame.rs`                                | none           | Frame round-trips through `FrameCodec::{encode,decode}`; length-prefix + type byte + CBOR correct; fuzz target nightly-clean |
| 1.B | Noise_XK handshake        | `transport/noise.rs`, `transport/connection.rs`    | 1.A            | Two daemons complete `Noise_XK_25519_ChaChaPoly_BLAKE2s` over Tor; `h_transport` extracted via `HKDF(hh, "skattr-binding-v1")` |
| 1.C | MLS 2-member groups       | `mls/{group, welcome, commit, keystore}.rs`, `storage::groups` wiring | 0.D            | `Group::create_solo` → `add_member` → Welcome → bidirectional encrypt/decrypt; group state persists across restart; external PSK bound into first Commit |
| 1.D | Invite & contact flow     | `invite/{link, qr}.rs`, `contact/{contact, card}.rs` | 1.C            | Invite link generates, parses, Ed25519 sig verifies, single-use enforced; `ContactCard::{sign,verify}` round-trips; contacts persisted via `storage::contacts` |
| 1.E | Delivery semantics        | `delivery/{sender, outbox, receiver}.rs`, `transport/connection` | 1.A, 1.B, 1.C  | Outbox + exponential backoff + ACK handling + receiver dedup + connection pool; kill-mid-message then reconnect delivers the message |
| 1.F | CLI integration           | `daemon/state.rs`, `crates/cli/src/main.rs`        | all above      | `skattr invite` / `add` / `send` / `tail` / `contacts` wire through a real `Daemon::execute`; IPC via Unix socket; config in `~/.config/skattr/config.toml` |
| 1.G | Message storage & search  | `storage/messages.rs` (FTS5), query API            | 0.D            | FTS5 virtual table populated; `messages::recent / search / unread_count / export` APIs + `skattr tail` / `skattr search` |

## Dependency DAG

```
                    0.D ──► 1.C ──► 1.D ─┐
                      \                   \
                       └── 1.G             ├─► 1.E ──► 1.F
                                           /
                    1.A ──► 1.B ──────────┘
```

## Execution order

Serial: **1.A → 1.B → 1.C → 1.D → 1.E → 1.F → 1.G**.

1.C could run in parallel with 1.A+1.B (disjoint sub-graphs), but the established cadence is single-stream subagent-driven development, so serial wins for coherent context.

1.G can slip to after 1.F since nothing earlier depends on it.

## Per-sub-project deliverables

Each sub-project produces:

1. A **design spec** at `docs/superpowers/specs/YYYY-MM-DD-phase-1<x>-<name>-design.md` (output of brainstorming).
2. An **implementation plan** at `docs/superpowers/plans/YYYY-MM-DD-phase-1<x>-<name>.md` (output of writing-plans).
3. A **merge to master** with: passing unit + integration tests on the sub-project's exit criterion, `cargo fmt --check` / `cargo clippy -D warnings` / `cargo test` green, CHANGELOG bullet, CLAUDE.md status update.

## Locked decisions (from implementation plan §Phase 1)

Already decided; do not relitigate in sub-project brainstorming:

- **Noise pattern:** `Noise_XK_25519_ChaChaPoly_BLAKE2s` via `snow`.
- **MLS ciphersuite:** `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519`.
- **Wire serialization:** CBOR via `ciborium`.
- **Max frame payload:** 16 MiB (protocol limit); max Envelope body: 64 KiB (attachments deferred to Phase 3).
- **Invite URI scheme:** `skattr://invite/v1#<fragment>`. Fragment-based to avoid referer leaks.
- **Transport↔MLS binding:** `h_transport = HKDF(noise_handshake_hash, "skattr-binding-v1")` as external PSK in first MLS Commit.
- **MLS epoch advance policy:** every 24 hours or every 100 messages, whichever first.

## Phase 1 exit criteria (from implementation plan)

Reached only after all seven sub-projects merge. Documented here for reference:

- Two users on different networks complete the full flow: invite → add contact → exchange messages in both directions.
- Message history survives daemon restart on both sides.
- MLS epoch advances observably (log + storage row shows epoch change).
- External security review of Noise + MLS integration code.
- All fuzz targets nightly-clean for one week.
- Latency under 5 s over real Tor; steady-state memory under 200 MB.

## What this doc does NOT cover

- **Sub-project internals.** Each sub-project's own design spec covers architecture, data flow, error handling, testing strategy for that module.
- **Phase 2+ work.** Mailbox server, offline delivery, UI (Tauri), group chat, etc. are out of Phase 1 scope.
- **Protocol-level changes to the locked decisions above.** Any change requires a new ADR under `docs/adr/`.
