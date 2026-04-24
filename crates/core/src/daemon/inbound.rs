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
//! 5. Routes through [`crate::delivery::receiver::receive`] — replay-window
//!    check, `(sender, message_id)` dedup, persist.
//! 6. Broadcasts [`Event::MessageReceived`] on the events channel for
//!    `ReceiveOutcome::New` (not for duplicates).

use std::sync::Arc;

use tokio::sync::broadcast;

use crate::daemon::commands::{Direction, MessageRecord};
use crate::daemon::events::Event;
use crate::delivery::peer::InboundDispatch;
use crate::delivery::receiver::{receive, ReceiveOutcome};
use crate::envelope::MessageId;
use crate::error::{CoreError, Result};
use crate::identity::PublicKey;
use crate::mls::{Group, GroupId};
use crate::storage::seen_messages::SeenMessagesRepo;
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

        // MLS generation is captured *after* decrypt so the broadcast
        // event reflects the epoch under which this message was
        // authenticated. Likewise `ts_daemon_recv` is the local clock at
        // the moment we successfully decoded the payload.
        let msg_id = envelope.id; // capture before receive() consumes envelope
        let mls_generation = group.epoch();
        let ts_daemon_recv = now_unix_seconds();
        // `receive()` uses milliseconds for the ±1h replay window check;
        // see `delivery::receiver::receive` docs.
        let now_ms = ts_daemon_recv.saturating_mul(1000);

        let msg_repo = MessageRepo::new(&self.pool);
        let seen_repo = SeenMessagesRepo::new(&self.pool);

        let outcome = receive(
            &from,
            group_id,
            envelope,
            now_ms,
            mls_generation,
            ts_daemon_recv,
            &seen_repo,
            &msg_repo,
        )?;

        match &outcome {
            ReceiveOutcome::New {
                envelope,
                row_id,
                mls_generation,
                ts_daemon_recv,
                ..
            } => {
                // 2-member group scope (Phase 1.G): the sender IS the
                // peer, so `contact == from`. Multi-member groups will
                // need a ContactRepo::find_by_group_id lookup here.
                let record = MessageRecord::project(
                    *row_id,
                    envelope,
                    from,
                    *mls_generation,
                    *ts_daemon_recv,
                    Direction::Incoming,
                );
                // Failure to deliver to an event subscriber is non-fatal — no
                // receivers is expected on startup before the CLI subscribes.
                let _ = self.events_tx.send(Event::MessageReceived {
                    contact: from,
                    record,
                });
            }
            ReceiveOutcome::Duplicate => {
                // Already seen; no event. Still ACK via the returned
                // `msg_id` so the sender stops retrying (1.E idempotency).
                tracing::debug!(
                    peer = ?from,
                    msg_id = ?msg_id,
                    "inbound: duplicate, acking without event",
                );
            }
            ReceiveOutcome::Rejected(reason) => {
                tracing::warn!(
                    peer = ?from,
                    reason = %reason,
                    "inbound: rejected by receiver (replay window or dedup)",
                );
                return Err(CoreError::Delivery(format!("inbound: rejected: {reason}")));
            }
        }

        Ok(msg_id)
    }
}

