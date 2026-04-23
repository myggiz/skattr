// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! End-to-end IPC round-trip with a single daemon.
//!
//! Spins up an IPC server backed by a `DaemonHandle` over an in-memory
//! `Pool` (no Tor, no real DeliveryHub I/O), connects an `IpcClient`
//! over `tokio::io::duplex`, exercises the Command surface, and asserts
//! result shapes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use skattr_core::daemon::{Command, CommandResult, IpcClient, IpcClientError};
use skattr_core::identity::{IdentityKey, Seed};
use skattr_core::test_exports::{
    handle_connection, CommandExecutor, DaemonHandle, DeliveryHub, IpcError, Pool,
};
use tokio::sync::broadcast;

// ---------------------------------------------------------------------------
// Shim: `Arc<DaemonHandle<S>>` → `Arc<dyn CommandExecutor>`
//
// `CommandExecutor` is implemented on `DaemonHandle<S>` (takes `&self`),
// not on `Arc<DaemonHandle<S>>`, so we cannot coerce directly. A thin
// newtype wrapper delegates without any overhead.
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
// Helper: build a DaemonHandle with a fresh in-memory Pool (no Tor).
// ---------------------------------------------------------------------------

fn build_handle() -> Arc<DuplexHandle> {
    let seed = Seed::generate().unwrap();
    let identity = IdentityKey::from_seed(&seed).unwrap();
    let pool = Arc::new(Pool::in_memory());
    let hub: Arc<DeliveryHub<tokio::io::DuplexStream>> = Arc::new(DeliveryHub::new(pool.clone()));
    let (events_tx, _) = broadcast::channel(16);
    Arc::new(DaemonHandle::new(pool, hub, identity, events_tx))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// `ListContacts` on a fresh daemon returns `Contacts([])`.
#[tokio::test]
async fn ipc_list_contacts_round_trip() {
    let handle = build_handle();

    let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
    let exec: Arc<dyn CommandExecutor> = Arc::new(ExecShim(handle.clone()));
    let events_tx = handle.events_tx.clone();
    tokio::spawn(async move {
        handle_connection(server_io, exec, events_tx).await;
    });

    let mut client = IpcClient::from_stream(client_io);
    let res = client.execute(Command::ListContacts).await.unwrap();
    assert!(
        matches!(res, CommandResult::Contacts(ref rows) if rows.is_empty()),
        "expected Contacts([]), got {res:?}"
    );
}

/// `CreateGroup` is a Phase 2 command — the server returns
/// `IpcError::UnknownCommand`, which surfaces as
/// `IpcClientError::Server(IpcError::UnknownCommand)`.
#[tokio::test]
async fn ipc_unknown_command_returns_typed_error() {
    let handle = build_handle();

    let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
    let exec: Arc<dyn CommandExecutor> = Arc::new(ExecShim(handle.clone()));
    let events_tx = handle.events_tx.clone();
    tokio::spawn(async move {
        handle_connection(server_io, exec, events_tx).await;
    });

    let mut client = IpcClient::from_stream(client_io);
    let err = client
        .execute(Command::CreateGroup {
            members: vec![],
            name: "x".into(),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(err, IpcClientError::Server(IpcError::UnknownCommand)),
        "expected Server(UnknownCommand), got {err:?}"
    );
}
