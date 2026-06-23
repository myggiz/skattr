# Phase 3.C — Offline attachment transfer (design)

**Date:** 2026-06-22
**Status:** Approved (brainstorm) — ready for implementation plan
**Depends on:** Phase 3.A (attachment core) and 3.B (direct attachment
transfer) — both complete and merged. Phase 2.C (offline message delivery) and
2.D (mailbox caps) provide the mailbox machinery this reuses.
**No ADR:** 3.C **reuses the frozen mailbox protocol (ADR 0006) unchanged** —
chunks ride the existing `Deposit` frame as opaque blobs. No protocol change, so
no new ADR.
**Scope boundary:** offline / not-both-online attachment delivery via the
mailbox, plus cross-session resume. Out of scope: 3.D (Tauri UI), concurrent
attachments per peer, real onion-key rotation, multi-member groups.

---

## 1. Goal

Deliver a file attachment to a peer who is **not online** at send time (or who
drops mid-transfer), via the semi-trusted mailbox servers, and resume an
in-progress transfer across a daemon restart. This closes 3.B's deferred
"offline-manifest / online-chunks" gap (a manifest delivered via mailbox while
the sender is offline left the receiver unable to pull chunks).

Proven through the real `run_with_transport` assembly + a real mailbox server
over loopback: an offline peer receives a multi-chunk file end-to-end via the
mailbox, byte-identical, metadata stripped.

## 2. Locked decisions (from the brainstorm)

1. **Deposit-reuse + hash-match identification.** Each chunk is deposited as an
   opaque `Deposit` blob (the raw 3.A AEAD chunk ciphertext) addressed to the
   recipient's `recipient_hash`. The receiver identifies a fetched deposit as
   "chunk *i* of attachment X" by matching `sha256(deposit)` against the chunk
   hashes in its pending manifests. **No ADR 0006 change; no new plaintext
   routing metadata on the wire** (nothing the message path doesn't already
   expose). The hash match *is* the integrity check (it is the manifest's
   `ChunkRef.ciphertext_hash`).
2. **Dedicated `attachment_deposits` table (migration 0016)** for sender durable
   state — small rows keyed by `(attachment_id, chunk_index)`, **no payload**
   (pulled from `ChunkStore` at deposit time). NOT the message outbox (which
   would duplicate ~MiB of chunk payload already staged in `ChunkStore`).
3. **Deposit-all + receiver dedups.** The sender enqueues every chunk; the
   receiver's existing `received_indices` dedups a chunk it already has. No
   per-chunk sender-feedback channel is invented (3.B pull has none).
4. **Offline size cap `MAX_OFFLINE_ATTACHMENT_BYTES = 10 MiB`** (a v1.0
   constant). Files at/under the cap may use the offline lane; larger files are
   direct-only and, if the peer is offline, wait for both peers online. This
   bounds the deposit-rate cost (`per_conn_deposits_per_min = 30`, chunks fixed
   at 48 KiB → ~1.4 MiB/min ≈ ≤~7 min for a 10 MiB file).
