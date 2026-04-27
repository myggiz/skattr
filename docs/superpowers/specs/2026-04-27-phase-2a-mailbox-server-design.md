# Phase 2.A — Mailbox server design

**Status:** approved (brainstorm 2026-04-27).
**Date:** 2026-04-27.
**Predecessor:** Phase 1.H merged 2026-04-24.
**Umbrella:** `docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md`.
**Kickoff:** `docs/superpowers/kickoffs/2026-04-26-phase-2a-mailbox-server-kickoff.md`.

## Scope

2.A ships the standalone mailbox server: an AGPLv3-licensed binary +
library (`crates/mailbox/`) that holds encrypted deposits for offline
recipients, plus a frozen wire-protocol surface in
`core::mailbox::protocol` that 2.B will consume unchanged.

**In scope:** `MailboxServer` + transport-agnostic dispatch loop,
SQLite deposit store, per-connection challenge-response auth,
operator-tunable caps + rate limiting, systemd unit + Dockerfile +
healthcheck + ops guide, full test pyramid (unit, property, fuzz,
adversarial, 24h soak, real-Tor smoke), and a protocol-freeze ADR.

**Out of scope** (deferred to v2 or later phases): `PROTOCOL_HELLO`
handshake, `SUBSCRIBE` / push delivery, `REGISTER` / registration
gate, client-side size-bucket padding, encryption-at-rest, Prometheus
exporter, federation, mailbox client (2.B), UI surfaces (2.F).

## Architectural decisions (locked in brainstorm)

1. **Minimal-4 protocol scope.** Frames: `Deposit` / `Challenge` /
   `Fetch` / `Delete` plus `Error`. No `HELLO`, no `SUBSCRIBE`, no
   `REGISTER`. Version is a per-frame `u16` field (`PROTOCOL_VERSION
   = 1`), not a handshake.
2. **Library + binary crate split.** `crates/mailbox/` ships both
   `[lib]` (`skattr_mailbox`) and `[bin]` (`skattr-mailbox`). The
   library is transport-agnostic; the binary owns Arti + signal
   handling + config loading. Tests drive the library in-process via
   `tokio::io::duplex` pairs.
3. **Plain SQLite, no `age`.** The mailbox stores only ciphertext;
   metadata (recipient hash, deposit time) protection is the
   operator's filesystem-encryption concern, not the app's.
4. **Per-connection rate limits + global cap.** "Per-circuit" in the
   prose maps to "per inbound stream" in code: each connection gets
   its own token bucket (30 deposits/min, 6 fetches/min); a global
   bucket (default 1000 deposits/min) bounds reconnect-storm bypass.
5. **Reuse `core::transport::frame::FrameCodec`.** No parallel wire
   format. The mailbox does NOT do Noise_XK — the v3 onion is the
   transport-auth layer and depositor anonymity is required.
6. **Wire types live in `core::mailbox::protocol`.** Already `pub`;
   2.A populates it; 2.B inherits unchanged.

## Wire protocol (v1, frozen)

All frames are CBOR (canonical: sorted keys, definite lengths) inside
the existing `length_u32 || type_u8 || payload` framing. Frame-type
bytes are disjoint from peer-to-peer frame types.

| Type | Name              | Direction | Body                                                                                  |
|------|-------------------|-----------|---------------------------------------------------------------------------------------|
| 0x82 | `Deposit`         | C→S       | `{ version: u16, recipient_hash: [u8;32], ciphertext: bytes, ttl_request: u32 }`      |
| 0x83 | `DepositOk`       | S→C       | `{ deposit_id: [u8;16], expires_at: i64 }`                                            |
| 0x84 | `Challenge`       | C→S       | `{ version: u16, identity_hash: [u8;32] }`                                            |
| 0x85 | `ChallengeNonce`  | S→C       | `{ nonce: [u8;32], issued_at: i64 }`                                                  |
| 0x86 | `Fetch`           | C→S       | `{ version: u16, identity_pubkey: [u8;32], nonce: [u8;32], signature: [u8;64] }`      |
| 0x87 | `FetchResponse`   | S→C       | `{ deposits: Vec<{ deposit_id: [u8;16], ciphertext: bytes, received_at: i64 }> }`     |
| 0x88 | `Delete`          | C→S       | `{ version: u16, identity_pubkey: [u8;32], nonce: [u8;32], signature: [u8;64], deposit_ids: Vec<[u8;16]> }` |
| 0x89 | `DeleteOk`        | S→C       | `{ deleted: u32, not_found: u32 }`                                                    |
| 0x8F | `Error`           | S→C       | `{ code: ErrorCode, message: String }`                                                |

