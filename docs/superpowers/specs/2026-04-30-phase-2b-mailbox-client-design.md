# Phase 2.B — Mailbox client + ContactCard rotation design

**Status:** approved (brainstorm 2026-04-30).
**Date:** 2026-04-30.
**Predecessor:** Phase 2.A merged (mailbox server + ADR 0006 freeze).
**Umbrella:** `docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md`.
**Frozen wire surface:** `docs/adr/0006-mailbox-protocol-v1.md`.

## Scope

2.B implements the client half of the mailbox protocol: a long-lived
`MailboxClient` for our own mailboxes (poll/fetch/delete), an
on-demand client for depositing into recipient mailboxes, an adaptive
`PollScheduler`, the `DeliveryHub` direct→mailbox fallback, and the
`RotateOnion` / `AddMailbox` / `RemoveMailbox` daemon commands plus
their event surface. ContactCards are populated with the user's
mailbox onions and republished on every rotation event. The frozen
v1 wire surface is binding — no protocol changes; this is a pure
client-side build.

**In scope:** `core::mailbox::{client, codec, poll}`, extensions to
`core::delivery::{hub, outbox}`, new `core::storage::mailboxes`
repo, migration 0008, three new `Command` variants and three new
`Event` variants on the IPC surface, full integration coverage
through `crates/tests/`.

**Out of scope** (deferred to later phases): UI surfaces for mailbox
CRUD (2.F renders settings against the wire surface 2.B ships); a
public "use this mailbox" directory (Phase 5+); cover-traffic
polling (Phase 4); multi-member groups (Phase 3); federated
mailboxes (off the table per ADR 0006); any wire-protocol change
(`MAILBOX_PROTOCOL_V2` and a separate spec).

## Architectural decisions (locked in brainstorm)

These are the decisions reached during the 2026-04-30 brainstorm.
They are binding for the implementation plan.

1. **Two connection lifecycles.** Long-lived per-mailbox
   `Framed<DataStream, MailboxFrameCodec>` for `'mine'` mailboxes
   (polling stays warm, no per-op circuit cost); open-on-demand for
   recipient mailboxes (deposits are rare per recipient, no idle
   state). No daemon-wide connection pool; mailbox-onion concentration
   across our contacts is a Phase-5 concern if it surfaces.
2. **Per-mailbox polling actors with daemon-wide activity bumps.**
   One actor per `'mine'` mailbox row; failure isolation matters and
   the constant factor is small (1–3 actors). The Idle↔Active state
   machine flips to Active on local-send / local-receive / fetch
   returning ≥1 deposit (all-of-the-above), with a 5-min Active hold
   timer. Send/receive triggers are daemon-wide; fetch-with-deposits
   trigger is per-mailbox.
3. **Schedule formula.** `next_interval(active, rng) → Duration`.
   Active base = 15 s; Idle base = 60 s; both with ±25% jitter; idle
   ceiling = 5 min for cycles where the actor is *Unreachable*.
   Pure function, unit-testable without clocks.
4. **Mailbox table extension — minimal.** Migration 0008 adds four
   nullable columns (`status`, `last_poll_at`, `last_error_at`,
   `last_error_kind`); the existing `(id, onion, registered_at, role)`
   shape is unchanged. `consecutive_failures` and `last_success_at`
   stay in the actor's in-memory state — adding columns is cheap if
   we need them in 0009+.
