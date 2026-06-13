# Phase 2.A — MLS Ratchet & Binding Integrity (Design)

**Date:** 2026-06-13
**Status:** Approved design; implementation plan to follow.
**Decomposition:** `docs/superpowers/specs/2026-06-13-phase-2-decomposition.md`
(sub-project 2.A).
**New ADR:** `docs/adr/0009-h-transport-mls-binding.md` (the binding
construction). **Relates:** ADR 0007 (identity binding), ADR 0008 (invite card).

The crypto-core sub-project of Phase 2: make the MLS layer's ratchet and
transport binding sound. Five items, grouped because they all touch
`mls/group.rs` + the send/receive critical section + invite-PSK registration,
and three of them (T1-1, T2-8, T2-1) converge on `add_contact`.

---

## 1. Problem (verified ground truth, 2026-06-13)

- **T1-1 `h_transport` binding — ABSENT.** Derived in the Noise handshake,
  exposed on `AuthenticatedConnection`, but never injected into MLS (dialer drops
  `_outcome`; accept loop uses `outcome` only for the x25519 lookup; `group.rs`
  registers only the invite PSK).
- **T1-3 per-group send lock — ABSENT.** `load → encrypt/decrypt → save` is
  `pool.transaction`-atomic on disk but has no cross-task serialization; two
  concurrent ops on a group encrypt at the same ratchet generation → undecryptable
  "sent" messages.
- **T2-2 inbound-Commit handling — ABSENT.** `Group::decrypt` gates on
  `can_send()` and errors on `ProcessedMessageContent::StagedCommitMessage`.
- **T2-8 per-invite PSK uniqueness — ABSENT.** Fixed `PreSharedKeyId::external(
  b"skattr-binding-v1", [0u8;32])` across all invites ⇒ second invite overwrites
  the first in the provider PSK store.
- **T2-1 invite single-use atomicity — ABSENT.** `add_contact`'s `is_consumed`
  check → group/contact writes → `mark_consumed` is not one transaction ⇒ a
  concurrent/retried submit or a crash mid-sequence can create two groups for one
  invite.

## 2. Goal & scope

**Goal:** the MLS group is cryptographically bound to the authenticated Noise
transport transcript from its genesis Commit; concurrent operations on a group
cannot race the ratchet; invites are single-use and their PSKs unique; the MLS
layer tolerates an inbound Commit.

### In scope
1. **`h_transport` binding (T1-1) + per-invite PSK uniqueness (T2-8)** via the
   dial-first genesis two-PSK commit (ADR 0009).
2. **Per-group send lock (T1-3).**
3. **Invite single-use atomicity (T2-1).**
4. **Inbound-Commit handling (T2-2)** — defensive (no PCS in v1.0); lowest
   priority but kept (audit-flagged, cheap).

### Out of scope (other Phase 2 sub-projects / later)
At-rest encryption (2.B), offline fallback + drain (2.C), resource hardening
(2.D), PCS/`advance_epoch`, multi-member groups, onion rotation.

### Non-goals
No new `Frame`/`Command`/`CommandResult`/`Event` variants. No change to the
Noise pattern or MLS ciphersuite. The `h_transport` HKDF label
(`identity/derive.rs`) is unchanged.

## 3. Components & changes

### 3.1 `h_transport` plumbing
- `delivery/dial.rs::TransportDial::dial` — return `(AuthenticatedConnection,
  h_transport)` (or a small struct) instead of dropping `_outcome`. The
  `OutboundDial` trait method's return type widens accordingly.
- `daemon/accept.rs::run_accept_loop` — forward `outcome.h_transport` into the
  Welcome-bootstrap call (currently only `outcome.peer_x25519` is used).

### 3.2 MLS two-PSK genesis commit (`mls/group.rs`)
- Replace the constant `PSK_ID_BYTES` + `register_external_psk` with a
  `psk_id(label, kp_ref)` helper (ADR 0009): `PreSharedKeyId::external(b"skattr-"
  ++ label ++ "-v1" ++ kp_ref, nonce = kp_ref)`.
- `create_solo` / `add_member` gain an `h_transport: Option<&[u8;32]>` parameter
  (in addition to the existing invite-PSK parameter) and the `kp_ref` needed to
  derive ids. `add_member` registers + proposes **both** PSKs in the genesis
  Commit.
- `join_from_welcome` gains the same `h_transport` + `kp_ref` so the joiner
  registers both PSKs before processing the Welcome.

### 3.3 `add_contact` reorder (dial-first; closes T1-1 committer side, T2-1)
1. Resolve the inviter onion from `link.body.card.body.onion` → dial →
   `(conn, h_transport)`.
