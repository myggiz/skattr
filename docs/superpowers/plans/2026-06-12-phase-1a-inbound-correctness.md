# Phase 1A — Inbound Correctness & Crash Fixes — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop an IPC-reachable daemon crash, and make messages fetched from a mailbox actually decrypt, persist, and surface (instead of being deleted from the server and silently discarded).

**Architecture:** Three changes in `crates/core`, each independently committable. (1) `send_message` rejects the internal `ContactCardUpdate` kind before it reaches the storage layer's `unreachable!()`. (2) A new `InboundDispatch::dispatch_mailbox` method decrypts a mailbox-fetched ciphertext by trial-decrypting against each known 2-member group, attributes the real sender, and persists via the existing transactional path. (3) The mailbox poll actor fetches, dispatches via `dispatch_mailbox`, and deletes from the server **only** the deposits that dispatched successfully — leaving undispatched ones for the next poll.

**Tech Stack:** Rust, tokio, OpenMLS, rusqlite, the existing `skattr-core` crate. Tests use `Pool::in_memory()` and the in-process mailbox harness; no Tor required.

**Scope note:** This is Plan 1A of Phase 1. Plan 1B (direct P2P transport wiring + the `Daemon::run` regression guardrail) is a separate plan that follows this one.

---

## File map

- `crates/core/src/daemon/dispatch.rs` — `send_message`: add kind validation (Task 1).
- `crates/core/src/delivery/peer.rs` — `InboundDispatch` trait: add `dispatch_mailbox` default method (Task 2).
- `crates/core/src/daemon/inbound.rs` — `DaemonInbound`: implement `dispatch_mailbox` (Task 2).
- `crates/core/src/mailbox/poll.rs` — `actor_loop`: replace the `run_one_poll_tick` + self-pubkey dispatch block with fetch → dispatch_mailbox → delete-dispatched (Task 3).
- `crates/tests/src/mailbox_offline_delivery.rs` (or a sibling) — extend/add an integration test proving the poll actor persists a received message and deletes only it (Task 3 verification).

---

## Task 1: Reject internal `ContactCardUpdate` kind in `send_message` (T0-3)

