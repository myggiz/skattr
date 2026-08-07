// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! On-demand outbound dialer for the per-peer delivery actor: resolve a
//! contact's onion, dial via the injected `Transport`, run the Noise
//! initiator handshake, and return an authenticated connection.

use crate::delivery::DeliveryErrorKind;
use crate::error::{CoreError, Result};
use crate::identity::{IdentityKey, PublicKey};
use crate::storage::{ContactRepo, Pool};
use crate::transport::{handshake_initiator, AuthenticatedConnection, Transport, ONION_PORT};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};

/// Upper bound on the byte-stream dial (onion connect) before we give up and
/// let the caller retry. The Noise handshake has its own `HANDSHAKE_TIMEOUT`;
/// this bounds the connect step so a dead onion can't stall the per-peer
/// actor's `select!` loop indefinitely.
const DIAL_TIMEOUT: Duration = Duration::from_secs(30);

/// Strategy for establishing an authenticated outbound connection to a
/// peer. Injected into the per-peer actor so the actor can dial on
/// demand without depending on a concrete `Transport`. Tests inject a
/// stub that hands back a pre-built duplex connection.
#[async_trait::async_trait]
pub(crate) trait OutboundDial<S>: Send + Sync + 'static
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Establish an authenticated connection to `peer`, or error if the peer
    /// has no resolvable onion / the dial or handshake fails. Returns the
    /// connection together with the handshake's `h_transport` (the
    /// transport↔MLS binding value; ADR 0009) so the caller can bind it into
    /// a genesis MLS Commit. Most callers ignore the second element.
    async fn dial(
        &self,
        peer: PublicKey,
    ) -> Result<(AuthenticatedConnection<S>, zeroize::Zeroizing<[u8; 32]>)>;

    /// Like [`dial`](Self::dial), but uses a caller-supplied `onion` instead of
    /// resolving via `latest_card`. Used by first-contact `add_contact`, where
    /// the inviter's onion lives in the invite card and its `ContactCard` is not
    /// persisted until *after* a successful dial (full add_contact atomicity,
    /// T2-1) — so `latest_card` would find nothing.
    async fn dial_at(
        &self,
        peer: PublicKey,
        onion: &str,
    ) -> Result<(AuthenticatedConnection<S>, zeroize::Zeroizing<[u8; 32]>)>;
}

/// Production [`OutboundDial`] backed by a real [`Transport`]. Resolves
/// the peer's latest [`ContactCard`](crate::contact::card::ContactCard)
/// onion, dials it, and runs the Noise_XK initiator handshake targeting
/// the peer's static X25519 key (derived from their Ed25519 identity).
pub(crate) struct TransportDial<T: Transport> {
    transport: Arc<T>,
    identity: Arc<IdentityKey>,
    pool: Arc<Pool>,
}

impl<T: Transport> TransportDial<T> {
    pub(crate) fn new(transport: Arc<T>, identity: Arc<IdentityKey>, pool: Arc<Pool>) -> Self {
        Self {
            transport,
            identity,
            pool,
        }
    }

    /// Shared "dial onion + Noise_XK initiator handshake" core for both
    /// [`OutboundDial::dial`] (which resolves the onion via `latest_card`) and
    /// [`OutboundDial::dial_at`] (which is handed the onion directly). Returns
    /// the authenticated connection plus the handshake's `h_transport` binding
    /// value (ADR 0009).
    async fn dial_onion(
        &self,
        peer: PublicKey,
        onion: &str,
    ) -> Result<(
        AuthenticatedConnection<T::Stream>,
        zeroize::Zeroizing<[u8; 32]>,
    )> {
        let stream = match tokio::time::timeout(
            DIAL_TIMEOUT,
            self.transport.dial(onion, ONION_PORT),
        )
        .await
        {
            Ok(res) => res?,
            Err(_) => return Err(CoreError::Delivery(DeliveryErrorKind::Timeout)),
        };
        let verifying = ed25519_dalek::VerifyingKey::from_bytes(&peer.0).map_err(|_| {
            CoreError::Delivery(DeliveryErrorKind::Other(
                "no route to peer: malformed peer identity key".into(),
            ))
        })?;
        let peer_x25519 = crate::identity::key::ed25519_pub_to_x25519(&verifying);
        let (conn, outcome) =
            handshake_initiator(stream, &self.identity, &peer_x25519, None).await?;
        Ok((conn, outcome.h_transport))
    }
}

#[async_trait::async_trait]
impl<T: Transport> OutboundDial<T::Stream> for TransportDial<T> {
    async fn dial(
        &self,
        peer: PublicKey,
    ) -> Result<(
        AuthenticatedConnection<T::Stream>,
        zeroize::Zeroizing<[u8; 32]>,
    )> {
        let card = ContactRepo::new(&self.pool)
            .latest_card(&peer)?
            .ok_or_else(|| {
                CoreError::Delivery(DeliveryErrorKind::Other(
                    "no route to peer: no contact card".into(),
                ))
            })?;
        let onion = card.body.onion.clone();
        self.dial_onion(peer, &onion).await
    }

