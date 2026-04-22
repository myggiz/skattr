// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Integration test: two daemons share an MLS 2-member group, send one
//! application message, kill the transport mid-flight, rebuild the
//! transport, and assert the message is delivered exactly once.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use skattr_core::envelope::{Envelope, Kind, MessageId};
use skattr_core::identity::IdentityKey;
use skattr_core::test_exports::{
    handshake_initiator, handshake_responder, noise_public_of, receive, DeliveryHub, Group,
    GroupId, InboundDispatch, KeyPackage, KeyPackageRepo, KillSwitch, KillableStream, MessageRepo,
    MlsGroupRepo, MlsProvider, Outbox, Pool, ReceiveOutcome, SeenMessagesRepo,
};

/// Integration-test `InboundDispatch` that reloads the MLS group from
/// storage on each invocation, decrypts, persists the new ratchet
/// state, then runs `receiver::receive` for ts-window + dedup +
/// persist. Reloading each call is fine for a test that sends a
/// handful of messages; production would cache.
///
/// `Group` is not `Clone`-able (wraps OpenMLS state), so `Pair` stores
/// only the `GroupId` here and the actual `Group` lives in storage.
/// Follow `crates/tests/src/mls_pair.rs` for the `Group::load` / save
/// pattern.
struct MlsInboundDispatch {
    pool: Arc<Pool>,
    group_id: GroupId,
    expected_peer: skattr_core::identity::PublicKey,
}

impl InboundDispatch for MlsInboundDispatch {
    fn dispatch(
        &self,
        peer: skattr_core::identity::PublicKey,
        ciphertext: &[u8],
    ) -> Option<MessageId> {
        if peer != self.expected_peer {
            return None;
        }
        let repo = MlsGroupRepo::new(&self.pool);
        let mut g = Group::load(&self.group_id, &repo).ok().flatten()?;
        let envelope = g.decrypt(ciphertext).ok()?;
        let _ = g.save(&repo);

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let seen = SeenMessagesRepo::new(&self.pool);
        let msgs = MessageRepo::new(&self.pool);
        let mid = envelope.id;
        match receive(&peer, &self.group_id.0, envelope, now_ms, &seen, &msgs).ok()? {
            ReceiveOutcome::New(_) | ReceiveOutcome::Duplicate => Some(mid),
            ReceiveOutcome::Rejected(_) => None,
        }
    }
}

/// The test envelopes use `ts: now_ms()` so the receiver's ±1h
/// replay-window check passes.
fn ts_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn env(body: &str) -> Envelope {
    Envelope {
        v: 1,
        id: MessageId::generate(),
        ts: ts_now_ms(),
        reply_to: None,
        kind: Kind::Text { body: body.into() },
    }
}

/// Minimal pair bootstrap: two pools, two identities, two MLS groups
/// (Alice's solo + Bob added). Identities are wrapped in `Arc` because
/// `IdentityKey` is not `Clone` and tokio::spawn requires `'static`.
struct Pair {
    alice_pool: Arc<Pool>,
    bob_pool: Arc<Pool>,
    alice_id: Arc<IdentityKey>,
    bob_id: Arc<IdentityKey>,
    gid: GroupId,
}

fn setup_pair() -> Pair {
    let alice_pool = Arc::new(Pool::in_memory());
    let bob_pool = Arc::new(Pool::in_memory());
    let alice_id = Arc::new(IdentityKey::generate().unwrap());
    let bob_id = Arc::new(IdentityKey::generate().unwrap());

    let alice_provider = MlsProvider::new();
    let bob_provider = MlsProvider::new();
    let bob_kp_repo = KeyPackageRepo::new(&bob_pool);
    let alice_group_repo = MlsGroupRepo::new(&alice_pool);
    let bob_group_repo = MlsGroupRepo::new(&bob_pool);

    let psk = [0x5Au8; 32];
    let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &bob_kp_repo).unwrap();

    let mut alice_group = Group::create_solo(&alice_id, Some(&psk), alice_provider).unwrap();
    let (welcome, _commit) = alice_group.add_member(&bob_kp, Some(&psk)).unwrap();
    alice_group.save(&alice_group_repo).unwrap();

    let bob_group =
        Group::join_from_welcome(&bob_id, welcome.as_slice(), Some(&psk), bob_provider).unwrap();
    bob_group.save(&bob_group_repo).unwrap();

    let gid = alice_group.id().clone();
    Pair {
        alice_pool,
        bob_pool,
        alice_id,
        bob_id,
        gid,
    }
}