**Why:** `Command::SendMessage { kind }` deserializes the full `envelope::Kind` enum, including `ContactCardUpdate`. `send_message` (`dispatch.rs:364`) passes it through to `MessageRepo::insert_in_tx`, which hits `unreachable!("ContactCardUpdate is intercepted in DaemonInbound; never reaches MessageRepo")` (`messages.rs:122`) — inside `pool.transaction`, so the process aborts in release (`panic="abort"`) or poisons the pool mutex in dev. Any local IPC client can trigger it.

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (function `send_message`, starts at line 364)
- Test: `crates/core/src/daemon/dispatch.rs` (the existing `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/core/src/daemon/dispatch.rs` (the module already defines `test_handle()` and imports `Command`, `CommandResult`, `IpcError`):

```rust
#[tokio::test]
async fn send_message_with_contact_card_update_kind_is_rejected_not_panic() {
    use crate::contact::card::{ContactCard, ContactCardBody};
    use crate::daemon::error_kind::DaemonErrorKind;
    use crate::envelope::Kind;
    use crate::identity::{PublicKey, Signature};

    let handle = test_handle();
    let peer = PublicKey([0x42; 32]);

    // Seed a contact + group so resolution gets past ContactNotFound and
    // reaches the kind path. (group_id can be any 32 bytes for this test;
    // the kind check must fire before MLS load.)
    {
        let repo = ContactRepo::new(&handle.pool);
        repo.upsert(&Contact {
            identity: peer,
            display_name: None,
            added_at: 0,
            card: None,
            muted: false,
        })
        .unwrap();
        repo.set_group_id(&peer, &[0x11u8; 32]).unwrap();
    }

    let card = ContactCard {
        body: ContactCardBody {
            identity: peer,
            onion: "x.onion".into(),
            mailboxes: vec![],
            version: 1,
            expires_at: 9_999_999_999,
        },
        signature: Signature([0u8; 64]),
    };
    let err = execute_command(
        handle,
        Command::SendMessage {
            contact: peer,
            kind: Kind::ContactCardUpdate { card: Box::new(card) },
        },
    )
    .await
    .unwrap_err();

    assert!(
        matches!(err, IpcError::Daemon(DaemonErrorKind::InvalidArgument { .. })),
        "ContactCardUpdate must be rejected as InvalidArgument, got {err:?}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails (panics today)**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib daemon::dispatch::tests::send_message_with_contact_card_update_kind_is_rejected_not_panic`
Expected: FAIL — the test panics with `unreachable!("ContactCardUpdate is intercepted ...")` (proving the crash path is reachable), not a clean assertion failure.

- [ ] **Step 3: Add the kind guard at the top of `send_message`**

In `crates/core/src/daemon/dispatch.rs`, inside `send_message`, immediately after the `use` block and before "1. Resolve group_id from contact." (around line 380), insert:

```rust
    // Reject internal, non-user-sendable kinds before any MLS work.
    // ContactCardUpdate is generated only by the daemon's own card-publish
    // path; if it reaches MessageRepo::insert_in_tx it hits an unreachable!()
    // inside the storage transaction (process abort in release).
    if matches!(kind, crate::envelope::Kind::ContactCardUpdate { .. }) {
        return Err(IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            message: "ContactCardUpdate is not a user-sendable message kind".into(),
        }));
    }
```

(`DaemonErrorKind` is already imported in `send_message` via `use crate::daemon::error_kind::DaemonErrorKind;`.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib daemon::dispatch::tests::send_message_with_contact_card_update_kind_is_rejected_not_panic`
Expected: PASS.

- [ ] **Step 5: Run clippy on the crate**

Run: `. "$HOME/.cargo/env" && cargo clippy -p skattr-core --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "fix(daemon): reject ContactCardUpdate in send_message instead of aborting

Command::SendMessage deserializes the full Kind enum; a client-supplied
ContactCardUpdate reached MessageRepo::insert_in_tx's unreachable!() inside
the pool transaction, aborting the daemon (panic=abort) or poisoning the
pool mutex. Reject it as InvalidArgument before any MLS work. (T0-3)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 2: Add `dispatch_mailbox` — decrypt mailbox-fetched ciphertext by trial-decrypt + attribute (T0-2 core)

**Why:** Mailbox-fetched deposits are not addressed to a known sender a priori. The current poll code passes the **self** pubkey to `InboundDispatch::dispatch`, which resolves the group via `ContactRepo::get_group_id(self)` — there is no such row, so it returns `Err` and the message is discarded (after already being deleted from the server). We add a dispatch entry point that finds the right group by trial-decrypt and attributes the actual sender, then reuses the existing transactional persist path (`dispatch_for_group`).

**Files:**
- Modify: `crates/core/src/delivery/peer.rs` (the `InboundDispatch` trait, around line 83-102 — add a defaulted method)
- Modify: `crates/core/src/daemon/inbound.rs` (`impl DaemonInbound` — add `dispatch_mailbox`; and `impl InboundDispatch for DaemonInbound` — override the trait method)
- Test: `crates/core/src/daemon/inbound.rs` (the existing `#[cfg(test)] mod tests`)

- [ ] **Step 1: Add the defaulted trait method**

In `crates/core/src/delivery/peer.rs`, inside `pub trait InboundDispatch` (after the existing `dispatch_welcome` default, before the closing brace around line 102), add:

```rust
    /// Decrypt and ingest a mailbox-fetched MLS ciphertext whose sender is
    /// not known a priori. Implementations trial-decrypt against each known
    /// group, attribute the matching peer, persist, and emit
    /// `Event::MessageReceived`. Returns the `MessageId` on success (so the
    /// caller can server-side delete the deposit) or `None` on failure (the
    /// caller must NOT delete — the deposit is retried on the next poll).
    ///
    /// Default impl returns `None` so existing impls compile unchanged.
    fn dispatch_mailbox(&self, _ciphertext: &[u8]) -> Option<crate::envelope::MessageId> {
        None
    }
```

- [ ] **Step 2: Write the failing test**

Add to the `tests` module in `crates/core/src/daemon/inbound.rs`. It builds a 2-member group (alice adds bob), links the contact so `contact_for_group` resolves, has bob encrypt a message, persists alice's group, then asserts `DaemonInbound::dispatch_mailbox` persists the row, returns the id, and emits `MessageReceived` attributed to bob:

```rust
#[tokio::test]
async fn dispatch_mailbox_trial_decrypts_attributes_sender_and_persists() {
    use crate::contact::Contact;
    use crate::daemon::events::Event;
    use crate::envelope::{Envelope, Kind, MessageId};
    use crate::mls::key_package::KeyPackage;
    use crate::storage::key_packages::KeyPackageRepo;
    use crate::storage::{ContactRepo, MessageRepo, MlsGroupRepo};

    let pool = Arc::new(Pool::in_memory());
    let (events_tx, mut rx) = broadcast::channel::<Event>(16);

    let alice_seed = crate::identity::Seed::generate().unwrap();
    let alice_id = crate::identity::IdentityKey::from_seed(&alice_seed).unwrap();
    let bob_seed = crate::identity::Seed::generate().unwrap();
    let bob_id = crate::identity::IdentityKey::from_seed(&bob_seed).unwrap();
    let bob_pk = bob_id.public();

    // Build the 2-member group (alice adds bob).
    let bob_provider = MlsProvider::new();
    let kp_repo = KeyPackageRepo::new(&pool);
    let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo).unwrap();
    let mut alice_group =
        crate::mls::Group::create_solo(&alice_id, None, MlsProvider::new()).unwrap();
    let (welcome, _commit) = alice_group.add_member(&bob_kp, None).unwrap();
    let group_id_bytes = alice_group.id().0.clone();
    let mut bob_group =
        crate::mls::Group::join_from_welcome(&bob_id, &welcome, None, bob_provider).unwrap();

    // Persist alice's group + link bob as the contact for this group so
    // contact_for_group can attribute the sender.
    alice_group.save(&MlsGroupRepo::new(&pool)).unwrap();
    let contacts = ContactRepo::new(&pool);
    contacts
        .upsert(&Contact {
            identity: bob_pk,
            display_name: Some("bob".into()),
            added_at: 0,
            card: None,
            muted: false,
        })
        .unwrap();
    contacts.set_group_id(&bob_pk, &group_id_bytes).unwrap();

    // Bob encrypts a message (ts within ±1h of now).
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let env = Envelope {
        v: 1,
        id: MessageId::generate(),
        ts: now_ms,
        reply_to: None,
        kind: Kind::Text { body: "from-mailbox".into() },
    };
    let expected_id = env.id;
    let ciphertext = bob_group.encrypt(&env).unwrap();

    let inbound = DaemonInbound::new(pool.clone(), events_tx.clone());
    let returned = inbound.dispatch_mailbox(&ciphertext);

    assert_eq!(returned, Some(expected_id), "must return the decrypted message id");

    // Event emitted, attributed to bob.
    match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await {
        Ok(Ok(Event::MessageReceived { contact, record })) => {
            assert_eq!(contact, bob_pk, "sender must be attributed to bob");
            assert!(matches!(&record.kind, Kind::Text { body } if body == "from-mailbox"));
        }
        other => panic!("expected MessageReceived, got {other:?}"),
    }

    // Persisted exactly once.
    let rows = MessageRepo::new(&pool).recent(&group_id_bytes, 10).unwrap();
    assert_eq!(rows.len(), 1);
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib daemon::inbound::tests::dispatch_mailbox_trial_decrypts_attributes_sender_and_persists`
Expected: FAIL — `assert_eq!(returned, Some(expected_id))` fails with `None` (default trait impl; `DaemonInbound` doesn't override yet).

- [ ] **Step 4: Implement `dispatch_mailbox` on `DaemonInbound`**

In `crates/core/src/daemon/inbound.rs`, add a method inside `impl DaemonInbound` (next to `dispatch_for_group`):

```rust
    /// Trial-decrypt a mailbox-fetched ciphertext against each known group.
    /// On the first group that decrypts, attribute the peer via
    /// `contact_for_group` and persist through the transactional
    /// `dispatch_for_group` path (which re-loads from disk, so the trial
    /// decrypt above — performed on an in-memory copy that is never saved —
    /// does not consume the on-disk message key).
    fn dispatch_mailbox_inner(&self, ciphertext: &[u8]) -> Option<MessageId> {
        let group_repo = MlsGroupRepo::new(&self.pool);
        let groups = group_repo.list().ok()?;
        for (gid_bytes, _epoch) in groups {
            let gid = GroupId(gid_bytes.clone());
            let mut g = match Group::load(&gid, &group_repo) {
                Ok(Some(g)) => g,
                _ => continue,
            };
            // Trial decrypt on the in-memory copy; do NOT save.
            if g.decrypt(ciphertext).is_err() {
                continue;
            }
            let gid_arr: [u8; 32] = match gid_bytes.as_slice().try_into() {
                Ok(a) => a,
                Err(_) => continue,
            };
            let peer = match ContactRepo::new(&self.pool).contact_for_group(&gid_arr) {
                Ok(Some(p)) => p,
                _ => continue,
            };
            // Re-load + decrypt + persist atomically against the on-disk state.
            return self.dispatch_for_group(peer, &gid_bytes, ciphertext).ok();
        }
        None
    }
```

Then add the trait override in `impl InboundDispatch for DaemonInbound` (next to `dispatch` / `dispatch_welcome`):

```rust
    fn dispatch_mailbox(&self, ciphertext: &[u8]) -> Option<MessageId> {
        match self.dispatch_mailbox_inner(ciphertext) {
            Some(mid) => Some(mid),
            None => {
                tracing::warn!("inbound: mailbox dispatch found no matching group; deposit retained");
                None
            }
        }
    }
```

(Imports `MlsGroupRepo`, `Group`, `GroupId`, `ContactRepo`, `MessageId` are already present at the top of `inbound.rs`.)

- [ ] **Step 5: Run the test to verify it passes**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib daemon::inbound::tests::dispatch_mailbox_trial_decrypts_attributes_sender_and_persists`
Expected: PASS.

- [ ] **Step 6: Run the full inbound + delivery test modules + clippy**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib daemon::inbound delivery::peer && cargo clippy -p skattr-core --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/delivery/peer.rs crates/core/src/daemon/inbound.rs
git commit -m "feat(inbound): dispatch_mailbox trial-decrypts and attributes mailbox messages

Mailbox-fetched deposits carry no a-priori sender; the old poll code passed
the self pubkey to dispatch(), which never resolved a group, so messages were
discarded after server-side deletion. dispatch_mailbox trial-decrypts against
each known group, attributes the real peer via contact_for_group, and persists
through the existing transactional path. (T0-2 core)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 3: Poll actor — fetch → dispatch_mailbox → delete only dispatched (T0-2 wiring)

**Why:** The actor currently calls `run_one_poll_tick`, which fetches **and deletes** every deposit, then dispatches with the self pubkey (which fails). We extract the success-path logic into a small `poll_dispatch_once` helper that fetches, dispatches each deposit via `dispatch_mailbox`, and deletes from the server **only** the deposits that dispatched successfully. The actor calls the helper. `run_one_poll_tick` is left unchanged (still used by the `RemoveMailbox` drain and `test_exports`).

**Files:**
- Modify: `crates/core/src/mailbox/poll.rs` — add `poll_dispatch_once`; call it from `actor_loop`'s success arm.
- Test: `crates/core/src/mailbox/poll.rs` — unit test in the existing `#[cfg(test)] mod tests` (self-contained: inline duplex mailbox server + a real `DaemonInbound` over an in-memory pool; mirrors `poll_tick_drives_full_challenge_fetch_delete_cycle`).

- [ ] **Step 1: Add the `poll_dispatch_once` helper**

In `crates/core/src/mailbox/poll.rs`, add (next to `run_one_poll_tick`):

```rust
/// One fetch → dispatch → delete-dispatched cycle for a recipient's own
/// mailbox. Fetches pending deposits, hands each ciphertext to the inbound
/// MLS pipeline via `dispatch_mailbox`, and server-side deletes ONLY the
/// deposits that persisted successfully. Undispatched deposits are left on
/// the server for the next poll (no silent loss on a transient failure).
///
/// Returns the number of deposits that were dispatched + deleted.
pub(crate) async fn poll_dispatch_once<S>(
    client: &mut crate::mailbox::client::MailboxClient<S>,
    signer: &crate::identity::IdentityKey,
    inbound: &dyn crate::delivery::peer::InboundDispatch,
) -> crate::error::Result<usize>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    let resp = client.fetch(signer).await?;
    if resp.deposits.is_empty() {
        return Ok(0);
    }
    let mut dispatched: Vec<[u8; 16]> = Vec::new();
    for dep in &resp.deposits {
        if inbound.dispatch_mailbox(&dep.ciphertext).is_some() {
            dispatched.push(dep.deposit_id);
        }
    }
    let count = dispatched.len();
    if !dispatched.is_empty() {
        client.delete(signer, dispatched).await?;
    }
    Ok(count)
}
```

- [ ] **Step 2: Call the helper from `actor_loop`'s success arm**

In `actor_loop`, replace the `match run_one_poll_tick(&mut client, &identity).await { Ok(resp) => { ... } ...}` success arm (lines ~431-461) — leave the `RateLimited` and other `Err` arms unchanged — so it calls the helper and only marks Active when something was dispatched:

```rust
        match poll_dispatch_once(&mut client, &identity, inbound.as_deref().unwrap_or(&NoopDispatch)).await {
            Ok(dispatched) => {
                consecutive_failures = 0;
                if dispatched > 0 {
                    active_until = Some(tokio::time::Instant::now() + ACTIVE_HOLD);
                }
                if !matches!(last_status, MailboxStatus::Reachable) {
                    let _ = MailboxRepo::new(&pool).mark_status(id, MailboxStatus::Reachable);
                    last_status = MailboxStatus::Reachable;
                    let _ = events.send(Event::MailboxStatusChanged {
                        mailbox_id: id,
                        status: MailboxStatus::Reachable,
                    });
                }
            }
            Err(CoreError::MailboxClient(MailboxClientErrorKind::RateLimited)) => {
```

(Keep the `RateLimited` arm and the trailing `Err(e) =>` arm exactly as today.) Remove the old `let self_pk = identity.public();` and the `for dep in &resp.deposits { let _ = disp.dispatch(self_pk, ...); }` loop — replaced by the helper.

Add a tiny no-op dispatcher near the top of `poll.rs` (used when `inbound` is `None`, e.g. in tests that don't decrypt):

```rust
/// Inbound dispatcher that ignores everything. Used when a poll actor has
/// no MLS pipeline wired (tests).
struct NoopDispatch;
impl crate::delivery::peer::InboundDispatch for NoopDispatch {
    fn dispatch(&self, _: crate::identity::PublicKey, _: &[u8]) -> Option<crate::envelope::MessageId> {
        None
    }
}
```

- [ ] **Step 3: Verify the crate compiles**

Run: `. "$HOME/.cargo/env" && cargo build -p skattr-core`
Expected: builds.

- [ ] **Step 4: Write the failing unit test**

Add to the `tests` module in `crates/core/src/mailbox/poll.rs`. It builds a 2-member group (alice adds bob), links bob as the contact, persists alice's group, has bob encrypt a message, runs an inline mailbox server that returns that ciphertext on Fetch and `DeleteOk` on Delete, and asserts `poll_dispatch_once` persists the row in alice's pool and reports 1 dispatched:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn poll_dispatch_once_persists_and_deletes_dispatched() {
        use crate::contact::Contact;
        use crate::daemon::inbound::DaemonInbound;
        use crate::envelope::{Envelope, Kind, MessageId};
        use crate::mailbox::client::MailboxClient;
        use crate::mailbox::codec::{MailboxFrame, MailboxFrameCodec};
        use crate::mailbox::protocol::{ChallengeNonce, DeleteOk, FetchResponse, PendingDeposit};
        use crate::mls::key_package::KeyPackage;
        use crate::storage::key_packages::KeyPackageRepo;
        use crate::storage::{ContactRepo, MessageRepo, MlsGroupRepo, Pool};
        use futures::{SinkExt, StreamExt};
        use std::sync::Arc;
        use tokio::io::duplex;
        use tokio_util::codec::Framed;

        let pool = Arc::new(Pool::in_memory());
        let alice = IdentityKey::generate().unwrap();
        let bob = IdentityKey::generate().unwrap();
        let bob_pk = bob.public();

        // Group: alice adds bob; alice persists; link bob as the contact.
        let bob_provider = crate::mls::provider::MlsProvider::new();
        let bob_kp = KeyPackage::generate(&bob, &bob_provider, &KeyPackageRepo::new(&pool)).unwrap();
        let mut alice_group =
            crate::mls::Group::create_solo(&alice, None, crate::mls::provider::MlsProvider::new())
                .unwrap();
        let (welcome, _commit) = alice_group.add_member(&bob_kp, None).unwrap();
        let gid = alice_group.id().0.clone();
        let mut bob_group =
            crate::mls::Group::join_from_welcome(&bob, &welcome, None, bob_provider).unwrap();
        alice_group.save(&MlsGroupRepo::new(&pool)).unwrap();
        let contacts = ContactRepo::new(&pool);
        contacts
            .upsert(&Contact { identity: bob_pk, display_name: None, added_at: 0, card: None, muted: false })
            .unwrap();
        contacts.set_group_id(&bob_pk, &gid).unwrap();

        // Bob encrypts a message (ts within ±1h).
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let env = Envelope { v: 1, id: MessageId::generate(), ts: now_ms, reply_to: None, kind: Kind::Text { body: "mbx".into() } };
        let ciphertext = bob_group.encrypt(&env).unwrap();

        // Inline mailbox server: Challenge→Nonce, Fetch→one deposit,
        // Challenge→Nonce, Delete→DeleteOk.
        let (a, b) = duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let mut framed = Framed::new(b, MailboxFrameCodec::new());
            let _ = framed.next().await; // Challenge (for fetch)
            framed.send(MailboxFrame::ChallengeNonce(ChallengeNonce { nonce: [1; 32], issued_at: 1 })).await.unwrap();
            let _ = framed.next().await; // Fetch
            framed.send(MailboxFrame::FetchResponse(FetchResponse {
                deposits: vec![PendingDeposit { deposit_id: [9; 16], ciphertext, received_at: 1 }],
            })).await.unwrap();
            let _ = framed.next().await; // Challenge (for delete)
            framed.send(MailboxFrame::ChallengeNonce(ChallengeNonce { nonce: [2; 32], issued_at: 2 })).await.unwrap();
            let _ = framed.next().await; // Delete
            framed.send(MailboxFrame::DeleteOk(DeleteOk { deleted: 1, not_found: 0 })).await.unwrap();
        });

        let (events_tx, _rx) = tokio::sync::broadcast::channel(16);
        let inbound = DaemonInbound::new(pool.clone(), events_tx);
        inbound.set_identity(Arc::new(IdentityKey::from_bytes(zeroize::Zeroizing::new(*alice.ed25519_seed()))));
        let mut client = MailboxClient::from_stream("a.onion".into(), a);

        let dispatched = poll_dispatch_once(&mut client, &alice, &inbound).await.unwrap();
        assert_eq!(dispatched, 1, "one deposit must be dispatched + deleted");

        let rows = MessageRepo::new(&pool).recent(&gid, 10).unwrap();
        assert_eq!(rows.len(), 1, "received message must persist in alice's pool");

        server.await.unwrap();
    }
```

> **Implementer note:** `IdentityKey::ed25519_seed()` and `from_bytes` are `pub(crate)` and reachable from this same-crate test; they reconstruct a second owned `IdentityKey` for `set_identity` because `IdentityKey` is not `Clone`. If a simpler `set_identity` input is available, use it. `DaemonInbound` is `pub(crate)` in `crate::daemon::inbound`, reachable here.

- [ ] **Step 5: Run the test to verify it fails, then passes**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib mailbox::poll::tests::poll_dispatch_once_persists_and_deletes_dispatched`
Expected: with Step 1-2 implemented, PASS. (To see it fail first, temporarily stub `dispatch_mailbox` to `None` — the row won't persist and `dispatched` will be 0.)

- [ ] **Step 6: Run the full poll module + clippy**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib mailbox::poll && cargo clippy -p skattr-core --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/mailbox/poll.rs
git commit -m "fix(mailbox): poll actor dispatches received messages and deletes only dispatched

The actor previously deleted every fetched deposit then dispatched with the
self pubkey (which never resolved a group), so received messages were lost.
Extract poll_dispatch_once: fetch, dispatch each deposit via dispatch_mailbox,
and delete only the deposits that persisted successfully; the rest are retried
next poll. (T0-2)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```


---

## Final verification

- [ ] **Run the full core + tests suites (non-ignored) and clippy/fmt**

Run:
```bash
. "$HOME/.cargo/env" && \
cargo test -p skattr-core && \
cargo test -p skattr-tests && \
cargo clippy --workspace --exclude skattr-ui --all-targets -- -D warnings && \
cargo fmt --all -- --check
```
Expected: all green.

- [ ] **Confirm the three behaviors by reading the diff**
  - `send_message` returns `InvalidArgument` for `ContactCardUpdate` (Task 1).
  - `DaemonInbound::dispatch_mailbox` decrypts, attributes, persists, emits (Task 2).
  - The poll actor deletes only successfully-dispatched deposits (Task 3).

---

## Spec coverage (self-review)

| Roadmap Phase 1 item | Covered by | Notes |
|---|---|---|
| T0-3 Validate `SendMessage` kinds | Task 1 | Phase-1 minimal: rejects the crashing `ContactCardUpdate`. Reject-or-inert decision for Reaction/Edit/Delete/Typing is deferred to Phase 4 per roadmap. |
| T0-2 Fix mailbox-poll inbound (decrypt-then-attribute, delete-after-dispatch) | Tasks 2 + 3 | Full. |
| T0-1 inbound accept loop / outbound dialer | — | **Plan 1B** (next plan). |
| Guardrail (`Daemon::run` end-to-end) | — | **Plan 1B** (needs the transport wiring to assert against). |

This plan deliberately excludes the direct-transport wiring (T0-1) and the
`Daemon::run` regression guardrail; both land in Plan 1B, which can assert a
full bidirectional round-trip once the dialer and accept loop exist.
