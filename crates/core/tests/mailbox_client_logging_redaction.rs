// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Mailbox-client log-redaction guard.
//!
//! Drives a representative MailboxClient session through a capturing
//! `tracing` subscriber and asserts that no onion addresses, public keys,
//! message ids, or ciphertexts appear in log lines at INFO level or above.

#![cfg(feature = "test-harness")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use tracing::Level;
use tracing_subscriber::{layer::SubscriberExt, Layer};

use futures::{SinkExt, StreamExt};
use skattr_core::identity::IdentityKey;
use skattr_core::mailbox::protocol::{
    ChallengeNonce, DeleteOk, DepositOk, ErrorBody, ErrorCode, FetchResponse, PendingDeposit,
};
use skattr_core::test_exports::{MailboxClientHandle, MailboxFrame, MailboxFrameCodec};
use tokio::io::duplex;
use tokio_util::codec::Framed;

// ── Capturing subscriber ─────────────────────────────────────────────────────

#[derive(Default, Clone)]
struct CapturingLayer {
    lines: Arc<Mutex<Vec<String>>>,
}

impl<S> Layer<S> for CapturingLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if *event.metadata().level() <= Level::INFO {
            struct V<'a>(&'a mut String);
            impl tracing::field::Visit for V<'_> {
                fn record_debug(&mut self, _: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                    self.0.push_str(&format!("{value:?} "));
                }
                fn record_str(&mut self, _: &tracing::field::Field, value: &str) {
                    self.0.push_str(value);
                    self.0.push(' ');
                }
            }
            let mut s = String::new();
            event.record(&mut V(&mut s));
            s.push_str(event.metadata().target());
            if let Ok(mut guard) = self.lines.lock() {
                guard.push(s);
            }
        }
    }
}

// ── In-process server that handles one complete session ───────────────────────

async fn run_server_session(mut framed: Framed<tokio::io::DuplexStream, MailboxFrameCodec>) {
    // 1. probe: Challenge → ChallengeNonce
    if let Some(Ok(MailboxFrame::Challenge(_))) = framed.next().await {
        framed
            .send(MailboxFrame::ChallengeNonce(ChallengeNonce {
                nonce: [0x42; 32],
                issued_at: 1,
            }))
            .await
            .unwrap();
    }

    // 2. fetch: Challenge → ChallengeNonce, Fetch → FetchResponse
    if let Some(Ok(MailboxFrame::Challenge(_))) = framed.next().await {
        framed
            .send(MailboxFrame::ChallengeNonce(ChallengeNonce {
                nonce: [0x43; 32],
                issued_at: 2,
            }))
            .await
            .unwrap();
    }
    if let Some(Ok(MailboxFrame::Fetch(_))) = framed.next().await {
        framed
            .send(MailboxFrame::FetchResponse(FetchResponse {
                deposits: vec![PendingDeposit {
                    deposit_id: [0x99; 16],
                    ciphertext: vec![0xAB, 0xCD],
                    received_at: 3,
                }],
            }))
            .await
            .unwrap();
    }

    // 3. deposit: Deposit → DepositOk
    if let Some(Ok(MailboxFrame::Deposit(_))) = framed.next().await {
        framed
            .send(MailboxFrame::DepositOk(DepositOk {
                deposit_id: [0x77; 16],
                expires_at: 9999,
            }))
            .await
            .unwrap();
    }

    // 4. error path: probe → Error{RateLimited}
    if let Some(Ok(MailboxFrame::Challenge(_))) = framed.next().await {
        framed
            .send(MailboxFrame::Error(ErrorBody {
                code: ErrorCode::RateLimited,
                message: "slow down".into(),
            }))
            .await
            .unwrap();
    }

    // 5. delete: Challenge → ChallengeNonce, Delete → DeleteOk
    if let Some(Ok(MailboxFrame::Challenge(_))) = framed.next().await {
        framed
            .send(MailboxFrame::ChallengeNonce(ChallengeNonce {
                nonce: [0x44; 32],
                issued_at: 4,
            }))
            .await
            .unwrap();
    }
    if let Some(Ok(MailboxFrame::Delete(_))) = framed.next().await {
        framed
            .send(MailboxFrame::DeleteOk(DeleteOk {
                deleted: 1,
                not_found: 0,
            }))
            .await
            .unwrap();
    }
}

