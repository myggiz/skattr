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

use tokio_util::codec::{Decoder, Encoder};

use crate::error::{CoreError, Result};

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

    fn decode(&mut self, _src: &mut bytes::BytesMut) -> Result<Option<Frame>> {
        todo!("parse u32 BE length, check MAX_FRAME_SIZE, switch on type byte")
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
            _ => todo!("Error variant lands in Task 4"),
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
}