5. **A stalled inbound transfer stays `pending` indefinitely** — no auto-fail,
   no janitor (consistent with 3.B's already-deferred partial-GC). Recovery is a
   user-driven resend. Auto-fail + partial-GC are a single later janitor
   (v1.1 / when 3.D surfaces transfer state).

## 3. Architecture & reuse map

An inbound attachment has **one manifest** (delivered over the message path) and
**two interchangeable chunk-receive paths** — 3.B direct pull and 3.C mailbox
fetch — that both write the **same** `ChunkStore` + `attachment_chunks` and run
the **same** reassembly. They compose without conflict: `received_indices` makes
double-delivery idempotent, and whichever lane delivers the last chunk fires the
single completion (the other lane then no-ops).

- **Frozen, untouched:** `mailbox/{protocol,codec,client,auth}.rs` (ADR 0006),
  `mailbox` server `policy.rs`.
- **New:** migration `0016` (`attachment_deposits` + `AttachmentDepositRepo`),
  `delivery::chunk_sweep`, an `InboundDispatch::dispatch_attachment_chunk` step
  in the poll handler, and the sender's offline-fallback trigger in `SendFile`.
- **Extended:** `daemon/inbound.rs` (chunk-match dispatch), `daemon/dispatch.rs`
  (`SendFile` offline fallback), `mailbox/poll.rs` (`poll_dispatch_once` tries
  chunk-match before MLS), `daemon/state.rs` (spawn `chunk_sweep`),
  `storage/migrations.rs` (register 0016).

## 4. Wire format & chunk identification

**No new wire format.** A chunk travels as a standard
`Deposit { recipient_hash, ciphertext, ttl_request }`:
- `recipient_hash = sha256(recipient_pubkey)` (same addressing as message
  deposits; mailbox list resolved by reusing 2.C's `run_mailbox_fallback`
  resolution).
- `ciphertext` = the raw 3.A per-chunk AEAD blob verbatim (≤48 KiB, far under
  the 1 MiB `max_deposit_size`).
- `ttl_request = 0` → operator default.

To the server a chunk deposit is indistinguishable from a message deposit.

**Receiver identification (local, per-poll):**
1. Build `HashMap<[u8;32] ciphertext_hash → (attachment_id, index)>` from all
   `direction='in'`, `status='pending'` attachment manifests.
2. For each fetched `PendingDeposit`: `sha256(ciphertext)` lookup. A hit is both
   identity and integrity verification.
3. Hit → `ChunkStore::put` + `mark_received` + add `deposit_id` to delete batch.
   Miss → hand to MLS `dispatch_mailbox`; if that also misses, **leave on
   server** (undispatched, retried next poll).

**Two self-healing ordering edges** (both rely on "leave unmatched deposits on
the server", which the existing poll already does — it deletes only dispatched
deposits):
- **Chunks before manifest:** no known hashes yet → no match → left on server →
  matched after the manifest lands on a later poll.
- **Duplicate chunk (dedup):** still a hash match, but `received_indices` shows
  it's stored → skip the write, **still delete from server** so the
  over-deposited duplicate is cleaned up rather than re-fetched forever.

The hash map is rebuilt per poll from currently-pending manifests, so
completed/failed attachments stop matching (stray late chunks fall through and
age out at TTL).

## 5. Sender path

**`SendFile` (extended):** stage chunks + persist the `attachments` `out` row +
announce the `Kind::File` manifest over the existing message path (which already
has 2.C direct→mailbox fallback, so offline *manifest* delivery is free).
Capture whether the manifest went **direct** (Delivered) or **via mailbox**
(Deposited).

**Offline-fallback trigger** (closes 3.B's gap), gated on
`total_size ≤ MAX_OFFLINE_ATTACHMENT_BYTES`:
- **Manifest deposited via mailbox** (peer offline) → enqueue all chunks into
  `attachment_deposits` immediately; do not wait on direct pull.
- **Manifest delivered direct** (peer online) → 3.B pull proceeds; arm a stall
  timer (reuse the `direct_timeout` notion). If no `AttachmentComplete` before
  it fires → enqueue all chunks into `attachment_deposits`. 3.B in-session resume
  still runs; the mailbox lane is the backstop.
- **Over the cap:** offline lane never engages; direct-only (logged; documented
  limitation — large files need both peers online).

**`attachment_deposits` (migration 0016)** — small rows, no payload:
```sql
CREATE TABLE IF NOT EXISTS attachment_deposits (
    attachment_id BLOB NOT NULL,
    chunk_index   INTEGER NOT NULL,
    mailbox_id    INTEGER NOT NULL,   -- 'theirs' mailbox targeted; failover updates it
    attempts      INTEGER NOT NULL DEFAULT 0,
    next_retry_at INTEGER NOT NULL,
    status        TEXT NOT NULL CHECK (status IN ('pending','deposited')) DEFAULT 'pending',
    PRIMARY KEY (attachment_id, chunk_index)
);
```
`AttachmentDepositRepo`: `enqueue_all(attachment_id, total_chunks, mailbox_id,
now)`, `due(now, limit) -> Vec<DepositRow>`, `mark_deposited(attachment_id,
index)`, `retarget(attachment_id, index, mailbox_id)` (failover),
`reschedule(attachment_id, index, attempts, next_retry_at)`,
`all_deposited(attachment_id) -> bool`, `delete_for_attachment(attachment_id)`.
One row per chunk; payload fetched from `ChunkStore` at deposit time.

**`delivery::chunk_sweep`** (sibling to `mailbox_sweeper`, same cadence): read
`due` rows, resolve the recipient's mailboxes (reuse `run_mailbox_fallback`
resolution + per-mailbox failover + backoff),
`MailboxClient::deposit(recipient_hash, ChunkStore::get_chunk(...), 0)`. On
`DepositOk` → `mark_deposited`. On `RecipientFull`/`ServerFull`/unreachable →
failover to the next mailbox / reschedule with backoff (bump `attempts`). When
`all_deposited(attachment_id)` → the sender's job is done: prune the deposit rows
and the staged `ChunkStore` blobs + `out` row (on whichever terminal —
`AttachmentComplete` or all-deposited — fires first).

## 6. Receiver path

Plugs into the existing `poll_dispatch_once` as a **chunk-match step before MLS
dispatch**, via a new `InboundDispatch` method (keeps logic in `DaemonInbound`
with pool/events/ChunkStore, not the poll loop):
```
for each deposit:
    if inbound.dispatch_attachment_chunk(&ciphertext) is Some -> delete
    else if inbound.dispatch_mailbox(&ciphertext) is Some     -> delete
    else                                                       -> leave on server
```

**`dispatch_attachment_chunk(&ciphertext) -> Option<()>`** (new on
`InboundDispatch`, default `None`; implemented by `DaemonInbound`):
1. Build the per-poll `sha256 → (attachment_id, index)` map from pending
   `direction='in'` manifests. Miss → `None`.
2. Hit + already in `received_indices` → return `Some` (so the duplicate is
   server-deleted) but skip the write.
3. Hit + new → `ChunkStore::put` + `mark_received`. If now complete →
   `reassemble` to `download_dir` (3.B path) → `set_status('complete')` → emit
   `Event::AttachmentReceived`; throttled `AttachmentProgress`. Return `Some`.

**Unified with 3.B:** both lanes write the same `ChunkStore` + `attachment_chunks`
and run the same completion; idempotent via `received_indices`. A peer that drops
mid-direct and later sends the rest via mailbox continues the same bitmap.

**Manifest ingest needs no special-casing:** a `Kind::File` manifest arriving via
mailbox already flows `dispatch_mailbox → dispatch_for_group`, which (3.B)
persists it as a `direction='in'` `pending` attachment — exactly what the
per-poll hash map reads. (3.B's `take_begin_attachment` peer-actor queue is the
*pull* trigger; the offline lane ignores it.)

## 7. Cross-session resume

No in-memory state required; all progress is durable:
- **Sender:** `attachment_deposits` rows + `ChunkStore` blobs survive restart.
  `chunk_sweep` re-queries `due()` and continues the `pending` rows; `deposited`
  rows skip. Crash after 40/200 deposited → resume at 41.
- **Receiver:** `attachment_chunks` (bitmap) + persisted manifest survive. Next
  poll rebuilds the hash map and resumes matching; `received_indices` dedups. A
  chunk in flight during a crash is **still on the server** (deleted only after
  successful store+mark) → re-fetched next poll, no loss.
- **Boot reconciliation (concrete default):** the only non-durable bit is the
  sender's in-memory **stall timer**. On boot, `chunk_sweep` simply resumes any
  attachment that already has `attachment_deposits` rows — that fully covers a
  restart of a transfer that had engaged the offline lane. An incomplete `out`
  attachment with **no** deposit rows (it was direct-only when interrupted) is
  **not** auto-enqueued on boot — that would require durable peer-online state
  3.C does not add; it is left to 3.B's reconnect-driven direct resume (and, if
  the peer is offline and it's within the cap, a subsequent send-side stall will
  engage the offline lane). The `attachment_deposits` PK + `deposited` status
  make re-enqueue idempotent, so no path can double-deposit.