5. **Outbox extension — explicit columns + composite unique index.**
   Migration 0008 adds `target_kind TEXT NOT NULL DEFAULT 'direct'
   CHECK ('direct','mailbox')` and `mailbox_id INTEGER NOT NULL
   DEFAULT 0` (FK-by-convention to `mailboxes.id`; `0` means "no
   mailbox" for direct rows). The unique index becomes
   `(target, message_id, target_kind, mailbox_id)`. Existing rows
   backfill `target_kind='direct'`, `mailbox_id=0`.
6. **Direct→mailbox fallback policy.**
   - **Time-based**: try direct for `direct_timeout_secs` (default
     30 s, configurable in 2.F via `[delivery] direct_timeout_secs`).
     On timeout or hard connect-error, kick off mailbox fallback.
   - **Pick-one-then-retry across recipient mailboxes.** Deterministic
     hash: `mailboxes[blake2s(message_id) % len]`. On failure,
     sequential failover through the remaining mailboxes.
   - **Zero recipient mailboxes**: outbox holds the message
     indefinitely; UI surfaces "queued — no offline delivery for this
     contact." Cascade-give-up after 7-day TTL flips the row to
     `Failed`.
   - **All recipient mailboxes reject**: cascade-give-up after one
     full pass with bounded backoff (1m → 5m → 25m → cap 1h);
     `Failed` event after the message TTL elapses.
7. **ContactCard shape unchanged.** `ContactCardBody.mailboxes` stays
   `Vec<String>` (onion strings). 1.D-era empty cards remain valid;
   2.B daemons start populating the field. No CBOR-shape change. No
   `MailboxRef` struct — there is no operator-attested key separate
   from the Tor HS key, so a per-mailbox fingerprint would be a
   placeholder for a Phase-5 idea we don't have a design for. The
   `version: u64` field is monotonic — every rotation event
   (onion change, mailbox add, mailbox remove) bumps it by 1.
8. **RotateOnion flow.**
   - Generate fresh HS key via `tor_hsservice` rotation; start a new
     `OnionService` task.
   - Sign a new `ContactCard` with `version + 1`, the new onion, the
     current mailbox list.
   - Publish the card to every contact via the same direct→mailbox
     fallback as ordinary messages (see decision 9 for the carriage
     mechanism).
   - Old `OnionService` task stays listening for a 24 h grace period
     (configurable via `[delivery] rotate_grace_secs`).
   - Contacts with zero listed mailboxes who are also offline: surface
     the list to the user, rotate anyway (warn-but-proceed). Those
     contacts pick up the card on next direct send.
9. **Card publication rides MLS app-message channel.** A new
   `Envelope` kind `ContactCardUpdate(ContactCard)` is encrypted into
   each pairwise 2-member group via `Group::encrypt`. The resulting
   ciphertext flows through the same direct→mailbox fallback as
   ordinary text messages — one delivery path, no special-case code.
   Receivers' inbound dispatcher branches on envelope kind, calls
   `ContactRepo::put_card`, and emits `Event::ContactCardReceived`.
   The mailbox operator sees ciphertext indistinguishable from any
   other message.
10. **AddMailbox = validate-then-publish.** `Command::AddMailbox` opens
    a connection to the onion, runs a single `Challenge` round-trip
    against our identity hash (proves reachability + protocol-version
    + identity-acceptance). On success: insert a row with
    `status='reachable'`, bump ContactCard version, fan out the new
    card. On failure: `DaemonErrorKind::InvalidArgument(reason)` —
    no row inserted, no card change.
11. **RemoveMailbox = drain-then-drop.** Mark the row
    `status='pending_removal'`; run one final Challenge→Fetch→Delete
    cycle to drain anything queued for us. After the drain (or hard
    error giving up), flip the status to `removed`, bump ContactCard
    version, fan out the new card. The row stays in the table for
    audit / re-add idempotency.
12. **Mailbox-deposit ACK semantics — three-state delivery.**
    `DeliveryStatus = Queued | Sent | Deposited | Acked | Failed`.
    `DepositOk` flips a row to `Deposited`. The peer's
    `Frame::Ack(message_id)` over a re-established direct connection
    flips it to `Acked`. If the conversation pair never re-establishes
    a direct circuit, the row stays `Deposited` permanently — that's
    the truthful state. No mailbox-routed ACK frames (would double
    mailbox traffic and complicate the design).
13. **Event surface — additive, append-only.** Three new variants:
    `MailboxStatusChanged`, `ContactCardReceived`,
    `DeliveryStatusChanged`. Two new `EventFilter` members:
    `Mailboxes`, `Delivery`. No existing event variant is changed.
14. **Module visibility.** `core::mailbox::client`,
    `core::mailbox::poll`, `core::mailbox::codec`, and
    `core::storage::mailboxes` stay `pub(crate)`. The Daemon/IPC
    layer is the stable boundary, exactly like 1.E's delivery code.

## Module layout

```
crates/core/src/
  mailbox/
    protocol.rs           (frozen 2.A — unchanged)
    client.rs             NEW — MailboxClient
    codec.rs              NEW — MailboxFrameCodec (client mirror)
    poll.rs               NEW — PollScheduler + per-mailbox actor
    mod.rs                (re-exports unchanged; new modules pub(crate))
  contact/
    card.rs               (no schema change; 2.B starts populating mailboxes)
  delivery/
    hub.rs                (extended — fallback orchestrator)
    outbox.rs             (extended — target_kind dispatch)
    peer.rs               (unchanged on the wire side; signals fallback start)
  storage/
    mailboxes.rs          NEW — MailboxRepo
    outbox.rs             (extended)
    migrations/
      0008_mailbox_status_and_outbox_target_kind.sql   NEW
  daemon/
    dispatch.rs           (extended — RotateOnion / AddMailbox / RemoveMailbox)
    commands.rs           (extended — wire-format additions)
    events.rs             (extended — wire-format additions)
```

## Wire-format contract (additive only)

Existing types are unchanged. New variants:

```rust
// daemon::commands
Command::AddMailbox    { onion: String }
Command::RemoveMailbox { id: i64 }
// (Command::ListMailboxes was stubbed in 2.C; 2.B replaces the handler)
Command::RotateOnion

CommandResult::Ok                     // existing
CommandResult::Mailboxes(Vec<MailboxSummary>)   // existing (from 2.C)

// daemon::events
Event::MailboxStatusChanged { mailbox_id: i64, status: MailboxStatus }
Event::ContactCardReceived  { contact: PublicKey, version: u64 }
Event::DeliveryStatusChanged { message_id: MessageId, status: DeliveryStatus }

EventFilter::Mailboxes
EventFilter::Delivery

// New shared enums
pub enum MailboxStatus {
    Unknown, Reachable, Unreachable,
    RateLimited, PendingRemoval, Removed,
}
pub enum DeliveryStatus {
    Queued, Sent, Deposited, Acked, Failed,
}
```

`MailboxSummary` (locked in 2.C, reused unchanged):
`{ id: i64, onion: String, status: MailboxStatus, registered_at: u64 }`.

`DaemonErrorKind` gains no new variants; `InvalidArgument` already
covers AddMailbox-validation failures. The wire `reason` string for
those failures is one of: `"unreachable"`, `"unsupported_version"`,
`"rate_limited"`, `"malformed_response"`, `"other"`.

## Key types and methods

```rust
// core::mailbox::client (pub(crate))
pub(crate) struct MailboxClient {
    onion: String,
    framed: Framed<DataStream, MailboxFrameCodec>,
}

impl MailboxClient {
    pub async fn connect(onion: &str, arti: &TorClient<TR>) -> Result<Self>;
    pub async fn deposit(&mut self, recipient_hash: [u8; 32],
                         ct: Vec<u8>, ttl_request: u32) -> Result<DepositOk>;
    pub async fn fetch(&mut self, identity: &IdentityKey) -> Result<FetchResponse>;
    pub async fn delete(&mut self, identity: &IdentityKey,
                        ids: Vec<DepositId>) -> Result<DeleteOk>;
    /// AddMailbox liveness probe — single Challenge round-trip.
    pub async fn probe(&mut self, identity_hash: [u8; 32]) -> Result<()>;
}

// core::mailbox::poll (pub(crate))
pub(crate) struct PollScheduler {
    ctrl: mpsc::Sender<PollerCtrl>,
}

pub(crate) enum PollerCtrl {
    AddMailbox(i64),
    RemoveMailbox(i64),
    BumpActive,            // daemon-wide activity flip
    Shutdown,
}

#[must_use]
pub(crate) fn next_interval(active: bool, rng: &mut impl Rng) -> Duration;

// core::storage::mailboxes (pub(crate))
pub(crate) struct MailboxRepo<'p> { pool: &'p Pool }
impl<'p> MailboxRepo<'p> {
    pub fn add_mine(&self, onion: &str, now: i64) -> Result<i64>;
    pub fn list_mine(&self) -> Result<Vec<MailboxRow>>;
    pub fn list_for_contact(&self, identity: &PublicKey) -> Result<Vec<String>>;
    pub fn mark_status(&self, id: i64, status: MailboxStatus, now: i64) -> Result<()>;
    pub fn mark_pending_removal(&self, id: i64) -> Result<()>;
    pub fn finalize_removal(&self, id: i64) -> Result<()>;
    pub fn touch_poll(&self, id: i64, now: i64) -> Result<()>;
    pub fn record_error(&self, id: i64, kind: &str, now: i64) -> Result<()>;
}

// core::delivery::hub (extended)
impl<S> DeliveryHub<S> {
    /// Called by PeerConnection after `direct_timeout_secs` of failed
    /// direct delivery. Spawns a one-shot fallback task that runs the
    /// pick-one-then-retry deposit loop and updates outbox state in
    /// `pool.transaction`.
    pub async fn ensure_mailbox_fallback(
        &self,
        peer: PublicKey,
        msg_id: MessageId,
        ct: Vec<u8>,
    );
}
```

Errors (extends `core::error::CoreError` per 1.H structural-match
discipline):

```rust
pub enum MailboxClientErrorKind {
    Unreachable,
    UnsupportedVersion,
    RateLimited,
    RecipientFull,
    InvalidSignature,
    NonceExpired,
    Malformed,
    HashMismatch,
    Other(String),
}
```

`CoreError::kind()` extended to match over the new sub-enum; the
build-time guard test (no `str::contains` in `kind()`) is extended
to cover `MailboxClient`.

## Migration 0008

```sql
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz B.V.
--
-- Skattr storage schema, version 8.
-- Phase 2.B mailbox client: status tracking on `mailboxes`, target-kind
-- + mailbox FK on `outbox`.

INSERT OR IGNORE INTO schema_version (version) VALUES (8);

ALTER TABLE mailboxes ADD COLUMN status TEXT NOT NULL DEFAULT 'unknown'
    CHECK (status IN ('unknown','reachable','unreachable',
                      'rate_limited','pending_removal','removed'));
ALTER TABLE mailboxes ADD COLUMN last_poll_at INTEGER;
ALTER TABLE mailboxes ADD COLUMN last_error_at INTEGER;
ALTER TABLE mailboxes ADD COLUMN last_error_kind TEXT;

ALTER TABLE outbox ADD COLUMN target_kind TEXT NOT NULL DEFAULT 'direct'
    CHECK (target_kind IN ('direct','mailbox'));
ALTER TABLE outbox ADD COLUMN mailbox_id INTEGER NOT NULL DEFAULT 0;

DROP INDEX IF EXISTS idx_outbox_target_message_id;
CREATE UNIQUE INDEX IF NOT EXISTS idx_outbox_target_message_kind_mailbox
    ON outbox(target, message_id, target_kind, mailbox_id);
```

The `mailbox_id` column is FK-by-convention rather than a SQL FK
because `mailbox_id = 0` for direct rows; a real FK would force a
sentinel row in `mailboxes`. The `MailboxRepo` enforces existence in
Rust; cross-row consistency is checked by tests (orphaned mailbox_id
references are a programmer error, not a recoverable runtime case).

## Data flow

### Outbound send (offline peer path)

```
SendMessage(peer, body)
   ↓
Group::encrypt(body) + transactional persist
  (sender row in messages, outbox row target_kind='direct')
   ↓
DeliveryHub::send(peer, msg_id, ct)
   ↓
PeerConnection actor tries direct connection
   ↓ (direct_timeout_secs elapses, or hard connect-error)
DeliveryHub::ensure_mailbox_fallback(peer, msg_id, ct)
   ↓
   look up peer's ContactCard.mailboxes (latest_card)
   pick-one: mailbox_onion = mailboxes[blake2s(msg_id) % len]
   ↓
   transactional UPDATE outbox SET target_kind='mailbox', mailbox_id=N
   ↓
   open-on-demand MailboxClient::connect(mailbox_onion)
   client.deposit(sha256(peer_pubkey), ciphertext, ttl_request)
   ↓ DepositOk
   transactional DELETE outbox row + emit DeliveryStatusChanged{Deposited}
   ↓ (later) peer fetches, decrypts, re-establishes direct, sends Frame::Ack
   emit DeliveryStatusChanged{Acked}
```

On `RateLimited` / `RecipientFull` / network error from one mailbox:
sequential failover through the remaining mailboxes. After one full
pass with no acceptance: outbox row stays, exponential backoff
(1m → 5m → 25m → cap 1h), retry next pass. After 7-day TTL: row
flips to `Failed`, emits `DeliveryStatusChanged{Failed}`, stays in
outbox for audit.

### Inbound poll (mine)

```
PollScheduler tick (per mailbox)
   ↓
MailboxClient.challenge() → ChallengeNonce
sign(AUTH_DOMAIN || nonce || 0x86 || sha256(positional_tuple))
   ↓
client.fetch() → FetchResponse { deposits: [...] }
   ↓ for each deposit:
   Group::decrypt(deposit.ciphertext) → MessageRecord
     ↓ (idempotency: seen_messages.contains((sender, msg_id))? skip)
     transactional persist (messages row + read_state advance candidate)
     branch on envelope kind:
       MlsApp(text)            → emit MessageReceived{ contact, record }
       ContactCardUpdate(card) → ContactRepo::put_card(card)
                                  → emit ContactCardReceived{ contact, version }
   ↓ collect deposit_ids
client.delete(deposit_ids) → DeleteOk
   ↓
MailboxRepo::touch_poll(id, now) + mark_status(Reachable)
emit MailboxStatusChanged if status changed
```

### Onion rotation

```
Command::RotateOnion
   ↓
Generate new HS key (tor_hsservice rotate)
Start new OnionService task
Schedule old-onion shutdown 24h from now (rotate_grace_secs)
   ↓
ContactCard::sign(new_onion, my_mailboxes, version+1, ...)
ContactRepo::put_self_card (locally persist)
   ↓ for each contact:
   Envelope::ContactCardUpdate(card) → Group::encrypt
   → DeliveryHub::send (uses direct→mailbox fallback)
   ↓ (24h later) old OnionService task aborted
```

Contacts with zero listed mailboxes who are also offline: the
`SendMessage`-equivalent for the card update queues in their outbox
indefinitely; UI surfaces the list at command-completion time. They
pick up the card on next direct send.

## Error handling

| Error | Action |
|-------|--------|
| `Unreachable` (transient) | Mark mailbox `Unreachable`, schedule next poll at idle-ceiling, resume normal cadence after one successful poll |
| `Unreachable` (consecutive ≥ 5) | Stay `Unreachable`, surface `MailboxStatusChanged`. Counter lives in actor in-memory state, not in schema |
| `UnsupportedVersion` | Mark `Unreachable`, log warning, do not retry until daemon restart |
| `RateLimited` | Polling: extend tick to idle-ceiling for one cycle. Deposit: rotate to next mailbox in pick-one-then-retry sequence |
| `RecipientFull` (deposit) | Same as `RateLimited` — rotate to next mailbox |
| `InvalidSignature` (poll) | Single retry after fresh Challenge (clock-skew tolerance); on second failure mark `Unreachable` and log error |
| `NonceExpired` | Re-Challenge inside the same connection, retry the Fetch/Delete once. Persistent failure → `Unreachable` for this cycle |
| `Malformed` / `HashMismatch` | Log error, drop connection, schedule reconnect on next cycle |
| Connection drop mid-op | Reconnect on next tick. Outbox row intact (transactional). Idempotent re-deposit safe via composite unique index |

**Idempotency under daemon crash mid-deposit**: outbox row unchanged
→ next startup re-attempts. If the previous attempt actually
completed but the response was lost, the recipient receives the same
ciphertext under a new server-issued `deposit_id`; recipient-side
`seen_messages` (1.E) catches the duplicate on decrypt. Mailbox
protocol does not surface client-supplied request ids (frozen).

**Mailbox transactional discipline**: the "deposit succeeded" + "outbox
row deleted" pair is atomic from the daemon's perspective. If the
post-deposit transaction fails, the next retry re-deposits — duplicate
ciphertext for the recipient, harmless because `seen_messages`
discards.

**Logging redaction**: mailbox-client log lines never include onion
strings, recipient hashes, message IDs, or ciphertext at `info+`.
The 2.A logging-redaction unit test is extended to cover the new
client modules.

## Test plan

Mirrors 2.A's six-layer pyramid.

### 1. Unit tests

- `MailboxClient` against in-process `MailboxFrameCodec` over
  `tokio::io::duplex` — happy paths + every `MailboxClientErrorKind`
  variant.
- `PollScheduler::next_interval` — boundary conditions (Idle/Active,
  jitter range, minimum tick).
- `MailboxRepo` — CRUD + status transitions, FK cascades, error
  paths.
- `outbox` repo extension — target_kind dispatch, composite unique
  index conflict handling.
- `delivery::hub` fallback — direct timeout fires the orchestrator;
  pick-one-then-retry deterministic by `(msg_id, mailbox_set)`.
- ContactCard population — RotateOnion writes new card; AddMailbox
  / RemoveMailbox bump version.

### 2. Property tests

- `next_interval` always in `[10s, 5min]` and within ±25% of base
  for arbitrary `(active, rng-state)`.
- Pick-one-then-retry: for any `(msg_id, mailboxes)` permutation of
  order, the same mailbox is tried first.
- Outbox idempotency: enqueueing the same `(target, message_id,
  target_kind, mailbox_id)` twice produces one row.

### 3. Integration tests (`crates/tests/src/`)

- `mailbox_offline_delivery.rs` — daemon-pair + in-process
  `MailboxServer` over `tokio::io::duplex`. Alice sends to offline
  Bob; deposit lands; Bob comes online; fetches; Ack flows back to
  Alice; row → `Acked`. Asserts `DeliveryStatusChanged` event
  sequence.
- `mailbox_failover.rs` — two mailboxes registered for Bob; first
  returns `RateLimited`; second accepts. Asserts the event log
  shows the failover.
- `rotate_onion_during_offline.rs` — Alice rotates while Bob is
  offline. Bob's deposits queue (24 h grace) on Alice's old onion
  AND a card-update flows to Bob's mailbox. Bob comes online,
  fetches, picks up the new card via `ContactCardReceived`, future
  messages route to the new onion. Old onion shut down at grace
  expiry (advanceable via `tokio::time::pause`).
- `add_mailbox_validates.rs` — `Command::AddMailbox` against an
  unreachable onion returns `InvalidArgument("unreachable")`;
  against a reachable one succeeds and emits `ContactCardReceived`
  on the peer side after the implicit republish.
- `remove_mailbox_drains.rs` — pre-deposit a message for our
  mailbox; `RemoveMailbox` runs final drain; the in-flight message
  arrives before the row flips to `Removed`.

### 4. `#[ignore]`-gated real-Tor scenario

`crates/tests/src/mailbox_client_real_tor.rs`: spawns the
`skattr-mailbox` binary on a real Arti circuit, two daemons over
real onion services. Drives offline-delivery + rotation. Manual
run before merge — matches 1.E / 1.F / 2.A pattern.

### 5. Adversarial regression

- Malicious mailbox returns `Internal` on every Fetch — poller
  backs off, marks `Unreachable` after 5 consecutive failures,
  surfaces `MailboxStatusChanged`.
- Malicious mailbox replays an old `ChallengeNonce` after issuing
  a new one — second Fetch with the old nonce gets `NonceExpired`;
  client re-Challenges and retries once; succeeds.
- Malicious mailbox returns `FetchResponse` with arbitrary
  ciphertext (not for us) — `Group::decrypt` rejects, `seen_messages`
  unaffected, deposit_id is still deleted on next pass to avoid
  permanent garbage.
- Mailbox returns malformed CBOR — `MailboxClientErrorKind::Malformed`,
  connection dropped, next-cycle reconnect succeeds.
- Concurrent rotate + add_mailbox — version monotonicity invariant
  holds. (Sequential through daemon command queue, but the property
  is asserted.)

### 6. Logging-redaction

Extends 2.A's redaction test: at `info+`, no onions, no recipient
hashes, no message IDs, no ciphertext are emitted by mailbox-client
modules. Hooks into a `tracing` test subscriber.

### Exit-criterion mapping

| Sub-project exit criterion | Test |
|----------------------------|------|
| Offline peer receives queued messages on reconnect | `mailbox_offline_delivery.rs` |
| Rotate-onion doesn't break conversations | `rotate_onion_during_offline.rs` |
| Mailbox failover works | `mailbox_failover.rs` |

The `#[ignore]`-gated real-Tor scenario is the merge-PR validation
step.

## Open questions (deferred to writing-plans / executing-plans, not
blocking the design)

