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

/// A NON-subscribed connection is one-shot: the server closes it after a single
/// `Execute` (`ipc/server/mod.rs`: `is_terminal = subscribed.is_none() …`), so a
/// second `Execute` on the same connection fails. This is the contract behind
/// the CLI reconnect fix — `send`/`send_file`/`remove`/`tail`/`search`/`export`/
/// `prune`/`chat` resolve a contact via `ListContacts` (Execute #1) then act
/// (Execute #2), so they MUST reconnect between the two or the second Execute
/// broken-pipes (Linux `os 32` / Windows `os 232`). Regression guard for #116.
#[tokio::test]
async fn non_subscribed_connection_is_one_shot_second_execute_fails() {
    let handle = build_handle();

    let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
    let exec: Arc<dyn CommandExecutor> = Arc::new(ExecShim(handle.clone()));
    let events_tx = handle.events_tx.clone();
    tokio::spawn(async move {
        handle_connection(server_io, exec, events_tx).await;
    });

    let mut client = IpcClient::from_stream(client_io);
    // First Execute succeeds; the server then writes Bye and closes the conn.
    let first = client.execute(Command::ListContacts).await.unwrap();
    assert!(matches!(first, CommandResult::Contacts(_)));
    // Second Execute on the SAME connection must fail (the socket is closed) —
    // reproducing the exact bug that broke every resolve-then-act CLI command.
    let second = client.execute(Command::ListContacts).await;
    assert!(
        second.is_err(),
        "non-subscribed connection must be one-shot: the 2nd Execute must fail, got {second:?}"
    );
}

/// The reconnect fix's positive path: two SEPARATE connections each do one
/// Execute successfully — the pattern every resolve-then-act CLI command now
/// uses (resolve on one connection, act on a fresh one).
#[tokio::test]
async fn two_connections_each_execute_succeed() {
    let handle = build_handle();

    for _ in 0..2 {
        let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
        let exec: Arc<dyn CommandExecutor> = Arc::new(ExecShim(handle.clone()));
        let events_tx = handle.events_tx.clone();
        tokio::spawn(async move {
            handle_connection(server_io, exec, events_tx).await;
        });
        let mut client = IpcClient::from_stream(client_io);
        let res = client.execute(Command::ListContacts).await.unwrap();
        assert!(matches!(res, CommandResult::Contacts(_)));
    }
}
