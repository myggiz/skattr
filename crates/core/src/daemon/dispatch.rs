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
        Command::Shutdown | Command::RotateOnion => Ok(CommandResult::Ok),
        Command::ListContacts => list_contacts(&handle).await,
        Command::CreateInvite { nickname, ttl_secs } => {
            create_invite(&handle, nickname, ttl_secs).await
        }
        Command::AddContact { invite_url } => add_contact(&handle, invite_url).await,
        Command::SendMessage { contact, kind } => send_message(&handle, contact, kind).await,
        Command::RecentMessages { contact, limit } => {
            recent_messages(&handle, contact, limit).await
        }
        Command::CreateGroup { .. } => Err(IpcError::UnknownCommand),
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
    }
}

async fn list_contacts<S>(
    handle: &Arc<DaemonHandle<S>>,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::commands::ContactSummary;
    use crate::storage::ContactRepo;

    let repo = ContactRepo::new(&handle.pool);
    let contacts = repo.list().map_err(map_err)?;
    let summaries: Vec<ContactSummary> = contacts
        .into_iter()
        .map(|c| {
            let (onion, card_version) = c
                .card
                .as_ref()
                .map(|card| (card.body.onion.clone(), card.body.version))
                .unwrap_or_else(|| (String::new(), 0));
            ContactSummary {
                pubkey: c.identity,
                nickname: c.display_name,
                onion,
                card_version,
                added_at: u64::try_from(c.added_at).unwrap_or(0),
            }
        })
        .collect();
    Ok(CommandResult::Contacts(summaries))
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
    use crate::mls::key_package::KeyPackage;
    use crate::mls::provider::MlsProvider;
    use crate::storage::KeyPackageRepo;
    use rand_core::{OsRng, RngCore as _};

    let onion = handle
        .onion()
        .ok_or(IpcError::Daemon(DaemonErrorKind::TorNotReady))?;

    let ttl = ttl_secs.unwrap_or(24 * 3600);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .map_err(|e| map_err(CoreError::Config(format!("clock: {e}"))))?;

    // Generate a fresh MLS KeyPackage. `generate` internally stores the
    // KP bytes in `KeyPackageRepo` with direction="ours", consumed=false.
    let provider = MlsProvider::new();
    let kp_repo = KeyPackageRepo::new(&handle.pool);
    let kp = KeyPackage::generate(&handle.identity, &provider, &kp_repo).map_err(map_err)?;
    let kp_hash = kp.hash().map_err(map_err)?;
    let kp_bytes = kp.to_bytes().map_err(map_err)?;

    // 32-byte one-time PSK.
    let mut psk = [0u8; 32];
    OsRng.fill_bytes(&mut psk);

    let link =
        InviteLink::generate(&handle.identity, onion, kp_bytes, psk, ttl, now).map_err(map_err)?;
    let url = link.to_url().map_err(map_err)?;
    let expires_at = u64::try_from(now + ttl as i64).unwrap_or(0);

    Ok(CommandResult::InviteCreated {
        url,
        key_package_id: Hex32::from(kp_hash),
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

    // Reject if this invite's KP was already consumed (double-use guard).
    if link.is_consumed(&kp_repo).map_err(map_err)? {
        return Err(IpcError::Daemon(DaemonErrorKind::InviteConsumed));
    }

    // Build our solo MLS group, then add the inviter as the second member.
    let provider = MlsProvider::new();
    let mut group =
        Group::create_solo(&handle.identity, Some(&link.psk.0), provider).map_err(map_err)?;

    let invitee_kp = KeyPackage::from_bytes(&link.body.key_package).map_err(map_err)?;
    let (_welcome, _commit) = group
        .add_member(&invitee_kp, Some(&link.psk.0))
        .map_err(map_err)?;
    let group_id = group.id().0.clone();

    // Persist group state.
    let group_repo = MlsGroupRepo::new(&handle.pool);
    group.save(&group_repo).map_err(map_err)?;

    // Persist contact row + group_id link.
    let contact_repo = ContactRepo::new(&handle.pool);
    let contact = Contact {
        identity: link.body.identity,
        display_name: None,
        added_at: now,
        card: None,
    };
    contact_repo.upsert(&contact).map_err(map_err)?;
    contact_repo
        .set_group_id(&link.body.identity, &group_id)
        .map_err(map_err)?;

    // Mark the inviter's single-use KP as consumed.
    link.mark_consumed(&kp_repo).map_err(map_err)?;

    let _ = handle
        .events_tx
        .send(Event::ContactUpdated(link.body.identity));

    Ok(CommandResult::ContactAdded(ContactSummary {
        pubkey: link.body.identity,
        nickname: None,
        onion: link.body.onion.clone(),
        card_version: 0,
        added_at: u64::try_from(now).unwrap_or(0),
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

    // 1. Resolve group_id from contact.
    let contact_repo = ContactRepo::new(&handle.pool);
    let group_id_bytes = match contact_repo.get_group_id(&contact).map_err(map_err)? {
        Some(bytes) if !bytes.is_empty() => bytes,
        _ => return Err(IpcError::Daemon(DaemonErrorKind::ContactNotFound)),
    };

    // 2. Load MLS group (provider is embedded in the blob).
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

    let insert_result: crate::error::Result<()> = handle.pool.transaction(|tx| {
        group.save_in_tx(&group_repo, tx)?;
        let _ = msg_repo.insert_in_tx(
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
        Ok(())
    });

    match insert_result {
        Ok(()) => { /* continue to hub.send below */ }
        Err(CoreError::Storage(StorageErrorKind::DuplicateMessage)) => {
            // Idempotent retry: this envelope_id was already persisted by a
            // prior attempt. Treat as Delivered — the outbox row from the
            // earlier attempt still owns delivery.
            return Ok(CommandResult::MessageSent {
                message_id: Hex16::from(message_id.0),
                status: SendStatus::Delivered,
            });
        }
        Err(e) => return Err(map_err(e)),
    }

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

    Ok(CommandResult::MessageSent {
        message_id: Hex16::from(message_id.0),
        status,
    })
}

async fn recent_messages<S>(
    handle: &Arc<DaemonHandle<S>>,
    contact: Option<crate::identity::PublicKey>,
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

    // Phase 1.F requires a contact filter. Global recent lands in Phase 1.G.
    let peer = contact.ok_or(IpcError::Daemon(DaemonErrorKind::ContactNotFound))?;

    let contact_repo = ContactRepo::new(&handle.pool);
    let group_id = match contact_repo.get_group_id(&peer).map_err(map_err)? {
        Some(bytes) if !bytes.is_empty() => bytes,
        _ => return Err(IpcError::Daemon(DaemonErrorKind::ContactNotFound)),
    };

    let msg_repo = MessageRepo::new(&handle.pool);
    let rows = msg_repo
        .recent(&group_id, usize::try_from(limit).unwrap_or(usize::MAX))
        .map_err(map_err)?;

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

    Ok(CommandResult::Messages(records))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    use crate::daemon::commands::{Command, CommandResult};
    use crate::daemon::events::Event;
    use crate::daemon::handle::DaemonHandle;
    use crate::daemon::ipc::wire::IpcError;
    use crate::delivery::hub::DeliveryHub;
    use crate::identity::{IdentityKey, Seed};
    use crate::storage::Pool;

    fn test_handle() -> Arc<DaemonHandle<tokio::io::DuplexStream>> {
        let seed = Seed::generate().unwrap();
        let identity = IdentityKey::from_seed(&seed).unwrap();
        let pool = Arc::new(Pool::in_memory());
        let hub: Arc<DeliveryHub<tokio::io::DuplexStream>> =
            Arc::new(DeliveryHub::new(pool.clone()));
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
        assert_eq!(parsed.body.onion, "testonion".repeat(8));

        // The KeyPackage is recorded in storage (single-use tracking).
        use crate::storage::KeyPackageRepo;
        let kp_repo = KeyPackageRepo::new(&handle.pool);
        assert!(kp_repo.get(&kpi.0).unwrap().is_some());
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
        let handle_b = test_handle();
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

        let handle_b = test_handle();
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
            })
            .unwrap();
            repo.upsert(&Contact {
                identity: PublicKey([0x02; 32]),
                display_name: None,
                added_at: 1_700_000_100,
                card: None,
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
        })
        .unwrap();
        cr.set_group_id(&peer, &gid).unwrap();

        let res = execute_command(
            handle,
            Command::RecentMessages {
                contact: Some(peer),
                limit: 10,
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
        let handle_b = test_handle();
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

        let handle_b = test_handle();
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
}
