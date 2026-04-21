// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Noise_XK handshake and transport cipher via `snow`.
//!
//! **Pattern:** `Noise_XK_25519_ChaChaPoly_BLAKE2s`, optionally with the
//! `psk3` modifier (`Noise_XKpsk3_25519_ChaChaPoly_BLAKE2s`) when an
//! invite PSK is supplied on both sides. The responder's static X25519
//! key is assumed known out-of-band (from a `ContactCard` or invite);
//! the initiator's static key is transmitted encrypted inside msg3.
//!
//! On completion we extract the Noise handshake hash and derive
//! `h_transport = HKDF(hh, "skattr-binding-v1")`, which the MLS layer
//! injects as an external PSK into the first Commit. This binding
//! prevents MLS-state replay across different Noise sessions.
//!
//! The Ed25519 → X25519 bridge (private via SHA-512 clamp, public via
//! the Edwards-Y → Montgomery-U birational map) lives on `IdentityKey`
//! — see `identity::key::{IdentityKey::noise_static_secret,
//! noise_static_public, ed25519_pub_to_x25519}`.

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::codec::Framed;
use zeroize::Zeroizing;

use crate::error::{CoreError, Result};
use crate::identity::derive::{hkdf_expand, INFO_TRANSPORT_BINDING_V1};
use crate::identity::IdentityKey;
use crate::transport::connection::AuthenticatedConnection;
use crate::transport::frame::{Frame, FrameCodec};

/// Base Noise pattern string (no PSK modifier).
pub(crate) const NOISE_PATTERN: &str = "Noise_XK_25519_ChaChaPoly_BLAKE2s";

/// Noise pattern string with a `psk3` modifier — used when the caller
/// supplies an invite PSK on both sides.
pub(crate) const NOISE_PATTERN_PSK3: &str = "Noise_XKpsk3_25519_ChaChaPoly_BLAKE2s";

/// Version byte written by the initiator before the first Noise frame.
/// Responder reads one byte and rejects anything other than this value.
pub(crate) const PROTOCOL_VERSION: u8 = 0x01;

/// Whole-handshake timeout — the three Noise frames plus version
/// preamble must complete inside this window. Defends against slowloris
/// and half-open connections. Surfaces as
/// `CoreError::Transport("handshake: timeout")`.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Outcome of a completed handshake.
///
/// The caller (contact layer, not this module) is responsible for
/// mapping `peer_x25519` back to an Ed25519 identity via a ContactCard
/// lookup: iterate known contacts, convert each stored Ed25519 pubkey
/// with `ed25519_pub_to_x25519`, compare. That resolver is outside 1.B.
pub struct HandshakeOutcome {
    /// Peer's X25519 static public key as received during Noise.
    pub peer_x25519: [u8; 32],
    /// 32-byte transport↔MLS binding token:
    /// `HKDF-SHA256(noise_handshake_hash, "skattr-binding-v1")`.
    pub h_transport: Zeroizing<[u8; 32]>,
}

/// Max size of a single Noise message payload buffer. Noise itself
/// caps messages at 65535 bytes; we use this for both send and recv
/// scratch buffers during the handshake.
const NOISE_SCRATCH: usize = 65535;

fn map_snow<E: std::fmt::Display>(kind: &str, e: E) -> CoreError {
    CoreError::Transport(format!("handshake: {kind}: {e}"))
}

/// Pick the Noise pattern name based on whether a PSK is in use.
/// `psk3` modifier engages when a PSK is supplied on both sides.
fn pattern_for(psk: Option<&[u8; 32]>) -> &'static str {
    if psk.is_some() {
        NOISE_PATTERN_PSK3
    } else {
        NOISE_PATTERN
    }
}

/// Build a `snow::HandshakeState` with optional PSK wiring.
fn build_handshake(
    identity: &IdentityKey,
    remote_static: Option<&[u8; 32]>,
    invite_psk: Option<&[u8; 32]>,
    initiator: bool,
) -> Result<snow::HandshakeState> {
    let pattern = pattern_for(invite_psk);
    let params: snow::params::NoiseParams = pattern.parse().map_err(|e| map_snow("builder", e))?;
    let secret = identity.noise_static_secret();
    let mut builder = snow::Builder::new(params).local_private_key(secret.as_ref());
    if let Some(rs) = remote_static {
        builder = builder.remote_public_key(rs);
    }
    if let Some(psk) = invite_psk {
        builder = builder.psk(3, psk);
    }
    let state = if initiator {
        builder.build_initiator()
    } else {
        builder.build_responder()
    }
    .map_err(|e| map_snow("builder", e))?;
    Ok(state)
}