`ErrorCode` (CBOR-tagged enum):

`UnsupportedVersion` · `MalformedRequest` · `TooLarge` · `RateLimited`
· `RecipientFull` · `TtlTooLong` · `TtlTooShort` · `InvalidSignature`
· `HashMismatch` · `NonceExpired` · `NotFound` · `Internal`.

### Auth construction

Fetch and Delete sign:

```
"skattr-mailbox-auth-v1" || nonce || op_byte || sha256(canonical_cbor(payload_minus_signature))
```

with the recipient's Ed25519 identity key. Server verifies:

1. `sha256(identity_pubkey) == challenge.identity_hash` for the open
   nonce. Reply `HashMismatch` on failure.
2. Signature verifies under `identity_pubkey` over the auth string.
   Reply `InvalidSignature` on failure.
3. Nonce was issued ≤ 30 s ago and has not been consumed. Reply
   `NonceExpired` on failure.

### Per-connection state machine

```
                   Deposit (no auth)
                      ▲
                      │
           ┌──────────┴──────────┐
           │                     │
Idle ────► AwaitingChallenge ──► AwaitingAuthedOp ──► Idle
       Challenge        ChallengeNonce         Fetch | Delete
```

A connection may issue any sequence of operations. Each Fetch/Delete
consumes exactly one nonce; the connection is not closed on
rate-limit, malformed-frame, or auth failure (that would invite
reconnect storms). The server closes only on stream-level error or
graceful client close.

### Version handling

Each request carries `version: u16`. If `version != PROTOCOL_VERSION`,
the server replies `Error { code: UnsupportedVersion }` and continues
to await the next frame (no close). 2.B's client compares the constant
at connect time and refuses to talk to older mailboxes.

## Storage

Plain SQLite (`rusqlite`, `bundled` feature, WAL mode, `synchronous =
NORMAL`). One file at `${data_dir}/mailbox.sqlite`.

```sql
-- migrations/0001_init.sql
CREATE TABLE deposits (
  deposit_id     BLOB PRIMARY KEY,        -- 16 random bytes
  recipient_hash BLOB NOT NULL,           -- 32 bytes
  ciphertext     BLOB NOT NULL,
  deposited_at   INTEGER NOT NULL,
  expires_at     INTEGER NOT NULL
);
CREATE INDEX idx_deposits_recipient ON deposits(recipient_hash, deposited_at);
CREATE INDEX idx_deposits_expiry    ON deposits(expires_at);

CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
INSERT INTO schema_version VALUES (1);
```

Migrations runner mirrors the `core::storage::migrations` pattern:
`include_str!`'d files keyed by `schema_version`. The 2.A merge ships
0001 only; future schema bumps land as `0002_*.sql`.

### Per-recipient cap eviction

On `Deposit`, in one SQL transaction:

1. `SELECT COALESCE(SUM(LENGTH(ciphertext)), 0) FROM deposits WHERE
   recipient_hash = ?` → `existing_bytes`.
2. If `existing_bytes + len(new_ciphertext) ≤ recipient_cap_bytes`,
   insert and commit.
3. Otherwise: `DELETE FROM deposits WHERE recipient_hash = ? AND
   expires_at < strftime('%s','now') ORDER BY deposited_at ASC LIMIT
   N` (`N` chosen to free enough bytes).
4. Re-check; if still over cap, rollback and reply `RecipientFull`.

This eviction policy never silently drops a *pending* (non-expired)
deposit; an attacker who fills a victim's quota with non-expired junk
gets `RecipientFull` rejections, but legitimate fetches recover the
queue when the recipient comes online.

### Background tasks

One tokio task each, started by `MailboxServer::spawn`:

- **`expire_sweep`** — every 60 s: `DELETE FROM deposits WHERE
  expires_at < ?`. Logs the rowcount at `info` (aggregate counter).
- **`challenge_sweep`** — every 30 s: drop nonces past 30 s TTL from
  the in-memory `Challenges` table.
- **`metrics_tick`** — every 60 s: log
  `accepted_deposits=N served_fetches=M storage_bytes=B
  active_recipients=R rate_limited=L` for the previous interval.

Tasks shut down on `JoinSet::shutdown_now()` from the server's
top-level cancellation signal.

## Policy

