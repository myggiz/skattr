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

    fn encode(&mut self, _item: Frame, _dst: &mut bytes::BytesMut) -> Result<()> {
        todo!("write u32 BE length prefix, type byte, payload")
    }
}
