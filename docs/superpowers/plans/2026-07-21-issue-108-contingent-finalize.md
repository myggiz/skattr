# #108 — Contingent finalize via uniform pending-gate — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enforce "no outbound MLS application frame while `is_pending(peer)`" at every emitter — closing the one un-gated path (`send_card_to_contact`) so a never-joining peer receives zero `WrongGroupId` app-frames.

**Architecture:** Add a single shared predicate `is_peer_pending`; route `send_message`'s existing guard through it; add a top-of-function skip to `send_card_to_contact` (the only un-gated `Group::encrypt` site). No MLS state-machine change, no wire change.

**Tech Stack:** Rust (rusqlite 0.38, OpenMLS), tokio.

**Spec:** `docs/superpowers/specs/2026-07-21-issue-108-contingent-finalize-design.md`

## Global Constraints

- Branch: `108-contingent-finalize`. No wire/protocol change; **no ADR**. Crypto/protocol second-reviewer still required (governs outbound frame emission on MLS groups).
- Milestone v1.1. Local gate authoritative: `cargo fmt --all -- --check`, `cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings`, `cargo test`. Cargo not on PATH — prefix with `. "$HOME/.cargo/env" &&`.
- Rust: no `unwrap`/`expect` outside tests; errors are our `CoreError`/typed daemon errors; GPLv3 headers intact; model states as data; fail loudly only where a failure is real (the card skip is a normal transient, not a failure — log + return).
- **The predicate must be shared:** both emitter sites (`send_message`, `send_card_to_contact`) and the tests call the same `is_peer_pending` — no duplicated inline `is_pending` logic.
- The card skip for a pending peer is **best-effort and silent** (log at `debug!`, `return`) — matching `send_card_to_contact`'s existing skip-on-anomaly style. On a real storage `Err` from the predicate inside `send_card_to_contact`, `warn!` + `return` (must not send, must not abort a broadcast).

**Verified facts (do not re-litigate):**
- Only two production `Group::encrypt` sites in `dispatch.rs`: `send_message` (line ~618, guarded at ~594) and `send_card_to_contact` (line ~1362, un-guarded).
- `send_file` announces via `send_message` (`dispatch.rs:757 let sent = send_message(handle, contact, kind).await?;`) → already guarded.
- `send_card_to_contact` callers: `publish_self_card_update` (broadcast loop, dispatch.rs:1410) and `welcome_sweep::on_welcome_acked` (welcome_sweep.rs:88).
- `on_welcome_acked` calls `finalize_welcome_ack` (deletes the `pending_welcomes` row) BEFORE `send_card_to_contact` → `is_pending` is already false there, so gating the card never blocks the post-Ack self-card.
- `send_card_to_contact` persists the advanced ratchet via `group.save(&group_repo)` INLINE (before the async `hub.send`), so a peer's `MlsGroupRepo::get(gid)` `state_blob` changes iff a card was actually encrypted+sent. This is the deterministic test observable.

