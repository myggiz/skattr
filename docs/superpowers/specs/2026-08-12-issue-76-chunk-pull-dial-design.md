# #76 — the attachment chunk-pull path cannot dial (design)

**Issue:** #76 (`bug`, `attachments`, `tests`, milestone v0.1.2) — High.
**Branch:** `76-chunk-pull-dial` (created; carries the red test `cdbcc5c`).
**Relates to:** #114 (the observability gap that hid this), #144/#146 (failed-attachment retry), #149 (the 14-day janitor that is now the terminal state), #99 (Arti circuit collapse — a *trigger* for this bug, not its cause).

**No wire-format change, no migration, no new dependency.** One new `InboundDispatch` method (`pub(crate)` trait, default impl) and one new `ChunkRx` method.

---

## 1. Problem

A received attachment's manifest arrives and the file never does. The receiver reports `⚠️ request timeout`; the sender logs **zero** `ChunkRequest`s. Both logs are accurate — and that contradiction is the whole bug.

Three facts in the current code compose into it:

**1. `send_chunk_requests` cannot report that it sent nothing** (`peer.rs:83-104`):

```rust
let Some(c) = conn.as_mut() else { return false };
```

The `bool` is discarded at every call site — `peer.rs:147`, `727`, `814`, `1010`, `1020`, all `let _ = …`. "Could not send" and "sent" are indistinguishable to the caller.

**2. The retry budget is spent on requests that were never transmitted** (`chunk_transfer.rs:145-158`). `timed_out` increments `attempts` and re-arms `sent_at` unconditionally; it never learns whether the transmit happened:

```rust
if f.attempts >= CHUNK_RETRY_BUDGET { return ChunkAction::Fail; }
f.attempts += 1;
```

With `CHUNK_RETRY_BUDGET = 3` and `CHUNK_REQUEST_TIMEOUT = 30s`, a fetch with no connection burns ~90 s of pure bookkeeping, then fails as `"request timeout"` (`peer.rs:731`).

**3. Nothing on the chunk path dials.** `ensure_conn` (`peer.rs:1138`) is called at exactly two sites — `591` (`DeliveryJob`) and `622` (`WelcomeJob`). The retry tick that drives chunk timeouts says so itself (`peer.rs:661-662`):

> `// Dial-on-demand still happens on the next job send; this tick never dials on its own.`

### The two observed symptoms

- **`request timeout`, sender saw nothing.** The fetch starts over a live connection (the manifest arrived on it), the connection then goes away — `PONG_DEADLINE` 30 s (`peer.rs:786`), `IDLE_CLOSE` 180 s (`peer.rs:775`), or a collapsed Tor circuit — and the retries run to exhaustion against `None`.
- **Bubble stuck at icon+filename, never failing.** If there is no connection when the begin is queued, the drain is gated on `conn.is_some()` (`peer.rs:754`) and the begin waits in the dispatcher indefinitely.

### Why this survived the test suite

The only integration coverage runs over loopback, and **loopback connections do not die**. The bug requires the connection to be absent or to vanish mid-pull. This is also why the #115 real-Tor run passed: fresh circuit, request went out immediately, chunk returned inside the window.

**Status:** root cause is test-proven. `cdbcc5c` adds `inbound_fetch_dials_when_there_is_no_connection`, which fails on zero dial attempts across 60 s of paused time — past the entire retry budget.

---

## 2. The architectural decision this changes

The existing actor has a consistent, unstated principle: **a dial happens only when the local user does something.** Background retry never dials — the outbox tick bails on a missing connection rather than creating one (`peer.rs:672`):

```rust
let Some(c) = conn.as_mut() else { break; };
```

That principle is sound for messages, because a message that cannot go direct has an escape hatch: the sustained-failure timer hands it to the mailbox fallback. The chunk pull is the one piece of background work with **no local user action behind it and no escape hatch** in a mailbox-less deployment, so it dead-ends.

This spec makes a narrow exception: **pending chunk work may dial, rate-limited.** The message path is deliberately left alone (§6).

---

## 3. Part A — tell the truth about transmission

Every `send_chunk_requests` call site consumes the returned `bool` instead of discarding it. A `false` return means the frames did not reach the wire, and the caller reacts (Part B) rather than recording a fiction.

---

## 4. Part B — spend attempts only on real attempts

New method on `ChunkRx`:

```rust
/// Roll back bookkeeping for requests that were never transmitted:
/// return them to `needed` so they are re-requested, and charge no attempt.
pub(crate) fn unsent(&mut self, indices: &[u32]);
```

**It must return each index to `needed`, not merely drop it from `inflight`.** `next_requests` *moves* indices out of the `needed` queue (`pop_front`) into `inflight` (`chunk_transfer.rs:94-110`), so an index removed from `inflight` alone would be absent from **both** collections — never re-requested, and `is_complete()` (`received >= total`) never satisfiable. That is a silent permanent deadlock, strictly worse than the bug being fixed.

```rust
for &i in indices {
    if self.inflight.remove(&i).is_some() {
        self.needed.push_front(i);   // front: retry promptly, preserve rough order
    }
}
```

The index is then neither received nor in flight, so the next `next_requests()` re-selects it immediately — no 30 s wait for a timeout that measures nothing, and no attempt consumed.

The `inflight.remove(&i).is_some()` guard matters: it makes `unsent` idempotent and stops an index that was already resolved (a `Chunk` that arrived between the failed send and the rollback) from being pushed back into `needed` and re-fetched.