2. `hub.ingest(inviter, conn)`.
3. Build the genesis group with invite PSK + `h_transport` (ids from `kp_ref`).
4. **One `pool.transaction`**: `is_consumed` check + persist {group, contact,
   card, group_id} + `mark_consumed`. (Dial precedes the txn; MLS state is the
   provider snapshot persisted inside it.)
5. Send the Welcome over the ingested connection (then the invitee's self-card,
   1C-T3, over the same connection).

### 3.4 Inviter Welcome-processing (`daemon/inbound.rs`)
`dispatch_welcome_bootstrap` / `welcome_join_persist` gain `h_transport` (from
the accept loop's `outcome`); register both PSKs before `join_from_welcome`.

### 3.5 Per-group send lock (T1-3)
A `group_id`-keyed async-mutex registry — `Arc<Mutex<HashMap<[u8;32],
Arc<tokio::Mutex<()>>>>>` on a shared component reachable from `DaemonHandle`
(used by both `dispatch::send_message` and `inbound::dispatch_for_group`).
Acquire the per-group lock around the whole **load → encrypt/decrypt → save**
critical section; the existing `pool.transaction` stays inside it. (Card-send
and Welcome-bootstrap paths that touch a group also take the lock.)

### 3.6 Inbound-Commit handling (T2-2, defensive)
Split `Group::decrypt`'s `can_send()` gate into separate read vs send
predicates (a `can_receive()`-style check on `state_machine.rs`); on
`ProcessedMessageContent::StagedCommitMessage`, merge it (route to the existing
`process_incoming_commit`) and return a no-op/`None`-equivalent instead of
erroring, so an inbound Commit advances the epoch rather than stalling delivery.

## 4. Data flow (first contact, with binding)

1. Bob `AddContact`: resolve Alice's onion → **dial Alice → `(conn, h_t)`** →
   `ingest` → build genesis Commit with `psk_id("invite",ref)` + `psk_id(
   "htransport",ref)` → one-txn persist + consume → send Welcome over `conn`.
2. Alice (accept loop): inbound handshake → **same `h_t`** + `outcome.peer_x25519`
   → `dispatch_welcome_bootstrap(welcome, peer_x25519, h_t)` → register both PSKs
   → `join_from_welcome` validates the binding → group Active, bound to the
   transport transcript.
3. Steady-state messaging: every send/receive on the group takes the per-group
   lock; ratchet advances are serialized.

## 5. Error handling

- **Dial failure in `add_contact`** → fail the add cleanly (no DB writes yet; the
  persist txn hasn't run). Surface as the existing `AddContact` error.
- **`h_transport`/invite PSK mismatch on join** → `join_from_welcome` fails (same
  path as a bad invite PSK today); the Welcome is rejected.
- **Send-lock contention** → tasks serialize (no error; bounded by op latency).
- **Inbound Commit** → merged, not errored (T2-2).

## 6. Testing

- **Unit (`mls/group.rs`):** genesis two-PSK commit round-trips — Bob commits
  with invite + `h_transport` PSKs, Alice joins with the same two, both validate;
  a wrong `h_transport` → join fails; two invites produce distinct PSK ids (no
  overwrite — T2-8).
- **Unit (invite atomicity, T2-1):** concurrent / repeated `AddContact` of one
  invite creates exactly one group; a simulated mid-sequence failure leaves the
  invite re-usable or the group fully written, never two groups.
- **Concurrency (T1-3):** N concurrent `send_message` on one group → all
  ciphertexts decrypt (no generation collision); a regression (lock removed)
  reproduces the race.
- **Inbound-Commit (T2-2):** feeding a `StagedCommitMessage` merges + advances
  the epoch instead of erroring.
- **Guardrail:** extend the 1C first-contact loopback test to assert the genesis
  group carries the `h_transport` binding PSK, and confirm the dial-first reorder
  keeps first contact + bidirectional delivery green end-to-end.

## 7. Exit criteria

The `h_transport` binding is injected into + validated on the genesis Commit
(ADR 0009); per-invite PSKs are unique; a concurrency test proves no ratchet
race; the invite is single-use under concurrent + crash-retry; an inbound Commit
merges; the 1C guardrail still passes (extended to assert the binding);
`cargo fmt --check`, `cargo clippy --workspace --exclude skattr-ui --all-targets
-- -D warnings`, and the full non-ignored suite are green; CLI builds.

## 8. Visibility & wire notes

All changes are within existing modules (`transport`, `delivery`, `mls`,
`daemon`); `OutboundDial`'s return type widens (still `pub(crate)`). No
public-API widening beyond that. The only protocol change is the MLS-internal
two-PSK genesis commit (ADR 0009) — wire-format neutral at the Skattr layer.
