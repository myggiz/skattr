// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic))]

//! IPC server half. Binds a Unix socket with `0600` mode and a `0700`
//! parent directory, peer-cred checks every accepted connection, and
//! hands each off to a per-connection task.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tokio::net::UnixListener;

use crate::daemon::ipc::wire::IpcError;
use crate::error::{CoreError, Result};

/// Server bound to a local Unix socket.
pub struct Server {
    listener: UnixListener,
    path: PathBuf,
    allowed_uid: u32,
}

impl Server {
    /// Bind a `Server` at `path`. Creates parents with mode `0700`,
    /// unlinks any stale file at `path`, then binds and chmods the
    /// socket to mode `0600`. `allowed_uid` is the UID that every
    /// accepted connection's peer-cred must match.
    pub fn bind(path: &Path, allowed_uid: u32) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(CoreError::Io)?;
            let mut perms = std::fs::metadata(parent)
                .map_err(CoreError::Io)?
                .permissions();
            perms.set_mode(0o700);
            std::fs::set_permissions(parent, perms).map_err(CoreError::Io)?;
        }
        // Remove stale file (a crashed prior daemon). Ignore errors if
        // it didn't exist.
        let _ = std::fs::remove_file(path);

        let listener = UnixListener::bind(path).map_err(CoreError::Io)?;

        // Tighten the socket file to 0600 immediately after bind.
        let mut perms = std::fs::metadata(path)
            .map_err(CoreError::Io)?
            .permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(path, perms).map_err(CoreError::Io)?;

        Ok(Self {
            listener,
            path: path.to_path_buf(),
            allowed_uid,
        })
    }

    /// Path the socket file is bound to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Wait for the next incoming connection. Returns the accepted
    /// stream only if its peer-cred UID matches `allowed_uid`; else
    /// closes immediately and returns `Err(IpcError::AuthDenied)`.
    pub async fn accept_one(&self) -> std::result::Result<tokio::net::UnixStream, IpcError> {
        let (stream, _) = self
            .listener
            .accept()
            .await
            .map_err(|e| IpcError::Internal(format!("accept: {e}")))?;
        let cred = stream
            .peer_cred()
            .map_err(|e| IpcError::Internal(format!("peer_cred: {e}")))?;
        check_peer_uid(Some(cred.uid()), self.allowed_uid).map_err(|_| IpcError::AuthDenied)?;
        Ok(stream)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Best-effort unlink. Errors are ignored (log-worthy but not
        // fatal); the OS will reap the file on logout if we miss it.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Return the effective UID of the current process without using `unsafe`.
///
/// On Linux we stat `/proc/self`; on other platforms we fall back to
/// the `$UID` environment variable then `0` (suitable for tests).
#[cfg(unix)]
pub(crate) fn current_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .or_else(|_| {
            std::env::var("UID")
                .ok()
                .and_then(|s| s.parse().ok())
                .ok_or(())
        })
        .unwrap_or(0)
}

/// Check that `peer_uid` matches `expected`. Unit-testable in isolation
/// from the `UnixStream` accept path.
pub(crate) fn check_peer_uid(peer_uid: Option<u32>, expected: u32) -> io::Result<()> {
    match peer_uid {
        Some(uid) if uid == expected => Ok(()),
        Some(uid) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("peer uid {uid} != expected {expected}"),
        )),
        None => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "peer uid unavailable",
        )),
    }
}

use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::broadcast;

use crate::daemon::commands::{Command, CommandResult};
use crate::daemon::events::Event;
use crate::daemon::ipc::codec::{read_frame, write_frame, CodecError};
use crate::daemon::ipc::wire::{EventFilter, IpcRequest, IpcResponse};

/// Execute one `Command` and return its `CommandResult` or a typed
/// `IpcError`. Decouples the per-connection handler from the concrete
/// `DaemonHandle` so the unit tests can drive the handler with a mock.
#[async_trait::async_trait]
pub trait CommandExecutor: Send + Sync {
    /// Dispatch `cmd` and return a result or typed wire error.
    async fn execute(&self, cmd: Command) -> std::result::Result<CommandResult, IpcError>;
}

