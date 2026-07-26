# #114 — Attachment observability + honest sender status (design)

**Issue:** #114 (`enhancement`, `attachments`, `ux`, milestone v0.1.2)
**Branch:** `114-attachment-observability`
**Relates to:** #76 (this gap is why #76 read as success on the sender), #115 (the
two-machine real-Tor test where the silence cost hours), #118 (CLI save verb).

**No ADR, no wire-format change.** No new `Frame` type, no new `Event` variant, no
migration. Part A is tracing only; Part B reuses the existing
`Event::AttachmentProgress` and changes which UI branch renders it.

---

## 1. Problem

Two distinct defects, both surfaced by the #115 two-machine real-Tor test.

### A. The chunk path is silent on success — at every level

`crates/core/src/delivery/{peer.rs,chunk_transfer.rs}` emit tracing **only on
failure**: `peer.rs` has `warn!` at the dial/send/unexpected-frame sites plus one
`debug!` (the not-outbound nack at `peer.rs:817`), and `chunk_transfer.rs` has a
single `warn!` (`serve_chunk_request` store-read failure, `chunk_transfer.rs:198`).
The success path — request served, chunk received, transfer finalized, completion
acked — has **zero** tracing calls.

Consequence, observed: the Windows peer restarted its daemon with
`RUST_LOG=info,skattr_core=debug,skattr_core::delivery=trace` and the attachment
path still emitted **zero** lines. `AttachmentComplete` is unloggable at *any*
level, because no call site exists. Confirming a successful transfer required
ChunkStore-prune elimination and disk forensics across two machines.

### B. The sender bubble reports the wrong thing

`FileAttachmentBubble.svelte` renders, for outgoing bubbles:

```svelte
{#if isOutgoing && deliveryStatus}
  <DeliveryIcon status={deliveryStatus} />
```

where `deliveryStatus` derives from the **text-message delivery store**
(`$delivery.get(record.message_id)`) — i.e. the MLS Ack of the *manifest message*.
The receiver acks that manifest the moment it ingests the MLS message, **before a
single chunk moves**. So the sender shows "Delivered ✓" with zero chunks
transferred.

Meanwhile core *does* emit a terminal sender signal — the `AttachmentComplete`
arm (`peer.rs:954`) calls `attachment_progress(attachment_id, t, t)` at
`peer.rs:968` — but every
transfer-state derivation in the bubble is gated on `!isOutgoing`, so the sender
discards it.

Net effect: a stalled transfer is invisible on the sending side in **both** the UI
and the log. That is the trust defect in #114's title.

---

## 2. Goals / non-goals

**Goals**
1. A stalled or completed transfer is diagnosable from either side's default log,
   without reproducing under a special `RUST_LOG`.
2. The sender UI distinguishes a completed attachment delivery from an
   in-progress/stalled one.
3. Existing text delivery-status behavior unchanged.

**Non-goals (explicit)**
- No stall detection timer (see §6).
- No durable sender-side progress across daemon restart (see §6).
- No new `Event` variant, `Frame`, IPC command, or migration.
- No rename of the shared `AttachmentStatus` strings (see §5.3).

---

## 3. Part A — tracing the silent path

### 3.1 Level policy

- **`info!`** — per-transfer milestones. ~4 lines per attachment across both
  sides. Visible in the default log and the UI log view, which is the point: a
  stalled transfer must be diagnosable with no env-var change.
- **`debug!`** — per-chunk events. A 10 MiB file is ~213 chunks at the 48 KiB
  `CHUNK_SIZE`; that volume does not belong at `info`.
- **`warn!`** — failures. Unchanged; all existing `warn!` sites stay as-is.

### 3.2 Field policy (redaction)

Permitted fields: `attachment_id` (hex), `index`, `received`, `total`, `bytes`.

Forbidden: **filename** (user content) and **peer pubkey / onion** (identity
material). This satisfies the standing rule "never log pubkeys, onions, or message
contents at `info` or higher".

`attachment_id` is deliberately included and deliberately *not* considered
sensitive: it is a random per-attachment 16-byte id, carries no identity, and is
the only value that correlates the two sides of a transfer — the entire diagnostic
value of these lines. Note it is 32 hex chars, so the ring-buffer `Redactor`
(`daemon/logs.rs:211`, which strips 64-hex pubkeys and `.onion`) does **not** strip
it. That is intended, not an oversight.

