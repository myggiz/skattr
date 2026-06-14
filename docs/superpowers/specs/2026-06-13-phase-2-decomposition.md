# Phase 2 Decomposition — Critical Security & Data-Integrity (T1)

**Date:** 2026-06-13
**Status:** Decomposition approved; per-sub-project specs to follow.
**Source:** `docs/V1.0-READINESS-AUDIT.md` (T1 + named T2 items); the
`docs/superpowers/specs/2026-06-12-v1.0-roadmap.md` Phase 2 section.
**Predecessor:** Phase 1 (1A/1B/1C) merged — direct P2P transport + first
contact work end-to-end.

This is a planning document, not an implementation spec. Phase 2 — the audit's
critical security & data-integrity workstream — is decomposed into **four
sub-projects**, each with its own `spec → plan → implement → verify` cycle
(mirroring Phase 1 → 1A/1B/1C). Ground-truth state for each item was verified
against the code on 2026-06-13.

---

## Sub-projects

### 2.A — MLS ratchet & binding integrity (the crypto core)

The protocol-soundness items; all touch `mls/group.rs` + the send/receive
critical section + invite-PSK registration, so they are designed together and
anchored by one ADR.

| Item | Audit | State | Fix touches |
|---|---|---|---|
| `h_transport` ↔ MLS binding | T1-1 | **ABSENT** — derived (`transport/noise.rs`), exposed (`AuthenticatedConnection::h_transport()`), but never injected into the first MLS Commit; the MLS layer registers only the *invite* PSK under the same `skattr-binding-v1` id. | `mls/group.rs`, `daemon/dispatch.rs`, `daemon/inbound.rs` + **ADR** (implement the external-PSK injection, or formally retire the claim from design/threat-model). |
| Per-group send lock | T1-3 | **ABSENT** — load→encrypt→save is `pool.transaction`-atomic on disk but has no cross-task serialization; two concurrent ops on a group encrypt at the same ratchet generation → undecryptable "sent" messages. | `daemon/dispatch.rs`, `daemon/inbound.rs` (a `group_id`-keyed async mutex around the critical section). |
| Inbound-Commit handling | T2-2 | **ABSENT** — `Group::decrypt` errors on `ProcessedMessageContent::StagedCommitMessage` and gates reads on `can_send()`. | `mls/group.rs` (split read/send predicates; merge an inbound staged Commit), `daemon/inbound.rs`. |
| Per-invite PSK uniqueness | T2-8 | **ABSENT** — `PreSharedKeyId::external(b"skattr-binding-v1", [0u8;32])` is a fixed id+nonce across all invites; a second invite overwrites the first in the provider PSK store (masked only by single-group scope today). | `mls/group.rs` (derive a unique PSK id/nonce per invite, e.g. from the `KeyPackageRef`). |
| Invite single-use atomicity | T2-1 | **ABSENT** — `add_contact`'s `is_consumed` check (→ group/contact writes →) `mark_consumed` is not one transaction; a concurrent/retried submit or a crash mid-sequence can create two groups for one invite. | `daemon/dispatch.rs` (wrap check-and-consume + writes in one `pool.transaction`). |

**Exit criteria:** the `h_transport` decision is made and reflected in code +
docs (ADR); a concurrency test proves no ratchet race on a shared group; an
inbound Commit merges instead of stalling delivery; per-invite PSKs are unique;
the invite is single-use under concurrent + crash-retry conditions.

### 2.B — At-rest encryption lifecycle

| Item | Audit | State | Fix touches |
|---|---|---|---|
| At-rest DB encryption | T1-2 | **ABSENT** — `Pool::close()` re-encrypts `skattr.sqlite → .age` + unlinks plaintext, but `Pool` lives behind `Arc` and `close(self)` is never reachable; no `Drop`; plaintext persists after every shutdown; `export_backup` always fails (no `.age`). | `storage/pool.rs` (reachable close / `Drop`), `daemon/state.rs` (reclaim ownership in teardown after the subsystem `Arc`s drop; crash-residue: lock sentinel + re-encrypt-on-boot), restore `export_backup`. |

Fully independent of the other sub-projects. Code-only here; the
docs-truthfulness follow-through (`passphrase-recovery.md`, `OPERATIONS.md`)
is Phase 4.

**Exit criteria:** plaintext `skattr.sqlite` (+WAL/SHM) is gone after a clean
shutdown and re-encrypted-on-boot after a crash; `export_backup` works.

### 2.C — Offline delivery: fallback + drain (client mailbox path)

