// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Mailbox frame codec — client-side mirror of `skattr-mailbox::codec`.
//!
//! Wire layout matches `core::transport::frame`:
//!
//! ```text
//! +----------------+----------+------------------+
//! | length (u32 BE)| type (u8)|  CBOR payload    |
//! +----------------+----------+------------------+
//! ```
//!
//! Type bytes are disjoint from peer-to-peer frames (0x82–0x8F). The
//! decoder rejects unknown types with [`crate::error::CoreError::Frame`].
//!
//! This is `pub(crate)` — the daemon/IPC layer is the stable boundary.
//!
// TODO(Task 8): retarget error mappings to MailboxClientErrorKind once Task 7 lands it.

use bytes::{BufMut as _, BytesMut};
use serde::{Deserialize, Serialize};
use tokio_util::codec::{Decoder, Encoder};

use crate::error::CoreError;
use crate::mailbox::protocol::{
    Challenge, ChallengeNonce, Delete, DeleteOk, Deposit, DepositOk, ErrorBody, Fetch,
    FetchResponse,
};

/// Hard cap on a single mailbox frame. Matches
/// `core::transport::frame::MAX_FRAME_SIZE`.
pub(crate) const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// Type byte for every wire frame.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MailboxFrameKind {
    /// 0x82 — `Deposit` (C→S).
    Deposit = 0x82,
    /// 0x83 — `DepositOk` (S→C).
    DepositOk = 0x83,
    /// 0x84 — `Challenge` (C→S).
    Challenge = 0x84,
    /// 0x85 — `ChallengeNonce` (S→C).
    ChallengeNonce = 0x85,
    /// 0x86 — `Fetch` (C→S).
    Fetch = 0x86,
    /// 0x87 — `FetchResponse` (S→C).
    FetchResponse = 0x87,
    /// 0x88 — `Delete` (C→S).
    Delete = 0x88,
    /// 0x89 — `DeleteOk` (S→C).
    DeleteOk = 0x89,
    /// 0x8F — `ErrorBody` (S→C).
    Error = 0x8F,
}

/// One fully-parsed mailbox frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MailboxFrame {
    /// Client → server.
    Deposit(Deposit),
    /// Server → client.
    DepositOk(DepositOk),
    /// Client → server.
    Challenge(Challenge),
    /// Server → client.
    ChallengeNonce(ChallengeNonce),
    /// Client → server.
    Fetch(Fetch),
    /// Server → client.
    FetchResponse(FetchResponse),
    /// Client → server.
    Delete(Delete),
    /// Server → client.
    DeleteOk(DeleteOk),
    /// Server → client typed error.
    Error(ErrorBody),
}

impl MailboxFrame {
    /// Wire-format type byte for this frame.
    #[must_use]
    pub(crate) fn kind(&self) -> MailboxFrameKind {
        match self {
            MailboxFrame::Deposit(_) => MailboxFrameKind::Deposit,
            MailboxFrame::DepositOk(_) => MailboxFrameKind::DepositOk,
            MailboxFrame::Challenge(_) => MailboxFrameKind::Challenge,
            MailboxFrame::ChallengeNonce(_) => MailboxFrameKind::ChallengeNonce,
            MailboxFrame::Fetch(_) => MailboxFrameKind::Fetch,
            MailboxFrame::FetchResponse(_) => MailboxFrameKind::FetchResponse,
            MailboxFrame::Delete(_) => MailboxFrameKind::Delete,
            MailboxFrame::DeleteOk(_) => MailboxFrameKind::DeleteOk,
            MailboxFrame::Error(_) => MailboxFrameKind::Error,
        }
    }
}

/// `tokio_util::codec::{Decoder, Encoder}` for [`MailboxFrame`].
#[derive(Debug, Default)]
pub(crate) struct MailboxFrameCodec {
    _private: (),
}

