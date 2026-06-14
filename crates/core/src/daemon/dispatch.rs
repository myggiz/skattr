// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic, clippy::expect_used))]

//! Command dispatch: one function per `Command` variant, consuming a
//! `DaemonHandle` + the command and returning a typed result / error.
//!
//! Per-variant handlers are private to this module. Task 14+ fill them
//! in; today all variants except `Shutdown` / `RotateOnion` return
//! `IpcError::UnknownCommand`.

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::daemon::commands::{Command, CommandResult};
use crate::daemon::handle::DaemonHandle;
use crate::daemon::ipc::wire::IpcError;
use crate::error::CoreError;
use crate::storage::StorageErrorKind;

/// Execute one command against `handle`. Every per-variant handler
/// lives in this module (small private fns); we keep them colocated so
/// a reader can see the whole command surface in one file.
pub async fn execute_command<S>(
    handle: Arc<DaemonHandle<S>>,
    cmd: Command,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    match cmd {
        Command::Shutdown => Ok(CommandResult::Ok),
        Command::RotateOnion => handle_rotate_onion(handle).await,
        Command::ListContacts => list_contacts(&handle, false).await,
        Command::CreateInvite { nickname, ttl_secs } => {
            create_invite(&handle, nickname, ttl_secs).await
        }
        Command::AddContact { invite_url } => add_contact(&handle, invite_url).await,
        Command::SendMessage { contact, kind } => send_message(&handle, contact, kind).await,
        Command::RecentMessages {
            contact,
            limit,
            before_id,
            paged,
        } => recent_messages(&handle, contact, limit, before_id, paged).await,
        Command::CreateGroup { .. } => Err(IpcError::UnknownCommand),
        Command::RenameContact { contact, nickname } => {
            rename_contact(&handle, contact, nickname).await
        }
        Command::RemoveContact { contact } => remove_contact(&handle, contact).await,
        Command::ListContactsWithFilter { include_hidden } => {
            list_contacts(&handle, include_hidden).await
        }
        Command::SearchMessages {
            query,
            contact,
            limit,
            offset,
            newest_first,
        } => search_messages(&handle, query, contact, limit, offset, newest_first).await,
        Command::MarkRead {
            contact,
            up_to_message_id,
        } => mark_read(&handle, contact, up_to_message_id).await,
        Command::PruneHistory {
            contact,
            before_ts_recv,
            keep_last,
        } => prune_history(&handle, contact, before_ts_recv, keep_last).await,
        Command::ExportHistory {
            contact,
            after_id,
            limit,
        } => export_history(&handle, contact, after_id, limit).await,
        Command::AddMailbox { onion } => handle_add_mailbox(handle, onion).await,
        Command::RemoveMailbox { id } => handle_remove_mailbox(handle, id).await,
        Command::ListMailboxes => handle_list_mailboxes(handle).await,
        Command::DaemonInfo => handle_daemon_info(handle).await,
        Command::GetConfig => get_config(&handle).await,
        Command::SetConfig { patch } => set_config(&handle, patch).await,
        Command::ChangePassphrase { old, new } => change_passphrase(&handle, old, new).await,
        Command::SetContactMuted { contact, muted } => {
            set_contact_muted(&handle, contact, muted).await
        }
        Command::TailLogs { since_seq, limit } => tail_logs(&handle, since_seq, limit).await,
        Command::GetPassphraseAuditLatest => get_passphrase_audit_latest(&handle).await,
        Command::WipeAllData => wipe_all_data(handle).await,
    }
}

async fn list_contacts<S>(
    handle: &Arc<DaemonHandle<S>>,
    include_hidden: bool,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::commands::ContactSummary;
    use crate::storage::{ContactRepo, MessageRepo, MlsGroupRepo, ReadStateRepo};

    let repo = ContactRepo::new(&handle.pool);
    let msg_repo = MessageRepo::new(&handle.pool);
    let group_repo = MlsGroupRepo::new(&handle.pool);
    let read_repo = ReadStateRepo::new(&handle.pool);
    let contacts = if include_hidden {
        repo.list_all().map_err(map_err)?
    } else {
        repo.list().map_err(map_err)?
    };

    let mut summaries: Vec<ContactSummary> = Vec::with_capacity(contacts.len());
    for c in contacts {
        let (onion, card_version) = c
            .card
            .as_ref()
            .map(|card| (card.body.onion.clone(), card.body.version))
            .unwrap_or_else(|| (String::new(), 0));

        let group_id = repo.get_group_id(&c.identity).map_err(map_err)?;
        let (unread_count, last_message_preview, last_ts_recv) = match group_id.as_deref() {
            Some(gid) => {
                let unread = msg_repo.unread_count(gid).map_err(map_err)?;
                let latest = msg_repo.latest_for_group(gid).map_err(map_err)?;
                let preview = latest.as_ref().and_then(|row| {
                    let env: crate::envelope::Envelope =
                        crate::envelope::Envelope::decode(row.body_blob.as_ref()?.as_slice())
                            .ok()?;
                    match env.kind {
                        crate::envelope::Kind::Text { body } => Some(truncate_preview(&body, 80)),
                        _ => None,
                    }
                });
                let ts = latest.map(|row| u64::try_from(row.ts_daemon_recv).unwrap_or(0));
                (unread, preview, ts)
            }
            None => (0, None, None),
        };

        let group_state: Option<crate::daemon::commands::MlsGroupStateLabel> =
            match group_id.as_deref() {
                Some(gid) => {
                    use crate::daemon::commands::MlsGroupStateLabel;
                    use crate::mls::group::{Group, GroupId};
                    match Group::load(&GroupId(gid.to_vec()), &group_repo) {
                        Ok(Some(_)) => Some(MlsGroupStateLabel::Active),
                        Ok(None) => Some(MlsGroupStateLabel::PendingJoin),
                        Err(_) => Some(MlsGroupStateLabel::Corrupt),
                    }
                }
                None => None,
            };

        let last_read_row_id: Option<i64> = match group_id.as_deref() {
            Some(gid) => read_repo.get(gid).map_err(map_err)?,
            None => None,
        };

        let peer_mailboxes: Vec<String> = c
            .card
            .as_ref()
            .map(|card| card.body.mailboxes.clone())
            .unwrap_or_default();

        summaries.push(ContactSummary {
            pubkey: c.identity,
            nickname: c.display_name,
            onion,
            card_version,
            added_at: u64::try_from(c.added_at).unwrap_or(0),
            unread_count,
            last_message_preview,
            last_ts_recv,
            group_state,
            last_read_row_id,
            muted: c.muted,
            peer_mailboxes,
        });
    }

    // Sort descending: contacts with recent messages first; ties broken by
    // `added_at` descending (newest-added first); contacts with no messages
    // come last.
    summaries.sort_by(|a, b| match (a.last_ts_recv, b.last_ts_recv) {
        (Some(av), Some(bv)) => bv.cmp(&av).then(b.added_at.cmp(&a.added_at)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => b.added_at.cmp(&a.added_at),
    });

    Ok(CommandResult::Contacts(summaries))
}

/// Truncate `s` to at most `max_chars` Unicode code points. Cheap
/// 2.C-grade preview; grapheme-aware truncation lands in 2.D.
fn truncate_preview(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

async fn create_invite<S>(
    handle: &Arc<DaemonHandle<S>>,
    _nickname: Option<String>,
    ttl_secs: Option<u64>,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::error_kind::DaemonErrorKind;
    use crate::daemon::hex::Hex32;
    use crate::invite::InviteLink;
    use crate::mls::key_package::{key_package_ref, KeyPackage};
    use crate::mls::provider::MlsProvider;
    use crate::storage::{KeyPackageRepo, OutstandingInviteRepo};
    use rand_core::{OsRng, RngCore as _};
    use zeroize::Zeroizing;

    let onion = handle
        .onion()
        .ok_or(IpcError::Daemon(DaemonErrorKind::TorNotReady))?;

    let ttl = ttl_secs.unwrap_or(24 * 3600);
    let now = crate::daemon::clock::now_unix_seconds();

    // Generate a fresh MLS KeyPackage. `generate` internally stores the
    // KP bytes in `KeyPackageRepo` with direction="ours", consumed=false.
    let provider = MlsProvider::new();
    let kp_repo = KeyPackageRepo::new(&handle.pool);
    let kp = KeyPackage::generate(&handle.identity, &provider, &kp_repo).map_err(map_err)?;

    // Canonical MLS KeyPackageRef — the value the Welcome message routes by.
    // This is the primary key for outstanding_invites so the inviter can look
    // up the PSK when the Welcome arrives (see parse_welcome_kp_hash).
    let kp_ref = key_package_ref(&kp).map_err(map_err)?;
    let kp_bytes = kp.to_bytes().map_err(map_err)?;

    // 32-byte one-time PSK.
    let mut psk_raw = [0u8; 32];
    OsRng.fill_bytes(&mut psk_raw);
    let psk = Zeroizing::new(psk_raw);

    let mailboxes: Vec<String> = crate::storage::MailboxRepo::new(&handle.pool)
        .list_mine()
        .map_err(map_err)?
        .into_iter()
        .filter(|r| r.status == crate::storage::MailboxStatus::Reachable)
        .map(|r| r.onion)
        .collect();
    let card = crate::contact::self_card::build_next_self_card(
        &handle.pool,
        &handle.identity,
        onion,
        mailboxes,
        crate::contact::self_card::DEFAULT_TTL_SECS,
        now,
    )
    .map_err(map_err)?;

    let link = InviteLink::generate(&handle.identity, card, kp_bytes.clone(), psk_raw, ttl, now)
        .map_err(map_err)?;
    let url = link.to_url().map_err(map_err)?;
    let expires_at = u64::try_from(now + ttl as i64).unwrap_or(0);

    // Persist the PSK + provider snapshot so the inviter can reconstruct both
    // at Welcome-receive time. The provider snapshot holds the init private
    // key that OpenMLS needs to process the Welcome via join_from_welcome.
    let provider_snap = provider.snapshot().map_err(map_err)?;
    let oi = OutstandingInviteRepo::new(&handle.pool);
    oi.put_with_provider(
        &kp_ref,
        &psk,
        &kp_bytes,
        &provider_snap,
        now + ttl as i64,
        now,
    )
    .map_err(map_err)?;

    Ok(CommandResult::InviteCreated {
        url,
        key_package_id: Hex32::from(kp_ref),
        expires_at,
    })
}

async fn add_contact<S>(
    handle: &Arc<DaemonHandle<S>>,
    invite_url: String,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::contact::Contact;
    use crate::daemon::commands::ContactSummary;
    use crate::daemon::error_kind::DaemonErrorKind;
    use crate::daemon::events::Event;
    use crate::invite::InviteLink;
    use crate::mls::{group::Group, key_package::KeyPackage, provider::MlsProvider};
    use crate::storage::{ContactRepo, KeyPackageRepo, MlsGroupRepo};

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .map_err(|e| map_err(CoreError::Config(format!("clock: {e}"))))?;

    let link = InviteLink::from_url(&invite_url, now).map_err(map_err)?;

    let kp_repo = KeyPackageRepo::new(&handle.pool);

    // Record the inbound KP so mark_consumed can find it (idempotent if
    // same invite is re-submitted).
    link.record_received(&kp_repo).map_err(map_err)?;

    // Fast-path double-use guard. The load-bearing single-use guarantee is
    // the re-check INSIDE the consuming transaction below (T2-1); this early
    // check just avoids the dial + MLS work for an obviously-spent invite.
    if link.is_consumed(&kp_repo).map_err(map_err)? {
        return Err(IpcError::Daemon(DaemonErrorKind::InviteConsumed));
    }

    let inviter = link.body.card.body.identity;
    let invitee_kp = KeyPackage::from_bytes(&link.body.key_package).map_err(map_err)?;
    let kp_ref = crate::mls::key_package::key_package_ref(&invitee_kp).map_err(map_err)?;

    // Verify the inviter's card BEFORE the dial. This is a pure check (no write):
    // the outer link signature already authenticates the card bytes, but
    // verifying the card's own signature + expiry keeps a self-inconsistent or
    // stale card out of the store. No DB write happens here, so a failure leaves
    // the store untouched.
    link.body.card.verify(now).map_err(map_err)?;

    // Dial the inviter FIRST (ADR 0009, T1-1) — by the onion embedded in the
    // invite card, NOT via ContactRepo::latest_card. Dialing the in-invite onion
    // means the inviter's card no longer has to be persisted before the dial, so
    // ALL writes (contact, card, group, group_id, mark_consumed) can move into
    // the single transaction below. Full add_contact atomicity (T2-1): a dial
    // failure (inviter offline / Tor flaky — common) leaves ZERO writes, so a
    // retry is clean and idempotent (no stranded contact, no stale-version retry
    // wall). The genesis Commit binds to this connection's h_transport (the
    // second external PSK below), and the per-peer actor reuses this one
    // connection for the Welcome (no second dial). The dial is MANDATORY —
    // first contact requires the connection anyway (to deliver the Welcome).
    let inviter_onion = link.body.card.body.onion.clone();
    let h_transport = handle
        .hub
        .connect_and_ingest_at(inviter, &inviter_onion)
        .await
        .map_err(map_err)?;

    let contact_repo = ContactRepo::new(&handle.pool);
    let contact = Contact {
        identity: inviter,
        display_name: None,
        added_at: now,
        card: None,
        muted: false,
    };

    // Build our solo MLS group, then add the inviter as the second member.
    // Both external PSKs (invite + h_transport) are keyed per-invite by the
    // invite KeyPackageRef (ADR 0009, T2-8) and proposed in the genesis Commit;
    // the inviter registers the same two values before join_from_welcome, so the
    // genesis group is bound to the authenticated transport transcript (T1-1).
    let provider = MlsProvider::new();
    let mut group = Group::create_solo(
        &handle.identity,
        Some((&kp_ref, &link.psk.0)),
        Some((&kp_ref, &*h_transport)),
        provider,
    )
    .map_err(map_err)?;

    let (welcome, _commit) = group
        .add_member(
            &invitee_kp,
            Some((&kp_ref, &link.psk.0)),
            Some((&kp_ref, &*h_transport)),
        )
        .map_err(map_err)?;
    let group_id = group.id().0.clone();

    // Full atomicity (T2-1): re-check consumed + upsert the inviter contact +
    // persist the verified card + save the genesis group + link the group_id +
    // mark the invite consumed, ALL in one transaction. Because the dial above
    // used the in-invite onion (not latest_card), the card no longer has to be
    // written before the dial — so every write lives here. The re-check inside
    // the tx is the load-bearing single-use guarantee: two concurrent (or
    // re-submitted) AddContacts cannot both pass it, so an invite can never
    // create two groups. On a re-submit the second caller sees consumed=true
    // here and returns InviteConsumed without writing anything. A dial failure
    // (above) aborts before this txn, leaving ZERO writes → clean retry.
    let group_repo = MlsGroupRepo::new(&handle.pool);
    handle
        .pool
        .transaction(|tx| {
            if link.is_consumed_in_tx(tx, &kp_repo)? {
                return Err(CoreError::Invite(crate::invite::InviteErrorKind::Consumed));
            }
            contact_repo.upsert_in_tx(tx, &contact)?;
            contact_repo.put_card_in_tx(tx, &link.body.card)?;
            group.save_in_tx(&group_repo, tx)?;
            contact_repo.set_group_id_in_tx(tx, &inviter, &group_id)?;
            link.mark_consumed_in_tx(tx, &kp_repo)?;
            Ok(())
        })
        .map_err(map_err)?;

    let _ = handle.events_tx.send(Event::ContactUpdated(inviter));

    // Submit Welcome to the inviter via the hub. We do not await the
    // ACK here — UI responsiveness comes first, and a failed delivery
    // surfaces via Event::DeliveryStatusChanged through the hub's
    // existing failure path.
    handle
        .hub
        .send_welcome(inviter, welcome)
        .await
        .map_err(map_err)?;

    // Send our own card to the new contact so they learn our onion for the
    // reverse direction. Best-effort: rides the same peer-actor connection
    // after the Welcome; if it fails, the inviter learns our onion on our next
    // message instead.
    match build_self_card(handle) {
        Ok(self_card) => {
            send_card_to_contact(handle, &self_card, inviter).await;
        }
        // `IpcError` implements Debug (not Display); `?e` is correct here. It
        // carries only DaemonErrorKind (counts / InvalidArgument message / unit
        // variants) — no onion or pubkey.
        Err(e) => {
            tracing::warn!(?e, "add_contact: could not build self-card to send")
        }
    }

    Ok(CommandResult::ContactAdded(ContactSummary {
        pubkey: inviter,
        nickname: None,
        onion: link.body.card.body.onion.clone(),
        card_version: 0,
        added_at: u64::try_from(now).unwrap_or(0),
        unread_count: 0,
        last_message_preview: None,
        last_ts_recv: None,
        group_state: None,
        last_read_row_id: None,
        muted: false,
        peer_mailboxes: Vec::new(),
    }))
}

async fn send_message<S>(
    handle: &Arc<DaemonHandle<S>>,
    contact: crate::identity::PublicKey,
    kind: crate::envelope::Kind,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::commands::SendStatus;
    use crate::daemon::error_kind::DaemonErrorKind;
    use crate::daemon::hex::Hex16;
    use crate::envelope::{Envelope, MessageId};
    use crate::mls::group::{Group, GroupId};
    use crate::storage::outbox::OutboxRepo;
    use crate::storage::{ContactRepo, MessageRepo, MlsGroupRepo};

    // Reject internal, non-user-sendable kinds before any MLS work.
    // ContactCardUpdate is generated only by the daemon's own card-publish
    // path; if it reaches MessageRepo::insert_in_tx it hits an unreachable!()
    // inside the storage transaction (process abort in release).
    if matches!(kind, crate::envelope::Kind::ContactCardUpdate { .. }) {
        return Err(IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            message: "ContactCardUpdate is not a user-sendable message kind".into(),
        }));
    }

    // 1. Resolve group_id from contact.
    let contact_repo = ContactRepo::new(&handle.pool);
    let group_id_bytes = match contact_repo.get_group_id(&contact).map_err(map_err)? {
        Some(bytes) if !bytes.is_empty() => bytes,
        _ => return Err(IpcError::Daemon(DaemonErrorKind::ContactNotFound)),
    };

    // 2. Load MLS group (provider is embedded in the blob).
    //
    // Per-group ratchet serialization (T1-3): acquire the group's lock BEFORE
    // load and hold it across encrypt + the save transaction so two concurrent
    // sends (or a send racing an inbound receive) can never load the same
    // on-disk snapshot and encrypt at the same ratchet generation. The guard is
    // a blocking `std::sync::Mutex` held only across this fully-synchronous
    // critical section; it is `drop`ped explicitly before the first `.await`
    // below so the future stays `Send`. The registry is shared with the inbound
    // dispatcher, so send + receive on this group serialize together.
    let group_id_arr: [u8; 32] = group_id_bytes
        .as_slice()
        .try_into()
        .map_err(|_| IpcError::Daemon(DaemonErrorKind::GroupCorrupt))?;
    let group_lock = handle.group_lock(&group_id_arr);
    let _guard = group_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let group_repo = MlsGroupRepo::new(&handle.pool);
    let group_id = GroupId(group_id_bytes.clone());
    let mut group = Group::load(&group_id, &group_repo)
        .map_err(map_err)?
        .ok_or(IpcError::Daemon(DaemonErrorKind::GroupCorrupt))?;

    // 3. Build envelope.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .map_err(|e| map_err(CoreError::Config(format!("clock: {e}"))))?;

    let message_id = MessageId::generate();

    let envelope = Envelope {
        v: 1,
        id: message_id,
        ts: now_ms,
        reply_to: None,
        kind,
    };

    // 4. MLS-encrypt (ratchet advances).
    let ciphertext = group.encrypt(&envelope).map_err(map_err)?;

    // 5. Atomic: persist advanced ratchet + message row + outbox entry in
    // one transaction. If any sub-operation fails the whole tx rolls back,
    // including the MLS snapshot, so the caller sees a clean error and can
    // retry without the ratchet having advanced on disk.
    let mls_generation = group.epoch();
    let ts_daemon_recv = now_ms / 1000;
    let msg_repo = MessageRepo::new(&handle.pool);
    let outbox_repo = OutboxRepo::new(&handle.pool);

    let insert_result: crate::error::Result<i64> = handle.pool.transaction(|tx| {
        group.save_in_tx(&group_repo, tx)?;
        let row_id = msg_repo.insert_in_tx(
            tx,
            crate::storage::messages::InsertParams {
                group_id: &group_id_bytes,
                sender: &handle.identity.public().0,
                envelope: &envelope,
                mls_generation,
                ts_daemon_recv,
            },
        )?;
        let _ = outbox_repo.insert_in_tx(tx, &contact.0, &message_id.0, &ciphertext, 0)?;
        Ok(row_id)
    });

    let row_id = match insert_result {
        Ok(id) => id,
        Err(CoreError::Storage(StorageErrorKind::DuplicateMessage)) => {
            // Idempotent retry: this envelope_id was already persisted by a
            // prior attempt. Treat as Delivered — the outbox row from the
            // earlier attempt still owns delivery.
            return Ok(CommandResult::MessageSent {
                message_id: Hex16::from(message_id.0),
                status: SendStatus::Delivered,
                record: None,
            });
        }
        Err(e) => return Err(map_err(e)),
    };

    // Critical section done (ratchet advanced + persisted). Release the
    // per-group lock BEFORE any `.await` so the guard never crosses an await
    // point (keeps the future `Send`) and the hub round-trip below doesn't
    // serialize other sends on this group.
    drop(_guard);

    // 6. Kick the delivery hub, wait up to 2 s for an ACK.
    let ack_rx = handle
        .hub
        .send(contact, message_id, ciphertext)
        .await
        .map_err(map_err)?;

    let status = match tokio::time::timeout(std::time::Duration::from_secs(2), ack_rx).await {
        Ok(Ok(Ok(()))) => SendStatus::Delivered,
        _ => SendStatus::Queued,
    };

    let record = crate::daemon::commands::MessageRecord::project(
        row_id,
        &envelope,
        contact,
        mls_generation,
        ts_daemon_recv,
        crate::daemon::commands::Direction::Outgoing,
    );

    Ok(CommandResult::MessageSent {
        message_id: Hex16::from(message_id.0),
        status,
        record: Some(record),
    })
}

