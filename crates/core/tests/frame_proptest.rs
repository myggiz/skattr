// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Property-based round-trip test for the frame codec.
//!
//! Hits the full encode → decode → encode path with 10 000 generated
//! frames per run. Payload sizes are capped at 64 KiB so the test
//! stays fast; the oversize path is covered by unit tests in
//! `transport::frame::tests`.

#![cfg(feature = "test-harness")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use bytes::BytesMut;
use proptest::prelude::*;
use skattr_core::test_exports::{Frame, FrameCodec};
use tokio_util::codec::{Decoder, Encoder};

fn arb_frame() -> impl Strategy<Value = Frame> {
    prop_oneof![
        prop::collection::vec(any::<u8>(), 0..65536).prop_map(Frame::NoiseInit),
        prop::collection::vec(any::<u8>(), 0..65536).prop_map(Frame::NoiseResp),
        prop::collection::vec(any::<u8>(), 0..65536).prop_map(Frame::MlsWelcome),
        prop::collection::vec(any::<u8>(), 0..65536).prop_map(Frame::MlsCommit),
        prop::collection::vec(any::<u8>(), 0..65536).prop_map(Frame::MlsApp),
        any::<[u8; 16]>().prop_map(Frame::Ack),
        Just(Frame::Ping),
        Just(Frame::Pong),
        Just(Frame::Bye),
        (any::<u16>(), "\\PC{0,256}").prop_map(|(code, message)| Frame::Error { code, message }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    #[test]
    fn encode_decode_round_trip(f in arb_frame()) {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::new();
        codec.encode(f.clone(), &mut buf).unwrap();
        let decoded = codec.decode(&mut buf).unwrap().expect("frame decoded");
        // Frame does not derive PartialEq (opaque Vec<u8> payloads); Debug
        // comparison is sufficient for structural equality.
        prop_assert_eq!(format!("{f:?}"), format!("{decoded:?}"));
        prop_assert!(buf.is_empty(), "codec must consume exactly one frame");
    }
}
