# Phase 2.D — Resource Hardening (anti-flood) (Design)

**Date:** 2026-06-15
**Status:** Approved (brainstorming complete); plan to follow.
**Predecessors:** Phase 2.A (MLS integrity) merged 2026-06-14; Phase 2.C
(offline delivery) merged 2026-06-15.
**Source:** `docs/superpowers/specs/2026-06-13-phase-2-decomposition.md` §2.D;
audit item T1-5 and the 1B accept-loop-spawn-bound TODO.
**Dependencies:** None — fully independent of 2.A/2.B/2.C. The mailbox crate is
separate (AGPLv3) and its wire protocol is frozen (ADR 0006).

Two independent halves, different crates and licenses:
- **Part A** — mailbox server resource limits (T1-5), `crates/mailbox/` (AGPLv3).
- **Part B** — daemon inbound accept-loop concurrency bound (1B TODO),
  `crates/core/src/daemon/accept.rs` (GPLv3).

**Wire-format / protocol NEUTRAL.** No new mailbox `MailboxFrame` types; no new
`core` `Command`/`CommandResult`/`Event` variants; ADR 0006 (mailbox protocol
v1) untouched. New internal rejection kinds reuse EXISTING wire `ErrorCode`s.

---

## Ground truth (verified against code 2026-06-15)

**Mailbox server (`crates/mailbox/src/`):**
- `Policy` (`policy.rs:14`) already has: `max_deposit_size`, `min/max/default_ttl_secs`,
  `recipient_cap_bytes`, `per_conn_deposits_per_min`, `per_conn_fetches_per_min`,
  `global_deposits_per_min`. Per-conn + global token buckets exist
  (`ConnRateLimiter`, `GlobalRateLimiter`).
- `Store::insert` (`store.rs:68`) enforces ONLY the per-recipient byte cap, with
  `evict_expired_for` (`store.rs:220`) evicting **expired** rows oldest-first for
  that recipient before rejecting `RecipientFull`. There is no global byte cap,
  no recipient-count cap, and no non-expired eviction. `storage_bytes()`
  (`store.rs:204`) exists for metrics only — nothing enforces a server-wide
  ceiling.
- `accept_loop` (`server.rs:67`) reads frames in a loop with NO idle/read
  timeout — a client can hold a connection open indefinitely. The "reject ≠
  close connection" contract holds (only codec/IO errors close; policy/auth/
  rate-limit rejections reply `Error` and keep the connection open).
- `arti.rs:167` (the `#[cfg(feature="bin")]` Tor path) spawns
  `server.accept_loop(data_stream)` per inbound stream with NO concurrency
  bound — an attacker who knows the onion can open unbounded connections.
- `handle_delete` (`dispatch.rs`) iterates `Delete.deposit_ids` with no length
  cap.
- `config.rs` loads `[policy]` into `Policy` via serde with `Policy::recommended`
  as the `#[serde(default)]`; `validate()` checks TTL ordering + deposit/recipient
  cap sanity. New knobs slot in here.

**Daemon accept loop (`crates/core/src/daemon/accept.rs`):**
- `run_accept_loop` (`accept.rs:23`) loops `inbound.recv()` and, per stream,
  `tokio::spawn`s a detached handshake+resolve+ingest task with NO concurrency
  bound and NO task tracking. A `// TODO(phase-2 transport hardening)` at
  `accept.rs:37` marks exactly this work (Semaphore permit + JoinSet drain).

## Locked decision (brainstorming, 2026-06-15)

**Global-pressure behavior = reject-after-expired-eviction.** When the global
storage cap is hit: evict globally-EXPIRED rows (free, no loss), re-check, and
if still over, REJECT the new deposit (`ServerFull`). Never evict an accepted,
non-expired message. Rationale: since Phase 2.C, the sender's outbox deletes a
row only on a SUCCESSFUL deposit, so a rejected deposit is retained and retried
by the sweeper once TTL frees space — non-lossy end-to-end. Evicting a
non-expired accepted message would be permanent loss (a durability attack on a
privacy tool). This intentionally supersedes the audit's literal "LRU lets a
fresh deposit land" wording, which predates 2.C's sender-side retry.

---

## Part A — Mailbox server hardening (T1-5)

### New `Policy` knobs