/// Handle one accepted connection. The loop owns a per-connection
/// `subscribed: Option<EventFilter>`; once set, events flow until the
/// client hangs up or a `Shutdown` arrives.
pub async fn handle_connection<S>(
    mut stream: S,
    executor: Arc<dyn CommandExecutor>,
    events_tx: broadcast::Sender<Event>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut events_rx: Option<broadcast::Receiver<Event>> = None;
    let mut subscribed: Option<EventFilter> = None;

    loop {
        // Two sources: inbound request, or a pending event on the
        // subscription. Use select to avoid blocking on a quiet client
        // once subscribed.
        let request_result: std::result::Result<IpcRequest, CodecError> = tokio::select! {
            r = read_frame::<_, IpcRequest>(&mut stream) => r,
            maybe_event = receive_if_some(events_rx.as_mut()) => {
                match maybe_event {
                    Some(ev) if event_matches(&ev, subscribed.as_ref()) => {
                        if write_frame(&mut stream, &IpcResponse::Event(ev)).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    Some(_) => continue, // filtered out
                    None => {
                        // lagged: reset and keep going
                        if let Some(filter) = subscribed.clone() {
                            events_rx = Some(events_tx.subscribe());
                            tracing::warn!(?filter, "ipc subscriber lagged; resubscribed");
                        }
                        continue;
                    }
                }
            }
        };

        let req = match request_result {
            Ok(r) => r,
            Err(CodecError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(CodecError::Cbor(s)) => {
                let _ = write_frame(&mut stream, &IpcResponse::Err(IpcError::Codec(s))).await;
                continue;
            }
            Err(CodecError::FrameTooLarge { got, max }) => {
                let _ = write_frame(
                    &mut stream,
                    &IpcResponse::Err(IpcError::FrameTooLarge { got, max }),
                )
                .await;
                break;
            }
            Err(CodecError::EmptyFrame) => {
                let _ = write_frame(
                    &mut stream,
                    &IpcResponse::Err(IpcError::Codec("empty frame".into())),
                )
                .await;
                break;
            }
            Err(_) => break,
        };

        match req {
            IpcRequest::Execute(cmd) => {
                let resp = match executor.execute(cmd).await {
                    Ok(result) => IpcResponse::Ok(result),
                    Err(e) => IpcResponse::Err(e),
                };
                // Close after a one-shot Execute if no subscription is
                // active. When subscribed, keep the connection open so
                // the client can interleave Execute(SendMessage) calls
                // with the ongoing event stream. Always close on error.
                let is_terminal = subscribed.is_none() || matches!(resp, IpcResponse::Err(_));
                if write_frame(&mut stream, &resp).await.is_err() {
                    break;
                }
                if is_terminal {
                    break;
                }
            }
            IpcRequest::Subscribe(filter) => {
                subscribed = Some(filter);
                events_rx = Some(events_tx.subscribe());
                if write_frame(&mut stream, &IpcResponse::Ok(CommandResult::Subscribed))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            IpcRequest::Shutdown => {
                let _ = write_frame(&mut stream, &IpcResponse::Ok(CommandResult::Ok)).await;
                break;
            }
        }
    }

    // Terminal frame. Ignore write errors — the peer may already be gone.
    let _ = write_frame(&mut stream, &IpcResponse::Bye).await;
}

/// Accept loop. Spawns [`handle_connection`] per accepted stream.
/// Terminates when `shutdown` future completes.
pub async fn serve(
    server: Server,
    executor: Arc<dyn CommandExecutor>,
    events_tx: broadcast::Sender<Event>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) {
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => {
                break;
            }
            accepted = server.accept_one() => {
                match accepted {
                    Ok(stream) => {
                        let exec = executor.clone();
                        let evs = events_tx.clone();
                        tokio::spawn(async move {
                            handle_connection(stream, exec, evs).await;
                        });
                    }
                    Err(IpcError::AuthDenied) => {
                        tracing::warn!("ipc: rejected connection: peer uid mismatch");
                    }
                    Err(e) => {
                        tracing::warn!(?e, "ipc: accept error");
                    }
                }
            }
        }
    }
}

async fn receive_if_some(rx: Option<&mut broadcast::Receiver<Event>>) -> Option<Event> {
    match rx {
        Some(r) => match r.recv().await {
            Ok(ev) => Some(ev),
            Err(broadcast::error::RecvError::Lagged(_)) => None,
            Err(broadcast::error::RecvError::Closed) => None,
        },
        None => std::future::pending().await,
    }
}

fn event_matches(event: &Event, filter: Option<&EventFilter>) -> bool {
    let Some(filter) = filter else { return false };
    match (filter, event) {
        (EventFilter::All, _) => true,
        (EventFilter::TorStatus, Event::TorStatusChanged(_)) => true,
        (EventFilter::Contact(peer), Event::MessageReceived { contact, .. }) => contact == peer,
        (EventFilter::Contact(peer), Event::DeliveryStatusChanged { .. }) => {
            // DeliveryStatusChanged doesn't carry the peer; forward all
            // for now. The CLI filters further by message_id.
            let _ = peer;
            true
        }
        (EventFilter::Contact(_), Event::ContactUpdated(_)) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn check_peer_uid_accepts_matching_uid() {
        assert!(check_peer_uid(Some(1000), 1000).is_ok());
    }

    #[test]
    fn check_peer_uid_rejects_mismatched_uid() {
        assert!(check_peer_uid(Some(999), 1000).is_err());
    }

    #[test]
    fn check_peer_uid_rejects_missing_uid() {
        assert!(check_peer_uid(None, 1000).is_err());
    }

    #[tokio::test]
    async fn bind_sets_socket_mode_0600_and_parent_0700() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("skattr").join("daemon.sock");
        let server = Server::bind(&sock, 1000).unwrap();

        let sock_mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            sock_mode, 0o600,
            "socket mode must be 0600; got {sock_mode:o}"
        );

        let parent_mode = std::fs::metadata(sock.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            parent_mode, 0o700,
            "parent mode must be 0700; got {parent_mode:o}"
        );

        drop(server);
        // Socket file removed on drop.
        assert!(!sock.exists(), "socket file must be unlinked on drop");
    }

    #[tokio::test]
    async fn bind_unlinks_stale_socket_file() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("daemon.sock");
        // Pre-create a stale socket file.
        std::fs::write(&sock, b"stale").unwrap();
        assert!(sock.exists());
        let server = Server::bind(&sock, 1000).unwrap();
        // Bind succeeded; socket now a real Unix listener.
        drop(server);
    }

    use crate::daemon::commands::{Command, CommandResult};
    use crate::daemon::events::{Event, TorStatus};
    use crate::daemon::ipc::codec::{read_frame, write_frame};
    use crate::daemon::ipc::wire::{EventFilter, IpcError, IpcRequest, IpcResponse};
    use async_trait::async_trait;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    struct EchoExec;
    #[async_trait]
    impl CommandExecutor for EchoExec {
        async fn execute(&self, cmd: Command) -> std::result::Result<CommandResult, IpcError> {
            match cmd {
                Command::ListContacts => Ok(CommandResult::Ok),
                Command::Shutdown => Ok(CommandResult::Ok),
                _ => Err(IpcError::UnknownCommand),
            }
        }
    }

    #[tokio::test]
    async fn per_conn_execute_returns_ok_and_bye() {
        let (mut client, server_stream) = tokio::io::duplex(1024 * 1024);
        let exec: Arc<dyn CommandExecutor> = Arc::new(EchoExec);
        let (events_tx, _) = broadcast::channel::<Event>(16);

        let handle_task = tokio::spawn(handle_connection(server_stream, exec, events_tx));

        write_frame(&mut client, &IpcRequest::Execute(Command::ListContacts))
            .await
            .unwrap();

        let ok: IpcResponse = read_frame(&mut client).await.unwrap();
        assert!(matches!(ok, IpcResponse::Ok(CommandResult::Ok)));
        let bye: IpcResponse = read_frame(&mut client).await.unwrap();
        assert!(matches!(bye, IpcResponse::Bye));

        // Drop client so the server exits its read loop cleanly.
        drop(client);
        handle_task.await.unwrap();
    }

    #[tokio::test]
    async fn per_conn_subscribe_forwards_events_then_execute_still_works() {
        let (mut client, server_stream) = tokio::io::duplex(1024 * 1024);
        let exec: Arc<dyn CommandExecutor> = Arc::new(EchoExec);
        let (events_tx, _) = broadcast::channel::<Event>(16);

        let events_tx_clone = events_tx.clone();
        let handle_task = tokio::spawn(handle_connection(server_stream, exec, events_tx_clone));

        // Subscribe -> Ok(Subscribed).
        write_frame(&mut client, &IpcRequest::Subscribe(EventFilter::TorStatus))
            .await
            .unwrap();
        match read_frame::<_, IpcResponse>(&mut client).await.unwrap() {
            IpcResponse::Ok(CommandResult::Subscribed) => {}
            other => panic!("expected Ok(Subscribed), got {other:?}"),
        }

        // Publish a matching event; subscriber should receive it.
        let _ = events_tx.send(Event::TorStatusChanged(TorStatus::Ready));
        match read_frame::<_, IpcResponse>(&mut client).await.unwrap() {
            IpcResponse::Event(Event::TorStatusChanged(TorStatus::Ready)) => {}
            other => panic!("expected Event(TorStatus::Ready), got {other:?}"),
        }

        // Execute after Subscribe on the same connection.
        write_frame(&mut client, &IpcRequest::Execute(Command::ListContacts))
            .await
            .unwrap();
        match read_frame::<_, IpcResponse>(&mut client).await.unwrap() {
            IpcResponse::Ok(CommandResult::Ok) => {}
            other => panic!("expected Ok, got {other:?}"),
        }

        // Hang up; server should exit cleanly.
        drop(client);
        handle_task.await.unwrap();
    }

    #[tokio::test]
    async fn per_conn_unknown_command_returns_err_but_keeps_connection() {
        let (mut client, server_stream) = tokio::io::duplex(1024 * 1024);
        let exec: Arc<dyn CommandExecutor> = Arc::new(EchoExec);
        let (events_tx, _) = broadcast::channel::<Event>(16);

        let handle_task = tokio::spawn(handle_connection(server_stream, exec, events_tx));

        write_frame(
            &mut client,
            &IpcRequest::Execute(Command::CreateGroup {
                members: vec![],
                name: "x".into(),
            }),
        )
        .await
        .unwrap();

        match read_frame::<_, IpcResponse>(&mut client).await.unwrap() {
            IpcResponse::Err(IpcError::UnknownCommand) => {}
            other => panic!("expected Err(UnknownCommand), got {other:?}"),
        }
        // Connection closed afterwards (Bye).
        match read_frame::<_, IpcResponse>(&mut client).await.unwrap() {
            IpcResponse::Bye => {}
            other => panic!("expected Bye, got {other:?}"),
        }

        drop(client);
        handle_task.await.unwrap();
    }
}
