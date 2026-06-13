# Phase 2.A — MLS Ratchet & Binding Integrity — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bind the MLS group to the authenticated Noise transcript at its genesis Commit (`h_transport`), make per-invite PSKs unique, serialize per-group ratchet operations, make invites single-use atomically, and tolerate an inbound Commit.

**Architecture:** Genesis `add_member` Commit carries TWO external PSKs — the invite PSK and `h_transport` — both keyed by the invite's `KeyPackageRef` (ADR 0009; this also fixes per-invite PSK uniqueness, T2-8). The invitee (committer) dials the inviter first to obtain `h_transport`; the inviter derives the same value from the inbound handshake and registers it before `join_from_welcome`. A `group_id`-keyed async mutex serializes ratchet ops; `add_contact`'s check-and-consume becomes one transaction.

**Tech Stack:** Rust, OpenMLS 0.8 (external PSK API: `PreSharedKeyId::external(id, nonce).store(provider, secret)` + `MlsGroup::propose_external_psk`), tokio, rusqlite. Tests use `Pool::in_memory()` + the in-process loopback harness; no Tor.

**Specs:** `docs/superpowers/specs/2026-06-13-phase-2a-mls-integrity-design.md`; `docs/adr/0009-h-transport-mls-binding.md`. Read both.

**Conventions (read once):**
- cargo NOT on PATH — prefix every cargo command with `. "$HOME/.cargo/env" && `.
- Tests/clippy use `--features test-harness`.
- NO `unwrap()`/`expect()` in non-test (`src/`) code — `?` + typed `CoreError`. Test code may `.unwrap()` with a `#[allow(clippy::unwrap_used, clippy::expect_used)]` on the tests module.
- No custom crypto. Use the existing `register_external_psk` / OpenMLS PSK API; only change the PSK *id derivation* and add a second PSK.
- Secrets (`h_transport`, PSKs) are `Zeroizing`/`[u8;32]`; never log them.
- Commit messages end with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

**Sequencing note:** the `h_transport` binding requires BOTH the committer (Task 3) and the joiner (Task 4) to register it; Tasks 1–3 keep `h_transport = None` so first contact stays green, and **Task 4 flips the binding on for both sides together**. Don't run the loopback first-contact guardrail expecting the binding before Task 4.

---

## File map

- `crates/core/src/mls/group.rs` — `psk_id` derivation; `create_solo`/`add_member`/`join_from_welcome` gain `h_transport` + `kp_ref`; two-PSK register/propose; inbound-Commit handling in `decrypt` (Task 6).
- `crates/core/src/mls/state_machine.rs` — a `can_receive`-style predicate (Task 6).
- `crates/core/src/delivery/dial.rs` — `OutboundDial::dial` return widens to include `h_transport` (Task 2).
- `crates/core/src/delivery/hub.rs` — `connect_and_ingest` helper (Task 2).
- `crates/core/src/daemon/accept.rs` — forward `outcome.h_transport` to the Welcome-bootstrap (Task 2/4).
- `crates/core/src/daemon/dispatch.rs` — `add_contact` dial-first + one-txn + pass `h_transport` (Tasks 3–4); per-group lock around `send_message` (Task 5).
- `crates/core/src/daemon/inbound.rs` — `dispatch_welcome_bootstrap`/`welcome_join_persist` register `h_transport`; per-group lock around `dispatch_for_group` (Tasks 4–5).
- `crates/core/src/daemon/handle.rs` — `group_id`-keyed mutex registry (Task 5).
- `crates/tests/src/first_contact_direct.rs` — extend to assert the binding (Task 7).

---

## Task 1: PSK id derivation + two-PSK MLS API (T2-8 + binding capability)

**Why:** Today the invite PSK is registered under a fixed `PreSharedKeyId::external(b"skattr-binding-v1", [0u8;32])` — constant across invites, so a second invite overwrites the first (T2-8). Replace it with a per-invite-unique id derived from the `KeyPackageRef`, and add support for a SECOND external PSK (`h_transport`) in the genesis Commit. Callers pass `h_transport: None` for now (binding activated in Task 4); the id-derivation change fixes T2-8 immediately.

