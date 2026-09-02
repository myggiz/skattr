<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->

# Outbox delivery truthfulness — design

Date: 2026-09-02
Issues: #227, #228, #229
Status: approved for planning

## Context

Field forensics on a live v0.1.24 install (Linux ↔ Windows, 2026-09-01/02)
found six outbound messages that had been "pending" in the UI for 23 hours.
Three independent defects stack to produce that, and a fourth condition —
the real one — makes the outcome unrecoverable.

Evidence (`~/.local/share/skattr/{skattr.sqlite,skattr.log}`, all UTC):

```
2026-09-01T02:21:46..55  six outbound dial failures ("Onion Service not found")
2026-09-01T14:56         daemon restart
14:56 -> 2026-09-02T01:25 ZERO delivery log lines - no retry in 10.5h
2026-09-02T01:25:10      inbound message arrives (the PEER dialed us)
2026-09-02T01:25:13      all six rows finally hit the wire, attempts 0 -> 1
2026-09-02T01:31         still queued; a new message sent at 01:30:07 was
                         acked on that same connection
```

### The three defects

**#227 — the retry tick has no dialer.** `delivery/peer.rs`, in the retry
tick's due-rows loop:

```rust
let Some(c) = conn.as_mut() else { break; };
```

With no live connection the tick breaks and does nothing. Only the `jobs`
arm and the #76 chunk-fetch block call `ensure_conn`. A queued message
therefore never causes a dial; it waits for a connection someone else
creates. That is why the six rows moved only when the peer dialed in.

**#229 — un-acked retry-tick sends wedge their row.** The tick inserts into
the in-memory `pending` map with a dropped receiver (`let (tx, _rx) =
oneshot::channel()`), so nothing times out. Only an `Ack` or `drain_pending`
(connection loss) removes it. The guard `if pending.contains_key(...) {
continue; }` then skips that row on every later tick — forever, while the
connection stays healthy. Observed: `attempts` frozen at 1 through ~360
ticks on a working connection.

**#228 — the ±1h window makes a stale row permanently undeliverable.**
`delivery/receiver.rs:75` rejects any envelope whose `ts` is more than
`REPLAY_WINDOW_MS` (1h) from the receiver's clock; the direct path passes
`enforce_ts_window = true`. The sender cannot re-stamp, because
`outbox.payload` is frozen MLS ciphertext. So a row that waits over an hour
is retried forever and silently dropped by the peer every time.

### The condition underneath: no mailbox

The mailbox path is *deliberately* exempt from the ts window (2.C's
ts-replay poison fix), because store-and-forward is precisely the answer to
a peer being offline. The direct-timeout fallback exists to route around
this — and it fired. `delivery/hub.rs:494`:

```rust
if onions.is_empty() {
    tracing::debug!("fallback: peer has no advertised mailboxes; \
                     leaving outbox row untouched");
    return;
}
```

The install has `direct_timeout_secs = 30`, so ~30s into the outage the
fallback ran, found the peer advertises no mailbox, logged **below the
configured filter level**, and returned. The mechanism designed to handle
this case was reachable, found nothing to do, and said so where no one
would see it.

Note the direction: `list_for_contact(&peer)` reads the **recipient's**
mailboxes. Deposits go to the mailbox your contact advertises, carried in
the signed `ContactCard.mailboxes` field. Enabling a mailbox locally makes
*you* reachable while offline; it does nothing for messages you send.

## Decisions

Locked with the maintainer during brainstorming:

1. **The mailbox is the offline-delivery answer.** Where a contact
   advertises a mailbox, a queued message must never expire — it waits as
   long as it needs to. Discarding undelivered messages instead of
   depositing them would defeat the purpose of having a mailbox at all.
2. **Expiry applies only when there is no mailbox.** Then direct is the
   only lane, and past the ts window it provably cannot deliver.
3. **The give-up deadline is the protocol deadline**, not a tuned value:
   `REPLAY_WINDOW_MS` minus a clock-skew margin. Coarse by construction.
4. **Dismiss keeps the row**, marked dismissed. It stays in history and in
   FTS. The action is therefore named *Dismiss*, not Delete.
5. **Resend is a new message.** Frozen ciphertext cannot be re-stamped, so a
   resend is a fresh envelope with a new `ts` and MLS generation — appended
   at the bottom, because that is genuinely what it is.
6. **Surface the no-mailbox fact at failure time only**, on the failed
   bubble. Not in the contact panel, not in the composer.

### Rejected, with reasons

