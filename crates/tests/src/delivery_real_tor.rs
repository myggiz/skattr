// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Integration test over real Arti: two daemons bootstrap, Bob publishes
//! an onion, Alice dials, both sides run Noise_XK + MLS, and Alice sends
//! five application messages through `DeliveryHub`. Asserts all five land
//! in Bob's `messages` table.
//!
//! `#[ignore]`-gated; run with:
//!
//! ```bash
//! cargo test -p skattr-tests --release -- --ignored
//! ```
//!
//! Mirrors `arti_echo.rs` in shape.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use skattr_core::envelope::{Envelope, Kind, MessageId};
use skattr_core::identity::{IdentityKey, Seed};
use skattr_core::test_exports::{
    handshake_initiator, handshake_responder, now_unix_seconds, DeliveryHub, Group, GroupId,
    InboundDispatch, KeyPackage, KeyPackageRepo, MessageRepo, MlsGroupRepo, MlsProvider,
    OnionListener, Outbox, Pool, ReceiveOutcome, SeenMessagesRepo, TorConfig, TorRuntime,
};

/// Create a tempdir with 0700 permissions — Arti 0.41 refuses to open
/// a `state_dir` that is group- or world-readable.
fn arti_friendly_tempdir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(tmp.path()).unwrap().permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(tmp.path(), perms).unwrap();
    }
    tmp
}

fn ts_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Integration-test `InboundDispatch` that reloads the MLS group from
/// storage on each call, decrypts the ciphertext, persists the new
/// ratchet state, then runs the replay-window + dedup + persist path.
///
/// An in-memory ciphertext-hash cache short-circuits duplicate deliveries
/// (spec §4.4 Case B) without re-entering the MLS state machine.
struct MlsInboundDispatch {
    pool: Arc<Pool>,
    group_id: GroupId,
    expected_peer: skattr_core::identity::PublicKey,
    seen_ciphertexts: std::sync::Mutex<std::collections::HashMap<[u8; 32], MessageId>>,
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

