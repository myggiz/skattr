// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 1.G integration test: paginated `ExportHistory` yields 2500 rows
//! oldest-first over three pages (1000 + 1000 + 500).
//!
//! Seeds one contact + group, inserts 2500 messages, then paginates
//! with `limit=1000` through three calls and asserts:
//!
//! - the cursor terminates exactly on the short third page,
//! - ordering is oldest-first by `ts_envelope`.
//!
//! Harness pattern mirrors `cli_search.rs`: a `DaemonHandle` backed by
//! `Pool::in_memory()`, a fresh `tokio::io::duplex` pair per call (since
//! `handle_connection` is one-shot — it closes after the first
//! `Execute` + `Bye`), and an `IpcClient::from_stream` on the caller side.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use skattr_core::contact::Contact;
use skattr_core::daemon::{Command, CommandResult, IpcClient, IpcClientError};
use skattr_core::envelope::{Envelope, Kind, MessageId};
use skattr_core::identity::{IdentityKey, PublicKey, Seed};
use skattr_core::test_exports::{
    handle_connection, CommandExecutor, ContactRepo, DaemonHandle, DeliveryHub, InsertParams,
    IpcError, MessageRepo, Pool,
};
use tokio::sync::broadcast;

// ---------------------------------------------------------------------------
// Shim: `Arc<DaemonHandle<S>>` → `Arc<dyn CommandExecutor>`.
// ---------------------------------------------------------------------------

type DuplexHandle = DaemonHandle<tokio::io::DuplexStream>;

struct ExecShim(Arc<DuplexHandle>);

#[async_trait::async_trait]
impl CommandExecutor for ExecShim {
    async fn execute(&self, cmd: Command) -> std::result::Result<CommandResult, IpcError> {
        self.0.execute(cmd).await
    }
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn build_handle() -> Arc<DuplexHandle> {
    let seed = Seed::generate().unwrap();
    let identity = IdentityKey::from_seed(&seed).unwrap();
    let pool = Arc::new(Pool::in_memory());
    let hub: Arc<DeliveryHub<tokio::io::DuplexStream>> = Arc::new(DeliveryHub::new(pool.clone()));
    let (events_tx, _) = broadcast::channel(16);
    Arc::new(DaemonHandle::new(pool, hub, identity, events_tx))
}

/// Run one command against a freshly-spawned IPC server.
///
/// `handle_connection` is one-shot: the server closes the stream after
/// the first `Execute` + `Bye`, so we create a new `tokio::io::duplex`
/// pair and spawn a new server task for every call — matching how the
/// production CLI issues one `UnixStream` per command.
async fn exec_one(
    handle: &Arc<DuplexHandle>,
    cmd: Command,
) -> std::result::Result<CommandResult, IpcClientError> {
    let (client_io, server_io) = tokio::io::duplex(2 * 1024 * 1024);
    let exec: Arc<dyn CommandExecutor> = Arc::new(ExecShim(handle.clone()));
    let events_tx = handle.events_tx.clone();
    tokio::spawn(async move {
        handle_connection(server_io, exec, events_tx).await;
    });
    IpcClient::from_stream(client_io).execute(cmd).await
}

// ---------------------------------------------------------------------------
// Test.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn export_history_paginates_2500_rows_oldest_first() {
    let handle = build_handle();
    let alice = PublicKey([0x77; 32]);
    let gid = [0x44u8; 32];

    // Seed contact + group mapping.
    let cr = ContactRepo::new(&handle.pool);
    cr.upsert(&Contact {
        identity: alice,
        display_name: None,
        added_at: 0,
        card: None,
        muted: false,
    })
    .unwrap();
    cr.set_group_id(&alice, &gid).unwrap();

    // Seed 2500 messages with strictly-increasing ts_envelope.
    let msgs = MessageRepo::new(&handle.pool);
    for i in 0..2500i64 {
        let env = Envelope {
            v: 1,
            id: MessageId::generate(),
            ts: 1_700_000_000 + i,
            reply_to: None,
            kind: Kind::Text {
                body: format!("msg-{i}"),
            },
        };
        msgs.insert(InsertParams {
            group_id: &gid,
            sender: &alice.0,
            envelope: &env,
            mls_generation: u64::try_from(i).unwrap(),
            ts_daemon_recv: 1_700_000_000 + i,
        })
        .unwrap();
    }

    // Paginate through three pages: 1000 + 1000 + 500.
    let mut got: Vec<skattr_core::daemon::commands::MessageRecord> = Vec::new();
    let mut after_id: Option<i64> = None;
    let mut page_count = 0;
    loop {
        let resp = exec_one(
            &handle,
            Command::ExportHistory {
                contact: alice,
                after_id,
                limit: 1000,
            },
        )
        .await
        .unwrap();
        let (records, next) = match resp {
            CommandResult::ExportPage {
                records,
                next_after_id,
            } => (records, next_after_id),
            other => panic!("expected ExportPage, got {other:?}"),
        };
        page_count += 1;
        got.extend(records);
        if next.is_none() {
            break;
        }
        after_id = next;
    }
    assert_eq!(
        page_count, 3,
        "expected exactly 3 pages for 2500 rows at limit=1000"
    );
    assert_eq!(got.len(), 2500);

    // Oldest-first by ts_envelope (equals ts_daemon_recv in this seed).
    let envelopes: Vec<i64> = got.iter().map(|r| r.ts_envelope).collect();
    let mut sorted = envelopes.clone();
    sorted.sort();
    assert_eq!(envelopes, sorted, "ExportPage must return oldest-first");
}
