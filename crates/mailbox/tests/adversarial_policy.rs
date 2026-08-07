// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Adversarial coverage for TooLarge / TtlTooLong / TtlTooShort /
//! RateLimited / RecipientFull / UnsupportedVersion.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use skattr_core::mailbox::protocol::{Deposit, ErrorCode, PROTOCOL_VERSION};
use skattr_mailbox::codec::{MailboxFrame, MailboxFrameCodec};
use skattr_mailbox::policy::Policy;
use skattr_mailbox::server::MailboxServer;
use skattr_mailbox::store::Store;
use tokio_util::codec::Framed;

async fn spawn_with_policy(
    policy: Policy,
) -> (
    Framed<tokio::io::DuplexStream, MailboxFrameCodec>,
    tokio::task::JoinHandle<Result<(), skattr_mailbox::error::MailboxError>>,
) {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let store = Arc::new(Store::in_memory().unwrap());
    let mb = MailboxServer::new(store, policy);
    let handle = tokio::spawn(async move { mb.accept_loop(server).await });
    (Framed::new(client, MailboxFrameCodec::new()), handle)
}

async fn deposit_and_get_code(
    framed: &mut Framed<tokio::io::DuplexStream, MailboxFrameCodec>,
    body: Deposit,
) -> ErrorCode {
    framed.send(MailboxFrame::Deposit(body)).await.unwrap();
    match framed.next().await.unwrap().unwrap() {
        MailboxFrame::Error(e) => e.code,
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn unsupported_version_rejected() {
    let (mut framed, handle) = spawn_with_policy(Policy::recommended()).await;
    let code = deposit_and_get_code(
        &mut framed,
        Deposit {
            version: 99,
            recipient_hash: [0; 32],
            ciphertext: vec![],
            ttl_request: 0,
        },
    )
    .await;
    assert_eq!(code, ErrorCode::UnsupportedVersion);
    drop(framed);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn too_large_rejected() {
    let policy = Policy::recommended();
    let max = policy.max_deposit_size as usize;
    let (mut framed, handle) = spawn_with_policy(policy).await;
    let code = deposit_and_get_code(
        &mut framed,
        Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash: [0; 32],
            ciphertext: vec![0; max + 1],
            ttl_request: 86_400,
        },
    )
    .await;
    assert_eq!(code, ErrorCode::TooLarge);
    drop(framed);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn ttl_too_short_rejected() {
    let (mut framed, handle) = spawn_with_policy(Policy::recommended()).await;
    let code = deposit_and_get_code(
        &mut framed,
        Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash: [0; 32],
            ciphertext: vec![1, 2, 3],
            ttl_request: 60, // below 1h min
        },
    )
    .await;
    assert_eq!(code, ErrorCode::TtlTooShort);
    drop(framed);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn ttl_too_long_rejected() {
    let (mut framed, handle) = spawn_with_policy(Policy::recommended()).await;
    let code = deposit_and_get_code(
        &mut framed,
        Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash: [0; 32],
            ciphertext: vec![1, 2, 3],
            ttl_request: u32::MAX,
        },
    )
    .await;
    assert_eq!(code, ErrorCode::TtlTooLong);
    drop(framed);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn rate_limit_triggers() {
    let mut policy = Policy::recommended();
    policy.per_conn_deposits_per_min = 1;
    let (mut framed, handle) = spawn_with_policy(policy).await;
    // First succeeds.
    framed
        .send(MailboxFrame::Deposit(Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash: [0; 32],
            ciphertext: vec![1, 2, 3],
            ttl_request: 86_400,
        }))
        .await
        .unwrap();
    let _ok = framed.next().await.unwrap().unwrap();
    // Second hits rate limit.
    let code = deposit_and_get_code(
        &mut framed,
        Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash: [1; 32],
            ciphertext: vec![1, 2, 3],
            ttl_request: 86_400,
        },
    )
    .await;
    assert_eq!(code, ErrorCode::RateLimited);
    drop(framed);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn recipient_full_rejected_when_no_evictable_rows() {
    let mut policy = Policy::recommended();
    policy.recipient_cap_bytes = 8;
    policy.max_deposit_size = 8;
    let (mut framed, handle) = spawn_with_policy(policy).await;
    // Fill the cap with two non-expired deposits.
    for i in 0..2 {
        framed
            .send(MailboxFrame::Deposit(Deposit {
                version: PROTOCOL_VERSION,
                recipient_hash: [9; 32],
                ciphertext: vec![i; 4],
                ttl_request: 86_400,
            }))
            .await
            .unwrap();
        let _ = framed.next().await.unwrap().unwrap();
    }
    let code = deposit_and_get_code(
        &mut framed,
        Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash: [9; 32],
            ciphertext: vec![0xAB; 4],
            ttl_request: 86_400,
        },
    )
    .await;
    assert_eq!(code, ErrorCode::RecipientFull);
    drop(framed);
    handle.await.unwrap().unwrap();
}
