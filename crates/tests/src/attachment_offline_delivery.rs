// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 3.C guardrail: an **offline** peer receives a multi-chunk file
//! attachment through the mailbox path — byte-identical, metadata stripped —
//! exercising the REAL production offline pipeline against a REAL in-process
//! mailbox server, deterministically.
//!
//! ## Why component-level (not a full `run_with_transport` spin-up)
//!
//! The production offline lane only fires after a per-peer **90 s** sustained
//! direct-failure stall (`OFFLINE_FALLBACK_STALL_SECS`) and a ~15 s sweep
//! cadence. A full-daemon offline test would have to wait those windows out in
//! real time. This guardrail matches the level of the existing 2.C offline
//! guardrail (`mailbox_offline_delivery.rs`): it drives the SAME production
//! functions directly, with the stall window collapsed to "due now" so no test
//! sleeps on real-time timers.
//!
//! ## Production realness
//!
//! Every load-bearing step is a real production function, reached through the
//! `test_exports` facade (no re-implementation, no `test_exports` hub
//! hand-wiring of the transfer itself):
//!
//!   * **Sender** — `attachment_stage_outbound` runs the exact pipeline
//!     `Command::SendFile` runs: `strip::strip_metadata` → `chunker::chunk_plaintext`
//!     → stage every AEAD chunk in a real `ChunkStore` → persist the `'out'`
//!     `AttachmentRepo` row → `AttachmentDepositRepo::enqueue_all`. The only
//!     determinism knob is `first_due_at_ms = 0` (rows due immediately),
//!     standing in for the elapsed 90 s stall.
//!   * **Deposit** — `attachment_run_chunk_sweep` calls the real
//!     `delivery::chunk_sweep::run_chunk_sweep` with the hub's actual
//!     `MailboxFallbackShared` (the same bundle `run_with_transport` hands the
//!     production sweep task). It resolves Bob's mailbox from his `ContactCard`,
//!     reads staged chunks, and deposits them into the in-process mailbox.
//!   * **Receive** — `attachment_poll_dispatch_once` calls the real
//!     `mailbox::poll::poll_dispatch_once` against a real `DaemonInbound`
//!     (`dispatch_attachment_chunk` → store → `mark_received` → on completion
//!     `finalize_offline` → reassemble → `Event::AttachmentReceived`).
//!
//! Two attachments run, correlated by `attachment_id`:
//!   1. a ~300 KiB `.bin` (≥6 chunks) asserted byte-identical end-to-end;
//!   2. a JPEG-with-EXIF asserted to have its `FF E1` APP1 marker stripped.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use skattr_core::contact::{Contact, ContactCard};
use skattr_core::daemon::events::Event;
use skattr_core::identity::IdentityKey;
use skattr_core::test_exports::{
    attachment_poll_dispatch_once, attachment_run_chunk_sweep, attachment_stage_outbound,
    daemon_inbound_for_attachments, AttachmentManifest, AttachmentRepo, ContactRepo, DeliveryHub,
    Pool, TestChunkStore, TestMailboxFactory,
};
use tokio::sync::broadcast;

use crate::mailbox_harness::{InProcessMailbox, InProcessMailboxFactory};

/// Deterministic, non-trivial binary payload of `len` bytes. `(i % 251)` cycles
/// with a prime period so chunk boundaries don't align with a short repeat — a
/// dropped/duplicated chunk changes the bytes. A `.bin` is a non-image, so the
/// strip step is an identity passthrough and the result must be byte-identical.
fn deterministic_payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// Minimal JPEG carrying an EXIF APP1 segment (`FF E1`). The strip step must
/// remove APP1 before the chunks are produced.
fn jpeg_with_exif() -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8]; // SOI
    v.extend_from_slice(&[0xFF, 0xE1, 0x00, 0x10]); // APP1, length 16
    v.extend_from_slice(b"Exif\0\0");
    v.extend_from_slice(&[0u8; 8]);
    v.extend_from_slice(&[0x55; 64]); // baseline scan filler
    v.extend_from_slice(&[0xFF, 0xD9]); // EOI
    v
}

fn contains_exif_app1(bytes: &[u8]) -> bool {
    bytes.windows(2).any(|w| w == [0xFF, 0xE1])
}

/// Insert a contact row + verified-shaped `ContactCard` advertising `mailboxes`
/// for `peer` into `pool`. `MailboxRepo::list_for_contact` (used by the chunk
/// sweep to resolve where to deposit) reads the mailbox list straight off this
/// card. Mirrors the 2.C `install_contact_with_card` helper.
fn install_contact_with_card(pool: &Pool, peer: &IdentityKey, onion: &str, mailboxes: Vec<String>) {
    let contacts = ContactRepo::new(pool);
    contacts
        .upsert(&Contact {
            identity: peer.public(),
            display_name: Some("peer".into()),
            added_at: 0,
            card: None,
            muted: false,
        })
        .unwrap();
    let card = ContactCard::sign(peer, onion.to_string(), mailboxes, 1, 24 * 3600, 0).unwrap();
    contacts.put_card(&card).unwrap();
}

