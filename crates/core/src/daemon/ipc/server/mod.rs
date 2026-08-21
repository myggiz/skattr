// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic))]

//! IPC server. Platform-neutral request/response loop on top of the
//! per-platform listener (Unix domain socket on Unix; Named Pipe on
//! Windows). The platform-specific `Server` type is re-exported from
//! the active child module.

use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::broadcast;

use crate::daemon::commands::{Command, CommandResult};
use crate::daemon::events::Event;
use crate::daemon::ipc::codec::{read_frame, write_frame, CodecError};
use crate::daemon::ipc::wire::{EventFilter, IpcError, IpcRequest, IpcResponse};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::current_uid;
#[cfg(unix)]
pub use unix::Server;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub(crate) use windows::current_sid;
#[cfg(target_os = "windows")]
pub use windows::Server;

/// Execute one `Command` and return its `CommandResult` or a typed
/// `IpcError`. Decouples the per-connection handler from the concrete
/// `DaemonHandle` so the unit tests can drive the handler with a mock.
#[async_trait::async_trait]
pub trait CommandExecutor: Send + Sync {
    /// Dispatch `cmd` and return a result or typed wire error.
    async fn execute(&self, cmd: Command) -> std::result::Result<CommandResult, IpcError>;

    /// Snapshot the latest cached `TorStatus`. Default `None` so test
    /// stubs and pre-2.C executors compile unchanged. The production
    /// `DaemonHandle` impl returns `latest_tor_status()`.
    fn latest_tor_status(&self) -> Option<crate::daemon::events::TorStatus> {
        None
    }

    /// Return the wipe target path if `WipeAllData` was dispatched on
    /// this connection, consuming the signal. Default `None` so test
    /// stubs and executors that never handle `WipeAllData` compile
    /// unchanged. The production `DaemonHandle` impl delegates to
    /// `DaemonHandle::take_wipe_target`.
    ///
    /// Called by `handle_connection` AFTER writing the terminal `Bye`
    /// frame so the teardown is flush-ordered (T3-3). Must be called
    /// at most once per wipe event — the `Mutex<Option>` implementation
    /// genuinely consumes the signal.
    fn take_wipe_target(&self) -> Option<PathBuf> {
        None
    }