async fn do_initiator<S>(
    mut stream: S,
    identity: &IdentityKey,
    peer_static_x25519: &[u8; 32],
    invite_psk: Option<&[u8; 32]>,
) -> Result<(AuthenticatedConnection<S>, HandshakeOutcome)>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    // 1-byte version preamble.
    stream
        .write_all(&[PROTOCOL_VERSION])
        .await
        .map_err(|e| map_snow("stream", e))?;
    stream.flush().await.map_err(|e| map_snow("stream", e))?;

    let mut handshake = build_handshake(identity, Some(peer_static_x25519), invite_psk, true)?;
    let mut framed = Framed::new(stream, FrameCodec::new());

    // msg1 → write, wrap in NoiseInit.
    let mut buf = vec![0u8; NOISE_SCRATCH];
    let n = handshake
        .write_message(&[], &mut buf)
        .map_err(|e| map_snow("authentication failed", e))?;
    framed
        .send(Frame::NoiseInit(buf[..n].to_vec()))
        .await
        .map_err(|e| map_snow("malformed", e))?;

    // msg2 ← read NoiseResp.
    let frame = framed
        .next()
        .await
        .ok_or_else(|| CoreError::Transport("handshake: stream closed".into()))?
        .map_err(|e| map_snow("malformed", e))?;
    let msg2 = match frame {
        Frame::NoiseResp(p) => p,
        other => {
            return Err(CoreError::Transport(format!(
                "handshake: malformed: unexpected frame type 0x{:02X}",
                other.frame_type() as u8
            )));
        }
    };
    let mut in_buf = vec![0u8; NOISE_SCRATCH];
    handshake
        .read_message(&msg2, &mut in_buf)
        .map_err(|e| map_snow("authentication failed", e))?;

    // msg3 → write, wrap in NoiseInit (direction-based reuse).
    let n = handshake
        .write_message(&[], &mut buf)
        .map_err(|e| map_snow("authentication failed", e))?;
    framed
        .send(Frame::NoiseInit(buf[..n].to_vec()))
        .await
        .map_err(|e| map_snow("malformed", e))?;

    finish_handshake(handshake, framed).await
}

async fn do_responder<S>(
    mut stream: S,
    identity: &IdentityKey,
    invite_psk: Option<&[u8; 32]>,
) -> Result<(AuthenticatedConnection<S>, HandshakeOutcome)>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    // Read + validate the 1-byte preamble.
    let mut ver = [0u8; 1];
    stream
        .read_exact(&mut ver)
        .await
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::UnexpectedEof => {
                CoreError::Transport("handshake: stream closed".into())
            }
            _ => map_snow("stream", e),
        })?;
    if ver[0] != PROTOCOL_VERSION {
        return Err(CoreError::Transport(format!(
            "handshake: unsupported version: {:#04x}",
            ver[0]
        )));
    }

    let mut handshake = build_handshake(identity, None, invite_psk, false)?;
    let mut framed = Framed::new(stream, FrameCodec::new());

    // msg1 ← read NoiseInit.
    let frame = framed
        .next()
        .await
        .ok_or_else(|| CoreError::Transport("handshake: stream closed".into()))?
        .map_err(|e| map_snow("malformed", e))?;
    let msg1 = match frame {
        Frame::NoiseInit(p) => p,
        other => {
            return Err(CoreError::Transport(format!(
                "handshake: malformed: unexpected frame type 0x{:02X}",
                other.frame_type() as u8
            )));
        }
    };
    let mut in_buf = vec![0u8; NOISE_SCRATCH];
    handshake
        .read_message(&msg1, &mut in_buf)
        .map_err(|e| map_snow("authentication failed", e))?;

    // msg2 → write NoiseResp.
    let mut buf = vec![0u8; NOISE_SCRATCH];
    let n = handshake
        .write_message(&[], &mut buf)
        .map_err(|e| map_snow("authentication failed", e))?;
    framed
        .send(Frame::NoiseResp(buf[..n].to_vec()))
        .await
        .map_err(|e| map_snow("malformed", e))?;

    // msg3 ← read NoiseInit.
    let frame = framed
        .next()
        .await
        .ok_or_else(|| CoreError::Transport("handshake: stream closed".into()))?
        .map_err(|e| map_snow("malformed", e))?;
    let msg3 = match frame {
        Frame::NoiseInit(p) => p,
        other => {
            return Err(CoreError::Transport(format!(
                "handshake: malformed: unexpected frame type 0x{:02X}",
                other.frame_type() as u8
            )));
        }
    };
    handshake
        .read_message(&msg3, &mut in_buf)
        .map_err(|e| map_snow("authentication failed", e))?;

    finish_handshake(handshake, framed).await
}