    async fn dial_at(
        &self,
        peer: PublicKey,
        onion: &str,
    ) -> Result<(
        AuthenticatedConnection<T::Stream>,
        zeroize::Zeroizing<[u8; 32]>,
    )> {
        self.dial_onion(peer, onion).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::delivery::hub::DeliveryHub;
    use crate::envelope::MessageId;
    use crate::transport::handshake_responder;
    use std::sync::Mutex as StdMutex;
    use tokio::io::DuplexStream;

    /// Dialer that yields a single pre-built authenticated connection on
    /// its first `dial`, then errors. Ignores the `peer` argument — the
    /// test only proves the actor dials when cold and delivers a frame.
    struct OneShotDialer(StdMutex<Option<AuthenticatedConnection<DuplexStream>>>);

    impl OneShotDialer {
        fn take_one(
            &self,
        ) -> Result<(
            AuthenticatedConnection<DuplexStream>,
            zeroize::Zeroizing<[u8; 32]>,
        )> {
            self.0
                .lock()
                .unwrap()
                .take()
                .map(|conn| (conn, zeroize::Zeroizing::new([0u8; 32])))
                .ok_or_else(|| {
                    CoreError::Delivery(DeliveryErrorKind::Other("oneshot exhausted".into()))
                })
        }
    }

    #[async_trait::async_trait]
    impl OutboundDial<DuplexStream> for OneShotDialer {
        async fn dial(
            &self,
            _peer: PublicKey,
        ) -> Result<(
            AuthenticatedConnection<DuplexStream>,
            zeroize::Zeroizing<[u8; 32]>,
        )> {
            self.take_one()
        }

        async fn dial_at(
            &self,
            _peer: PublicKey,
            _onion: &str,
        ) -> Result<(
            AuthenticatedConnection<DuplexStream>,
            zeroize::Zeroizing<[u8; 32]>,
        )> {
            self.take_one()
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn actor_dials_when_cold_and_delivers() {
        let alice = IdentityKey::generate().unwrap();
        let bob = IdentityKey::generate().unwrap();
        // Capture bob's public key BEFORE moving `bob` into the responder.
        let bob_pub = PublicKey(bob.public().0);
        let bob_x = crate::identity::key::ed25519_pub_to_x25519(
            &ed25519_dalek::VerifyingKey::from_bytes(&bob.public().0).unwrap(),
        );

        let (a, b) = tokio::io::duplex(64 * 1024);
        let init = tokio::spawn(async move {
            handshake_initiator(a, &alice, &bob_x, None)
                .await
                .unwrap()
                .0
        });
        let resp = tokio::spawn(async move { handshake_responder(b, &bob, None).await.unwrap().0 });
        let alice_conn = init.await.unwrap();
        let mut bob_conn = resp.await.unwrap();

        let pool = Arc::new(Pool::in_memory());
        let dialer: Arc<dyn OutboundDial<DuplexStream>> =
            Arc::new(OneShotDialer(StdMutex::new(Some(alice_conn))));
        let hub = DeliveryHub::<DuplexStream>::new_with_dialer(pool, dialer);

        // Hub has no live conn for bob → the actor must dial via the
        // injected dialer before it can send.
        let _ack = hub
            .send(bob_pub, MessageId::generate(), b"hi".to_vec())
            .await
            .unwrap();

        let frame = tokio::time::timeout(std::time::Duration::from_secs(2), bob_conn.recv())
            .await
            .expect("frame within 2s")
            .unwrap();
        assert!(frame.is_some(), "bob received the dialed-and-sent frame");
        match frame {
            Some(crate::transport::Frame::MlsApp(ct)) => assert_eq!(ct, b"hi"),
            other => panic!("expected MlsApp, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn welcome_arm_dials_when_cold_and_delivers() {
        use crate::transport::Frame;

        let alice = IdentityKey::generate().unwrap();
        let bob = IdentityKey::generate().unwrap();
        let bob_pub = PublicKey(bob.public().0);
        let bob_x = crate::identity::key::ed25519_pub_to_x25519(
            &ed25519_dalek::VerifyingKey::from_bytes(&bob.public().0).unwrap(),
        );
        let (a, b) = tokio::io::duplex(64 * 1024);
        let init = tokio::spawn(async move {
            handshake_initiator(a, &alice, &bob_x, None)
                .await
                .unwrap()
                .0
        });
        let resp = tokio::spawn(async move { handshake_responder(b, &bob, None).await.unwrap().0 });
        let alice_conn = init.await.unwrap();
        let mut bob_conn = resp.await.unwrap();

        let pool = Arc::new(Pool::in_memory());
        let dialer: Arc<dyn OutboundDial<DuplexStream>> =
            Arc::new(OneShotDialer(StdMutex::new(Some(alice_conn))));
        let hub = DeliveryHub::<DuplexStream>::new_with_dialer(pool, dialer);

        // Cold actor: no conn ingested. send_welcome must dial then deliver.
        let _ack = hub
            .send_welcome(bob_pub, b"welcome-bytes".to_vec())
            .await
            .unwrap();
        let frame = tokio::time::timeout(std::time::Duration::from_secs(2), bob_conn.recv())
            .await
            .expect("frame within 2s")
            .unwrap();
        match frame {
            Some(Frame::MlsWelcome(w)) => assert_eq!(w, b"welcome-bytes"),
            other => panic!("expected MlsWelcome, got {other:?}"),
        }
    }
}