**Files:**
- Modify: `crates/core/src/mls/group.rs`
- Test: `crates/core/src/mls/group.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Add the `psk_id` derivation helper**

In `group.rs`, replace the `PSK_ID_BYTES` const + `register_external_psk` with a derivation that keys both id and nonce on the 32-byte `kp_ref` and a label:

```rust
/// Derive a per-invite-unique external PSK id. `label` distinguishes the two
/// PSKs carried in one genesis Commit ("invite" vs "htransport"); `kp_ref`
/// (the invite's 32-byte KeyPackageRef) makes every invite's PSK ids unique so
/// registering one never overwrites another invite's (ADR 0009, fixes T2-8).
fn psk_id(label: &[u8], kp_ref: &[u8; 32]) -> openmls::prelude::PreSharedKeyId {
    let mut id = Vec::with_capacity(8 + label.len() + 32);
    id.extend_from_slice(b"skattr-");
    id.extend_from_slice(label);
    id.extend_from_slice(b"-v1");
    id.extend_from_slice(kp_ref);
    openmls::prelude::PreSharedKeyId::external(id, kp_ref.to_vec())
}

/// Register an external PSK secret under `psk_id(label, kp_ref)`.
fn register_psk(
    provider: &MlsProvider,
    label: &[u8],
    kp_ref: &[u8; 32],
    secret: &[u8; 32],
) -> Result<openmls::prelude::PreSharedKeyId> {
    let id = psk_id(label, kp_ref);
    id.store(provider.as_openmls(), secret)
        .map_err(|e| CoreError::from(MlsErrorKind::Other(format!("mls: psk register: {e:?}"))))?;
    Ok(id)
}
```

> **VERIFY** the real `PreSharedKeyId::external` arity + `.store` signature against the current `register_external_psk` (the explore confirms `PreSharedKeyId::external(id: Vec<u8>, nonce: Vec<u8>)` and `.store(provider.as_openmls(), secret)`). Keep the existing `MlsProvider::as_openmls()` accessor. Remove the old `PSK_ID_BYTES` const + `register_external_psk` (update their call sites in this same task).

- [ ] **Step 2: Thread `kp_ref` + `h_transport` through `create_solo`, `add_member`, `join_from_welcome`**

Change the three signatures so each takes the invite PSK (existing), the `kp_ref`, and an optional `h_transport`:

```rust
pub fn create_solo(
    identity: &IdentityKey,
    invite_psk: Option<(&[u8; 32], &[u8; 32])>,   // (kp_ref, secret)
    h_transport: Option<(&[u8; 32], &[u8; 32])>,  // (kp_ref, secret) — same kp_ref
    provider: MlsProvider,
) -> Result<Self> { /* ... register both via register_psk(label, kp_ref, secret) ... */ }

pub fn add_member(
    &mut self,
    invitee_kp: &KeyPackage,
    invite_psk: Option<(&[u8; 32], &[u8; 32])>,
    h_transport: Option<(&[u8; 32], &[u8; 32])>,
) -> Result<(WelcomeBytes, CommitBytes)> {
    // register + propose BOTH PSKs (when Some) before committing add_members:
    //   if let Some((kp_ref, sec)) = invite_psk { let id = register_psk(&self.provider, b"invite", kp_ref, sec)?; self.inner.propose_external_psk(self.provider.as_openmls(), &signer, id)?; }
    //   if let Some((kp_ref, sec)) = h_transport { let id = register_psk(&self.provider, b"htransport", kp_ref, sec)?; self.inner.propose_external_psk(self.provider.as_openmls(), &signer, id)?; }
    //   ... then the existing add_members + merge_pending_commit ...
}