// ── Test ─────────────────────────────────────────────────────────────────────

/// Drive a representative session and assert nothing sensitive appears at INFO+.
///
/// Uses a plain `#[test]` (not `#[tokio::test]`) to avoid nested-runtime
/// issues: `tracing::subscriber::with_default` is sync, so we build our own
/// `current_thread` runtime inside the closure.
#[test]
fn info_level_logs_redact_onions_pubkeys_message_ids_ciphertext() {
    let captured = Arc::new(Mutex::new(Vec::<String>::new()));
    let layer = CapturingLayer {
        lines: captured.clone(),
    };
    let subscriber = tracing_subscriber::registry().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let signer = IdentityKey::generate().unwrap();
            let (client_half, server_half) = duplex(256 * 1024);
            let server_framed = Framed::new(server_half, MailboxFrameCodec::new());
            let server = tokio::spawn(run_server_session(server_framed));

            let mut client = MailboxClientHandle::from_stream(
                // Realistic-looking onion; the redaction test asserts it never
                // leaks into INFO+ logs.
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.onion".into(),
                client_half,
            );

            // 1. probe
            client.probe([0xCD; 32]).await.unwrap();

            // 2. fetch
            let resp = client.fetch(&signer).await.unwrap();
            assert_eq!(resp.deposits.len(), 1);

            // 3. deposit
            client
                .deposit([0xBB; 32], vec![1, 2, 3], 86_400)
                .await
                .unwrap();

            // 4. error path — probe against a rate-limited server
            let _ = client.probe([0xEE; 32]).await; // returns Err(RateLimited) — ignored

            // 5. delete
            client
                .delete(&signer, vec![resp.deposits[0].deposit_id])
                .await
                .unwrap();

            server.await.unwrap();
        });
    });

    let lines = captured.lock().unwrap();

    // There must be at least some captured lines (we added tracing::info! calls).
    // If this assertion fails, the tracing::info! calls were removed from
    // client.rs and the guard is no longer meaningful — add them back.
    assert!(
        !lines.is_empty(),
        "expected at least one INFO+ log line from MailboxClient; \
         check that tracing::info! calls exist in mailbox/client.rs"
    );

    for line in lines.iter() {
        // ── Onion addresses ─────────────────────────────────────────────
        // v3 onion: 56 base32 chars + ".onion"
        assert!(
            !line.contains(".onion"),
            "leaked onion address at INFO+: {line:?}"
        );

        // ── Public keys / message IDs: long hex runs ─────────────────────
        // A pub key is 32 bytes = 64 hex chars. A deposit_id is 16 bytes = 32
        // hex chars. Flag any consecutive hex run of 32+ chars.
        let hex_run: usize = line
            .chars()
            .scan(0usize, |acc, c| {
                *acc = if c.is_ascii_hexdigit() { *acc + 1 } else { 0 };
                Some(*acc)
            })
            .max()
            .unwrap_or(0);
        assert!(
            hex_run < 32,
            "long hex run (>= 32 chars) looks like a key/id at INFO+: {line:?}"
        );

        // ── Ciphertext: base64 or long alphanumeric repr ──────────────────
        // Any long base64 run (64+ chars of [A-Za-z0-9+/=]) is suspicious.
        let b64_run: usize = line
            .chars()
            .scan(0usize, |acc, c| {
                *acc = if c.is_alphanumeric() || c == '+' || c == '/' || c == '=' {
                    *acc + 1
                } else {
                    0
                };
                Some(*acc)
            })
            .max()
            .unwrap_or(0);
        assert!(
            b64_run < 64,
            "long base64-like run (>= 64 chars) may be ciphertext at INFO+: {line:?}"
        );
    }
}