        // Stable hash of the ciphertext for replay detection.
        let hash = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(ciphertext);
            let out = h.finalize();
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&out);
            arr
        };

        // Fast path: already decrypted this ciphertext. ACK again so the
        // sender can clear its outbox row (spec §4.4 Case B).
        if let Some(cached_mid) = self
            .seen_ciphertexts
            .lock()
            .ok()
            .and_then(|g| g.get(&hash).copied())
        {
            return Some(cached_mid);
        }

        // Full path: reload group, decrypt, save, run receiver.
        let repo = MlsGroupRepo::new(&self.pool);
        let mut g = Group::load(&self.group_id, &repo).ok().flatten()?;
        let envelope = g.decrypt(ciphertext).ok().flatten()?;
        let mls_generation = g.epoch();
        let _ = g.save(&repo);
        let mid = envelope.id;

        // Cache before returning so subsequent identical ciphertexts
        // short-circuit above.
        if let Ok(mut set) = self.seen_ciphertexts.lock() {
            set.insert(hash, mid);
        }

        let now_ms = ts_now_ms();
        let ts_daemon_recv = now_unix_seconds();
        let seen = SeenMessagesRepo::new(&self.pool);
        let msgs = MessageRepo::new(&self.pool);
        let receive = skattr_core::test_exports::receive;
        match receive(
            &peer,
            &self.group_id.0,
            envelope,
            now_ms,
            mls_generation,
            ts_daemon_recv,
            &seen,
            &msgs,
            true,
        )
        .ok()?
        {
            ReceiveOutcome::New { .. } | ReceiveOutcome::Duplicate => Some(mid),
            ReceiveOutcome::Rejected(_) => None,
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "real Tor bootstrap + HS publish + dial, ~2-5 min; run with --ignored"]
async fn five_messages_delivered_over_real_tor() {
    // Bootstrap two Arti runtimes in separate temp dirs.
    let tmp_a = arti_friendly_tempdir();
    let tmp_b = arti_friendly_tempdir();

    let rt_a = TorRuntime::bootstrap(TorConfig {
        state_dir: tmp_a.path().to_path_buf(),
        socks_port: None,
        trust_dir_permissions: true,
    })
    .await
    .expect("A: bootstrap");

    let mut rt_b = TorRuntime::bootstrap(TorConfig {
        state_dir: tmp_b.path().to_path_buf(),
        socks_port: None,
        trust_dir_permissions: true,
    })
    .await
    .expect("B: bootstrap");

    // Identities (Arc because tokio::spawn requires 'static and IdentityKey
    // is not Clone).
    let alice_id = Arc::new(IdentityKey::generate().unwrap());
    let bob_id = Arc::new(IdentityKey::generate().unwrap());

    // In-memory storage pools.
    let alice_pool = Arc::new(Pool::in_memory());
    let bob_pool = Arc::new(Pool::in_memory());

    // MLS group setup: Alice solo → Alice adds Bob → Bob joins via Welcome.
    let alice_provider = MlsProvider::new();
    let bob_provider = MlsProvider::new();
    let bob_kp_repo = KeyPackageRepo::new(&bob_pool);
    let alice_group_repo = MlsGroupRepo::new(&alice_pool);
    let bob_group_repo = MlsGroupRepo::new(&bob_pool);

    let psk = [0x5Au8; 32];
    let kp_ref = [7u8; 32]; // ADR 0009: fixed test KeyPackageRef for the PSK id
    let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &bob_kp_repo).unwrap();

    let mut alice_group =
        Group::create_solo(&alice_id, Some((&kp_ref, &psk)), None, alice_provider).unwrap();
    let (welcome, _commit) = alice_group
        .add_member(&bob_kp, Some((&kp_ref, &psk)), None)
        .unwrap();
    alice_group.save(&alice_group_repo).unwrap();

    let bob_group = Group::join_from_welcome(
        &bob_id,
        welcome.as_slice(),
        Some((&kp_ref, &psk)),
        None,
        bob_provider,
    )
    .unwrap();
    bob_group.save(&bob_group_repo).unwrap();

    let gid: GroupId = alice_group.id().clone();

    // Bob publishes an onion service.
    let bob_hs_key_path = tmp_b.path().join("hs.key.age");
    let bob_seed = Seed::generate().unwrap();
    let bob_onion = rt_b
        .publish_onion(&bob_hs_key_path, &bob_seed, "skattr-1e-tor")
        .await
        .expect("B: publish onion");
    eprintln!("Bob onion: {bob_onion}");

    // Bob hands the rend-request stream to an OnionListener.
    let rend_requests = rt_b
        .rend_requests_take()
        .expect("B: rend_requests must be populated after publish");
    let mut bob_listener = OnionListener::spawn(rend_requests, 8);

    // Alice dials Bob.
    let a_stream = rt_a
        .connect(&bob_onion, 1)
        .await
        .expect("A: connect to Bob");

    // Bob accepts the incoming DataStream.
    let b_stream = bob_listener
        .accepted
        .recv()
        .await
        .expect("B: accept DataStream from listener");

    // Run Noise_XK handshake on both sides concurrently.
    let bob_id_noise = bob_id.clone();
    let alice_id_noise = alice_id.clone();
    let bob_static = skattr_core::test_exports::noise_public_of(&bob_id);

    let responder =
        tokio::spawn(async move { handshake_responder(b_stream, &bob_id_noise, None).await });
    let (alice_conn, _alice_outcome) =
        handshake_initiator(a_stream, &alice_id_noise, &bob_static, None)
            .await
            .expect("A: Noise handshake");
    let (bob_conn, _bob_outcome) = responder.await.unwrap().expect("B: Noise handshake");

    // Create inbound dispatchers for both sides.
    let bob_dispatch: Arc<dyn InboundDispatch> = Arc::new(MlsInboundDispatch {
        pool: bob_pool.clone(),
        group_id: gid.clone(),
        expected_peer: alice_id.public(),
        seen_ciphertexts: std::sync::Mutex::new(std::collections::HashMap::new()),
    });

    // Alice only sends in this test; she doesn't need an InboundDispatch.
    // Let type inference resolve the concrete stream type (arti_client::DataStream)
    // from the connections produced by TorRuntime::connect / OnionListener.
    let alice_hub = DeliveryHub::new(alice_pool.clone());
    let bob_hub = DeliveryHub::new_with_inbound(bob_pool.clone(), bob_dispatch);

    // Register the post-handshake connections with each hub.
    alice_hub.ingest(bob_id.public(), alice_conn).await;
    bob_hub.ingest(alice_id.public(), bob_conn).await;

    // Alice encrypts and sends five envelopes.
    let peer_bob = bob_id.public();
    let alice_group_repo2 = MlsGroupRepo::new(&alice_pool);
    let mut alice_group2 = Group::load(&gid, &alice_group_repo2)
        .unwrap()
        .expect("alice group must exist after save");

    for i in 0..5u32 {
        let envelope = Envelope {
            v: 1,
            id: MessageId::generate(),
            ts: ts_now_ms(),
            reply_to: None,
            kind: Kind::Text {
                body: format!("real-tor-msg-{i}"),
            },
        };
        let mid = envelope.id;
        let ct = alice_group2.encrypt(&envelope).unwrap();
        alice_group2.save(&alice_group_repo2).unwrap();

        Outbox::new(&alice_pool)
            .enqueue(&peer_bob, mid, &ct, 0)
            .unwrap();
        drop(alice_hub.send(peer_bob, mid, ct).await.unwrap());
    }

    // Wait up to 30 s for all five to appear in Bob's messages table.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let msgs = MessageRepo::new(&bob_pool);
        let recent = msgs.recent(&gid.0, 50).unwrap();
        if recent.len() >= 5 {
            eprintln!("All 5 messages delivered.");
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("only {}/{} messages delivered within 30 s", recent.len(), 5);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Graceful shutdown.
    let _ = rt_a.shutdown().await;
    let _ = rt_b.shutdown().await;
}