pub fn join_from_welcome(
    identity: &IdentityKey,
    welcome_bytes: &[u8],
    invite_psk: Option<(&[u8; 32], &[u8; 32])>,
    h_transport: Option<(&[u8; 32], &[u8; 32])>,
    provider: MlsProvider,
) -> Result<Self> {
    // register BOTH PSKs (when Some) BEFORE StagedWelcome::new_from_welcome:
    //   if let Some((kp_ref, sec)) = invite_psk { register_psk(&provider, b"invite", kp_ref, sec)?; }
    //   if let Some((kp_ref, sec)) = h_transport { register_psk(&provider, b"htransport", kp_ref, sec)?; }
    //   ... existing StagedWelcome path ...
}
```

> **VERIFY** the exact `propose_external_psk(provider, &signer, psk_id)` call + the `add_members`/`merge_pending_commit` sequence already in `add_member` (the explore confirms these). The `(&[u8;32], &[u8;32])` = `(kp_ref, secret)` shape lets `register_psk` derive the id; pick whatever tuple/struct reads cleanly but keep kp_ref + secret together. `signer`/`own_public_key`/`load_signer` helpers are unchanged.

- [ ] **Step 3: Update existing callers to the new signatures (pass `h_transport: None`, supply `kp_ref`)**

The callers are `daemon/dispatch.rs::add_contact` (the committer — has `link`, so `kp_ref` = the invite KeyPackage's ref) and `daemon/inbound.rs::welcome_join_persist` (the joiner — `kp_ref` via `parse_welcome_kp_hash(welcome)`), plus the `group.rs`/integration tests that call these. For now pass `h_transport: None`; for the invite PSK pass `Some((&kp_ref, &psk))`.

> **VERIFY:** in `add_contact`, the invite KP is `link.body.key_package`; its ref is `crate::mls::key_package::key_package_ref(&invitee_kp)?` (returns `[u8;32]`). In `welcome_join_persist`, the joiner already computes `kp_ref` via `parse_welcome_kp_hash` (it looks up `outstanding_invites` by `kp_ref`) — reuse that. Update every test in `group.rs`/`crates/tests` that calls `create_solo`/`add_member`/`join_from_welcome` to the new arity.

- [ ] **Step 4: Write MLS unit tests — two-PSK round trip + uniqueness**

Add to `group.rs` tests:

```rust
#[test]
fn genesis_two_psk_commit_round_trips_and_binds() {
    // bob create_solo + add_member(alice_kp) with invite_psk + h_transport (both Some,
    // same kp_ref); alice join_from_welcome with the SAME invite_psk + h_transport.
    // Assert join succeeds and a round-trip message decrypts.
}

#[test]
fn wrong_h_transport_rejects_join() {
    // alice joins with a DIFFERENT h_transport secret -> join_from_welcome must Err.
}

#[test]
fn distinct_kp_refs_yield_distinct_psk_ids() {
    // psk_id(b"invite", ref_a) != psk_id(b"invite", ref_b); and invite vs htransport
    // labels differ for the same ref. (Construct two refs, assert the PreSharedKeyId
    // bytes differ — compare via the id's serialized/identity bytes.)
}
```

> **VERIFY** the test idioms against existing `group.rs` tests (how they build identities, `MlsProvider::new()`, `KeyPackage::generate`, encrypt/decrypt). For `wrong_h_transport_rejects_join`, the join must fail because OpenMLS can't resolve the htransport PSK the commit references. For `distinct_*`, compare the `PreSharedKeyId` via its public bytes accessor (or serialize it); if no easy equality, assert the derived id `Vec<u8>` inputs differ.

- [ ] **Step 5: Run + clippy**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib --features test-harness mls::group && cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings`
Expected: PASS, no warnings. (Other crates compile because Step 3 updated all callers.)

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/mls/group.rs crates/core/src/daemon/dispatch.rs crates/core/src/daemon/inbound.rs
git commit -m "feat(mls): per-invite-unique two-PSK genesis commit API (T2-8 + binding capability)

