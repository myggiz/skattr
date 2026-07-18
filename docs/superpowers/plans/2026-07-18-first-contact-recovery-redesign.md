# First-contact recovery redesign — responder idempotent re-Ack — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the lost-Ack first-contact case self-heal by having the responder idempotently re-Ack a re-sent Welcome from a peer it has already joined, instead of rejecting it on the consumed invite.

**Architecture:** The invitee side (durable `pending_welcomes` + sweeper re-send) is already built (Tasks 1–6, committed). This plan adds the responder half: at first-contact join, record `kp_ref → (peer_x25519, peer_identity)` in a new durable table; on a re-sent Welcome whose `kp_ref` matches a record with the **same authenticated `peer_x25519`**, return the stored identity as a re-Ack **without** re-running the MLS join (no PSK/`h_transport` processing, no state mutation). A retargeted fault-injecting guardrail drops the first Ack and proves recovery through the real `run_with_transport` assembly.

**Tech Stack:** Rust 2021, Tokio, `rusqlite` 0.38 (bundled), OpenMLS, `snow` (Noise_XK), `tracing`.

## Global Constraints

- Cargo is not on PATH — prefix every cargo command with `. "$HOME/.cargo/env" &&`.
- Work ONLY on branch `fix/93-first-contact-pending-join`. Do not touch master.
- Every `.rs` file carries the GPLv3 header: `// SPDX-License-Identifier: GPL-3.0-or-later` then `// Copyright (C) 2026 Myggiz AB`.
- No `unwrap()`/`expect()` in non-`#[cfg(test)]` library code — use `?` and typed `CoreError`/`StorageErrorKind`. Inside `#[cfg(test)]` and test-only helpers, `unwrap` is allowed (match existing style).
- Redaction: never log pubkeys, onions, x25519 keys, `kp_ref`, or payload bytes at any level. Log static text + counts only. `CoreError` Display is onion/secret-free.
- `rusqlite` stays pinned at 0.38 — do not bump.
- Model MLS/first-contact states as enums, not bool flags where a state is introduced (existing `GroupState` pattern).
- TDD: the test must fail before the implementation exists. Watch it fail for the right reason.
- Migrations are `include_str!`'d SQL registered in `crates/core/src/storage/migrations.rs` keyed by an integer `version`; the highest existing is `0017_pending_welcomes` (version 17). The new one is version 18. The `SchemaTooNew` downgrade guard already handles the version bump.
- **Protocol/auth rule (CLAUDE.md):** this change touches auth semantics. ADR 0011 is written first (Task 8), and the wiring task (Task 10) requires the **opus crypto/second reviewer**.
- Local gate is authoritative (CI is on-demand). Per-task gate: `. "$HOME/.cargo/env" && cargo fmt --all && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings` then the task's `cargo test`. Full-workspace gate at the end.

## What is already committed (do NOT re-implement)

Tasks 1–6 on this branch: migration `0017_pending_welcomes` + `PendingWelcomeRepo`; genesis `GroupState::PendingJoin` + `set_active`; `send_message` block-while-pending (gated on `PendingWelcomeRepo::is_pending`); `delivery::welcome_sweep` (durable re-send + `finalize_welcome_ack` row-delete on Ack) spawned in `run_with_transport` with a `welcome_nudge`; `RemoveContact` deletes the pending row.

## Key existing symbols (read before implementing)

- `crates/core/src/daemon/inbound.rs`:
  - `welcome_join_persist(&self, welcome_bytes: &[u8], expected_x25519: Option<&[u8;32]>, h_transport: Option<&[u8;32]>) -> Result<PublicKey>` (`:437`). Inside it: `let kp_ref = parse_welcome_kp_hash(welcome_bytes)?;` (`:467`), `let derived = group.peer_identity()?;` (`:513`), the join transaction ending with `kp_repo.mark_consumed_in_tx(tx, &kp_sha256)?; oi.mark_consumed_in_tx(tx, &kp_ref)?;` (`:583-584`).
  - `dispatch_welcome_bootstrap(&self, welcome: &[u8], expected_x25519: &[u8;32], h_transport: Option<&[u8;32]>) -> Option<PublicKey>` (`:676`) — calls `welcome_join_persist(welcome, Some(expected_x25519), h_transport)`.
  - `self.pool: Arc<Pool>` is available on the dispatch struct.
