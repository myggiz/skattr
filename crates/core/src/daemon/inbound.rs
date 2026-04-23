// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic))]

//! MLS-aware `InboundDispatch`: decrypt, persist, emit event.
//!
//! [`DaemonInbound`] implements the [`InboundDispatch`] trait used by the
//! delivery layer. On each inbound MLS ciphertext it:
//!
//! 1. Looks up the peer's group_id from `ContactRepo`.
//! 2. Loads the MLS [`Group`] from `MlsGroupRepo`.
//! 3. Decrypts the ciphertext to an [`Envelope`].
//! 4. Saves updated MLS state.
//! 5. Persists the message to [`MessageRepo`].
//! 6. Broadcasts [`Event::MessageReceived`] on the events channel.

use std::sync::Arc;

use tokio::sync::broadcast;

use crate::daemon::events::Event;
use crate::delivery::peer::InboundDispatch;
use crate::envelope::MessageId;
use crate::error::{CoreError, Result};
use crate::identity::PublicKey;
use crate::mls::{Group, GroupId};
use crate::storage::{ContactRepo, MessageRepo, MlsGroupRepo, Pool};

/// Concrete [`InboundDispatch`] used by the running daemon.
///
/// Holds a reference-counted pool and a broadcast sender so it can be
/// shared across peer actor threads without additional locking.
pub(crate) struct DaemonInbound {
    pub pool: Arc<Pool>,
    pub events_tx: broadcast::Sender<Event>,
}

impl DaemonInbound {
    /// Create a new `DaemonInbound`.
    pub(crate) fn new(pool: Arc<Pool>, events_tx: broadcast::Sender<Event>) -> Self {
        Self { pool, events_tx }
    }

    /// Production-side dispatch: look up the peer's group via contacts,
    /// then delegate to [`dispatch_for_group`] with the resolved group_id.
    fn dispatch_inner(&self, from: PublicKey, ciphertext: &[u8]) -> Result<MessageId> {
        let contact_repo = ContactRepo::new(&self.pool);
        let group_id = contact_repo
            .get_group_id(&from)?
            .filter(|b| !b.is_empty())
            .ok_or_else(|| CoreError::Mls("mls: inbound: no group for peer".into()))?;
        self.dispatch_for_group(from, &group_id, ciphertext)
    }

    /// Test hook and inner implementation: run the decrypt + persist + emit
    /// pipeline with an explicit `group_id`, bypassing the contacts lookup.
    ///
    /// This is `pub` (within the crate) so that unit tests can drive it
    /// without needing a full contacts row.
    pub(crate) fn dispatch_for_group(
        &self,
        from: PublicKey,
        group_id: &[u8],
        ciphertext: &[u8],
    ) -> Result<MessageId> {
        let group_repo = MlsGroupRepo::new(&self.pool);
        let gid = GroupId(group_id.to_vec());
        let mut group = Group::load(&gid, &group_repo)?
            .ok_or_else(|| CoreError::Mls("mls: inbound: unknown group_id".into()))?;

        let envelope = group.decrypt(ciphertext)?;
        group.save(&group_repo)?;

        let msg_repo = MessageRepo::new(&self.pool);
        msg_repo.insert(group_id, &from.0, &envelope)?;

        let message_id = envelope.id;

        // Failure to deliver to an event subscriber is non-fatal — no
        // receivers is expected on startup before the CLI subscribes.
        let _ = self
            .events_tx
            .send(Event::MessageReceived { from, envelope });

        Ok(message_id)
    }
}

