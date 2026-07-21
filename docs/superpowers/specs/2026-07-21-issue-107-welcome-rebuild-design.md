# #107 — First-contact lost-Welcome recovery (design — decide-at-spec)

**Issue:** myggiz/skattr#107
**Milestone:** v1.1
**Status:** **Approach A chosen** (2026-07-21). Approach B disclosed as the future
deeper fix. The plan implements Approach A.
**Branch:** `107-welcome-rebuild`

> **Decision (made):** Approach **A** — bounded-retry → clean-fail → re-invite.
> Approach B (full auto-rebuild) is disclosed below and deferred.

## Problem (recap)

The #93 durable welcome-sweep (`crates/core/src/delivery/welcome_sweep.rs`)
re-sends the **stored `welcome_bytes`**. Those bytes are cryptographically bound
to the Noise connection they were built on: `h_transport =
HKDF(noise_handshake_hash, "skattr-binding-v1")` (ADR 0009), injected as an
external PSK into the two-PSK `add_member` genesis Commit. On any re-send over a
**different** connection (dial retry, guard reset, restart), the responder
derives that connection's *new* `h_transport` and registers that PSK, while the
stored Welcome references the *old* one → `Psk(KeyNotFound)`, **permanently**.

This is a regression vs v0.1.0 only in *durability*: v0.1.0 had no sweep, so a
failed Welcome simply failed and the user re-invited over a fresh connection
(fresh `h_transport` → binds). #93 turned a transient failure into a permanent
stuck "Connecting…".

## Current mechanism (verified)