- `crates/core/src/delivery/peer.rs`: `welcome_msg_id(welcome: &[u8]) -> MessageId` (`MessageId(pub [u8;16])`).
- `crates/core/src/daemon/accept.rs:95-119` — the carve-out arm: on `Some(peer)` from `dispatch_welcome_bootstrap` it sends `Frame::Ack(welcome_msg_id(&bytes).0)` then `hub.ingest(peer, conn)`. **This arm is unchanged by this plan** — the re-Ack returns `Some(stored_identity)` so accept.rs Acks + ingests identically.
- `crates/core/src/storage/pending_welcomes.rs` — the repo pattern to mirror (constructor, `insert_in_tx`, typed errors, `#[cfg(feature="test-harness")]` visibility).
- Task-7 harness reference saved at `<scratchpad>/task7-harness-reference/` (`plumbing.diff` = `run_loopback_with_transport` in `state.rs` + `test_exports` in `lib.rs` + `transport.rs` wrapper support + `tests/lib.rs` module registration; `first_contact_welcome_dropped.rs` = a working fault-injecting `Transport` wrapper that parses `length:u32 BE | type:u8 | payload` framing and drops one directional frame). Task 11 reuses this.

---

### Task 8: ADR 0011 — first-contact idempotent re-Ack

**Files:**
- Create: `docs/adr/0011-first-contact-idempotent-reack.md`

**Interfaces:** none (documentation). Written first per CLAUDE.md ("protocol changes need an ADR before code").

- [ ] **Step 1: Write the ADR.** Follow the format of an existing ADR (read `docs/adr/0009-*.md` for house style — Status / Context / Decision / Consequences). Content must state:
  - **Context:** #93 lost-Ack — the responder joins, consumes the invite (`inbound.rs:584`), sends an Ack that is lost; the invitee re-sends the identical Welcome; the current code rejects it (`unknown kp_ref`) and never re-Acks, so first contact is permanently stuck. `h_transport` is per-connection (ADR 0009) so re-processing the Welcome on a new connection cannot work.
  - **Decision:** At first-contact join the responder records `kp_ref → (peer_x25519, peer_identity)` durably. On any first-contact Welcome whose `kp_ref` matches a record **and** whose authenticated Noise static key (`peer_x25519`) equals the recorded one, the responder returns the stored identity as an idempotent re-Ack — it re-sends `Frame::Ack(welcome_msg_id(welcome))` and does **not** re-run the MLS join (no PSK/`h_transport` processing, no group-state mutation). A `kp_ref` match with a **different** `peer_x25519` is rejected (replay by a different peer), preserving KeyPackage single-use. No wire-format change: reuses `Frame::MlsWelcome` and `Frame::Ack` (ADR 0006 frozen).
  - **Consequences:** Lost-Ack self-heals. **Lost-Welcome** (responder never joined) still cannot recover over a fresh circuit (the re-sent Welcome's `h_transport` won't match) — disclosed as a v1.0 limitation tied to #90 Mode A. Security: the re-Ack never touches MLS state, so a malicious/stale/replayed duplicate cannot corrupt the group; the peer is Noise-authenticated so re-Ack cannot be provoked for another identity.

- [ ] **Step 2: Commit**

```bash
git add docs/adr/0011-first-contact-idempotent-reack.md
git commit -m "docs(adr): ADR 0011 — first-contact idempotent re-Ack (#93)"
```

---

### Task 9: `first_contact_acks` durable storage

**Files:**
- Create: `crates/core/src/storage/migrations/0018_first_contact_acks.sql`
- Create: `crates/core/src/storage/first_contact_acks.rs`
- Modify: `crates/core/src/storage/migrations.rs` (register version 18)
- Modify: `crates/core/src/storage/mod.rs` (add `mod first_contact_acks;` / `pub(crate) mod`, mirroring how `pending_welcomes` is declared)