```rust
pub struct Policy {
    pub max_deposit_size: u64,           // 1 MiB
    pub min_ttl_secs: u32,               // 3 600           (1 h)
    pub max_ttl_secs: u32,               // 2 592 000       (30 days)
    pub default_ttl_secs: u32,           // 604 800         (7 days)
    pub recipient_cap_bytes: u64,        // 256 MiB
    pub per_conn_deposits_per_min: u32,  // 30
    pub per_conn_fetches_per_min: u32,   // 6
    pub global_deposits_per_min: u32,    // 1 000
}
```

All fields operator-overridable in `mailbox.toml`. Full template:

```toml
[server]
data_dir = "/var/lib/skattr-mailbox"
# storage_path, arti_state_dir, health_socket all default to
# children of data_dir; each can be overridden individually.
# storage_path = "/var/lib/skattr-mailbox/mailbox.sqlite"
# arti_state_dir = "/var/lib/skattr-mailbox/arti"
# health_socket = "/var/lib/skattr-mailbox/health.sock"
instance_label = "mailbox-1"   # optional; for log disambiguation

[policy]
max_deposit_size           = 1048576      # 1 MiB
min_ttl_secs               = 3600         # 1 h
max_ttl_secs               = 2592000      # 30 days
default_ttl_secs           = 604800       # 7 days
recipient_cap_bytes        = 268435456    # 256 MiB
per_conn_deposits_per_min  = 30
per_conn_fetches_per_min   = 6
global_deposits_per_min    = 1000
```

The existing `MailboxConfig` (storage_path / arti_state_dir /
max_deposit_size / max_ttl_days / instance_label) is replaced with
this richer schema; 2.A's first commit on the config module is the
breaking rename + field expansion.

Clamps:

- `ttl_request < min_ttl_secs` → `TtlTooShort`.
- `ttl_request > max_ttl_secs` → `TtlTooLong`.
- `ttl_request == 0` → use `default_ttl_secs`.
- `len(ciphertext) > max_deposit_size` → `TooLarge`.

### Rate limiting

Token bucket per connection (one per `MailboxServer::accept_loop`
invocation), refilled at `per_conn_*_per_min / 60` tokens/sec. Plus a
single global token bucket at `global_deposits_per_min / 60`
tokens/sec, shared via `Arc<Mutex<TokenBucket>>`.

Order of checks per Deposit: (a) parse, (b) per-conn bucket, (c)
global bucket, (d) policy clamps, (e) cap enforcement, (f) insert.
Reject at the earliest failed check with the corresponding
`ErrorCode`. **The connection is never closed** on any of these —
client gets the typed error and may retry.

## Crate layout

```
crates/mailbox/                                        AGPLv3 (every .rs)
├── Cargo.toml                  [lib] + [bin]
├── migrations/0001_init.sql
├── src/
│   ├── lib.rs                  pub: MailboxServer, Policy, Store,
│   │                           MailboxError, MailboxErrorKind
│   ├── server.rs               MailboxServer::new(store, policy);
│   │                           accept_loop<S>(stream) per-stream FSM
│   ├── dispatch.rs             handle_deposit / _challenge / _fetch /
│   │                           _delete; pure functions over Store +
│   │                           Policy + Challenges; unit-testable
│   ├── store.rs                rusqlite Store: insert / fetch /
│   │                           delete / expire_sweep / cap-enforce
│   ├── auth.rs                 Challenges (issue / verify / sweep)
│   ├── policy.rs               Policy, TokenBucket, ConnRateLimiter
│   ├── health.rs               UDS server at ${data_dir}/health.sock
│   ├── arti.rs                 [bin-only] Arti bootstrap; publishes
│   │                           v3 onion; routes inbound streams to
│   │                           MailboxServer::accept_loop
│   ├── config.rs               MailboxConfig (toml load)
│   ├── error.rs                MailboxError + MailboxErrorKind
│   └── main.rs                 [bin-only] CLI parse → load config →
│                               wire arti → spawn server → SIGTERM
└── tests/
    ├── deposit_roundtrip.rs    unit + property
    ├── auth_replay.rs          adversarial
    ├── caps_eviction.rs        adversarial
    ├── rate_limit.rs           per-conn + global
    └── soak.rs                 #[ignore]-gated 24h driver
```

`crates/core/src/mailbox/protocol.rs` — populated to match the v1
table above. Already `pub`.

## Module visibility

- `core::mailbox::protocol` — `pub` (the sole cross-crate surface).
- `core::mailbox::client`, `scheduler` — unchanged (`pub(crate)`),
  not 2.A's concern.
