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
    /// Per-group ratchet-serialization registry (T1-3). This is the SAME Arc
    /// the send-side `DaemonHandle` holds (wired in `Daemon::run`), so an
    /// inbound decrypt+save and a concurrent outbound encrypt+save on one group
    /// serialize on the same lock. Defaults to a private registry (used by unit
    /// tests that exercise only the inbound path).
    group_locks: crate::daemon::handle::GroupLockRegistry,
    /// Per-peer queue of inbound attachments whose manifest just arrived,
    /// awaiting the peer actor's `take_begin_attachment` drain.
    begins: std::sync::Mutex<
        std::collections::HashMap<
            PublicKey,
            std::collections::VecDeque<crate::delivery::chunk_transfer::AttachmentBegin>,
        >,
    >,
    /// Chunk blob store for the offline attachment receiver (Phase 3.C).
    /// `None` until wired by `set_chunk_store`.
    pub chunk_store:
        std::sync::RwLock<Option<std::sync::Arc<crate::attachment::store::ChunkStore>>>,
    /// Directory where reassembled files are written (Phase 3.C).
    /// `None` until wired by `set_download_dir`.
    pub download_dir: std::sync::RwLock<Option<std::path::PathBuf>>,
}

impl DaemonInbound {
    /// Create a new `DaemonInbound` with a private group-lock registry. Used by
    /// unit tests that drive only the inbound path. Production callers
    /// (`Daemon::run`) must call [`set_group_locks`] with the send handle's
    /// registry so send + receive serialize together (T1-3).
    pub(crate) fn new(pool: Arc<Pool>, events_tx: broadcast::Sender<Event>) -> Self {
        Self {
            pool,
            events_tx,
            identity: std::sync::RwLock::new(None),
            group_locks: crate::daemon::handle::new_group_lock_registry(),
            begins: std::sync::Mutex::new(std::collections::HashMap::new()),
            chunk_store: std::sync::RwLock::new(None),
            download_dir: std::sync::RwLock::new(None),
        }
    }

    /// Replace the group-lock registry with the shared one the send-side
    /// `DaemonHandle` holds, so inbound decrypt+save and outbound encrypt+save
    /// on the same group serialize on the same lock (T1-3). Called by
    /// `Daemon::run` before the inbound dispatcher starts processing frames.
    pub(crate) fn set_group_locks(&mut self, registry: crate::daemon::handle::GroupLockRegistry) {
        self.group_locks = registry;
    }

    /// Wire the local identity so Welcome messages can be processed.
    /// Must be called before any Welcome frame can arrive.
    pub(crate) fn set_identity(&self, identity: Arc<IdentityKey>) {
        if let Ok(mut guard) = self.identity.write() {
            *guard = Some(identity);
        }
    }

    /// Wire the chunk store for the offline attachment receiver (Phase 3.C).
    pub(crate) fn set_chunk_store(
        &self,
        store: std::sync::Arc<crate::attachment::store::ChunkStore>,
    ) {
        if let Ok(mut g) = self.chunk_store.write() {
            *g = Some(store);
        }
    }

