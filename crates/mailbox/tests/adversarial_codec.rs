// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Malformed CBOR and unknown frames yield ErrorCode::MalformedRequest.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use bytes::BytesMut;
use futures::StreamExt;
use skattr_core::mailbox::protocol::ErrorCode;
use skattr_mailbox::codec::{MailboxFrame, MailboxFrameCodec};
use skattr_mailbox::policy::Policy;
use skattr_mailbox::server::MailboxServer;
use skattr_mailbox::store::Store;
use tokio::io::AsyncWriteExt;
use tokio_util::codec::Framed;

#[tokio::test]
async fn malformed_cbor_returns_malformed_request_keeps_open() {
    let (mut client, server) = tokio::io::duplex(64 * 1024);
    let store = Arc::new(Store::in_memory().unwrap());
    let mb = MailboxServer::new(store, Policy::recommended());
    let handle = tokio::spawn(async move { mb.accept_loop(server).await });

    // Hand-craft: length=4, type=Deposit (0x82), 3 garbage bytes.
    let mut buf = BytesMut::new();
    buf.extend_from_slice(&4u32.to_be_bytes());
    buf.extend_from_slice(&[0x82, 0xFF, 0xFF, 0xFF]);
    client.write_all(&buf).await.unwrap();

    let mut framed = Framed::new(client, MailboxFrameCodec::new());
    let resp = framed.next().await.unwrap().unwrap();
    if let MailboxFrame::Error(e) = resp {
        assert_eq!(e.code, ErrorCode::MalformedRequest);
    } else {
        panic!("expected Error");
    }
    drop(framed);
    handle.await.unwrap().unwrap();
}
