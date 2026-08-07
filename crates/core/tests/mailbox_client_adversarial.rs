// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Adversarial mailbox-client regression suite. Each scenario exercises
//! one class of mailbox-side misbehavior and asserts the client surfaces
//! the right `MailboxClientErrorKind` variant.

#![cfg(feature = "test-harness")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use futures::{SinkExt, StreamExt};
use skattr_core::error::{CoreError, MailboxClientErrorKind};
use skattr_core::identity::IdentityKey;
use skattr_core::test_exports::{MailboxClientHandle, MailboxFrame, MailboxFrameCodec};
use tokio::io::{duplex, AsyncWriteExt};
use tokio_util::codec::Framed;

use skattr_core::mailbox::protocol::{
    ChallengeNonce, ErrorBody, ErrorCode, FetchResponse, PendingDeposit,
};

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Build a `MailboxClientHandle` backed by a duplex pair; return (client, server_framed).
fn make_pair() -> (
    MailboxClientHandle,
    Framed<tokio::io::DuplexStream, MailboxFrameCodec>,
) {
    let (client_half, server_half) = duplex(256 * 1024);
    let client = MailboxClientHandle::from_stream("test.onion".into(), client_half);
    let server_framed = Framed::new(server_half, MailboxFrameCodec::new());
    (client, server_framed)
}

// ── Scenario 1: server returns Internal on every Fetch ───────────────────────

/// Verify that `Error{Internal}` on every Fetch maps to `Other(_)` five times.
#[tokio::test]
async fn malicious_mailbox_replies_internal_on_every_fetch() {
    let signer = IdentityKey::generate().unwrap();

    for _ in 0..5 {
        let (mut client, mut server_framed) = make_pair();
        let server = tokio::spawn(async move {
            // Challenge → ChallengeNonce
            let _ = server_framed.next().await;
            server_framed
                .send(MailboxFrame::ChallengeNonce(ChallengeNonce {
                    nonce: [0x11; 32],
                    issued_at: 1,
                }))
                .await
                .unwrap();
            // Fetch → Error{Internal}
            let _ = server_framed.next().await;
            server_framed
                .send(MailboxFrame::Error(ErrorBody {
                    code: ErrorCode::Internal,
                    message: "internal server error".into(),
                }))
                .await
                .unwrap();
        });

        let err = client.fetch(&signer).await.unwrap_err();
        assert!(
            matches!(
                err,
                CoreError::MailboxClient(MailboxClientErrorKind::Other(_))
            ),
            "expected Other(_) for Internal error code, got: {err:?}"
        );
        server.await.unwrap();
    }
}

// ── Scenario 2: server issues NonceExpired on first Fetch; fresh fetch works ──

/// First `fetch` sees NonceExpired; a second `fetch` (fresh connection) succeeds.
#[tokio::test]
async fn malicious_replay_old_challenge_nonce() {
    let signer = IdentityKey::generate().unwrap();

    // First fetch: Challenge succeeds, Fetch returns NonceExpired.
    let (mut client_first, mut server_framed_first) = make_pair();
    let server_first = tokio::spawn(async move {
        let _ = server_framed_first.next().await; // consume Challenge
        server_framed_first
            .send(MailboxFrame::ChallengeNonce(ChallengeNonce {
                nonce: [0xAA; 32],
                issued_at: 1,
            }))
            .await
            .unwrap();
        let _ = server_framed_first.next().await; // consume Fetch
        server_framed_first
            .send(MailboxFrame::Error(ErrorBody {
                code: ErrorCode::NonceExpired,
                message: "nonce expired".into(),
            }))
            .await
            .unwrap();
    });

    let err = client_first.fetch(&signer).await.unwrap_err();
    assert!(
        matches!(
            err,
            CoreError::MailboxClient(MailboxClientErrorKind::NonceExpired)
        ),
        "expected NonceExpired, got: {err:?}"
    );
    server_first.await.unwrap();

    // Second fetch on a fresh connection: fresh nonce, succeeds with empty deposits.
    let (mut client_second, mut server_framed_second) = make_pair();
    let server_second = tokio::spawn(async move {
        let _ = server_framed_second.next().await; // consume Challenge
        server_framed_second
            .send(MailboxFrame::ChallengeNonce(ChallengeNonce {
                nonce: [0xBB; 32], // different nonce
                issued_at: 2,
            }))
            .await
            .unwrap();
        let _ = server_framed_second.next().await; // consume Fetch
        server_framed_second
            .send(MailboxFrame::FetchResponse(FetchResponse {
                deposits: vec![],
            }))
            .await
            .unwrap();
    });

    let resp = client_second.fetch(&signer).await.unwrap();
    assert!(resp.deposits.is_empty(), "expected empty fetch response");
    server_second.await.unwrap();
}