    /// Best-effort close the storage pool before wipe teardown so the
    /// WAL is folded and the plaintext DB is encrypted before
    /// `remove_dir_all` deletes the data directory. Default no-op so
    /// test stubs compile unchanged. The production `DaemonHandle` impl
    /// calls `self.pool.close()` and logs any error.
    fn wipe_close(&self) {}
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
    // Set when a `WipeAllData` command is dispatched. Used in post-loop
    // teardown. We stash it here rather than calling `take_wipe_target`
    // a second time — Fix 2 makes the signal genuinely consuming.
    let mut pending_wipe: Option<PathBuf> = None;

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
                // Consume the wipe signal exactly once. Fix 2 makes
                // `take_wipe_target` a genuine take (Mutex<Option>) so
                // we must not call it twice; stash the result in
                // `pending_wipe` for the post-loop teardown section.
                let wipe_target = executor.take_wipe_target();
                // Close after a one-shot Execute if no subscription is
                // active. When subscribed, keep the connection open so
                // the client can interleave Execute(SendMessage) calls
                // with the ongoing event stream. Always close on error.
                // Also close immediately if a WipeAllData was dispatched —
                // the client must receive its reply and Bye before teardown.
                let is_terminal = subscribed.is_none()
                    || matches!(resp, IpcResponse::Err(_))
                    || wipe_target.is_some();
                if write_frame(&mut stream, &resp).await.is_err() {
                    break;
                }
                if let Some(dir) = wipe_target {
                    pending_wipe = Some(dir);
                }
                if is_terminal {
                    break;
                }
            }
            IpcRequest::Subscribe(filter) => {
                subscribed = Some(filter.clone());
                events_rx = Some(events_tx.subscribe());
                if write_frame(&mut stream, &IpcResponse::Ok(CommandResult::Subscribed))
                    .await
                    .is_err()
                {
                    break;
                }
                // 2.C: replay cached TorStatus immediately if the filter
                // matches. The cache is populated by the tap task in
                // Daemon::run; lag-induced gaps fall through (None).
                if let Some(status) = executor.latest_tor_status() {
                    let replay = Event::TorStatusChanged(status);
                    if event_matches(&replay, subscribed.as_ref())
                        && write_frame(&mut stream, &IpcResponse::Event(replay))
                            .await
                            .is_err()
                    {
                        break;
                    }
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

    // T3-3: flush-ordered wipe teardown. `pending_wipe` was set in the
    // Execute arm when `WipeAllData` was dispatched. Now that the reply +
    // Bye are flushed, perform the actual teardown: close the pool (so the
    // WAL is folded and the DB is encrypted), drop the executor (Arc<Handle>
    // and its background tasks), remove the data directory, then exit. This
    // runs deterministically AFTER the client has received its reply.
    //
    // We use `pending_wipe` (not a second `take_wipe_target()` call) because
    // Fix 2 makes the signal genuinely consuming — a second call returns None.
    if let Some(data_dir) = pending_wipe {
        // Close the pool before dropping the executor so the WAL is
        // folded and plaintext DB is encrypted before removal.
        executor.wipe_close();
        // Drop the executor (and its Arc<DaemonHandle>) so background
        // tasks (retention sweep, log tap, mailbox poller) get a chance
        // to wind down before we remove their storage.
        drop(executor);

        if let Err(e) = tokio::fs::remove_dir_all(&data_dir).await {
            tracing::error!(
                error = %e,
                dir = ?data_dir,
                "wipe_all_data: remove_dir_all failed; exiting anyway"
            );
        }
        #[cfg(not(test))]
        std::process::exit(0);
        // In tests, skip process::exit so the async task completes normally.
        #[cfg(test)]
        return;
    }
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
        (EventFilter::Contact(peer), Event::ContactRemoved(contact)) => contact == peer,
        (EventFilter::Contact(peer), Event::ContactCardReceived { contact, .. }) => contact == peer,
        (EventFilter::Messages { contact: None }, Event::MessageReceived { .. }) => true,
        (EventFilter::Messages { contact: Some(c) }, Event::MessageReceived { contact, .. }) => {
            c == contact
        }
        (EventFilter::Messages { .. }, _) => false,
        (EventFilter::Mailboxes, Event::MailboxStatusChanged { .. }) => true,
        (EventFilter::Mailboxes, _) => false,
        (EventFilter::Delivery, Event::DeliveryStatusChanged { .. }) => true,
        (EventFilter::Delivery, _) => false,
        (EventFilter::Logs, Event::LogRecord(_)) => true,
        (EventFilter::Logs, _) => false,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[tokio::test]
    async fn event_matches_filters_message_received_by_contact() {
        use crate::daemon::commands::{Direction, MessageRecord};
        use crate::daemon::events::Event;
        use crate::daemon::ipc::wire::EventFilter;
        use crate::envelope::{Kind, MessageId};
        use crate::identity::PublicKey;

        let alice = PublicKey([0xAA; 32]);
        let bob = PublicKey([0xBB; 32]);

        let make_record = |id: u8, contact: PublicKey| {
            MessageRecord::project(
                i64::from(id),
                &crate::envelope::Envelope {
                    v: 1,
                    id: MessageId([id; 16]),
                    ts: 1_700_000_000,
                    reply_to: None,
                    kind: Kind::Text { body: "x".into() },
                },
                contact,
                1,
                1_700_000_000,
                Direction::Incoming,
                None,
            )
        };

        // Filter scoped to Bob: only Bob's events should pass.
        let bob_filter = EventFilter::Messages { contact: Some(bob) };
        let mut survivors = Vec::new();
        for evt in [
            Event::MessageReceived {
                contact: alice,
                record: make_record(1, alice),
            },
            Event::MessageReceived {
                contact: bob,
                record: make_record(2, bob),
            },
        ] {
            if event_matches(&evt, Some(&bob_filter)) {
                survivors.push(evt);
            }
        }
        assert_eq!(survivors.len(), 1);

        // Filter scoped to all: both pass.
        let all_filter = EventFilter::Messages { contact: None };
        let mut all = Vec::new();
        for evt in [
            Event::MessageReceived {
                contact: alice,
                record: make_record(1, alice),
            },
            Event::MessageReceived {
                contact: bob,
                record: make_record(2, bob),
            },
        ] {
            if event_matches(&evt, Some(&all_filter)) {
                all.push(evt);
            }
        }
        assert_eq!(all.len(), 2);

        // Empty filter (None) → no events pass through (existing contract).
        assert!(!event_matches(
            &Event::MessageReceived {
                contact: alice,
                record: make_record(1, alice),
            },
            None,
        ));
    }

    #[test]
    fn event_filter_mailboxes_matches_only_mailbox_status() {
        use crate::daemon::events::{Event, MailboxStatus};
        use crate::daemon::ipc::wire::EventFilter;

        let mailbox_event = Event::MailboxStatusChanged {
            mailbox_id: 1,
            status: MailboxStatus::Reachable,
        };
        assert!(event_matches(&mailbox_event, Some(&EventFilter::Mailboxes)));

        let tor_event = Event::TorStatusChanged(crate::daemon::events::TorStatus::Ready);
        assert!(!event_matches(&tor_event, Some(&EventFilter::Mailboxes)));
    }

    #[test]
    fn event_filter_delivery_matches_only_delivery_status() {
        use crate::daemon::events::{DeliveryStatus, Event};
        use crate::daemon::ipc::wire::EventFilter;
        use crate::envelope::MessageId;

        let delivery_event = Event::DeliveryStatusChanged {
            message: MessageId([0; 16]),
            status: DeliveryStatus::Deposited,
        };
        assert!(event_matches(&delivery_event, Some(&EventFilter::Delivery)));

        let tor_event = Event::TorStatusChanged(crate::daemon::events::TorStatus::Ready);
        assert!(!event_matches(&tor_event, Some(&EventFilter::Delivery)));
    }

    #[test]
    fn event_filter_contact_matches_contact_card_received_for_same_peer() {
        use crate::daemon::events::Event;
        use crate::daemon::ipc::wire::EventFilter;
        use crate::identity::PublicKey;

        let alice = PublicKey([7; 32]);
        let bob = PublicKey([8; 32]);
        let card_event = Event::ContactCardReceived {
            contact: alice,
            version: 1,
        };
        assert!(event_matches(
            &card_event,
            Some(&EventFilter::Contact(alice))
        ));
        assert!(!event_matches(
            &card_event,
            Some(&EventFilter::Contact(bob))
        ));
    }

    #[test]
    fn event_filter_all_matches_new_events() {
        use crate::daemon::events::{Event, MailboxStatus};
        use crate::daemon::ipc::wire::EventFilter;
        use crate::identity::PublicKey;

        let mailbox_event = Event::MailboxStatusChanged {
            mailbox_id: 1,
            status: MailboxStatus::Reachable,
        };
        assert!(event_matches(&mailbox_event, Some(&EventFilter::All)));

        let card_event = Event::ContactCardReceived {
            contact: PublicKey([7; 32]),
            version: 1,
        };
        assert!(event_matches(&card_event, Some(&EventFilter::All)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subscribe_ack_replays_cached_tor_status() {
        use crate::daemon::events::TorStatus;

        struct StubExec {
            status: std::sync::Mutex<Option<TorStatus>>,
        }
        #[async_trait]
        impl CommandExecutor for StubExec {
            async fn execute(&self, _: Command) -> std::result::Result<CommandResult, IpcError> {
                Ok(CommandResult::Ok)
            }
            fn latest_tor_status(&self) -> Option<TorStatus> {
                self.status.lock().unwrap().clone()
            }
        }

        let (mut client, server_stream) = tokio::io::duplex(1024 * 1024);
        let (events_tx, _) = broadcast::channel::<Event>(16);
        let executor: Arc<dyn CommandExecutor> = Arc::new(StubExec {
            status: std::sync::Mutex::new(Some(TorStatus::Bootstrapping(7))),
        });

        let handle_task = tokio::spawn(handle_connection(server_stream, executor, events_tx));

        // Subscribe with EventFilter::All.
        write_frame(&mut client, &IpcRequest::Subscribe(EventFilter::All))
            .await
            .unwrap();

        // First response: Subscribed ack.
        let r1: IpcResponse = read_frame(&mut client).await.unwrap();
        assert!(
            matches!(r1, IpcResponse::Ok(CommandResult::Subscribed)),
            "expected Ok(Subscribed), got {r1:?}"
        );

        // Second response: replayed TorStatus from the cache.
        let r2: IpcResponse = read_frame(&mut client).await.unwrap();
        assert!(
            matches!(
                r2,
                IpcResponse::Event(Event::TorStatusChanged(TorStatus::Bootstrapping(7)))
            ),
            "expected replayed TorStatus::Bootstrapping(7), got {r2:?}"
        );

        drop(client);
        let _ = handle_task.await;
    }

    /// A `WipeAllData` command dispatched on a *subscribed* connection must be
    /// treated as terminal: the server sends `Ok` + `Bye` and the
    /// `handle_connection` future must complete, even though the connection
    /// is normally kept alive while subscribed.
    ///
    /// This is the regression test for Fix 3a (wipe-terminal) + Fix 2
    /// (consuming wipe signal). `process::exit` is guarded with
    /// `#[cfg(not(test))]` in the production path so this test task
    /// exits normally.
    #[tokio::test]
    async fn wipe_on_subscribed_connection_is_terminal() {
        struct WipeExec {
            wipe_signaled: std::sync::Mutex<bool>,
        }
        #[async_trait]
        impl CommandExecutor for WipeExec {
            async fn execute(&self, _cmd: Command) -> std::result::Result<CommandResult, IpcError> {
                *self.wipe_signaled.lock().unwrap() = true;
                Ok(CommandResult::Ok)
            }
            fn take_wipe_target(&self) -> Option<std::path::PathBuf> {
                let mut flag = self.wipe_signaled.lock().unwrap();
                if *flag {
                    *flag = false; // consume so second call returns None
                    Some(std::path::PathBuf::from("/nonexistent-wipe-test-dir"))
                } else {
                    None
                }
            }
        }

        let (mut client, server_stream) = tokio::io::duplex(1024 * 1024);
        let (events_tx, _) = broadcast::channel::<Event>(16);
        let executor: Arc<dyn CommandExecutor> = Arc::new(WipeExec {
            wipe_signaled: std::sync::Mutex::new(false),
        });

        let handle_task = tokio::spawn(handle_connection(server_stream, executor, events_tx));

        // Subscribe first so the connection is in the subscribed state.
        write_frame(&mut client, &IpcRequest::Subscribe(EventFilter::All))
            .await
            .unwrap();
        let r1: IpcResponse = read_frame(&mut client).await.unwrap();
        assert!(
            matches!(r1, IpcResponse::Ok(CommandResult::Subscribed)),
            "expected Ok(Subscribed), got {r1:?}"
        );

        // Execute WipeAllData on the subscribed connection.
        write_frame(&mut client, &IpcRequest::Execute(Command::WipeAllData))
            .await
            .unwrap();

        // Server must send Ok(CommandResult::Ok).
        let r2: IpcResponse = read_frame(&mut client).await.unwrap();
        assert!(
            matches!(r2, IpcResponse::Ok(CommandResult::Ok)),
            "expected Ok(Ok) after wipe, got {r2:?}"
        );

        // Server must send Bye (connection treated as terminal even though subscribed).
        let r3: IpcResponse = read_frame(&mut client).await.unwrap();
        assert!(
            matches!(r3, IpcResponse::Bye),
            "expected Bye after wipe on subscribed connection, got {r3:?}"
        );

        // The handle_connection future must complete within 5 s (not hang
        // waiting for more input on the subscribed connection).
        let timed = tokio::time::timeout(std::time::Duration::from_secs(5), handle_task).await;
        assert!(
            timed.is_ok(),
            "handle_connection must complete after wipe (timed out)"
        );
        timed.unwrap().unwrap();
    }
}