- **Re-stamping on retry** (re-encrypt with a fresh `ts`, no expiry ever).
  Technically viable and cheaper at rest than it first appears —
  `messages.body_text` already stores the same plaintext in the same DB for
  FTS — but it makes the mailbox redundant, changes MLS generation
  ordering, and can deliver a message stale enough to embarrass. The
  mailbox already solves this correctly.
- **A short (5–10 min) give-up deadline.** It becomes a pollable presence
  oracle: send, wait, repeat, and you have an occupancy log. Skattr has no
  presence signal by design (#196). Coarseness is the mitigation.
- **A configurable deadline.** Any value past ~1h only reintroduces silent
  non-delivery; the knob's sole effect is to let users re-break it.

## Design

### 1. #227 — a paced dialer in the retry tick

In `full_run`'s retry tick, when this peer has due direct rows and
`conn.is_none()`, call `ensure_conn` before the send loop. Pace it with the
existing `CHUNK_DIAL_BACKOFF_MS` ladder (`15s → 60s → 5m → 15m`, held at the
last entry) rather than a second scheme: a failed Tor dial costs up to
`DIAL_TIMEOUT` (30s) inline against a 1s `RETRY_TICK`, so an unpaced dial is
a storm against an offline peer.

On dial failure, call `arm_failure(&mut first_failure_at)`. This is the
load-bearing half: today only the `jobs` arm arms the timer, so the mailbox
fallback is reachable only from a live user send, never from a row merely
sitting in the queue. Arming here is what makes the designed offline path
work for the case that actually occurs.

### 2. #229 — a deadline on `pending` entries

Track the send instant alongside each retry-tick `pending` entry and evict
entries older than 30s — the deadline the chunk-request path in the same
function already uses. The row becomes eligible again under its own outbox
backoff. This preserves the guard's real purpose (do not double-send a frame
genuinely in flight) while making it recover from a peer that accepts and
never acks.

### 3. #228 — expiry, in a sweeper rather than the peer actor

The give-up check does **not** belong in `full_run`. That function has no
`events` sender in production (only in tests), `peer.rs` is already 3571
lines, and queue lifecycle is not connection work — the existing
`mailbox_sweeper` and `chunk_sweep` are the established home for exactly
this shape of job, and both already hold an events sender and are spawned in
`run_with_transport`.

Add `delivery::outbox_sweep`, a sibling of those two. On each tick, for
direct rows whose envelope `ts` is older than the expiry deadline:

- If the contact advertises a mailbox — leave the row alone. The
  direct-timeout fallback owns retargeting it, and once retargeted the
  sweeper's backoff owns delivery. **No expiry on this path.**
- If the contact advertises no mailbox — delete the row and emit
  `Event::DeliveryStatusChanged { status: DeliveryStatus::Failed(reason) }`,
  where `reason` names the actual cause and the actual remedy: that the
  contact has no mailbox, so messages cannot reach them while offline.

**Why "no expiry" is safe here: the mailbox lane terminates.** A deposit is
not an open-ended wait. `run_mailbox_sweep` deletes the outbox row on a
successful deposit and emits `DeliveryStatus::Deposited`, which the UI
already renders as the single-check "sent" glyph titled *"Delivered to
mailbox"*. So the three states are already distinct and honest:

| icon | meaning |
|---|---|
| clock | ours; still trying |
| ✓ | handed to the recipient's mailbox — our responsibility ends |
| ✓✓ | the peer itself acked, directly |

`Deposited` is terminal by design. We never learn whether the recipient
fetched it, and we should not: there is no fetch signal to leak, which is
the metadata-minimisation property we want. The cost is social rather than
technical — a contact reading from a mailbox looks, from the sender's side,
identical to one ignoring them — and that is the correct trade here.

The consequence for this design is that "pending forever" is structurally
impossible once a mailbox is in play. Removing expiry on that lane removes
nothing, because the lane ends on its own. Expiry is needed only where no
mailbox exists and the row therefore has no terminal state at all.

**Edge case — a mailbox contact whose row was never retargeted.** Leaving
the row alone assumes the direct-timeout fallback will retarget it, which
#227's arming fix makes true for the case observed. If retargeting never
happens for some other reason, the row ages past the window while still
marked `direct` and becomes undeliverable without ever being expired — the
original bug, narrowed to a smaller window. The sweeper must therefore treat
"aged past the deadline, contact has a mailbox, still `target_kind='direct'`"
as a retarget trigger rather than a no-op, so the mailbox lane is entered
even if the peer actor never armed the timer. A row that has already been
retargeted is untouched.