Render as hex, e.g. `aid = %hex::encode(attachment_id)` (or an equivalent
already-used formatting helper); do not log the raw byte array.

### 3.3 Sites

| # | Site | Level | Fields | Message |
|---|---|---|---|---|
| A1 | `chunk_transfer::serve_chunk_request`, success arm (`Ok(ct)`) | `debug!` | `aid, index, bytes = ct.len()` | `attachment: served chunk` |
| A2 | `peer.rs` `Frame::ChunkRequest` arm, on the **first** request seen for an attachment in this actor | `info!` | `aid, total` | `attachment: serving to peer` |
| A3 | `peer.rs` `Frame::Chunk` arm, after `rx.verify` succeeds and `on_received` runs | `debug!` | `aid, index, received, total` | `attachment: chunk received` |
| A4 | `peer.rs` `maybe_start_next_rx`, after a `ChunkRx` is installed and the first window is sent | `info!` | `aid, total` | `attachment: fetching from peer` |
| A5 | `peer.rs` `finalize_rx`, both CAS outcomes | `info!` | `aid, won` | `attachment: transfer complete` |
| A6 | `peer.rs` `Frame::AttachmentComplete` arm (sender side) | `info!` | `aid` | `attachment: delivery acked by peer` |

A2's "first request" condition reuses the served-index set introduced in §4.1 —
the line fires when the set for that attachment is created, so a 213-chunk
transfer logs one `info` line, not 213.

A5 logs both outcomes (`won = true` — this lane finalized; `won = false` — the
other lane already did) because "which lane won" was itself a question during the
#76 diagnosis. It does **not** log on the `Err` path; that already `warn!`s.

### 3.4 Non-change

`serve_chunk_request` keeps its current signature. Its `debug!` uses only values
already in scope. No function signature in either file changes for Part A.

---

## 4. Part B — honest sender status

### 4.1 Core: emit sender-side progress as chunks are served

In the `Frame::ChunkRequest` arm of the peer actor, after a `Frame::Chunk` reply is
successfully sent, record the index as served and emit progress:

```rust
d.attachment_progress(attachment_id, served_count, total);
```

**Served-count state.** A per-actor `HashMap<[u8; 16], HashSet<u32>>` of served
indices, owned by the peer actor loop alongside the existing `active_rx` state.
`served_count = set.len() as u32`.

Rationale for a set rather than a counter or `max(index)+1`: chunk requests arrive
**windowed (N=8) and out of order**, and may be **retried**, so a counter would
over-count on retry and `max(index)+1` would over-report on out-of-order arrival.
Both would show a progress figure that lies — the exact class of defect this issue
exists to remove.

**Lifecycle.** The entry is removed when the sender's `AttachmentComplete` arm runs
for that attachment (alongside the existing `store.remove` / deposit-prune). The
map is per-actor in-memory state: it persists across reconnects within the
actor's lifetime (a redial does not clear it) and is dropped only on actor
teardown or that `forget` at completion; see §6.

**Emit only on a successfully sent chunk** — not on a nack, not when the send
errored (that path already drops the connection).

### 4.2 UI: transfer state supersedes the manifest ack

In `FileAttachmentBubble.svelte`, add sender-side derivations mirroring the
existing receiver ones (`receiving` / `complete`, which are gated on
`!isOutgoing`), and let them take precedence over `DeliveryIcon`. Rendering for an
outgoing file bubble:

| Condition | Shows |
|---|---|
| `xferState` present and not complete | `Sending N/M` (progress row, same component as receive) |
| `xferState.status === "complete"` | `Delivered` |
| no `xferState` (pre-first-chunk, or post-restart) | `DeliveryIcon` fallback, capped: a `delivered` wire status renders as `sent`, since the manifest ack is not proof the file transferred |

The `DeliveryIcon` is *not* removed: it remains the fallback when no transfer state
exists. It is simply no longer allowed to claim "Delivered" while a transfer is in
flight. This keeps the change additive and preserves the pre-transfer display.

