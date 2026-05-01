// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Client side of the v1 mailbox protocol.
//!
//! `MailboxClient` wraps a long-lived `Framed<S, MailboxFrameCodec>`
//! stream. Production callers go through `MailboxClient::connect`
//! (Task 12) which drives the Arti dial; tests use `from_stream` to
//! pass a `tokio::io::DuplexStream` directly.

use futures::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::Framed;

use crate::error::{CoreError, MailboxClientErrorKind, Result};
use crate::mailbox::codec::{MailboxFrame, MailboxFrameCodec};
use crate::mailbox::protocol::{Challenge, ChallengeNonce, ErrorBody, ErrorCode, PROTOCOL_VERSION};

/// Single-mailbox client over a long-lived framed stream.
pub(crate) struct MailboxClient<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    onion: String,
    framed: Framed<S, MailboxFrameCodec>,
}

impl<S> MailboxClient<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    /// Wrap an already-connected stream.
    pub fn from_stream(onion: String, stream: S) -> Self {
        Self {
            onion,
            framed: Framed::new(stream, MailboxFrameCodec::new()),
        }
    }

    /// Onion this client is bound to.
    #[must_use]
    pub fn onion(&self) -> &str {
        &self.onion
    }

    /// Single Challenge round-trip — used by AddMailbox liveness check.
    pub async fn probe(&mut self, identity_hash: [u8; 32]) -> Result<()> {
        self.framed
            .send(MailboxFrame::Challenge(Challenge {
                version: PROTOCOL_VERSION,
                identity_hash,
            }))
            .await
            .map_err(|_| CoreError::MailboxClient(MailboxClientErrorKind::Unreachable))?;
        match self.framed.next().await {
            Some(Ok(MailboxFrame::ChallengeNonce(_))) => Ok(()),
            Some(Ok(MailboxFrame::Error(ErrorBody { code, .. }))) => {
                Err(CoreError::MailboxClient(map_error(code)))
            }
            Some(Ok(_)) => Err(CoreError::MailboxClient(MailboxClientErrorKind::Malformed)),
            Some(Err(_)) | None => {
                Err(CoreError::MailboxClient(MailboxClientErrorKind::Unreachable))
            }
        }
    }
}

/// Map a wire `ErrorCode` into our typed `MailboxClientErrorKind`.
/// `pub(crate)` so subsequent tasks (deposit/fetch/delete) can reuse it.
pub(crate) fn map_error(code: ErrorCode) -> MailboxClientErrorKind {
    use MailboxClientErrorKind as E;
    match code {
        ErrorCode::UnsupportedVersion => E::UnsupportedVersion,
        ErrorCode::RateLimited => E::RateLimited,
        ErrorCode::RecipientFull => E::RecipientFull,
        ErrorCode::InvalidSignature => E::InvalidSignature,
        ErrorCode::NonceExpired => E::NonceExpired,
        ErrorCode::HashMismatch => E::HashMismatch,
        ErrorCode::MalformedRequest => E::Malformed,
        ErrorCode::TooLarge
        | ErrorCode::TtlTooLong
        | ErrorCode::TtlTooShort
        | ErrorCode::NotFound
        | ErrorCode::Internal => E::Other(format!("server error: {code:?}")),
    }
}

/// Helper used by tests + later tasks (deposit recipient hashing).
pub(crate) fn recipient_hash_from_pubkey(pk: &[u8; 32]) -> [u8; 32] {
    Sha256::digest(pk).into()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::mailbox::protocol::ChallengeNonce;
    use futures::SinkExt;
    use tokio::io::duplex;
    use tokio_util::codec::Framed;

    /// Spawn a tiny inline server on the duplex peer that responds to
    /// one Challenge with a fixed nonce.
    async fn inline_challenge_server(server: tokio::io::DuplexStream) {
        let mut framed = Framed::new(server, MailboxFrameCodec::new());
        if let Some(Ok(MailboxFrame::Challenge(_))) = framed.next().await {
            framed
                .send(MailboxFrame::ChallengeNonce(ChallengeNonce {
                    nonce: [0x55; 32],
                    issued_at: 1,
                }))
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn probe_succeeds_on_challenge_nonce() {
        let (a, b) = duplex(64 * 1024);
        let server = tokio::spawn(inline_challenge_server(b));
        let mut client = MailboxClient::from_stream("aaaa.onion".into(), a);
        client.probe([0xCD; 32]).await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn probe_returns_rate_limited_on_error() {
        let (a, b) = duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let mut framed = Framed::new(b, MailboxFrameCodec::new());
            let _ = framed.next().await;
            framed
                .send(MailboxFrame::Error(ErrorBody {
                    code: ErrorCode::RateLimited,
                    message: "slow down".into(),
                }))
                .await
                .unwrap();
        });
        let mut client = MailboxClient::from_stream("a.onion".into(), a);
        let err = client.probe([0; 32]).await.unwrap_err();
        assert!(matches!(
            err,
            CoreError::MailboxClient(MailboxClientErrorKind::RateLimited)
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn probe_unreachable_on_eof() {
        let (a, b) = duplex(64);
        drop(b);
        let mut client = MailboxClient::from_stream("a.onion".into(), a);
        let err = client.probe([0; 32]).await.unwrap_err();
        assert!(matches!(
            err,
            CoreError::MailboxClient(MailboxClientErrorKind::Unreachable)
        ));
    }
}