    /// Wire the download directory for offline attachment reassembly (Phase 3.C).
    pub(crate) fn set_download_dir(&self, dir: std::path::PathBuf) {
        if let Ok(mut g) = self.download_dir.write() {
            *g = Some(dir);
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
        self.dispatch_for_group(from, &group_id, ciphertext, true)
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
        enforce_ts_window: bool,
    ) -> Result<MessageId> {
        // Per-group ratchet serialization (T1-3): hold the group's lock across
        // the entire load→decrypt→save critical section so a concurrent
        // outbound send (or another inbound frame) on this group can't load the
        // same on-disk snapshot and process at the same ratchet generation.
        // This is the SAME registry the send-side `DaemonHandle` uses (wired in
        // `Daemon::run`), so send + receive serialize together. The whole
        // function is synchronous, so a blocking `std::sync::Mutex` guard held
        // for its duration is correct. A group_id that isn't 32 bytes is an
        // MLS-storage anomaly — surfaced as the same unknown-group error below.
        let group_id_arr: [u8; 32] = group_id.try_into().map_err(|_| {
            CoreError::from(MlsErrorKind::Other(
                "mls: inbound: bad group_id length".into(),
            ))
        })?;
        let group_lock = crate::daemon::handle::group_lock_for(&self.group_locks, &group_id_arr);
        let _guard = group_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let group_repo = MlsGroupRepo::new(&self.pool);
        let gid = GroupId(group_id.to_vec());
        let mut group = Group::load(&gid, &group_repo)?.ok_or_else(|| {
            CoreError::from(MlsErrorKind::Other("mls: inbound: unknown group_id".into()))
        })?;

        let Some(envelope) = group.decrypt(ciphertext)? else {
            // The inbound message was a Commit (not an application message):
            // it advanced the ratchet on `group` in memory but carries no
            // payload. Persist the advanced group so the epoch bump is durable
            // (otherwise the next inbound message would fail to decrypt), then
            // ACK without a message row or `MessageReceived` event (T2-2,
            // defensive — v1.0 does not perform PCS). Hold the per-group lock,
            // already acquired above, across this save.
            let saved = self
                .pool
                .transaction(|tx| group.save_in_tx(&group_repo, tx));
            saved?;
            // No envelope id is available (a Commit has no Envelope). Return a
            // fresh id so the caller's ACK contract (which only needs *an* id to
            // stop the sender retrying) is satisfied.
            return Ok(MessageId::generate());
        };

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

        // --- Kind::File: stage the manifest + queue the chunk fetch (3.B). ---
        // The manifest message ALSO falls through to the normal persist below so
        // it appears in history and emits Event::MessageReceived; this block only
        // adds the attachment side-effects.
        if let crate::envelope::Kind::File {
            manifest: manifest_bytes,
        } = &envelope.kind
        {
            match crate::attachment::manifest::AttachmentManifest::from_cbor(manifest_bytes) {
                Ok(m)
                    if m.total_size <= crate::attachment::MAX_ATTACHMENT_BYTES
                        && !m.chunks.is_empty() =>
                {
                    let repo = crate::storage::attachments::AttachmentRepo::new(&self.pool);
                    let total_chunks = m.chunks.len() as i64;
                    if let Err(e) = repo.insert(
                        &m.attachment_id,
                        "in",
                        manifest_bytes,
                        total_chunks,
                        now_unix_seconds(),
                    ) {
                        tracing::warn!(err = %e, "inbound: attachment manifest insert failed");
                    } else {
                        // Record the sender so the offline path (finalize_offline)
                        // can populate Event::AttachmentReceived.contact (Phase 3.C).
                        let _ = repo.set_peer(&m.attachment_id, &from.0);
                        let begin = crate::delivery::chunk_transfer::AttachmentBegin {
                            attachment_id: m.attachment_id,
                            manifest: m,
                        };
                        if let Ok(mut map) = self.begins.lock() {
                            map.entry(from).or_default().push_back(begin);
                        }
                    }
                }
                Ok(_) => tracing::warn!("inbound: rejecting oversize/empty attachment manifest"),
                Err(e) => {
                    tracing::warn!(err = %e, "inbound: bad attachment manifest, skipping fetch")
                }
            }
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
                enforce_ts_window,
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

    /// Trial-decrypt a mailbox-fetched ciphertext against each known group.
    /// On the first group that decrypts, attribute the peer via
    /// `contact_for_group` and persist through the transactional
    /// `dispatch_for_group` path (which re-loads from disk, so the trial
    /// decrypt above — performed on an in-memory copy that is never saved —
    /// does not consume the on-disk message key).
    ///
    /// NOTE: trial-decrypt is O(groups) per deposit (each iteration does a full
    /// `Group::load`). Fine for v1.0's 2-member-only groups; revisit if the
    /// group count grows.
    ///
    /// The mailbox path passes `enforce_ts_window = false`: store-and-forward
    /// deposits are legitimately old, so replay resistance comes from the
    /// `(sender, envelope_id)` dedup + MLS generation + server delete, not the
    /// ±1h window (which still guards the live direct path).
    fn dispatch_mailbox_inner(&self, ciphertext: &[u8]) -> Option<MessageId> {
        let group_repo = MlsGroupRepo::new(&self.pool);
        let groups = match group_repo.list() {
            Ok(groups) => groups,
            Err(e) => {
                tracing::warn!(error = %e, "inbound: mailbox dispatch could not list groups");
                return None;
            }
        };
        for (gid_bytes, _epoch) in groups {
            let gid = GroupId(gid_bytes.clone());
            let mut g = match Group::load(&gid, &group_repo) {
                Ok(Some(g)) => g,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(error = %e, "inbound: mailbox dispatch skipping unloadable group");
                    continue;
                }
            };
            // Trial decrypt on the in-memory copy; do NOT save. Only an
            // application message (`Ok(Some(_))`) identifies the owning group;
            // `Ok(None)` is a Commit (no user payload) and `Err` is a
            // non-matching group — skip both (a mailbox-delivered Commit is not
            // a user message and is not persisted here).
            if !matches!(g.decrypt(ciphertext), Ok(Some(_))) {
                continue;
            }
            let gid_arr: [u8; 32] = match gid_bytes.as_slice().try_into() {
                Ok(a) => a,
                Err(_) => continue,
            };
            let peer = match ContactRepo::new(&self.pool).contact_for_group(&gid_arr) {
                Ok(Some(p)) => p,
                _ => continue,
            };
            // Re-load + decrypt + persist atomically against the on-disk state.
            return self
                .dispatch_for_group(peer, &gid_bytes, ciphertext, false)
                .ok();
        }
        None
    }

    /// Join an MLS group from a Welcome, persist group + contact + consumed
    /// markers atomically, and emit `Event::ContactUpdated`.
    ///
    /// The invitee's identity is **derived from the joined group**
    /// ([`Group::peer_identity`]) — not passed in — so this is correct even when
    /// the caller (the accept loop) has only the sender's X25519 static. If
    /// `bind_x25519` is `Some`, the derived identity is required to bind to it
    /// via `ed25519_pub_to_x25519` **before** anything is committed (the
    /// transport↔MLS identity binding; see ADR 0007). Returns the derived peer.
    fn welcome_join_persist(
        &self,
        welcome_bytes: &[u8],
        bind_x25519: Option<&[u8; 32]>,
        // The handshake's transport↔MLS binding value (ADR 0009). When `Some`,
        // it is registered as the genesis commit's second external PSK before
        // `join_from_welcome`, so OpenMLS validates the binding while processing
        // the genesis Commit carried by the Welcome (active since 2.A Task 4).
        // `None` (the established-conn `dispatch_welcome` path) registers no
        // transport binding. Typed `Option` — never a `[0u8; 32]` sentinel — so
        // a zero array can never masquerade as a real binding.
        h_transport: Option<&[u8; 32]>,
    ) -> Result<PublicKey> {
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

        // Invite PSK id is derived per-invite from the same KeyPackageRef the
        // committer used (ADR 0009, T2-8). The h_transport binding (second
        // external PSK, same kp_ref) is registered when present — both ids are
        // keyed by `kp_ref` so OpenMLS resolves the same two PSKs the committer
        // proposed in the genesis Commit (ADR 0009, T1-1).
        let group = Group::join_from_welcome(
            &identity_arc,
            welcome_bytes,
            Some((&kp_ref, &*psk)),
            h_transport.map(|h| (&kp_ref, h)),
            provider,
        )?;

        // Self-attribute: the contact's Ed25519 identity is the OTHER member's
        // MLS signature key, derived from the joined group.
        let derived = group.peer_identity()?;

        // Transport↔MLS binding (security-critical): in the honest flow the
        // invitee dials with its own identity as the Noise static, so the
        // handshake's peer_x25519 equals ed25519_pub_to_x25519(invitee). A
        // mismatch means the Welcome arrived over a connection not bound to the
        // invitee's identity (relay/MITM) — abort BEFORE committing.
        if let Some(expected) = bind_x25519 {
            let vk = ed25519_dalek::VerifyingKey::from_bytes(&derived.0).map_err(|_| {
                CoreError::from(MlsErrorKind::Other(
                    "inbound welcome: derived identity not a valid key".into(),
                ))
            })?;
            if crate::identity::key::ed25519_pub_to_x25519(&vk) != *expected {
                return Err(CoreError::from(MlsErrorKind::Other(
                    "inbound welcome: transport identity does not bind to MLS identity".into(),
                )));
            }
        }

        let group_id = group.id().0.clone();

        // Per-group ratchet serialization (T1-3): take the new group's lock
        // around the genesis save. The group doesn't exist on disk yet, but
        // holding the lock keyed by its id is correct + cheap and keeps the
        // first message after join from racing this persist. Fully synchronous,
        // so a blocking guard for the save section is fine. A non-32-byte
        // group_id is an MLS anomaly — surfaced as an error.
        let group_id_arr: [u8; 32] = group_id.as_slice().try_into().map_err(|_| {
            CoreError::from(MlsErrorKind::Other(
                "inbound welcome: bad group_id length".into(),
            ))
        })?;
        let group_lock = crate::daemon::handle::group_lock_for(&self.group_locks, &group_id_arr);
        let _guard = group_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let group_repo = MlsGroupRepo::new(&self.pool);
        let kp_repo = KeyPackageRepo::new(&self.pool);

        self.pool.transaction(|tx| {
            group.save_in_tx(&group_repo, tx)?;
            // Execute contact upsert + group_id link directly on `tx` to
            // avoid re-locking the Pool mutex (pool.transaction already holds
            // the mutex; ContactRepo::upsert / set_group_id call pool.with_mut
            // and would deadlock).
            tx.execute(
                // hidden=0 on conflict mirrors ContactRepo::upsert_in_tx: a
                // previously-removed (soft-deleted) contact that rejoins via an
                // inbound Welcome must be un-hidden, or it stays invisible.
                "INSERT INTO contacts (identity_pubkey, display_name, added_at) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(identity_pubkey) DO UPDATE SET display_name=excluded.display_name, hidden=0",
                rusqlite::params![&derived.0[..], Option::<String>::None, now],
            )
            .map_err(|e| {
                CoreError::Storage(crate::storage::StorageErrorKind::Other(format!(
                    "welcome: upsert contact: {e}"
                )))
            })?;
            tx.execute(
                "UPDATE contacts SET group_id = ?1 WHERE identity_pubkey = ?2",
                rusqlite::params![&group_id[..], &derived.0[..]],
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

        let _ = self.events_tx.send(Event::ContactUpdated(derived));
        Ok(derived)
    }

    /// Mark a completed offline attachment as complete and emit
    /// `Event::AttachmentReceived`. Encrypted chunks are retained in the
    /// `ChunkStore` for on-demand decryption; nothing is written to disk here.
    /// The sender's pubkey is looked up from the `peer` column written when the
    /// manifest was ingested; falls back to `PublicKey([0u8; 32])` if unavailable
    /// (should not happen for well-formed 'in' rows).
    fn finalize_offline(
        &self,
        attachment_id: &[u8; 16],
        manifest: &crate::attachment::manifest::AttachmentManifest,
    ) {
        let safe = crate::delivery::chunk_transfer::sanitize_filename(&manifest.filename);
        let repo = crate::storage::attachments::AttachmentRepo::new(&self.pool);
        // Resolve the sender recorded at manifest-ingest time.
        let contact = repo
            .peer_for(attachment_id)
            .ok()
            .flatten()
            .map(PublicKey)
            .unwrap_or(PublicKey([0u8; 32]));
        // Fire-gate: only the lane that flips pending→complete emits, so the
        // direct/offline lanes can't race and produce duplicate events.
        match repo.set_status_if_pending(attachment_id) {
            Ok(true) => {
                // Encrypted-at-rest: retain chunks, do not reassemble. Emit availability.
                let _ = self.events_tx.send(Event::AttachmentReceived {
                    contact,
                    attachment_id: crate::daemon::hex::Hex16::from(*attachment_id),
                    filename: safe,
                    mime: manifest.mime.clone(),
                    size: manifest.total_size,
                });
            }
            Ok(false) => {
                // Another lane already finalized; chunks are shared + retained, nothing to do.
            }
            Err(e) => {
                tracing::warn!(err = %e, "inbound: failed to mark attachment complete");
            }
        }
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
        // Welcome over an already-established connection: no binding check (the
        // connection is already attributed to `peer`); derive + persist, then
        // assert the derived identity matches the bound peer (defense in depth).
        // No h_transport is available on this path: the connection predates
        // this Welcome, so there is no fresh handshake transcript to bind. Pass
        // `None` STRUCTURALLY — the binding is registered only on the bootstrap
        // (fresh-handshake) path; a zero array must never stand in for a real
        // binding here.
        match self.welcome_join_persist(welcome, None, None) {
            Ok(derived) => {
                if derived != peer {
                    // `peer` is an Ed25519 pubkey — not logged (warn is >= info;
                    // CLAUDE.md forbids pubkeys at info+). Static text only.
                    tracing::warn!("inbound: welcome derived identity differs from connected peer");
                }
                Some(crate::delivery::peer::welcome_msg_id(welcome))
            }
            Err(e) => {
                // `e` Display is onion/pubkey-free static text. Do NOT log `peer`.
                tracing::warn!(err = %e, "inbound: dispatch_welcome failed, not ACKing");
                None
            }
        }
    }

    fn dispatch_welcome_bootstrap(
        &self,
        welcome: &[u8],
        expected_x25519: &[u8; 32],
        h_transport: Option<&[u8; 32]>,
    ) -> Option<PublicKey> {
        match self.welcome_join_persist(welcome, Some(expected_x25519), h_transport) {
            Ok(derived) => Some(derived),
            Err(_e) => {
                // Static, onion/pubkey-free warn: do not leak the rejected
                // Welcome bytes, the derived identity, or the expected x25519.
                tracing::warn!("inbound: welcome bootstrap rejected");
                None
            }
        }
    }

    fn dispatch_mailbox(&self, ciphertext: &[u8]) -> Option<MessageId> {
        match self.dispatch_mailbox_inner(ciphertext) {
            Some(mid) => Some(mid),
            None => {
                tracing::warn!(
                    "inbound: mailbox dispatch found no matching group; deposit retained"
                );
                None
            }
        }
    }

    fn dispatch_attachment_chunk(&self, ciphertext: &[u8]) -> bool {
        use sha2::{Digest, Sha256};
        let store = match self.chunk_store.read() {
            Ok(g) => match g.as_ref() {
                Some(s) => s.clone(),
                None => return false,
            },
            Err(_) => return false,
        };
        let hash: [u8; 32] = Sha256::digest(ciphertext).into();

        let repo = crate::storage::attachments::AttachmentRepo::new(&self.pool);
        let pending = match repo.list_pending_in() {
            Ok(v) => v,
            Err(_) => return false,
        };
        for (attachment_id, manifest_bytes) in pending {
            let manifest =
                match crate::attachment::manifest::AttachmentManifest::from_cbor(&manifest_bytes) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
            let Some(chunk_ref) = manifest.chunks.iter().find(|c| c.ciphertext_hash == hash) else {
                continue;
            };
            let index = chunk_ref.index;
            // Dedup: already held → report handled (delete from server) without re-storing.
            let already = repo.received_indices(&attachment_id).unwrap_or_default();
            if already.contains(&index) {
                return true;
            }
            if store.put(&attachment_id, index, ciphertext).is_err() {
                return false; // could not store; leave on server for retry
            }
            if repo.mark_received(&attachment_id, index).is_err() {
                return false;
            }
            // Completion check.
            let now = repo
                .received_indices(&attachment_id)
                .map(|v| v.len() as i64)
                .unwrap_or(0);
            if now >= manifest.chunks.len() as i64 {
                self.finalize_offline(&attachment_id, &manifest);
            } else if now % 8 == 0 {
                let _ = self
                    .events_tx
                    .send(crate::daemon::events::Event::AttachmentProgress {
                        attachment_id: crate::daemon::hex::Hex16::from(attachment_id),
                        received: now as u32,
                        total: manifest.chunks.len() as u32,
                    });
            }
            return true;
        }
        false
    }

    fn take_begin_attachment(
        &self,
        peer: PublicKey,
    ) -> Option<crate::delivery::chunk_transfer::AttachmentBegin> {
        self.begins.lock().ok()?.get_mut(&peer)?.pop_front()
    }

    fn attachment_received(
        &self,
        contact: PublicKey,
        attachment_id: [u8; 16],
        filename: &str,
        mime: &str,
        size: u64,
    ) {
        let _ = self.events_tx.send(Event::AttachmentReceived {
            contact,
            attachment_id: crate::daemon::hex::Hex16::from(attachment_id),
            filename: filename.to_string(),
            mime: mime.to_string(),
            size,
        });
    }

    fn attachment_progress(&self, attachment_id: [u8; 16], received: u32, total: u32) {
        let _ = self.events_tx.send(Event::AttachmentProgress {
            attachment_id: crate::daemon::hex::Hex16::from(attachment_id),
            received,
            total,
        });
    }

    fn attachment_failed(&self, attachment_id: [u8; 16], reason: &str) {
        let _ = self.events_tx.send(Event::AttachmentFailed {
            attachment_id: crate::daemon::hex::Hex16::from(attachment_id),
            reason: reason.to_string(),
        });
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
            crate::mls::Group::create_solo(&alice_id, None, None, MlsProvider::new()).unwrap();
        let (welcome, _commit) = alice_group.add_member(&bob_kp, None, None).unwrap();
        let group_id_bytes = alice_group.id().0.clone();

        // Bob joins from the Welcome — this is bob's group state.
        let mut bob_group =
            crate::mls::Group::join_from_welcome(&bob_id, &welcome, None, None, bob_provider)
                .unwrap();

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
            .dispatch_for_group(peer, &group_id_bytes, &ciphertext, true)
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
            crate::mls::Group::create_solo(&alice_id, None, None, MlsProvider::new()).unwrap();
        let (welcome, _commit) = alice_group.add_member(&bob_kp, None, None).unwrap();
        let group_id_bytes = alice_group.id().0.clone();
        let mut bob_group =
            crate::mls::Group::join_from_welcome(&bob_id, &welcome, None, None, bob_provider)
                .unwrap();

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
            .dispatch_for_group(peer, &group_id_bytes, &ciphertext, true)
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
            None,
            crate::mls::provider::MlsProvider::new(),
        )
        .unwrap();
        let (welcome, _commit) = alice_group.add_member(&bob_kp, None, None).unwrap();
        let group_id_bytes = alice_group.id().0.clone();
        let mut bob_group =
            crate::mls::Group::join_from_welcome(&bob_id, &welcome, None, None, bob_provider)
                .unwrap();

        // Pre-upsert bob as a contact in alice's pool so put_card has a row to attach to.
        let contact_repo = crate::storage::ContactRepo::new(&pool);
        contact_repo
            .upsert(&Contact {
                identity: peer,
                display_name: Some("Bob".into()),
                added_at: 0,
                card: None,
                muted: false,
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
            .dispatch_for_group(peer, &group_id_bytes, &ciphertext, true)
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
            None,
            crate::mls::provider::MlsProvider::new(),
        )
        .unwrap();
        let (welcome, _commit) = alice_group.add_member(&bob_kp, None, None).unwrap();
        let group_id_bytes = alice_group.id().0.clone();
        let mut bob_group =
            crate::mls::Group::join_from_welcome(&bob_id, &welcome, None, None, bob_provider)
                .unwrap();

        // Pre-upsert both contacts.
        let contact_repo = crate::storage::ContactRepo::new(&pool);
        for (id, name) in [(&bob_id, "Bob"), (&charlie_id, "Charlie")] {
            contact_repo
                .upsert(&Contact {
                    identity: id.public(),
                    display_name: Some(name.into()),
                    added_at: 0,
                    card: None,
                    muted: false,
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
        let result = inbound.dispatch_for_group(peer, &group_id_bytes, &ciphertext, true);

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
        let alice =
            crate::identity::IdentityKey::from_seed(&crate::identity::Seed::generate().unwrap())
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
        let bob =
            crate::identity::IdentityKey::from_seed(&crate::identity::Seed::generate().unwrap())
                .unwrap();
        let mut bob_group = crate::mls::Group::create_solo(
            &bob,
            Some((&alice_kp_ref, &psk_bytes)),
            None,
            MlsProvider::new(),
        )
        .unwrap();
        let alice_kp_for_add = KeyPackage::from_bytes(&alice_kp_bytes).unwrap();
        let (welcome_bytes, _commit) = bob_group
            .add_member(&alice_kp_for_add, Some((&alice_kp_ref, &psk_bytes)), None)
            .unwrap();

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

    /// Build Alice (inviter) + an outstanding invite, then have Bob (invitee)
    /// produce a Welcome. Returns everything the bootstrap tests need.
    /// Mirrors `dispatch_welcome_joins_group_and_emits_contact_updated`.
    fn bootstrap_fixture(
        pool: &Arc<Pool>,
    ) -> (
        DaemonInbound,
        crate::identity::IdentityKey, // bob (invitee)
        Vec<u8>,                      // welcome_bytes
        [u8; 32],                     // alice_kp_ref (for outstanding-invite assertions)
        broadcast::Receiver<Event>,
    ) {
        use crate::mls::key_package::{key_package_ref, KeyPackage};
        use crate::storage::key_packages::KeyPackageRepo;
        use crate::storage::OutstandingInviteRepo;

        let (events_tx, rx) = broadcast::channel::<Event>(16);

        // Alice = inviter
        let alice =
            crate::identity::IdentityKey::from_seed(&crate::identity::Seed::generate().unwrap())
                .unwrap();
        let alice_provider = MlsProvider::new();
        let kp_repo = KeyPackageRepo::new(pool);
        let alice_kp = KeyPackage::generate(&alice, &alice_provider, &kp_repo).unwrap();
        let alice_kp_ref = key_package_ref(&alice_kp).unwrap();
        let alice_kp_bytes = alice_kp.to_bytes().unwrap();

        let psk_bytes = [0xABu8; 32];
        let provider_snap = alice_provider.snapshot().unwrap();
        let oi = OutstandingInviteRepo::new(pool);
        oi.put_with_provider(
            &alice_kp_ref,
            &zeroize::Zeroizing::new(psk_bytes),
            &alice_kp_bytes,
            &provider_snap,
            crate::daemon::clock::now_unix_seconds() + 3600,
            crate::daemon::clock::now_unix_seconds(),
        )
        .unwrap();

        // Bob = invitee; builds his solo group and adds Alice's KP.
        let bob =
            crate::identity::IdentityKey::from_seed(&crate::identity::Seed::generate().unwrap())
                .unwrap();
        let mut bob_group = crate::mls::Group::create_solo(
            &bob,
            Some((&alice_kp_ref, &psk_bytes)),
            None,
            MlsProvider::new(),
        )
        .unwrap();
        let alice_kp_for_add = KeyPackage::from_bytes(&alice_kp_bytes).unwrap();
        let (welcome_bytes, _commit) = bob_group
            .add_member(&alice_kp_for_add, Some((&alice_kp_ref, &psk_bytes)), None)
            .unwrap();

        let inbound = DaemonInbound::new(pool.clone(), events_tx);
        inbound.set_identity(Arc::new(alice));
        (inbound, bob, welcome_bytes, alice_kp_ref, rx)
    }

    /// Honest first-contact bootstrap: the binding x25519 equals
    /// `ed25519_pub_to_x25519(bob_identity)` (i.e. `bob.noise_static_public()`).
    /// Must return Bob's identity, persist contact+group, consume the invite,
    /// and emit `ContactUpdated`.
    #[tokio::test]
    async fn dispatch_welcome_bootstrap_succeeds_and_binds() {
        let pool = Arc::new(Pool::in_memory());
        let (inbound, bob, welcome_bytes, alice_kp_ref, mut rx) = bootstrap_fixture(&pool);
        let bob_pubkey = bob.public();
        let correct_x25519 = bob.noise_static_public();

        let derived = crate::delivery::peer::InboundDispatch::dispatch_welcome_bootstrap(
            &inbound,
            &welcome_bytes,
            &correct_x25519,
            None, // no transport binding registered: the fixture committer used None too
        );
        assert_eq!(
            derived,
            Some(bob_pubkey),
            "bootstrap must return Bob's derived identity"
        );

        // ContactUpdated event must fire for the derived peer.
        match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await {
            Ok(Ok(Event::ContactUpdated(p))) => assert_eq!(p, bob_pubkey),
            other => panic!("expected ContactUpdated, got {other:?}"),
        }

        // Invite consumed; contact + group_id persisted under Bob.
        let oi = crate::storage::OutstandingInviteRepo::new(&pool);
        assert!(
            oi.get_psk(&alice_kp_ref).unwrap().is_none(),
            "invite consumed"
        );
        let cr = crate::storage::ContactRepo::new(&pool);
        assert!(cr.get(&bob_pubkey).unwrap().is_some(), "contact persisted");
        let gid = cr.get_group_id(&bob_pubkey).unwrap();
        assert_eq!(gid.map(|g| g.len()), Some(32), "group_id linked");
    }

    /// Binding mismatch: a WRONG `expected_x25519` (a third party's static)
    /// must be REFUSED. The bootstrap returns `None` AND nothing is persisted —
    /// no contact, no group, the invite is NOT consumed. This is the
    /// anti-relay / anti-MITM guarantee of ADR 0007.
    #[tokio::test]
    async fn dispatch_welcome_bootstrap_rejects_binding_mismatch() {
        let pool = Arc::new(Pool::in_memory());
        let (inbound, bob, welcome_bytes, alice_kp_ref, mut rx) = bootstrap_fixture(&pool);
        let bob_pubkey = bob.public();

        // A relay's x25519 static — distinct from bob's own identity-derived one.
        let relay =
            crate::identity::IdentityKey::from_seed(&crate::identity::Seed::generate().unwrap())
                .unwrap();
        let wrong_x25519 = relay.noise_static_public();
        assert_ne!(
            wrong_x25519,
            bob.noise_static_public(),
            "test setup: relay static must differ from bob's"
        );

        let derived = crate::delivery::peer::InboundDispatch::dispatch_welcome_bootstrap(
            &inbound,
            &welcome_bytes,
            &wrong_x25519,
            None, // no transport binding registered: the fixture committer used None too
        );
        assert_eq!(derived, None, "binding mismatch must be refused");

        // NOTHING persisted: no contact, no group_id, invite NOT consumed.
        let cr = crate::storage::ContactRepo::new(&pool);
        assert!(
            cr.get(&bob_pubkey).unwrap().is_none(),
            "no contact may be persisted on binding mismatch"
        );
        let oi = crate::storage::OutstandingInviteRepo::new(&pool);
        assert!(
            oi.get_psk(&alice_kp_ref).unwrap().is_some(),
            "invite must NOT be consumed on binding mismatch"
        );

        // No event must be emitted.
        match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
            Err(_timeout) => {} // expected
            Ok(other) => panic!("unexpected event on binding mismatch: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_mailbox_accepts_old_ts_and_dedups_replays() {
        // ts-poison guardrail (Phase 2.C Task 2 + Task 7): a mailbox deposit
        // whose envelope `ts` is far older than the ±1h direct-path replay
        // window must NOT be rejected (store-and-forward deposits are
        // legitimately old), and a second dispatch of the SAME ciphertext must
        // be a dedup no-op — neither re-decryptable nor re-persisted.
        use crate::contact::Contact;
        use crate::mls::key_package::KeyPackage;
        use crate::storage::key_packages::KeyPackageRepo;
        use crate::storage::{ContactRepo, MessageRepo, MlsGroupRepo};

        let pool = Arc::new(Pool::in_memory());
        let (events_tx, mut rx) = broadcast::channel::<Event>(16);

        let alice_id =
            crate::identity::IdentityKey::from_seed(&crate::identity::Seed::generate().unwrap())
                .unwrap();
        let bob_id =
            crate::identity::IdentityKey::from_seed(&crate::identity::Seed::generate().unwrap())
                .unwrap();
        let bob_pk = bob_id.public();

        let bob_provider = MlsProvider::new();
        let kp_repo = KeyPackageRepo::new(&pool);
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo).unwrap();
        let mut alice_group =
            crate::mls::Group::create_solo(&alice_id, None, None, MlsProvider::new()).unwrap();
        let (welcome, _commit) = alice_group.add_member(&bob_kp, None, None).unwrap();
        let group_id_bytes = alice_group.id().0.clone();
        let mut bob_group =
            crate::mls::Group::join_from_welcome(&bob_id, &welcome, None, None, bob_provider)
                .unwrap();
        alice_group.save(&MlsGroupRepo::new(&pool)).unwrap();

        let contacts = ContactRepo::new(&pool);
        contacts
            .upsert(&Contact {
                identity: bob_pk,
                display_name: Some("bob".into()),
                added_at: 0,
                card: None,
                muted: false,
            })
            .unwrap();
        contacts.set_group_id(&bob_pk, &group_id_bytes).unwrap();

        // Bob encrypts with a deliberately OLD ts: 24h before now, far outside
        // the ±1h window the direct path enforces.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let old_ts = now_ms - 24 * 3600 * 1000;
        let env = crate::envelope::Envelope {
            v: 1,
            id: MessageId::generate(),
            ts: old_ts,
            reply_to: None,
            kind: Kind::Text {
                body: "stale-but-valid".into(),
            },
        };
        let expected_id = env.id;
        let ciphertext = bob_group.encrypt(&env).unwrap();

        let inbound = DaemonInbound::new(pool.clone(), events_tx.clone());

        // First dispatch: old ts is NOT rejected — returns the id + persists.
        let first = inbound.dispatch_mailbox(&ciphertext);
        assert_eq!(
            first,
            Some(expected_id),
            "old-ts mailbox deposit must dispatch (ts-window exempt)"
        );
        match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await {
            Ok(Ok(Event::MessageReceived { contact, record })) => {
                assert_eq!(contact, bob_pk);
                assert!(matches!(&record.kind, Kind::Text { body } if body == "stale-but-valid"));
            }
            other => panic!("expected MessageReceived, got {other:?}"),
        }

        // Second dispatch of the SAME ciphertext: dedup no-op (the MLS ratchet
        // generation is already consumed, so trial-decrypt no longer matches).
        let second = inbound.dispatch_mailbox(&ciphertext);
        assert_eq!(second, None, "replayed deposit must be a dedup no-op");
        match tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await {
            Err(_timeout) => {} // expected: no second event
            Ok(other) => panic!("replay must not emit a second event: {other:?}"),
        }

        // Persisted exactly once across both dispatches.
        let rows = MessageRepo::new(&pool).recent(&group_id_bytes, 10).unwrap();
        assert_eq!(rows.len(), 1, "replay must not add a second row");
    }

    #[tokio::test]
    async fn dispatch_mailbox_trial_decrypts_attributes_sender_and_persists() {
        use crate::contact::Contact;
        use crate::mls::key_package::KeyPackage;
        use crate::storage::key_packages::KeyPackageRepo;
        use crate::storage::{ContactRepo, MessageRepo, MlsGroupRepo};

        let pool = Arc::new(Pool::in_memory());
        let (events_tx, mut rx) = broadcast::channel::<Event>(16);

        let alice_seed = crate::identity::Seed::generate().unwrap();
        let alice_id = crate::identity::IdentityKey::from_seed(&alice_seed).unwrap();
        let bob_seed = crate::identity::Seed::generate().unwrap();
        let bob_id = crate::identity::IdentityKey::from_seed(&bob_seed).unwrap();
        let bob_pk = bob_id.public();

        // Build the 2-member group (alice adds bob).
        let bob_provider = MlsProvider::new();
        let kp_repo = KeyPackageRepo::new(&pool);
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo).unwrap();
        let mut alice_group =
            crate::mls::Group::create_solo(&alice_id, None, None, MlsProvider::new()).unwrap();
        let (welcome, _commit) = alice_group.add_member(&bob_kp, None, None).unwrap();
        let group_id_bytes = alice_group.id().0.clone();
        let mut bob_group =
            crate::mls::Group::join_from_welcome(&bob_id, &welcome, None, None, bob_provider)
                .unwrap();

        // Persist alice's group + link bob as the contact for this group so
        // contact_for_group can attribute the sender.
        alice_group.save(&MlsGroupRepo::new(&pool)).unwrap();
        let contacts = ContactRepo::new(&pool);
        contacts
            .upsert(&Contact {
                identity: bob_pk,
                display_name: Some("bob".into()),
                added_at: 0,
                card: None,
                muted: false,
            })
            .unwrap();
        contacts.set_group_id(&bob_pk, &group_id_bytes).unwrap();

        // Bob encrypts a message (ts within ±1h of now).
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let env = crate::envelope::Envelope {
            v: 1,
            id: MessageId::generate(),
            ts: now_ms,
            reply_to: None,
            kind: Kind::Text {
                body: "from-mailbox".into(),
            },
        };
        let expected_id = env.id;
        let ciphertext = bob_group.encrypt(&env).unwrap();

        let inbound = DaemonInbound::new(pool.clone(), events_tx.clone());
        let returned = inbound.dispatch_mailbox(&ciphertext);

        assert_eq!(
            returned,
            Some(expected_id),
            "must return the decrypted message id"
        );

        // Event emitted, attributed to bob.
        match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await {
            Ok(Ok(Event::MessageReceived { contact, record })) => {
                assert_eq!(contact, bob_pk, "sender must be attributed to bob");
                assert!(
                    matches!(&record.kind, Kind::Text { body } if body == "from-mailbox"),
                    "unexpected kind: {:?}",
                    record.kind
                );
            }
            other => panic!("expected MessageReceived, got {other:?}"),
        }

        // Persisted exactly once.
        let rows = MessageRepo::new(&pool).recent(&group_id_bytes, 10).unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn dispatch_welcome_rejects_unknown_kp_hash() {
        let pool = Arc::new(Pool::in_memory());
        let (events_tx, _rx) = broadcast::channel::<Event>(16);
        let alice_arc = Arc::new(
            crate::identity::IdentityKey::from_seed(&crate::identity::Seed::generate().unwrap())
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
        let alice =
            crate::identity::IdentityKey::from_seed(&crate::identity::Seed::generate().unwrap())
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

        let bob =
            crate::identity::IdentityKey::from_seed(&crate::identity::Seed::generate().unwrap())
                .unwrap();
        let mut bob_group = crate::mls::Group::create_solo(
            &bob,
            Some((&alice_kp_ref, &psk)),
            None,
            MlsProvider::new(),
        )
        .unwrap();
        let alice_kp_for_add = KeyPackage::from_bytes(&alice_kp_bytes).unwrap();
        let (welcome_bytes, _) = bob_group
            .add_member(&alice_kp_for_add, Some((&alice_kp_ref, &psk)), None)
            .unwrap();

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

        let result = inbound.dispatch_for_group(peer, &unknown_gid, garbage_ct, true);
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
            crate::mls::Group::create_solo(&alice_id, None, None, MlsProvider::new()).unwrap();
        let (welcome, _commit) = alice_group.add_member(&bob_kp, None, None).unwrap();
        let group_id_bytes = alice_group.id().0.clone();
        let mut bob_group =
            crate::mls::Group::join_from_welcome(&bob_id, &welcome, None, None, bob_provider)
                .unwrap();

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
        let result = inbound.dispatch_for_group(peer, &group_id_bytes, &ciphertext, true);
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

    #[test]
    fn dispatch_attachment_chunk_matches_stores_and_dedups() {
        use crate::attachment::{chunker::chunk_plaintext, store::ChunkStore};
        use crate::storage::attachments::AttachmentRepo;

        let dir = tempfile::tempdir().unwrap();
        let pool = std::sync::Arc::new(crate::storage::Pool::in_memory());
        // Pool::in_memory runs all migrations including 0016 (attachments + peer col).
        let (events_tx, _rx) = tokio::sync::broadcast::channel(8);
        let inbound = DaemonInbound::new(pool.clone(), events_tx);
        let store = std::sync::Arc::new(ChunkStore::new(dir.path()));
        inbound.set_chunk_store(store.clone());
        inbound.set_download_dir(dir.path().join("downloads"));

        // Build a 2-chunk attachment.
        let payload = vec![7u8; crate::attachment::CHUNK_SIZE + 100];
        let (manifest, cts) =
            chunk_plaintext(&payload, "f.bin", "application/octet-stream").unwrap();
        // Persist the manifest as a pending 'in' attachment.
        AttachmentRepo::new(&pool)
            .insert(
                &manifest.attachment_id,
                "in",
                &manifest.to_cbor().unwrap(),
                cts.len() as i64,
                0,
            )
            .unwrap();

        // First chunk: recognized + stored.
        assert!(inbound.dispatch_attachment_chunk(&cts[0]));
        // Same chunk again: still recognized (dedup) → true (server-delete) but no double-count.
        assert!(inbound.dispatch_attachment_chunk(&cts[0]));
        assert_eq!(
            AttachmentRepo::new(&pool)
                .received_indices(&manifest.attachment_id)
                .unwrap(),
            vec![0]
        );
        // A non-chunk blob: not recognized.
        assert!(!inbound.dispatch_attachment_chunk(b"not a chunk"));
    }
}
