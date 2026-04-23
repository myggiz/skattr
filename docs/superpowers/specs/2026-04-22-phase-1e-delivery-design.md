# Phase 1.E Delivery Semantics — Design Spec

**Status:** Approved 2026-04-22. Scope locked by `docs/superpowers/specs/2026-04-21-phase-1-decomposition.md` §1.E.
**Depends on:** 1.A (frame codec), 1.B (Noise handshake + `AuthenticatedConnection`), 1.C (MLS 2-member groups).
**Exit criterion (verbatim from decomposition):** "Outbox + exponential backoff + ACK handling + receiver dedup + connection pool; kill-mid-message then reconnect delivers the message."

## 1. Scope

In scope:

- Persisted outbox with exponential-backoff retry and ACK correlation by `MessageId`.
- A per-peer actor-based connection pool, same actor type for inbound and outbound.
- Receiver-side dedup over the existing `seen_messages` 24 h sliding window.
- Keepalive ping/pong and idle-close inside the per-peer actor.
- An integration test (`tokio::io::duplex` + a killable-stream wrapper) that directly exercises the kill-mid-message → reconnect → exactly-once-delivered flow.
- A second `#[ignore]`-gated real-Tor integration test that proves the same stack composes end-to-end at the Tor layer.

Out of scope (owned by later sub-projects):

- Mailbox-deposit fallback path. Phase 2 owns offline delivery (`skattr-implementation-plan.md` §Phase 2).
- CLI wiring of `send`, `tail`, etc. Phase 1.F.
- Full message history / FTS. Phase 1.G.
- MLS epoch advance policy (24 h or 100 msgs). 1.C's work; 1.E neither advances epochs nor blocks on them.

## 2. Decisions locked during brainstorming

| # | Decision | Rationale |
|---|----------|-----------|
| D1 | Per-peer actor task per live peer, owning `Option<AuthenticatedConnection<DataStream>>` | `AuthenticatedConnection` is not split-readable; one task owning both halves avoids locking and gives per-peer ordering for free |
| D2 | ACK correlation = in-memory `HashMap<MessageId, oneshot::Sender>` on the actor, with outbox as retry source-of-truth | Gives the daemon a prompt `Event::DeliveryStatusChanged { status: Delivered, .. }` while still surviving actor/conn restart via the persisted outbox |
| D3 | Kill-mid-message test uses `tokio::io::duplex` + a `KillableStream<S>` wrapper, runs in CI on every build; a separate `#[ignore]`d real-Tor smoke test lives alongside `arti_echo.rs` | Duplex test is fast and exercises every layer above raw bytes; Tor test costs little to keep and catches Tor-specific regressions |
| D4 | Migration 0004 adds `message_id BLOB NOT NULL` + `UNIQUE(target, message_id)` to `outbox` | Idempotent enqueue and ACK-by-id without a separate correlation table |
| D5 | Direct-only delivery; no mailbox fallback in 1.E | Phase 2 owns offline delivery |
| D6 | Encrypt-then-persist: `Group::encrypt` is called by the caller of `Daemon::send` *before* outbox insert; retry re-sends the stored ciphertext | MLS ratchet advances exactly once per message; re-encryption on retry would silently corrupt state |

## 3. Architecture

```
                         ┌──────────────────────────────────────┐
  Daemon::send(peer,     │            DeliveryHub               │
    envelope) ──────────▶│  (per-daemon actor, owns pool)       │
                         │                                      │
                         │  - HashMap<PublicKey,                │
                         │       mpsc::Sender<DeliveryJob>>     │
                         │  - spawns PeerConnection actors      │
                         │    on first send to a peer           │
                         │  - periodic seen_messages sweep      │
                         └──┬───────────────────────────────────┘
                            │ mpsc (cap 64 per peer)
                            ▼
                    ┌──────────────────────────────────────┐
                    │  PeerConnection actor (one per peer) │
                    │                                      │
                    │  - owns Option<AuthenticatedConn>    │
                    │  - pending ACKs:                     │
                    │       HashMap<MessageId, oneshot>    │
                    │  - select! {                         │
                    │      DeliveryJob from hub,           │
                    │      frame from conn.recv(),         │
                    │      retry_tick (1 s),               │
                    │      keepalive_tick (60 s),          │
                    │      idle_timeout (180 s),           │
                    │      cancel_token                    │
                    │    }                                 │
                    └──────────────────────────────────────┘
                            │
                            ▼ (on demand)
                TorRuntime::connect + noise::handshake_initiator
```