**Interfaces:**
- Consumes: `Pool`, `rusqlite::Transaction`, `CoreError`/`StorageErrorKind` (as `pending_welcomes.rs` does).
- Produces: `FirstContactAckRepo` with:
  - `new(pool: &Pool) -> Self`
  - `insert_in_tx(&self, tx: &rusqlite::Transaction<'_>, kp_ref: &[u8;32], peer_x25519: &[u8;32], peer_identity: &[u8;32], now_ms: i64) -> Result<()>` (uses `INSERT OR IGNORE` — idempotent on duplicate join attempts)
  - `lookup(&self, kp_ref: &[u8;32]) -> Result<Option<FirstContactAck>>` where `pub(crate) struct FirstContactAck { pub peer_x25519: [u8;32], pub peer_identity: [u8;32] }`

- [ ] **Step 1: Write the migration SQL.** Create `0018_first_contact_acks.sql`:

```sql
-- SPDX-License-Identifier: GPL-3.0-or-later
CREATE TABLE first_contact_acks (
    kp_ref        BLOB PRIMARY KEY NOT NULL,
    peer_x25519   BLOB NOT NULL,
    peer_identity BLOB NOT NULL,
    created_at    INTEGER NOT NULL
);
```

- [ ] **Step 2: Register the migration.** In `crates/core/src/storage/migrations.rs`, add after the version-17 entry, matching the existing struct-literal style:

```rust
    Migration {
        version: 18,
        sql: include_str!("migrations/0018_first_contact_acks.sql"),
    },
```