### 4.3 What is NOT changed

`applyProgress` in `stores/attachments.ts` sets `status: "receiving"`. On a sender
that label reads oddly, but it is an internal status string that the receiver path
also depends on; renaming it would touch the receiver derivations and the store
tests for a cosmetic gain. The sender-side derivations key off `status !==
"complete"` and the numeric `received`/`total`, so the label is never surfaced to
the user. Deliberately left alone.

---

## 5. Data flow (after this change)

```
SENDER                                        RECEIVER
  SendFile → stage chunks → announce manifest (Kind::File, MLS)
                                              manifest ingested → MLS Ack
  [DeliveryIcon: manifest acked]              maybe_start_next_rx  ── info A4
                                              send ChunkRequest window (N=8)
  ChunkRequest arm      ── info A2 (first)
  serve_chunk_request   ── debug A1
  attachment_progress(served, total)          Frame::Chunk → verify
  [bubble: "Sending 3/12"]                    ── debug A3
                                              ... all chunks received ...
                                              finalize_rx ── info A5
                                              AttachmentReceived emitted
                                              → Frame::AttachmentComplete
  AttachmentComplete arm ── info A6
  attachment_progress(t, t) [existing]
  [bubble: "Delivered"]
```

---

## 6. Deliberate exclusions (YAGNI)

**No stall timer.** A bubble stuck at "Sending 3/12" plus a log that stops after
`chunk 3` is already an unambiguous stall signal. A timer adds tunable state and a
new failure mode (false "stalled" on a slow Tor circuit) with no evidence yet that
the simpler signal is insufficient. Revisit if real stall reports say otherwise.

**No durable sender progress.** The served-index map and the UI store are both
session-scoped. After a daemon or UI restart an in-flight outgoing bubble falls
back to `DeliveryIcon` (§4.2 row 3). This matches the already-documented Phase 3.D
limitation (post-restart received-attachment state is session-scoped); it is a
known boundary, not new debt. Making it durable would need sender-side receipt
persistence — out of scope.

**No new Event variant.** `AttachmentProgress` already carries
`(attachment_id, received, total)` and is already wired end-to-end. A distinct
`AttachmentDelivered` would duplicate `received == total`.

---

## 7. Testing

**Part A — no new tests.** Tracing has no behavior; asserting on log output would
test the subscriber, not the code. Requirement: existing suites stay green
(`cargo test` workspace, incl. `attachment_roundtrip_multichunk_over_loopback`).

**Part B core — the served-set is the real logic, so it gets real tests:**
1. Serving chunks `0,1,2` emits progress `1/N, 2/N, 3/N`.
2. **Out-of-order** requests (`5` then `0`) report `1/N` then `2/N` — never `6/N`.
3. **Duplicate/retried** request for an already-served index does not advance the
   count.
4. A nack (unknown/not-outbound attachment) emits no progress.

**Part B UI — vitest on `FileAttachmentBubble`:**
5. Outgoing + progress `3/12` → renders `3/12`, does **not** render Delivered.
6. Outgoing + `status: "complete"` → renders Delivered.
7. Outgoing + manifest ack only (delivery store says Delivered, no `xferState`) →
   falls back to `DeliveryIcon`, does **not** claim the transfer finished. This is
   the regression guard for the actual #114 defect.

**Gate (local-first, per repo cadence):** `cargo fmt --all -- --check`,
`cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness
-- -D warnings`, `cargo test`, `cargo clippy -p skattr-ui --all-targets -- -D
warnings`, `pnpm check`, `pnpm exec vitest run`.

---

## 8. Acceptance criteria (from #114)

- [x] The sender UI distinguishes a completed attachment delivery from an
      in-progress/stalled one — §4.2, tests 5-7.
- [x] The sender log records chunk-serve activity (not only failures), so a
      stalled transfer is diagnosable from the sending side alone — §3.3 A1/A2/A6.
- [x] Existing text delivery-status behavior unchanged — §4.2 keeps `DeliveryIcon`
      as fallback; no change to `delivery.ts` or the text path.
- [x] Additionally (scope note in #114's comment): the **receiver** side is no
      longer silent either — §3.3 A3/A4/A5.
