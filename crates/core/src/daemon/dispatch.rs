// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic))]

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
        Command::SendMessage { .. }
        | Command::RecentMessages { .. }
        | Command::CreateGroup { .. } => Err(IpcError::UnknownCommand),
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
    use crate::daemon::hex::Hex32;
    use crate::daemon::error_kind::DaemonErrorKind;
    use crate::invite::InviteLink;
    use crate::mls::key_package::KeyPackage;
    use crate::mls::provider::MlsProvider;
    use crate::storage::KeyPackageRepo;
    use rand_core::{OsRng, RngCore as _};

    let onion = handle.onion().ok_or(IpcError::Daemon(DaemonErrorKind::TorNotReady))?;

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
    let mut group = Group::create_solo(
        &handle.identity,
        Some(&link.psk.0),
        provider,
    )
    .map_err(map_err)?;

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

    let _ = handle.events_tx.send(Event::ContactUpdated(link.body.identity));

    Ok(CommandResult::ContactAdded(ContactSummary {
        pubkey: link.body.identity,
        nickname: None,
        onion: link.body.onion.clone(),
        card_version: 0,
        added_at: u64::try_from(now).unwrap_or(0),
    }))
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
            Command::CreateGroup { members: vec![], name: "x".into() },
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
            Command::CreateInvite { nickname: None, ttl_secs: Some(3600) },
        )
        .await
        .unwrap();

        let (url, kpi, expires_at) = match result {
            CommandResult::InviteCreated { url, key_package_id, expires_at } => {
                (url, key_package_id, expires_at)
            }
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
            Command::CreateInvite { nickname: None, ttl_secs: Some(3600) },
        )
        .await;
        assert!(
            matches!(
                result,
                Err(IpcError::Daemon(crate::daemon::error_kind::DaemonErrorKind::TorNotReady))
            ),
            "expected TorNotReady, got {result:?}"
        );
    }

    #[tokio::test]
    async fn add_contact_from_self_invite_persists_group_link_and_emits_event() {
        let handle_a = test_handle();
        handle_a.set_onion("alice.onion".to_string());

        // Alice creates an invite.
        let CommandResult::InviteCreated { url, .. } =
            execute_command(
                handle_a.clone(),
                Command::CreateInvite { nickname: None, ttl_secs: Some(3600) },
            )
            .await
            .unwrap()
        else {
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

        let CommandResult::InviteCreated { url, .. } =
            execute_command(
                handle_a.clone(),
                Command::CreateInvite { nickname: None, ttl_secs: Some(3600) },
            )
            .await
            .unwrap()
        else {
            panic!("expected InviteCreated");
        };

        let handle_b = test_handle();
        // First use succeeds.
        execute_command(handle_b.clone(), Command::AddContact { invite_url: url.clone() })
            .await
            .unwrap();
        // Second use is rejected.
        let res = execute_command(handle_b.clone(), Command::AddContact { invite_url: url }).await;
        assert!(
            matches!(
                res,
                Err(IpcError::Daemon(crate::daemon::error_kind::DaemonErrorKind::InviteConsumed))
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

        let result = execute_command(handle, Command::ListContacts).await.unwrap();
        match result {
            CommandResult::Contacts(summaries) => {
                assert_eq!(summaries.len(), 2);
                let names: Vec<Option<String>> = summaries.iter().map(|s| s.nickname.clone()).collect();
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
}
