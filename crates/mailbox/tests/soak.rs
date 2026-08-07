// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! 24-hour soak driver. `#[ignore]`-gated; run on a developer
//! workstation as part of the freeze-PR validation, not on CI:
//!
//! ```bash
//! cargo test -p skattr-mailbox --release --test soak -- --ignored \
//!     --nocapture > docs/superpowers/runs/<merge-date>-mailbox-soak.txt
//! ```

#![cfg(not(target_os = "windows"))] // UDS-only platforms
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use futures::{SinkExt, StreamExt};
use rand::{rngs::OsRng, Rng, RngCore, SeedableRng};
use sha2::{Digest, Sha256};
use skattr_core::mailbox::protocol::{Challenge, Deposit, PROTOCOL_VERSION};
use skattr_mailbox::codec::{MailboxFrame, MailboxFrameCodec};
use skattr_mailbox::policy::Policy;
use skattr_mailbox::server::MailboxServer;
use skattr_mailbox::store::Store;
use tokio_util::codec::Framed;

const SOAK_RECIPIENTS: usize = 1_000;
const SOAK_DURATION_SECS: u64 = 24 * 3600;
const SOAK_DEPOSIT_RATE_PER_HOUR: u64 = 100;

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore]
async fn soak_24h() {
    let store = Arc::new(Store::in_memory().unwrap());
    let mb = Arc::new(MailboxServer::new(store.clone(), Policy::recommended()));

    // Pre-generate identities.
    let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(0x5BAA_F00D);
    let identities: Vec<[u8; 32]> = (0..SOAK_RECIPIENTS)
        .map(|_| {
            let mut s = [0u8; 32];
            rng.fill_bytes(&mut s);
            let sk = SigningKey::from_bytes(&s);
            sk.verifying_key().to_bytes()
        })
        .collect();

    // Spawn deposit producers. One task per recipient, jittered.
    let start = Instant::now();
    let deadline = start + Duration::from_secs(SOAK_DURATION_SECS);
    let mut handles = Vec::new();
    for pk in &identities {
        let recipient_hash: [u8; 32] = Sha256::digest(pk).into();
        let mb = mb.clone();
        handles.push(tokio::spawn(async move {
            // Mean inter-arrival = 3600 / rate seconds.
            let mean = Duration::from_secs_f64(3600.0 / SOAK_DEPOSIT_RATE_PER_HOUR as f64);
            // ChaCha20 is `Send`, unlike `rand::thread_rng()`'s `ThreadRng`,
            // which is required because this future runs on a multi-thread
            // tokio runtime.
            let mut rng = rand_chacha::ChaCha20Rng::from_entropy();
            while Instant::now() < deadline {
                let jitter = rng.gen_range(0.5..1.5);
                tokio::time::sleep(mean.mul_f64(jitter)).await;
                let (client, server) = tokio::io::duplex(8 * 1024);
                let mb2 = mb.clone();
                tokio::spawn(async move { mb2.accept_loop(server).await });
                let mut framed = Framed::new(client, MailboxFrameCodec::new());
                let body = Deposit {
                    version: PROTOCOL_VERSION,
                    recipient_hash,
                    ciphertext: vec![0u8; rng.gen_range(64..4096)],
                    ttl_request: 86_400,
                };
                if framed.send(MailboxFrame::Deposit(body)).await.is_err() {
                    continue;
                }
                let _ = framed.next().await;
            }
        }));
    }

    // Periodic invariant checks.
    let store_for_metrics = store.clone();
    let metrics_handle = tokio::spawn(async move {
        let mut peak_bytes: u64 = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            if Instant::now() >= deadline {
                break;
            }
            let bytes = store_for_metrics.storage_bytes().unwrap_or(0);
            peak_bytes = peak_bytes.max(bytes);
            eprintln!(
                "soak metrics: t={}s bytes={} peak={}",
                start.elapsed().as_secs(),
                bytes,
                peak_bytes
            );
        }
        peak_bytes
    });

    for h in handles {
        let _ = h.await;
    }
    let peak_bytes = metrics_handle.await.unwrap();
    let final_bytes = store.storage_bytes().unwrap();
    let policy = Policy::recommended();
    let max_recipient_total = policy.recipient_cap_bytes * SOAK_RECIPIENTS as u64;
    eprintln!(
        "SOAK SUMMARY peak_bytes={} final_bytes={} max_allowed={}",
        peak_bytes, final_bytes, max_recipient_total
    );
    assert!(
        peak_bytes <= max_recipient_total + policy.max_deposit_size,
        "storage exceeded recipient_cap_bytes * recipients by more than one deposit"
    );
}