async fn recent_messages<S>(
    handle: &Arc<DaemonHandle<S>>,
    contact: Option<crate::identity::PublicKey>,
    limit: u32,
    before_id: Option<i64>,
    paged: bool,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::commands::{Direction, MessageRecord};
    use crate::daemon::error_kind::DaemonErrorKind;
    use crate::envelope::Envelope;
    use crate::identity::PublicKey;
    use crate::storage::{ContactRepo, MessageRepo};

    // Phase 1.F requires a contact filter. Global recent lands in Phase 1.G.
    let peer = contact.ok_or(IpcError::Daemon(DaemonErrorKind::ContactNotFound))?;

    let contact_repo = ContactRepo::new(&handle.pool);
    let group_id = match contact_repo.get_group_id(&peer).map_err(map_err)? {
        Some(bytes) if !bytes.is_empty() => bytes,
        _ => return Err(IpcError::Daemon(DaemonErrorKind::ContactNotFound)),
    };

    let msg_repo = MessageRepo::new(&handle.pool);
    let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);
    let rows = match before_id {
        Some(b) => msg_repo
            .recent_before(&group_id, b, limit_usize)
            .map_err(map_err)?,
        None => msg_repo.recent(&group_id, limit_usize).map_err(map_err)?,
    };

    // Local identity pubkey — determines message direction.
    let my_pubkey: PublicKey = handle.identity.public();

    let records: Vec<MessageRecord> = rows
        .into_iter()
        .filter_map(|row| {
            // Decode body_blob -> Envelope (skip rows with no blob or bad CBOR).
            let blob = row.body_blob.as_deref().unwrap_or(&[]);
            let env: Envelope = Envelope::decode(blob).ok()?;

            let mut sender_arr = [0u8; 32];
            if row.sender.len() == 32 {
                sender_arr.copy_from_slice(&row.sender);
            }
            let sender_pk = PublicKey(sender_arr);

            let direction = if sender_pk == my_pubkey {
                Direction::Outgoing
            } else {
                Direction::Incoming
            };
            // `contact` field is always the peer — outgoing rows were sent
            // to `peer`; incoming rows were sent by `peer` (the sender).
            Some(MessageRecord::project(
                row.id,
                &env,
                peer,
                u64::try_from(row.mls_generation).unwrap_or(0),
                row.ts_daemon_recv,
                direction,
            ))
        })
        .collect();

    if paged {
        // A full page (records.len() == limit_usize) means there may be
        // more older rows; surface the oldest row's id as the cursor.
        // A short (or empty) page means end-of-stream.
        let next_before_id = if records.len() == limit_usize {
            records.last().map(|r| r.row_id)
        } else {
            None
        };
        Ok(CommandResult::MessagesPage {
            records,
            next_before_id,
        })
    } else {
        Ok(CommandResult::Messages(records))
    }
}

async fn search_messages<S>(
    handle: &Arc<DaemonHandle<S>>,
    query: String,
    contact: Option<crate::identity::PublicKey>,
    limit: u32,
    offset: u32,
    newest_first: bool,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::commands::{Direction, MessageRecord, SearchHitRecord};
    use crate::daemon::error_kind::DaemonErrorKind;
    use crate::envelope::Envelope;
    use crate::identity::PublicKey;
    use crate::storage::{ContactRepo, MessageRepo};

    // 1. Resolve optional contact -> group_id scope.
    let group_id_owned: Option<Vec<u8>> = match contact {
        Some(pk) => match ContactRepo::new(&handle.pool)
            .get_group_id(&pk)
            .map_err(map_err)?
        {
            Some(bytes) if !bytes.is_empty() => Some(bytes),
            _ => return Err(IpcError::Daemon(DaemonErrorKind::ContactNotFound)),
        },
        None => None,
    };

    // 2. Run FTS5 search.
    let msg_repo = MessageRepo::new(&handle.pool);
    let hits = msg_repo
        .search(
            &query,
            group_id_owned.as_deref(),
            usize::try_from(limit).unwrap_or(usize::MAX),
            usize::try_from(offset).unwrap_or(0),
            newest_first,
        )
        .map_err(map_err)?;

    // 3. Project each hit. Direction computed from sender vs local
    // pubkey, matching the idiom in `recent_messages`. For the `contact`
    // field: when the query was scoped to a peer use that peer for every
    // row; when unscoped fall back to the stored sender (correct for
    // incoming, best-effort for outgoing — Phase 1.G 2-member-group
    // scope per CLAUDE.md).
    let my_pubkey: PublicKey = handle.identity.public();
    let records: Vec<SearchHitRecord> = hits
        .into_iter()
        .filter_map(|h| {
            let env = Envelope::decode(h.message.body_blob.as_deref().unwrap_or(&[])).ok()?;
            let mut sender_arr = [0u8; 32];
            if h.message.sender.len() == 32 {
                sender_arr.copy_from_slice(&h.message.sender);
            }
            let sender_pk = PublicKey(sender_arr);
            let direction = if sender_pk == my_pubkey {
                Direction::Outgoing
            } else {
                Direction::Incoming
            };
            let contact_for_record = match contact {
                Some(pk) => pk,
                None => {
                    // Unscoped search: resolve peer via the hit's group_id
                    // so outgoing rows (where sender == local identity)
                    // still report the correct peer. 2-member-group scope.
                    let gid_arr: std::result::Result<[u8; 32], _> =
                        h.message.group_id[..].try_into();
                    match gid_arr {
                        Ok(arr) => ContactRepo::new(&handle.pool)
                            .contact_for_group(&arr)
                            .ok()
                            .flatten()
                            .unwrap_or(sender_pk),
                        Err(_) => sender_pk, // group_id length anomaly — fallback
                    }
                }
            };

            Some(SearchHitRecord {
                record: MessageRecord::project(
                    h.message.id,
                    &env,
                    contact_for_record,
                    u64::try_from(h.message.mls_generation).unwrap_or(0),
                    h.message.ts_daemon_recv,
                    direction,
                ),
                bm25: h.bm25,
                snippet: h.snippet,
            })
        })
        .collect();

    Ok(CommandResult::SearchResults(records))
}

async fn mark_read<S>(
    handle: &Arc<DaemonHandle<S>>,
    contact: crate::identity::PublicKey,
    up_to_message_id: i64,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::error_kind::DaemonErrorKind;
    use crate::storage::{ContactRepo, MessageRepo};

    let group_id_bytes = match ContactRepo::new(&handle.pool)
        .get_group_id(&contact)
        .map_err(map_err)?
    {
        Some(bytes) if !bytes.is_empty() => bytes,
        _ => return Err(IpcError::Daemon(DaemonErrorKind::ContactNotFound)),
    };

    MessageRepo::new(&handle.pool)
        .mark_read(&group_id_bytes, up_to_message_id)
        .map_err(map_err)?;

    Ok(CommandResult::MarkedRead {
        up_to: up_to_message_id,
    })
}

async fn prune_history<S>(
    handle: &Arc<DaemonHandle<S>>,
    contact: Option<crate::identity::PublicKey>,
    before_ts_recv: Option<i64>,
    keep_last: Option<u64>,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::error_kind::DaemonErrorKind;
    use crate::storage::{ContactRepo, MessageRepo};

    // Validate: exactly one of before_ts_recv or keep_last must be Some.
    match (before_ts_recv, keep_last) {
        (Some(_), Some(_)) | (None, None) => {
            return Err(IpcError::Daemon(
                crate::daemon::error_kind::DaemonErrorKind::InvalidArgument {
                    message: "exactly one of before_ts_recv or keep_last must be Some".into(),
                },
            ));
        }
        _ => {}
    }

    // Resolve optional contact to group_id.
    let group_id_owned: Option<Vec<u8>> = match contact {
        Some(pk) => match ContactRepo::new(&handle.pool)
            .get_group_id(&pk)
            .map_err(map_err)?
        {
            Some(bytes) if !bytes.is_empty() => Some(bytes),
            _ => return Err(IpcError::Daemon(DaemonErrorKind::ContactNotFound)),
        },
        None => None,
    };

    let msg_repo = MessageRepo::new(&handle.pool);
    let rows_deleted = match (before_ts_recv, keep_last) {
        (Some(ts), None) => msg_repo
            .prune_before(group_id_owned.as_deref(), ts)
            .map_err(map_err)?,
        (None, Some(k)) => {
            let gid = group_id_owned.as_deref().ok_or_else(|| {
                IpcError::Daemon(
                    crate::daemon::error_kind::DaemonErrorKind::InvalidArgument {
                        message: "keep_last requires a contact".into(),
                    },
                )
            })?;
            msg_repo.prune_keep_last(gid, k).map_err(map_err)?
        }
        _ => unreachable!("validated above"),
    };

    Ok(CommandResult::Pruned { rows_deleted })
}

async fn export_history<S>(
    handle: &Arc<DaemonHandle<S>>,
    contact: crate::identity::PublicKey,
    after_id: Option<i64>,
    limit: u32,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::commands::{Direction, MessageRecord};
    use crate::daemon::error_kind::DaemonErrorKind;
    use crate::envelope::Envelope;
    use crate::identity::PublicKey;
    use crate::storage::{ContactRepo, MessageRepo};

    // Cap limit at 1000 to keep the ExportPage response under the 1 MiB
    // IPC body cap set in Phase 1.F.
    const EXPORT_PAGE_MAX: u32 = 1000;
    let lim = limit.min(EXPORT_PAGE_MAX);
    let lim_usize = usize::try_from(lim).unwrap_or(0);

    // Resolve contact -> group_id (export is always scoped).
    let group_id_bytes = match ContactRepo::new(&handle.pool)
        .get_group_id(&contact)
        .map_err(map_err)?
    {
        Some(bytes) if !bytes.is_empty() => bytes,
        _ => return Err(IpcError::Daemon(DaemonErrorKind::ContactNotFound)),
    };

    let rows = MessageRepo::new(&handle.pool)
        .export_page(&group_id_bytes, after_id, lim_usize)
        .map_err(map_err)?;

    let my_pubkey: PublicKey = handle.identity.public();
    let mut records = Vec::with_capacity(rows.len());
    for row in &rows {
        // Skip rows whose body_blob fails to decode (matches recent_messages idiom).
        let Ok(env) = Envelope::decode(row.body_blob.as_deref().unwrap_or(&[])) else {
            continue;
        };
        let mut sender_arr = [0u8; 32];
        if row.sender.len() == 32 {
            sender_arr.copy_from_slice(&row.sender);
        }
        let sender_pk = PublicKey(sender_arr);
        let direction = if sender_pk == my_pubkey {
            Direction::Outgoing
        } else {
            Direction::Incoming
        };
        records.push(MessageRecord::project(
            row.id,
            &env,
            contact, // scoped to the requested peer
            u64::try_from(row.mls_generation).unwrap_or(0),
            row.ts_daemon_recv,
            direction,
        ));
    }

    // Cursor logic: a FULL page (rows.len() == lim_usize) means there may
    // be more; set next_after_id to the last row's id. A short page means
    // end-of-stream.
    let next_after_id = if rows.len() == lim_usize && lim_usize > 0 {
        rows.last().map(|r| r.id)
    } else {
        None
    };

    Ok(CommandResult::ExportPage {
        records,
        next_after_id,
    })
}

/// `RotateOnion` handler: bump the self-card version and republish the
/// current onion to all contacts.
///
/// # Deferred: real HS key rotation (Task 23.5)
///
/// This implementation is intentionally degenerate — it does **not** rotate
/// the underlying hidden-service key, so the onion address stays the same.
/// Contacts receive a `ContactCardUpdate` with an incremented version, but
/// the onion they route to is unchanged. This is a no-op semantically, but
/// it exercises the `publish_self_card_update` path end-to-end for
/// integration tests (Task 27 `rotate_onion_during_offline` will surface
/// what is still missing).
///
/// ## What full rotation (Task 23.5) needs
///
/// Full design (deferred — see `docs/superpowers/specs/
/// 2026-04-30-phase-2b-mailbox-client-design.md` decision 8):
///
/// 1. Generate a new HS key file alongside the current one.
/// 2. Launch a second `OnionService` bound to the new key; await its
///    published-status.
/// 3. Replace the daemon's onion-listener accept loop to feed both
///    old + new services into the same `DeliveryHub::ingest`.
/// 4. Schedule a `tokio::time::sleep(rotate_grace_secs)` task that shuts
///    down the old service when fired. Default: 24 h
///    (`[delivery] rotate_grace_secs`).
/// 5. After successful publish to N/N contacts (best-effort), call
///    `handle.set_onion(new_address)` so subsequent IPC sees the new onion.
///
/// The `TorRuntime::publish_onion` API today owns the lifecycle of a single
/// `OnionService` and aborts it on `TorRuntime::shutdown`. Supporting
/// concurrent services requires either: (a) a parallel
/// `TorRuntime::publish_secondary_onion(...) -> SecondaryHandle` method; or
/// (b) a `RotatingOnion` wrapper struct that owns two services and a swap
/// mechanism. Either path is non-trivial and is deferred to Task 23.5.
async fn handle_rotate_onion<S>(
    handle: Arc<DaemonHandle<S>>,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // TODO Task 23.5: real HS key rotation — see doc-comment above.
    publish_self_card_update(handle).await?;
    Ok(CommandResult::Ok)
}

/// `AddMailbox` handler: probe the mailbox, persist `Reachable`, kick
/// the `PollScheduler`, and republish the self-card so contacts learn
/// the new mailbox onion. See [`publish_self_card_update`] for the
/// best-effort fan-out semantics.
async fn handle_add_mailbox<S>(
    handle: Arc<DaemonHandle<S>>,
    onion: String,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::error_kind::DaemonErrorKind;
    use sha2::{Digest, Sha256};

    let factory = handle.mailbox_factory.as_ref().ok_or_else(|| {
        IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            message: "mailbox subsystem not initialized".into(),
        })
    })?;

    // 1. Open a fresh probe connection. Any connect-side error surfaces
    // as `unreachable` — this is the AddMailbox-validation failure mode.
    let mut client = factory.connect(&onion).await.map_err(|_| {
        IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            message: "unreachable".into(),
        })
    })?;

    // 2. Single-Challenge probe. Maps client errors onto stable
    // argument-validation reasons the CLI / UI can surface to the user.
    let identity_hash: [u8; 32] = {
        let pk = handle.identity.public().0;
        let mut h = Sha256::new();
        h.update(pk);
        h.finalize().into()
    };
    if let Err(e) = client.probe(identity_hash).await {
        let reason = match e {
            crate::error::CoreError::MailboxClient(
                crate::error::MailboxClientErrorKind::UnsupportedVersion,
            ) => "unsupported_version",
            crate::error::CoreError::MailboxClient(
                crate::error::MailboxClientErrorKind::RateLimited,
            ) => "rate_limited",
            crate::error::CoreError::MailboxClient(
                crate::error::MailboxClientErrorKind::Malformed,
            ) => "malformed_response",
            _ => "other",
        };
        return Err(IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            message: reason.into(),
        }));
    }

    // 3. Insert the row + flip to Reachable. `add_mine` is idempotent on
    // (onion, role='mine') so repeating AddMailbox just no-ops the row
    // and re-runs the rest of the post-probe steps.
    let now = crate::daemon::clock::now_unix_seconds();
    let mb_repo = crate::storage::MailboxRepo::new(&handle.pool);
    let id = mb_repo.add_mine(&onion, now).map_err(map_err)?;
    mb_repo
        .mark_status(id, crate::storage::MailboxStatus::Reachable)
        .map_err(map_err)?;

    // 4. Notify the scheduler so it spawns a poll actor for this row.
    if let Some(ctrl) = &handle.poller_ctrl {
        let _ = ctrl
            .send(crate::mailbox::poll::PollerCtrl::AddMailbox(id))
            .await;
    }

    // 5. Republish self-card so peers pick up the new mailbox onion.
    publish_self_card_update(handle.clone()).await?;

    Ok(CommandResult::Ok)
}

