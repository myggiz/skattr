// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Background sweeper that re-deposits due mailbox-kind outbox rows.
//!
//! The single deposit/retry engine for offline delivery: each pass reads due
//! `target_kind='mailbox'` rows and deposits them (with per-mailbox failover +
//! backoff via `redeposit_mailbox_row`). Keeps the per-peer direct actor
//! strictly direct-only.

use crate::delivery::hub::MailboxFallbackShared;
use crate::storage::Pool;

/// Run one sweep pass: deposit every due mailbox-kind outbox row (best-effort).
pub(crate) async fn run_mailbox_sweep(
    pool: &Pool,
    shared: &MailboxFallbackShared,
    now: i64,
    batch: usize,
) {
    let rows = match crate::storage::outbox::OutboxRepo::new(pool).due_mailbox(now, batch) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(target: "skattr::delivery::sweeper", error = %e, "due_mailbox failed");
            return;
        }
    };
    for row in &rows {
        crate::delivery::hub::redeposit_mailbox_row(pool, shared, row, now).await;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::contact::card::{ContactCard, ContactCardBody};
    use crate::contact::Contact;
    use crate::daemon::events::{DeliveryStatus, Event};
    use crate::error::Result;
    use crate::identity::{IdentityKey, PublicKey, Signature};
    use crate::mailbox::client::MailboxClient;
    use crate::mailbox::codec::{MailboxFrame, MailboxFrameCodec};
    use crate::mailbox::poll::{MailboxConnectFactory, MailboxStream};
    use crate::mailbox::protocol::DepositOk;
    use crate::storage::outbox::OutboxRepo;
    use crate::storage::ContactRepo;
    use futures::{SinkExt, StreamExt};
    use std::collections::HashMap as StdHashMap;
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::broadcast;
    use tokio_util::codec::Framed;

    /// Stub `MailboxConnectFactory`: per-onion, hands out one in-process duplex
    /// peer with a tiny inline server that replies `DepositOk` to one Deposit.
    struct StubFactory {
        slots: StdMutex<StdHashMap<String, Vec<tokio::io::DuplexStream>>>,
    }

    impl StubFactory {
        fn new() -> Self {
            Self {
                slots: StdMutex::new(StdHashMap::new()),
            }
        }

        fn seed(&self, onion: &str) -> tokio::task::JoinHandle<()> {
            let (a, b) = tokio::io::duplex(64 * 1024);
            self.slots
                .lock()
                .unwrap()
                .entry(onion.to_string())
                .or_default()
                .push(a);
            tokio::spawn(deposit_server(b))
        }
    }

    #[async_trait::async_trait]
    impl MailboxConnectFactory for StubFactory {
        async fn connect(&self, onion: &str) -> Result<MailboxClient<Box<dyn MailboxStream>>> {
            let stream_opt = {
                let mut slots = self.slots.lock().unwrap();
                slots.get_mut(onion).and_then(|v| v.pop())
            };
            match stream_opt {
                Some(s) => {
                    let boxed: Box<dyn MailboxStream> = Box::new(s);
                    Ok(MailboxClient::from_stream(onion.to_string(), boxed))
                }
                None => Err(crate::error::CoreError::MailboxClient(
                    crate::error::MailboxClientErrorKind::Unreachable,
                )),
            }
        }
    }

    /// Tiny inline mailbox server: read one Deposit, reply DepositOk, then exit.
    async fn deposit_server(server: tokio::io::DuplexStream) {
        let mut framed = Framed::new(server, MailboxFrameCodec::new());
        let req = framed.next().await;
        let Some(Ok(MailboxFrame::Deposit(_))) = req else {
            return;
        };
        let _ = framed
            .send(MailboxFrame::DepositOk(DepositOk {
                deposit_id: [0xAB; 16],
                expires_at: 9_999,
            }))
            .await;
    }

    fn seed_contact_with_card(pool: &Pool, peer: PublicKey, mailboxes: Vec<String>) {
        let contacts = ContactRepo::new(pool);
        contacts
            .upsert(&Contact {
                identity: peer,
                display_name: None,
                added_at: 0,
                card: None,
                muted: false,
            })
            .unwrap();
        contacts
            .put_card(&ContactCard {
                body: ContactCardBody {
                    identity: peer,
                    onion: "self.onion".into(),
                    mailboxes,
                    version: 1,
                    expires_at: 9_999_999_999,
                },
                signature: Signature([0u8; 64]),
            })
            .unwrap();
    }

    #[tokio::test]
    async fn one_sweep_deposits_due_mailbox_row_and_deletes_it() {
        let pool = Arc::new(Pool::in_memory());
        let peer = PublicKey([0x42; 32]);
        let mid = [0x77u8; 16];
        let payload = vec![0xDE, 0xAD, 0xBE, 0xEF];

        seed_contact_with_card(&pool, peer, vec!["mb1.onion".into()]);

        // A due mailbox-kind outbox row linked to the peer.
        OutboxRepo::new(&pool)
            .insert_for_mailbox(&peer.0, &mid, 1, &payload, 0)
            .unwrap();

        let factory = Arc::new(StubFactory::new());
        let s1 = factory.seed("mb1.onion");

        let (events_tx, mut events_rx) = broadcast::channel::<Event>(8);
        let identity = Arc::new(IdentityKey::generate().unwrap());
        let shared = MailboxFallbackShared {
            factory: factory.clone(),
            events: events_tx,
            identity,
        };

        run_mailbox_sweep(&pool, &shared, 100, 16).await;

        // Row deleted = deposit succeeded.
        let still_due = OutboxRepo::new(&pool).due_mailbox(10_100, 16).unwrap();
        assert!(still_due.is_empty(), "mailbox row deleted after deposit");

        let evt = tokio::time::timeout(std::time::Duration::from_secs(2), events_rx.recv())
            .await
            .expect("event in time")
            .expect("channel open");
        assert!(matches!(
            evt,
            Event::DeliveryStatusChanged {
                status: DeliveryStatus::Deposited,
                ..
            }
        ));

        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), s1).await;
    }

    #[tokio::test]
    async fn sweep_reschedules_with_backoff_when_no_mailboxes() {
        let pool = Arc::new(Pool::in_memory());
        let peer = PublicKey([0x11; 32]);
        let mid = [0x22u8; 16];

        // Contact exists but has no card → no advertised mailboxes.
        ContactRepo::new(&pool)
            .upsert(&Contact {
                identity: peer,
                display_name: None,
                added_at: 0,
                card: None,
                muted: false,
            })
            .unwrap();
        OutboxRepo::new(&pool)
            .insert_for_mailbox(&peer.0, &mid, 1, b"x", 0)
            .unwrap();

        let factory = Arc::new(StubFactory::new());
        let (events_tx, _events_rx) = broadcast::channel::<Event>(4);
        let identity = Arc::new(IdentityKey::generate().unwrap());
        let shared = MailboxFallbackShared {
            factory,
            events: events_tx,
            identity,
        };

        run_mailbox_sweep(&pool, &shared, 100, 16).await;

        // Row still present (not deleted) but rescheduled into the future with
        // attempts bumped — not due at `now`, due far ahead.
        let due_now = OutboxRepo::new(&pool).due_mailbox(100, 16).unwrap();
        assert!(due_now.is_empty(), "row rescheduled past `now`");
        let due_later = OutboxRepo::new(&pool).due_mailbox(i64::MAX, 16).unwrap();
        assert_eq!(due_later.len(), 1, "row retained for a later retry");
        assert_eq!(due_later[0].attempts, 1, "attempts bumped by reschedule");
    }
}