// ── Scenario 3: server returns garbage ciphertext in FetchResponse ────────────

/// Client passes arbitrary ciphertext through; it does NOT decrypt at this layer.
#[tokio::test]
async fn malicious_returns_arbitrary_ciphertext_in_fetch_response() {
    let signer = IdentityKey::generate().unwrap();
    let garbage_ciphertext: Vec<u8> = (0u8..=255u8).cycle().take(512).collect();
    let garbage_clone = garbage_ciphertext.clone();

    let (mut client, mut server_framed) = make_pair();
    let server = tokio::spawn(async move {
        let _ = server_framed.next().await; // consume Challenge
        server_framed
            .send(MailboxFrame::ChallengeNonce(ChallengeNonce {
                nonce: [0x55; 32],
                issued_at: 1,
            }))
            .await
            .unwrap();
        let _ = server_framed.next().await; // consume Fetch
        server_framed
            .send(MailboxFrame::FetchResponse(FetchResponse {
                deposits: vec![PendingDeposit {
                    deposit_id: [0xDE; 16],
                    ciphertext: garbage_clone,
                    received_at: 1,
                }],
            }))
            .await
            .unwrap();
    });

    // The client should succeed and pass the ciphertext through unchanged.
    let resp = client.fetch(&signer).await.unwrap();
    assert_eq!(resp.deposits.len(), 1, "expected one deposit");
    assert_eq!(
        resp.deposits[0].ciphertext, garbage_ciphertext,
        "client must not modify ciphertext"
    );
    server.await.unwrap();
}

// ── Scenario 4: server writes malformed CBOR ─────────────────────────────────

/// A frame with valid length+type header but garbage CBOR body maps to `Malformed`.
#[tokio::test]
async fn mailbox_returns_malformed_cbor() {
    let (client_half, mut server_half) = duplex(256 * 1024);
    let mut client = MailboxClientHandle::from_stream("bad.onion".into(), client_half);

    let server = tokio::spawn(async move {
        // Write a frame: length=5 (1 type byte + 4 payload), type=0x85
        // (ChallengeNonce), followed by 4 garbage bytes that aren't valid CBOR.
        let garbage_payload = [0xFF, 0xFE, 0xFD, 0xFC];
        let length: u32 = 1 + garbage_payload.len() as u32;
        server_half.write_all(&length.to_be_bytes()).await.unwrap();
        server_half.write_all(&[0x85]).await.unwrap(); // ChallengeNonce type byte
        server_half.write_all(&garbage_payload).await.unwrap();
        server_half.flush().await.unwrap();
    });

    // probe sends a Challenge and waits for a ChallengeNonce; the malformed
    // ChallengeNonce body surfaces as Malformed.
    let err = client.probe([0xCD; 32]).await.unwrap_err();
    assert!(
        matches!(
            err,
            CoreError::MailboxClient(MailboxClientErrorKind::Malformed)
        ),
        "expected Malformed for garbage CBOR, got: {err:?}"
    );
    server.await.unwrap();
}

// ── Scenario 5: server writes unknown frame type byte ────────────────────────

/// A frame with an unknown type byte (0x20) maps to `Malformed`.
#[tokio::test]
async fn mailbox_returns_unknown_frame_type_byte() {
    let (client_half, mut server_half) = duplex(256 * 1024);
    let mut client = MailboxClientHandle::from_stream("unknown-type.onion".into(), client_half);

    let server = tokio::spawn(async move {
        // Write a frame with length=3 (1 type + 2 payload), type=0x20 (bogus).
        let payload = [0xA0, 0x00]; // CBOR empty map — content irrelevant; type byte is invalid
        let length: u32 = 1 + payload.len() as u32;
        server_half.write_all(&length.to_be_bytes()).await.unwrap();
        server_half.write_all(&[0x20]).await.unwrap(); // unknown type byte
        server_half.write_all(&payload).await.unwrap();
        server_half.flush().await.unwrap();
    });

    let err = client.probe([0xCD; 32]).await.unwrap_err();
    assert!(
        matches!(
            err,
            CoreError::MailboxClient(MailboxClientErrorKind::Malformed)
        ),
        "expected Malformed for unknown type byte, got: {err:?}"
    );
    server.await.unwrap();
}