- **Card-update envelope kind tagging.** The exact wire shape of
  `Envelope::ContactCardUpdate(ContactCard)` (CBOR enum tag,
  inbound dispatcher branch placement) is a writing-plans-level
  detail — the design commits only that card publications ride MLS
  app messages, not the byte-level enum tag.
- **`TorClient` accessor on `MailboxClient::connect`.** Whether to
  pass the existing daemon-owned `TorClient` reference vs a new
  `Arc<TorClient>` clone is an executing-plans-level decision based
  on what 0.C exposes.
- **`PollScheduler` shutdown ordering vs DeliveryHub shutdown.** If
  the daemon stops while a fallback orchestrator is mid-deposit, the
  outbox row must remain intact. Implementation guarantees this via
  `pool.transaction` boundaries — the test plan covers it under
  "Connection drop mid-op."

## Risks and mitigations

| Risk | Mitigation |
|------|------------|
| Real-Tor circuits intermittently fail mid-poll, spamming `Unreachable` events | Consecutive-failures counter (in-memory) gates the event emission to ≥ 5 in a row; transient drops below that don't surface to the UI |
| Outbox unique-index migration fails on a non-empty table during 0008 backfill | Migration uses `DROP INDEX … IF EXISTS` then `CREATE UNIQUE INDEX … IF NOT EXISTS` — backfill assigns `target_kind='direct'`, `mailbox_id=0` to existing rows so the new composite index is satisfied without uniqueness violations (no two existing rows share `(target, message_id)` already, by 1.E's invariant) |
| RotateOnion fans out N card updates and saturates outbox / mailbox bandwidth on a daemon with many contacts | Card updates are MLS app messages — they share the existing per-peer rate limit and direct-timeout policy. No special path means no special amplification |
| Mailbox operator silently drops a deposit (operator misbehaviour) | Sender-side: row stays `Deposited` forever (truthful — we don't know). UI does not show `Acked` until a real peer ACK arrives. No redundant deposits to multiple mailboxes per message (pick-one), so the operator sees one chance. Phase-5 trust-on-failure heuristics are out of scope |
| `tor_hsservice` rotation API behaves differently than expected (graceful old-onion teardown) | Test the rotation path with two integration scenarios: graceful (24 h timer elapses cleanly) and abrupt (daemon shutdown mid-rotation — old onion shuts down, new onion is the only one referenced from the persisted card) |
| `MailboxStatusChanged` events flap on flaky Tor | Hysteresis: status flips to `Reachable` only after one successful Challenge round-trip; flips to `Unreachable` only after 5 consecutive failures. Both transitions emit one event each |

## Out of scope for 2.B

- UI surfaces for mailbox CRUD (2.F renders settings against this
  wire surface; 2.C ships the stub).
- Public "use this mailbox" directory (Phase 5+).
- Cover-traffic polling (Phase 4).
- Multi-member groups (Phase 3) — `mailbox_for_group` semantics need
  redefinition when groups are not 2-member.
- Federated mailboxes (off the design table per ADR 0006).
- Wire-protocol changes — anything that needs a new frame byte or a
  typed-field shape change is `MAILBOX_PROTOCOL_V2` and a separate
  spec.
- Mailbox-routed ACKs — direct-only `Frame::Ack` is the design.

## What this doc does NOT cover

- Implementation step-by-step task ordering — `superpowers:writing-plans`
  produces that as
  `docs/superpowers/plans/2026-04-30-phase-2b-mailbox-client.md`.
- Wire-protocol details — see ADR 0006 and
  `crates/core/src/mailbox/protocol.rs` (frozen).
- 2.F-side UI rendering of mailbox settings — out of 2.B scope.
- Phase 3+ protocol changes (multi-member groups, attachments, etc.).
