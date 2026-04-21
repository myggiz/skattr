// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Length-prefixed framed wire protocol.
//!
//! Layout on the wire:
//!
//! ```text
//! +----------------+----------+------------------+
//! | length (u32 BE)| type (u8)|     payload      |
//! +----------------+----------+------------------+
//! ```
//!
//! `length` covers `type + payload`. Maximum frame size is 16 MiB;
//! oversized frames are rejected and close the connection.

use serde::{Deserialize, Serialize};
use tokio_util::codec::{Decoder, Encoder};

use crate::error::{CoreError, Result};

#[derive(Debug, Serialize, Deserialize)]
struct ErrorPayload {
    code: u16,
    message: String,
}

/// Hard cap on frame size (16 MiB).
pub const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// Frame type byte, corresponds one-to-one with [`Frame`] variants.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    /// First Noise handshake message (initiator).
    NoiseInit = 0x01,
    /// Noise handshake response.
    NoiseResp = 0x02,
    /// MLS Welcome message.
    MlsWelcome = 0x03,
    /// MLS Commit (ratchet / membership change).
    MlsCommit = 0x04,
    /// MLS application message (the common case).
    MlsApp = 0x05,
    /// Delivery ACK carrying a message id.
    Ack = 0x06,
    /// Keepalive ping.
    Ping = 0x07,
    /// Keepalive pong.
    Pong = 0x08,
    /// Graceful close.
    Bye = 0x09,
    /// Typed error; body is CBOR `{ code, message }`.
    Error = 0x0A,
}

/// A fully-parsed frame.
#[derive(Debug, Clone)]
pub enum Frame {
    /// Noise handshake initiator.
    NoiseInit(Vec<u8>),
    /// Noise handshake responder.
    NoiseResp(Vec<u8>),
    /// MLS Welcome blob.
    MlsWelcome(Vec<u8>),
    /// MLS Commit blob.
    MlsCommit(Vec<u8>),
    /// MLS application payload.
    MlsApp(Vec<u8>),
    /// Delivery ACK for a 16-byte message id.
    Ack([u8; 16]),
    /// Ping.
    Ping,
    /// Pong.
    Pong,
    /// Graceful close.
    Bye,
    /// Typed error.
    Error {
        /// Numeric error code.
        code: u16,
        /// Human-readable description (must be non-sensitive).
        message: String,
    },
}

/// `tokio_util::codec` encoder/decoder for [`Frame`]s.
#[derive(Debug, Default)]
pub struct FrameCodec {
    _private: (),
}

impl FrameCodec {
    /// Construct a codec.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Decoder for FrameCodec {
    type Item = Frame;
    type Error = CoreError;

    fn decode(&mut self, src: &mut bytes::BytesMut) -> Result<Option<Frame>> {
        if src.len() < 4 {
            return Ok(None);
        }

        let mut len_bytes = [0u8; 4];
        len_bytes.copy_from_slice(&src[0..4]);
        let length = u32::from_be_bytes(len_bytes) as usize;

        if length == 0 {
            return Err(CoreError::Frame("zero-length frame".into()));
        }
        if length > MAX_FRAME_SIZE {
            return Err(CoreError::Frame(format!(
                "frame too large: {length} bytes"
            )));
        }
        if src.len() < 4 + length {
            return Ok(None);
        }

        // Consume length prefix.
        let _ = src.split_to(4);
        // Consume type byte.
        let type_byte = src[0];
        let _ = src.split_to(1);
        let payload_len = length - 1;
        let payload = src.split_to(payload_len);

        let frame = match type_byte {
            0x01 => Frame::NoiseInit(payload.to_vec()),
            0x02 => Frame::NoiseResp(payload.to_vec()),
            0x03 => Frame::MlsWelcome(payload.to_vec()),
            0x04 => Frame::MlsCommit(payload.to_vec()),
            0x05 => Frame::MlsApp(payload.to_vec()),
            0x06 => {
                if payload.len() != 16 {
                    return Err(CoreError::Frame(format!(
                        "Ack payload must be 16 bytes, got {}",
                        payload.len()
                    )));
                }
                let mut id = [0u8; 16];
                id.copy_from_slice(&payload);
                Frame::Ack(id)
            }
            0x07 => {
                if !payload.is_empty() {
                    return Err(CoreError::Frame(
                        "Ping must have empty payload".into(),
                    ));
                }
                Frame::Ping
            }
            0x08 => {
                if !payload.is_empty() {
                    return Err(CoreError::Frame(
                        "Pong must have empty payload".into(),
                    ));
                }
                Frame::Pong
            }
            0x09 => {
                if !payload.is_empty() {
                    return Err(CoreError::Frame("Bye must have empty payload".into()));
                }
                Frame::Bye
            }
            other => {
                return Err(CoreError::Frame(format!(
                    "unknown or not-yet-handled frame type 0x{other:02X}"
                )));
            }
        };

        Ok(Some(frame))
    }
}