impl InboundDispatch for DaemonInbound {
    fn dispatch(&self, peer: PublicKey, ciphertext: &[u8]) -> Option<MessageId> {
        match self.dispatch_inner(peer, ciphertext) {
            Ok(mid) => Some(mid),
            Err(e) => {
                tracing::warn!(
                    peer = ?peer,
                    err = %e,
                    "inbound: dispatch failed, dropping frame"
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    use crate::daemon::events::Event;
    use crate::envelope::{Kind, MessageId};
    use crate::identity::PublicKey;
    use crate::mls::provider::MlsProvider;
    use crate::storage::Pool;

    /// Set up a 2-member MLS group (alice + bob). Bob encrypts an envelope;
    /// alice's daemon receives it via `DaemonInbound::dispatch_for_group`.
    /// Expect:
    ///  - `dispatch_for_group` returns `Ok(message_id)`
    ///  - a `MessageReceived` event appears on the broadcast channel
    ///  - the event carries the original body
    #[tokio::test]
    async fn dispatch_emits_event_after_successful_decrypt() {
        use crate::mls::key_package::KeyPackage;
        use crate::storage::key_packages::KeyPackageRepo;

        let pool = Arc::new(Pool::in_memory());
        let (events_tx, mut rx) = broadcast::channel::<Event>(16);

        // Set up identities.
        let alice_seed = crate::identity::Seed::generate().unwrap();
        let alice_id = crate::identity::IdentityKey::from_seed(&alice_seed).unwrap();
        let bob_seed = crate::identity::Seed::generate().unwrap();
        let bob_id = crate::identity::IdentityKey::from_seed(&bob_seed).unwrap();
        // `peer` is bob's public key — the sender from alice's perspective.
        let peer = bob_id.public();

        // Generate bob's KeyPackage so he can join alice's group.
        let bob_provider = MlsProvider::new();
        let kp_repo = KeyPackageRepo::new(&pool);
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo).unwrap();

        // Alice creates a solo group, adds bob, producing a Welcome.
        let mut alice_group =
            crate::mls::Group::create_solo(&alice_id, None, MlsProvider::new()).unwrap();
        let (welcome, _commit) = alice_group.add_member(&bob_kp, None).unwrap();
        let group_id_bytes = alice_group.id().0.clone();

        // Bob joins from the Welcome — this is bob's group state.
        let mut bob_group =
            crate::mls::Group::join_from_welcome(&bob_id, &welcome, None, bob_provider).unwrap();

        // Bob encrypts an envelope.
        let msg_id = MessageId::generate();
        let env = crate::envelope::Envelope {
            v: 1,
            id: msg_id,
            ts: 1_700_000_000,
            reply_to: None,
            kind: Kind::Text { body: "hi".into() },
        };
        let ciphertext = bob_group.encrypt(&env).unwrap();

        // Persist alice's group state so DaemonInbound can load it.
        let group_repo = MlsGroupRepo::new(&pool);
        alice_group.save(&group_repo).unwrap();

        let inbound = DaemonInbound::new(pool.clone(), events_tx.clone());

        let returned_mid = inbound
            .dispatch_for_group(peer, &group_id_bytes, &ciphertext)
            .unwrap();

        // Message id must round-trip.
        assert_eq!(returned_mid.0, msg_id.0);

        // An event must be broadcast.
        match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await {
            Ok(Ok(Event::MessageReceived { from, envelope })) => {
                assert_eq!(from, peer);
                assert!(
                    matches!(&envelope.kind, Kind::Text { body } if body == "hi"),
                    "unexpected kind: {:?}",
                    envelope.kind
                );
            }
            other => panic!("expected MessageReceived, got {other:?}"),
        }
    }

    /// When no group exists for the given id, dispatch_for_group must
    /// return an error (and the InboundDispatch blanket returns None).
    #[tokio::test]
    async fn dispatch_returns_none_for_unknown_group() {
        let pool = Arc::new(Pool::in_memory());
        let (events_tx, _rx) = broadcast::channel::<Event>(16);

        let inbound = DaemonInbound::new(pool, events_tx);
        let peer = PublicKey([0xBB; 32]);
        let unknown_gid = vec![0xDE; 32];
        let garbage_ct = b"not a real ciphertext";

        let result = inbound.dispatch_for_group(peer, &unknown_gid, garbage_ct);
        assert!(result.is_err(), "expected Err for unknown group, got Ok");

        // The InboundDispatch impl must return None (not panic).
        let mid = InboundDispatch::dispatch(&inbound, peer, garbage_ct);
        assert!(mid.is_none());
    }
}