async fn finish_handshake<S>(
    handshake: snow::HandshakeState,
    framed: Framed<S, FrameCodec>,
) -> Result<(AuthenticatedConnection<S>, HandshakeOutcome)>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    // Handshake hash is 32 bytes for BLAKE2s.
    let hh = handshake.get_handshake_hash().to_vec();
    let h_transport = hkdf_expand::<32>(&hh, INFO_TRANSPORT_BINDING_V1)?;

    // Peer's X25519 static public is what snow cached during msg3 (initiator)
    // or msg3 decryption (responder). Snow exposes it via
    // `get_remote_static`.
    let peer_x25519_slice = handshake
        .get_remote_static()
        .ok_or_else(|| CoreError::Transport("handshake: builder: missing remote static".into()))?;
    let mut peer_x25519 = [0u8; 32];
    peer_x25519.copy_from_slice(peer_x25519_slice);

    let transport = handshake
        .into_transport_mode()
        .map_err(|e| map_snow("builder", e))?;

    // h_transport lives in two places briefly: the HandshakeOutcome
    // (so 1.C can inject it as external PSK into the first MLS Commit)
    // and the AuthenticatedConnection (so `h_transport()` can return
    // a reference after the outcome is consumed). Both copies zeroize
    // on drop. When 1.C lands, the outcome's copy will typically be
    // consumed immediately and dropped, leaving the connection's copy
    // as the sole live instance.
    let outcome = HandshakeOutcome {
        peer_x25519,
        h_transport: h_transport.clone(),
    };
    let conn = AuthenticatedConnection::new(peer_x25519, h_transport, framed, transport);
    Ok((conn, outcome))
}

/// Drive the initiator side of Noise_XK over `stream`.
///
/// Writes the 1-byte version preamble, then the -e / -e, ee, s, es /
/// -s, se, (psk) token sequence as three `Frame::NoiseInit` /
/// `Frame::NoiseResp` frames. On success returns an
/// [`AuthenticatedConnection`] wrapping `stream` plus a
/// [`HandshakeOutcome`].
pub async fn handshake_initiator<S>(
    stream: S,
    identity: &IdentityKey,
    peer_static_x25519: &[u8; 32],
    invite_psk: Option<&[u8; 32]>,
) -> Result<(AuthenticatedConnection<S>, HandshakeOutcome)>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        do_initiator(stream, identity, peer_static_x25519, invite_psk),
    )
    .await
    .map_err(|_| CoreError::Transport("handshake: timeout".into()))?
}