/// Perform a paired Noise handshake over a fresh `tokio::io::duplex` pair
/// wrapped in `KillableStream`. Returns the two `AuthenticatedConnection`s.
async fn paired_handshake(
    alice_id: Arc<IdentityKey>,
    bob_id: Arc<IdentityKey>,
    kill: KillSwitch,
) -> (
    skattr_core::test_exports::AuthenticatedConnection<KillableStream<tokio::io::DuplexStream>>,
    skattr_core::test_exports::AuthenticatedConnection<KillableStream<tokio::io::DuplexStream>>,
) {
    let (a_raw, b_raw) = tokio::io::duplex(64 * 1024);
    let alice_stream = KillableStream::new(a_raw, kill.clone());
    let bob_stream = KillableStream::new(b_raw, kill);
    let bob_static = noise_public_of(&bob_id);
    let responder =
        tokio::spawn(async move { handshake_responder(bob_stream, &bob_id, None).await });
    let (alice_conn, _) = handshake_initiator(alice_stream, &alice_id, &bob_static, None)
        .await
        .unwrap();
    let (bob_conn, _) = responder.await.unwrap().unwrap();
    (alice_conn, bob_conn)
}

#[tokio::test]
async fn kill_mid_message_redelivers_exactly_once() {
    let pair = setup_pair();

    // Build per-side InboundDispatch. Each side's Group lives in
    // storage (saved by setup_pair); the dispatcher reloads on demand.
    let alice_dispatch: Arc<dyn InboundDispatch> = Arc::new(MlsInboundDispatch {
        pool: pair.alice_pool.clone(),
        group_id: pair.gid.clone(),
        expected_peer: pair.bob_id.public(),
    });
    let bob_dispatch: Arc<dyn InboundDispatch> = Arc::new(MlsInboundDispatch {
        pool: pair.bob_pool.clone(),
        group_id: pair.gid.clone(),
        expected_peer: pair.alice_id.public(),
    });

    let alice_hub: DeliveryHub<KillableStream<tokio::io::DuplexStream>> =
        DeliveryHub::new_with_inbound(pair.alice_pool.clone(), alice_dispatch.clone());
    let bob_hub: DeliveryHub<KillableStream<tokio::io::DuplexStream>> =
        DeliveryHub::new_with_inbound(pair.bob_pool.clone(), bob_dispatch.clone());

    // --- Round 1: handshake + send + kill before ACK arrives ---
    let kill1 = KillSwitch::new();
    let (alice_conn, bob_conn) =
        paired_handshake(pair.alice_id.clone(), pair.bob_id.clone(), kill1.clone()).await;

    // Plumb both authenticated conns into their respective hubs.
    alice_hub.ingest(pair.bob_id.public(), alice_conn).await;
    bob_hub.ingest(pair.alice_id.public(), bob_conn).await;

    // Alice loads her group from storage, encrypts, and enqueues.
    let alice_group_repo = MlsGroupRepo::new(&pair.alice_pool);
    let mut alice_group = Group::load(&pair.gid, &alice_group_repo)
        .unwrap()
        .expect("alice group must exist");
    let message = env("hello bob");
    let mid = message.id;
    let ct = alice_group.encrypt(&message).unwrap();
    alice_group.save(&alice_group_repo).unwrap();

    let peer_bob = pair.bob_id.public();
    Outbox::new(&pair.alice_pool)
        .enqueue(&peer_bob, mid, &ct, 0)
        .unwrap();

    // Alice sends; kill the wire mid-flight.
    let ack_rx = alice_hub.send(peer_bob, mid, ct.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    kill1.kill();
    // Expect oneshot to resolve Err because the conn is dead.
    let _ = tokio::time::timeout(Duration::from_secs(1), ack_rx).await;

    // At this point Bob may or may not have observed the frame; we
    // don't assert either way — the point is that the next round must
    // produce exactly one stored message.
    drop(alice_hub);
    drop(bob_hub);

    // --- Round 2: rebuild hubs, rebuild transport, wait for retry ---
    let alice_hub2: DeliveryHub<KillableStream<tokio::io::DuplexStream>> =
        DeliveryHub::new_with_inbound(pair.alice_pool.clone(), alice_dispatch.clone());
    let bob_hub2: DeliveryHub<KillableStream<tokio::io::DuplexStream>> =
        DeliveryHub::new_with_inbound(pair.bob_pool.clone(), bob_dispatch.clone());

    let kill2 = KillSwitch::new();
    let (a_conn2, b_conn2) =
        paired_handshake(pair.alice_id.clone(), pair.bob_id.clone(), kill2).await;
    alice_hub2.ingest(pair.bob_id.public(), a_conn2).await;
    bob_hub2.ingest(pair.alice_id.public(), b_conn2).await;

    // Wait up to 3s for the retry tick to fire and deliver.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let seen = SeenMessagesRepo::new(&pair.bob_pool);
        if seen.contains(&pair.alice_id.public().0, &mid.0).unwrap() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("retry tick did not redeliver within 3s");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Assert: bob's messages has exactly one row for this sender.
    let msgs = MessageRepo::new(&pair.bob_pool);
    let recent = msgs.recent(&pair.gid.0, 10).unwrap();
    let from_alice: Vec<_> = recent
        .iter()
        .filter(|m| m.sender == pair.alice_id.public().0)
        .collect();
    assert_eq!(
        from_alice.len(),
        1,
        "exactly one row must land in bob's messages table, got {}",
        from_alice.len()
    );

    // Alice's outbox must be empty.
    let ob = Outbox::new(&pair.alice_pool);
    assert!(
        ob.due(i64::MAX, 10).unwrap().is_empty(),
        "alice's outbox must be empty after ACK"
    );
}

#[tokio::test]
async fn kill_before_any_frame_sent_delivers_on_retry() {
    let pair = setup_pair();
    let bob_dispatch: Arc<dyn InboundDispatch> = Arc::new(MlsInboundDispatch {
        pool: pair.bob_pool.clone(),
        group_id: pair.gid.clone(),
        expected_peer: pair.alice_id.public(),
    });
    // Alice is outbound-only; no inbound dispatcher needed.
    let alice_hub: DeliveryHub<KillableStream<tokio::io::DuplexStream>> =
        DeliveryHub::new(pair.alice_pool.clone());

    // Alice loads her group from storage and encrypts the message.
    let alice_group_repo = MlsGroupRepo::new(&pair.alice_pool);
    let mut alice_group = Group::load(&pair.gid, &alice_group_repo)
        .unwrap()
        .expect("alice group must exist");
    let message = env("first wire never flies");
    let mid = message.id;
    let ct = alice_group.encrypt(&message).unwrap();
    alice_group.save(&alice_group_repo).unwrap();

    let peer_bob = pair.bob_id.public();

    // Enqueue before any connection exists.
    Outbox::new(&pair.alice_pool)
        .enqueue(&peer_bob, mid, &ct, 0)
        .unwrap();

    // Build a fresh, non-killed transport and ingest on both ends.
    let kill = KillSwitch::new();
    let (a_conn, b_conn) = paired_handshake(pair.alice_id.clone(), pair.bob_id.clone(), kill).await;

    let bob_hub: DeliveryHub<KillableStream<tokio::io::DuplexStream>> =
        DeliveryHub::new_with_inbound(pair.bob_pool.clone(), bob_dispatch);
    alice_hub.ingest(peer_bob, a_conn).await;
    bob_hub.ingest(pair.alice_id.public(), b_conn).await;

    // Let the retry tick pick up the row.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let ob = Outbox::new(&pair.alice_pool);
        if ob.due(i64::MAX, 10).unwrap().is_empty() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("retry tick did not deliver within 3s");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let msgs = MessageRepo::new(&pair.bob_pool);
    let recent = msgs.recent(&pair.gid.0, 10).unwrap();
    assert_eq!(recent.len(), 1);
}
