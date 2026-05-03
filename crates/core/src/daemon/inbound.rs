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

use crate::daemon::clock::now_unix_seconds;
use crate::daemon::commands::{Direction, MessageRecord};
use crate::daemon::events::Event;
use crate::delivery::peer::InboundDispatch;
use crate::delivery::receiver::{receive_in_tx, ReceiveOutcome};
use crate::envelope::MessageId;
use crate::error::{CoreError, Result};
use crate::identity::{IdentityKey, PublicKey};
use crate::mls::{Group, GroupId, MlsErrorKind};
use crate::storage::seen_messages::SeenMessagesRepo;
use crate::storage::{ContactRepo, MessageRepo, MlsGroupRepo, Pool};

/// Concrete [`InboundDispatch`] used by the running daemon.
///
/// Holds a reference-counted pool and a broadcast sender so it can be
/// shared across peer actor threads without additional locking.
pub(crate) struct DaemonInbound {
    pub pool: Arc<Pool>,
    pub events_tx: broadcast::Sender<Event>,
    /// Local identity for MLS Welcome processing. Set once after
    /// construction via [`set_identity`]; `None` until that call.
    pub identity: std::sync::RwLock<Option<Arc<IdentityKey>>>,
}

impl DaemonInbound {
    /// Create a new `DaemonInbound`.
    pub(crate) fn new(pool: Arc<Pool>, events_tx: broadcast::Sender<Event>) -> Self {
        Self {
            pool,
            events_tx,
            identity: std::sync::RwLock::new(None),
        }
    }

    /// Wire the local identity so Welcome messages can be processed.
    /// Must be called before any Welcome frame can arrive.
    pub(crate) fn set_identity(&self, identity: Arc<IdentityKey>) {
        if let Ok(mut guard) = self.identity.write() {
            *guard = Some(identity);
        }
    }

