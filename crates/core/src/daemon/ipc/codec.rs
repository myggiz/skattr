// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic))]

//! Length-prefix + CBOR codec for IPC frames.
//!
//! Frame layout: `u32_be(body_len) || cbor(body)`. `body_len` is
//! capped by [`crate::daemon::ipc::wire::MAX_IPC_BODY`].

use std::io;

use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::daemon::ipc::wire::MAX_IPC_BODY;

/// Codec-layer errors. Higher layers map these to [`crate::daemon::ipc::wire::IpcError`].
#[derive(Debug, Error)]
pub enum CodecError {
    /// I/O failure during read/write.
    #[error("codec: io: {0}")]
    Io(#[from] io::Error),
    /// Declared length exceeded `MAX_IPC_BODY`.
    #[error("codec: frame too large ({got} bytes, max {max})")]
    FrameTooLarge {
        /// Declared length.
        got: u32,
        /// Maximum accepted length.
        max: u32,
    },
    /// Declared length was zero (protocol error — an empty CBOR body
    /// is not allowed; callers must send at least the smallest
    /// variant's encoding).
    #[error("codec: empty frame")]
    EmptyFrame,
    /// CBOR serialize/deserialize failure.
    #[error("codec: cbor: {0}")]
    Cbor(String),
}

/// Encode `value` as CBOR and write it as a length-prefixed frame.
pub async fn write_frame<W, T>(w: &mut W, value: &T) -> Result<(), CodecError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut body: Vec<u8> = Vec::new();
    ciborium::ser::into_writer(value, &mut body).map_err(|e| CodecError::Cbor(e.to_string()))?;
    let len = u32::try_from(body.len()).map_err(|_| CodecError::FrameTooLarge {
        got: u32::MAX,
        max: MAX_IPC_BODY,
    })?;
    if len > MAX_IPC_BODY {
        return Err(CodecError::FrameTooLarge {
            got: len,
            max: MAX_IPC_BODY,
        });
    }
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(&body).await?;
    w.flush().await?;
    Ok(())
}

/// Read one length-prefixed CBOR frame and decode it.
pub async fn read_frame<R, T>(r: &mut R) -> Result<T, CodecError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);
    if len == 0 {
        return Err(CodecError::EmptyFrame);
    }
    if len > MAX_IPC_BODY {
        return Err(CodecError::FrameTooLarge {
            got: len,
            max: MAX_IPC_BODY,
        });
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body).await?;
    ciborium::de::from_reader(&body[..]).map_err(|e| CodecError::Cbor(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::commands::Command;
    use crate::daemon::ipc::wire::{IpcRequest, IpcResponse, MAX_IPC_BODY};

    #[tokio::test]
    async fn roundtrip_request_response() {
        let req = IpcRequest::Execute(Command::ListContacts);

        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &req).await.unwrap();

        let mut reader = &buf[..];
        let back: IpcRequest = read_frame(&mut reader).await.unwrap();
        match back {
            IpcRequest::Execute(Command::ListContacts) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_rejects_oversize_length_prefix() {
        let mut buf: Vec<u8> = Vec::new();
        let bogus_len: u32 = MAX_IPC_BODY + 1;
        buf.extend_from_slice(&bogus_len.to_be_bytes());
        let mut reader = &buf[..];
        let err = read_frame::<_, IpcRequest>(&mut reader).await.unwrap_err();
        assert!(
            matches!(err, CodecError::FrameTooLarge { got, max } if got == bogus_len && max == MAX_IPC_BODY),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn read_rejects_zero_length_frame() {
        let buf: Vec<u8> = 0u32.to_be_bytes().to_vec();
        let mut reader = &buf[..];
        let err = read_frame::<_, IpcRequest>(&mut reader).await.unwrap_err();
        assert!(matches!(err, CodecError::EmptyFrame), "got {err:?}");
    }

    #[tokio::test]
    async fn read_rejects_malformed_cbor() {
        let mut buf: Vec<u8> = Vec::new();
        let bad_body = vec![0xFF_u8; 8];
        let len = u32::try_from(bad_body.len()).unwrap();
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&bad_body);
        let mut reader = &buf[..];
        let err = read_frame::<_, IpcResponse>(&mut reader).await.unwrap_err();
        assert!(matches!(err, CodecError::Cbor(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn write_rejects_oversize_body() {
        let big = "x".repeat((MAX_IPC_BODY + 1) as usize);
        let req = IpcRequest::Execute(Command::AddContact { invite_url: big });
        let mut buf: Vec<u8> = Vec::new();
        let err = write_frame(&mut buf, &req).await.unwrap_err();
        assert!(
            matches!(err, CodecError::FrameTooLarge { .. }),
            "got {err:?}"
        );
    }
}