**Key signatures:**
- `PendingWelcomeRepo::new(&Pool).is_pending(&[u8;32]) -> Result<bool>`
- `PublicKey.0: [u8;32]`; `Pool` = `crate::storage::Pool`; `MlsGroupRepo::new(&Pool).get(&[u8]) -> Result<Option<Vec<u8>>>`
- `send_card_to_contact(handle: &Arc<DaemonHandle<S>>, card: &ContactCard, peer: PublicKey)` (returns `()`)
- Test harness (dispatch.rs test module): `test_handle()`, `test_handle_with_mailbox(factory)` (has migrations + `poller_ctrl` for `publish_self_card_update`), `execute_command(handle, Command)`, `read_self_card_version(&pool)`; existing broadcast test `rotate_onion_publishes_card_update_to_contacts` (dispatch.rs:4031); connected-group fixture pattern in `remove_contact_soft_deletes_connected` (#109).

---

### Task 1: Shared `is_peer_pending` predicate + gate `send_card_to_contact`

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (add helper; refactor `send_message` guard; gate `send_card_to_contact`) + its `#[cfg(test)]` module

**Interfaces — Produces:**
- `fn is_peer_pending(pool: &crate::storage::Pool, peer: &crate::identity::PublicKey) -> crate::error::Result<bool>`

- [ ] **Step 1: Write the failing test — card skipped for a pending peer**

In the dispatch.rs test module. Build a pending contact (its group exists, a `pending_welcomes` row present) and assert `send_card_to_contact` leaves the group's `state_blob` unchanged (no encrypt); build a connected contact (group present, NO pending row) and assert its blob CHANGES. Mirror the connected-group fixture from `remove_contact_soft_deletes_connected`.

```rust
#[tokio::test]
async fn send_card_to_contact_skips_pending_peer() {
    use crate::storage::{MlsGroupRepo, PendingWelcomeRepo};
    let handle = test_handle();
    handle.set_onion("card-skip-test.onion".to_string());
    let card = build_self_card(&handle).unwrap();

    // Pending contact: real linked group + a pending_welcomes row.
    let (pending_pk, pending_gid) = seed_connected_contact(&handle, 0xAA); // helper below
    PendingWelcomeRepo::new(&handle.pool)
        .insert_pending_for_test(&pending_pk.0); // helper below (inserts a pending_welcomes row)

    let before = MlsGroupRepo::new(&handle.pool).get(&pending_gid).unwrap();
    send_card_to_contact(&handle, &card, pending_pk).await;
    let after = MlsGroupRepo::new(&handle.pool).get(&pending_gid).unwrap();
    assert_eq!(before, after, "pending peer's group ratchet must NOT advance (card skipped)");

    // Connected contact: same setup, NO pending row → card must send (blob changes).
    let (conn_pk, conn_gid) = seed_connected_contact(&handle, 0xBB);
    let before_c = MlsGroupRepo::new(&handle.pool).get(&conn_gid).unwrap();
    send_card_to_contact(&handle, &card, conn_pk).await;
    let after_c = MlsGroupRepo::new(&handle.pool).get(&conn_gid).unwrap();
    assert_ne!(before_c, after_c, "connected peer must receive the card (ratchet advances)");
}
```

Add two test helpers in the test module (adapt to the real fixture code — mirror `remove_contact_soft_deletes_connected` for the group construction, and the `PendingWelcomeRepo` insert pattern used by the first-contact tests):
- `fn seed_connected_contact(handle, tag: u8) -> (PublicKey, Vec<u8>)` — creates a contact with a real 2-member MLS group via `Group::create_solo` + `add_member` (+ `set_active` to make it usable, as the connected fixtures do), `set_group_id`, and returns `(pubkey, group_id)`. No `pending_welcomes` row.
- `insert_pending_for_test(peer: &[u8;32])` — insert one `pending_welcomes` row for `peer` (use `PendingWelcomeRepo::insert_in_tx` inside a `pool.transaction`, matching how `add_contact` seeds it).

- [ ] **Step 2: Run it — verify it fails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core daemon::dispatch::tests::send_card_to_contact_skips_pending_peer`
Expected: FAIL — the pending peer's blob DOES change (card currently sent, gate absent).

- [ ] **Step 3: Add the shared predicate**

Add near the other free helpers in `dispatch.rs` (e.g. just above `send_message`):

```rust
/// A first-contact Welcome to `peer` is still unacked (#93/#108): the local
/// MLS group exists but MUST NOT emit outbound application frames yet. The
/// single source of truth for the pending-gate, shared by every emitter.
fn is_peer_pending(
    pool: &crate::storage::Pool,
    peer: &crate::identity::PublicKey,
) -> crate::error::Result<bool> {
    crate::storage::PendingWelcomeRepo::new(pool).is_pending(&peer.0)
}
```

- [ ] **Step 4: Route `send_message`'s guard through the shared predicate**

Replace the inline guard block (dispatch.rs ~591–599) with:

```rust
    if is_peer_pending(&handle.pool, &contact).map_err(map_err)? {
        return Err(IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            message: "not connected yet — waiting for them to join".into(),
        }));
    }