## 8. Caps, failures, cleanup

- **Mailbox limits (reuse 2.D, no server change):** a chunk deposit counts
  against `recipient_cap_bytes` (256 MiB) and `global_storage_cap_bytes` (4 GiB)
  like any message; a ≤10 MiB offline file fits easily.
  `per_conn_deposits_per_min = 30` throttles the sweep naturally — backoff
  handles rate-limit rejections, no special logic.
- **Deposit failures (sender):** `RecipientFull`/`ServerFull`/unreachable →
  failover to the recipient's next advertised mailbox, else reschedule with
  backoff. The chunk stays `pending` and retries indefinitely (best-effort,
  matching the message mailbox lane). No `AttachmentFailed` for deposit trouble.
- **Cleanup:** sender prunes `attachment_deposits` + staged blobs + `out` row on
  the first terminal (`AttachmentComplete` or all-deposited). Receiver removes a
  completed attachment's `ChunkStore` staging (3.B). An incomplete inbound
  attachment stays `pending` indefinitely (Q5); its deposits age out at TTL.
  Partial-`ChunkStore` GC for never-completed inbound attachments is the
  **deferred janitor** shared with 3.B — out of 3.C.
- **TTL limitation (documented):** if the recipient never polls within the
  mailbox TTL (~7 days), chunk deposits expire server-side; the sender has marked
  them `deposited` and won't re-deposit (no fetch feedback), so the transfer
  stays `pending`. Accepted best-effort bound for v1.0, disclosed in the
  limitations alongside "large files need both peers online".

