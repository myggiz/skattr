// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Asserts the mailbox's `info`/`warn`/`error`-level log lines never
//! contain a full 64-hex pubkey, full 32-byte hash, ciphertext, or
//! 16-byte deposit_id. Implementation strategy: drive a sequence of
//! in-process operations through MailboxServer, capture all log
//! events at `info+` via a `tracing_subscriber::Layer` that records
//! every message into a Vec<String>, then scan.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use futures::{SinkExt, StreamExt};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use skattr_core::mailbox::protocol::{Challenge, Deposit, PROTOCOL_VERSION};
use skattr_mailbox::codec::{MailboxFrame, MailboxFrameCodec};
use skattr_mailbox::policy::Policy;
use skattr_mailbox::server::MailboxServer;
use skattr_mailbox::store::Store;
use tokio_util::codec::Framed;
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

#[derive(Default, Clone)]
struct Capture(Arc<Mutex<Vec<String>>>);

impl<S: Subscriber> Layer<S> for Capture {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = StringVisitor(String::new());
        event.record(&mut visitor);
        let level = *event.metadata().level();
        if level <= tracing::Level::INFO {
            self.0.lock().unwrap().push(visitor.0);
        }
    }
}

struct StringVisitor(String);

impl tracing::field::Visit for StringVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(&format!("{}={:?} ", field.name(), value));
    }
}

#[tokio::test]
async fn no_full_hash_or_pubkey_at_info_level() {
    let cap = Capture::default();
    let cap_clone = cap.clone();
    let subscriber = Registry::default().with(cap_clone);
    let _g = tracing::subscriber::set_default(subscriber);

    // Drive a Deposit + Challenge + a deliberate auth failure.
    let (client, server) = tokio::io::duplex(64 * 1024);
    let store = Arc::new(Store::in_memory().unwrap());
    let mb = MailboxServer::new(store, Policy::recommended());
    let handle = tokio::spawn(async move { mb.accept_loop(server).await });

    let mut framed = Framed::new(client, MailboxFrameCodec::new());
    let recipient = [0xAA; 32];
    framed
        .send(MailboxFrame::Deposit(Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash: recipient,
            ciphertext: vec![0xCC; 64],
            ttl_request: 86_400,
        }))
        .await
        .unwrap();
    let _ = framed.next().await.unwrap().unwrap();

    let sk = SigningKey::generate(&mut OsRng);
    let pk: [u8; 32] = sk.verifying_key().to_bytes();
    let id_hash: [u8; 32] = Sha256::digest(pk).into();
    framed
        .send(MailboxFrame::Challenge(Challenge {
            version: PROTOCOL_VERSION,
            identity_hash: id_hash,
        }))
        .await
        .unwrap();
    let _ = framed.next().await.unwrap().unwrap();

    drop(framed);
    handle.await.unwrap().unwrap();

    let lines = cap.0.lock().unwrap().clone();
    let recipient_hex = hex_lower(&recipient);
    let pk_hex = hex_lower(&pk);
    let id_hash_hex = hex_lower(&id_hash);
    let ciphertext_hex = hex_lower(&[0xCC; 64]);
    for line in &lines {
        let lower = line.to_ascii_lowercase();
        for forbidden in [&recipient_hex, &pk_hex, &id_hash_hex, &ciphertext_hex] {
            assert!(
                !lower.contains(forbidden.as_str()),
                "info-level log leaked secret hex: line={line:?} forbidden={forbidden:?}"
            );
        }
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut s, "{b:02x}");
    }
    s
}