- `crates/mailbox/src/lib.rs` exposes `MailboxServer`, `Policy`,
  `Store`, `MailboxError`, `MailboxErrorKind`. Everything else
  `pub(crate)`.

## Errors

`MailboxError` and `MailboxErrorKind` follow the 1.H pattern (six
subsystem sub-enums, structural `kind()`):

```rust
#[derive(Debug, thiserror::Error)]
pub enum MailboxError {
    #[error("storage: {0}")] Storage(StorageErrorKind),
    #[error("auth: {0}")]    Auth(AuthErrorKind),
    #[error("policy: {0}")]  Policy(PolicyErrorKind),
    #[error("transport: {0}")] Transport(TransportErrorKind),
    #[error("arti: {0}")]    Arti(ArtiErrorKind),
    #[error("config: {0}")]  Config(ConfigErrorKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxErrorKind { Storage, Auth, Policy, Transport, Arti, Config }
```

Wire-level errors are `protocol::Error { code, message }`; the server
maps internal `MailboxError` variants to the appropriate `ErrorCode`
in one place (`dispatch::error_code_for`). Internal errors that
don't have a public mapping become `ErrorCode::Internal` and are
logged at `error` (without leaking details to the client).

`unwrap`/`expect` are forbidden in library code (CLAUDE.md);
`anyhow` is allowed only in `main.rs`.

## Operational artefacts

### `packaging/systemd/skattr-mailbox.service`

```
[Unit]
Description=Skattr mailbox server
After=network.target

[Service]
Type=notify
ExecStart=/usr/local/bin/skattr-mailbox --config /etc/skattr-mailbox/mailbox.toml
DynamicUser=yes
StateDirectory=skattr-mailbox
WorkingDirectory=/var/lib/skattr-mailbox
ProtectSystem=strict
ProtectHome=yes
NoNewPrivileges=yes
PrivateTmp=yes
PrivateDevices=yes
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
RestrictNamespaces=yes
MemoryDenyWriteExecute=yes
LockPersonality=yes
SystemCallFilter=@system-service
SystemCallFilter=~@privileged
WatchdogSec=120
Restart=on-failure
RestartSec=10s

[Install]
WantedBy=multi-user.target
```

### `packaging/Dockerfile`

Two-stage. Build: `rust:1.X-slim` (toolchain pinned). Run:
`gcr.io/distroless/cc-debian12:nonroot`.
`COPY --from=builder /target/release/skattr-mailbox
/usr/local/bin/`. `ENTRYPOINT ["/usr/local/bin/skattr-mailbox",
"--config", "/etc/skattr-mailbox/mailbox.toml"]`. Volume:
`/var/lib/skattr-mailbox`. Compose snippet in the ops guide.

### Healthcheck

UDS at `${data_dir}/health.sock`, mode 0600, owned by service user.
One-line line-based protocol:

- Request: `GET /health\n`.
- Reply on healthy: `ok\n`.
- Reply on degraded: `degraded: <reason>\n` where `<reason>` ∈
  { `db_unavailable`, `arti_not_bootstrapped`, `disk_full` }.

systemd's `WatchdogSec=120` is fed by the server's own sd-notify
watchdog, not by the UDS healthcheck. Docker `HEALTHCHECK` invokes a
tiny `socat` one-liner against the UDS. The healthcheck is **never**
reachable through the onion.

### Ops guide

`docs/operations/mailbox-setup.md` covers:

1. From-source install (cargo build, copy systemd unit, edit
   `mailbox.toml`).
2. Docker compose snippet.
3. `mailbox.toml` field reference (every `Policy` knob).
4. Backup procedure (`sqlite3 mailbox.sqlite '.backup mailbox.bak'`;
   no quiesce required thanks to WAL).
5. Upgrade procedure (stop, swap binary, start; migrations are
   forward-only).
6. Troubleshooting (`degraded:` reasons, log inspection).

Target: working mailbox in ≤ 30 minutes from a fresh VM.

## Logging policy

`tracing` levels:

- `error` — unrecoverable internal errors only. Never includes a
  recipient hash, pubkey, or ciphertext. Includes a stable error tag
  (e.g., `db_constraint_violation`).
- `warn` — aggregate policy rejects: `rejected: TooLarge ×N in last
  60s`. No per-request data.
- `info` — aggregate counters at fixed cadence (every 60 s):
  `accepted_deposits=N served_fetches=M storage_bytes=B
  active_recipients=R rate_limited=L`.