impl MailboxFrameCodec {
    /// Construct a codec.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

fn encode_cbor<T: Serialize>(value: &T) -> Result<Vec<u8>, CoreError> {
    let mut out = Vec::new();
    // TODO(Task 8): retarget to MailboxClientErrorKind once Task 7 lands it.
    ciborium::into_writer(value, &mut out)
        .map_err(|e| CoreError::CborEncode(format!("mailbox codec: {e}")))?;
    Ok(out)
}

fn decode_cbor<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, CoreError> {
    // TODO(Task 8): retarget to MailboxClientErrorKind once Task 7 lands it.
    ciborium::from_reader(bytes).map_err(|e| CoreError::CborDecode(format!("mailbox codec: {e}")))
}

impl Encoder<MailboxFrame> for MailboxFrameCodec {
    type Error = CoreError;

    fn encode(&mut self, item: MailboxFrame, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let (type_byte, payload) = match item {
            MailboxFrame::Deposit(b) => (MailboxFrameKind::Deposit as u8, encode_cbor(&b)?),
            MailboxFrame::DepositOk(b) => (MailboxFrameKind::DepositOk as u8, encode_cbor(&b)?),
            MailboxFrame::Challenge(b) => (MailboxFrameKind::Challenge as u8, encode_cbor(&b)?),
            MailboxFrame::ChallengeNonce(b) => {
                (MailboxFrameKind::ChallengeNonce as u8, encode_cbor(&b)?)
            }
            MailboxFrame::Fetch(b) => (MailboxFrameKind::Fetch as u8, encode_cbor(&b)?),
            MailboxFrame::FetchResponse(b) => {
                (MailboxFrameKind::FetchResponse as u8, encode_cbor(&b)?)
            }
            MailboxFrame::Delete(b) => (MailboxFrameKind::Delete as u8, encode_cbor(&b)?),
            MailboxFrame::DeleteOk(b) => (MailboxFrameKind::DeleteOk as u8, encode_cbor(&b)?),
            MailboxFrame::Error(b) => (MailboxFrameKind::Error as u8, encode_cbor(&b)?),
        };

        let length = 1 + payload.len();
        if length > MAX_FRAME_SIZE {
            return Err(CoreError::Frame(format!(
                "mailbox codec: frame too large: {length} bytes"
            )));
        }

        dst.reserve(4 + length);
        let length_u32 = u32::try_from(length).map_err(|_| {
            CoreError::Frame(format!("mailbox codec: length overflow: {length}"))
        })?;
        dst.extend_from_slice(&length_u32.to_be_bytes());
        dst.put_u8(type_byte);
        dst.extend_from_slice(&payload);
        Ok(())
    }
}

impl Decoder for MailboxFrameCodec {
    type Item = MailboxFrame;
    type Error = CoreError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<MailboxFrame>, Self::Error> {
        if src.len() < 4 {
            return Ok(None);
        }
        let mut len_bytes = [0u8; 4];
        len_bytes.copy_from_slice(&src[0..4]);
        let length = u32::from_be_bytes(len_bytes) as usize;

        if length == 0 {
            return Err(CoreError::Frame(
                "mailbox codec: zero-length frame".into(),
            ));
        }
        if length > MAX_FRAME_SIZE {
            return Err(CoreError::Frame(format!(
                "mailbox codec: frame too large: {length} bytes"
            )));
        }
        if src.len() < 4 + length {
            return Ok(None);
        }

        let _ = src.split_to(4);
        let type_byte = src[0];
        let _ = src.split_to(1);
        let payload_len = length - 1;
        let payload = src.split_to(payload_len);

        let frame = match type_byte {
            0x82 => MailboxFrame::Deposit(decode_cbor(&payload)?),
            0x83 => MailboxFrame::DepositOk(decode_cbor(&payload)?),
            0x84 => MailboxFrame::Challenge(decode_cbor(&payload)?),
            0x85 => MailboxFrame::ChallengeNonce(decode_cbor(&payload)?),
            0x86 => MailboxFrame::Fetch(decode_cbor(&payload)?),
            0x87 => MailboxFrame::FetchResponse(decode_cbor(&payload)?),
            0x88 => MailboxFrame::Delete(decode_cbor(&payload)?),
            0x89 => MailboxFrame::DeleteOk(decode_cbor(&payload)?),
            0x8F => MailboxFrame::Error(decode_cbor(&payload)?),
            other => {
                return Err(CoreError::Frame(format!(
                    "mailbox codec: unknown mailbox frame type 0x{other:02X}"
                )));
            }
        };
        Ok(Some(frame))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::mailbox::protocol::{ErrorCode, PROTOCOL_VERSION};

    fn round_trip(f: MailboxFrame) -> MailboxFrame {
        let mut codec = MailboxFrameCodec::new();
        let mut buf = BytesMut::new();
        codec.encode(f, &mut buf).unwrap();
        codec.decode(&mut buf).unwrap().unwrap()
    }

    #[test]
    fn deposit_round_trips() {
        let f = MailboxFrame::Deposit(Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash: [0xAA; 32],
            ciphertext: vec![1, 2, 3, 4],
            ttl_request: 86_400,
        });
        assert_eq!(round_trip(f.clone()), f);
    }

