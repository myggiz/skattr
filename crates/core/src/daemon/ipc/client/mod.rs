// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic))]

//! IPC client. The platform-specific `connect` impl is in the
//! active child module; the rest is generic over the stream type.

use std::io;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};

use crate::daemon::commands::{Command, CommandResult};
use crate::daemon::events::Event;
use crate::daemon::ipc::codec::{read_frame, write_frame, CodecError};
use crate::daemon::ipc::wire::{EventFilter, IpcError, IpcRequest, IpcResponse};

#[cfg(unix)]
mod unix;

#[cfg(target_os = "windows")]
mod windows;

/// Client-side error surface. Distinct from the wire-level
/// [`IpcError`] so a "socket missing" is not conflated with a typed
/// daemon error.
#[derive(Debug, Error)]
pub enum IpcClientError {
    /// Could not connect; socket file missing or connection refused.
    #[error("daemon not running")]
    DaemonNotRunning,
    /// I/O failure after a successful connect.
    #[error("ipc io: {0}")]
    Io(#[from] io::Error),
    /// Frame codec failure.
    #[error("ipc codec: {0}")]
    Codec(String),
    /// The daemon answered with a typed error.
    #[error("ipc server error: {0:?}")]
    Server(IpcError),
    /// Expected `Event` frame but received something else.
    #[error("ipc protocol: expected Event, got {0:?}")]
    UnexpectedFrame(IpcResponse),
}

impl From<CodecError> for IpcClientError {
    fn from(e: CodecError) -> Self {
        match e {
            CodecError::Io(i) => IpcClientError::Io(i),
            other => IpcClientError::Codec(other.to_string()),
        }
    }
}

/// Generic over the underlying duplex stream so unit tests can use
/// `tokio::io::duplex` instead of a real `UnixStream`.
#[derive(Debug)]
pub struct IpcClient<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    stream: BufReader<S>,
    subscribed: bool,
}

impl<S> IpcClient<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Build a client from an existing duplex stream. Used by tests
    /// (`tokio::io::duplex`) and the integration test harness.
    pub fn from_stream(stream: S) -> Self {
        Self {
            stream: BufReader::new(stream),
            subscribed: false,
        }
    }

    /// Send a one-shot `Execute` request and return the single
    /// `CommandResult`. Consumes the `Bye` frame on the way out.
    pub async fn execute(
        &mut self,
        cmd: Command,
    ) -> std::result::Result<CommandResult, IpcClientError> {
        write_frame(&mut self.stream, &IpcRequest::Execute(cmd)).await?;
        let resp = read_frame::<_, IpcResponse>(&mut self.stream).await?;
        let result = match resp {
            IpcResponse::Ok(r) => Ok(r),
            IpcResponse::Err(e) => Err(IpcClientError::Server(e)),
            other => Err(IpcClientError::UnexpectedFrame(other)),
        };
        // Drain the `Bye` best-effort; don't fail the call if the
        // server already hung up.
        let _ = read_frame::<_, IpcResponse>(&mut self.stream).await;
        result
    }

    /// Start an event subscription. After this returns Ok, call
    /// [`IpcClient::next_event`] in a loop.
    pub async fn subscribe(
        &mut self,
        filter: EventFilter,
    ) -> std::result::Result<(), IpcClientError> {
        write_frame(&mut self.stream, &IpcRequest::Subscribe(filter)).await?;
        match read_frame::<_, IpcResponse>(&mut self.stream).await? {
            IpcResponse::Ok(CommandResult::Subscribed) => {
                self.subscribed = true;
                Ok(())
            }
            IpcResponse::Err(e) => Err(IpcClientError::Server(e)),
            other => Err(IpcClientError::UnexpectedFrame(other)),
        }
    }

    /// Read the next event from the server. Only call after
    /// [`IpcClient::subscribe`]. Returns an error if the server sent
    /// anything other than `Event(..)`.
    pub async fn next_event(&mut self) -> std::result::Result<Event, IpcClientError> {
        if !self.subscribed {
            return Err(IpcClientError::UnexpectedFrame(IpcResponse::Bye));
        }
        match read_frame::<_, IpcResponse>(&mut self.stream).await? {
            IpcResponse::Event(e) => Ok(e),
            IpcResponse::Bye => Err(IpcClientError::UnexpectedFrame(IpcResponse::Bye)),
            other => Err(IpcClientError::UnexpectedFrame(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::commands::{Command, CommandResult};
    use crate::daemon::events::Event;
    use crate::daemon::ipc::server::{handle_connection, CommandExecutor};
    use crate::daemon::ipc::wire::{EventFilter, IpcError};
    use async_trait::async_trait;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    struct OkExec;
    #[async_trait]
    impl CommandExecutor for OkExec {
        async fn execute(&self, _cmd: Command) -> std::result::Result<CommandResult, IpcError> {
            Ok(CommandResult::Ok)
        }
    }

    #[tokio::test]
    async fn execute_roundtrip_over_duplex() {
        let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
        let (events_tx, _) = broadcast::channel::<Event>(16);
        let exec: Arc<dyn CommandExecutor> = Arc::new(OkExec);
        tokio::spawn(handle_connection(server_io, exec, events_tx));

        let mut client = IpcClient::from_stream(client_io);
        let got = client.execute(Command::ListContacts).await.unwrap();
        assert!(matches!(got, CommandResult::Ok));
    }

    #[tokio::test]
    async fn subscribe_streams_events() {
        let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
        let (events_tx, _) = broadcast::channel::<Event>(16);
        let exec: Arc<dyn CommandExecutor> = Arc::new(OkExec);
        let events_tx_clone = events_tx.clone();
        tokio::spawn(handle_connection(server_io, exec, events_tx_clone));

        let mut client = IpcClient::from_stream(client_io);
        client.subscribe(EventFilter::All).await.unwrap();

        let _ = events_tx.send(Event::ContactUpdated(crate::identity::PublicKey([5; 32])));
        let ev = client.next_event().await.unwrap();
        assert!(matches!(ev, Event::ContactUpdated(_)));
    }
}