Added to the `Policy` struct (`policy.rs`), `recommended()`, the `[policy]`
config doc + `validate()`:

| Field | Type | Recommended default | Purpose |
|---|---|---|---|
| `global_storage_cap_bytes` | `u64` | `4_294_967_296` (4 GiB) | Server-wide byte ceiling across all recipients. |
| `max_recipients` | `u64` | `100_000` | Cap on distinct recipient hashes with stored rows (bounds the many-tiny-recipients row/index DoS the byte cap misses). |
| `idle_timeout_secs` | `u32` | `120` | Per-connection idle read deadline. |
| `max_connections` | `u32` | `512` | Server-wide concurrent `accept_loop` ceiling. |
| `max_delete_ids` | `u32` | `1_024` | Max length of `Delete.deposit_ids`. |

`validate()` additions: `global_storage_cap_bytes >= recipient_cap_bytes`;
`max_recipients >= 1`; `idle_timeout_secs >= 1`; `max_connections >= 1`;
`max_delete_ids >= 1`. All new fields get `#[serde(default = "...")]` per-field
defaults so an existing `mailbox.toml` without them still loads (forward-compat).

### A1 — Global + recipient-count caps in `Store::insert`

Extend the existing atomic insert transaction (`store.rs:68`). Order inside the
one transaction:
1. (existing) Per-recipient cap: sum this recipient's bytes; if over, evict this
   recipient's EXPIRED rows oldest-first; if still over → `RecipientFull`.
2. NEW recipient-count cap: if this `recipient_hash` has zero existing rows AND
   `COUNT(DISTINCT recipient_hash) >= max_recipients`, reject (new internal
   `PolicyErrorKind::RecipientLimit`). (A deposit to an EXISTING recipient does
   not increase the distinct count, so it is exempt from this check.)
3. NEW global cap: if `SUM(LENGTH(ciphertext)) over ALL rows + new_len >
   global_storage_cap_bytes`, evict globally-EXPIRED rows oldest-first
   (`evict_expired_global`), re-check; if still over → reject (new internal
   `PolicyErrorKind::ServerFull`).

All checks + the insert remain in ONE transaction so a rejection never leaves a
partial state (mirrors the existing per-recipient atomicity). The new
`evict_expired_global(tx, target_bytes, now)` parallels `evict_expired_for` but
is not recipient-scoped. No non-expired eviction is ever performed.

`Store::insert` gains two parameters: `global_storage_cap_bytes: u64` and
`max_recipients: u64` (threaded from `Policy` by the dispatch handler, like the
existing `recipient_cap_bytes`).

### A2 — Bounded `Delete.deposit_ids`

In `handle_delete` (`dispatch.rs`): if `delete.deposit_ids.len() as u64 >
policy.max_delete_ids`, return a policy rejection BEFORE touching the store
(reusing the existing wire `ErrorCode` for a rejected/malformed request — see
Wire neutrality). Prevents an unbounded per-request loop / transaction.

### A3 — Idle-connection timeout

In `accept_loop` (`server.rs:78`): wrap the `framed.next().await` in
`tokio::time::timeout(Duration::from_secs(policy.idle_timeout_secs as u64), ...)`.
On `Err(Elapsed)`, close the connection (return `Ok(())`) — an idle client
holding a slot is shed. A `None` (clean EOF) and codec/IO handling are unchanged.

### A4 — Per-server connection semaphore

`MailboxServer` gains an `Arc<tokio::sync::Semaphore>` sized `max_connections`
(built in `MailboxServer::new` from the policy). A new transport-agnostic method
`serve_connection(stream)` does `try_acquire_owned()`:
- permit acquired → hold it for the connection's lifetime and run `accept_loop`,
  releasing on return;
- no permit (server at capacity) → SHED: log at `debug`, drop the stream
  (closes it), return immediately. Load-shedding, not queuing — queuing under
  flood defeats the bound.

`arti.rs` calls `server.serve_connection(data_stream)` instead of
`server.accept_loop(...)` directly, so the bound lives in the duplex-testable
library, not the Tor-only path. `accept_loop` itself stays public for the
existing tests; `serve_connection` wraps it.

---

## Part B — Daemon inbound accept-loop spawn bound (1B TODO)