- [ ] **Step 3: Write the failing test.** In `crates/core/src/storage/first_contact_acks.rs`, add a `#[cfg(test)]` module (mirror `pending_welcomes.rs` tests — use `Pool::in_memory()` or the crate's test pool helper):

```rust
    #[test]
    fn insert_then_lookup_roundtrips_and_is_idempotent() {
        let pool = test_pool(); // same helper pending_welcomes tests use
        let repo = FirstContactAckRepo::new(&pool);
        let kp = [7u8; 32];
        let x = [8u8; 32];
        let id = [9u8; 32];
        pool.with_tx(|tx| {           // use the crate's tx helper; see pending_welcomes
            repo.insert_in_tx(tx, &kp, &x, &id, 1_000).unwrap();
            repo.insert_in_tx(tx, &kp, &x, &id, 2_000).unwrap(); // OR IGNORE: no error
            Ok(())
        })
        .unwrap();
        let got = repo.lookup(&kp).unwrap().unwrap();
        assert_eq!(got.peer_x25519, x);
        assert_eq!(got.peer_identity, id);
        assert!(repo.lookup(&[0u8; 32]).unwrap().is_none());
    }
```

(If `pending_welcomes.rs` uses a specific pool/tx helper name, use the identical one — read that file first and match it exactly.)

- [ ] **Step 4: Run — expect FAIL** (type not defined). Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib storage::first_contact_acks 2>&1 | grep -E "test result:|error\[|FAILED"`

- [ ] **Step 5: Implement `FirstContactAckRepo`** in `first_contact_acks.rs` with the GPLv3 header, mirroring `pending_welcomes.rs` structure (same imports, same error mapping, same `#[cfg(feature = "test-harness")]`/`pub(crate)` visibility as `PendingWelcomeRepo`). `insert_in_tx` runs `INSERT OR IGNORE INTO first_contact_acks (kp_ref, peer_x25519, peer_identity, created_at) VALUES (?1, ?2, ?3, ?4)`. `lookup` runs `SELECT peer_x25519, peer_identity FROM first_contact_acks WHERE kp_ref = ?1` and maps a row to `FirstContactAck` (guard each blob to `[u8;32]` via `try_into`, returning `StorageErrorKind::Other` on a wrong length, exactly as `pending_welcomes::due` guards `peer_pubkey`).

- [ ] **Step 6: Run — expect PASS.** Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib storage::first_contact_acks`

- [ ] **Step 7: Lint + commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings
git add crates/core/src/storage/first_contact_acks.rs crates/core/src/storage/migrations/0018_first_contact_acks.sql crates/core/src/storage/migrations.rs crates/core/src/storage/mod.rs
git commit -m "feat(#93): first_contact_acks table + FirstContactAckRepo (kp_ref -> peer binding)"
```

---

### Task 10: Responder idempotent re-Ack wiring (crypto second-reviewer)

**Files:**
- Modify: `crates/core/src/daemon/inbound.rs` — record write in `welcome_join_persist`; re-Ack pre-check in `dispatch_welcome_bootstrap`.

**Interfaces:**
- Consumes: `FirstContactAckRepo` (Task 9), `parse_welcome_kp_hash`, `welcome_join_persist`'s in-scope `kp_ref`/`derived`/`expected_x25519`, `now_ms()` the way the surrounding code already obtains the clock.
- Produces: `dispatch_welcome_bootstrap` returns `Some(peer_identity)` for a matched re-Ack (no join), `None` for a `kp_ref`/`x25519` mismatch (rejected), unchanged behavior otherwise.

- [ ] **Step 1: Write the record on join.** In `welcome_join_persist`, inside the SAME transaction that runs `oi.mark_consumed_in_tx(tx, &kp_ref)?;` (`inbound.rs:584`), and ONLY when `expected_x25519` is `Some` (the first-contact/bootstrap path), add:

```rust
            // #93 / ADR 0011: bind this consumed invite to the joining peer so a
            // later re-sent Welcome (lost-Ack) can be re-Acked without re-joining.
            if let Some(x25519) = expected_x25519 {
                crate::storage::first_contact_acks::FirstContactAckRepo::new(&self.pool)
                    .insert_in_tx(tx, &kp_ref, x25519, &derived.0, now)?;
            }
```

(`derived: PublicKey` is in scope at `:513`; `derived.0` is `[u8;32]`. `now` is the same millisecond clock the tx already uses — reuse the existing local; if none, obtain it exactly as the surrounding code does, without introducing `unwrap`.)

- [ ] **Step 2: Write the failing test (re-Ack path).** Add a `#[cfg(test)]` test in `inbound.rs` (mirror the existing `dispatch_welcome_*` tests around `:1259`/`:1421` for setup — they build a real group + Welcome + PSK). The test drives `dispatch_welcome_bootstrap` twice with the SAME welcome bytes and SAME `expected_x25519`:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn resent_welcome_from_same_peer_reacks_without_rejoin() {
        // ... build inbound dispatch + a valid first-contact welcome + expected_x25519,
        //     exactly as dispatch_welcome_joins_group_and_emits_contact_updated does ...
        let first = inbound.dispatch_welcome_bootstrap(&welcome, &expected_x25519, Some(&h));
        assert!(first.is_some(), "first join succeeds");
        let derived = first.unwrap();
        // Second, identical welcome from the same authenticated peer: the invite is
        // now consumed, so the OLD code returned None. New code re-Acks with the
        // same identity, WITHOUT re-joining.
        let second = inbound.dispatch_welcome_bootstrap(&welcome, &expected_x25519, Some(&h));
        assert_eq!(second, Some(derived), "re-sent welcome is re-Acked with stored identity");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resent_welcome_from_different_peer_is_rejected() {
        // ... same first join ...
        let mut other = expected_x25519;
        other[0] ^= 0xFF; // a different authenticated peer replaying the same welcome
        let replay = inbound.dispatch_welcome_bootstrap(&welcome, &other, Some(&h));
        assert_eq!(replay, None, "kp_ref match but different peer_x25519 is rejected");
    }
```

- [ ] **Step 3: Run — expect FAIL** (the second call currently returns `None`, so the first test fails). Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib resent_welcome 2>&1 | grep -E "test result:|FAILED"`

- [ ] **Step 4: Implement the pre-check** in `dispatch_welcome_bootstrap`, BEFORE the `welcome_join_persist` call:

```rust
        // #93 / ADR 0011: idempotent re-Ack. If we already joined a first
        // contact under this welcome's kp_ref, and the SAME authenticated Noise
        // static key re-presents it (lost-Ack retry), re-Ack with the stored
        // identity WITHOUT re-running the MLS join. A kp_ref match with a
        // different peer_x25519 is a replay by another peer — reject.
        if let Ok(kp_ref) = crate::daemon::inbound::parse_welcome_kp_hash(welcome) {
            match crate::storage::first_contact_acks::FirstContactAckRepo::new(&self.pool)
                .lookup(&kp_ref)
            {
                Ok(Some(rec)) => {
                    if &rec.peer_x25519 == expected_x25519 {
                        return Some(PublicKey(rec.peer_identity));
                    }
                    tracing::warn!("inbound: welcome kp_ref reused by a different peer — rejected");
                    return None;
                }
                Ok(None) => {} // fall through to the normal first-contact join
                Err(e) => {
                    tracing::warn!(err = %e, "inbound: first_contact_acks lookup failed");
                    // fall through: a lookup error must not block a legitimate first join
                }
            }
        }
```

(Use the actual path/visibility of `parse_welcome_kp_hash` — it is called at `inbound.rs:467`; call it the same way. `PublicKey` is already imported in this file.)

- [ ] **Step 5: Run — expect PASS**, and confirm no regression in the existing welcome tests:

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib inbound && cargo test -p skattr-core --lib dispatch_welcome`
Expected: PASS including the two new tests and the pre-existing `dispatch_welcome_*` tests.

- [ ] **Step 6: Lint + commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings
git add crates/core/src/daemon/inbound.rs
git commit -m "feat(#93): responder idempotent re-Ack for a re-sent first-contact Welcome (ADR 0011)"
```

---

### Task 11: Retargeted guardrail — drop the first Ack, prove recovery (crypto/assembly reviewer)

**Files:**
- Create: `crates/tests/src/first_contact_ack_dropped.rs`
- Modify: `crates/tests/src/lib.rs` (register the module)
- Modify (reuse from the saved reference `plumbing.diff`): `crates/core/src/daemon/state.rs` (`run_loopback_with_transport`), `crates/core/src/lib.rs` (`test_exports` re-exports), `crates/core/src/transport/transport.rs` (any wrapper-support export the reference added).

**Interfaces:**
- Consumes: the real `run_with_transport` assembly via `run_loopback_with_transport`; `PendingWelcomeRepo::is_pending`; the responder re-Ack (Task 10).

- [ ] **Step 1: Re-apply the reusable harness plumbing.** Read `<scratchpad>/task7-harness-reference/plumbing.diff` and re-apply the NON-test parts (`run_loopback_with_transport` in `state.rs`, the `test_exports` re-exports in `lib.rs`, any `transport.rs` support) so a test can drive the real assembly over a wrapped loopback transport. Do not re-introduce the old Welcome-dropping test.

- [ ] **Step 2: Write the failing test with an Ack-dropping fault seam.** Create `first_contact_ack_dropped.rs` (GPLv3 header). Adapt the fault-injecting `Transport` wrapper from `<scratchpad>/task7-harness-reference/first_contact_welcome_dropped.rs`, but change the drop target: **drop exactly the first RESPONDER→INVITEE post-handshake data frame** (that frame is the responder's `Frame::Ack` for the Welcome — accept.rs sends it immediately after a successful join). Parse the `length:u32 BE | type:u8 | payload` framing (length covers type+payload; cap `MAX_FRAME_SIZE = 16 MiB`); relay the two plaintext handshake frames, then swallow the first data frame in the responder→invitee direction once (global one-shot), then relay the rest. Assert, through the real assembly, on OBSERVABLE STATE:

```rust
// Shape — implement against run_loopback_with_transport:
// 1. Two daemons over the drop-first-Ack transport.
// 2. Invitee add_contact(invite). Responder joins; its Ack is dropped once.
// 3. After the drop, poll (bounded, up to ~30s) and assert the invitee is STILL
//    pending: PendingWelcomeRepo::is_pending(&responder_id) == true, and
//    send_message(invitee -> responder) returns the "not connected yet" error
//    (NO app frame emitted while pending).
// 4. The sweeper re-sends the Welcome; the responder RE-ACKS (Task 10).
// 5. Poll (bounded) until first contact completes: is_pending == false on the
//    invitee, both daemons report the contact group_state == active, and a text
//    message round-trips BOTH directions.
```

Name it `first_contact_recovers_after_dropped_ack`. Use bounded condition-polling (loop with a timeout), never a fixed `sleep` — mirror how `loopback_harness.rs`/existing guardrails wait for conditions.

- [ ] **Step 3: Run — expect PASS** against the full Task 8–10 implementation. Run: `. "$HOME/.cargo/env" && cargo test -p skattr-tests first_contact_recovers_after_dropped_ack -- --nocapture 2>&1 | grep -E "test result:|FAILED|pending|active"`

- [ ] **Step 4: Bug-catch check (mandatory).** Temporarily revert Task 10's pre-check (make `dispatch_welcome_bootstrap` skip the re-Ack lookup) and confirm this test **FAILS** — the invitee never clears pending because the responder rejects the re-sent Welcome with `unknown kp_ref`. Restore Task 10. Paste the observed failure into the commit body. A guardrail that passes against the unfixed responder is worthless.

- [ ] **Step 5: Confirm no regression** in the existing happy-path first-contact guardrail: `. "$HOME/.cargo/env" && cargo test -p skattr-tests first_contact`

- [ ] **Step 6: Lint + commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all && cargo clippy -p skattr-tests --all-targets --features test-harness -- -D warnings && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings
git add crates/tests/src/first_contact_ack_dropped.rs crates/tests/src/lib.rs crates/core/src/daemon/state.rs crates/core/src/lib.rs crates/core/src/transport/transport.rs
git commit -m "test(#93): non-loopback guardrail — first contact recovers after a dropped Ack"
```

---

### Task 12: Disclose lost-Welcome limitation + CHANGELOG + full gate

**Files:**
- Modify: `docs/THREAT_MODEL.md` (or the doc that holds the v1.0 limitations list — grep for the existing "first-contact Welcome is direct-only" limitation and add beside it)
- Modify: `CLAUDE.md` (the "Deferred / known-limitation status" section — add the lost-Welcome-over-new-circuit entry, tied to #90 Mode A)
- Modify: `CHANGELOG.md`

**Interfaces:** none (docs + changelog).

- [ ] **Step 1: Disclose the limitation.** In the threat model / limitations doc, add (next to the existing direct-only-Welcome limitation): *first-contact recovery self-heals a **lost Ack** (the responder re-Acks a re-sent Welcome, ADR 0011); it does **not** recover a **lost first Welcome** delivered over a since-replaced circuit — the re-sent Welcome carries the original connection's `h_transport` (ADR 0009) and cannot bind on a new connection. The invitee stays "Connecting…" and keeps retrying. Tracked with #90 Mode A; full recovery is v1.1.* Add the matching one-line entry to CLAUDE.md's Deferred-status section.

- [ ] **Step 2: CHANGELOG entry.** Add an entry referencing `#93` describing the responder idempotent re-Ack (ADR 0011) that fixes lost-Ack first-contact recovery, with the disclosed lost-Welcome limitation.

- [ ] **Step 3: Full local gate (authoritative).** Run and confirm all green:

```bash
. "$HOME/.cargo/env" \
  && cargo fmt --all -- --check \
  && cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings \
  && cargo test \
  && cargo deny check
```

- [ ] **Step 4: Commit**

```bash
git add docs/ CLAUDE.md CHANGELOG.md
git commit -m "docs(#93): disclose lost-Welcome-over-new-circuit limitation + CHANGELOG"
```

---

## After all tasks

Dispatch the final whole-branch review (opus) over the full branch range (`git merge-base master HEAD`..HEAD) — it covers Tasks 1–12 and must triage the Minor findings recorded in `.superpowers/sdd/progress.md` (Task 1 corrupt-row-skip warn, Task 3 `unwrap_or(0)` clock swallow). Then use superpowers:finishing-a-development-branch.