impl Encoder<Frame> for FrameCodec {
    type Error = CoreError;

    fn encode(&mut self, item: Frame, dst: &mut bytes::BytesMut) -> Result<()> {
        use bytes::BufMut as _;

        let (type_byte, payload): (u8, Vec<u8>) = match item {
            Frame::Ping => (0x07, Vec::new()),
            Frame::Pong => (0x08, Vec::new()),
            Frame::Bye => (0x09, Vec::new()),
            Frame::Ack(id) => (0x06, id.to_vec()),
            Frame::NoiseInit(p) => (0x01, p),
            Frame::NoiseResp(p) => (0x02, p),
            Frame::MlsWelcome(p) => (0x03, p),
            Frame::MlsCommit(p) => (0x04, p),
            Frame::MlsApp(p) => (0x05, p),
            Frame::Error { code, message } => {
                let mut buf = Vec::new();
                ciborium::into_writer(&ErrorPayload { code, message }, &mut buf)
                    .map_err(|e| CoreError::Frame(format!("encode Error: {e}")))?;
                (0x0A, buf)
            }
        };

        let length = 1 + payload.len();
        if length > MAX_FRAME_SIZE {
            return Err(CoreError::Frame(format!(
                "encoded frame too large: {length} bytes"
            )));
        }

        dst.reserve(4 + length);
        dst.extend_from_slice(&u32::try_from(length).unwrap().to_be_bytes());
        dst.put_u8(type_byte);
        dst.extend_from_slice(&payload);
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use bytes::BytesMut;
    use tokio_util::codec::{Decoder, Encoder};

    fn enc(frame: Frame) -> BytesMut {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::new();
        codec.encode(frame, &mut buf).unwrap();
        buf
    }

    #[test]
    fn encode_ping_is_length1_type07() {
        let buf = enc(Frame::Ping);
        assert_eq!(&buf[..], &[0, 0, 0, 1, 0x07]);
    }

    #[test]
    fn encode_pong_is_length1_type08() {
        assert_eq!(&enc(Frame::Pong)[..], &[0, 0, 0, 1, 0x08]);
    }

    #[test]
    fn encode_bye_is_length1_type09() {
        assert_eq!(&enc(Frame::Bye)[..], &[0, 0, 0, 1, 0x09]);
    }

    #[test]
    fn encode_ack_is_length17_type06_plus_16_bytes() {
        let id = [0xAB; 16];
        let buf = enc(Frame::Ack(id));
        // 4-byte length (17) + 1 type byte + 16 payload = 21 bytes total
        assert_eq!(buf.len(), 21);
        assert_eq!(&buf[..4], &[0, 0, 0, 17]);
        assert_eq!(buf[4], 0x06);
        assert_eq!(&buf[5..], &id);
    }

    #[test]
    fn encode_noise_init_wraps_payload_with_length_and_type01() {
        let payload = b"hello-noise".to_vec();
        let buf = enc(Frame::NoiseInit(payload.clone()));
        let length = 1 + payload.len();
        let expected_len_bytes = u32::try_from(length).unwrap().to_be_bytes();
        assert_eq!(&buf[..4], expected_len_bytes);
        assert_eq!(buf[4], 0x01);
        assert_eq!(&buf[5..], &payload[..]);
    }

    #[test]
    fn encode_noise_resp_uses_type02() {
        let buf = enc(Frame::NoiseResp(vec![0xAA, 0xBB]));
        assert_eq!(&buf[..], &[0, 0, 0, 3, 0x02, 0xAA, 0xBB]);
    }

    #[test]
    fn encode_mls_welcome_uses_type03() {
        let buf = enc(Frame::MlsWelcome(vec![0x11]));
        assert_eq!(&buf[..], &[0, 0, 0, 2, 0x03, 0x11]);
    }

    #[test]
    fn encode_mls_commit_uses_type04() {
        let buf = enc(Frame::MlsCommit(vec![0x22]));
        assert_eq!(&buf[..], &[0, 0, 0, 2, 0x04, 0x22]);
    }

    #[test]
    fn encode_mls_app_uses_type05() {
        let buf = enc(Frame::MlsApp(vec![0x33]));
        assert_eq!(&buf[..], &[0, 0, 0, 2, 0x05, 0x33]);
    }

    #[test]
    fn encode_error_uses_type0a_and_cbor_payload() {
        let buf = enc(Frame::Error {
            code: 42,
            message: "bad".into(),
        });
        // Decode the payload back with ciborium to confirm it's valid CBOR
        // and matches what we passed in.
        let payload = &buf[5..]; // skip 4-byte length + 1 type byte
        let decoded: ErrorPayload =
            ciborium::from_reader(payload).expect("payload must be valid CBOR");
        assert_eq!(decoded.code, 42);
        assert_eq!(decoded.message, "bad");
        assert_eq!(buf[4], 0x0A); // type byte
    }

    #[test]
    fn encode_rejects_oversized_payload() {
        // MAX_FRAME_SIZE - 1 bytes fit; MAX_FRAME_SIZE bytes as payload
        // push length (1 + MAX_FRAME_SIZE) past MAX_FRAME_SIZE.
        let too_big = vec![0u8; MAX_FRAME_SIZE];
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::new();
        let err = codec
            .encode(Frame::MlsApp(too_big), &mut buf)
            .expect_err("oversize must error");
        assert!(matches!(err, CoreError::Frame(_)));
    }

    fn round_trip(f: Frame) -> Frame {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::new();
        codec.encode(f, &mut buf).unwrap();
        codec.decode(&mut buf).unwrap().unwrap()
    }

    #[test]
    fn decode_ping_round_trips() {
        assert!(matches!(round_trip(Frame::Ping), Frame::Ping));
    }

    #[test]
    fn decode_pong_round_trips() {
        assert!(matches!(round_trip(Frame::Pong), Frame::Pong));
    }

    #[test]
    fn decode_bye_round_trips() {
        assert!(matches!(round_trip(Frame::Bye), Frame::Bye));
    }

    #[test]
    fn decode_ack_round_trips() {
        let id = [0xCD; 16];
        match round_trip(Frame::Ack(id)) {
            Frame::Ack(got) => assert_eq!(got, id),
            other => panic!("expected Ack, got {other:?}"),
        }
    }

    #[test]
    fn decode_ack_rejects_wrong_payload_length() {
        // Hand-craft bytes: length = 9 (1 type + 8 payload), type 0x06, 8 random bytes.
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&9u32.to_be_bytes());
        buf.extend_from_slice(&[0x06]);
        buf.extend_from_slice(&[0; 8]);
        let mut codec = FrameCodec::new();
        let err = codec.decode(&mut buf).expect_err("must reject wrong-size ack");
        assert!(matches!(err, CoreError::Frame(_)));
    }