- `debug` — per-request, but `recipient_hash` and `identity_pubkey`
  printed only as the first 8 hex chars (`recipient=ab12cd34…`);
  never the full 64 hex; never ciphertext; never a raw `deposit_id`.
- `trace` — same redaction as debug; gated behind `RUST_LOG=trace`,
  off in release builds.

A unit test (`logging_redaction.rs`) constructs a synthetic event for
each redacted field type and asserts the formatted output never
contains the full hex of the secret.

## Test plan

Six layers; every layer is required green for the merge PR.

### 1. Unit (`src/**/*.rs` `#[cfg(test)]` blocks)

- `Store::insert/fetch/delete/expire_sweep` round-trips.
- `Challenges::issue/verify/sweep` happy + 30 s expiry + replay.
- `TokenBucket` over a fake clock.
- Policy clamps (TTL min/max, default, deposit size, recipient cap).
- CBOR codec round-trip for every frame body.
- `dispatch::error_code_for` for every internal error variant.

### 2. Property (`crates/mailbox/tests/property.rs`, `proptest`)

- `cbor_encode → cbor_decode` is identity for arbitrary valid frames.
- `sign(payload) → verify(payload)` always passes for valid (key,
  nonce, payload) triples.
- Cap enforcement: ∀ random `(existing, new, cap)`, post-state
  invariant `Σ ciphertext ≤ cap` always holds.
- TTL clamp is monotonic in `now`: ∀ `now₁ ≤ now₂`,
  `expires_at(req, now₁) ≤ expires_at(req, now₂)`.

### 3. Fuzz (`crates/mailbox/fuzz/`, `cargo fuzz`)

Targets:

- `fuzz_target_frame_decode` — feed arbitrary bytes into each frame
  decoder.
- `fuzz_target_dispatch` — feed an arbitrary `Vec<Frame>` into a
  fresh `MailboxServer` and assert no panic / no UB / state remains
  consistent.

Local bar: ≥ 1 hour each, no findings. Corpus committed under
`crates/mailbox/fuzz/corpus/`. CI runs a 60-second smoke variant on
every PR (gated behind a `fuzz-smoke` feature so PRs don't require
nightly).

### 4. Adversarial (`crates/mailbox/tests/adversarial_*.rs`)

One file per attack class; every `ErrorCode` variant has at least one
test that triggers it:

- `auth_replay.rs` — old nonce, reused nonce, signed-by-wrong-key,
  hash-mismatch.
- `oversize_ttl.rs` — oversize, TTL underflow, TTL overflow.
- `concurrent_delete.rs` — two clients race the same `deposit_id`,
  both observe consistent `deleted + not_found` totals.
- `caps_eviction.rs` — fill cap with expired + non-expired; verify
  eviction order; verify `RecipientFull` on real overflow.
- `malformed_cbor.rs` — truncated, surplus bytes, wrong type tags.
- `rate_limit.rs` — per-conn burst + global cap; reconnect-storm
  bypass attempt; `RateLimited` does not close connection.

### 5. 24-hour soak (`crates/mailbox/tests/soak.rs`, `#[ignore]`)

Driver runs the in-process `MailboxServer` over `tokio::io::duplex`
(no real Tor). Synthetic load: 1 000 recipients × ~100 deposits/hour
random Poisson arrivals + matching fetch/delete cycles for a 10 %
"online" subset.

Asserts:

- No panics, no `tokio` task leaks (final `JoinSet::len() == 0` after
  shutdown).
- RSS sampled every minute; max ≤ 2 × steady-state RSS.
- Storage bounded: total bytes never exceeds operator cap by more
  than one deposit's worth.
- Rate-limit acceptance ratio matches expected (Bernoulli) within
  ±5 %.
- Final `expire_sweep` leaves the DB empty.

Output (final summary line) committed to `docs/superpowers/runs/`
on the merge PR.

### 6. Real-Tor smoke (`crates/tests/src/mailbox_real_tor.rs`,
`#[ignore]`)

Mirror of 1.E's `delivery_real_tor.rs` pattern: spawn the
`skattr-mailbox` binary, publish a v3 onion via Arti, drive
`MailboxClient` against it, assert one full deposit → fetch → delete
cycle. Run with `cargo test -p skattr-tests --release -- --ignored`.

## Freeze definition

The protocol is frozen for 2.B when **all** hold on the merge PR:

- `MAILBOX_PROTOCOL_VERSION: u16 = 1` exported from
  `core::mailbox::protocol`.