    /// Production-side dispatch: look up the peer's group via contacts,
    /// then delegate to [`dispatch_for_group`] with the resolved group_id.
    fn dispatch_inner(&self, from: PublicKey, ciphertext: &[u8]) -> Result<MessageId> {
        let contact_repo = ContactRepo::new(&self.pool);
        let group_id = contact_repo
            .get_group_id(&from)?
            .filter(|b| !b.is_empty())
            .ok_or_else(|| {
                CoreError::from(MlsErrorKind::Other(
                    "mls: inbound: no group for peer".into(),
                ))
            })?;
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
        let mut group = Group::load(&gid, &group_repo)?.ok_or_else(|| {
            CoreError::from(MlsErrorKind::Other("mls: inbound: unknown group_id".into()))
        })?;

        let envelope = group.decrypt(ciphertext)?;

        // --- ContactCardUpdate fast path ---
        // Intercept before the message-persist transaction so that card
        // updates never land in the `messages` table.
        if let crate::envelope::Kind::ContactCardUpdate { card } = &envelope.kind {
            let now_secs = now_unix_seconds();
            let claimed_identity = card.verify(now_secs)?;
            // Cross-check: the card's identity MUST equal the MLS-authenticated
            // sender. A peer cannot publish a card claiming someone else's identity.
            if claimed_identity != from {
                tracing::warn!(
                    peer = ?from,
                    "inbound: card-update sender mismatch — dropping"
                );
                return Err(crate::error::CoreError::Contact(
                    crate::contact::ContactErrorKind::Other(
                        "card-update: sender identity mismatch".into(),
                    ),
                ));
            }
            // Persist (transactional, version-monotonic per ContactRepo::put_card).
            let contacts = crate::storage::ContactRepo::new(&self.pool);
            contacts.put_card(card)?;
            // Emit an event so UI can refresh.
            let _ = self.events_tx.send(Event::ContactCardReceived {
                contact: from,
                version: card.body.version,
            });
            // No message row, no MessageReceived. Returning the envelope's
            // message id keeps the caller's ACK contract clean.
            return Ok(envelope.id);
        }

        // MLS generation is captured *after* decrypt so the broadcast
        // event reflects the epoch under which this message was
        // authenticated. Likewise `ts_daemon_recv` is the local clock at
        // the moment we successfully decoded the payload.
        let msg_id = envelope.id; // capture before receive_in_tx() consumes envelope
        let mls_generation = group.epoch();
        let ts_daemon_recv = now_unix_seconds();
        // `receive_in_tx()` uses milliseconds for the ±1h replay window
        // check; see `delivery::receiver::receive` docs.
        let now_ms = ts_daemon_recv.saturating_mul(1000);

        let msg_repo = MessageRepo::new(&self.pool);
        let seen_repo = SeenMessagesRepo::new(&self.pool);

        // Wrap MLS ratchet save + seen_messages insert + messages insert
        // in one atomic transaction. If receive_in_tx fails after
        // save_in_tx, the entire tx rolls back — the ratchet does not
        // advance on disk without a corresponding history row.
        //
        // Rejected envelopes are surfaced as Err *inside* the closure so
        // the transaction rolls back before Pool::transaction commits.
        // If we returned Ok(Rejected) the closure would succeed, the tx
        // would commit the advanced ratchet snapshot, and the outer
        // match returning Err would be too late — the exact durability
        // gap this task was introduced to close.
        //
        // Event broadcast stays OUTSIDE this block; a failed subscriber
        // must not roll back the persisted message.
        let outcome = self.pool.transaction(|tx| {
            group.save_in_tx(&group_repo, tx)?;
            let out = receive_in_tx(
                tx,
                &from,
                group_id,
                envelope,
                now_ms,
                mls_generation,
                ts_daemon_recv,
                &seen_repo,
                &msg_repo,
            )?;
            match &out {
                ReceiveOutcome::Rejected(reason) => {
                    tracing::warn!(
                        peer = ?from,
                        reason = %reason,
                        "inbound: rejected by receiver (replay window or dedup)",
                    );
                    Err(CoreError::Delivery(
                        crate::delivery::DeliveryErrorKind::Other(format!(
                            "inbound: rejected: {reason}"
                        )),
                    ))
                }
                _ => Ok(out),
            }
        })?;

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
            // Rejected is surfaced via Err inside the transaction closure
            // above and never reaches here — the tx rolls back before
            // Pool::transaction returns Ok.
            ReceiveOutcome::Rejected(_) => unreachable!(
                "Rejected outcome is converted to Err inside the closure; \
                 it cannot reach the outer match"
            ),
        }