```

(Keep the surrounding comment about pending app frames. Behavior identical — same error, same message.)

- [ ] **Step 5: Gate `send_card_to_contact`**

At the very top of `send_card_to_contact` (before the `get_group_id` lookup, dispatch.rs ~1305):

```rust
    // #108: a group with an unacked first-contact Welcome must emit no app
    // frames. Skip silently — the post-Ack self-card (welcome_sweep) re-delivers.
    match is_peer_pending(&handle.pool, &peer) {
        Ok(true) => {
            tracing::debug!(
                target: "skattr::daemon::dispatch",
                "card-send: peer has an unacked Welcome; skipping"
            );
            return;
        }
        Ok(false) => {}
        Err(e) => {
            tracing::warn!(
                target: "skattr::daemon::dispatch",
                err = %e,
                "card-send: is_pending check failed; skipping"
            );
            return;
        }
    }
```

- [ ] **Step 6: Run it — verify it passes**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core daemon::dispatch::tests::send_card_to_contact_skips_pending_peer`
Expected: PASS.

- [ ] **Step 7: Lock the inherited guards — `send_message` regression + `send_file` blocked**

Add (or confirm an existing test covers) these; if an equivalent already exists, reference it in your report rather than duplicating:

```rust
#[tokio::test]
async fn send_message_to_pending_peer_still_rejected() {
    let handle = test_handle();
    let (pk, _gid) = seed_connected_contact(&handle, 0xCC);
    insert_pending_for_test_via(&handle, &pk.0); // wrapper around the pending insert
    let r = execute_command(handle, Command::SendMessage {
        contact: pk,
        kind: Kind::Text { body: "hi".into() },
    }).await;
    assert!(matches!(
        r,
        Err(IpcError::Daemon(DaemonErrorKind::InvalidArgument { ref message }))
            if message.contains("not connected yet")
    ), "pending send must be rejected, got {r:?}");
}

#[tokio::test]
async fn send_file_to_pending_peer_is_blocked() {
    // send_file announces via send_message → inherits the pending guard.
    // Assert SendFile to a pending contact surfaces the same "not connected yet"
    // rejection (or the daemon's SendFile error that wraps it) and writes no
    // outbox/attachment rows for that peer. Mirror an existing SendFile test's
    // setup for the file/manifest; the assertion is: pending → rejected, no frame.
}
```

For `send_file`, use the smallest real `SendFile` invocation an existing send_file test uses; the point is to prove the block, so a tiny temp file is fine. If `send_file` rejects before writing anything, assert the error; if it currently would proceed, that reveals a real gap — gate `send_file` too (before the announce) and note it in your report.

- [ ] **Step 8: Run the focused tests + full crate gate**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core daemon::dispatch::tests` then `cargo clippy -p skattr-core --lib --features test-harness -- -D warnings`
Expected: all green, clippy clean.

- [ ] **Step 9: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "feat(#108): gate all app-frame emitters on is_pending (shared predicate)"
```

---

### Task 2: Broadcast guardrail + post-Ack does-not-over-block

Prove the acceptance criterion at the broadcast level and that the gate releases exactly on Ack.

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` test module (broadcast guardrail; post-Ack test)
- Modify: `crates/core/src/delivery/welcome_sweep.rs` test module (ordering assertion), if not already covered

**Interfaces — Consumes:** `is_peer_pending` gate from Task 1; `publish_self_card_update` (via `Command::RotateOnion`); `seed_connected_contact` / pending-insert helpers from Task 1.

- [ ] **Step 1: Write the failing test — broadcast skips pending, cards connected**

```rust
#[tokio::test]
async fn card_broadcast_skips_pending_contact_only() {
    use crate::storage::{MlsGroupRepo, PendingWelcomeRepo};
    let (handle, _ctrl_rx) = test_handle_with_mailbox(Arc::new(UnreachableFactory));
    handle.set_onion("broadcast-test.onion".to_string());

    // One pending contact + one connected contact, both with real groups.
    let (pending_pk, pending_gid) = seed_connected_contact(&handle, 0xAA);
    insert_pending_for_test_via(&handle, &pending_pk.0);
    let (conn_pk, conn_gid) = seed_connected_contact(&handle, 0xBB);
    let _ = (pending_pk, conn_pk);

    let pend_before = MlsGroupRepo::new(&handle.pool).get(&pending_gid).unwrap();
    let conn_before = MlsGroupRepo::new(&handle.pool).get(&conn_gid).unwrap();

    // RotateOnion drives publish_self_card_update over all contacts.
    let res = execute_command(handle.clone(), Command::RotateOnion).await.unwrap();
    assert!(matches!(res, CommandResult::Ok));

    let pend_after = MlsGroupRepo::new(&handle.pool).get(&pending_gid).unwrap();
    let conn_after = MlsGroupRepo::new(&handle.pool).get(&conn_gid).unwrap();

    assert_eq!(pend_before, pend_after, "#108: pending contact gets NO card (no WrongGroupId source)");
    assert_ne!(conn_before, conn_after, "connected contact still receives the broadcast card");
}
```

Note: this reuses the `test_handle_with_mailbox` wiring that `rotate_onion_publishes_card_update_to_contacts` uses (it has the `poller_ctrl` channel `publish_self_card_update` needs). Confirm `seed_connected_contact` produces contacts that `ContactRepo::list()` returns (so the broadcast loop visits them).

- [ ] **Step 2: Run it — verify it passes (gate from Task 1 already in place)**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core daemon::dispatch::tests::card_broadcast_skips_pending_contact_only`
Expected: PASS (Task 1 added the gate). If it FAILS because both blobs change, the gate isn't reached from the broadcast path — investigate and report before proceeding.