    #[test]
    fn decode_noise_init_round_trips() {
        let payload = b"init-payload".to_vec();
        match round_trip(Frame::NoiseInit(payload.clone())) {
            Frame::NoiseInit(got) => assert_eq!(got, payload),
            other => panic!("expected NoiseInit, got {other:?}"),
        }
    }

    #[test]
    fn decode_noise_resp_round_trips() {
        let payload = b"resp-payload".to_vec();
        match round_trip(Frame::NoiseResp(payload.clone())) {
            Frame::NoiseResp(got) => assert_eq!(got, payload),
            other => panic!("expected NoiseResp, got {other:?}"),
        }
    }

    #[test]
    fn decode_mls_welcome_round_trips() {
        let payload = vec![0x11, 0x22, 0x33];
        match round_trip(Frame::MlsWelcome(payload.clone())) {
            Frame::MlsWelcome(got) => assert_eq!(got, payload),
            other => panic!("expected MlsWelcome, got {other:?}"),
        }
    }

    #[test]
    fn decode_mls_commit_round_trips() {
        let payload = vec![0xAA, 0xBB];
        match round_trip(Frame::MlsCommit(payload.clone())) {
            Frame::MlsCommit(got) => assert_eq!(got, payload),
            other => panic!("expected MlsCommit, got {other:?}"),
        }
    }

    #[test]
    fn decode_mls_app_round_trips() {
        let payload = vec![0x01; 1000];
        match round_trip(Frame::MlsApp(payload.clone())) {
            Frame::MlsApp(got) => assert_eq!(got, payload),
            other => panic!("expected MlsApp, got {other:?}"),
        }
    }
}
