// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Cross-codec parity for v1 mailbox frames.
//!
//! For every frame type the client (core) encodes is decodable by the
//! server (mailbox) and vice versa. This is the wire-frozen contract;
//! any divergence here is a Phase 2.B bug, not a feature change.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use tokio_util::bytes::BytesMut;
use skattr_core::test_exports::{
    MailboxFrame as ClientFrame, MailboxFrameCodec as ClientCodec,
    MailboxFrameKind as ClientFrameKind,
};
use skattr_core::mailbox::protocol::{
    Challenge, ChallengeNonce, Delete, DeleteOk, Deposit, DepositOk, ErrorBody, ErrorCode, Fetch,
    FetchResponse, PendingDeposit, PROTOCOL_VERSION,
};
use skattr_mailbox::codec::{MailboxFrame as ServerFrame, MailboxFrameCodec as ServerCodec, MailboxFrameKind as ServerFrameKind};
use tokio_util::codec::{Decoder, Encoder};

// === C → S frames (client encode → server decode) =====================

#[test]
fn client_deposit_decodes_on_server() {
    let body = Deposit {
        version: PROTOCOL_VERSION,
        recipient_hash: [0xAA; 32],
        ciphertext: vec![1, 2, 3, 4],
        ttl_request: 86_400,
    };
    let mut buf = BytesMut::new();
    ClientCodec::new()
        .encode(ClientFrame::Deposit(body.clone()), &mut buf)
        .unwrap();
    let decoded = ServerCodec::new().decode(&mut buf).unwrap().unwrap();
    let ServerFrame::Deposit(got) = decoded else {
        panic!("wrong frame kind on server decode")
    };
    assert_eq!(got, body);
}

#[test]
fn client_challenge_decodes_on_server() {
    let body = Challenge { version: PROTOCOL_VERSION, identity_hash: [0xCD; 32] };
    let mut buf = BytesMut::new();
    ClientCodec::new()
        .encode(ClientFrame::Challenge(body.clone()), &mut buf)
        .unwrap();
    let ServerFrame::Challenge(got) = ServerCodec::new().decode(&mut buf).unwrap().unwrap() else {
        panic!("wrong frame kind")
    };
    assert_eq!(got, body);
}

#[test]
fn client_fetch_decodes_on_server() {
    let body = Fetch {
        version: PROTOCOL_VERSION,
        identity_pubkey: [0x11; 32],
        nonce: [0x22; 32],
        signature: [0x33; 64],
    };
    let mut buf = BytesMut::new();
    ClientCodec::new()
        .encode(ClientFrame::Fetch(body.clone()), &mut buf)
        .unwrap();
    let ServerFrame::Fetch(got) = ServerCodec::new().decode(&mut buf).unwrap().unwrap() else {
        panic!("wrong frame kind")
    };
    assert_eq!(got, body);
}

#[test]
fn client_delete_decodes_on_server() {
    let body = Delete {
        version: PROTOCOL_VERSION,
        identity_pubkey: [0x66; 32],
        nonce: [0x77; 32],
        signature: [0x88; 64],
        deposit_ids: vec![[0x99; 16], [0xAA; 16]],
    };
    let mut buf = BytesMut::new();
    ClientCodec::new()
        .encode(ClientFrame::Delete(body.clone()), &mut buf)
        .unwrap();
    let ServerFrame::Delete(got) = ServerCodec::new().decode(&mut buf).unwrap().unwrap() else {
        panic!("wrong frame kind")
    };
    assert_eq!(got, body);
}

// === S → C frames (server encode → client decode) =====================

#[test]
fn server_deposit_ok_decodes_on_client() {
    let body = DepositOk { deposit_id: [0x42; 16], expires_at: 1_700_000_000 };
    let mut buf = BytesMut::new();
    ServerCodec::new()
        .encode(ServerFrame::DepositOk(body.clone()), &mut buf)
        .unwrap();
    let ClientFrame::DepositOk(got) = ClientCodec::new().decode(&mut buf).unwrap().unwrap() else {
        panic!("wrong frame kind")
    };
    assert_eq!(got, body);
}

#[test]
fn server_challenge_nonce_decodes_on_client() {
    let body = ChallengeNonce { nonce: [0x55; 32], issued_at: 1_700_000_000 };
    let mut buf = BytesMut::new();
    ServerCodec::new()
        .encode(ServerFrame::ChallengeNonce(body.clone()), &mut buf)
        .unwrap();
    let ClientFrame::ChallengeNonce(got) = ClientCodec::new().decode(&mut buf).unwrap().unwrap()
    else {
        panic!("wrong frame kind")
    };
    assert_eq!(got, body);
}

#[test]
fn server_fetch_response_decodes_on_client() {
    let body = FetchResponse {
        deposits: vec![PendingDeposit {
            deposit_id: [0x44; 16],
            ciphertext: vec![9, 9, 9],
            received_at: 1_700_000_000,
        }],
    };
    let mut buf = BytesMut::new();
    ServerCodec::new()
        .encode(ServerFrame::FetchResponse(body.clone()), &mut buf)
        .unwrap();
    let ClientFrame::FetchResponse(got) = ClientCodec::new().decode(&mut buf).unwrap().unwrap()
    else {
        panic!("wrong frame kind")
    };
    assert_eq!(got, body);
}

#[test]
fn server_delete_ok_decodes_on_client() {
    let body = DeleteOk { deleted: 3, not_found: 1 };
    let mut buf = BytesMut::new();
    ServerCodec::new()
        .encode(ServerFrame::DeleteOk(body.clone()), &mut buf)
        .unwrap();
    let ClientFrame::DeleteOk(got) = ClientCodec::new().decode(&mut buf).unwrap().unwrap() else {
        panic!("wrong frame kind")
    };
    assert_eq!(got, body);
}

#[test]
fn server_error_decodes_on_client() {
    let body = ErrorBody { code: ErrorCode::RateLimited, message: "slow down".into() };
    let mut buf = BytesMut::new();
    ServerCodec::new()
        .encode(ServerFrame::Error(body.clone()), &mut buf)
        .unwrap();
    let ClientFrame::Error(got) = ClientCodec::new().decode(&mut buf).unwrap().unwrap() else {
        panic!("wrong frame kind")
    };
    assert_eq!(got, body);
}

// === Type-byte parity ==================================================

#[test]
fn every_frame_type_byte_matches_server() {
    assert_eq!(ClientFrameKind::Deposit as u8, ServerFrameKind::Deposit as u8);
    assert_eq!(ClientFrameKind::DepositOk as u8, ServerFrameKind::DepositOk as u8);
    assert_eq!(ClientFrameKind::Challenge as u8, ServerFrameKind::Challenge as u8);
    assert_eq!(ClientFrameKind::ChallengeNonce as u8, ServerFrameKind::ChallengeNonce as u8);
    assert_eq!(ClientFrameKind::Fetch as u8, ServerFrameKind::Fetch as u8);
    assert_eq!(ClientFrameKind::FetchResponse as u8, ServerFrameKind::FetchResponse as u8);
    assert_eq!(ClientFrameKind::Delete as u8, ServerFrameKind::Delete as u8);
    assert_eq!(ClientFrameKind::DeleteOk as u8, ServerFrameKind::DeleteOk as u8);
    assert_eq!(ClientFrameKind::Error as u8, ServerFrameKind::Error as u8);
}