- All six test layers green.
- 24-hour soak run once with green output, summary committed.
- Local fuzz ≥ 1 hour with no findings; CI smoke green.
- Every `ErrorCode` variant has a triggering test in the adversarial
  suite.
- ADR `docs/adr/0006-mailbox-protocol-v1.md` records the frozen
  wire types, error codes, and the rule "incompatible changes
  require a new `MAILBOX_PROTOCOL_V2`."
- `core::mailbox::protocol` exports nothing beyond the wire types
  (no policy, no transport assumptions) so 2.B inherits a clean
  surface.

## Cross-cutting compliance

- **License headers.** Every `crates/mailbox/**/*.rs` carries the
  AGPLv3 header (`SPDX-License-Identifier: AGPL-3.0-or-later`).
  `core::mailbox::protocol` keeps the GPLv3 header.
- **No `unwrap`/`expect` in library code.** Enforced by clippy.
- **No identity pubkeys, full hashes, ciphertext, or deposit_ids
  above `debug` level.** Enforced by the redaction unit test.
- **`cargo deny check` clean.** No new banned licenses; new deps
  justified in PR description.
- **Workspace lints unchanged.** `cargo clippy --all-targets -D
  warnings` and `cargo fmt --check` are merge-blocking.
- **Second reviewer required** on any PR touching
  `core::mailbox::protocol` (CLAUDE.md crypto/protocol rule).

## Risks and mitigations

| Risk                                                                        | Mitigation                                                                                                                                                            |
|-----------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Wire types need revision after 2.B reveals real-use issues                  | `PROTOCOL_VERSION = 1` is per-frame; v2 ships as a parallel decoder; mailboxes can advertise both. ADR documents the bump path.                                       |
| Per-recipient cap eviction lets an attacker DoS one victim                  | Eviction never drops non-expired deposits; attacker just gets `RecipientFull` and the victim's pending queue is preserved. Documented as accepted trade-off.          |
| Per-connection rate limit bypassed by reconnect storm                       | Global token bucket is the backstop. Operator can lower `global_deposits_per_min` aggressively. Tor circuit churn is itself a soft brake.                             |
| Healthcheck UDS leaks on permission misconfig                               | Mode 0600 on creation; systemd `StateDirectory=` owns the parent. Test asserts `umask` doesn't widen it.                                                              |
| Distroless image lacks debugging affordances                                | `:debug-nonroot` variant documented for operators who need a shell; production stays on plain `:nonroot`.                                                             |
| `tor-hsservice 0.41` API drift between brainstorm and implementation        | Pin transitively via existing `arti-client = 0.41`; bump is its own PR with re-running the real-Tor smoke test.                                                       |
| 24h soak flakes on CI infra                                                 | Soak is `#[ignore]`-gated; runs on a developer workstation as part of merge PR validation, not on CI minutes. Output committed so reviewer can verify without re-running. |
| Sustained Challenge or malformed-frame flood on one connection isn't bound by the deposit/fetch buckets | Implementation may add a per-conn frames/min ceiling (default 120) as defence-in-depth; v1 protocol does not require it because the global deposit cap and OS-level CPU budget already bound blast radius. Decision deferred to writing-plans. |

## Deliverables

1. Populated `core::mailbox::protocol` with the v1 wire types,
   `PROTOCOL_VERSION` constant, and `ErrorCode` enum.
2. `crates/mailbox/` promoted to `[lib] + [bin]`; new modules per
   the layout above.
3. `migrations/0001_init.sql` + migrations runner.
4. Six test-layer suites passing.
5. `packaging/systemd/skattr-mailbox.service` +
   `packaging/Dockerfile`.
6. `docs/operations/mailbox-setup.md` (≤ 30-min target).
7. `docs/adr/0006-mailbox-protocol-v1.md` (the freeze ADR).
8. `docs/superpowers/runs/<merge-date>-mailbox-soak.txt` (soak summary).
9. CHANGELOG bullet + `CLAUDE.md` repository-state update.

## Open items deferred to the implementation plan

- Exact `cargo fuzz` directory wiring (`crates/mailbox/fuzz/` vs.
  workspace-level) — let writing-plans decide based on `cargo fuzz`
  conventions in the workspace.
- Whether the 24h soak driver lives in `tests/soak.rs` or as a
  separate `xtask`-style binary — writing-plans picks based on test
  ergonomics.
- Specific Rust toolchain version pin in the Dockerfile builder
  stage — must match `rust-toolchain.toml`; mechanical.