/// Build this daemon's current signed self-card (onion + reachable mailboxes).
///
/// Bumps the persisted self-card version via `build_next_self_card`, so even
/// if no send follows, the next publish picks up from the new version.
fn build_self_card<S>(
    handle: &Arc<DaemonHandle<S>>,
) -> std::result::Result<crate::contact::ContactCard, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::error_kind::DaemonErrorKind;
    let onion = handle
        .onion()
        .ok_or(IpcError::Daemon(DaemonErrorKind::TorNotReady))?;
    let mailboxes: Vec<String> = crate::storage::MailboxRepo::new(&handle.pool)
        .list_mine()
        .map_err(map_err)?
        .into_iter()
        .filter(|r| r.status == crate::storage::MailboxStatus::Reachable)
        .map(|r| r.onion)
        .collect();
    crate::contact::self_card::build_next_self_card(
        &handle.pool,
        &handle.identity,
        onion,
        mailboxes,
        crate::contact::self_card::DEFAULT_TTL_SECS,
        crate::daemon::clock::now_unix_seconds(),
    )
    .map_err(map_err)
}

/// Encrypt `card` as a `ContactCardUpdate` for `peer`'s group and hand it to
/// the hub. Best-effort: a missing-but-expected group is skipped silently; an
/// encrypt / save / Group::load failure is logged and skipped.
async fn send_card_to_contact<S>(
    handle: &Arc<DaemonHandle<S>>,
    card: &crate::contact::ContactCard,
    peer: crate::identity::PublicKey,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::envelope::{Envelope, Kind, MessageId};
    use crate::mls::group::{Group, GroupId};
    use crate::storage::{ContactRepo, MlsGroupRepo};

    // 2-member-group lookup: skip contacts not yet linked (a missing/empty
    // group_id for a not-yet-linked contact is a normal, expected skip).
    let group_id_bytes = match ContactRepo::new(&handle.pool).get_group_id(&peer) {
        Ok(Some(gid)) if !gid.is_empty() => gid,
        _ => return,
    };

    // Per-group ratchet serialization (T1-3): the card-send load→encrypt→save
    // advances the same ratchet as `send_message`, so it must take the same
    // per-group lock. Guard scoped to the sync critical section and dropped
    // before the `hub.send().await` at the end. A group_id that isn't 32 bytes
    // is an MLS-storage anomaly — skip (matching the load-miss handling below).
    let group_id_arr: [u8; 32] = match group_id_bytes.as_slice().try_into() {
        Ok(a) => a,
        Err(_) => return,
    };
    let group_lock = handle.group_lock(&group_id_arr);
    let prepared = {
        let _guard = group_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let group_repo = MlsGroupRepo::new(&handle.pool);
        // A linked group_id whose group fails to load is an MLS-storage signal —
        // surface it rather than skip silently ("MLS state is fragile").
        let mut group = match Group::load(&GroupId(group_id_bytes), &group_repo) {
            Ok(Some(g)) => g,
            Ok(None) => {
                tracing::warn!(
                    target: "skattr::daemon::dispatch",
                    "card-send: group_id present but Group::load missed; skipping"
                );
                return;
            }
            Err(e) => {
                tracing::warn!(
                    target: "skattr::daemon::dispatch",
                    err = %e,
                    "card-send: load group failed; skipping"
                );
                return;
            }
        };
        let now_ms = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
        )
        .unwrap_or(0);
        let msg_id = MessageId::generate();
        let env = Envelope {
            v: 1,
            id: msg_id,
            ts: now_ms,
            reply_to: None,
            kind: Kind::ContactCardUpdate {
                card: Box::new(card.clone()),
            },
        };
        let ct = match group.encrypt(&env) {
            Ok(ct) => ct,
            Err(e) => {
                tracing::warn!(
                    target: "skattr::daemon::dispatch",
                    err = %e,
                    "card-send: encrypt failed; skipping"
                );
                return;
            }
        };
        // Persist the advanced ratchet before handing off to the hub — if save
        // fails we MUST NOT send the ciphertext (the peer would accept it and we'd
        // be one epoch behind on disk).
        if let Err(e) = group.save(&group_repo) {
            tracing::warn!(
                target: "skattr::daemon::dispatch",
                err = %e,
                "card-send: save group failed; skipping"
            );
            return;
        }
        // Block value: (msg_id, ciphertext). The `_guard` drops here, releasing
        // the per-group lock before the `hub.send().await` below.
        (msg_id, ct)
    };
    let (msg_id, ciphertext) = prepared;
    let _ = handle.hub.send(peer, msg_id, ciphertext).await;
}

/// Build a fresh self-card and fan it out to every contact via the MLS
/// app-message channel + `DeliveryHub::send` (which itself owns the
/// direct → mailbox-fallback path). Best-effort: any single contact's
/// failure is logged and skipped; the overall publish does not abort.
///
/// The new card row is persisted via `build_next_self_card` BEFORE any
/// send, so even if every send fails the version counter has bumped and
/// the next publish picks up from there.
async fn publish_self_card_update<S>(
    handle: Arc<DaemonHandle<S>>,
) -> std::result::Result<(), IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::storage::ContactRepo;

    let card = build_self_card(&handle)?;

    let contacts = ContactRepo::new(&handle.pool).list().map_err(map_err)?;
    for contact in contacts {
        send_card_to_contact(&handle, &card, contact.identity).await;
    }
    Ok(())
}

/// `RemoveMailbox` handler: mark `pending_removal`, attempt a best-effort
/// final drain (fetch → dispatch → delete-only-dispatched via
/// `poll_dispatch_once`), then mark `removed`, stop the poll actor, and
/// republish the self-card.
///
/// If the mailbox is unreachable during the drain attempt, we proceed
/// anyway — callers must not get stuck on a misbehaving mailbox.
///
/// The drain dispatches each held deposit through the inbound MLS pipeline
/// ([`InboundDispatch::dispatch_mailbox`]) and server-side deletes ONLY the
/// deposits that persisted, so offline messages are preserved into local
/// storage before the mailbox is forgotten and a transient dispatch failure
/// cannot lose them on this irreversible path (Task 22.5). The drain is
/// skipped entirely when no inbound dispatcher is wired (test handles).
async fn handle_remove_mailbox<S>(
    handle: Arc<DaemonHandle<S>>,
    id: i64,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::error_kind::DaemonErrorKind;

    let repo = crate::storage::MailboxRepo::new(&handle.pool);

    // 1. Find the row.
    let row = match repo.get(id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            return Err(IpcError::Daemon(DaemonErrorKind::InvalidArgument {
                message: "not_found".into(),
            }))
        }
        Err(e) => return Err(map_err(e)),
    };

    // 2. Mark pending_removal + emit status event.
    repo.mark_pending_removal(id).map_err(map_err)?;
    let _ = handle
        .events_tx
        .send(crate::daemon::events::Event::MailboxStatusChanged {
            mailbox_id: id,
            status: crate::storage::MailboxStatus::PendingRemoval,
        });

    // 3. Best-effort final drain: fetch → dispatch → delete-only-dispatched, so a
    //    transient dispatch failure on this irreversible removal path does not
    //    destroy held offline messages (poll_dispatch_once, not run_one_poll_tick).
    //    We only drain when an inbound dispatcher is wired; without one we cannot
    //    persist deposits, so we skip the drain entirely rather than delete
    //    messages we can't dispatch. Any error (unreachable, auth, timeout) is
    //    swallowed — removal must finalize regardless (Task 22.5).
    if let (Some(factory), Some(inbound)) = (&handle.mailbox_factory, &handle.inbound) {
        if let Ok(mut client) = factory.connect(&row.onion).await {
            let _ =
                crate::mailbox::poll::poll_dispatch_once(&mut client, &handle.identity, &**inbound)
                    .await;
        }
    }

    // 4. Tell scheduler to stop the actor.
    if let Some(ctrl) = &handle.poller_ctrl {
        let _ = ctrl
            .send(crate::mailbox::poll::PollerCtrl::RemoveMailbox(id))
            .await;
    }

    // 5. Finalize.
    repo.finalize_removal(id).map_err(map_err)?;
    let _ = handle
        .events_tx
        .send(crate::daemon::events::Event::MailboxStatusChanged {
            mailbox_id: id,
            status: crate::storage::MailboxStatus::Removed,
        });

    // 6. Republish card (best-effort: if onion isn't set yet the helper
    //    returns TorNotReady; propagate that to the caller).
    publish_self_card_update(handle.clone()).await?;

    Ok(CommandResult::Ok)
}

/// `ListMailboxes` handler: return every `role='mine'` row.
async fn handle_list_mailboxes<S>(
    handle: Arc<DaemonHandle<S>>,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::commands::MailboxSummary;

    let rows = crate::storage::MailboxRepo::new(&handle.pool)
        .list_mine()
        .map_err(map_err)?;
    let summaries = rows
        .into_iter()
        .map(|r| MailboxSummary {
            id: r.id,
            onion: r.onion,
            status: r.status,
            registered_at: u64::try_from(r.registered_at).unwrap_or(0),
        })
        .collect();
    Ok(CommandResult::Mailboxes(summaries))
}

async fn handle_daemon_info<S>(
    handle: Arc<DaemonHandle<S>>,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let local_pubkey = handle.identity.public();
    let current_onion = handle.onion();
    let daemon_version = env!("CARGO_PKG_VERSION").to_string();
    let schema_version = handle.pool.schema_version().map_err(map_err)?;

    Ok(CommandResult::DaemonInfo {
        local_pubkey,
        current_onion,
        daemon_version,
        schema_version,
    })
}

async fn rename_contact<S>(
    handle: &Arc<DaemonHandle<S>>,
    contact: crate::identity::PublicKey,
    nickname: Option<String>,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::error_kind::DaemonErrorKind;
    use crate::daemon::events::Event;
    use crate::storage::ContactRepo;

    let trimmed = match nickname {
        None => None,
        Some(s) => {
            let t = s.trim().to_string();
            if t.is_empty() {
                return Err(IpcError::Daemon(DaemonErrorKind::InvalidArgument {
                    message: "nickname must not be empty or whitespace-only".into(),
                }));
            }
            if t.chars().count() > 64 {
                return Err(IpcError::Daemon(DaemonErrorKind::InvalidArgument {
                    message: "nickname must be 64 characters or fewer".into(),
                }));
            }
            Some(t)
        }
    };

    let repo = ContactRepo::new(&handle.pool);
    repo.set_display_name(&contact, trimmed.as_deref())
        .map_err(map_err)?;
    let _ = handle.events_tx.send(Event::ContactUpdated(contact));
    Ok(CommandResult::Ok)
}

async fn remove_contact<S>(
    handle: &Arc<DaemonHandle<S>>,
    contact: crate::identity::PublicKey,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::events::Event;
    use crate::storage::ContactRepo;

    let repo = ContactRepo::new(&handle.pool);
    repo.set_hidden(&contact, true).map_err(map_err)?;
    let _ = handle.events_tx.send(Event::ContactUpdated(contact));
    Ok(CommandResult::Ok)
}

async fn set_contact_muted<S>(
    handle: &Arc<DaemonHandle<S>>,
    contact: crate::identity::PublicKey,
    muted: bool,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::events::Event;
    use crate::storage::ContactRepo;

    let repo = ContactRepo::new(&handle.pool);
    // Check existence first — must fail with ContactNotFound, not silently no-op.
    if repo.get(&contact).map_err(map_err)?.is_none() {
        use crate::daemon::error_kind::DaemonErrorKind;
        return Err(IpcError::Daemon(DaemonErrorKind::ContactNotFound));
    }
    repo.set_muted(&contact, muted).map_err(map_err)?;

    // Emit ContactUpdated so live UIs re-fetch the contact summary.
    let _ = handle.events_tx.send(Event::ContactUpdated(contact));
    Ok(CommandResult::Ok)
}

async fn get_config<S>(
    handle: &Arc<DaemonHandle<S>>,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let cfg = handle.config.read().await;
    Ok(CommandResult::Config(cfg.snapshot()))
}

async fn set_config<S>(
    handle: &Arc<DaemonHandle<S>>,
    patch: crate::daemon::commands::ConfigPatch,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::error_kind::DaemonErrorKind;
    let mut cfg = handle.config.write().await;
    cfg.apply_patch(&patch).map_err(|e| {
        IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            message: e.to_string(),
        })
    })?;
    cfg.save_to_disk(&handle.config_path).map_err(|e| {
        IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            message: format!("save_to_disk: {e}"),
        })
    })?;
    // persist_logs_to_disk: flag is saved to config.toml (apply_patch
    // already did that); the change takes effect on the next daemon
    // restart. Hot-toggle is deferred because tracing-subscriber's
    // reload::Layer generics are complex to compose across the existing
    // layered subscriber stack. The subscriber-init path reads this flag
    // on startup and installs the appender accordingly.
    Ok(CommandResult::Ok)
}

/// Map any `CoreError` to an `IpcError`. Projects via `CoreError::kind`
/// into `DaemonErrorKind`; otherwise `Internal(...)` with a truncated
/// display. Logs the full error server-side.
pub(crate) fn map_err(err: CoreError) -> IpcError {
    if let Some(kind) = err.kind() {
        tracing::warn!(?err, ?kind, "ipc: typed daemon error");
        IpcError::Daemon(kind)
    } else {
        let msg = format!("{err}");
        let truncated: String = msg.chars().take(256).collect();
        tracing::warn!(?err, "ipc: internal error");
        IpcError::Internal(truncated)
    }
}

async fn get_passphrase_audit_latest<S>(
    handle: &Arc<DaemonHandle<S>>,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::storage::PassphraseAuditRepo;
    let repo = PassphraseAuditRepo::new(&handle.pool);
    let ts = repo.latest_ts().map_err(map_err)?;
    Ok(CommandResult::PassphraseAudit {
        last_changed_unix: ts.map(|v| v as u64),
    })
}

async fn tail_logs<S>(
    handle: &Arc<DaemonHandle<S>>,
    since_seq: Option<u64>,
    limit: u32,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (records, next_since_seq) = handle.log_sink.snapshot(since_seq, limit as usize);
    Ok(CommandResult::Logs {
        records,
        next_since_seq,
    })
}

async fn change_passphrase<S>(
    handle: &Arc<DaemonHandle<S>>,
    old: String,
    new: String,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::error_kind::DaemonErrorKind;
    use crate::storage::{AuditOutcome, PassphraseAuditRepo};
    use zeroize::Zeroizing;

    let old = Zeroizing::new(old);
    let new = Zeroizing::new(new);

    if new.len() < 8 {
        return Err(IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            message: "new passphrase must be at least 8 characters".into(),
        }));
    }
    let entropy = zxcvbn::zxcvbn(new.as_str(), &[]);
    if entropy.score() < zxcvbn::Score::Three {
        return Err(IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            message: "new passphrase too weak (zxcvbn score < 3)".into(),
        }));
    }
    if *old == *new {
        return Err(IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            message: "new passphrase must differ from current".into(),
        }));
    }

    // Resolve the vault path under the live data_dir.
    let data_dir = handle.config.read().await.data_dir.clone();
    let vault_path = data_dir.join("identity.vault");

    // Open the vault under the OLD passphrase first — failure here means
    // the user gave us the wrong current passphrase.
    let (mut vault, _identity) = crate::identity::Vault::open(&vault_path, old.as_str())
        .map_err(|_| IpcError::Daemon(DaemonErrorKind::Unauthorized))?;

    // Vault::change_passphrase is atomic on its own (sidecar + rename).
    // The storage age key is derived from the BIP39 seed via HKDF and is
    // independent of the user passphrase, so this single rewrite is the
    // entire rekey surface.
    vault
        .change_passphrase(old.as_str(), new.as_str())
        .map_err(map_err)?;

    // Append the audit row. Best-effort: a failure here doesn't
    // unwind the rekey (already on disk), but does surface an error
    // to the caller so the UI can warn.
    let audit = PassphraseAuditRepo::new(&handle.pool);
    audit
        .append(
            crate::daemon::clock::now_unix_seconds(),
            AuditOutcome::Changed,
        )
        .map_err(map_err)?;

    Ok(CommandResult::PassphraseChanged)
}

// ---------------------------------------------------------------------------
// WipeAllData handler
// ---------------------------------------------------------------------------