## 9. Testing & guardrails

Reuses 2.C's `offline_peer_receives_via_mailbox_fallback` harness shape (a real
`MailboxServer` on the loopback net + daemons' `MailboxConnectFactory` wired to
it), through the real `run_with_transport` — no `test_exports` shortcut.

- **Guardrail 1 — `offline_attachment_via_mailbox`:** Bob is not directly
  dialable (forces the mailbox lane, the 2.C trick). Alice `SendFile`s a
  multi-chunk file with EXIF; the manifest deposits to Bob's mailbox; `chunk_sweep`
  deposits all chunks; Bob polls, hash-matches, reassembles. Assert
  `Event::AttachmentReceived`, byte-identical-to-stripped file at Bob's
  `download_dir`, EXIF absent.
- **Guardrail 2 — `offline_attachment_cross_session_resume`:** restart the
  receiver daemon mid-transfer (some chunks fetched+persisted), assert it
  resumes from `attachment_chunks` on the next poll and completes byte-identical.
  Sender-restart (resume from `attachment_deposits`) as a second case if the
  harness supports clean restart cheaply.
- **Unit coverage:** `AttachmentDepositRepo` (enqueue-all, `due`/backoff,
  `mark_deposited`, retarget-failover, all-deposited prune);
  `dispatch_attachment_chunk` (hit→store+mark+delete; already-received→delete
  without write; miss→None; manifest-absent→None); `chunk_sweep` (due→deposit via
  stub factory→deposited; `RecipientFull`→failover/backoff); migration `0016`
  applies on fresh DB and over `0015`.

Success = both guardrails green through the real assembly + the full workspace
green under the real CI gate (`fmt` / `clippy --all-features -D warnings` /
`test --features test-harness`).

## 10. Deferred / explicitly-not-3.C

- Auto-fail of stalled inbound transfers + partial-`ChunkStore` GC janitor
  (shared deferral with 3.B) — v1.1.
- Chunk **batching** per ≤1 MiB deposit (throughput optimization to beat the
  deposit-rate limit for large files) — v1.1; 3.C keeps the 10 MiB offline cap.
- Per-chunk sender-feedback / precise deposit-only-missing — not built (3.B pull
  has no per-chunk receipt; deposit-all + receiver dedup is the model).
- Re-deposit on TTL expiry (no fetch feedback exists) — out of scope.
- 3.D (UI), concurrent attachments per peer, onion-key rotation, multi-member
  groups.