        Ok(msg_id)
    }

    /// Join an MLS group from a Welcome, persist group + contact + consumed
    /// markers atomically, and emit `Event::ContactUpdated`.
    fn dispatch_welcome_inner(&self, peer: PublicKey, welcome_bytes: &[u8]) -> Result<()> {
        use crate::mls::key_package::{parse_welcome_kp_hash, KeyPackage};
        use crate::mls::provider::MlsProvider;
        use crate::storage::key_packages::KeyPackageRepo;
        use crate::storage::OutstandingInviteRepo;

        let identity_arc = self
            .identity
            .read()
            .ok()
            .and_then(|g| g.clone())
            .ok_or_else(|| {
                CoreError::from(MlsErrorKind::Other(
                    "inbound welcome: identity not wired".into(),
                ))
            })?;

        // Canonical KeyPackageRef (32 bytes) for outstanding_invites lookup.
        let kp_ref = parse_welcome_kp_hash(welcome_bytes)?;

        let oi = OutstandingInviteRepo::new(&self.pool);
        let (psk, inviter_kp_bytes, provider_snap_opt, expires_at) =
            oi.get_psk_and_kp_bytes(&kp_ref)?.ok_or_else(|| {
                CoreError::from(MlsErrorKind::Other(
                    "inbound welcome: unknown kp_ref".into(),
                ))
            })?;

        let now = now_unix_seconds();
        if expires_at < now {
            return Err(CoreError::from(MlsErrorKind::Other(
                "inbound welcome: invite expired".into(),
            )));
        }

        // SHA-256 hash for the key_packages table (different from KeyPackageRef).
        let inviter_kp = KeyPackage::from_bytes(&inviter_kp_bytes)?;
        let kp_sha256 = inviter_kp.hash()?;

        // Restore the MlsProvider that was used to generate the KP so OpenMLS
        // can find the init private key. If no snapshot was stored (legacy rows
        // or pre-migration invites) we fall back to a fresh provider — this will
        // likely fail with NoMatchingKeyPackage, but that's safer than panic.
        let provider = if let Some(snap) = provider_snap_opt {
            MlsProvider::load(&snap)?
        } else {
            MlsProvider::new()
        };

        let group = Group::join_from_welcome(
            &identity_arc,
            welcome_bytes,
            Some(&*psk),
            provider,
        )?;
        let group_id = group.id().0.clone();

        let group_repo = MlsGroupRepo::new(&self.pool);
        let kp_repo = KeyPackageRepo::new(&self.pool);

        self.pool.transaction(|tx| {
            group.save_in_tx(&group_repo, tx)?;
            // Execute contact upsert + group_id link directly on `tx` to
            // avoid re-locking the Pool mutex (pool.transaction already holds
            // the mutex; ContactRepo::upsert / set_group_id call pool.with_mut
            // and would deadlock).
            tx.execute(
                "INSERT INTO contacts (identity_pubkey, display_name, added_at) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(identity_pubkey) DO UPDATE SET display_name=excluded.display_name",
                rusqlite::params![&peer.0[..], Option::<String>::None, now],
            )
            .map_err(|e| {
                CoreError::Storage(crate::storage::StorageErrorKind::Other(format!(
                    "welcome: upsert contact: {e}"
                )))
            })?;
            tx.execute(
                "UPDATE contacts SET group_id = ?1 WHERE identity_pubkey = ?2",
                rusqlite::params![&group_id[..], &peer.0[..]],
            )
            .map_err(|e| {
                CoreError::Storage(crate::storage::StorageErrorKind::Other(format!(
                    "welcome: set group_id: {e}"
                )))
            })?;
            kp_repo.mark_consumed_in_tx(tx, &kp_sha256)?;
            oi.mark_consumed_in_tx(tx, &kp_ref)?;
            Ok(())
        })?;

        let _ = self.events_tx.send(Event::ContactUpdated(peer));
        Ok(())
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

    fn dispatch_welcome(&self, peer: PublicKey, welcome: &[u8]) -> Option<MessageId> {
        let synthetic_id = crate::delivery::peer::welcome_msg_id(welcome);
        match self.dispatch_welcome_inner(peer, welcome) {
            Ok(()) => Some(synthetic_id),
            Err(e) => {
                tracing::warn!(
                    peer = ?peer,
                    err = %e,
                    "inbound: dispatch_welcome failed, not ACKing"
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

    /// ContactCardUpdate envelope: card is persisted, ContactCardReceived
    /// event fires, NO MessageReceived event, NO messages row created.
    #[tokio::test]
    async fn contact_card_update_is_persisted_and_emits_event_without_message_row() {
        use crate::contact::Contact;
        use crate::mls::key_package::KeyPackage;
        use crate::storage::key_packages::KeyPackageRepo;

        let pool = Arc::new(Pool::in_memory());
        let (events_tx, mut rx) = broadcast::channel::<Event>(16);

        // Set up identities.
        let alice_seed = crate::identity::Seed::generate().unwrap();
        let alice_id = crate::identity::IdentityKey::from_seed(&alice_seed).unwrap();
        let bob_seed = crate::identity::Seed::generate().unwrap();
        let bob_id = crate::identity::IdentityKey::from_seed(&bob_seed).unwrap();
        let peer = bob_id.public();

        // Build 2-member MLS group (alice adds bob).
        let bob_provider = crate::mls::provider::MlsProvider::new();
        let kp_repo = KeyPackageRepo::new(&pool);
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo).unwrap();
        let mut alice_group = crate::mls::Group::create_solo(
            &alice_id,
            None,
            crate::mls::provider::MlsProvider::new(),
        )
        .unwrap();
        let (welcome, _commit) = alice_group.add_member(&bob_kp, None).unwrap();
        let group_id_bytes = alice_group.id().0.clone();
        let mut bob_group =
            crate::mls::Group::join_from_welcome(&bob_id, &welcome, None, bob_provider).unwrap();

        // Pre-upsert bob as a contact in alice's pool so put_card has a row to attach to.
        let contact_repo = crate::storage::ContactRepo::new(&pool);
        contact_repo
            .upsert(&Contact {
                identity: peer,
                display_name: Some("Bob".into()),
                added_at: 0,
                card: None,
            })
            .unwrap();

        // Bob signs a ContactCard claiming his own identity at version 2.
        let now_secs = now_unix_seconds();
        let card = crate::contact::ContactCard::sign(
            &bob_id,
            "aaaa.onion".into(),
            vec!["bbbb.onion".into()],
            2,
            3600,
            now_secs,
        )
        .unwrap();

        // Bob encrypts a ContactCardUpdate envelope.
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
            kind: Kind::ContactCardUpdate {
                card: Box::new(card),
            },
        };
        let ciphertext = bob_group.encrypt(&env).unwrap();

        // Persist alice's group state.
        let group_repo = MlsGroupRepo::new(&pool);
        alice_group.save(&group_repo).unwrap();

        let inbound = DaemonInbound::new(pool.clone(), events_tx.clone());
        let returned_mid = inbound
            .dispatch_for_group(peer, &group_id_bytes, &ciphertext)
            .unwrap();

        // Message id must round-trip.
        assert_eq!(returned_mid.0, msg_id.0);

        // ContactCardReceived event must arrive.
        match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await {
            Ok(Ok(Event::ContactCardReceived { contact, version })) => {
                assert_eq!(contact, peer);
                assert_eq!(version, 2);
            }
            other => panic!("expected ContactCardReceived, got {other:?}"),
        }

        // No MessageReceived event must be queued.
        // The channel should be empty (recv should time out immediately).
        match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
            Err(_timeout) => {} // expected: no more events
            Ok(Ok(Event::MessageReceived { .. })) => {
                panic!("MessageReceived must NOT be emitted for ContactCardUpdate")
            }
            Ok(other) => panic!("unexpected: {other:?}"),
        }

        // alice's ContactRepo::latest_card(&bob) must return the new card.
        let stored = contact_repo.latest_card(&peer).unwrap().unwrap();
        assert_eq!(stored.body.version, 2);
        assert_eq!(stored.body.onion, "aaaa.onion");

        // No messages row for this group_id.
        let msg_count: i64 = pool
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM messages WHERE group_id = ?1",
                    rusqlite::params![&group_id_bytes[..]],
                    |r| r.get(0),
                )
                .map_err(|e| {
                    crate::error::CoreError::Storage(crate::storage::StorageErrorKind::Other(
                        e.to_string(),
                    ))
                })
            })
            .unwrap();
        assert_eq!(
            msg_count, 0,
            "ContactCardUpdate must not create a messages row"
        );
    }

    /// ContactCardUpdate where the card claims a different identity than the
    /// MLS-authenticated sender: must be rejected with an error and must
    /// not persist the card or emit any event.
    #[tokio::test]
    async fn contact_card_update_with_mismatched_identity_is_rejected() {
        use crate::contact::Contact;
        use crate::mls::key_package::KeyPackage;
        use crate::storage::key_packages::KeyPackageRepo;

        let pool = Arc::new(Pool::in_memory());
        let (events_tx, mut rx) = broadcast::channel::<Event>(16);

        // Set up identities: alice, bob (sender), charlie (identity claimed in card).
        let alice_seed = crate::identity::Seed::generate().unwrap();
        let alice_id = crate::identity::IdentityKey::from_seed(&alice_seed).unwrap();
        let bob_seed = crate::identity::Seed::generate().unwrap();
        let bob_id = crate::identity::IdentityKey::from_seed(&bob_seed).unwrap();
        let charlie_seed = crate::identity::Seed::generate().unwrap();
        let charlie_id = crate::identity::IdentityKey::from_seed(&charlie_seed).unwrap();
        let peer = bob_id.public(); // MLS sender is bob

        // Build 2-member MLS group.
        let bob_provider = crate::mls::provider::MlsProvider::new();
        let kp_repo = KeyPackageRepo::new(&pool);
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo).unwrap();
        let mut alice_group = crate::mls::Group::create_solo(
            &alice_id,
            None,
            crate::mls::provider::MlsProvider::new(),
        )
        .unwrap();
        let (welcome, _commit) = alice_group.add_member(&bob_kp, None).unwrap();
        let group_id_bytes = alice_group.id().0.clone();
        let mut bob_group =
            crate::mls::Group::join_from_welcome(&bob_id, &welcome, None, bob_provider).unwrap();

        // Pre-upsert both contacts.
        let contact_repo = crate::storage::ContactRepo::new(&pool);
        for (id, name) in [(&bob_id, "Bob"), (&charlie_id, "Charlie")] {
            contact_repo
                .upsert(&Contact {
                    identity: id.public(),
                    display_name: Some(name.into()),
                    added_at: 0,
                    card: None,
                })
                .unwrap();
        }

        // Bob signs a card claiming CHARLIE's identity (the mismatch).
        let now_secs = now_unix_seconds();
        // We build the card manually: sign with charlie's key but the
        // MLS sender is bob — so the identity in the card won't match `peer`.
        let card = crate::contact::ContactCard::sign(
            &charlie_id, // signer != MLS sender (bob)
            "cccc.onion".into(),
            vec![],
            1,
            3600,
            now_secs,
        )
        .unwrap();

        // Bob encrypts the forged card inside his MLS session.
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
            kind: Kind::ContactCardUpdate {
                card: Box::new(card),
            },
        };
        let ciphertext = bob_group.encrypt(&env).unwrap();

        // Persist alice's group state.
        let group_repo = MlsGroupRepo::new(&pool);
        alice_group.save(&group_repo).unwrap();

        let inbound = DaemonInbound::new(pool.clone(), events_tx.clone());
        let result = inbound.dispatch_for_group(peer, &group_id_bytes, &ciphertext);

        // Must return Err.
        assert!(
            result.is_err(),
            "mismatched identity card must be rejected; got Ok"
        );

        // No event must be emitted.
        match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
            Err(_timeout) => {} // expected: no events
            Ok(other) => panic!("unexpected event: {other:?}"),
        }

        // No card must have been persisted for bob.
        assert!(
            contact_repo.latest_card(&peer).unwrap().is_none(),
            "no card must be stored for bob after mismatch rejection"
        );
    }

    #[tokio::test]
    async fn dispatch_welcome_joins_group_and_emits_contact_updated() {
        use crate::mls::key_package::{key_package_ref, KeyPackage};
        use crate::mls::provider::MlsProvider;
        use crate::storage::key_packages::KeyPackageRepo;
        use crate::storage::OutstandingInviteRepo;

        let pool = Arc::new(Pool::in_memory());
        let (events_tx, mut rx) = broadcast::channel::<Event>(16);

        // Alice = inviter
        let alice = crate::identity::IdentityKey::from_seed(
            &crate::identity::Seed::generate().unwrap(),
        )
        .unwrap();
        let alice_provider = MlsProvider::new();
        let kp_repo = KeyPackageRepo::new(&pool);
        let alice_kp = KeyPackage::generate(&alice, &alice_provider, &kp_repo).unwrap();
        let alice_kp_ref = key_package_ref(&alice_kp).unwrap();
        let alice_kp_bytes = alice_kp.to_bytes().unwrap();

        // outstanding_invites row keyed on KeyPackageRef
        let psk_bytes = [0xABu8; 32];
        let provider_snap = alice_provider.snapshot().unwrap();
        let oi = OutstandingInviteRepo::new(&pool);
        oi.put_with_provider(
            &alice_kp_ref,
            &zeroize::Zeroizing::new(psk_bytes),
            &alice_kp_bytes,
            &provider_snap,
            crate::daemon::clock::now_unix_seconds() + 3600,
            crate::daemon::clock::now_unix_seconds(),
        )
        .unwrap();

        // Bob builds his solo group, adds Alice's KP
        let bob = crate::identity::IdentityKey::from_seed(
            &crate::identity::Seed::generate().unwrap(),
        )
        .unwrap();
        let mut bob_group =
            crate::mls::Group::create_solo(&bob, Some(&psk_bytes), MlsProvider::new()).unwrap();
        let alice_kp_for_add = KeyPackage::from_bytes(&alice_kp_bytes).unwrap();
        let (welcome_bytes, _commit) =
            bob_group.add_member(&alice_kp_for_add, Some(&psk_bytes)).unwrap();

        // Drive Alice's dispatch_welcome
        let alice_arc = Arc::new(alice);
        let inbound = DaemonInbound::new(pool.clone(), events_tx.clone());
        inbound.set_identity(alice_arc.clone());
        let bob_pubkey = bob.public();
        let result = crate::delivery::peer::InboundDispatch::dispatch_welcome(
            &inbound,
            bob_pubkey,
            &welcome_bytes,
        );
        assert!(result.is_some(), "dispatch_welcome must succeed");
        let returned_id = result.unwrap();
        assert_eq!(
            returned_id.0,
            crate::delivery::peer::welcome_msg_id(&welcome_bytes).0
        );

        // ContactUpdated event must fire
        match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await {
            Ok(Ok(Event::ContactUpdated(p))) => assert_eq!(p, bob_pubkey),
            other => panic!("expected ContactUpdated, got {other:?}"),
        }

        // outstanding_invites row gone
        assert!(oi.get_psk(&alice_kp_ref).unwrap().is_none());

        // Alice's contact for Bob exists with group_id linked
        let cr = crate::storage::ContactRepo::new(&pool);
        let stored = cr
            .get(&bob_pubkey)
            .unwrap()
            .unwrap_or_else(|| panic!("contact must be persisted"));
        let gid = cr
            .get_group_id(&bob_pubkey)
            .unwrap()
            .unwrap_or_else(|| panic!("gid must be set"));
        assert_eq!(gid.len(), 32);
        assert_eq!(stored.identity, bob_pubkey);
    }

    #[tokio::test]
    async fn dispatch_welcome_rejects_unknown_kp_hash() {
        let pool = Arc::new(Pool::in_memory());
        let (events_tx, _rx) = broadcast::channel::<Event>(16);
        let alice_arc = Arc::new(
            crate::identity::IdentityKey::from_seed(
                &crate::identity::Seed::generate().unwrap(),
            )
            .unwrap(),
        );
        let inbound = DaemonInbound::new(pool, events_tx);
        inbound.set_identity(alice_arc);

        let result = crate::delivery::peer::InboundDispatch::dispatch_welcome(
            &inbound,
            crate::identity::PublicKey([0xCCu8; 32]),
            b"not a real welcome",
        );
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn dispatch_welcome_rejects_expired_invite() {
        use crate::mls::key_package::{key_package_ref, KeyPackage};
        use crate::mls::provider::MlsProvider;
        use crate::storage::key_packages::KeyPackageRepo;
        use crate::storage::OutstandingInviteRepo;

        let pool = Arc::new(Pool::in_memory());
        let (events_tx, _rx) = broadcast::channel::<Event>(16);
        let alice = crate::identity::IdentityKey::from_seed(
            &crate::identity::Seed::generate().unwrap(),
        )
        .unwrap();
        let kp_repo = KeyPackageRepo::new(&pool);
        let alice_kp = KeyPackage::generate(&alice, &MlsProvider::new(), &kp_repo).unwrap();
        let alice_kp_ref = key_package_ref(&alice_kp).unwrap();
        let alice_kp_bytes = alice_kp.to_bytes().unwrap();

        let psk = [0u8; 32];
        let provider_snap = MlsProvider::new().snapshot().unwrap();
        OutstandingInviteRepo::new(&pool)
            .put_with_provider(
                &alice_kp_ref,
                &zeroize::Zeroizing::new(psk),
                &alice_kp_bytes,
                &provider_snap,
                crate::daemon::clock::now_unix_seconds() - 1, // expired
                crate::daemon::clock::now_unix_seconds() - 3600,
            )
            .unwrap();

        let bob = crate::identity::IdentityKey::from_seed(
            &crate::identity::Seed::generate().unwrap(),
        )
        .unwrap();
        let mut bob_group =
            crate::mls::Group::create_solo(&bob, Some(&psk), MlsProvider::new()).unwrap();
        let alice_kp_for_add = KeyPackage::from_bytes(&alice_kp_bytes).unwrap();
        let (welcome_bytes, _) = bob_group.add_member(&alice_kp_for_add, Some(&psk)).unwrap();

        let alice_arc = Arc::new(alice);
        let inbound = DaemonInbound::new(pool, events_tx);
        inbound.set_identity(alice_arc);

        let result = crate::delivery::peer::InboundDispatch::dispatch_welcome(
            &inbound,
            bob.public(),
            &welcome_bytes,
        );
        assert!(result.is_none(), "expired invite must not be processed");
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

    /// Rollback test: when `receive_in_tx` rejects the envelope (ts outside
    /// ±1h replay window), the wrapping transaction rolls back and storage
    /// must be entirely unchanged — the MLS ratchet blob on disk must match
    /// its pre-dispatch snapshot and no `messages` or `seen_messages` rows
    /// must have been inserted.
    ///
    /// This exercises the atomicity guarantee introduced in Task 9: before
    /// the C1 fix, the closure returned `Ok(Rejected)`, causing
    /// `Pool::transaction` to commit the advanced-ratchet snapshot with no
    /// corresponding history row. Now the closure maps `Rejected` to `Err`,
    /// which rolls the transaction back before commit.
    ///
    /// The epoch-equality assertion that appeared in the original version of
    /// this test is invariant under `decrypt()` — `Group::epoch()` only
    /// advances on Commit processing, so it always equalled the pre-dispatch
    /// value regardless of rollback. The blob comparison below is the real
    /// load-bearing check.
    #[tokio::test]
    async fn dispatch_for_group_rollback_leaves_storage_unchanged() {
        use crate::mls::key_package::KeyPackage;
        use crate::storage::key_packages::KeyPackageRepo;

        let pool = Arc::new(Pool::in_memory());
        let (events_tx, _rx) = broadcast::channel::<Event>(16);

        // Identities.
        let alice_seed = crate::identity::Seed::generate().unwrap();
        let alice_id = crate::identity::IdentityKey::from_seed(&alice_seed).unwrap();
        let bob_seed = crate::identity::Seed::generate().unwrap();
        let bob_id = crate::identity::IdentityKey::from_seed(&bob_seed).unwrap();
        let peer = bob_id.public();

        // Build 2-member group.
        let bob_provider = MlsProvider::new();
        let kp_repo = KeyPackageRepo::new(&pool);
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo).unwrap();
        let mut alice_group =
            crate::mls::Group::create_solo(&alice_id, None, MlsProvider::new()).unwrap();
        let (welcome, _commit) = alice_group.add_member(&bob_kp, None).unwrap();
        let group_id_bytes = alice_group.id().0.clone();
        let mut bob_group =
            crate::mls::Group::join_from_welcome(&bob_id, &welcome, None, bob_provider).unwrap();

        // Bob encrypts an envelope with a ts that is 10 hours in the past —
        // far outside the ±1h replay window. `dispatch_for_group` must
        // reject it and roll back.
        let stale_ts_ms: i64 = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
        )
        .unwrap_or(0)
            - 10 * 3600 * 1000; // 10 hours ago

        let msg_id = MessageId::generate();
        let env = crate::envelope::Envelope {
            v: 1,
            id: msg_id,
            ts: stale_ts_ms,
            reply_to: None,
            kind: Kind::Text {
                body: "stale".into(),
            },
        };
        let ciphertext = bob_group.encrypt(&env).unwrap();

        // Persist alice's initial group state and snapshot the raw blob.
        let group_repo = MlsGroupRepo::new(&pool);
        alice_group.save(&group_repo).unwrap();

        let blob_before: Vec<u8> = pool
            .with(|c| {
                c.query_row(
                    "SELECT state_blob FROM mls_groups WHERE group_id = ?1",
                    rusqlite::params![&group_id_bytes[..]],
                    |r| r.get::<_, Vec<u8>>(0),
                )
                .map_err(|e| {
                    crate::error::CoreError::Storage(crate::storage::StorageErrorKind::Other(
                        e.to_string(),
                    ))
                })
            })
            .unwrap();

        let inbound = DaemonInbound::new(pool.clone(), events_tx);

        // dispatch_for_group must return Err (rejected by replay window).
        let result = inbound.dispatch_for_group(peer, &group_id_bytes, &ciphertext);
        assert!(
            result.is_err(),
            "expected Err for stale-ts envelope, got Ok"
        );

        // 1. Raw state_blob must be identical to the pre-dispatch snapshot.
        let blob_after: Vec<u8> = pool
            .with(|c| {
                c.query_row(
                    "SELECT state_blob FROM mls_groups WHERE group_id = ?1",
                    rusqlite::params![&group_id_bytes[..]],
                    |r| r.get::<_, Vec<u8>>(0),
                )
                .map_err(|e| {
                    crate::error::CoreError::Storage(crate::storage::StorageErrorKind::Other(
                        e.to_string(),
                    ))
                })
            })
            .unwrap();
        assert_eq!(
            blob_before, blob_after,
            "rollback must leave mls_groups.state_blob untouched"
        );

        // 2. No messages row must have been persisted for this group.
        let msg_count: i64 = pool
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM messages WHERE group_id = ?1",
                    rusqlite::params![&group_id_bytes[..]],
                    |r| r.get(0),
                )
                .map_err(|e| {
                    crate::error::CoreError::Storage(crate::storage::StorageErrorKind::Other(
                        e.to_string(),
                    ))
                })
            })
            .unwrap();
        assert_eq!(msg_count, 0, "rollback must not persist a messages row");

        // 3. No seen_messages row must have been inserted for this
        //    (sender, message_id) pair.
        let seen_count: i64 = pool
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM seen_messages WHERE sender = ?1 AND message_id = ?2",
                    rusqlite::params![&peer.0[..], &msg_id.0[..]],
                    |r| r.get(0),
                )
                .map_err(|e| {
                    crate::error::CoreError::Storage(crate::storage::StorageErrorKind::Other(
                        e.to_string(),
                    ))
                })
            })
            .unwrap();
        assert_eq!(
            seen_count, 0,
            "rollback must not persist a seen_messages row"
        );
    }
}