Inbound side (existing `OnionListener` → `DataStream`): Listener → accept task → `noise::handshake_responder` → `DeliveryHub::ingest(peer_pubkey, conn)`. If an actor already exists, the newer connection replaces it (the older one is closed). Same `PeerConnection` actor type services both directions.

Module layout:

| Path | Status | Purpose |
|------|--------|---------|
| `delivery/hub.rs` | new (replaces `delivery/sender.rs`) | `DeliveryHub` router; no I/O |
| `delivery/peer.rs` | new | `PeerConnection` actor |
| `delivery/outbox.rs` | exists, stubbed | `Outbox` wrapper + `backoff()` |
| `delivery/backoff.rs` | new | `fn backoff(attempts: u32) -> Duration` + unit tests (split out for clarity) |
| `delivery/receiver.rs` | exists, stubbed | `receive` + `build_ack` |
| `delivery/mod.rs` | exists | Re-exports, declares submodules |
| `storage/outbox.rs` | exists | Adapt API to `message_id` column |
| `storage/migrations/0004_outbox_message_id.sql` | new | Schema change |

`delivery::sender` is removed; its `Sender` type was a vestigial stub. All items remain `pub(crate)`; no new public API on `core::lib`.

## 4. Data flow

### 4.1 Outbound send (Alice → Bob)

1. `Daemon::send(bob_pubkey, Envelope { id: MID, .. })`.
2. Resolve Bob's `Group` via `storage::groups`; `Group::encrypt(&envelope) → ciphertext`. Ratchet advances here.
3. `OutboxRepo::insert(target=bob, message_id=MID, payload=ciphertext, next_retry_at=now)`. Idempotent: duplicate `(target, MID)` returns the existing rowid without re-insert (`INSERT … ON CONFLICT DO NOTHING`).
4. `DeliveryHub::send(bob, MID, ciphertext) → oneshot<Result<()>>`. Hub looks up or spawns the per-peer actor, forwards a `DeliveryJob { message_id, ciphertext, ack_tx }`.
5. Actor: ensure conn (dial + handshake if `None`); `conn.send(Frame::MlsApp(ciphertext))`; store `ack_tx` in `pending_acks[MID]`.
6. On inbound `Frame::Ack(MID)`: remove from `pending_acks`, fire `ack_tx`, `OutboxRepo::ack_by_message_id(bob, MID)`, emit `Event::DeliveryStatusChanged { message: MID, status: DeliveryStatus::Delivered }`.

### 4.2 Retry tick

Per-peer actor runs `tokio::time::interval(Duration::from_secs(1))`. On each tick:

- `OutboxRepo::due(target=self.peer, now, limit=32)`.
- For each row whose `message_id` is *not* already in `pending_acks`, re-send (reconnect first if needed); register a fresh `oneshot`; bump `reschedule(id, now + backoff(attempts))`.
- Same ciphertext re-sent. MLS ratchet unchanged.
- Failures during re-send leave the row alone; next tick tries again.

Retries are per-peer, bounded by outbox row count, and ordered FIFO by `next_retry_at`.

### 4.3 Inbound receive (Bob)