/// One offline transfer: stage on Alice's side (due now), persist the manifest
/// as a pending `'in'` row in Bob's pool, sweep until every chunk is deposited
/// into the mailbox, then poll Bob's mailbox until the file is reassembled.
/// Returns the written file path from the `AttachmentReceived` event.
#[allow(clippy::too_many_arguments)]
async fn run_one_offline_transfer(
    alice_pool: &Arc<Pool>,
    alice_store: &TestChunkStore,
    alice_hub: &Arc<DeliveryHub<tokio::io::DuplexStream>>,
    bob_pool: &Arc<Pool>,
    bob_id: &Arc<IdentityKey>,
    bob_inbound: &dyn skattr_core::test_exports::InboundDispatch,
    bob_events_rx: &mut broadcast::Receiver<Event>,
    factory: &Arc<dyn TestMailboxFactory>,
    mailbox_onion: &str,
    alice_pub: skattr_core::identity::PublicKey,
    payload: &[u8],
    filename: &str,
    mime: &str,
    min_chunks: usize,
) -> String {
    // ── Sender: real strip → chunk → stage → persist 'out' → enqueue (due 0). ──
    let manifest: AttachmentManifest = attachment_stage_outbound(
        alice_pool,
        alice_store,
        &bob_id.public(),
        payload,
        filename,
        mime,
        0, // due immediately — deterministic substitute for the 90 s stall
    )
    .unwrap();
    let total_chunks = manifest.chunks.len();
    assert!(
        total_chunks >= min_chunks,
        "expected ≥{min_chunks} chunks for {filename} ({} bytes), got {total_chunks}",
        payload.len()
    );

    // ── Recipient pre-state: the manifest the `Kind::File` MLS message would
    //    have delivered, persisted as a pending 'in' row + sender recorded so
    //    `dispatch_attachment_chunk` matches chunks and `finalize_offline` can
    //    attribute the sender. ──
    let bob_attachments = AttachmentRepo::new(bob_pool);
    bob_attachments
        .insert(
            &manifest.attachment_id,
            "in",
            &manifest.to_cbor().unwrap(),
            total_chunks as i64,
            0,
        )
        .unwrap();
    bob_attachments
        .set_peer(&manifest.attachment_id, &alice_pub.0)
        .unwrap();

    // ── Deposit: real chunk sweep. Loop (batch < total) until all deposited;
    //    each call uses an ever-advancing `now_ms` so any rescheduled row is due
    //    on the next pass (none should reschedule here — the mailbox is up). ──
    let mut sweeps = 0;
    loop {
        attachment_run_chunk_sweep(
            alice_pool,
            alice_hub,
            alice_store,
            (sweeps + 1) as i64, // now_ms: rows enqueued due at 0 are due
            4,                   // small batch so a multi-chunk file needs several sweeps
        )
        .await;
        sweeps += 1;
        if skattr_core::test_exports::AttachmentDepositRepo::new(alice_pool)
            .all_deposited(&manifest.attachment_id)
            .unwrap()
        {
            break;
        }
        assert!(sweeps < 100, "chunk sweep did not deposit all chunks");
    }
    if total_chunks > 4 {
        assert!(
            sweeps > 1,
            "batch<total should force multiple sweeps (got {sweeps}) — proves the \
             sweep loop, not a single fire, deposits a multi-chunk file"
        );
    }

    // ── Receive: real poll_dispatch_once against Bob's real DaemonInbound.
    //    Loop until every chunk has been dispatched (one deposit per chunk). ──
    let mut dispatched_total = 0usize;
    let mut polls = 0;
    while dispatched_total < total_chunks {
        let n = attachment_poll_dispatch_once(factory.as_ref(), mailbox_onion, bob_id, bob_inbound)
            .await
            .unwrap();
        dispatched_total += n;
        polls += 1;
        assert!(polls < 200, "poll loop did not drain all chunks");
        if n == 0 {
            // Nothing pending this tick; the mailbox returns all at once, so
            // this only happens after the last chunk — guard against a hang.
            break;
        }
    }
    assert_eq!(
        dispatched_total, total_chunks,
        "every chunk deposit must be dispatched + deleted exactly once"
    );

    // ── The completion event carries the reassembled file path. ──
    let want_id = skattr_core::daemon::hex::Hex16::from(manifest.attachment_id).to_string();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "AttachmentReceived(id={want_id}) not seen"
        );
        match tokio::time::timeout(remaining, bob_events_rx.recv()).await {
            Ok(Ok(Event::AttachmentReceived {
                contact,
                attachment_id,
                path,
                ..
            })) if attachment_id.to_string() == want_id => {
                assert_eq!(
                    contact, alice_pub,
                    "received attachment must be attributed to Alice"
                );
                return path;
            }
            Ok(Ok(_)) => {}
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(broadcast::error::RecvError::Closed)) => panic!("Bob event channel closed"),
            Err(_) => panic!("AttachmentReceived(id={want_id}) not seen within timeout"),
        }
    }
}