In `run_accept_loop` (`accept.rs:23`):
- A bounded `Arc<tokio::sync::Semaphore>` (a module const, e.g.
  `MAX_INFLIGHT_HANDSHAKES = 64`). For each inbound stream, `acquire_owned()` a
  permit BEFORE `tokio::spawn` — the loop awaits an available permit, providing
  backpressure so the number of concurrent handshake tasks is bounded. The
  permit moves into the spawned task and releases on completion (drop).
- Spawn handshake tasks into a `tokio::task::JoinSet` instead of detaching them.
  When the loop exits (inbound source closed / transport shutdown), drain the
  JoinSet (await/abort) so in-flight handshakes don't dangle past shutdown.
- Remove the `// TODO(phase-2 transport hardening)` comment block.

Behavior change is concurrency-bounding only; an authorized peer's handshake +
ingest path is unchanged. Because acquisition is awaited (backpressure) rather
than shed, no legitimate inbound is dropped — a flood merely serializes behind
the bound, and each task is onion-gated + handshake-timeout-bounded as today.

---

## Error handling

- All caps are best-effort REJECTIONS, never panics. Over-cap / oversize
  requests reply an `Error` frame (existing wire `ErrorCode`) and the connection
  stays open — matching the server's existing "reject ≠ close" contract.
- The connection semaphore sheds by closing only the NEW stream; existing
  connections are unaffected.
- Store cap checks remain inside the single insert transaction (no partial
  state on rejection).
- No `unwrap`/`expect` in non-test code (mailbox uses `MailboxError`; core uses
  `CoreError`). No secrets/onions/ciphertext logged above `debug`.

## Wire neutrality (ADR 0006 frozen)

New internal `PolicyErrorKind` variants (`ServerFull`, `RecipientLimit`) and the
oversize-`Delete` rejection MUST map, via `error_frame()`, to an EXISTING
mailbox `ErrorCode` — the same code the client already treats as "rejected, keep
in outbox and retry." The client's behavior is identical regardless of
recipient-vs-server-vs-malformed cause, so no new wire `ErrorCode` value is
added. The implementation plan verifies the exact existing code to reuse (the
`RecipientFull` mapping is the model). If no suitable code exists, that is an ADR
0006 question to escalate BEFORE coding — but the expectation is reuse.

## Security posture

- **Durability preserved:** reject-after-expired means an accepted, non-expired
  message is never evicted; rejected deposits are retried by the 2.C sender.
- **Layered defense:** token buckets + per-recipient cap (first line) → global
  byte cap + recipient-count cap (disk backstop) → connection semaphore + idle
  timeout (connection backstop) → bounded Delete (per-request backstop).
- **No metadata leak:** no new wire fields; rejections reuse existing codes;
  logging stays redaction-safe (onions only at `debug`).
- The daemon accept-loop bound closes an unbounded-task-spawn vector while
  preserving onion-gating + handshake timeout.

## Out of scope (unchanged deferrals)

- At-rest DB encryption (T1-2) — Phase 2.B.
- Real onion-key rotation (Task 23.5), multi-member groups,
  metadata-minimization, third-party audit — v1.1+.
- Mailbox-server metrics/alerting beyond existing counters — operations, Phase 4.

## Exit criteria

1. The mailbox server enforces a global byte cap, a recipient-count cap, an
   idle-connection timeout, a concurrent-connection ceiling, and a bounded
   `Delete` length — all operator-tunable with sane defaults.
2. Under an anonymous flood and a targeted victim-fill, disk stays bounded and a
   fresh legit deposit still lands once expiry/space frees up (no permanent
   victim lockout via eviction; rejected deposits retried by the sender).
3. An oversize `Delete` is rejected; an idle connection is closed; concurrent
   connections beyond `max_connections` are shed.
4. The daemon inbound accept loop bounds concurrent handshakes and drains
   in-flight tasks on shutdown.
5. Wire-format neutral (no new `MailboxFrame`/`Command`/`CommandResult`/`Event`/
   `ErrorCode` wire variants; ADR 0006 untouched).
6. `cargo fmt --check`, `cargo clippy --workspace --exclude skattr-ui
   --all-targets --all-features -- -D warnings`, the full core + mailbox + tests
   suites (single-threaded) all green.

## Delivery model

`spec (this doc) → writing-plans → subagent-driven execution → two-stage review
per task → verification → finish branch`.