Derive external PSK ids from the invite KeyPackageRef (fixes per-invite PSK
overwrite, T2-8) and support a second external PSK (h_transport) in the genesis
add_member commit. Callers pass h_transport=None for now; activated in 2.A
Task 4. (ADR 0009)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 2: Plumb `h_transport` from the connection to the daemon (no behavior change)

**Why:** `h_transport` is dropped by the dialer (`_outcome`) and ignored by the accept loop. Surface it so Tasks 3–4 can register it. This task only widens signatures + adds a hub helper; it changes no behavior (callers don't use the value yet).

**Files:**
- Modify: `crates/core/src/delivery/dial.rs`, `crates/core/src/delivery/hub.rs`, `crates/core/src/daemon/accept.rs`, `crates/core/src/daemon/inbound.rs`

- [ ] **Step 1: Widen `OutboundDial::dial` to return `h_transport`**

In `dial.rs`, change the trait + impl so `dial` returns `(AuthenticatedConnection<S>, Zeroizing<[u8;32]>)` (the conn + a copy of `h_transport`). `TransportDial::dial` stops dropping `_outcome`: `let (conn, outcome) = handshake_initiator(...).await?; Ok((conn, outcome.h_transport))`.

> **VERIFY:** `HandshakeOutcome.h_transport: Zeroizing<[u8;32]>` (explore-confirmed). The existing actor-side caller of `dial` (the per-peer actor's `ensure_conn`) must be updated to ignore the new tuple element (`let (c, _h) = d.dial(peer).await?;`). The `OneShotDialer` test stub returns the new shape too.

- [ ] **Step 2: Add `DeliveryHub::connect_and_ingest`**

In `hub.rs`, add a method that dials a peer via the injected dialer, ingests the connection, and returns `h_transport` (for the genesis-commit binding):

```rust
/// Dial `peer`, ingest the resulting connection (so the actor reuses it for
/// the Welcome + messages), and return the connection's `h_transport` for the
/// caller to bind into the genesis MLS Commit. Errors if no dialer is wired or
/// the dial fails.
pub(crate) async fn connect_and_ingest(&self, peer: PublicKey) -> Result<zeroize::Zeroizing<[u8; 32]>> {
    let dialer = self.dialer.as_ref().ok_or_else(|| {
        CoreError::Delivery(crate::delivery::DeliveryErrorKind::Other("no dialer wired".into()))
    })?;
    let (conn, h_transport) = dialer.dial(peer).await?;
    self.ingest(peer, conn).await;
    Ok(h_transport)
}
```

> **VERIFY** the `dialer` field type + `ingest` signature in `hub.rs` (explore-confirmed `Option<Arc<dyn OutboundDial<S>>>` + `ingest(&self, peer, conn)`). Return type matches Step 1's `Zeroizing<[u8;32]>`.

- [ ] **Step 3: Forward `h_transport` from the accept loop into the Welcome-bootstrap signature**

In `accept.rs::run_accept_loop`, the `handshake_responder` already yields `outcome`. Add `outcome.h_transport` as a new argument to `dispatch_welcome_bootstrap`. In `inbound.rs`, widen `dispatch_welcome_bootstrap` + `welcome_join_persist` to ACCEPT an `h_transport: &[u8;32]` param but, for this task, pass it through to `join_from_welcome` as `None`-equivalent (i.e. do NOT register it yet — Task 4 flips that). The signature exists so Task 4 is a one-line flip.

> **VERIFY:** `dispatch_welcome_bootstrap(&self, welcome, expected_x25519)` (post-1C) → add `h_transport: &[u8;32]`. Keep registering only the invite PSK in `welcome_join_persist` for now (pass `h_transport: None` to `join_from_welcome`); just carry the param. This keeps first contact green.

- [ ] **Step 4: Build + delivery/accept/inbound tests + clippy**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib --features test-harness delivery:: daemon::accept daemon::inbound && cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings`
Expected: PASS (no behavior change), no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/delivery/dial.rs crates/core/src/delivery/hub.rs crates/core/src/daemon/accept.rs crates/core/src/daemon/inbound.rs
git commit -m "feat(delivery): surface h_transport from dial + accept (plumbing, no behavior change)

OutboundDial::dial returns h_transport; DeliveryHub::connect_and_ingest dials +
ingests + returns it; the accept loop forwards it into dispatch_welcome_bootstrap.
Values are carried but not yet registered (binding activated in 2.A Task 4). (ADR 0009)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 3: `add_contact` dial-first + single-use atomicity (T2-1)

**Why:** To bind the genesis Commit, the invitee must dial the inviter BEFORE building the group (to obtain `h_transport`). Reorder `add_contact` accordingly using `connect_and_ingest`, and wrap the check-and-consume + group/contact writes in ONE transaction (T2-1). This task does the reorder + atomicity but still passes `h_transport: None` to the commit (binding flipped on in Task 4) — so first contact stays green.

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (`add_contact`)
- Test: `crates/core/src/daemon/dispatch.rs` (tests)

- [ ] **Step 1: Reorder `add_contact` to dial-first + one transaction**

Restructure the post-1C `add_contact` so the order is: parse+verify invite → resolve `kp_ref` → `handle.hub.connect_and_ingest(inviter)` (dial + ingest, capture `h_transport`) → build the genesis group in-memory (`create_solo` + `add_member` with the invite PSK; `h_transport: None` for now) → **one `pool.transaction`** that (a) re-checks `is_consumed`, (b) persists group + contact + card + group_id link, (c) `mark_consumed` — all atomically → send the Welcome over the ingested connection → send the invitee self-card (1C-T3).

> **VERIFY (the hard part):** the persist steps currently use repo methods (`group.save`, `contact_repo.upsert`, `set_group_id`, `put_card`, `mark_consumed`). Inside one `pool.transaction(|tx| {...})` you need their `*_in_tx` variants (e.g. `Group::save_in_tx`, `OutstandingInviteRepo`/`KeyPackageRepo` `*_in_tx`, contact upsert/set_group_id/put_card in-tx). Confirm which `_in_tx` variants exist; if a repo lacks one, add a minimal `_in_tx` mirroring its non-tx body (do NOT change behavior). The `is_consumed` check + `mark_consumed` MUST be inside the same tx as the writes. The dial happens BEFORE the tx (it's async/network; the tx is sync). If the dial fails, return the error before any writes. The card `verify(now)` (1C-T2) stays before `put_card`.

- [ ] **Step 2: Write the atomicity test**

Add to `dispatch.rs` tests: drive `AddContact` of one invite twice concurrently (or sequentially re-submit) and assert exactly ONE group/contact results and the second attempt returns `InviteConsumed` (not a second group). Mirror the 1C `add_contact_persists_inviter_card_for_dialer` setup (self-invite via `CreateInvite` → `AddContact`).

> **VERIFY:** for the concurrency flavor, two `execute_command(AddContact{same url})` — the second must hit `InviteConsumed` (the check is now inside the consuming tx). If true concurrency is awkward in the test harness, a sequential re-submit + a simulated-crash assertion (the invite stays consumable XOR the group is fully written) is acceptable; the key assertion is "never two groups for one invite."

- [ ] **Step 3: Run dispatch tests + the loopback first-contact guardrail (still green) + clippy**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib --features test-harness daemon::dispatch && cargo test -p skattr-tests first_contact_invite_add_then_bidirectional_over_loopback && cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings`
Expected: PASS — first contact still works (dial-first reorder is behavior-equivalent with `h_transport: None`), atomicity test passes, no warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs crates/core/src/storage/
git commit -m "feat(dispatch): add_contact dial-first + single-use atomicity (T2-1)

Dial the inviter first (connect_and_ingest, capturing h_transport for the
binding) before building the genesis group, and wrap is_consumed + group/contact
writes + mark_consumed in one pool.transaction so an invite can never create two
groups. h_transport not yet bound (Task 4). (ADR 0009)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 4: Activate the `h_transport` binding (T1-1) — committer + joiner together

**Why:** Flip the binding on. The committer (`add_contact`) and the joiner (`welcome_join_persist`) must register `h_transport` together, or first contact breaks. This is the one task that activates T1-1 end-to-end.

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (`add_contact`), `crates/core/src/daemon/inbound.rs` (`welcome_join_persist`)
- Test: `crates/tests/src/` (an end-to-end binding test) or `daemon::inbound` tests

- [ ] **Step 1: Committer passes the real `h_transport`**

In `add_contact`, pass the `h_transport` captured from `connect_and_ingest` (Task 3) into `create_solo`/`add_member` as `Some((&kp_ref, &h_transport))` (it was `None` in Task 3).

- [ ] **Step 2: Joiner registers the real `h_transport`**

In `inbound.rs::welcome_join_persist`, pass the `h_transport` param (threaded in Task 2) into `join_from_welcome` as `Some((&kp_ref, &h_transport))` (it was carried-but-unused). `kp_ref` is the same value already computed from `parse_welcome_kp_hash`.

> **VERIFY:** the inviter's `h_transport` comes from the SAME Noise session as the invitee's (the invitee dialed the inviter; the inviter is the responder). Confirm the accept loop's `outcome.h_transport` (forwarded in Task 2) is the value reaching `welcome_join_persist`. Both ends MUST derive identical bytes (transcript hash) — if the binding test fails with a PSK mismatch, the bug is a plumbing mismatch (wrong connection's h_transport), not the crypto.

- [ ] **Step 3: End-to-end binding test**

Add a test (in `daemon::inbound` tests or a focused integration test) that exercises the real two-side flow with matching `h_transport` and asserts the group becomes Active (join validates the binding PSK); and a negative case where the inviter registers a different `h_transport` → `join_from_welcome`/`dispatch_welcome_bootstrap` rejects. (The pure-MLS round-trip is already in Task 1; this asserts the daemon wiring passes the matching value through.)

- [ ] **Step 4: Run + the loopback guardrail (binding now live) + clippy**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib --features test-harness daemon:: mls:: && cargo test -p skattr-tests first_contact_invite_add_then_bidirectional_over_loopback && cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings`
Expected: PASS — first contact works WITH the binding active (both sides register the same `h_transport`); no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs crates/core/src/daemon/inbound.rs crates/tests/
git commit -m "feat(mls): activate h_transport binding on the genesis commit (T1-1)

Committer (add_contact) injects the dialed connection's h_transport as a second
external PSK; the inviter registers the same value (same Noise session) before
join_from_welcome. The genesis group is now bound to the authenticated transport
transcript. (ADR 0009)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 5: Per-group send lock (T1-3)

**Why:** Concurrent ops on a group load the same snapshot and encrypt at the same ratchet generation → undecryptable messages. Serialize per group.

**Files:**
- Modify: `crates/core/src/daemon/handle.rs` (registry), `crates/core/src/daemon/dispatch.rs` (`send_message`, card-send), `crates/core/src/daemon/inbound.rs` (`dispatch_for_group`)
- Test: `crates/core/src/daemon/dispatch.rs` (concurrency test)

- [ ] **Step 1: Add a `group_id`-keyed mutex registry to `DaemonHandle`**

Add a field + accessor:
```rust
// in DaemonHandle<S>
group_locks: Arc<tokio::sync::Mutex<std::collections::HashMap<[u8; 32], Arc<tokio::sync::Mutex<()>>>>>,
```
```rust
/// Per-group serialization lock: the load→encrypt/decrypt→save critical
/// section for a given group must hold this so concurrent ops can't encrypt at
/// the same ratchet generation.
pub(crate) async fn group_lock(&self, group_id: &[u8; 32]) -> Arc<tokio::sync::Mutex<()>> {
    let mut map = self.group_locks.lock().await;
    map.entry(*group_id).or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))).clone()
}
```

> **VERIFY** `DaemonHandle`'s constructors (`new_with_mailbox` etc.) — initialize `group_locks` to an empty map there + in `clone_for_dispatch` (it must be the SAME `Arc` across clones so send + receive share locks). Confirm the executor (`clone_for_dispatch`) shares the registry `Arc`.

- [ ] **Step 2: Hold the lock around the critical section in send + receive**

In `dispatch.rs::send_message`: `let lock = handle.group_lock(&group_id).await; let _guard = lock.lock().await;` BEFORE the `group.load → encrypt → pool.transaction(save+insert+outbox)` block; drop after. Same in `inbound.rs::dispatch_for_group` around its load→decrypt→save, and in the card-send (`send_card_to_contact`) + `welcome_join_persist` group-touching sections.

> **VERIFY:** acquire `group_lock` BEFORE `Group::load` and hold across the save. The `pool.transaction` stays inside. `group_id` is the `[u8;32]` already in scope at each site (or derive from the `GroupId`). Avoid deadlock: never hold two different group locks at once (each site touches one group).

- [ ] **Step 3: Concurrency test**

Add to `dispatch.rs` tests: build one 2-member group; fire N (e.g. 8) concurrent `send_message` for the same contact via `tokio::join!`/`JoinSet`; assert all N ciphertexts persist AND all decrypt on the receiver side in some order (no generation collision / no decrypt failure).

> **VERIFY:** without the lock this should be reproducibly racy; with it, deterministic. Use the real `send_message` path + a receiver `Group` (or `dispatch_for_group`) to decrypt all N. If exercising true concurrency is hard at the dispatch layer, drive it at the `Group` + `group_lock` level directly, but prefer the real `send_message`.

- [ ] **Step 4: Run + clippy**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib --features test-harness daemon:: && cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/handle.rs crates/core/src/daemon/dispatch.rs crates/core/src/daemon/inbound.rs
git commit -m "fix(daemon): per-group send lock serializes ratchet ops (T1-3)

A group_id-keyed async mutex (shared across send + receive via DaemonHandle)
held around load->encrypt/decrypt->save eliminates the concurrent-generation
race that produced undecryptable messages.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 6: Inbound-Commit handling (T2-2, defensive)

**Why:** `Group::decrypt` errors on an inbound `StagedCommitMessage` and gates reads on `can_send()`. No PCS in v1.0, so this is defensive — but cheap and audit-flagged: tolerate an inbound Commit instead of stalling delivery.

**Files:**
- Modify: `crates/core/src/mls/group.rs` (`decrypt`), `crates/core/src/mls/state_machine.rs`
- Test: `crates/core/src/mls/group.rs` (tests)

- [ ] **Step 1: Add a `can_receive` predicate**

In `state_machine.rs`, add a `can_receive(&self) -> bool` that allows processing inbound messages in the states where receiving is valid (at least `Active`), distinct from `can_send`. Gate `decrypt`'s entry on `can_receive()` instead of `can_send()`.

- [ ] **Step 2: Merge an inbound Commit instead of erroring**

In `decrypt`, change the `ProcessedMessageContent::StagedCommitMessage(staged)` arm: instead of returning an error, `self.inner.merge_staged_commit(self.provider.as_openmls(), *staged)` (advance the epoch), update `self.state` to the new epoch, persist nothing here (the caller saves), and signal "no application payload" to the caller. Since `decrypt` returns `Result<Envelope>`, change its return to `Result<Option<Envelope>>` (None for a merged Commit) — OR add a sibling `decrypt_or_commit` — pick the lower-churn shape and update `dispatch_for_group` to skip persistence when there's no Envelope.

> **VERIFY** the real `merge_staged_commit` signature (explore-confirmed it's used in `process_incoming_commit`) and `ProcessedMessageContent` variants. Changing `decrypt`'s return type ripples to `dispatch_for_group`/`dispatch_mailbox`/tests — update them (a merged Commit → no message row, no event, ACK-as-processed). If `process_incoming_commit` already does the merge, route `StagedCommitMessage` to it rather than duplicating. Keep it minimal: the goal is "don't error + don't stall," not full PCS support.

- [ ] **Step 3: Test**

Add a `group.rs` test: construct an inbound Commit (e.g. via a self-update/`advance_epoch` on the peer side, or a crafted staged commit) and assert `decrypt` merges it (epoch advances, returns None/no-payload) instead of erroring.

> **VERIFY:** if constructing a real inbound Commit is hard without PCS, use `advance_epoch` on one side to produce a Commit and feed it to the other side's `decrypt`; assert no error + epoch advance. If `advance_epoch` isn't easily exercisable, a focused test that the `StagedCommitMessage` arm merges (not errors) is sufficient.

- [ ] **Step 4: Run + clippy**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib --features test-harness mls:: daemon::inbound && cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/mls/group.rs crates/core/src/mls/state_machine.rs crates/core/src/daemon/inbound.rs
git commit -m "fix(mls): tolerate an inbound Commit instead of erroring (T2-2)

Split can_receive from can_send; merge an inbound StagedCommitMessage (advance
epoch) instead of erroring, so a PCS/epoch-advance Commit doesn't stall delivery.
Defensive: no PCS in v1.0, but the layer is now correct.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 7: Guardrail extension + final verification

**Why:** Prove the binding is present end-to-end and nothing regressed.

**Files:**
- Modify: `crates/tests/src/first_contact_direct.rs`

- [ ] **Step 1: Assert the binding in the first-contact guardrail**

Extend `first_contact_invite_add_then_bidirectional_over_loopback` (or add a sibling) so that, after first contact completes, it asserts the established group carries the `h_transport` binding PSK. If there's no easy public way to inspect the group's PSK proposals from a test, instead assert the binding indirectly: a NEGATIVE test where a tampered/relayed connection (different `h_transport`) causes the Welcome to be rejected — proving the binding is load-bearing. Prefer whichever is cleanly observable; document the choice in a comment.

> **VERIFY:** the loopback transport gives both daemons the same Noise session per connection, so the honest path's `h_transport` matches and first contact succeeds. A true relay/mismatch is hard to stage over loopback; the pure-MLS mismatch test (Task 1/4) already proves rejection, so here it's acceptable to assert the positive path still works end-to-end + reference the unit tests for the mismatch. Don't weaken any existing assertion.

- [ ] **Step 2: Final full gates**

Run:
```bash
. "$HOME/.cargo/env" && \
cargo fmt --all -- --check && \
cargo test -p skattr-core --features test-harness && \
cargo test -p skattr-tests && \
cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings && \
cargo build -p skattr-cli
```
Expected: all green; CLI builds.

- [ ] **Step 3: Commit**

```bash
git add crates/tests/src/first_contact_direct.rs
git commit -m "test(2.A): assert h_transport binding in the first-contact guardrail

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Spec coverage (self-review)

| Spec / ADR item | Covered by | Notes |
|---|---|---|
| T2-8 per-invite PSK uniqueness | Task 1 | kp_ref-derived ids; shipped first. |
| T1-1 h_transport binding (capability) | Task 1 | two-PSK API + MLS round-trip test. |
| h_transport plumbing | Task 2 | dial returns it; connect_and_ingest; accept forwards. |
| T2-1 invite single-use atomicity | Task 3 | dial-first + one-txn check-and-consume. |
| T1-1 binding activation (end-to-end) | Task 4 | committer + joiner register together. |
| T1-3 per-group send lock | Task 5 | group_id-keyed mutex; concurrency test. |
| T2-2 inbound-Commit handling | Task 6 | can_receive + merge StagedCommit (defensive). |
| Guardrail + binding assertion | Task 7 | 1C guardrail extended; full gates. |

The riskiest tasks are **1** (two-PSK MLS API), **3** (`add_contact` dial-first + one-txn — reshapes the 1C path), and **4** (binding activation — committer+joiner must match); each carries explicit VERIFY notes. Tasks 1–3 keep `h_transport=None` so first contact stays green until Task 4 flips the binding on for both sides together.
