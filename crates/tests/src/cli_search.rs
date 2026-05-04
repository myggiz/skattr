// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 1.G integration test: full IPC round-trip for `SearchMessages`.
//!
//! Spawns one daemon over an in-memory duplex pipe (no Tor), seeds two
//! contacts each with a group and mixed text, then asserts:
//!
//! - cross-group search returns every matching row,
//! - `contact: Some(pk)` restricts results to that peer's group,
//! - a whitespace-only query short-circuits to an empty result set.
//!
//! Harness pattern mirrors `cli_ipc_roundtrip.rs`: a `DaemonHandle`
//! backed by `Pool::in_memory()`, a `tokio::io::duplex` pipe, a shim
//! that adapts `Arc<DaemonHandle<_>>` to `CommandExecutor`, and an
//! `IpcClient::from_stream` on the caller side.

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

fn seed_contact(handle: &DuplexHandle, pubkey: PublicKey, group_id: &[u8]) {
    let cr = ContactRepo::new(&handle.pool);
    cr.upsert(&Contact {
        identity: pubkey,
        display_name: None,
        added_at: 0,
        card: None,
        muted: false,
    })
    .unwrap();
    cr.set_group_id(&pubkey, group_id).unwrap();
}

fn to_env(body: &str) -> Envelope {
    Envelope {
        v: 1,
        id: MessageId::generate(),
        ts: 1_700_000_000,
        reply_to: None,
        kind: Kind::Text { body: body.into() },
    }
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
    let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
    let exec: Arc<dyn CommandExecutor> = Arc::new(ExecShim(handle.clone()));
    let events_tx = handle.events_tx.clone();
    tokio::spawn(async move {
        handle_connection(server_io, exec, events_tx).await;
    });
    IpcClient::from_stream(client_io).execute(cmd).await
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_messages_round_trips_through_ipc() {
    let handle = build_handle();
    let alice = PublicKey([0x77; 32]);
    let bob = PublicKey([0x88; 32]);
    let alice_gid = [0x11u8; 32];
    let bob_gid = [0x22u8; 32];
    seed_contact(&handle, alice, &alice_gid);
    seed_contact(&handle, bob, &bob_gid);

    let msgs = MessageRepo::new(&handle.pool);
    for body in ["alpha bravo", "bravo charlie", "delta echo"] {
        msgs.insert(InsertParams {
            group_id: &alice_gid,
            sender: &alice.0,
            envelope: &to_env(body),
            mls_generation: 1,
            ts_daemon_recv: 1_700_000_000,
        })
        .unwrap();
    }
    for body in ["alpha foxtrot", "bravo golf"] {
        msgs.insert(InsertParams {
            group_id: &bob_gid,
            sender: &bob.0,
            envelope: &to_env(body),
            mls_generation: 1,
            ts_daemon_recv: 1_700_000_000,
        })
        .unwrap();
    }

    // 1. Cross-group: "bravo" matches 3 rows (alice:2 + bob:1).
    let r = exec_one(
        &handle,
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
    match r {
        CommandResult::SearchResults(hits) => assert_eq!(
            hits.len(),
            3,
            "unscoped 'bravo' should return 3 hits across both groups"
        ),
        other => panic!("expected SearchResults, got {other:?}"),
    }

    // 2. Contact-scoped: "bravo" restricted to Alice's group → 2 rows.
    let r = exec_one(
        &handle,
        Command::SearchMessages {
            query: "bravo".into(),
            contact: Some(alice),
            limit: 10,
            offset: 0,
            newest_first: false,
        },
    )
    .await
    .unwrap();
    match r {
        CommandResult::SearchResults(hits) => assert_eq!(
            hits.len(),
            2,
            "alice-scoped 'bravo' should return 2 hits (alpha+charlie rows)"
        ),
        other => panic!("expected SearchResults, got {other:?}"),
    }

    // 3. Whitespace-only query short-circuits to empty — must never
    //    reach FTS5 (which would error on an empty MATCH argument).
    let r = exec_one(
        &handle,
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
    match r {
        CommandResult::SearchResults(hits) => {
            assert!(hits.is_empty(), "whitespace-only query must be empty");
        }
        other => panic!("expected SearchResults, got {other:?}"),
    }
}
