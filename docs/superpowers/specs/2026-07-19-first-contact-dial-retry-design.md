# First-contact dial-first retry (#90)

**Date:** 2026-07-19
**Issue:** #90 (intermittent first-contact failure over real Tor). Relates:
#99 (Arti bump — spiked, does NOT fix #90), #93 (durable Welcome re-send).
**Area:** `daemon::dispatch::add_contact` — first-contact outbound dial.

---

## Problem

First contact over real Tor fails intermittently. The #99 Arti-0.44 spike
measured the outbound onion dial at **~63% success per attempt** (5/8 vs the
0.41 baseline 4/6) — **flaky, not dead**, and unchanged by the version bump.

`add_contact` (`dispatch.rs:356-368`) must dial the inviter's onion **once,
synchronously**, to capture `h_transport` (ADR 0009) before it can build the
genesis Commit. That dial is a **single** attempt: `dial_onion`
(`delivery/dial.rs:78`) wraps `transport.dial` in one `DIAL_TIMEOUT = 30s`
timeout and, on failure, returns `DeliveryErrorKind::Timeout`, which `add_contact`
maps to `DaemonErrorKind::DeliveryTimeout`. Per 2.A atomicity the dial runs
**before** the invite-consuming transaction, so a failure leaves **zero writes**
— no contact is created, and the #93 sweeper (which would otherwise retry
delivery) never gets a chance. `dial.rs:18` even documents the intent ("let the
caller retry"), but no caller does.

So a ~37% single-attempt failure rate becomes a ~37% first-contact failure rate,
even though a retry would almost certainly succeed.

## Goal / non-goals

**Goal:** make the first-contact dial-first resilient to the flaky HS-client
circuit by retrying it a bounded number of times, so `add_contact` succeeds
~95% of the time on a reachable peer.

**Non-goals**
- No change to `dial_onion` / `DIAL_TIMEOUT` or to any **other** dial path
  (message send, the sweeper). The sweeper is already resilient (unbounded
  ≤60 s-backoff retries); only `add_contact`'s synchronous dial-first fails
  fast. Scoping the retry to `add_contact` keeps every other path's latency
  semantics unchanged.
- No Arti bump (#99 spike showed it doesn't help).
- No move of `h_transport` capture off the synchronous path (larger redesign;
  unnecessary — a bounded retry suffices).

## Design

Wrap `add_contact`'s dial-first in a bounded retry:

- **`DIAL_ATTEMPTS = 3`**, **`DIAL_RETRY_BACKOFF = 2s`** (named constants near
  `add_contact`).
- Loop: attempt `dialer.dial_at(...)` (each attempt keeps the existing 30 s
  `DIAL_TIMEOUT` inside `dial_onion`). On success, break with the connection +
  `h_transport`. On failure, if attempts remain, `tokio::time::sleep(BACKOFF)`
  and retry; after the last failure, return `DaemonErrorKind::DeliveryTimeout`
  (unchanged error surface — the UI's existing "couldn't reach — try again"
  path still applies).
- **Probability:** ~63%/attempt → ~95% over 3. **Worst case** (unreachable peer):
  3 × 30 s + 2 × 2 s ≈ **94 s** of "Connecting…" before `DeliveryTimeout`. A
  *successful* dial returns as soon as it lands (typically seconds), so the
  common case is unaffected.
- **Redaction-safe logging:** on each non-final failure,
  `warn!("first-contact: dial attempt {n}/{DIAL_ATTEMPTS} failed, retrying")`
  (no onion / pubkey / error payload — the attempt number only).

### Safety

- The dial is **before** the invite-consuming transaction (2.A): every retry is
  a fresh dial with zero prior writes, so there is no torn/partial state and no
  double-consume risk. The invite is only consumed once, in the single
  transaction that runs *after* a dial finally succeeds.
- Each retry is a fresh Noise handshake → fresh `h_transport`; the genesis Commit
  is built from the `h_transport` of the attempt that succeeded, so the ADR-0009
  binding is intact.
- Bounded (3 attempts): a genuinely-unreachable peer still fails deterministically
  with `DeliveryTimeout`, just after ~94 s instead of ~30 s.

## Error handling

| case | behavior |
|---|---|
| Dial succeeds on attempt 1..3 | proceed to the genesis transaction (unchanged) |
| All 3 attempts fail | `DeliveryTimeout` → UI "couldn't reach — try again" (unchanged) |
| Non-dial error (malformed peer key, no card) | returned immediately, not retried (it won't change across attempts) |
| Invite already consumed | early-out before the dial (unchanged) |

*(Only the transport/timeout dial failure is retried; a deterministic error
— e.g. malformed peer identity or missing contact card — returns on the first
attempt, since retrying cannot change it.)*

## Test plan

- `StubDialer` configured to fail the first K dials then succeed: `add_contact`
  **succeeds** when `K < DIAL_ATTEMPTS` (proves the retry lands), through the
  real `add_contact` path; assert the contact + `pending_welcomes` row exist.
- `StubDialer` that fails **all** attempts: `add_contact` returns
  `DeliveryTimeout` after `DIAL_ATTEMPTS` tries; assert zero writes (no contact,
  no `pending_welcomes` row) — 2.A atomicity preserved across retries.
- (Optional) assert a deterministic non-dial error is not retried.

The existing `StubDialer` in `dispatch.rs` tests already models a dialer; extend
it with a fail-N-then-succeed counter.

## Files (anticipated)

- `crates/core/src/daemon/dispatch.rs` — the bounded retry loop in `add_contact`;
  `DIAL_ATTEMPTS` / `DIAL_RETRY_BACKOFF` constants; `StubDialer` fail-count
  extension + the two tests.