- `add_contact` (consumer/committer, `dispatch.rs`): parse invite →
  `invitee_kp = KeyPackage::from_bytes(link.body.key_package)` (the peer's KP)
  + `kp_ref` + `link.psk.0` (invite PSK); **dial the inviter first** →
  capture `h_transport`; `Group::create_solo(.., invite_psk, h_transport_psk)`
  → `group.add_member(invitee_kp, invite_psk, h_transport_psk)` → `welcome`;
  store `pending_welcomes{peer, group_id, welcome_bytes, next_retry_at,
  attempts, created_at}`. The per-peer actor reuses *this* connection for the
  first Welcome (no second dial) — so the **first** attempt is correctly bound;
  only **re-sends** are stale.
- `run_welcome_sweep`: `due()` → `hub.send_welcome(peer, row.welcome_bytes)`
  (the actor dials a *fresh* connection and sends the stored bytes) → await
  `Frame::Ack([u8;16])` (bounded `ACK_TIMEOUT = 45 s`) → on Ack
  `finalize_welcome_ack` (delete row); else `reschedule` with bounded backoff
  (`BACKOFF_MS = [5,15,30,60] s`). **The sweep never stops** — it retries
  forever.
- The **Ack is a bare 16-byte correlator** — it carries no group_id/epoch. The
  invitee finalizes by *peer only* (keeps whatever group is currently linked).
- The responder already derives `h_transport` **per-connection** and registers
  the PSK before `join_from_welcome` — so the responder side is correct; the fix
  is entirely **consumer-side**.

## The crux (why the naive rebuild is wrong)

Re-minting a fresh genesis per re-send produces a **new group_id each attempt**.
Because the Ack is peer-only (no group_id), a naive "re-mint + relink group_id"
**collides with #93's lost-Ack recovery**: if the responder already joined an
*earlier* mint but its Ack was lost, a later re-mint relinks the invitee to the
*new* group while the responder sits in the *old* one — a new corruption class
the Ack cannot disambiguate. The invitee cannot distinguish lost-Welcome
(responder never joined → must re-mint) from lost-Ack (responder joined → must
**not** re-mint) from a timeout alone. Correctly resolving this requires the Ack
to identify the responder's joined group_id.

---

## Approach A — bounded-retry → clean-fail → re-invite (recommended, small)

**Idea:** don't try to auto-recover the connection-rebind; instead restore
v0.1.0's *recoverability*. Bound the sweep; when a first contact can't complete,
stop retrying and surface it honestly so the user removes it (now possible via
**#109**) and re-invites over a fresh connection (fresh `h_transport` → binds).

**Changes:**
1. **Bound the sweep.** Add a stop condition to `run_welcome_sweep`: when
   `attempts >= MAX_WELCOME_ATTEMPTS` (or `now - created_at >= MAX_WELCOME_AGE`),
   stop rescheduling that row — do not keep dialing forever. (Constants, e.g.
   `MAX_WELCOME_ATTEMPTS = 10`, `MAX_WELCOME_AGE = 24 h` — tuned in the plan.)
2. **Model the failed state durably** (do *not* just delete the row — deleting it
   would clear `is_pending`, and since `GroupState` isn't persisted
   (`Group::load` → always `Active`), the contact would then mis-render as
   *connected*, re-triggering the #101 bug). Add a `failed` flag (or a `status`)
   to `pending_welcomes` (migration). A `failed` row keeps `is_pending == true`
   (still not Active) but the sweep skips it (no more dials).
3. **Surface it in the UI.** Extend #101's `pendingState` with a `failed` arm →
   *"Couldn't connect — remove this contact and send a new invite."* (a distinct
   message from the transient "Connecting…"/"Not connected yet"). The composer
   stays blocked. The existing #109 Remove action clears it; a fresh add binds.
4. **(Optional) auto-offer removal** — the #109 Remove button is already present
   for pending contacts, so this may need no new UI control beyond the message.

**Cost/risk:** small. One migration + a stop condition + a UI state. **No
ADR, no protocol/wire change, no touch to ADR 0009.** Does **not** auto-recover
lost-Welcome — the user re-invites — but kills the permanent-stuck wart, which
*is* the regression the issue documents.

**Testing:** the sweep stops after the cap (no infinite reschedule); a capped
row is `failed` + still `is_pending` (never mis-rendered Active); the UI shows
the failed message; #109 Remove clears a failed contact; a fresh add after
removal binds (component-level, matching prior first-contact test patterns).

---

## Approach B — full auto-rebuild (the deep fix, large)

**Idea:** the sweep rebuilds the Welcome per connection so a circuit change
auto-recovers.

**Changes (architecture level):**
1. **Store rebuild inputs, not bytes.** Persist what's needed to re-mint: the
   peer's KeyPackage (`link.body.key_package`), `kp_ref`, and the invite PSK
   (`link.psk.0`) — new columns on `pending_welcomes` (or a sibling table),
   replacing/augmenting `welcome_bytes`. Survives restart.
2. **Atomic dial → h_transport → rebuild → send seam.** Replace
   `hub.send_welcome(peer, bytes)` with a flow where the actor dials, hands back
   *this connection's* `h_transport`, the consumer rebuilds
   `create_solo + add_member(fresh h_transport)` → a fresh Welcome, and sends it
   on the **same** connection (a new hub/actor API — the connection must stay
   open across capture→build→send).
3. **Ack carries the joined group_id** — a new additive transport frame (next
   free `FrameType`, like 3.B's 0x0B–0x0E): the responder Acks with the group_id
   it actually joined. The invitee keeps the **matching** provisional mint and
   discards the others (bounded set of un-resolved mints). This resolves the
   crux and reconciles with #93 (a lost-Ack re-Ack now carries the responder's
   real group_id, so the invitee relinks to the correct earlier mint).
4. **Reconcile #93/ADR 0012.** The responder's idempotent re-Ack path must also
   emit the joined group_id; the invitee's finalize selects the provisional mint
   by that group_id rather than by peer alone.

**Cost/risk:** large. **Two ADR updates** — ADR 0009 (per-connection rebuild
supersedes stored-bytes) + a **new ADR** for the Ack-carries-group_id frame —
plus a bounded multi-mint provisional-group model, and it touches the core
transport↔MLS binding. Heavy crypto second-review; realistically multi-session.
Auto-recovers lost-Welcome without user action.

**Testing:** a live `run_with_transport` guardrail that drops the first
connection (guard-reset analog) and asserts the re-mint over a new connection
binds + first contact completes; plus a lost-Ack test proving the group_id-Ack
keeps the *right* mint (no #93 regression); plus cross-restart rebuild from the
stored inputs.

---

## Recommendation

**Approach A for v1.1 now; disclose Approach B as the future deeper fix.**

Rationale (matches the repo's governing bar — cheap security/correctness fixes
pulled forward, deep protocol work disclosed): the *regression* the issue
documents is "transient failure became permanent stuck." Approach A removes that
at low risk and no protocol change, and #109 already provides the manual
recovery it leans on. Approach B auto-recovers a case A makes cleanly
recoverable by hand, at the cost of two ADRs and changes to the core security
binding — high effort/risk for incremental UX. Ship A, disclose B in the
threat-model/limitations as the remaining gap.

## Out of scope (both)

- Any weakening of ADR 0009's per-connection `h_transport` binding (locked crypto
  decision — a MITM-resistance property; not on the table).
- The responder side (already correct — derives `h_transport` per connection).
- First-contact **Welcome mailbox fallback** (#37) — orthogonal; still
  direct-only.

## Acceptance criteria

**If A:** the sweep stops after the cap (no infinite dialing); a capped first
contact is durably `failed` + still `is_pending` (never mis-rendered Active); the
UI shows a distinct "couldn't connect — remove & re-invite" state; #109 Remove
clears it; a fresh add afterward binds. No ADR/protocol change.

**If B:** a first contact whose initial Welcome is lost and whose circuit is
replaced auto-completes on a later sweep tick (re-mint binds); the lost-Ack case
still keeps the responder's actual group (no #93 regression); rebuild works
across a daemon restart from durable inputs; two ADRs written; crypto second-
review passed.