| Item | Audit | State | Fix touches |
|---|---|---|---|
| Direct→mailbox fallback wiring | T1-6 | **ABSENT** — `DeliveryHub::ensure_mailbox_fallback` exists + is tested, but the hub is built with `fallback: None`, the per-peer direct-timeout trigger (Task 20.5) is unwired, and the outbox retarget is broken (`set_mailbox_target` flips `target_kind` but leaves `target = peer`; `row_to_entry` discards `target_kind`; no mailbox-deposit retry loop). | `delivery/peer.rs` (timeout trigger), `daemon/state.rs` (construct the fallback), `storage/outbox.rs` + `delivery/outbox.rs` (retarget + honor `target_kind` on retry; mailbox-deposit path). |
| RemoveMailbox drain | T1-4 | **PARTIAL** — `handle_remove_mailbox` calls `run_one_poll_tick` (fetch+delete) but `let _ =`-discards the deposits (Task 22.5); held offline messages are destroyed on mailbox removal. Can now reuse 1A's `dispatch_mailbox`. | `daemon/dispatch.rs` (dispatch drained deposits via `dispatch_mailbox` before finalizing). |
| ts-replay poison-deposit | (1A TODO) | **OPEN** — a mailbox deposit rejected by the ±1h `Envelope.ts` replay window is retained and re-fetched every poll (poison) and never surfaces; legitimate for store-and-forward. | `daemon/inbound.rs` / `mailbox/poll.rs` (terminal-handle or exempt the mailbox path from the live-window check). |

Closes Phase 1's deferred "one peer offline → mailbox" guardrail half. Soft
dependency on 2.A (deliveries should ride a sound ratchet).

**Exit criteria:** fallback fires automatically on sustained direct-delivery
failure and delivers offline; mailbox-kind outbox rows retry over the mailbox
path; removing a mailbox preserves held messages; the offline guardrail (extend
the Phase 1 loopback test, one peer offline → mailbox → poll → receive) passes;
no poison deposit.

### 2.D — Resource hardening (anti-flood; independent)

| Item | Audit | State | Fix touches |
|---|---|---|---|
| Mailbox server hardening | T1-5 | **PARTIAL** — has per-conn + global token buckets + a per-recipient storage cap; **missing** global storage cap, recipient-count cap, LRU-fallback eviction (so a legit deposit can always land), idle-connection timeout, per-server connection semaphore, and a bounded `Delete.deposit_ids` length. | `crates/mailbox/src/` (`store.rs`, `policy.rs`, `server.rs`). |
| Accept-loop spawn bound | (1B TODO) | **OPEN** — the daemon's inbound accept loop spawns a detached handshake task per stream with no concurrency bound (onion-gated + 30s-timeout-bounded today). | `crates/core/src/daemon/accept.rs` (Semaphore permit + JoinSet drain on shutdown). |

Fully independent — the mailbox crate is separate (AGPLv3) and the wire protocol
is frozen (ADR 0006); parallelizable with everything else.

**Exit criteria:** the mailbox server survives an anonymous-flood and a targeted
victim-fill without unbounded disk or victim lockout (LRU lets a fresh legit
deposit land); idle connections time out; concurrent connections are bounded; an
oversize `Delete` is rejected; the daemon accept loop bounds concurrent
handshakes.

---

## Dependencies & sequencing

```
2.A (MLS integrity)  ──soft──►  2.C (offline delivery rides a sound ratchet)
2.B (at-rest)        independent
2.D (hardening)      independent (separate crate; parallelizable)
```

The only dependency is soft (2.C after 2.A). **Recommended order:
2.A → 2.C → (2.B, 2.D any time / parallel).**

- **2.A first** — the security core: an active ratchet-race data-corruption bug
  (T1-3) plus the `h_transport` binding decision that underpins the whole threat
  model. Everything else is sounder once it lands.
- **2.C next** — closes Phase 1's deferred offline guardrail.
- **2.B, 2.D** — independent; slot in whenever (2.D is a good parallel/standalone
  candidate since it's a separate crate).

## Out of Phase 2 scope (unchanged deferrals)

Real onion-key rotation (Task 23.5), multi-member groups, metadata-minimization,
third-party audit — all **v1.1+** per the v1.0 roadmap. Reactions/edit/delete/
typing remain inert. Docs-truthfulness + release CI are **Phase 4**.

## Delivery model

Each sub-project: `spec (docs/superpowers/specs/) → writing-plans →
subagent-driven execution → verification`. The next step after this
decomposition is the **2.A spec** (anchored by the `h_transport` ADR decision).