/// An offline peer receives a multi-chunk file via the mailbox, byte-identical
/// and metadata-stripped, through the real `run_chunk_sweep` (deposit) +
/// `poll_dispatch_once` / `dispatch_attachment_chunk` / `reassemble` (receive)
/// production functions against a real in-process mailbox server. This is the
/// Phase 3.C offline-transfer guardrail.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn offline_attachment_via_mailbox() {
    // ── Identities + pools ───────────────────────────────────────────────
    let alice_id = Arc::new(IdentityKey::generate().unwrap());
    let bob_id = Arc::new(IdentityKey::generate().unwrap());
    let alice_pub = alice_id.public();
    let alice_pool = Arc::new(Pool::in_memory());
    let bob_pool = Arc::new(Pool::in_memory());

    // ── Real in-process mailbox + factory (the recipient's offline inbox). ──
    let mailbox_onion = "bobsmailbox.onion";
    let mb = Arc::new(InProcessMailbox::new(mailbox_onion));
    let factory: Arc<dyn TestMailboxFactory> = InProcessMailboxFactory::single(mb.clone());

    // ── Alice knows Bob, whose ContactCard advertises the mailbox onion, so
    //    the chunk sweep's `list_for_contact` resolves where to deposit. ──
    install_contact_with_card(
        &alice_pool,
        &bob_id,
        "bobs.onion",
        vec![mailbox_onion.to_string()],
    );

    // ── Alice's delivery hub with the real mailbox fallback bundle wired in —
    //    the same `MailboxFallbackShared` the production sweep task consumes. ──
    let (alice_events_tx, _alice_events_rx) = broadcast::channel::<Event>(64);
    let alice_hub: Arc<DeliveryHub<tokio::io::DuplexStream>> =
        skattr_core::test_exports::delivery_hub_with_mailbox(
            alice_pool.clone(),
            None,
            alice_events_tx,
            factory.clone(),
            alice_id.clone(),
        );

    // ── Sender + recipient chunk stores (real `ChunkStore`s on disk). ──
    let alice_chunk_dir = tempfile::tempdir().unwrap();
    let bob_chunk_dir = tempfile::tempdir().unwrap();
    let bob_download_dir = tempfile::tempdir().unwrap();
    let alice_store = TestChunkStore::new(alice_chunk_dir.path());
    let bob_store = TestChunkStore::new(bob_chunk_dir.path());

    // ── Bob's real production DaemonInbound, wired for the offline receive
    //    path (chunk store + download dir), plus an event subscription. ──
    let (bob_events_tx, mut bob_events_rx) = broadcast::channel::<Event>(64);
    let bob_inbound = daemon_inbound_for_attachments(
        bob_pool.clone(),
        bob_id.clone(),
        bob_events_tx,
        &bob_store,
        bob_download_dir.path().to_path_buf(),
    );

    // ── Transfer 1: ~300 KiB binary (≥6 chunks), byte-identical end-to-end. ──
    let bin_payload = deterministic_payload(300 * 1024);
    let bin_path = run_one_offline_transfer(
        &alice_pool,
        &alice_store,
        &alice_hub,
        &bob_pool,
        &bob_id,
        bob_inbound.as_ref(),
        &mut bob_events_rx,
        &factory,
        mailbox_onion,
        alice_pub,
        &bin_payload,
        "payload.bin",
        "application/octet-stream",
        6,
    )
    .await;
    let got_bin = std::fs::read(&bin_path).unwrap();
    assert_eq!(
        got_bin, bin_payload,
        "received .bin must be byte-identical to the source across all chunks"
    );

    // ── Transfer 2: JPEG with EXIF — strip must remove the APP1 marker. ──
    let jpeg_payload = jpeg_with_exif();
    assert!(
        contains_exif_app1(&jpeg_payload),
        "source JPEG must contain the EXIF APP1 marker we expect stripped"
    );
    let jpeg_path = run_one_offline_transfer(
        &alice_pool,
        &alice_store,
        &alice_hub,
        &bob_pool,
        &bob_id,
        bob_inbound.as_ref(),
        &mut bob_events_rx,
        &factory,
        mailbox_onion,
        alice_pub,
        &jpeg_payload,
        "photo.jpg",
        "image/jpeg",
        1,
    )
    .await;
    let got_jpeg = std::fs::read(&jpeg_path).unwrap();
    assert!(
        !contains_exif_app1(&got_jpeg),
        "EXIF APP1 marker (FF E1) must be stripped before the chunks hit the mailbox"
    );
}