1. `OnionListener::accepted.recv() → DataStream`.
2. `noise::handshake_responder(stream, identity) → AuthenticatedConnection`. Resolve `peer_x25519` to `PublicKey` via the ContactCard lookup (iterate contacts, bridge Ed25519 → X25519, compare).
3. `DeliveryHub::ingest(peer, conn)`. If no actor exists, spawn. If one does (peer dialed concurrently): older actor closes its conn, new one takes over — the hub sends a `ReplaceConn { new_conn }` message, actor drains `pending_acks` and swaps.
4. Actor `select!` on `conn.recv()`:
    - `Frame::MlsApp(ct)` → `Group::decrypt(ct) → Envelope` → `receiver::receive(peer, envelope)`:
        - `ts` out of ±1 h → `ReceiveOutcome::Rejected`, log `warn`, do **not** ACK.
        - `SeenMessagesRepo::insert(peer, id, now)` returns `false` → `ReceiveOutcome::Duplicate`, **do** send `Frame::Ack(id)`, do not re-insert into `messages`.
        - Insert returns `true` → `MessagesRepo::insert(...)`, emit `Event::MessageReceived`, send `Frame::Ack(id)`.
    - `Frame::Ack(id)` → fire oneshot + `OutboxRepo::ack_by_message_id`.
    - `Frame::Ping` → reply `Frame::Pong`.
    - `Frame::Pong` → reset keepalive-timeout timer.
    - `Frame::Bye` → actor closes conn, exits; hub removes channel.
    - Anything else → log `warn`, drop frame (don't kill the connection over one bad frame).

### 4.4 Exactly-once property under kill-mid-message

Property: a successful `Daemon::send` that crosses the wire lands in the receiver's `messages` table exactly once, regardless of where the connection dies.

- Alice enqueues row `R(MID)`, actor sends frame.
- Case A — frame lost: Bob never saw it. Retry tick re-sends. Bob's `seen_messages.insert` returns `true`, inserts into `messages`, ACKs. Alice acks row `R`.
- Case B — frame delivered, ACK lost. Bob already has `(alice, MID)` in `seen_messages` and the row in `messages`. Retry tick re-sends same ciphertext. Bob decrypts, `seen_messages.insert` returns `false`, Bob sends fresh `Frame::Ack(MID)` anyway, row `R` removed on Alice side.
- Case C — actor/daemon restart between encrypt and outbox insert: unrecoverable, error returned to caller, MLS ratchet state for that message is lost. Acceptable; storage-level failure is a fatal daemon condition.

## 5. Persistence

### 5.1 Migration 0004

File: `crates/core/src/storage/migrations/0004_outbox_message_id.sql`.

```sql
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz AB
--
-- Skattr storage schema, version 4.
-- Add per-message id to the outbox so the delivery layer can
-- correlate inbound ACKs to rows without a separate lookup table
-- and so enqueues are idempotent per (target, message_id).

INSERT OR IGNORE INTO schema_version (version) VALUES (4);

ALTER TABLE outbox
    ADD COLUMN message_id BLOB NOT NULL
    DEFAULT (x'00000000000000000000000000000000');

CREATE UNIQUE INDEX IF NOT EXISTS idx_outbox_target_message_id
    ON outbox(target, message_id);
```

The `DEFAULT` exists only to satisfy SQLite's "add NOT NULL column" rule; no pre-1.E rows are written to `outbox`, so no real row takes the default value.

### 5.2 `storage::outbox::OutboxRepo` API

Replaces the existing stub API; callers in `delivery/` are the only consumers.

```rust
/// A row read back from the `outbox` table:
/// `(id, target, payload, message_id, attempts)`.
pub type OutboxRow = (i64, Vec<u8>, Vec<u8>, [u8; 16], u32);

pub(crate) struct OutboxRepo<'p> { pool: &'p Pool }

impl<'p> OutboxRepo<'p> {
    pub(crate) fn new(pool: &'p Pool) -> Self;

    /// Idempotent insert. Returns `Some(rowid)` on fresh insert,
    /// `None` if `(target, message_id)` was already present.
    pub(crate) fn insert(
        &self,
        target: &[u8],
        message_id: &[u8; 16],
        payload: &[u8],
        next_retry_at: i64,
    ) -> Result<Option<i64>>;

    pub(crate) fn due(&self, now: i64, limit: usize) -> Result<Vec<OutboxRow>>;

    /// Returns `true` if a row was deleted.
    pub(crate) fn ack_by_message_id(
        &self,
        target: &[u8],
        message_id: &[u8; 16],
    ) -> Result<bool>;

    pub(crate) fn reschedule(&self, id: i64, next_retry_at: i64) -> Result<()>;
}
```

### 5.3 `delivery::outbox::Outbox` wrapper

Thin `pub(crate)` layer on `OutboxRepo`; exists so `delivery/` code doesn't touch the storage crate directly.

```rust
pub(crate) struct Outbox<'p> { repo: OutboxRepo<'p> }

impl<'p> Outbox<'p> {
    pub(crate) fn enqueue(&self, entry: OutboxEntry) -> Result<()>;
    pub(crate) fn due(&self, max: usize) -> Result<Vec<OutboxEntry>>;
    pub(crate) fn ack(&self, target: &PublicKey, message_id: MessageId) -> Result<()>;
    pub(crate) fn reschedule(&self, id: i64, attempts_now: u32) -> Result<()>;
}
```

`OutboxEntry` keeps its existing shape (`id`, `target: PublicKey`, `payload: Vec<u8>`, `message_id: MessageId`, `attempts: u32`, `next_retry_at: i64`); `reschedule` computes the new `next_retry_at` via `backoff(attempts_now)`.

### 5.4 Seen-messages sweep

`SeenMessagesRepo` already implements `sweep_older_than(cutoff)`. `DeliveryHub` spawns a secondary task on start:

```rust
let mut sweep = tokio::time::interval(Duration::from_secs(3600));
loop {
    select! {
        _ = cancel.cancelled() => break,
        _ = sweep.tick() => {
            let cutoff_ms = now_ms() - 24 * 3600 * 1000;
            let _ = seen_repo.sweep_older_than(cutoff_ms);
        }
    }
}
```

No schema change.

## 6. Error handling & lifecycle

### 6.1 Error taxonomy

| Source | Variant | Actor behaviour |
|--------|---------|-----------------|
| `TorRuntime::connect` fail | `CoreError::Transport` | Pending job's oneshot resolves with `Err`; row stays; retry tick picks up later |
| `noise::handshake_*` fail | `CoreError::Transport` | Same as connect fail |
| `conn.send` / `conn.recv` fail mid-stream | `CoreError::Transport` | Drain `pending_acks` (drop oneshots); `conn = None`; next retry tick redials |
| `Group::encrypt` fail | `CoreError::Mls` | Surfaces to `Daemon::send` caller; **never** inserted into outbox, no wasted ratchet row |
| `Group::decrypt` fail on inbound | `CoreError::Mls` | `warn!` + drop frame; do **not** exit actor |
| `OutboxRepo::insert` fail after encrypt | `CoreError::Storage` | Fatal: caller gets error, MLS ratchet-state for this one message is lost. Acceptable; storage failure is already unrecoverable elsewhere |
| `SeenMessagesRepo::insert` fail | `CoreError::Storage` | Log `error!`, drop frame, no ACK. Caller will retry; if storage is truly dead, retry storms harmlessly |

### 6.2 Connection lifecycle

- **Cold start:** job arrives, `conn == None` → dial + handshake. On failure, oneshot resolves `Err`, no dial retry inside the same tick — retry tick handles it.
- **Reconnect:** triggered by send/recv error. Actor nulls conn, drops all `pending_acks` oneshots. Subsequent retry tick handles redial.
- **Idle close:** `tokio::time::sleep(Duration::from_secs(180))` branch of `select!` — if no frames in 3 min, `conn.close().await`; actor stays alive for next job.
- **Keepalive:** `tokio::time::interval(Duration::from_secs(60))` — send `Frame::Ping` when `conn.is_some()`. Missing `Pong` for 30 s → treat as dead conn.
- **Shutdown:** `DeliveryHub` owns a `tokio_util::sync::CancellationToken`; drop hub → all actors see cancel → `conn.close()` → exit. Sweep task same.

### 6.3 Backpressure

- Per-peer `mpsc::Sender<DeliveryJob>` capacity = 64. Full → `Daemon::send` awaits. Outbox is durable queue; channel just paces.
- Hub's own control mpsc (`RegisterInbound`, etc.) capacity = 16.
- `OnionListener::accepted` capacity is caller-set; hub drains eagerly.

### 6.4 Ordering

- Per-peer FIFO: guaranteed. One actor, one send loop. Within a group this aligns with MLS generation numbers because `Daemon::send` callers serialise their `encrypt → insert → hub::send` in issue order.
- Across-peer ordering: not preserved; not required.

## 7. Testing

### 7.1 Unit tests

In-module tests using `Pool::in_memory()` where storage is touched.

- `delivery::backoff`
  - `backoff(0..=8)` — monotone non-decreasing; caps at 5 min; sampled 1000 × within ±25 % jitter band.
- `delivery::outbox`
  - `enqueue` idempotent per `(target, message_id)` (second enqueue is a no-op).
  - `due` respects `next_retry_at`; `ack` removes exactly one row.
  - `reschedule` bumps `attempts`.
- `delivery::receiver`
  - `receive` rejects `ts` beyond ±1 h.
  - First insert returns `New`; second returns `Duplicate`; `ts`-out-of-window returns `Rejected`.
  - `build_ack` returns the input id.
- `delivery::peer` (uses `tokio::io::duplex(64 * 1024)`; no real Tor)
  - Dial path: hub spawns actor, actor runs handshake, round-trips one `MlsApp` + `Ack`.
  - Idle close under `tokio::time::pause/advance` past 180 s → conn dropped; next job re-dials.
  - Keepalive: with `pause/advance`, pong missing 30 s after ping → actor marks conn dead.
  - `ReplaceConn` drains pending ACKs cleanly.
- `delivery::hub`
  - First send to a peer spawns an actor; second send routes to the same channel.
  - `ingest` for a peer with an existing actor triggers `ReplaceConn`.
- `storage::outbox` (migration golden)
  - Apply 0001 → 0004 on a fresh `Pool::in_memory()`; assert `message_id` column exists, unique index present.
  - Round-trip: `insert` → `due` → `ack_by_message_id` → row gone.

### 7.2 Integration tests

`crates/tests/src/delivery_kill_mid_message.rs` — **runs in CI**, no `#[ignore]`.

Helpers in `core::test_exports` (gated on `test-harness`):

- `pub fn spawn_paired_daemons_over_duplex() -> (DaemonHandle, DaemonHandle, KillSwitch)`
  - Build two in-memory daemons (in-memory `Pool`, ephemeral identity). Connect them via `tokio::io::duplex(64 * 1024)` wrapped in `KillableStream`.
  - `KillableStream<S: AsyncRead + AsyncWrite>` holds `Arc<AtomicBool>`; any `poll_read` / `poll_write` after the bool flips returns `io::Error::from(io::ErrorKind::BrokenPipe)`.
  - Returns handles to both daemons and the shared `KillSwitch`.

Test body:

1. Alice creates solo group, adds Bob via test-exported key-package path, exchanges one warm-up message to prime ratchet.
2. Alice `Daemon::send(bob, Envelope { id: MID, .. })`. Wait until Alice's `MlsApp` has been written (hook exposed via `test_exports::next_frame_sent()`).
3. `kill_switch.kill()` before Bob's ACK reaches Alice.
4. Assert: Alice outbox row for `MID` present; Bob `messages` count for this sender — may be 0 or 1; `seen_messages` matches `messages`.
5. Rebuild the duplex pair (replace transports on both daemons via `test_exports::swap_transport`), wait for fresh handshake + retry tick.
6. Assert: Alice outbox row gone; Bob's `messages` count == 1; an `Event::DeliveryStatusChanged { message: MID, status: DeliveryStatus::Delivered }` event observed on Alice's broadcast channel.

Second test variant in the same file: `kill_before_any_frame_sent` — flip kill switch before Alice writes any frame. Retry tick dials fresh, delivers, Bob `messages` count == 1.

`crates/tests/src/delivery_real_tor.rs` — **gated `#[ignore]`**, run via `cargo test -p skattr-tests --release -- --ignored`.

- Two daemons over real Arti (like `arti_echo.rs`).
- Alice sends 5 messages, all delivered. No kill scenario. Proof of composition at the Tor layer.

### 7.3 Coverage vs exit criterion

| Exit-criterion clause | Covered by |
|-----------------------|-----------|
| Outbox | unit (delivery::outbox) + golden migration + both integration tests |
| Exponential backoff | unit (delivery::backoff) |
| ACK handling | integration (`DeliveryStatusChanged { status: Delivered }` fires after retry-redelivery) + peer unit |
| Receiver dedup | integration (`messages` row count stays at 1 on re-delivery) + receiver unit |
| Connection pool | peer unit (idle close, reconnect) + hub unit (actor spawn/route) + integration uses same path |
| Kill-mid-message → reconnect → delivered | `delivery_kill_mid_message.rs` |

## 8. Dependencies & risks

- No new external crates. Everything uses `tokio`, `tokio-util`, `futures`, `rand`, `arti-client`, `snow`, `openmls`, `rusqlite` — all already in the workspace.
- Risk: `tokio::io::duplex` + a killable wrapper must produce the same `AsyncRead + AsyncWrite` shape that `TorRuntime::connect` returns (`arti_client::DataStream`). Mitigation: `PeerConnection` is generic over `S: AsyncRead + AsyncWrite + Unpin + Send + 'static`; production uses `DataStream`, tests use `KillableStream<DuplexStream>`. Already the pattern used by `AuthenticatedConnection<S>` in 1.B.
- Risk: MLS `Group::encrypt` advancing the ratchet before outbox insert means a storage-insert failure loses that ratchet step. Accepted — see §6.1.
- Risk: a malformed inbound frame must not DoS an actor. Covered: unknown frames are `warn`ed and dropped; only explicit protocol violations (e.g. receiving a `NoiseInit` on an already-authenticated connection) close the actor.
- Risk: concurrent outbound + inbound dial to the same peer (both sides try to dial each other at once). Covered by `DeliveryHub::ingest`'s `ReplaceConn` semantics — the newer connection wins, the older is drained and closed.

## 9. Non-goals

- No mailbox deposit.
- No CLI surface. `Daemon::execute(Command::Send { .. })` is not wired in 1.E — 1.F owns the Command plumbing. 1.E adds a `pub(crate) async fn Daemon::send(&self, peer, envelope)` plus a matching `test_exports::send(daemon, peer, envelope)` shim (gated on the `test-harness` feature) so integration tests in `crates/tests/` can trigger delivery.
- No per-message size limits beyond the 16 MiB frame cap and the 64 KiB Envelope cap already established by 1.A / the decomposition doc.
- No group-chat fan-out; 1.C locks 2-member-only and 1.E inherits that.
- No delivery status persisted across daemon restart beyond "is in outbox" / "is not in outbox". Phase 1.G revisits.

## 10. Open questions — none

Everything surfaced during brainstorming is answered above. If something surfaces during plan writing, revise this doc first.