    #[test]
    fn deposit_ok_round_trips() {
        let f = MailboxFrame::DepositOk(DepositOk {
            deposit_id: [0x42; 16],
            expires_at: 1_700_000_000,
        });
        assert_eq!(round_trip(f.clone()), f);
    }

    #[test]
    fn error_round_trips() {
        let f = MailboxFrame::Error(ErrorBody {
            code: ErrorCode::TtlTooLong,
            message: "expires_at too far".into(),
        });
        assert_eq!(round_trip(f.clone()), f);
    }

    #[test]
    fn type_byte_layout() {
        let mut codec = MailboxFrameCodec::new();
        let mut buf = BytesMut::new();
        codec
            .encode(
                MailboxFrame::Challenge(Challenge {
                    version: PROTOCOL_VERSION,
                    identity_hash: [0; 32],
                }),
                &mut buf,
            )
            .unwrap();
        // length_u32 (4) + type byte
        assert_eq!(buf[4], 0x84);
    }

    #[test]
    fn unknown_type_rejected() {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&1u32.to_be_bytes());
        buf.extend_from_slice(&[0x20]);
        let mut codec = MailboxFrameCodec::new();
        let err = codec.decode(&mut buf).expect_err("must reject");
        assert!(matches!(err, CoreError::Frame(_)));
    }

    #[test]
    fn zero_length_rejected() {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&[0, 0, 0, 0]);
        let mut codec = MailboxFrameCodec::new();
        assert!(codec.decode(&mut buf).is_err());
    }

    #[test]
    fn oversized_length_rejected() {
        let mut buf = BytesMut::new();
        let oversized = u32::try_from(MAX_FRAME_SIZE + 1).unwrap();
        buf.extend_from_slice(&oversized.to_be_bytes());
        let mut codec = MailboxFrameCodec::new();
        assert!(codec.decode(&mut buf).is_err());
    }

    #[test]
    fn malformed_cbor_rejected() {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&3u32.to_be_bytes());
        buf.extend_from_slice(&[0x82]); // Deposit frame
        buf.extend_from_slice(&[0xFF, 0xFF]); // garbage CBOR
        let mut codec = MailboxFrameCodec::new();
        let err = codec.decode(&mut buf).expect_err("must reject");
        assert!(matches!(err, CoreError::CborDecode(_)));
    }

    #[test]
    fn partial_length_returns_none() {
        let mut buf = BytesMut::from(&[0u8, 0, 0][..]);
        let mut codec = MailboxFrameCodec::new();
        assert!(codec.decode(&mut buf).unwrap().is_none());
        assert_eq!(buf.len(), 3);
    }

    #[test]
    fn partial_payload_returns_none() {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&5u32.to_be_bytes());
        buf.extend_from_slice(&[0x82, 0xA0, 0x00]); // 3 of 4 expected payload bytes
        let mut codec = MailboxFrameCodec::new();
        assert!(codec.decode(&mut buf).unwrap().is_none());
    }
}
