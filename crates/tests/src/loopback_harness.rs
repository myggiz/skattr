// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Shared helpers for the in-process `LoopbackTransport` guardrail tests
//! (`daemon_run_direct`, `first_contact_direct`). Extracted to avoid the
//! byte-identical duplication that previously lived in both files.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::time::Duration;

use skattr_core::daemon::commands::MlsGroupStateLabel;
use skattr_core::daemon::events::Event;
use skattr_core::daemon::ipc::wire::EventFilter;
use skattr_core::daemon::{Command, CommandResult, Config, IpcClient};
use skattr_core::envelope::Kind;
use skattr_core::identity::PublicKey;

/// Passphrase used to create + unlock the loopback daemons' identity vaults.
pub(crate) const PASSPHRASE: &str = "loopback-guardrail-passphrase-xyz";

/// Initialise a fresh identity vault at `data_dir/identity.vault`.
/// Mirrors `cli_two_daemons` / `cli_real_tor`'s vault setup exactly.
pub(crate) fn init_vault(data_dir: &Path) {
    std::fs::create_dir_all(data_dir).unwrap();
    let seed = skattr_core::identity::Seed::generate().unwrap();
    let identity = skattr_core::identity::IdentityKey::from_seed(&seed).unwrap();
    skattr_core::identity::Vault::create(&data_dir.join("identity.vault"), identity, PASSPHRASE)
        .unwrap();
}

/// Build a `Config` with a unique data dir + IPC socket path under `data_dir`.
pub(crate) fn config_for(data_dir: &Path) -> Config {
    let mut config = Config::defaults().unwrap();
    config.data_dir = data_dir.to_path_buf();
    config.ipc_socket = Some(data_dir.join("daemon.sock"));
    config
}

/// Poll `ipc_path`'s `ListContacts` until the entry for `peer` reports
/// `group_state == Active`, or panic after `timeout`.
pub(crate) async fn wait_for_group_active(ipc_path: &Path, peer: PublicKey, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let mut client = IpcClient::connect(ipc_path).await.expect("connect IPC");
        if let CommandResult::Contacts(v) = client
            .execute(Command::ListContacts)
            .await
            .expect("ListContacts")
        {
            if let Some(s) = v.into_iter().find(|s| s.pubkey == peer) {
                if s.group_state == Some(MlsGroupStateLabel::Active) {
                    return;
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("group_state for {peer:?} did not become Active within {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Open a subscription on `ipc_path` for `MessageReceived` events from
/// `sender`. Must be established **before** the message is sent so the event
/// cannot fire before we are listening (the delivery path can complete inside
/// the `SendMessage` call). The returned client is then drained by
/// [`wait_for_message`].
pub(crate) async fn subscribe_messages(
    ipc_path: &Path,
    sender: PublicKey,
) -> IpcClient<skattr_core::daemon::ipc::IpcStream> {
    let mut sub = IpcClient::connect(ipc_path)
        .await
        .expect("connect for subscribe");
    sub.subscribe(EventFilter::Messages {
        contact: Some(sender),
    })
    .await
    .expect("subscribe to Messages");
    sub
}

/// Drain a pre-established subscription until a `MessageReceived` from
/// `sender` whose body equals `expected_body` arrives, or panic after
/// `timeout`.
pub(crate) async fn wait_for_message(
    sub: &mut IpcClient<skattr_core::daemon::ipc::IpcStream>,
    sender: PublicKey,
    expected_body: &str,
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!(
                "MessageReceived(body={expected_body:?}) from {sender:?} not seen in {timeout:?}"
            );
        }
        match tokio::time::timeout(remaining, sub.next_event()).await {
            Ok(Ok(Event::MessageReceived { contact, record })) if contact == sender => {
                if let Kind::Text { body } = &record.kind {
                    if body == expected_body {
                        return;
                    }
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => panic!("subscribe stream error: {e:?}"),
            Err(_) => panic!(
                "MessageReceived(body={expected_body:?}) from {sender:?} not seen in {timeout:?}"
            ),
        }
    }
}