To confirm this test is meaningful (bug-catch), temporarily revert the Task-1 gate locally and verify it FAILS (pending blob changes), then restore. Note this in your report.

- [ ] **Step 3: Post-Ack — the gate does not over-block**

```rust
#[tokio::test]
async fn card_sends_after_pending_cleared() {
    use crate::storage::{MlsGroupRepo, PendingWelcomeRepo};
    let handle = test_handle();
    handle.set_onion("post-ack-test.onion".to_string());
    let card = build_self_card(&handle).unwrap();

    let (pk, gid) = seed_connected_contact(&handle, 0xDD);
    insert_pending_for_test_via(&handle, &pk.0);

    // Simulate the Ack: delete the pending_welcomes row (what finalize_welcome_ack does).
    PendingWelcomeRepo::new(&handle.pool).delete(&pk.0).unwrap();

    let before = MlsGroupRepo::new(&handle.pool).get(&gid).unwrap();
    send_card_to_contact(&handle, &card, pk).await;
    let after = MlsGroupRepo::new(&handle.pool).get(&gid).unwrap();
    assert_ne!(before, after, "after Ack (pending row cleared) the card must send");
}
```

- [ ] **Step 4: Ordering assertion (welcome_sweep)**

In `crates/core/src/delivery/welcome_sweep.rs` test module, confirm/add a test that `on_welcome_acked` deletes the `pending_welcomes` row before it would send the self-card (so the Task-1 gate never blocks the post-Ack card). If an existing test already proves `finalize_welcome_ack` deletes the row (from #93), a focused assertion that `is_pending` is false immediately after `finalize_welcome_ack` suffices — reference the existing test in your report rather than duplicating. Do not build live transport here; a pure-DB assertion on the ordering is the intent.

- [ ] **Step 5: Run the new tests + full crate gate**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core daemon::dispatch::tests` and `cargo test -p skattr-core delivery::welcome_sweep`, then `cargo clippy -p skattr-core --lib --features test-harness -- -D warnings`
Expected: green, clippy clean.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs crates/core/src/delivery/welcome_sweep.rs
git commit -m "test(#108): guardrail — card broadcast skips pending, sends post-Ack"
```

---

### Final: whole-branch gate

- [ ] Run the full authoritative local gate:
  - `. "$HOME/.cargo/env" && cargo fmt --all -- --check`
  - `cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings`
  - `cargo test`
  - `cargo deny check`
- [ ] Open PR referencing `Closes #108`; crypto/protocol second-reviewer; babysit CodeRabbit.

## Self-review notes (coverage)

- Invariant enforced at every emitter → Task 1 (shared predicate; `send_card_to_contact` gate; `send_message` routed through it; `send_file` inherits + locked). ✅
- Skip (not defer) for pending peer → Task 1 Step 5 (silent `debug!` return). ✅
- Post-Ack self-card unaffected → Task 2 Steps 3–4. ✅
- Acceptance ("zero app frames / WrongGroupId to a never-joining peer") → Task 2 Step 1 broadcast guardrail (pending blob unchanged), bug-catch verified in Step 2. ✅
- `send_message` unchanged behavior → Task 1 Step 7 regression. ✅
- No wire/MLS-state change → production diff is a predicate + two guard insertions only. ✅
