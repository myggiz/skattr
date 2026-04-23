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
    // Reference all fields to silence dead_code warnings until T14+
    // call them.
    let _ = handle.pool.clone();
    let _ = handle.events_tx.clone();

    match cmd {
        Command::Shutdown | Command::RotateOnion => Ok(CommandResult::Ok),
        Command::CreateInvite { .. }
        | Command::AddContact { .. }
        | Command::ListContacts
        | Command::SendMessage { .. }
        | Command::RecentMessages { .. }
        | Command::CreateGroup { .. } => Err(IpcError::UnknownCommand),
    }
}

/// Map any `CoreError` to an `IpcError`. Projects via `CoreError::kind`
/// into `DaemonErrorKind`; otherwise `Internal(...)` with a truncated
/// display. Logs the full error server-side.
#[allow(dead_code)] // wired up by Task 14+ handlers.
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
}