`"request timeout"` consequently regains its meaning: **we asked the peer and it stayed silent.**

**Accepted trade-off, stated deliberately:** for a resend that had already timed out legitimately, the rollback also discards that attempt history, so a chunk can receive more than `CHUNK_RETRY_BUDGET` real attempts. This is the correct side to err on — the budget exists to detect *peer silence*, and a failed local send is not evidence about the peer — but it is a loosening, not an oversight. The alternative (preserving `attempts` while restoring the previous `sent_at`) requires keeping a value the struct does not store, for a case that only makes a stuck transfer fail sooner.

---

## 5. Part C — dial on demand, with capped backoff

In the retry tick's `chunk_enabled` block: when there is pending chunk work, no connection, and the cooldown has expired, call `ensure_conn`.

```
BACKOFF: [15s, 60s, 300s, 900s]  — then hold at 900s
```

Reused from `chunk_sweep` (`chunk_sweep.rs:22`) so the codebase has one backoff shape rather than two. A successful dial resets the schedule to the start; a failure advances it. State is actor-local (`full_run` locals), not persisted — a restarted actor gets a fresh schedule, which is correct, since a restart usually means conditions changed.

Pacing rationale: a failed Tor dial already blocks the actor inline for up to `DIAL_TIMEOUT` = 30 s (`dial.rs:21`), so an un-paced tick (`RETRY_TICK` = 1 s) would be a dial storm against a dead peer. The first 15 s step still reconnects promptly in the common case — a connection that just dropped while both peers are up.

### Detecting "pending chunk work" without consuming it

Two forms, and only one is side-effect-free to observe:

- `active_rx.is_some()` — a fetch in progress whose connection died. **This is the field case.**
- A begin still in the dispatcher, never started. `take_begin_attachment` **consumes**, and the drain is gated on `conn.is_some()`.

So the second form needs a non-consuming probe. New `InboundDispatch` method, defaulting to `false` like the other optional members:

```rust
fn has_pending_begin(&self, _peer: PublicKey) -> bool { false }
```

**Rejected alternative:** draining begins into the actor's local `rx_queue` regardless of connection. It needs no new API, but it moves begins out of dispatcher state into actor-local memory, where an actor crash loses them — trading durability for a smaller diff.

### Interactions

- **Simultaneous dials.** Both peers may now dial at once, yielding two connections; the hub's `ReplaceConn` already handles this, and the backoff bounds the churn. Pre-existing possibility, not introduced here.
- **Inline blocking.** A dial blocks the actor's `select!` for up to 30 s. This is existing behaviour for `DeliveryJob` (`peer.rs:591`); a `ReplaceConn` arriving meanwhile waits in the buffered ctrl channel rather than being lost.

---

## 6. What this does *not* do

- **No UI "waiting for peer" state.** It would need new IPC surface and event plumbing; the core fix stands alone.
- **No change to the mailbox/offline (3.C) lane.** It already covers the genuinely-offline case where a mailbox is configured. This fix is what makes a *direct-only* deployment work — the configuration both field-test machines actually had.
- **No change to the message path's dial policy**, despite it having the same background-never-dials property. Queued messages have the mailbox fallback as their escape hatch and are not dead-ended. Changing them is a separate decision with its own risk.
- **No new terminal state.** A pull that cannot connect stays `'pending'` and keeps trying; #149's 14-day janitor remains the terminal, and #146's retry still resumes from held chunks.

---

## 7. Testing

1. **The red test goes green** — `inbound_fetch_dials_when_there_is_no_connection` (`cdbcc5c`): actor spawned with `conn: None`, a working dialer, one pending begin; asserts a dial happens and a `ChunkRequest` reaches the wire. This is the primary proof.
2. **Backoff is respected** — a dialer that always fails; assert attempts follow the schedule over simulated time rather than once per `RETRY_TICK`.
3. **No false `request timeout`** — no connection, an active fetch; assert `attachment_failed` is **not** emitted after 3 × 30 s. Direct regression guard for the misleading error that cost the field diagnosis.
4. **A live connection still behaves** — the existing chunk tests (resume-on-`ReplaceConn`, retry-requeued-begin, completion/CAS) must pass unchanged; no dial should occur when a connection is present.
5. **`unsent` returns indices to the queue** — a `ChunkRx` unit test: request a window, roll it back with `unsent`, and assert the same indices come back from `next_requests()` and that the transfer can still reach `is_complete()`. This pins the deadlock described in §4, which is invisible at the actor level (it presents as a transfer that simply never finishes).

All four run on paused time (`#[tokio::test(start_paused = true)]`), so they are deterministic and instant rather than wall-clock waits.

**Gate:** `cargo fmt --all -- --check`, `cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings`, `cargo test`, `cargo deny check`. CI runs the UI job on the PR.

---

## 8. Acceptance

| # | Criterion | Where |
|---|---|---|
| 1 | A pending inbound attachment with no connection causes a dial | §5, test 1 |
| 2 | A fetch whose connection dies mid-transfer recovers instead of timing out | §5 (`active_rx` arm), test 3 |
| 3 | `"request timeout"` is reported only when a request was actually transmitted | §3+§4, test 3 |
| 4 | Dial attempts against an unreachable peer are paced, not per-tick | §5, test 2 |
| 5 | A begin that never started is detected without consuming it | §5 (`has_pending_begin`) |
| 6 | Existing chunk-transfer behaviour over a live connection is unchanged | test 4 |