The deadline is derived, not a free constant:

```rust
/// Direct delivery is impossible past `REPLAY_WINDOW_MS`; stop a margin
/// short of it so a message is never written to the wire that the peer
/// will certainly reject.
const DIRECT_EXPIRY_MS: i64 = REPLAY_WINDOW_MS - 5 * 60 * 1000; // 55 min
```

Deriving it means the two can never drift apart.

### 4. Make the silent case loud

Raise `hub.rs:494`'s `debug!` to `info!`. "This peer cannot receive anything
while offline" is the single most consequential fact the delivery path
knows, and it is currently logged beneath the default filter. Redaction
rules still apply: no onion, no pubkey.

### 5. Durable state

Failed-ness needs no storage: "no outbox row, `delivered_at` is null, and
outgoing" already means the daemon gave up. Dismissal is not derivable and
does need a column.

Migration `0021_messages_dismissed_at.sql`:

```sql
ALTER TABLE messages ADD COLUMN dismissed_at INTEGER;
```

Nullable, mirroring the existing `delivered_at` on the same table — same
shape, same convention, no new concept.

`MessageRecord` gains one computed field:

```rust
/// Durable delivery state, derived daemon-side. The session-scoped UI
/// store remains the live overlay; this is the baseline that survives a
/// restart.
delivery_state: DeliveryState,  // Pending | Delivered | Failed | Dismissed
```

An enum, not a pair of bool flags, per the repo's Rust standard. Computing
it daemon-side from `delivered_at`, `dismissed_at` and outbox membership
keeps one source of truth, so the UI never re-derives the deadline and
cannot drift from the daemon on it.

### 6. IPC

One additive command:

```rust
DismissMessage { message_id: Hex16 },
```

Resend needs no command — it is `send()` with the original body, which the
UI already has.

### 7. UI

`conversation.ts`'s existing hydration (line 147, which already seeds
`Delivered` from `delivered_at`) extends to seed `Failed` and `Dismissed`
from `delivery_state`. Same pattern, one more arm.

`MessageBubble` renders the failed state with the `alert-triangle` glyph
that already exists — `DeliveryIcon` and `deliveryToIconStatus` already
handle `"failed"` end to end, and #197 deliberately made the icon *shape*
carry the state so it survives a light theme or a dimmed window. Nothing
new is needed to make it look right; only to make it reachable.

A failed bubble gains the reason text plus **Resend** and **Dismiss**. A
dismissed bubble greys and drops both actions.

**Attachments.** `Kind::File` messages ride the same outbox, so a failed
file bubble must not be the one place left showing an eternal clock.
`FileAttachmentBubble` gains the same failed state, reason, and Dismiss.
It does **not** get Resend: re-sending a file needs the original path, which
may no longer exist, and inventing a recovery story for that is out of scope
here. Dismiss alone is honest.

## Testing

TDD per the repo rule — every test fails before its fix. Each core change
gets a guardrail through the real `run_with_transport` assembly over
loopback, per the audit's defining rule.

1. **#227** — peer offline, message queued, peer returns: the message
   arrives with no new user action and no inbound dial from the peer. This
   guardrail does not exist today and is the one that would have caught the
   field bug.
2. **#227 pacing** — dials against a persistently offline peer are bounded
   over a window, mirroring the existing `dials must be paced by backoff`
   assertion.
3. **#229** — a peer that accepts frames and never acks: the row is
   re-attempted and `attempts` advances while the connection stays up.
4. **#228 no-mailbox** — a row aged past `DIRECT_EXPIRY_MS` for a contact
   with no mailbox is deleted and emits exactly one `Failed`.
5. **#228 with-mailbox** — the same aged row for a contact that *does*
   advertise a mailbox is **not** expired. This is the test that protects
   decision 1 from a later well-meaning simplification.
6. **Dismiss** — survives a restart; the row stays in history and remains
   FTS-searchable.
7. **UI** — vitest for the failed and dismissed bubble states. Note the
   known blind spot: jsdom performs no layout, so these assert content and
   actions, not positioning.

## Out of scope

- #230 (inbound messages invisible until the conversation is reopened) —
  separate defect, separate issue, needs an e2e repro in real layout first.
- Resend for failed attachments (see §7).
- Surfacing mailbox status in the contact panel or composer — explicitly
  rejected above.
- Any change to the mailbox wire protocol (ADR 0006 stays frozen).