/// Unix seconds from the system clock, saturating to 0 on error.
///
/// Duplicates the pattern used by the three Task 13 integration test
/// copies; kept local here to avoid leaking a clock helper into the
/// public API. A future cleanup pass may hoist this into a shared
/// internal utility.
fn now_unix_seconds() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    )
    .unwrap_or(0)
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

        // Bob encrypts an envelope. `Envelope.ts` is millis-since-epoch
        // and must land inside the ±1h replay window `receive()` checks.
        let now_ms = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
        )
        .unwrap_or(0);
        let msg_id = MessageId::generate();
        let env = crate::envelope::Envelope {
            v: 1,
            id: msg_id,
            ts: now_ms,
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
            Ok(Ok(Event::MessageReceived { contact, record })) => {
                assert_eq!(contact, peer);
                assert!(
                    matches!(&record.kind, Kind::Text { body } if body == "hi"),
                    "unexpected kind: {:?}",
                    record.kind
                );
            }
            other => panic!("expected MessageReceived, got {other:?}"),
        }
    }

    /// Phase 1.G: the broadcast event's `record` must carry the real
    /// decrypt-time metadata — `mls_generation` sourced from
    /// `Group::epoch()` (post-`add_member` this is 1 for the
    /// alice-adds-bob flow) and a non-zero `ts_daemon_recv` from the
    /// local clock. Neither must be zeroed/placeholdered.
    #[tokio::test]
    async fn dispatch_emits_message_received_with_mls_generation_and_ts_daemon_recv() {
        use crate::daemon::commands::Direction;
        use crate::mls::key_package::KeyPackage;
        use crate::storage::key_packages::KeyPackageRepo;
        use std::time::{SystemTime, UNIX_EPOCH};
        let pool = Arc::new(Pool::in_memory());
        let (events_tx, mut rx) = broadcast::channel::<Event>(16);
        // Identities.
        let alice_seed = crate::identity::Seed::generate().unwrap();
        let alice_id = crate::identity::IdentityKey::from_seed(&alice_seed).unwrap();
        let bob_seed = crate::identity::Seed::generate().unwrap();
        let bob_id = crate::identity::IdentityKey::from_seed(&bob_seed).unwrap();
        let peer = bob_id.public();

        let bob_provider = MlsProvider::new();
        let kp_repo = KeyPackageRepo::new(&pool);
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo).unwrap();
        let mut alice_group =
            crate::mls::Group::create_solo(&alice_id, None, MlsProvider::new()).unwrap();
        let (welcome, _commit) = alice_group.add_member(&bob_kp, None).unwrap();
        let group_id_bytes = alice_group.id().0.clone();
        let mut bob_group =
            crate::mls::Group::join_from_welcome(&bob_id, &welcome, None, bob_provider).unwrap();

        // Snapshot alice's post-add_member epoch — this is the ordering
        // signal our Event must surface.
        let expected_epoch = alice_group.epoch();

        // Bob encrypts an envelope; ts must be within ±1h of local clock
        // (millis), per the replay window.
        let now_ms = i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
        )
        .unwrap_or(0);
        let env = crate::envelope::Envelope {
            v: 1,
            id: MessageId::generate(),
            ts: now_ms,
            reply_to: None,
            kind: Kind::Text {
                body: "phase-1g-check".into(),
            },
        };
        let ciphertext = bob_group.encrypt(&env).unwrap();

        let group_repo = MlsGroupRepo::new(&pool);
        alice_group.save(&group_repo).unwrap();

        let before_recv = i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        )
        .unwrap_or(0);

        let inbound = DaemonInbound::new(pool.clone(), events_tx.clone());
        inbound
            .dispatch_for_group(peer, &group_id_bytes, &ciphertext)
            .unwrap();

        match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await {
            Ok(Ok(Event::MessageReceived { contact, record })) => {
                assert_eq!(contact, peer, "event contact must be the sender");
                assert!(
                    matches!(&record.kind, Kind::Text { body } if body == "phase-1g-check"),
                    "unexpected kind: {:?}",
                    record.kind
                );
                assert!(
                    matches!(record.direction, Direction::Incoming),
                    "inbound dispatch must project Direction::Incoming"
                );
                assert_eq!(
                    record.mls_generation, expected_epoch,
                    "record.mls_generation must equal Group::epoch() at decrypt time"
                );
                assert!(
                    record.mls_generation >= 1,
                    "post-add_member epoch must be at least 1; got {}",
                    record.mls_generation
                );
                // `ts_daemon_recv` is wall-clock seconds and must be
                // non-zero and not older than the test start. We don't
                // pin a tight upper bound because the broadcast send
                // happens asynchronously.
                assert!(
                    record.ts_daemon_recv >= u64::try_from(before_recv).unwrap_or(0),
                    "ts_daemon_recv must be >= test-start clock ({}); got {}",
                    before_recv,
                    record.ts_daemon_recv,
                );
                assert_eq!(
                    record.ts_envelope, env.ts,
                    "ts_envelope must round-trip the sender-claimed envelope ts"
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
