// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Adversarial coverage for HashMismatch / InvalidSignature / NonceExpired.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use ed25519_dalek::{Signer, SigningKey};
use futures::{SinkExt, StreamExt};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use skattr_core::mailbox::protocol::{Challenge, ErrorCode, Fetch, PROTOCOL_VERSION};
use skattr_mailbox::auth::{payload_digest, AUTH_DOMAIN, OP_BYTE_FETCH};
use skattr_mailbox::codec::{MailboxFrame, MailboxFrameCodec};
use skattr_mailbox::policy::Policy;
use skattr_mailbox::server::MailboxServer;
use skattr_mailbox::store::Store;
use tokio_util::codec::Framed;

async fn spawn_server() -> (
    Framed<tokio::io::DuplexStream, MailboxFrameCodec>,
    tokio::task::JoinHandle<Result<(), skattr_mailbox::error::MailboxError>>,
) {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let store = Arc::new(Store::in_memory().unwrap());
    let mb = MailboxServer::new(store, Policy::recommended());
    let handle = tokio::spawn(async move { mb.accept_loop(server).await });
    (Framed::new(client, MailboxFrameCodec::new()), handle)
}

fn build_signed_fetch(sk: &SigningKey, nonce: [u8; 32]) -> Fetch {
    let pk: [u8; 32] = sk.verifying_key().to_bytes();
    // Compute digest the same way handle_fetch does — positional tuple
    // (CBOR definite-length array). Task 16 (commit 00a99d8) replaced
    // the earlier struct-based digest input with this tuple form so the
    // encoding has no field-order ambiguity.
    let digest = payload_digest(&(PROTOCOL_VERSION, pk, nonce)).unwrap();
    let mut input = Vec::new();
    input.extend_from_slice(AUTH_DOMAIN);
    input.extend_from_slice(&nonce);
    input.push(OP_BYTE_FETCH);
    input.extend_from_slice(&digest);
    let sig = sk.sign(&input).to_bytes();
    Fetch {
        version: PROTOCOL_VERSION,
        identity_pubkey: pk,
        nonce,
        signature: sig,
    }
}

async fn issue_nonce(
    framed: &mut Framed<tokio::io::DuplexStream, MailboxFrameCodec>,
    identity_hash: [u8; 32],
) -> [u8; 32] {
    framed
        .send(MailboxFrame::Challenge(Challenge {
            version: PROTOCOL_VERSION,
            identity_hash,
        }))
        .await
        .unwrap();
    match framed.next().await.unwrap().unwrap() {
        MailboxFrame::ChallengeNonce(n) => n.nonce,
        other => panic!("expected ChallengeNonce, got {other:?}"),
    }
}

#[tokio::test]
async fn hash_mismatch_rejected() {
    let (mut framed, handle) = spawn_server().await;
    let sk = SigningKey::generate(&mut OsRng);
    // Bind nonce to a totally unrelated identity hash.
    let nonce = issue_nonce(&mut framed, [0xFF; 32]).await;
    let fetch = build_signed_fetch(&sk, nonce);
    framed.send(MailboxFrame::Fetch(fetch)).await.unwrap();
    let resp = framed.next().await.unwrap().unwrap();
    if let MailboxFrame::Error(e) = resp {
        assert_eq!(e.code, ErrorCode::HashMismatch);
    } else {
        panic!("expected Error frame");
    }
    drop(framed);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn invalid_signature_rejected() {
    let (mut framed, handle) = spawn_server().await;
    let sk = SigningKey::generate(&mut OsRng);
    let pk: [u8; 32] = sk.verifying_key().to_bytes();
    let id_hash: [u8; 32] = Sha256::digest(pk).into();
    let nonce = issue_nonce(&mut framed, id_hash).await;
    let mut fetch = build_signed_fetch(&sk, nonce);
    fetch.signature[0] ^= 0xFF;
    framed.send(MailboxFrame::Fetch(fetch)).await.unwrap();
    let resp = framed.next().await.unwrap().unwrap();
    if let MailboxFrame::Error(e) = resp {
        assert_eq!(e.code, ErrorCode::InvalidSignature);
    } else {
        panic!("expected Error frame");
    }
    drop(framed);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn nonce_replay_rejected() {
    let (mut framed, handle) = spawn_server().await;
    let sk = SigningKey::generate(&mut OsRng);
    let pk: [u8; 32] = sk.verifying_key().to_bytes();
    let id_hash: [u8; 32] = Sha256::digest(pk).into();
    let nonce = issue_nonce(&mut framed, id_hash).await;
    let fetch = build_signed_fetch(&sk, nonce);
    // First use: succeeds.
    framed
        .send(MailboxFrame::Fetch(fetch.clone()))
        .await
        .unwrap();
    let _ = framed.next().await.unwrap().unwrap();
    // Replay: must reject as NonceExpired.
    framed.send(MailboxFrame::Fetch(fetch)).await.unwrap();
    let resp = framed.next().await.unwrap().unwrap();
    if let MailboxFrame::Error(e) = resp {
        assert_eq!(e.code, ErrorCode::NonceExpired);
    } else {
        panic!("expected Error frame");
    }
    drop(framed);
    handle.await.unwrap().unwrap();
}
