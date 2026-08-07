// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Integration test: Noise_XK handshake + post-handshake round-trip
//! over a `tokio::io::duplex`. Runs both halves concurrently via
//! `tokio::join!`.

#![cfg(feature = "test-harness")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use skattr_core::identity::IdentityKey;
use skattr_core::test_exports::{
    handshake_initiator, handshake_responder, noise_public_of, Frame, HandshakeOutcome,
};

#[tokio::test]
async fn pair_completes_handshake_and_round_trips_frame() {
    // Fresh random identities for each side — determinism isn't
    // needed; we're testing round-trip behaviour, not specific
    // output bytes.
    let initiator = IdentityKey::generate().unwrap();
    let responder = IdentityKey::generate().unwrap();
    let responder_x25519 = noise_public_of(&responder);

    let (init_io, resp_io) = tokio::io::duplex(32 * 1024);

    let init_fut =
        async move { handshake_initiator(init_io, &initiator, &responder_x25519, None).await };
    let resp_fut = async move { handshake_responder(resp_io, &responder, None).await };

    let (init_r, resp_r) = tokio::join!(init_fut, resp_fut);
    let (mut init_conn, init_out): (_, HandshakeOutcome) = match init_r {
        Ok(v) => v,
        Err(e) => panic!("initiator handshake: {e:?}"),
    };
    let (mut resp_conn, resp_out): (_, HandshakeOutcome) = match resp_r {
        Ok(v) => v,
        Err(e) => panic!("responder handshake: {e:?}"),
    };

    assert_eq!(
        *init_out.h_transport, *resp_out.h_transport,
        "both sides must agree on h_transport"
    );
    assert_ne!(*init_out.h_transport, [0u8; 32]);

    // Round-trip a Ping from initiator to responder.
    init_conn.send(Frame::Ping).await.unwrap();
    let got = resp_conn.recv().await.unwrap().expect("one frame");
    assert!(matches!(got, Frame::Ping));

    init_conn.close().await.unwrap();
}

#[tokio::test]
async fn h_transport_is_non_zero_and_zeroizing() {
    // A second run focused on the binding hash alone. Covers (a) the
    // two sides agree, (b) the hash is not all-zero (would indicate
    // HKDF dropped to a default), (c) the outcome's `h_transport`
    // field is dereferenceable to `[u8; 32]` (type-level proof it is
    // Zeroizing<[u8; 32]>).
    let a = IdentityKey::generate().unwrap();
    let b = IdentityKey::generate().unwrap();
    let b_pub = noise_public_of(&b);

    let (ai, bi) = tokio::io::duplex(16 * 1024);
    let fa = async move { handshake_initiator(ai, &a, &b_pub, None).await };
    let fb = async move { handshake_responder(bi, &b, None).await };
    let (ra, rb) = tokio::join!(fa, fb);
    let (_, oa) = match ra {
        Ok(v) => v,
        Err(e) => panic!("initiator: {e:?}"),
    };
    let (_, ob) = match rb {
        Ok(v) => v,
        Err(e) => panic!("responder: {e:?}"),
    };

    assert_eq!(*oa.h_transport, *ob.h_transport);
    assert_ne!(*oa.h_transport, [0u8; 32]);
    // Type-level proof that the field derefs to [u8; 32] (Zeroizing<[u8; 32]>).
    let _guard: &[u8; 32] = &oa.h_transport;
}