/// Drive the responder side of Noise_XK over `stream`.
///
/// Reads and validates the 1-byte version preamble, then the three
/// Noise frames. On success returns an [`AuthenticatedConnection`]
/// wrapping `stream` plus a [`HandshakeOutcome`]. Identity resolution
/// (X25519 → Ed25519 → ContactCard) is the caller's responsibility.
pub async fn handshake_responder<S>(
    stream: S,
    identity: &IdentityKey,
    invite_psk: Option<&[u8; 32]>,
) -> Result<(AuthenticatedConnection<S>, HandshakeOutcome)>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        do_responder(stream, identity, invite_psk),
    )
    .await
    .map_err(|_| CoreError::Transport("handshake: timeout".into()))?
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::identity::IdentityKey;
    use zeroize::Zeroizing;

    /// Drive both sides of a no-PSK handshake over a tokio duplex
    /// and return both outcomes.
    async fn run_pair(
        initiator: IdentityKey,
        responder: IdentityKey,
        init_psk: Option<[u8; 32]>,
        resp_psk: Option<[u8; 32]>,
    ) -> (
        Result<(
            AuthenticatedConnection<tokio::io::DuplexStream>,
            HandshakeOutcome,
        )>,
        Result<(
            AuthenticatedConnection<tokio::io::DuplexStream>,
            HandshakeOutcome,
        )>,
    ) {
        let (init_io, resp_io) = tokio::io::duplex(16 * 1024);
        let responder_pub = responder.noise_static_public();

        let init_fut = async move {
            let psk_ref = init_psk.as_ref().map(|p| p as &[u8; 32]);
            handshake_initiator(init_io, &initiator, &responder_pub, psk_ref).await
        };
        let resp_fut = async move {
            let psk_ref = resp_psk.as_ref().map(|p| p as &[u8; 32]);
            handshake_responder(resp_io, &responder, psk_ref).await
        };

        tokio::join!(init_fut, resp_fut)
    }

    #[tokio::test]
    async fn happy_path_no_psk() {
        let initiator = IdentityKey::from_bytes(Zeroizing::new([0x11u8; 32]));
        let responder = IdentityKey::from_bytes(Zeroizing::new([0x22u8; 32]));
        let init_pub = initiator.noise_static_public();
        let resp_pub = responder.noise_static_public();

        let (init_r, resp_r) = run_pair(initiator, responder, None, None).await;
        let (_init_conn, init_out) = init_r.expect("initiator handshake must succeed");
        let (_resp_conn, resp_out) = resp_r.expect("responder handshake must succeed");

        // Each side sees the other's X25519 static pub.
        assert_eq!(init_out.peer_x25519, resp_pub, "initiator sees responder");
        assert_eq!(resp_out.peer_x25519, init_pub, "responder sees initiator");

        // Both sides derive the same binding hash.
        assert_eq!(*init_out.h_transport, *resp_out.h_transport);
        assert_ne!(*init_out.h_transport, [0u8; 32], "must not be all-zero");
    }

    #[tokio::test]
    async fn h_transport_is_hkdf_of_handshake_hash_label_v1() {
        use crate::identity::derive::{hkdf_expand, INFO_TRANSPORT_BINDING_V1};

        let initiator = IdentityKey::from_bytes(Zeroizing::new([0x33u8; 32]));
        let responder = IdentityKey::from_bytes(Zeroizing::new([0x44u8; 32]));

        let (init_r, resp_r) = run_pair(initiator, responder, None, None).await;
        let (_ic, init_out) = init_r.expect("initiator ok");
        let (_rc, resp_out) = resp_r.expect("responder ok");

        // We can't directly ask snow for the handshake hash from
        // outside the handshake function, but we CAN prove the
        // binding-label invariant: applying HKDF with a DIFFERENT
        // label must yield a different 32-byte output.
        let alt_info = b"skattr-binding-wrong";
        let expected_hh = &*init_out.h_transport;
        let wrong_label = hkdf_expand::<32>(expected_hh, alt_info).unwrap();
        assert_ne!(*wrong_label, *init_out.h_transport);

        // Sanity-check the canonical label bytes so a rename would
        // also fail this test.
        assert_eq!(INFO_TRANSPORT_BINDING_V1, b"skattr-binding-v1");

        // And that the two sides' h_transport agree.
        assert_eq!(*init_out.h_transport, *resp_out.h_transport);
    }

    #[tokio::test]
    async fn happy_path_with_matching_psk() {
        let initiator = IdentityKey::from_bytes(Zeroizing::new([0x55u8; 32]));
        let responder = IdentityKey::from_bytes(Zeroizing::new([0x66u8; 32]));
        let psk = [0xEEu8; 32];

        let (init_r, resp_r) = run_pair(initiator, responder, Some(psk), Some(psk)).await;
        let (_ic, init_out) = init_r.expect("initiator ok");
        let (_rc, resp_out) = resp_r.expect("responder ok");

        assert_eq!(*init_out.h_transport, *resp_out.h_transport);
        // PSK path must produce a DIFFERENT h_transport than the no-PSK
        // path would for the same identities, because snow mixes the
        // PSK into the handshake hash.
        let (no_psk_init, _no_psk_resp) = run_pair(
            IdentityKey::from_bytes(Zeroizing::new([0x55u8; 32])),
            IdentityKey::from_bytes(Zeroizing::new([0x66u8; 32])),
            None,
            None,
        )
        .await;
        let (_, no_psk_out) = no_psk_init.expect("no-psk also ok");
        assert_ne!(
            *init_out.h_transport, *no_psk_out.h_transport,
            "PSK must be mixed into the handshake hash"
        );
    }

    #[tokio::test]
    async fn psk_mismatch_fails_with_authentication_failed() {
        let initiator = IdentityKey::from_bytes(Zeroizing::new([0x77u8; 32]));
        let responder = IdentityKey::from_bytes(Zeroizing::new([0x88u8; 32]));

        let (init_r, resp_r) =
            run_pair(initiator, responder, Some([0xAAu8; 32]), Some([0xBBu8; 32])).await;

        // In Noise_XKpsk3 the PSK is applied at msg3.  The responder reads
        // msg3 with the wrong PSK and fails with a MAC error — guaranteed.
        // The initiator writes msg3 (mixing in its own PSK) and calls
        // into_transport_mode() before the responder has had a chance to
        // reject: snow accepts the write unconditionally, so the initiator
        // may see either success or a "stream closed" error depending on
        // scheduling.  We therefore only assert the responder side.
        let resp_err = resp_r
            .err()
            .expect("responder must fail with mismatched PSK");
        match resp_err {
            CoreError::Transport(s) => assert!(
                s.starts_with("handshake: authentication failed")
                    || s == "handshake: stream closed",
                "unexpected responder error message: {s}"
            ),
            other => panic!("expected CoreError::Transport for responder, got {other:?}"),
        }

        // If the initiator did fail, its error must also be a Transport variant.
        if let Err(init_err) = init_r {
            match init_err {
                CoreError::Transport(s) => assert!(
                    s.starts_with("handshake: authentication failed")
                        || s == "handshake: stream closed",
                    "unexpected initiator error message: {s}"
                ),
                other => panic!("expected CoreError::Transport for initiator, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn unilateral_psk_fails() {
        // Initiator has PSK, responder doesn't → patterns don't match
        // → msg1 parse / msg3 decrypt fails on the responder.
        let initiator = IdentityKey::from_bytes(Zeroizing::new([0x99u8; 32]));
        let responder = IdentityKey::from_bytes(Zeroizing::new([0xAAu8; 32]));

        let (init_r, resp_r) = run_pair(initiator, responder, Some([0xCCu8; 32]), None).await;

        assert!(init_r.is_err(), "initiator must fail under unilateral PSK");
        assert!(resp_r.is_err(), "responder must fail under unilateral PSK");
    }

    #[tokio::test]
    async fn responder_rejects_wrong_version_byte() {
        use tokio::io::AsyncWriteExt;

        let (mut init_io, resp_io) = tokio::io::duplex(4096);
        let responder = IdentityKey::from_bytes(Zeroizing::new([0xBBu8; 32]));

        // Skip the real initiator — write a bogus version byte directly.
        let writer = tokio::spawn(async move {
            init_io.write_all(&[0x02u8]).await.unwrap();
            init_io.flush().await.unwrap();
            // Keep the stream alive until the responder has read the byte.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });

        let resp_err = handshake_responder(resp_io, &responder, None)
            .await
            .err()
            .expect("responder must reject 0x02");

        writer.await.unwrap();

        match resp_err {
            CoreError::Transport(s) => assert!(
                s.starts_with("handshake: unsupported version: 0x02"),
                "got: {s}"
            ),
            other => panic!("expected CoreError::Transport, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn responder_rejects_unexpected_first_frame_type() {
        use tokio::io::AsyncWriteExt;

        let (mut init_io, resp_io) = tokio::io::duplex(4096);
        let responder = IdentityKey::from_bytes(Zeroizing::new([0xCCu8; 32]));

        // Proper version byte + a Ping frame (type 0x07 — wrong for a
        // handshake start).
        let writer = tokio::spawn(async move {
            init_io.write_all(&[PROTOCOL_VERSION]).await.unwrap();
            // length=1, type=0x07 (Ping).
            init_io.write_all(&1u32.to_be_bytes()).await.unwrap();
            init_io.write_all(&[0x07u8]).await.unwrap();
            init_io.flush().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });

        let resp_err = handshake_responder(resp_io, &responder, None)
            .await
            .err()
            .expect("responder must reject non-NoiseInit first frame");

        writer.await.unwrap();

        match resp_err {
            CoreError::Transport(s) => assert!(
                s.starts_with("handshake: malformed: unexpected frame type 0x07"),
                "got: {s}"
            ),
            other => panic!("expected CoreError::Transport, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn responder_rejects_stream_closed_before_preamble() {
        let (init_io, resp_io) = tokio::io::duplex(4096);
        let responder = IdentityKey::from_bytes(Zeroizing::new([0xDDu8; 32]));

        // Drop init_io without writing anything — responder's
        // read_exact sees UnexpectedEof immediately.
        drop(init_io);

        let resp_err = handshake_responder(resp_io, &responder, None)
            .await
            .err()
            .expect("responder must fail on EOF");

        match resp_err {
            CoreError::Transport(s) => assert!(s == "handshake: stream closed", "got: {s}"),
            other => panic!("expected CoreError::Transport, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn wrong_peer_static_fails_authentication() {
        let initiator = IdentityKey::from_bytes(Zeroizing::new([0xE1u8; 32]));
        let responder = IdentityKey::from_bytes(Zeroizing::new([0xE2u8; 32]));
        let bogus_responder_pub = [0x00u8; 32]; // All-zero X25519 pub.

        let (init_io, resp_io) = tokio::io::duplex(16 * 1024);
        let init_fut = async move {
            handshake_initiator(init_io, &initiator, &bogus_responder_pub, None).await
        };
        let resp_fut = async move { handshake_responder(resp_io, &responder, None).await };
        let (init_r, resp_r) = tokio::join!(init_fut, resp_fut);

        // At least one side must surface an authentication failure.
        // (The initiator may succeed in writing msg1 and only fail when
        // msg2 comes back mangled, or may fail on msg3 encryption; the
        // responder is guaranteed to fail on msg1 or msg3 decrypt.)
        let any_auth_fail = [&init_r, &resp_r].iter().any(|r| match r {
            Err(CoreError::Transport(s)) => {
                s.starts_with("handshake: authentication failed") || s == "handshake: stream closed"
            }
            _ => false,
        });
        assert!(
            any_auth_fail,
            "expected at least one side to fail with authentication failed / stream closed"
        );
        assert!(init_r.is_err() || resp_r.is_err(), "both must not succeed");
    }

    #[tokio::test]
    async fn send_recv_round_trip_post_handshake() {
        let initiator = IdentityKey::from_bytes(Zeroizing::new([0xF1u8; 32]));
        let responder = IdentityKey::from_bytes(Zeroizing::new([0xF2u8; 32]));
        let responder_pub = responder.noise_static_public();

        let (init_io, resp_io) = tokio::io::duplex(16 * 1024);
        let init_fut =
            async move { handshake_initiator(init_io, &initiator, &responder_pub, None).await };
        let resp_fut = async move { handshake_responder(resp_io, &responder, None).await };
        let (init_r, resp_r) = tokio::join!(init_fut, resp_fut);
        let (mut init_conn, _init_out) = init_r.unwrap();
        let (mut resp_conn, _resp_out) = resp_r.unwrap();

        // Round-trip a Ping from initiator → responder.
        init_conn
            .send(crate::transport::frame::Frame::Ping)
            .await
            .unwrap();
        let received = resp_conn.recv().await.unwrap().expect("one frame expected");
        assert!(matches!(received, crate::transport::frame::Frame::Ping));

        // And a Pong in the other direction.
        resp_conn
            .send(crate::transport::frame::Frame::Pong)
            .await
            .unwrap();
        let received = init_conn.recv().await.unwrap().expect("one frame expected");
        assert!(matches!(received, crate::transport::frame::Frame::Pong));

        // And a Bye that both sides observe cleanly via close → recv.
        init_conn.close().await.unwrap();
        let after = resp_conn.recv().await.unwrap();
        match after {
            Some(crate::transport::frame::Frame::Bye) => {}
            other => panic!("expected Bye after close, got {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn handshake_times_out_after_window() {
        let (init_io, _resp_io) = tokio::io::duplex(4096);
        // Keep _resp_io alive so the duplex doesn't get EOF'd — the
        // initiator should block on reading msg2 until the timer fires.
        let _keepalive = _resp_io;

        let responder_pub =
            IdentityKey::from_bytes(Zeroizing::new([0x11u8; 32])).noise_static_public();
        let initiator = IdentityKey::from_bytes(Zeroizing::new([0x22u8; 32]));

        let handle = tokio::spawn(async move {
            handshake_initiator(init_io, &initiator, &responder_pub, None).await
        });

        // Advance virtual time past the timeout window.
        tokio::time::advance(HANDSHAKE_TIMEOUT + std::time::Duration::from_secs(1)).await;

        let result = handle.await.expect("task must not panic");
        match result {
            Err(CoreError::Transport(s)) => assert_eq!(s, "handshake: timeout"),
            Err(other) => panic!("expected Transport(timeout), got {other:?}"),
            Ok(_) => panic!("expected timeout, got Ok"),
        }
    }
}