async fn wipe_all_data<S>(
    handle: Arc<DaemonHandle<S>>,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // Resolve data_dir from the live config (same pattern as change_passphrase).
    // Read-lock dropped before the spawn so nothing holds the lock during teardown.
    let data_dir = handle.config.read().await.data_dir.clone();

    // Spawn the teardown so we can return Ok BEFORE the IPC layer tears down.
    // This is the ONE handler that intentionally outlives its caller.
    tokio::spawn(async move {
        // Allow ~150ms for the reply to flush back over the IPC stream.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        // Best-effort: drop the handle so background tasks (retention sweep,
        // log tap, mailbox poller) get a chance to wind down.
        std::mem::drop(handle);

        // Brief settle to let in-flight tasks finish their current ticks.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        // Wipe the data directory.
        if let Err(e) = tokio::fs::remove_dir_all(&data_dir).await {
            tracing::error!(
                error = %e,
                dir = ?data_dir,
                "wipe_all_data: remove_dir_all failed; exiting anyway"
            );
        }
        std::process::exit(0);
    });

    Ok(CommandResult::Ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    use crate::daemon::commands::{Command, CommandResult};
    use crate::daemon::events::Event;
    use crate::daemon::handle::DaemonHandle;
    use crate::daemon::ipc::wire::IpcError;
    use crate::delivery::dial::OutboundDial;
    use crate::delivery::hub::DeliveryHub;
    use crate::identity::{IdentityKey, PublicKey as IdPublicKey, Seed};
    use crate::storage::Pool;
    use crate::transport::{handshake_initiator, handshake_responder, AuthenticatedConnection};

    fn test_handle() -> Arc<DaemonHandle<tokio::io::DuplexStream>> {
        let seed = Seed::generate().unwrap();
        let identity = IdentityKey::from_seed(&seed).unwrap();
        let pool = Arc::new(Pool::in_memory());
        let hub: Arc<DeliveryHub<tokio::io::DuplexStream>> =
            Arc::new(DeliveryHub::new(pool.clone()));
        let (events_tx, _) = broadcast::channel::<Event>(16);
        Arc::new(DaemonHandle::new(pool, hub, identity, events_tx))
    }

    /// Stub dialer for `add_contact` tests: on each `dial`, runs a real
    /// Noise_XK handshake over an in-process duplex and hands back the
    /// initiator's authenticated connection plus the handshake's `h_transport`.
    /// `add_contact`'s dial is mandatory (ADR 0009, T1-1), so the single-daemon
    /// dispatch tests need a working dialer; they do not exercise the binding
    /// round-trip (no joiner present), only that the committer dial succeeds.
    /// The responder side is dropped immediately — the subsequent best-effort
    /// Welcome send over the conn just fails quietly, which is fine here.
    struct StubDialer;

    #[async_trait::async_trait]
    impl OutboundDial<tokio::io::DuplexStream> for StubDialer {
        // `dial_at` ignores the supplied onion (the stub has no real transport)
        // and returns the same canned authenticated connection as `dial`.
        async fn dial_at(
            &self,
            peer: IdPublicKey,
            _onion: &str,
        ) -> crate::error::Result<(
            AuthenticatedConnection<tokio::io::DuplexStream>,
            zeroize::Zeroizing<[u8; 32]>,
        )> {
            self.dial(peer).await
        }

        async fn dial(
            &self,
            _peer: IdPublicKey,
        ) -> crate::error::Result<(
            AuthenticatedConnection<tokio::io::DuplexStream>,
            zeroize::Zeroizing<[u8; 32]>,
        )> {
            // Build a self-consistent Noise_XK pair: the initiator must target
            // the responder's ACTUAL static (rs), so derive `peer_x` from the
            // throwaway responder identity — NOT from the real `peer` (we don't
            // hold the inviter's private key, and these single-daemon tests do
            // not exercise the binding round-trip). Setup steps are infallible
            // in the test environment, so `unwrap` is acceptable (the test
            // module allows it); a real handshake error surfaces as a Delivery
            // error via `?`.
            let init_id = IdentityKey::from_seed(&Seed::generate().unwrap()).unwrap();
            let resp_id = IdentityKey::from_seed(&Seed::generate().unwrap()).unwrap();
            let peer_x = resp_id.noise_static_public();
            let (a, b) = tokio::io::duplex(64 * 1024);
            // Drive both handshake halves concurrently on this task (works on a
            // current_thread runtime — no spawn). The responder side is then
            // dropped; the subsequent best-effort Welcome send over the conn
            // just fails quietly, which is fine for these single-daemon tests.
            let (init_res, _resp_res) = tokio::join!(
                handshake_initiator(a, &init_id, &peer_x, None),
                handshake_responder(b, &resp_id, None),
            );
            let (conn, outcome) = init_res?;
            Ok((conn, outcome.h_transport))
        }
    }

    /// Like [`test_handle`] but with a working stub dialer wired into the hub,
    /// for tests that drive `add_contact` (whose dial is mandatory).
    fn test_handle_with_dialer() -> Arc<DaemonHandle<tokio::io::DuplexStream>> {
        let seed = Seed::generate().unwrap();
        let identity = IdentityKey::from_seed(&seed).unwrap();
        let pool = Arc::new(Pool::in_memory());
        let dialer: Arc<dyn OutboundDial<tokio::io::DuplexStream>> = Arc::new(StubDialer);
        let hub: Arc<DeliveryHub<tokio::io::DuplexStream>> =
            Arc::new(DeliveryHub::new_with_dialer(pool.clone(), dialer));
        let (events_tx, _) = broadcast::channel::<Event>(16);
        Arc::new(DaemonHandle::new(pool, hub, identity, events_tx))
    }

    #[tokio::test]
    async fn shutdown_returns_ok() {
        let handle = test_handle();
        let result = execute_command(handle, Command::Shutdown).await;
        assert!(matches!(result, Ok(CommandResult::Ok)));
    }

    #[tokio::test]
    async fn change_passphrase_rejects_too_short_new() {
        let handle = test_handle();
        let err = execute_command(
            handle,
            Command::ChangePassphrase {
                old: "anything-old".into(),
                new: "short".into(),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            IpcError::Daemon(crate::daemon::error_kind::DaemonErrorKind::InvalidArgument { .. })
        ));
    }

    #[tokio::test]
    async fn change_passphrase_rejects_equal_old_and_new() {
        let handle = test_handle();
        let err = execute_command(
            handle,
            Command::ChangePassphrase {
                old: "samepassphrase".into(),
                new: "samepassphrase".into(),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            IpcError::Daemon(crate::daemon::error_kind::DaemonErrorKind::InvalidArgument { .. })
        ));
    }

    #[tokio::test]
    async fn change_passphrase_rejects_weak_new() {
        let handle = test_handle();
        let err = execute_command(
            handle,
            Command::ChangePassphrase {
                old: "current-passphrase".into(),
                new: "abcdefgh".into(), // 8 chars but extremely weak per zxcvbn
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            IpcError::Daemon(crate::daemon::error_kind::DaemonErrorKind::InvalidArgument { .. })
        ));
    }

    #[tokio::test]
    async fn create_group_returns_unknown_command() {
        let handle = test_handle();
        let result = execute_command(
            handle,
            Command::CreateGroup {
                members: vec![],
                name: "x".into(),
            },
        )
        .await;
        assert!(matches!(result, Err(IpcError::UnknownCommand)));
    }

    use crate::contact::Contact;
    use crate::daemon::commands::ContactSummary;
    use crate::identity::PublicKey;
    use crate::storage::ContactRepo;

    #[tokio::test]
    async fn create_invite_returns_parseable_url_and_records_keypackage() {
        let handle = test_handle();
        // Set the onion so invite can embed it (publishes typically do).
        handle.set_onion("testonion".repeat(8));

        let result = execute_command(
            handle.clone(),
            Command::CreateInvite {
                nickname: None,
                ttl_secs: Some(3600),
            },
        )
        .await
        .unwrap();

        let (url, kpi, expires_at) = match result {
            CommandResult::InviteCreated {
                url,
                key_package_id,
                expires_at,
            } => (url, key_package_id, expires_at),
            other => panic!("expected InviteCreated, got {other:?}"),
        };
        assert!(url.starts_with("skattr://invite/v1#"), "url={url}");
        assert!(expires_at > 0);
        assert_ne!(kpi.0, [0u8; 32]);

        // The URL parses back cleanly.
        let parsed = crate::invite::InviteLink::from_url(&url, 1).unwrap();
        assert_eq!(parsed.body.card.body.onion, "testonion".repeat(8));

        // key_package_id is now the canonical MLS KeyPackageRef (KeyPackageRef
        // computed by make_key_package_ref), not plain SHA-256. It is still
        // 32 non-zero bytes.
        assert_ne!(kpi.0, [0u8; 32]);
    }

    #[tokio::test]
    async fn create_invite_persists_outstanding_invite_row() {
        use crate::storage::OutstandingInviteRepo;

        let handle = test_handle();
        handle.set_onion("testonion".repeat(8));

        let result = execute_command(
            handle.clone(),
            Command::CreateInvite {
                nickname: None,
                ttl_secs: Some(3600),
            },
        )
        .await
        .unwrap();

        let kp_ref = match result {
            CommandResult::InviteCreated { key_package_id, .. } => key_package_id.0,
            other => panic!("unexpected: {other:?}"),
        };

        let oi = OutstandingInviteRepo::new(&handle.pool);
        let (psk, expires_at) = oi.get_psk(&kp_ref).unwrap().expect("row must exist");
        assert_eq!(psk.as_ref().len(), 32);
        let now = crate::daemon::clock::now_unix_seconds();
        assert!(expires_at >= now + 3500 && expires_at <= now + 3700);
    }

    #[tokio::test]
    async fn create_invite_without_onion_returns_tor_not_ready() {
        let handle = test_handle();
        // onion not set — still None
        let result = execute_command(
            handle,
            Command::CreateInvite {
                nickname: None,
                ttl_secs: Some(3600),
            },
        )
        .await;
        assert!(
            matches!(
                result,
                Err(IpcError::Daemon(
                    crate::daemon::error_kind::DaemonErrorKind::TorNotReady
                ))
            ),
            "expected TorNotReady, got {result:?}"
        );
    }

    #[tokio::test]
    async fn add_contact_from_self_invite_persists_group_link_and_emits_event() {
        let handle_a = test_handle();
        handle_a.set_onion("alice.onion".to_string());

        // Alice creates an invite.
        let CommandResult::InviteCreated { url, .. } = execute_command(
            handle_a.clone(),
            Command::CreateInvite {
                nickname: None,
                ttl_secs: Some(3600),
            },
        )
        .await
        .unwrap() else {
            panic!("expected InviteCreated");
        };

        // Bob's handle consumes it. Bob is a separate daemon with a separate pool.
        let handle_b = test_handle_with_dialer();
        let mut events_rx = handle_b.events_tx.subscribe();
        let res = execute_command(handle_b.clone(), Command::AddContact { invite_url: url })
            .await
            .unwrap();
        let summary = match res {
            CommandResult::ContactAdded(s) => s,
            other => panic!("expected ContactAdded, got {other:?}"),
        };

        // Contact row written with a non-empty group_id.
        let repo = ContactRepo::new(&handle_b.pool);
        let gid = repo.get_group_id(&summary.pubkey).unwrap().unwrap();
        assert!(!gid.is_empty(), "group_id must be set");

        // Event fired.
        match tokio::time::timeout(std::time::Duration::from_secs(1), events_rx.recv()).await {
            Ok(Ok(Event::ContactUpdated(pk))) => assert_eq!(pk, summary.pubkey),
            other => panic!("expected ContactUpdated event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn add_contact_persists_inviter_card_for_dialer() {
        // Alice (inviter) sets an onion so the embedded self-card carries it,
        // and so the dialer's send_welcome succeeds.
        let handle_a = test_handle();
        handle_a.set_onion("alice.onion".to_string());
        let alice_pub = handle_a.identity.public();

        let CommandResult::InviteCreated { url, .. } = execute_command(
            handle_a.clone(),
            Command::CreateInvite {
                nickname: None,
                ttl_secs: Some(3600),
            },
        )
        .await
        .unwrap() else {
            panic!("expected InviteCreated");
        };

        // Bob (dialer) consumes the invite.
        let handle_b = test_handle_with_dialer();
        execute_command(handle_b.clone(), Command::AddContact { invite_url: url })
            .await
            .unwrap();

        // Bob can now resolve Alice's onion via her persisted card.
        let card = ContactRepo::new(&handle_b.pool)
            .latest_card(&alice_pub)
            .unwrap()
            .expect("inviter card must be persisted");
        assert_eq!(card.body.onion, "alice.onion");
    }

    #[tokio::test]
    async fn add_contact_double_use_is_rejected() {
        let handle_a = test_handle();
        handle_a.set_onion("alice.onion".to_string());

        let CommandResult::InviteCreated { url, .. } = execute_command(
            handle_a.clone(),
            Command::CreateInvite {
                nickname: None,
                ttl_secs: Some(3600),
            },
        )
        .await
        .unwrap() else {
            panic!("expected InviteCreated");
        };

        let handle_b = test_handle_with_dialer();
        // First use succeeds.
        execute_command(
            handle_b.clone(),
            Command::AddContact {
                invite_url: url.clone(),
            },
        )
        .await
        .unwrap();
        // Second use is rejected.
        let res = execute_command(handle_b.clone(), Command::AddContact { invite_url: url }).await;
        assert!(
            matches!(
                res,
                Err(IpcError::Daemon(
                    crate::daemon::error_kind::DaemonErrorKind::InviteConsumed
                ))
            ),
            "expected InviteConsumed, got {res:?}"
        );
    }

    /// T2-1: an invite is single-use *atomically* — re-submitting the same
    /// invite URL must reject with InviteConsumed and must NOT create a second
    /// group or a second contact. The consumed re-check + group write +
    /// mark_consumed run in one transaction, so the second AddContact can never
    /// pass the in-tx check and write a second group.
    #[tokio::test]
    async fn add_contact_is_single_use_under_resubmit() {
        use crate::mls::group::{Group, GroupId};
        use crate::storage::MlsGroupRepo;

        let handle_a = test_handle();
        handle_a.set_onion("alice.onion".to_string());
        let alice_pub = handle_a.identity.public();

        let CommandResult::InviteCreated { url, .. } = execute_command(
            handle_a.clone(),
            Command::CreateInvite {
                nickname: None,
                ttl_secs: Some(3600),
            },
        )
        .await
        .unwrap() else {
            panic!("expected InviteCreated");
        };

        let handle_b = test_handle_with_dialer();

        // First AddContact succeeds.
        let first = execute_command(
            handle_b.clone(),
            Command::AddContact {
                invite_url: url.clone(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(first, CommandResult::ContactAdded(_)));

        // Capture the single group_id created for Alice.
        let repo = ContactRepo::new(&handle_b.pool);
        let gid_after_first = repo
            .get_group_id(&alice_pub)
            .unwrap()
            .expect("group_id present after first AddContact");
        assert!(!gid_after_first.is_empty());

        // Re-submitting the SAME invite must reject and write nothing new.
        let second =
            execute_command(handle_b.clone(), Command::AddContact { invite_url: url }).await;
        assert!(
            matches!(
                second,
                Err(IpcError::Daemon(
                    crate::daemon::error_kind::DaemonErrorKind::InviteConsumed
                ))
            ),
            "second AddContact must be InviteConsumed, got {second:?}"
        );

        // Exactly ONE contact for Alice, and its group_id is unchanged.
        let all = repo.list_all().unwrap();
        let alice_rows = all.iter().filter(|c| c.identity == alice_pub).count();
        assert_eq!(alice_rows, 1, "exactly one contact row for the inviter");
        let gid_after_second = repo
            .get_group_id(&alice_pub)
            .unwrap()
            .expect("group_id still present");
        assert_eq!(
            gid_after_first, gid_after_second,
            "the inviter's group_id must not change on a rejected re-submit"
        );

        // The single group exists and loads; no orphan second group was written
        // (the rejected attempt's create_solo group was never saved).
        let group_repo = MlsGroupRepo::new(&handle_b.pool);
        let loaded = Group::load(&GroupId(gid_after_first.clone()), &group_repo).unwrap();
        assert!(loaded.is_some(), "the one genesis group must be persisted");
    }

    /// T2-1 (full atomicity): a dial failure during `add_contact` must leave
    /// ZERO writes — no contact row, no card, no group — so a later retry (with
    /// a working dialer) succeeds cleanly. Before this fix the inviter contact +
    /// card were persisted BEFORE the (mandatory) dial, so a routine dial
    /// failure stranded a contact + card and the monotonic-version `put_card`
    /// guard blocked the natural retry. Now every write lives in one
    /// transaction that only runs after the dial succeeds.
    #[tokio::test]
    async fn add_contact_dial_failure_writes_nothing_then_retry_succeeds() {
        let handle_a = test_handle();
        handle_a.set_onion("alice.onion".to_string());
        let alice_pub = handle_a.identity.public();

        let CommandResult::InviteCreated { url, .. } = execute_command(
            handle_a.clone(),
            Command::CreateInvite {
                nickname: None,
                ttl_secs: Some(3600),
            },
        )
        .await
        .unwrap() else {
            panic!("expected InviteCreated");
        };

        // Bob's daemon has NO dialer wired → connect_and_ingest_at errors
        // ("no dialer wired") BEFORE any DB write.
        let handle_b_nodial = test_handle();
        let failed = execute_command(
            handle_b_nodial.clone(),
            Command::AddContact {
                invite_url: url.clone(),
            },
        )
        .await;
        assert!(failed.is_err(), "add_contact must fail when the dial fails");

        // Nothing was written: no contact row, no card, no group_id for Alice.
        let repo = ContactRepo::new(&handle_b_nodial.pool);
        assert!(
            repo.get(&alice_pub).unwrap().is_none(),
            "dial failure must leave NO contact row (no stranded contact)"
        );
        assert!(
            repo.latest_card(&alice_pub).unwrap().is_none(),
            "dial failure must leave NO card"
        );

        // A retry on a fresh daemon WITH a working dialer succeeds cleanly —
        // proving the invite is still spendable and nothing is stuck. (We use a
        // fresh daemon because the failed attempt's daemon never persisted the
        // inbound KP either; the invite URL itself is unconsumed.)
        let handle_b = test_handle_with_dialer();
        let ok = execute_command(handle_b.clone(), Command::AddContact { invite_url: url })
            .await
            .unwrap();
        assert!(
            matches!(ok, CommandResult::ContactAdded(_)),
            "retry with a working dialer must succeed"
        );
        let repo2 = ContactRepo::new(&handle_b.pool);
        assert!(
            repo2.get(&alice_pub).unwrap().is_some(),
            "retry persists the contact"
        );
        let gid = repo2.get_group_id(&alice_pub).unwrap().unwrap();
        assert!(!gid.is_empty(), "retry persists the group_id");
    }

    #[tokio::test]
    async fn list_contacts_returns_all_rows_projected() {
        let handle = test_handle();
        // Seed two contacts directly via the repo.
        {
            let repo = ContactRepo::new(&handle.pool);
            repo.upsert(&Contact {
                identity: PublicKey([0x01; 32]),
                display_name: Some("alice".into()),
                added_at: 1_700_000_000,
                card: None,
                muted: false,
            })
            .unwrap();
            repo.upsert(&Contact {
                identity: PublicKey([0x02; 32]),
                display_name: None,
                added_at: 1_700_000_100,
                card: None,
                muted: false,
            })
            .unwrap();
        }

        let result = execute_command(handle, Command::ListContacts)
            .await
            .unwrap();
        match result {
            CommandResult::Contacts(summaries) => {
                assert_eq!(summaries.len(), 2);
                let names: Vec<Option<String>> =
                    summaries.iter().map(|s| s.nickname.clone()).collect();
                assert!(names.contains(&Some("alice".into())));
                assert!(names.contains(&None));
                // No card yet -> onion is empty string, version 0.
                for s in &summaries {
                    if s.nickname == Some("alice".into()) {
                        assert_eq!(s.onion, "");
                        assert_eq!(s.card_version, 0);
                    }
                }
            }
            other => panic!("expected Contacts, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_contacts_projects_muted_and_peer_mailboxes() {
        use crate::contact::card::{ContactCard, ContactCardBody};
        use crate::identity::Signature;
        use crate::storage::ContactRepo;

        let handle = test_handle();
        let peer = PublicKey([0x42; 32]);

        {
            let repo = ContactRepo::new(&handle.pool);
            // Seed a muted contact.
            repo.upsert(&Contact {
                identity: peer,
                display_name: Some("muted-peer".into()),
                added_at: 1_700_000_000,
                card: None,
                muted: false,
            })
            .unwrap();
            repo.set_muted(&peer, true).unwrap();

            // Seed a ContactCard with two mailboxes.
            let card = ContactCard {
                body: ContactCardBody {
                    identity: peer,
                    onion: "xyzxyz.onion".into(),
                    mailboxes: vec!["mailbox1.onion".into(), "mailbox2.onion".into()],
                    version: 1,
                    expires_at: 9_999_999_999,
                },
                // Signature not verified by put_card; zeroed bytes suffice.
                signature: Signature([0u8; 64]),
            };
            repo.put_card(&card).unwrap();
        }

        let result = execute_command(handle, Command::ListContacts)
            .await
            .unwrap();
        match result {
            CommandResult::Contacts(summaries) => {
                assert_eq!(summaries.len(), 1);
                let s = &summaries[0];
                assert!(s.muted, "muted should be true");
                assert_eq!(
                    s.peer_mailboxes,
                    vec!["mailbox1.onion".to_string(), "mailbox2.onion".to_string()]
                );
            }
            other => panic!("expected Contacts, got {other:?}"),
        }
    }

    use crate::daemon::commands::SendStatus;
    use crate::envelope::Kind;

    #[tokio::test]
    async fn send_message_to_unknown_contact_returns_contact_not_found() {
        let handle = test_handle();
        let res = execute_command(
            handle,
            Command::SendMessage {
                contact: crate::identity::PublicKey([0x99; 32]),
                kind: Kind::Text { body: "hi".into() },
            },
        )
        .await;
        assert!(matches!(
            res,
            Err(IpcError::Daemon(
                crate::daemon::error_kind::DaemonErrorKind::ContactNotFound
            ))
        ));
    }

    #[tokio::test]
    async fn send_message_without_group_returns_contact_not_found() {
        let handle = test_handle();
        let peer = crate::identity::PublicKey([0x10; 32]);

        // Seed a contact row but do NOT set group_id (stays empty blob).
        let repo = ContactRepo::new(&handle.pool);
        repo.upsert(&crate::contact::Contact {
            identity: peer,
            display_name: None,
            added_at: 0,
            card: None,
            muted: false,
        })
        .unwrap();

        let res = execute_command(
            handle,
            Command::SendMessage {
                contact: peer,
                kind: Kind::Text { body: "hi".into() },
            },
        )
        .await;
        // Empty group_id is the "not linked" state.
        assert!(matches!(
            res,
            Err(IpcError::Daemon(
                crate::daemon::error_kind::DaemonErrorKind::ContactNotFound
            ))
        ));
    }

    #[tokio::test]
    async fn recent_messages_returns_contact_not_found_for_unknown_contact() {
        let handle = test_handle();
        let res = execute_command(
            handle,
            Command::RecentMessages {
                contact: Some(crate::identity::PublicKey([0x88; 32])),
                limit: 50,
                before_id: None,
                paged: false,
            },
        )
        .await;
        assert!(
            matches!(
                res,
                Err(IpcError::Daemon(
                    crate::daemon::error_kind::DaemonErrorKind::ContactNotFound
                ))
            ),
            "expected ContactNotFound, got {res:?}"
        );
    }

    #[tokio::test]
    async fn recent_messages_projects_stored_rows() {
        use crate::daemon::commands::{Direction, MessageRecord};
        use crate::envelope::{Envelope, Kind, MessageId};
        use crate::storage::MessageRepo;

        let handle = test_handle();
        let peer = crate::identity::PublicKey([0x10; 32]);
        let gid = [0xAAu8; 32];

        // Seed contact + group_id mapping.
        let cr = ContactRepo::new(&handle.pool);
        cr.upsert(&crate::contact::Contact {
            identity: peer,
            display_name: None,
            added_at: 0,
            card: None,
            muted: false,
        })
        .unwrap();
        cr.set_group_id(&peer, &gid).unwrap();

        // Insert one message row (sender = peer, so direction = Incoming).
        let mr = MessageRepo::new(&handle.pool);
        let env = Envelope {
            v: 1,
            id: MessageId::generate(),
            ts: 1_700_000_000,
            reply_to: None,
            kind: Kind::Text { body: "hey".into() },
        };
        mr.insert(crate::storage::messages::InsertParams {
            group_id: &gid,
            sender: &peer.0,
            envelope: &env,
            mls_generation: 0,
            ts_daemon_recv: env.ts,
        })
        .unwrap();

        let res = execute_command(
            handle.clone(),
            Command::RecentMessages {
                contact: Some(peer),
                limit: 10,
                before_id: None,
                paged: false,
            },
        )
        .await
        .unwrap();

        match res {
            CommandResult::Messages(records) => {
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].contact, peer);
                // Sender (peer) != local identity -> Incoming.
                assert_eq!(records[0].direction, Direction::Incoming);
                assert!(
                    matches!(records[0].kind, Kind::Text { .. }),
                    "expected Text kind"
                );
                assert_ne!(
                    records[0].row_id, 0,
                    "row_id must be the SQLite id, not a placeholder"
                );
            }
            other => panic!("expected Messages, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_messages_returns_empty_vec_for_contact_with_no_messages() {
        let handle = test_handle();
        let peer = crate::identity::PublicKey([0x20; 32]);
        let gid = [0xBBu8; 32];

        let cr = ContactRepo::new(&handle.pool);
        cr.upsert(&crate::contact::Contact {
            identity: peer,
            display_name: None,
            added_at: 0,
            card: None,
            muted: false,
        })
        .unwrap();
        cr.set_group_id(&peer, &gid).unwrap();

        let res = execute_command(
            handle,
            Command::RecentMessages {
                contact: Some(peer),
                limit: 10,
                before_id: None,
                paged: false,
            },
        )
        .await
        .unwrap();

        match res {
            CommandResult::Messages(records) => assert!(records.is_empty()),
            other => panic!("expected Messages, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_message_with_real_group_yields_queued_without_transport() {
        // Alice creates an invite. Bob (separate identity + pool) consumes
        // it, establishing a real MLS group for Alice (as a contact).
        // Bob then sends to Alice. The hub has no peer actor wired, so the
        // DeliveryHub::send will enqueue but no ACK arrives within 2 s.
        // Expected: SendStatus::Queued.
        let handle_a = test_handle();
        handle_a.set_onion("alice.onion".to_string());

        let CommandResult::InviteCreated { url, .. } = execute_command(
            handle_a.clone(),
            Command::CreateInvite {
                nickname: None,
                ttl_secs: Some(3600),
            },
        )
        .await
        .unwrap() else {
            panic!("expected InviteCreated");
        };

        // Bob consumes Alice's invite; this creates a group with Alice as
        // the contact (group_id is set on Alice's pubkey in Bob's pool).
        let handle_b = test_handle_with_dialer();
        let CommandResult::ContactAdded(summary) =
            execute_command(handle_b.clone(), Command::AddContact { invite_url: url })
                .await
                .unwrap()
        else {
            panic!("expected ContactAdded");
        };

        // Bob sends to Alice (summary.pubkey == Alice's pubkey).
        let fut = execute_command(
            handle_b,
            Command::SendMessage {
                contact: summary.pubkey,
                kind: Kind::Text { body: "hi".into() },
            },
        );
        let res = tokio::time::timeout(std::time::Duration::from_secs(3), fut)
            .await
            .expect("outer 3 s budget");
        match res {
            Ok(CommandResult::MessageSent {
                status: SendStatus::Queued,
                ..
            }) => {}
            other => panic!("expected MessageSent(Queued), got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_message_persists_post_encrypt_mls_generation_and_ts_daemon_recv() {
        // Same Alice/Bob invite dance as the sibling Queued test. After
        // Bob sends, assert that the outgoing row Bob persisted carries
        // (a) a non-zero mls_generation (the ratchet advanced on encrypt)
        // and (b) a real ts_daemon_recv clock value (not the 0 legacy
        // default or the envelope.ts placeholder).
        let handle_a = test_handle();
        handle_a.set_onion("alice.onion".to_string());

        let CommandResult::InviteCreated { url, .. } = execute_command(
            handle_a.clone(),
            Command::CreateInvite {
                nickname: None,
                ttl_secs: Some(3600),
            },
        )
        .await
        .unwrap() else {
            panic!("expected InviteCreated");
        };

        let handle_b = test_handle_with_dialer();
        let CommandResult::ContactAdded(summary) =
            execute_command(handle_b.clone(), Command::AddContact { invite_url: url })
                .await
                .unwrap()
        else {
            panic!("expected ContactAdded");
        };

        // Bob sends to Alice; timeout at 3 s (matches the sibling test).
        let fut = execute_command(
            handle_b.clone(),
            Command::SendMessage {
                contact: summary.pubkey,
                kind: Kind::Text { body: "hi".into() },
            },
        );
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), fut)
            .await
            .expect("outer 3 s budget");

        // ContactSummary does not carry a group_id, so look it up the
        // same way the recent_messages handler does.
        let contact_repo = ContactRepo::new(&handle_b.pool);
        let group_id = contact_repo
            .get_group_id(&summary.pubkey)
            .unwrap()
            .expect("group_id present after AddContact");

        let (mls_gen, ts_recv): (i64, i64) = handle_b
            .pool
            .with(|c| {
                c.query_row(
                    "SELECT mls_generation, ts_daemon_recv FROM messages \
                     WHERE group_id = ?1 ORDER BY id DESC LIMIT 1",
                    rusqlite::params![&group_id[..]],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(|e| {
                    crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
                })
            })
            .unwrap();
        assert!(mls_gen > 0, "encrypt advances epoch; got {mls_gen}");
        assert!(
            ts_recv > 1_600_000_000,
            "ts_daemon_recv must be a real clock value; got {ts_recv}"
        );
    }

    #[tokio::test]
    async fn search_messages_returns_bm25_ranked_hits() {
        let handle = test_handle();
        let alice = crate::identity::PublicKey([0x77; 32]);
        let gid = [0x88u8; 32];

        let cr = ContactRepo::new(&handle.pool);
        cr.upsert(&crate::contact::Contact {
            identity: alice,
            display_name: None,
            added_at: 0,
            card: None,
            muted: false,
        })
        .unwrap();
        cr.set_group_id(&alice, &gid).unwrap();

        let msgs = crate::storage::MessageRepo::new(&handle.pool);
        for body in ["alpha bravo", "bravo only", "delta only"] {
            let env = crate::envelope::Envelope {
                v: 1,
                id: crate::envelope::MessageId::generate(),
                ts: 1_700_000_000,
                reply_to: None,
                kind: crate::envelope::Kind::Text { body: body.into() },
            };
            msgs.insert(crate::storage::messages::InsertParams {
                group_id: &gid,
                sender: &alice.0,
                envelope: &env,
                mls_generation: 1,
                ts_daemon_recv: 1_700_000_000,
            })
            .unwrap();
        }

        let result = execute_command(
            handle.clone(),
            Command::SearchMessages {
                query: "bravo".into(),
                contact: None,
                limit: 10,
                offset: 0,
                newest_first: false,
            },
        )
        .await
        .unwrap();
        match result {
            CommandResult::SearchResults(hits) => {
                assert_eq!(hits.len(), 2);
                assert!(hits[0].snippet.contains("bravo"));
            }
            other => panic!("expected SearchResults, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn search_messages_empty_query_returns_empty_results() {
        let handle = test_handle();
        let result = execute_command(
            handle,
            Command::SearchMessages {
                query: "   ".into(),
                contact: None,
                limit: 10,
                offset: 0,
                newest_first: false,
            },
        )
        .await
        .unwrap();
        match result {
            CommandResult::SearchResults(v) => assert!(v.is_empty()),
            other => panic!("expected SearchResults, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn search_messages_unknown_contact_returns_contact_not_found() {
        let handle = test_handle();
        let unknown = crate::identity::PublicKey([0xEE; 32]);
        let err = execute_command(
            handle,
            Command::SearchMessages {
                query: "bravo".into(),
                contact: Some(unknown),
                limit: 10,
                offset: 0,
                newest_first: false,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            IpcError::Daemon(crate::daemon::error_kind::DaemonErrorKind::ContactNotFound)
        ));
    }

    #[tokio::test]
    async fn mark_read_advances_cursor() {
        let handle = test_handle();
        let alice = crate::identity::PublicKey([0x77; 32]);
        let gid = [0x88u8; 32];

        let cr = ContactRepo::new(&handle.pool);
        cr.upsert(&crate::contact::Contact {
            identity: alice,
            display_name: None,
            added_at: 0,
            card: None,
            muted: false,
        })
        .unwrap();
        cr.set_group_id(&alice, &gid).unwrap();

        // Insert a message so the mark_read has an id to point at.
        let msgs = crate::storage::MessageRepo::new(&handle.pool);
        let env = crate::envelope::Envelope {
            v: 1,
            id: crate::envelope::MessageId::generate(),
            ts: 1_700_000_000,
            reply_to: None,
            kind: crate::envelope::Kind::Text { body: "hi".into() },
        };
        let row_id = msgs
            .insert(crate::storage::messages::InsertParams {
                group_id: &gid,
                sender: &alice.0,
                envelope: &env,
                mls_generation: 1,
                ts_daemon_recv: 1_700_000_000,
            })
            .unwrap();

        let result = execute_command(
            handle.clone(),
            Command::MarkRead {
                contact: alice,
                up_to_message_id: row_id,
            },
        )
        .await
        .unwrap();
        match result {
            CommandResult::MarkedRead { up_to } => assert_eq!(up_to, row_id),
            other => panic!("expected MarkedRead, got {other:?}"),
        }

        let cur = crate::storage::ReadStateRepo::new(&handle.pool)
            .get(&gid)
            .unwrap();
        assert_eq!(cur, Some(row_id));
    }

    #[tokio::test]
    async fn mark_read_unknown_contact_returns_contact_not_found() {
        let handle = test_handle();
        let unknown = crate::identity::PublicKey([0xEE; 32]);
        let err = execute_command(
            handle,
            Command::MarkRead {
                contact: unknown,
                up_to_message_id: 42,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            IpcError::Daemon(crate::daemon::error_kind::DaemonErrorKind::ContactNotFound)
        ));
    }

    #[tokio::test]
    async fn prune_history_keep_last_returns_deleted_count() {
        let handle = test_handle();
        let alice = crate::identity::PublicKey([0x77; 32]);
        let gid = [0x88u8; 32];

        let cr = ContactRepo::new(&handle.pool);
        cr.upsert(&crate::contact::Contact {
            identity: alice,
            display_name: None,
            added_at: 0,
            card: None,
            muted: false,
        })
        .unwrap();
        cr.set_group_id(&alice, &gid).unwrap();

        let msgs = crate::storage::MessageRepo::new(&handle.pool);
        for i in 0..8i64 {
            let env = crate::envelope::Envelope {
                v: 1,
                id: crate::envelope::MessageId::generate(),
                ts: 1_700_000_000 + i,
                reply_to: None,
                kind: crate::envelope::Kind::Text {
                    body: format!("m{i}"),
                },
            };
            msgs.insert(crate::storage::messages::InsertParams {
                group_id: &gid,
                sender: &alice.0,
                envelope: &env,
                mls_generation: u64::try_from(i).unwrap(),
                ts_daemon_recv: i,
            })
            .unwrap();
        }

        let result = execute_command(
            handle.clone(),
            Command::PruneHistory {
                contact: Some(alice),
                before_ts_recv: None,
                keep_last: Some(3),
            },
        )
        .await
        .unwrap();
        match result {
            CommandResult::Pruned { rows_deleted } => assert_eq!(rows_deleted, 5),
            other => panic!("expected Pruned, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn prune_history_before_ts_returns_deleted_count() {
        let handle = test_handle();
        let alice = crate::identity::PublicKey([0x77; 32]);
        let gid = [0x99u8; 32];

        let cr = ContactRepo::new(&handle.pool);
        cr.upsert(&crate::contact::Contact {
            identity: alice,
            display_name: None,
            added_at: 0,
            card: None,
            muted: false,
        })
        .unwrap();
        cr.set_group_id(&alice, &gid).unwrap();

        let msgs = crate::storage::MessageRepo::new(&handle.pool);
        for i in 0..6i64 {
            let env = crate::envelope::Envelope {
                v: 1,
                id: crate::envelope::MessageId::generate(),
                ts: 1_700_000_000,
                reply_to: None,
                kind: crate::envelope::Kind::Text {
                    body: format!("t{i}"),
                },
            };
            msgs.insert(crate::storage::messages::InsertParams {
                group_id: &gid,
                sender: &alice.0,
                envelope: &env,
                mls_generation: 0,
                ts_daemon_recv: i * 100, // 0, 100, 200, 300, 400, 500
            })
            .unwrap();
        }

        let result = execute_command(
            handle,
            Command::PruneHistory {
                contact: Some(alice),
                before_ts_recv: Some(250),
                keep_last: None,
            },
        )
        .await
        .unwrap();
        match result {
            CommandResult::Pruned { rows_deleted } => assert_eq!(rows_deleted, 3),
            other => panic!("expected Pruned, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn prune_history_rejects_both_before_and_keep_last() {
        let handle = test_handle();
        let err = execute_command(
            handle,
            Command::PruneHistory {
                contact: None,
                before_ts_recv: Some(1),
                keep_last: Some(2),
            },
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                err,
                IpcError::Daemon(
                    crate::daemon::error_kind::DaemonErrorKind::InvalidArgument { .. }
                )
            ),
            "expected InvalidArgument, got {err:?}"
        );
    }

    #[tokio::test]
    async fn prune_history_rejects_neither() {
        let handle = test_handle();
        let err = execute_command(
            handle,
            Command::PruneHistory {
                contact: None,
                before_ts_recv: None,
                keep_last: None,
            },
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                err,
                IpcError::Daemon(
                    crate::daemon::error_kind::DaemonErrorKind::InvalidArgument { .. }
                )
            ),
            "expected InvalidArgument, got {err:?}"
        );
    }

    #[tokio::test]
    async fn prune_history_keep_last_requires_contact() {
        let handle = test_handle();
        let err = execute_command(
            handle,
            Command::PruneHistory {
                contact: None, // global
                before_ts_recv: None,
                keep_last: Some(3), // but keep_last requires a scoped group
            },
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                err,
                IpcError::Daemon(
                    crate::daemon::error_kind::DaemonErrorKind::InvalidArgument { .. }
                )
            ),
            "expected InvalidArgument, got {err:?}"
        );
    }

    #[tokio::test]
    async fn export_history_paginates_and_advances_cursor() {
        let handle = test_handle();
        let alice = crate::identity::PublicKey([0x77; 32]);
        let gid = [0x88u8; 32];

        let cr = ContactRepo::new(&handle.pool);
        cr.upsert(&crate::contact::Contact {
            identity: alice,
            display_name: None,
            added_at: 0,
            card: None,
            muted: false,
        })
        .unwrap();
        cr.set_group_id(&alice, &gid).unwrap();

        let msgs = crate::storage::MessageRepo::new(&handle.pool);
        for i in 0..5i64 {
            let env = crate::envelope::Envelope {
                v: 1,
                id: crate::envelope::MessageId::generate(),
                ts: 1_700_000_000 + i,
                reply_to: None,
                kind: crate::envelope::Kind::Text {
                    body: format!("m{i}"),
                },
            };
            msgs.insert(crate::storage::messages::InsertParams {
                group_id: &gid,
                sender: &alice.0,
                envelope: &env,
                mls_generation: u64::try_from(i).unwrap(),
                ts_daemon_recv: i,
            })
            .unwrap();
        }

        // Page 1: after_id = None, limit 2 -> 2 rows, next_after_id = Some(last.id).
        let r1 = execute_command(
            handle.clone(),
            Command::ExportHistory {
                contact: alice,
                after_id: None,
                limit: 2,
            },
        )
        .await
        .unwrap();
        let (recs1, next1) = match r1 {
            CommandResult::ExportPage {
                records,
                next_after_id,
            } => (records, next_after_id),
            other => panic!("expected ExportPage, got {other:?}"),
        };
        assert_eq!(recs1.len(), 2);
        assert!(next1.is_some());

        // Page 2: after_id = next1, limit 2 -> 2 rows, next_after_id = Some(last.id).
        let r2 = execute_command(
            handle.clone(),
            Command::ExportHistory {
                contact: alice,
                after_id: next1,
                limit: 2,
            },
        )
        .await
        .unwrap();
        let (recs2, next2) = match r2 {
            CommandResult::ExportPage {
                records,
                next_after_id,
            } => (records, next_after_id),
            other => panic!("expected ExportPage, got {other:?}"),
        };
        assert_eq!(recs2.len(), 2);
        assert!(next2.is_some());

        // Page 3: after_id = next2, limit 2 -> 1 row (short page), next_after_id = None.
        let r3 = execute_command(
            handle.clone(),
            Command::ExportHistory {
                contact: alice,
                after_id: next2,
                limit: 2,
            },
        )
        .await
        .unwrap();
        let (recs3, next3) = match r3 {
            CommandResult::ExportPage {
                records,
                next_after_id,
            } => (records, next_after_id),
            other => panic!("expected ExportPage, got {other:?}"),
        };
        assert_eq!(recs3.len(), 1);
        assert!(next3.is_none(), "short page -> caller stops");
    }

    #[tokio::test]
    async fn export_history_unknown_contact_returns_contact_not_found() {
        let handle = test_handle();
        let unknown = crate::identity::PublicKey([0xEE; 32]);
        let err = execute_command(
            handle,
            Command::ExportHistory {
                contact: unknown,
                after_id: None,
                limit: 10,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            IpcError::Daemon(crate::daemon::error_kind::DaemonErrorKind::ContactNotFound)
        ));
    }

    #[tokio::test]
    async fn search_messages_unscoped_resolves_outgoing_contact_via_group() {
        use crate::daemon::commands::Direction;
        let handle = test_handle();
        let my_pubkey = handle.identity.public();
        let peer = crate::identity::PublicKey([0x55; 32]);
        let gid = [0x66u8; 32];

        let cr = ContactRepo::new(&handle.pool);
        cr.upsert(&crate::contact::Contact {
            identity: peer,
            display_name: None,
            added_at: 0,
            card: None,
            muted: false,
        })
        .unwrap();
        cr.set_group_id(&peer, &gid).unwrap();

        // Insert one OUTGOING row: sender == local pubkey.
        let msgs = crate::storage::MessageRepo::new(&handle.pool);
        let env = crate::envelope::Envelope {
            v: 1,
            id: crate::envelope::MessageId::generate(),
            ts: 1_700_000_000,
            reply_to: None,
            kind: crate::envelope::Kind::Text {
                body: "outbound hello".into(),
            },
        };
        msgs.insert(crate::storage::messages::InsertParams {
            group_id: &gid,
            sender: &my_pubkey.0,
            envelope: &env,
            mls_generation: 1,
            ts_daemon_recv: 1_700_000_000,
        })
        .unwrap();

        // Unscoped search that matches the outgoing row.
        let result = execute_command(
            handle.clone(),
            Command::SearchMessages {
                query: "hello".into(),
                contact: None,
                limit: 10,
                offset: 0,
                newest_first: false,
            },
        )
        .await
        .unwrap();

        match result {
            CommandResult::SearchResults(hits) => {
                assert_eq!(hits.len(), 1);
                assert_eq!(hits[0].record.direction, Direction::Outgoing);
                assert_eq!(
                    hits[0].record.contact, peer,
                    "unscoped outgoing hit must resolve contact to peer, not self"
                );
            }
            other => panic!("expected SearchResults, got {other:?}"),
        }
    }

    // ── Task 21: Command::AddMailbox ──────────────────────────────────────

    /// Stub `MailboxConnectFactory` whose `connect` always errors with
    /// `Unreachable`. Drives the AddMailbox-validation failure path.
    struct UnreachableFactory;

    #[async_trait::async_trait]
    impl crate::mailbox::poll::MailboxConnectFactory for UnreachableFactory {
        async fn connect(
            &self,
            _onion: &str,
        ) -> crate::error::Result<
            crate::mailbox::client::MailboxClient<Box<dyn crate::mailbox::poll::MailboxStream>>,
        > {
            Err(crate::error::CoreError::MailboxClient(
                crate::error::MailboxClientErrorKind::Unreachable,
            ))
        }
    }

    /// Stub factory that hands out one in-process duplex peer per
    /// `connect` call. The test seeds it with one stream whose remote
    /// end is wired to a tiny `Challenge → ChallengeNonce` server, so a
    /// single AddMailbox probe succeeds.
    struct OneShotChallengeFactory {
        slots: std::sync::Mutex<Vec<tokio::io::DuplexStream>>,
    }

    #[async_trait::async_trait]
    impl crate::mailbox::poll::MailboxConnectFactory for OneShotChallengeFactory {
        async fn connect(
            &self,
            onion: &str,
        ) -> crate::error::Result<
            crate::mailbox::client::MailboxClient<Box<dyn crate::mailbox::poll::MailboxStream>>,
        > {
            let s_opt = self.slots.lock().unwrap().pop();
            match s_opt {
                Some(s) => {
                    let boxed: Box<dyn crate::mailbox::poll::MailboxStream> = Box::new(s);
                    Ok(crate::mailbox::client::MailboxClient::from_stream(
                        onion.to_string(),
                        boxed,
                    ))
                }
                None => Err(crate::error::CoreError::MailboxClient(
                    crate::error::MailboxClientErrorKind::Unreachable,
                )),
            }
        }
    }

    /// Build a `DaemonHandle` with a stub mailbox factory and a fresh
    /// `(poller_ctrl_tx, poller_ctrl_rx)` so the test can observe
    /// `PollerCtrl::AddMailbox(id)` messages emitted by the handler.
    fn test_handle_with_mailbox(
        factory: Arc<dyn crate::mailbox::poll::MailboxConnectFactory>,
    ) -> (
        Arc<DaemonHandle<tokio::io::DuplexStream>>,
        tokio::sync::mpsc::Receiver<crate::mailbox::poll::PollerCtrl>,
    ) {
        let seed = Seed::generate().unwrap();
        let identity = IdentityKey::from_seed(&seed).unwrap();
        let pool = Arc::new(Pool::in_memory());
        let hub: Arc<DeliveryHub<tokio::io::DuplexStream>> =
            Arc::new(DeliveryHub::new(pool.clone()));
        let (events_tx, _) = broadcast::channel::<Event>(16);
        let (ctrl_tx, ctrl_rx) = tokio::sync::mpsc::channel::<crate::mailbox::poll::PollerCtrl>(16);
        let handle =
            DaemonHandle::new_with_mailbox(pool, hub, identity, events_tx, factory, ctrl_tx);
        (Arc::new(handle), ctrl_rx)
    }

    #[tokio::test]
    async fn add_mailbox_unreachable_returns_invalid_argument() {
        let (handle, _ctrl_rx) = test_handle_with_mailbox(Arc::new(UnreachableFactory));
        handle.set_onion("self.onion".to_string());
        let res = execute_command(
            handle,
            Command::AddMailbox {
                onion: "dead.onion".into(),
            },
        )
        .await;
        assert!(
            matches!(
                res,
                Err(IpcError::Daemon(
                    crate::daemon::error_kind::DaemonErrorKind::InvalidArgument { ref message }
                )) if message == "unreachable"
            ),
            "expected InvalidArgument(unreachable), got {res:?}"
        );
    }

    #[tokio::test]
    async fn add_mailbox_without_factory_returns_invalid_argument() {
        // The base `test_handle()` wires `mailbox_factory: None`. The
        // handler must reject with InvalidArgument so callers see a
        // typed error rather than panicking on `as_ref().unwrap()`.
        let handle = test_handle();
        handle.set_onion("self.onion".to_string());
        let res = execute_command(
            handle,
            Command::AddMailbox {
                onion: "any.onion".into(),
            },
        )
        .await;
        assert!(
            matches!(
                res,
                Err(IpcError::Daemon(
                    crate::daemon::error_kind::DaemonErrorKind::InvalidArgument { .. }
                ))
            ),
            "expected InvalidArgument, got {res:?}"
        );
    }

    #[tokio::test]
    async fn add_mailbox_reachable_inserts_row_and_notifies_scheduler() {
        use crate::mailbox::codec::{MailboxFrame, MailboxFrameCodec};
        use crate::mailbox::protocol::ChallengeNonce;
        use futures::{SinkExt, StreamExt};
        use tokio_util::codec::Framed;

        // Server peer answers a single Challenge with a fixed nonce.
        let (a, b) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let mut framed = Framed::new(b, MailboxFrameCodec::new());
            if let Some(Ok(MailboxFrame::Challenge(_))) = framed.next().await {
                let _ = framed
                    .send(MailboxFrame::ChallengeNonce(ChallengeNonce {
                        nonce: [0xA1; 32],
                        issued_at: 1,
                    }))
                    .await;
            }
        });

        let factory = Arc::new(OneShotChallengeFactory {
            slots: std::sync::Mutex::new(vec![a]),
        });
        let (handle, mut ctrl_rx) = test_handle_with_mailbox(factory);
        handle.set_onion("self.onion".to_string());

        let res = execute_command(
            handle.clone(),
            Command::AddMailbox {
                onion: "live.onion".into(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(res, CommandResult::Ok));

        // Row persisted as Reachable.
        use crate::storage::{MailboxRepo, MailboxStatus};
        let rows = MailboxRepo::new(&handle.pool).list_mine().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].onion, "live.onion");
        assert_eq!(rows[0].status, MailboxStatus::Reachable);

        // Scheduler received a `PollerCtrl::AddMailbox(id)` for the
        // exact row id.
        let ctrl_msg = tokio::time::timeout(std::time::Duration::from_secs(1), ctrl_rx.recv())
            .await
            .expect("ctrl message arrives within 1 s")
            .expect("ctrl channel still open");
        match ctrl_msg {
            crate::mailbox::poll::PollerCtrl::AddMailbox(id) => assert_eq!(id, rows[0].id),
            other => panic!("expected AddMailbox, got {other:?}"),
        }

        // The probe consumed the slot; the server task completes.
        let _ = server.await;
    }

    // ── Task 22: Command::RemoveMailbox ───────────────────────────────────

    #[tokio::test]
    async fn remove_mailbox_unknown_id_returns_invalid_argument() {
        // Base handle (no mailbox factory); the row lookup must fail first.
        let (handle, _ctrl_rx) = test_handle_with_mailbox(Arc::new(UnreachableFactory));
        handle.set_onion("self.onion".to_string());
        let res = execute_command(handle, Command::RemoveMailbox { id: 999 }).await;
        assert!(
            matches!(
                res,
                Err(IpcError::Daemon(
                    crate::daemon::error_kind::DaemonErrorKind::InvalidArgument { ref message }
                )) if message == "not_found"
            ),
            "expected InvalidArgument(not_found), got {res:?}"
        );
    }

    #[tokio::test]
    async fn remove_mailbox_marks_pending_then_removed_and_notifies_scheduler() {
        use crate::mailbox::codec::{MailboxFrame, MailboxFrameCodec};
        use crate::mailbox::protocol::ChallengeNonce;
        use crate::storage::{MailboxRepo, MailboxStatus};
        use futures::{SinkExt, StreamExt};
        use tokio_util::codec::Framed;

        // Server peer for the drain: answers Challenge → ChallengeNonce,
        // then FetchRequest → FetchResponse(empty), then DeleteRequest → Ok.
        // For simplicity, use an unreachable factory for the drain so we
        // exercise the "proceed despite drain failure" path and keep the
        // test hermetic (no full mailbox server FSM needed here).
        let factory = Arc::new(UnreachableFactory);
        let (handle, mut ctrl_rx) = test_handle_with_mailbox(factory);
        handle.set_onion("self.onion".to_string());

        // Pre-insert a 'mine' mailbox row directly via MailboxRepo::add_mine.
        let repo = MailboxRepo::new(&handle.pool);
        let mb_id = repo.add_mine("target.onion", 1_000).unwrap();
        repo.mark_status(mb_id, MailboxStatus::Reachable).unwrap();

        // Subscribe to events before driving the command.
        let mut events_rx = handle.events_tx.subscribe();

        let res = execute_command(handle.clone(), Command::RemoveMailbox { id: mb_id })
            .await
            .unwrap();
        assert!(matches!(res, CommandResult::Ok));

        // Row must be Removed after the handler completes.
        let row = MailboxRepo::new(&handle.pool).get(mb_id).unwrap().unwrap();
        assert_eq!(row.status, MailboxStatus::Removed, "status must be Removed");

        // Scheduler must have received RemoveMailbox(id).
        let ctrl_msg = tokio::time::timeout(std::time::Duration::from_secs(1), ctrl_rx.recv())
            .await
            .expect("ctrl message arrives within 1 s")
            .expect("ctrl channel still open");
        match ctrl_msg {
            crate::mailbox::poll::PollerCtrl::RemoveMailbox(id) => {
                assert_eq!(id, mb_id)
            }
            other => panic!("expected RemoveMailbox, got {other:?}"),
        }

        // Two MailboxStatusChanged events must have been emitted:
        // 1. PendingRemoval  2. Removed
        let ev1 = tokio::time::timeout(std::time::Duration::from_secs(1), events_rx.recv())
            .await
            .expect("event 1 arrives")
            .expect("event channel open");
        assert!(
            matches!(
                ev1,
                crate::daemon::events::Event::MailboxStatusChanged {
                    mailbox_id,
                    status: MailboxStatus::PendingRemoval
                } if mailbox_id == mb_id
            ),
            "expected PendingRemoval event, got {ev1:?}"
        );
        let ev2 = tokio::time::timeout(std::time::Duration::from_secs(1), events_rx.recv())
            .await
            .expect("event 2 arrives")
            .expect("event channel open");
        assert!(
            matches!(
                ev2,
                crate::daemon::events::Event::MailboxStatusChanged {
                    mailbox_id,
                    status: MailboxStatus::Removed
                } if mailbox_id == mb_id
            ),
            "expected Removed event, got {ev2:?}"
        );
    }

    // ── Task 24: Command::ListMailboxes ───────────────────────────────────

    #[tokio::test]
    async fn list_mailboxes_returns_real_rows_after_add() {
        use crate::daemon::commands::MailboxSummary;
        use crate::storage::{MailboxRepo, MailboxStatus};

        let (handle, _ctrl_rx) = test_handle_with_mailbox(Arc::new(UnreachableFactory));
        handle.set_onion("self.onion".to_string());

        // Pre-insert two 'mine' mailbox rows.
        let repo = MailboxRepo::new(&handle.pool);
        let id1 = repo.add_mine("alpha.onion", 1_000).unwrap();
        repo.mark_status(id1, MailboxStatus::Reachable).unwrap();
        let id2 = repo.add_mine("beta.onion", 2_000).unwrap();
        repo.mark_status(id2, MailboxStatus::Unreachable).unwrap();

        let res = execute_command(handle, Command::ListMailboxes)
            .await
            .unwrap();
        let summaries: Vec<MailboxSummary> = match res {
            CommandResult::Mailboxes(s) => s,
            other => panic!("expected Mailboxes, got {other:?}"),
        };

        assert_eq!(summaries.len(), 2, "two rows expected");
        // list_mine returns in registered_at, id order.
        assert_eq!(summaries[0].id, id1);
        assert_eq!(summaries[0].onion, "alpha.onion");
        assert_eq!(summaries[0].status, MailboxStatus::Reachable);
        assert_eq!(summaries[0].registered_at, 1_000);

        assert_eq!(summaries[1].id, id2);
        assert_eq!(summaries[1].onion, "beta.onion");
        assert_eq!(summaries[1].status, MailboxStatus::Unreachable);
        assert_eq!(summaries[1].registered_at, 2_000);
    }

    // ── Task 23: Command::RotateOnion ─────────────────────────────────────

    /// Read the current `self_card_state.version` directly from the pool.
    fn read_self_card_version(pool: &crate::storage::Pool) -> u64 {
        pool.with(|c| {
            c.query_row(
                "SELECT version FROM self_card_state WHERE id = 1",
                rusqlite::params![],
                |r| r.get::<_, i64>(0),
            )
            .map_err(|e| crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string())))
        })
        .map(|v| u64::try_from(v).unwrap_or(0))
        .unwrap_or(0)
    }

    #[tokio::test]
    async fn rotate_onion_publishes_card_update_to_contacts() {
        // `test_handle_with_mailbox` builds a handle whose pool has all
        // migrations (including 0009 self_card_state) and wires up a
        // `poller_ctrl` channel required by `publish_self_card_update`.
        let (handle, _ctrl_rx) = test_handle_with_mailbox(Arc::new(UnreachableFactory));
        // Tor must appear "ready" so `publish_self_card_update` can read the onion.
        handle.set_onion("rotate-test.onion".to_string());

        let version_before = read_self_card_version(&handle.pool);

        let res = execute_command(handle.clone(), Command::RotateOnion)
            .await
            .unwrap();
        assert!(matches!(res, CommandResult::Ok), "expected Ok, got {res:?}");

        // The self-card version counter must have advanced by exactly 1.
        let version_after = read_self_card_version(&handle.pool);
        assert_eq!(
            version_after,
            version_before + 1,
            "self_card_state.version must be bumped by RotateOnion"
        );
    }

    #[tokio::test]
    async fn rotate_onion_without_published_onion_fails() {
        // Base handle with no `set_onion` call → onion() returns None.
        let (handle, _ctrl_rx) = test_handle_with_mailbox(Arc::new(UnreachableFactory));
        // Deliberately do NOT call handle.set_onion(…).

        let res = execute_command(handle, Command::RotateOnion).await;
        assert!(
            matches!(
                res,
                Err(IpcError::Daemon(
                    crate::daemon::error_kind::DaemonErrorKind::TorNotReady
                ))
            ),
            "expected TorNotReady when onion is not set, got {res:?}"
        );
    }

    #[tokio::test]
    async fn daemon_info_returns_pubkey_onion_version_schema() {
        let h = test_handle();
        h.set_onion("example.onion".to_string());
        let result = execute_command(h.clone(), Command::DaemonInfo).await;
        match result.unwrap() {
            CommandResult::DaemonInfo {
                local_pubkey,
                current_onion,
                daemon_version,
                schema_version,
            } => {
                assert_eq!(local_pubkey, h.identity.public());
                assert_eq!(current_onion.as_deref(), Some("example.onion"));
                assert_eq!(daemon_version, env!("CARGO_PKG_VERSION"));
                assert!(schema_version >= 9);
            }
            other => panic!("expected DaemonInfo, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn daemon_info_returns_none_onion_when_not_yet_published() {
        let h = test_handle();
        let result = execute_command(h, Command::DaemonInfo).await;
        match result.unwrap() {
            CommandResult::DaemonInfo { current_onion, .. } => {
                assert!(current_onion.is_none());
            }
            other => panic!("expected DaemonInfo, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_contacts_populates_new_projection_fields() {
        use crate::envelope::{Envelope, Kind, MessageId};

        let h = test_handle();
        let repo = crate::storage::ContactRepo::new(&h.pool);
        let pk_a = crate::identity::PublicKey([0xAA; 32]);
        let pk_b = crate::identity::PublicKey([0xBB; 32]);
        let group_a = vec![1u8; 32];
        let group_b = vec![2u8; 32];
        repo.upsert(&crate::contact::Contact {
            identity: pk_a,
            display_name: Some("alice".into()),
            added_at: 100,
            card: None,
            muted: false,
        })
        .unwrap();
        repo.upsert(&crate::contact::Contact {
            identity: pk_b,
            display_name: Some("bob".into()),
            added_at: 200,
            card: None,
            muted: false,
        })
        .unwrap();
        repo.set_group_id(&pk_a, &group_a).unwrap();
        repo.set_group_id(&pk_b, &group_b).unwrap();

        let env = Envelope {
            v: 1,
            id: MessageId([0x01; 16]),
            ts: 1_700_000_000,
            reply_to: None,
            kind: Kind::Text {
                body: "yo this is a preview".into(),
            },
        };
        crate::storage::MessageRepo::new(&h.pool)
            .insert(crate::storage::messages::InsertParams {
                group_id: &group_a,
                sender: &pk_a.0,
                envelope: &env,
                mls_generation: 0,
                ts_daemon_recv: 1_700_000_500,
            })
            .unwrap();

        let result = execute_command(h, Command::ListContacts).await.unwrap();
        let summaries = match result {
            CommandResult::Contacts(v) => v,
            other => panic!("expected Contacts, got {other:?}"),
        };
        assert_eq!(summaries.len(), 2);
        // alice has the recent message and should sort first.
        assert_eq!(summaries[0].pubkey, pk_a);
        assert_eq!(
            summaries[0].last_message_preview.as_deref(),
            Some("yo this is a preview"),
        );
        assert_eq!(summaries[0].last_ts_recv, Some(1_700_000_500));
        assert_eq!(summaries[1].pubkey, pk_b);
        assert!(summaries[1].last_message_preview.is_none());
        assert!(summaries[1].last_ts_recv.is_none());
    }

    #[tokio::test]
    async fn list_contacts_truncates_preview_to_80_codepoints() {
        use crate::envelope::{Envelope, Kind, MessageId};

        let h = test_handle();
        let pk = crate::identity::PublicKey([0xCC; 32]);
        let group = vec![3u8; 32];
        crate::storage::ContactRepo::new(&h.pool)
            .upsert(&crate::contact::Contact {
                identity: pk,
                display_name: None,
                added_at: 0,
                card: None,
                muted: false,
            })
            .unwrap();
        crate::storage::ContactRepo::new(&h.pool)
            .set_group_id(&pk, &group)
            .unwrap();

        let body = "x".repeat(200);
        let env = Envelope {
            v: 1,
            id: MessageId([0x02; 16]),
            ts: 0,
            reply_to: None,
            kind: Kind::Text { body: body.clone() },
        };
        crate::storage::MessageRepo::new(&h.pool)
            .insert(crate::storage::messages::InsertParams {
                group_id: &group,
                sender: &pk.0,
                envelope: &env,
                mls_generation: 0,
                ts_daemon_recv: 100,
            })
            .unwrap();

        let r = execute_command(h, Command::ListContacts).await.unwrap();
        let summaries = match r {
            CommandResult::Contacts(v) => v,
            other => panic!("{other:?}"),
        };
        let preview = summaries[0].last_message_preview.as_ref().unwrap();
        assert_eq!(preview.chars().count(), 80);
        assert!(preview.chars().all(|c| c == 'x'));
    }

    // ── Task 6: paged recent_messages branch ─────────────────────────────

    #[tokio::test]
    async fn recent_messages_unpaged_returns_messages_tuple_variant() {
        let handle = test_handle();
        let (peer_pk, _gid) = seed_contact_with_group(&handle, "peer1", 5).await;

        let result = execute_command(
            handle.clone(),
            Command::RecentMessages {
                contact: Some(peer_pk),
                limit: 10,
                before_id: None,
                paged: false,
            },
        )
        .await
        .unwrap();

        match result {
            CommandResult::Messages(rows) => assert_eq!(rows.len(), 5),
            other => panic!("expected Messages(Vec), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_messages_paged_first_page_carries_cursor() {
        let handle = test_handle();
        let (peer_pk, _gid) = seed_contact_with_group(&handle, "peer2", 60).await;

        let result = execute_command(
            handle.clone(),
            Command::RecentMessages {
                contact: Some(peer_pk),
                limit: 50,
                before_id: None,
                paged: true,
            },
        )
        .await
        .unwrap();

        match result {
            CommandResult::MessagesPage {
                records,
                next_before_id,
            } => {
                assert_eq!(records.len(), 50);
                assert!(next_before_id.is_some());
                let min_id = records
                    .iter()
                    .map(|r| r.row_id)
                    .min()
                    .expect("non-empty page");
                assert_eq!(
                    next_before_id,
                    Some(min_id),
                    "next_before_id must be the smallest row_id in the DESC-ordered page"
                );
            }
            other => panic!("expected MessagesPage, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_messages_paged_last_page_has_null_cursor() {
        let handle = test_handle();
        let (peer_pk, _gid) = seed_contact_with_group(&handle, "peer3", 30).await;

        let result = execute_command(
            handle.clone(),
            Command::RecentMessages {
                contact: Some(peer_pk),
                limit: 50,
                before_id: None,
                paged: true,
            },
        )
        .await
        .unwrap();

        match result {
            CommandResult::MessagesPage {
                records,
                next_before_id: None,
            } => {
                assert_eq!(records.len(), 30);
            }
            other => panic!("expected MessagesPage with null cursor, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn recent_messages_paged_with_before_id_excludes_cursor_row() {
        let handle = test_handle();
        let (peer_pk, _gid) = seed_contact_with_group(&handle, "peer4", 30).await;

        let first = execute_command(
            handle.clone(),
            Command::RecentMessages {
                contact: Some(peer_pk),
                limit: 10,
                before_id: None,
                paged: true,
            },
        )
        .await
        .unwrap();
        let cursor = match first {
            CommandResult::MessagesPage {
                next_before_id: Some(c),
                ..
            } => c,
            other => panic!("expected MessagesPage cursor, got {other:?}"),
        };

        let second = execute_command(
            handle.clone(),
            Command::RecentMessages {
                contact: Some(peer_pk),
                limit: 10,
                before_id: Some(cursor),
                paged: true,
            },
        )
        .await
        .unwrap();
        match second {
            CommandResult::MessagesPage { records, .. } => {
                assert!(records.iter().all(|r| r.row_id < cursor));
            }
            other => panic!("expected MessagesPage, got {other:?}"),
        }
    }

    /// Creates a real two-party MLS group via the Alice/Bob invite dance.
    ///
    /// `handle` plays Bob's role: it adds Alice's invite via `AddContact`
    /// and returns Alice's pubkey + her group_id (as seen in Bob's pool).
    /// After this call, `SendMessage { contact: peer_pk, .. }` will succeed
    /// because the MLS state, contact row, and group linkage are all in place.
    ///
    /// The returned `group_id` is the raw 32-byte group_id used in Bob's
    /// storage — useful for direct SQL assertions in tests.
    async fn seed_contact_with_real_group(
        handle: &Arc<DaemonHandle<tokio::io::DuplexStream>>,
    ) -> (crate::identity::PublicKey, Vec<u8>) {
        use crate::storage::ContactRepo;

        // Alice: separate handle, must have an onion so CreateInvite succeeds.
        let handle_a = test_handle();
        handle_a.set_onion("alice-seed-real.onion".to_string());

        let CommandResult::InviteCreated { url, .. } = execute_command(
            handle_a.clone(),
            Command::CreateInvite {
                nickname: None,
                ttl_secs: Some(3600),
            },
        )
        .await
        .unwrap() else {
            panic!("seed_contact_with_real_group: expected InviteCreated");
        };

        // Bob (handle) consumes Alice's invite; this creates a real MLS group.
        let CommandResult::ContactAdded(summary) =
            execute_command(handle.clone(), Command::AddContact { invite_url: url })
                .await
                .unwrap()
        else {
            panic!("seed_contact_with_real_group: expected ContactAdded");
        };

        let peer_pk = summary.pubkey;
        let contact_repo = ContactRepo::new(&handle.pool);
        let group_id = contact_repo
            .get_group_id(&peer_pk)
            .unwrap()
            .expect("group_id present after AddContact");

        (peer_pk, group_id)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_message_returns_record_with_row_id() {
        use crate::daemon::commands::Direction;
        use crate::envelope::Kind;

        let handle = test_handle_with_dialer();
        let (peer_pk, _gid) = seed_contact_with_real_group(&handle).await;

        let result = execute_command(
            handle.clone(),
            Command::SendMessage {
                contact: peer_pk,
                kind: Kind::Text {
                    body: "hello".into(),
                },
            },
        )
        .await
        .unwrap();

        match result {
            CommandResult::MessageSent {
                record: Some(rec),
                status: _,
                ..
            } => {
                assert!(rec.row_id > 0, "record.row_id must be set");
                assert_eq!(rec.direction, Direction::Outgoing);
                assert_eq!(rec.contact, peer_pk);
                match &rec.kind {
                    Kind::Text { body } => assert_eq!(body, "hello"),
                    other => panic!("expected Kind::Text, got {other:?}"),
                }
                assert!(
                    rec.mls_generation > 0,
                    "post-encrypt mls_generation must advance"
                );
                assert!(rec.ts_daemon_recv > 0);
            }
            other => panic!("expected MessageSent with Some(record), got {other:?}"),
        }
    }

    /// Seeds a contact + a placeholder group_id + N text messages.
    /// Returns the peer pubkey + the group_id bytes.
    ///
    /// This is for tests that exercise the read path only — there is
    /// no MLS Group state, no Welcome processing. Use one of the
    /// existing real-group helpers (e.g. `seed_contact_with_real_group`)
    /// when the test needs to encrypt.
    async fn seed_contact_with_group(
        handle: &Arc<DaemonHandle<tokio::io::DuplexStream>>,
        nickname: &str,
        n_messages: usize,
    ) -> (crate::identity::PublicKey, Vec<u8>) {
        use crate::envelope::{Envelope, Kind, MessageId};
        use crate::storage::MessageRepo;

        // Per-call distinct peer pubkey + group_id so calls within a
        // single test handle don't collide. Fold nickname bytes into the
        // first byte for stable-but-distinct values.
        let nickname_byte = nickname
            .as_bytes()
            .iter()
            .fold(0u8, |acc, &b| acc.wrapping_add(b));
        let mut peer_bytes = [0xABu8; 32];
        peer_bytes[0] = nickname_byte;
        let peer_pk = crate::identity::PublicKey(peer_bytes);
        let mut gid = vec![0xCDu8; 32];
        gid[0] = nickname_byte;

        // Seed the contact row then link it to the group_id.
        let contact_repo = ContactRepo::new(&handle.pool);
        contact_repo
            .upsert(&crate::contact::Contact {
                identity: peer_pk,
                display_name: Some(nickname.into()),
                added_at: 0,
                card: None,
                muted: false,
            })
            .unwrap();
        contact_repo.set_group_id(&peer_pk, &gid).unwrap();

        // Insert N text messages with distinct envelope ids + timestamps.
        let msg_repo = MessageRepo::new(&handle.pool);
        for i in 0..n_messages {
            let env = Envelope {
                v: 1,
                id: MessageId([i as u8; 16]),
                ts: 1_700_000_000 + i as i64,
                reply_to: None,
                kind: Kind::Text {
                    body: format!("m{i}"),
                },
            };
            msg_repo
                .insert(crate::storage::messages::InsertParams {
                    group_id: &gid,
                    sender: &peer_pk.0,
                    envelope: &env,
                    mls_generation: 0,
                    ts_daemon_recv: env.ts,
                })
                .unwrap();
        }
        (peer_pk, gid)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn list_contacts_carries_group_state_and_read_cursor() {
        use crate::daemon::commands::MlsGroupStateLabel;

        let handle = test_handle_with_dialer();
        let (peer_pk, _gid) = seed_contact_with_real_group(&handle).await;

        // Send a real message so a row exists, then mark up to its row_id as read.
        let send_result = execute_command(
            handle.clone(),
            Command::SendMessage {
                contact: peer_pk,
                kind: crate::envelope::Kind::Text { body: "x".into() },
            },
        )
        .await
        .unwrap();
        let row_id = match send_result {
            CommandResult::MessageSent {
                record: Some(rec), ..
            } => rec.row_id,
            other => panic!("expected MessageSent with record, got {other:?}"),
        };
        execute_command(
            handle.clone(),
            Command::MarkRead {
                contact: peer_pk,
                up_to_message_id: row_id,
            },
        )
        .await
        .unwrap();

        let result = execute_command(handle.clone(), Command::ListContacts)
            .await
            .unwrap();
        let summary = match result {
            CommandResult::Contacts(s) => s
                .into_iter()
                .find(|c| c.pubkey == peer_pk)
                .expect("seeded peer not found"),
            other => panic!("expected Contacts, got {other:?}"),
        };

        assert_eq!(summary.group_state, Some(MlsGroupStateLabel::Active));
        assert_eq!(summary.last_read_row_id, Some(row_id));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn list_contacts_reports_corrupt_for_unloadable_group_blob() {
        use crate::daemon::commands::MlsGroupStateLabel;
        use crate::storage::MlsGroupRepo;

        let handle = test_handle();
        // seed_contact_with_group provides a placeholder group_id without real MLS state.
        // We write a garbage blob to that group_id to trigger a load failure.
        let (peer_pk, gid) = seed_contact_with_group(&handle, "broken", 0).await;

        // Write a garbage blob to force load failure.
        MlsGroupRepo::new(&handle.pool)
            .put(&gid, b"\xFF\xFF\xFFnot a valid mls blob", 0)
            .unwrap();

        let result = execute_command(handle.clone(), Command::ListContacts)
            .await
            .unwrap();
        let summary = match result {
            CommandResult::Contacts(s) => s.into_iter().find(|c| c.pubkey == peer_pk).unwrap(),
            other => panic!("expected Contacts, got {other:?}"),
        };
        assert_eq!(summary.group_state, Some(MlsGroupStateLabel::Corrupt));
    }

    // ── Task 8: rename_contact dispatcher ───────────────────────────────────

    #[tokio::test]
    async fn rename_contact_validates_nickname() {
        use crate::daemon::error_kind::DaemonErrorKind;
        let handle = test_handle();
        let peer = PublicKey([0x77; 32]);

        // Pre-create the contact row so set_display_name finds something to update.
        let repo = crate::storage::ContactRepo::new(&handle.pool);
        repo.upsert(&crate::contact::Contact {
            identity: peer,
            display_name: None,
            added_at: 0,
            card: None,
            muted: false,
        })
        .unwrap();

        // empty after trim
        let err = execute_command(
            handle.clone(),
            Command::RenameContact {
                contact: peer,
                nickname: Some("   ".into()),
            },
        )
        .await
        .expect_err("empty after trim must reject");
        assert!(matches!(
            err,
            IpcError::Daemon(DaemonErrorKind::InvalidArgument { .. })
        ));

        // > 64 chars
        let too_long = "x".repeat(65);
        let err = execute_command(
            handle.clone(),
            Command::RenameContact {
                contact: peer,
                nickname: Some(too_long),
            },
        )
        .await
        .expect_err("> 64 chars must reject");
        assert!(matches!(
            err,
            IpcError::Daemon(DaemonErrorKind::InvalidArgument { .. })
        ));

        // happy path
        let ok = execute_command(
            handle.clone(),
            Command::RenameContact {
                contact: peer,
                nickname: Some("Alice".into()),
            },
        )
        .await
        .unwrap();
        assert!(matches!(ok, CommandResult::Ok));

        // verify persisted
        let stored = repo.get(&peer).unwrap().unwrap();
        assert_eq!(stored.display_name.as_deref(), Some("Alice"));
    }

    #[tokio::test]
    async fn rename_contact_emits_contact_updated_event() {
        let handle = test_handle();
        let peer = PublicKey([0x88; 32]);
        crate::storage::ContactRepo::new(&handle.pool)
            .upsert(&crate::contact::Contact {
                identity: peer,
                display_name: None,
                added_at: 0,
                card: None,
                muted: false,
            })
            .unwrap();

        let mut rx = handle.events_tx.subscribe();
        let _ = execute_command(
            handle.clone(),
            Command::RenameContact {
                contact: peer,
                nickname: Some("Bob".into()),
            },
        )
        .await
        .unwrap();

        match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await {
            Ok(Ok(crate::daemon::events::Event::ContactUpdated(p))) => assert_eq!(p, peer),
            other => panic!("expected ContactUpdated, got {other:?}"),
        }
    }

    // ── Task 9: remove_contact dispatcher ────────────────────────────────────

    #[tokio::test]
    async fn remove_contact_is_idempotent() {
        let handle = test_handle();
        let peer = PublicKey([0x91; 32]);
        crate::storage::ContactRepo::new(&handle.pool)
            .upsert(&crate::contact::Contact {
                identity: peer,
                display_name: Some("Bob".into()),
                added_at: 0,
                card: None,
                muted: false,
            })
            .unwrap();

        let r1 = execute_command(handle.clone(), Command::RemoveContact { contact: peer })
            .await
            .unwrap();
        let r2 = execute_command(handle.clone(), Command::RemoveContact { contact: peer })
            .await
            .unwrap();
        assert!(matches!(r1, CommandResult::Ok));
        assert!(matches!(r2, CommandResult::Ok));

        // Default ListContacts filters them out.
        let listed = execute_command(handle.clone(), Command::ListContacts)
            .await
            .unwrap();
        match listed {
            CommandResult::Contacts(v) => assert!(v.is_empty()),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn remove_contact_preserves_mls_group_state() {
        let handle = test_handle();
        use crate::mls::key_package::KeyPackage;
        use crate::mls::provider::MlsProvider;
        let bob_id =
            crate::identity::IdentityKey::from_seed(&crate::identity::Seed::generate().unwrap())
                .unwrap();
        let bob_provider = MlsProvider::new();
        let kp_repo = crate::storage::KeyPackageRepo::new(&handle.pool);
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo).unwrap();
        let mut group =
            crate::mls::Group::create_solo(&handle.identity, None, None, MlsProvider::new())
                .unwrap();
        let _ = group.add_member(&bob_kp, None, None).unwrap();
        let group_repo = crate::storage::MlsGroupRepo::new(&handle.pool);
        group.save(&group_repo).unwrap();
        let gid = group.id().0.clone();
        let blob_before: Vec<u8> = handle
            .pool
            .with(|c| {
                c.query_row(
                    "SELECT state_blob FROM mls_groups WHERE group_id = ?1",
                    rusqlite::params![&gid[..]],
                    |r| r.get::<_, Vec<u8>>(0),
                )
                .map_err(|e| {
                    crate::error::CoreError::Storage(crate::storage::StorageErrorKind::Other(
                        e.to_string(),
                    ))
                })
            })
            .unwrap();

        let bob_pk = bob_id.public();
        let repo = crate::storage::ContactRepo::new(&handle.pool);
        repo.upsert(&crate::contact::Contact {
            identity: bob_pk,
            display_name: None,
            added_at: 0,
            card: None,
            muted: false,
        })
        .unwrap();
        repo.set_group_id(&bob_pk, &gid).unwrap();

        let _ = execute_command(handle.clone(), Command::RemoveContact { contact: bob_pk })
            .await
            .unwrap();

        let blob_after: Vec<u8> = handle
            .pool
            .with(|c| {
                c.query_row(
                    "SELECT state_blob FROM mls_groups WHERE group_id = ?1",
                    rusqlite::params![&gid[..]],
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
            "RemoveContact must not touch MLS state"
        );
    }

    // ── Task 13: set_contact_muted dispatcher ──────────────────────────────────

    #[tokio::test]
    async fn set_contact_muted_toggles_and_emits_event() {
        let handle = test_handle();
        let pk = PublicKey([0xAA; 32]);
        crate::storage::ContactRepo::new(&handle.pool)
            .upsert(&crate::contact::Contact {
                identity: pk,
                display_name: None,
                added_at: 0,
                card: None,
                muted: false,
            })
            .unwrap();

        let mut sub = handle.events_tx.subscribe();
        let result = execute_command(
            handle.clone(),
            Command::SetContactMuted {
                contact: pk,
                muted: true,
            },
        )
        .await
        .unwrap();
        assert!(matches!(result, CommandResult::Ok));

        // Event should be emitted
        match tokio::time::timeout(std::time::Duration::from_secs(1), sub.recv()).await {
            Ok(Ok(Event::ContactUpdated(p))) => assert_eq!(p, pk),
            other => panic!("expected ContactUpdated, got {other:?}"),
        }

        // Persisted in storage
        let repo = crate::storage::ContactRepo::new(&handle.pool);
        assert!(repo.is_muted(&pk).unwrap());

        // Toggle back
        let mut sub = handle.events_tx.subscribe();
        let _ = execute_command(
            handle.clone(),
            Command::SetContactMuted {
                contact: pk,
                muted: false,
            },
        )
        .await
        .unwrap();

        // Event should be emitted again
        match tokio::time::timeout(std::time::Duration::from_secs(1), sub.recv()).await {
            Ok(Ok(Event::ContactUpdated(p))) => assert_eq!(p, pk),
            other => panic!("expected ContactUpdated, got {other:?}"),
        }

        // Persisted
        assert!(!repo.is_muted(&pk).unwrap());
    }

    #[tokio::test]
    async fn set_contact_muted_returns_contact_not_found() {
        let handle = test_handle();
        let pk = PublicKey([0xFF; 32]);
        let err = execute_command(
            handle,
            Command::SetContactMuted {
                contact: pk,
                muted: true,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            IpcError::Daemon(crate::daemon::error_kind::DaemonErrorKind::ContactNotFound)
        ));
    }

    // ── Task 10: list_contacts_with_filter ────────────────────────────────────

    #[tokio::test]
    async fn list_contacts_with_filter_includes_hidden_when_opted_in() {
        let handle = test_handle();
        let visible = PublicKey([0xA1; 32]);
        let archived = PublicKey([0xA2; 32]);
        let repo = crate::storage::ContactRepo::new(&handle.pool);
        repo.upsert(&crate::contact::Contact {
            identity: visible,
            display_name: Some("Visible".into()),
            added_at: 0,
            card: None,
            muted: false,
        })
        .unwrap();
        repo.upsert(&crate::contact::Contact {
            identity: archived,
            display_name: Some("Archived".into()),
            added_at: 0,
            card: None,
            muted: false,
        })
        .unwrap();
        repo.set_hidden(&archived, true).unwrap();

        // Default: only visible.
        let r = execute_command(handle.clone(), Command::ListContacts)
            .await
            .unwrap();
        match r {
            CommandResult::Contacts(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].pubkey, visible);
            }
            other => panic!("unexpected: {other:?}"),
        }

        // include_hidden = true: both.
        let r = execute_command(
            handle.clone(),
            Command::ListContactsWithFilter {
                include_hidden: true,
            },
        )
        .await
        .unwrap();
        match r {
            CommandResult::Contacts(v) => assert_eq!(v.len(), 2),
            other => panic!("unexpected: {other:?}"),
        }
    }

    // ── Task 12: GetConfig / SetConfig ────────────────────────────────────────

    fn test_handle_with_config(
        tmp: &tempfile::TempDir,
    ) -> Arc<DaemonHandle<tokio::io::DuplexStream>> {
        let seed = Seed::generate().unwrap();
        let identity = IdentityKey::from_seed(&seed).unwrap();
        let pool = Arc::new(Pool::in_memory());
        let hub: Arc<DeliveryHub<tokio::io::DuplexStream>> =
            Arc::new(DeliveryHub::new(pool.clone()));
        let (events_tx, _) = broadcast::channel::<Event>(16);
        let mut config = crate::daemon::config::Config::defaults()
            .unwrap_or_else(|_| crate::daemon::config::Config::fallback_for_tests());
        config.history.retention_days = 0;
        let config_path = tmp.path().join("config.toml");
        Arc::new(DaemonHandle::new_with_config(
            pool,
            hub,
            identity,
            events_tx,
            config,
            config_path,
        ))
    }

    #[tokio::test]
    async fn get_config_returns_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let handle = test_handle_with_config(&tmp);
        let result = execute_command(handle.clone(), Command::GetConfig)
            .await
            .unwrap();
        match result {
            CommandResult::Config(snap) => {
                assert_eq!(snap.history_retention_days, 0);
                assert_eq!(snap.direct_timeout_secs, 30);
            }
            other => panic!("expected Config, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_config_persists_and_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let handle = test_handle_with_config(&tmp);
        let patch = crate::daemon::commands::ConfigPatch {
            history_retention_days: Some(7),
            ..Default::default()
        };
        execute_command(handle.clone(), Command::SetConfig { patch })
            .await
            .unwrap();
        let result = execute_command(handle, Command::GetConfig).await.unwrap();
        match result {
            CommandResult::Config(snap) => assert_eq!(snap.history_retention_days, 7),
            other => panic!("expected Config, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_config_invalid_direct_timeout_returns_daemon_error() {
        let tmp = tempfile::tempdir().unwrap();
        let handle = test_handle_with_config(&tmp);
        let patch = crate::daemon::commands::ConfigPatch {
            direct_timeout_secs: Some(0), // out of range 1..=600
            ..Default::default()
        };
        let result = execute_command(handle, Command::SetConfig { patch }).await;
        assert!(
            matches!(
                result,
                Err(IpcError::Daemon(
                    crate::daemon::error_kind::DaemonErrorKind::InvalidArgument { .. }
                ))
            ),
            "expected InvalidArgument, got {result:?}"
        );
    }

    // ── Task 22: TailLogs handler ─────────────────────────────────────────────

    #[tokio::test]
    async fn tail_logs_returns_recent_records() {
        let handle = test_handle();
        // Push records directly via the sink (simulates tracing layer output).
        for i in 0..5_u32 {
            handle.log_sink.push(
                crate::daemon::commands::LogLevel::Info,
                "test".into(),
                format!("m-{i}"),
            );
        }
        let result = execute_command(
            handle,
            Command::TailLogs {
                since_seq: None,
                limit: 100,
            },
        )
        .await
        .unwrap();
        match result {
            CommandResult::Logs {
                records,
                next_since_seq,
            } => {
                assert_eq!(
                    records.len(),
                    5,
                    "expected 5 records, got {}",
                    records.len()
                );
                assert!(
                    records.iter().any(|r| r.message.contains("m-4")),
                    "last record not found"
                );
                assert!(
                    next_since_seq > 0,
                    "next_since_seq should be non-zero cursor"
                );
            }
            other => panic!("expected CommandResult::Logs, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tail_logs_since_seq_cursor_works() {
        let handle = test_handle();
        for i in 0..10_u32 {
            handle.log_sink.push(
                crate::daemon::commands::LogLevel::Info,
                "cursor-test".into(),
                format!("msg-{i}"),
            );
        }
        // First page: no cursor.
        let result = execute_command(
            handle.clone(),
            Command::TailLogs {
                since_seq: None,
                limit: 5,
            },
        )
        .await
        .unwrap();
        let cursor = match result {
            CommandResult::Logs {
                records,
                next_since_seq,
            } => {
                assert_eq!(records.len(), 5);
                next_since_seq
            }
            other => panic!("unexpected {other:?}"),
        };

        // Second page: use cursor, should return the remaining 5.
        let result2 = execute_command(
            handle,
            Command::TailLogs {
                since_seq: Some(cursor),
                limit: 100,
            },
        )
        .await
        .unwrap();
        match result2 {
            CommandResult::Logs { records, .. } => {
                assert_eq!(records.len(), 5);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn send_message_with_contact_card_update_kind_is_rejected_not_panic() {
        use crate::contact::card::{ContactCard, ContactCardBody};
        use crate::daemon::error_kind::DaemonErrorKind;
        use crate::envelope::Kind;
        use crate::identity::Signature;

        let handle = test_handle();
        let peer = PublicKey([0x42; 32]);

        // Seed a contact + group so resolution gets past ContactNotFound and
        // reaches the kind path. (group_id can be any 32 bytes for this test;
        // the kind check must fire before MLS load.)
        {
            let repo = ContactRepo::new(&handle.pool);
            repo.upsert(&Contact {
                identity: peer,
                display_name: None,
                added_at: 0,
                card: None,
                muted: false,
            })
            .unwrap();
            repo.set_group_id(&peer, &[0x11u8; 32]).unwrap();
        }

        let card = ContactCard {
            body: ContactCardBody {
                identity: peer,
                onion: "x.onion".into(),
                mailboxes: vec![],
                version: 1,
                expires_at: 9_999_999_999,
            },
            signature: Signature([0u8; 64]),
        };
        let err = execute_command(
            handle,
            Command::SendMessage {
                contact: peer,
                kind: Kind::ContactCardUpdate {
                    card: Box::new(card),
                },
            },
        )
        .await
        .unwrap_err();

        assert!(
            matches!(
                err,
                IpcError::Daemon(DaemonErrorKind::InvalidArgument { .. })
            ),
            "ContactCardUpdate must be rejected as InvalidArgument, got {err:?}"
        );
    }

    // ── Task 5 (T1-3): per-group ratchet-serialization lock ────────────────────

    /// Concurrency guardrail for T1-3. Builds one real 2-member MLS group
    /// (Bob = the sender handle's identity, Alice the joiner), fires N
    /// concurrent `send_message` for Alice on a multi-thread runtime, then
    /// decrypts ALL N collected ciphertexts on Alice's sibling `Group`.
    ///
    /// With the per-group lock in place every concurrent send loads the
    /// previous on-disk ratchet snapshot and encrypts at a DISTINCT generation,
    /// so all N decrypt. Without the lock, two sends would load the same
    /// snapshot and encrypt at the same generation → at least one ciphertext
    /// fails to decrypt (the race this lock closes). The test is therefore
    /// RED without the lock and GREEN with it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_sends_on_one_group_all_decrypt() {
        use crate::envelope::Kind;
        use crate::mls::group::Group;
        use crate::mls::key_package::KeyPackage;
        use crate::mls::provider::MlsProvider;
        use crate::storage::{ContactRepo, MlsGroupRepo};
        use tokio::task::JoinSet;

        const N: usize = 8;

        // Bob's handle (sender). Dialer wired so `send_message`'s hub.send
        // resolves (it returns Queued — no real peer — which is fine here).
        let handle = test_handle_with_dialer();

        // Alice (the joiner) — a fresh identity + KeyPackage. Her provider must
        // be reused at join time so OpenMLS can find the init private key.
        let alice_id = IdentityKey::from_seed(&Seed::generate().unwrap()).unwrap();
        let alice_pk = alice_id.public();
        let alice_provider = MlsProvider::new();
        let kp_repo = crate::storage::KeyPackageRepo::new(&handle.pool);
        let alice_kp = KeyPackage::generate(&alice_id, &alice_provider, &kp_repo).unwrap();

        // Bob builds the solo group with the SENDER handle's identity (so
        // `send_message`'s encrypt — which loads from handle.pool — uses it),
        // then adds Alice. Save Bob's genesis group + link the contact.
        let mut bob_group =
            Group::create_solo(&handle.identity, None, None, MlsProvider::new()).unwrap();
        let (welcome, _commit) = bob_group.add_member(&alice_kp, None, None).unwrap();
        let group_repo = MlsGroupRepo::new(&handle.pool);
        bob_group.save(&group_repo).unwrap();
        let gid = bob_group.id().0.clone();

        let contact_repo = ContactRepo::new(&handle.pool);
        contact_repo
            .upsert(&crate::contact::Contact {
                identity: alice_pk,
                display_name: None,
                added_at: 0,
                card: None,
                muted: false,
            })
            .unwrap();
        contact_repo.set_group_id(&alice_pk, &gid).unwrap();

        // Alice joins from the Welcome — her decrypting sibling group.
        let mut alice_group =
            Group::join_from_welcome(&alice_id, &welcome, None, None, alice_provider).unwrap();

        // Fire N concurrent sends on the SAME group.
        let mut set: JoinSet<()> = JoinSet::new();
        for i in 0..N {
            let h = handle.clone();
            set.spawn(async move {
                let res = execute_command(
                    h,
                    Command::SendMessage {
                        contact: alice_pk,
                        kind: Kind::Text {
                            body: format!("concurrent-{i}"),
                        },
                    },
                )
                .await;
                assert!(res.is_ok(), "concurrent send {i} failed: {res:?}");
            });
        }
        while let Some(joined) = set.join_next().await {
            joined.expect("send task panicked");
        }

        // Collect every ciphertext queued for Alice from the outbox.
        let payloads: Vec<Vec<u8>> = handle
            .pool
            .with(|c| {
                let mut stmt = c
                    .prepare("SELECT payload FROM outbox WHERE target = ?1 ORDER BY id")
                    .map_err(|e| {
                        crate::error::CoreError::Storage(crate::storage::StorageErrorKind::Other(
                            e.to_string(),
                        ))
                    })?;
                let rows = stmt
                    .query_map(rusqlite::params![&alice_pk.0[..]], |r| {
                        r.get::<_, Vec<u8>>(0)
                    })
                    .map_err(|e| {
                        crate::error::CoreError::Storage(crate::storage::StorageErrorKind::Other(
                            e.to_string(),
                        ))
                    })?;
                let mut out = Vec::new();
                for row in rows {
                    out.push(row.map_err(|e| {
                        crate::error::CoreError::Storage(crate::storage::StorageErrorKind::Other(
                            e.to_string(),
                        ))
                    })?);
                }
                Ok(out)
            })
            .unwrap();

        assert_eq!(
            payloads.len(),
            N,
            "every concurrent send must persist exactly one outbox ciphertext"
        );

        // Bob's on-disk ratchet must have advanced N generations.
        let bob_after = Group::load(&crate::mls::group::GroupId(gid.clone()), &group_repo)
            .unwrap()
            .expect("bob group still present");
        assert_eq!(
            bob_after.epoch(),
            1,
            "epoch stays at 1 (application messages, no commits)"
        );

        // ALL N ciphertexts must decrypt on Alice's side — the core T1-3
        // assertion. A generation collision (no lock) would make at least one
        // fail here.
        let mut decrypted = std::collections::HashSet::new();
        for (i, ct) in payloads.iter().enumerate() {
            let env = alice_group
                .decrypt(ct)
                .unwrap_or_else(|e| panic!("ciphertext {i} failed to decrypt: {e}"))
                .unwrap_or_else(|| panic!("ciphertext {i} was a commit, not an app message"));
            match env.kind {
                Kind::Text { body } => {
                    assert!(
                        decrypted.insert(body),
                        "duplicate plaintext — a generation was reused (race not serialized)"
                    );
                }
                other => panic!("unexpected kind: {other:?}"),
            }
        }
        assert_eq!(decrypted.len(), N, "all N distinct messages decrypted");
    }
}
