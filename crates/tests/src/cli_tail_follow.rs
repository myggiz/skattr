// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Phase 1.G integration test: `Event::MessageReceived` reaches a
//! `Subscribe(EventFilter::Messages { contact })` client, and
//! non-matching contacts are filtered out server-side.
//!
//! Harness limitation: a true Alice→Bob delivery would require symmetric
//! MLS group state on both daemons, which only works over real Tor (see
//! `cli_two_daemons.rs` module doc). This test instead drives the events
//! broadcaster directly — mimicking what `DaemonInbound::dispatch_for_group`
//! does after a successful decrypt — and asserts the subscribe + filter +
//! delivery contract end-to-end.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use skattr_core::daemon::commands::{Direction, MessageRecord};
use skattr_core::daemon::events::Event;
use skattr_core::daemon::ipc::wire::EventFilter;
use skattr_core::daemon::{Command, CommandResult, IpcClient};
use skattr_core::envelope::{Envelope, Kind, MessageId};
use skattr_core::identity::{IdentityKey, PublicKey, Seed};
use skattr_core::test_exports::{
    handle_connection, CommandExecutor, DaemonHandle, DeliveryHub, IpcError, Pool,
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

fn make_record(contact: PublicKey, body: &str, seq: u8) -> MessageRecord {
    MessageRecord::project(
        i64::from(seq),
        &Envelope {
            v: 1,
            id: MessageId([seq; 16]),
            ts: 1_700_000_000 + i64::from(seq),
            reply_to: None,
            kind: Kind::Text { body: body.into() },
        },
        contact,
        1,
        1_700_000_000 + i64::from(seq),
        Direction::Incoming,
    )
}

// ---------------------------------------------------------------------------
// Test.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn message_received_event_reaches_subscriber() {
    let handle = build_handle();
    let alice = PublicKey([0x77; 32]);
    let charlie = PublicKey([0xCC; 32]);

    let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
    let exec: Arc<dyn CommandExecutor> = Arc::new(ExecShim(handle.clone()));
    let events_tx = handle.events_tx.clone();
    let server_h = tokio::spawn(async move {
        handle_connection(server_io, exec, events_tx).await;
    });

    let mut client = IpcClient::from_stream(client_io);
    client
        .subscribe(EventFilter::Messages {
            contact: Some(alice),
        })
        .await
        .unwrap();

    // Emit two matching events + one mismatched (should be filtered).
    let _ = handle.events_tx.send(Event::MessageReceived {
        contact: alice,
        record: make_record(alice, "first", 1),
    });
    let _ = handle.events_tx.send(Event::MessageReceived {
        contact: charlie,
        record: make_record(charlie, "not for us", 2),
    });
    let _ = handle.events_tx.send(Event::MessageReceived {
        contact: alice,
        record: make_record(alice, "second", 3),
    });

    // Read exactly 2 events with a 5s budget. The charlie event must
    // not appear — it's filtered server-side by `EventFilter::Messages`.
    let mut seen_bodies = Vec::new();
    for _ in 0..2 {
        let evt = tokio::time::timeout(Duration::from_secs(5), client.next_event())
            .await
            .expect("timeout waiting for event")
            .unwrap();
        match evt {
            Event::MessageReceived { contact, record } => {
                assert_eq!(contact, alice, "filter must reject non-alice events");
                assert!(matches!(record.direction, Direction::Incoming));
                if let Kind::Text { body } = record.kind {
                    seen_bodies.push(body);
                }
            }
            other => panic!("expected MessageReceived, got {other:?}"),
        }
    }
    assert_eq!(seen_bodies, vec!["first".to_string(), "second".to_string()]);

    // Close client → server loop exits cleanly.
    drop(client);
    let _ = server_h.await;
}
