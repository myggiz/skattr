// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Outbox queue lifecycle: retarget aged direct rows onto the mailbox lane
//! where one exists, and expire them only where none does.
//!
//! Sibling to [`mailbox_sweeper`](crate::delivery::mailbox_sweeper) and
//! [`chunk_sweep`](crate::delivery::chunk_sweep). This is deliberately not in
//! the per-peer actor: queue lifetime is not connection work, and the actor
//! has no events sender in production.
//!
//! Two rules, in order:
//!
//! 1. **The contact advertises a mailbox** — never expire. The mailbox lane is
//!    exempt from the ±1 h `ts` window by design (2.C) and terminates on its
//!    own at `Deposited`, so a queued message can wait as long as it needs to.
//!    That is the entire point of having a mailbox.
//! 2. **The contact advertises no mailbox** — direct is the only lane, and
//!    past the window the peer will certainly reject the envelope
//!    ([`receiver`](crate::delivery::receiver) enforces it). The row is
//!    deleted and the message marked failed, with a reason naming the remedy.

use crate::delivery::hub::MailboxFallbackShared;
use crate::delivery::receiver::REPLAY_WINDOW_MS;
use crate::envelope::MessageId;
use crate::identity::PublicKey;
use crate::storage::mailboxes::MailboxRepo;
use crate::storage::messages::MessageRepo;
use crate::storage::outbox::{OutboxRepo, OutboxTargetKind};
use crate::storage::Pool;

/// Stop a margin short of the window so a message is never written to the wire
/// that the peer will certainly reject. Derived from [`REPLAY_WINDOW_MS`]
/// rather than written out, so the two cannot drift.
pub(crate) const DIRECT_EXPIRY_MS: i64 = REPLAY_WINDOW_MS - 5 * 60 * 1000;

/// Shown on the failed bubble. Names the remedy, not just the symptom.
const NO_MAILBOX_REASON: &str =
    "Not delivered — this contact has no mailbox, so messages cannot reach \
     them while they are offline.";

/// Run one sweep pass over due direct outbox rows (best-effort).
///
/// Known limitation: rows are read through [`OutboxRepo::due`], so a row whose
/// dial backoff puts `next_retry_at` in the future is not seen until it comes
/// due. Backoff is capped at 5 minutes ([`crate::delivery::backoff`]) against a
/// 55-minute deadline and a 60-second sweep, so a failed bubble can appear up
/// to ~5 minutes late — never early. Accepted; a second query is not worth it.
pub(crate) async fn run_outbox_sweep(
    pool: &Pool,
    shared: &MailboxFallbackShared,
    now: i64,
    batch: usize,
) {
    let outbox = OutboxRepo::new(pool);
    let rows = match outbox.due(now, batch) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(target: "skattr::delivery::outbox_sweep", error = %e, "due failed");
            return;
        }
    };

    let messages = MessageRepo::new(pool);
    let mailboxes = MailboxRepo::new(pool);
    let self_pubkey = shared.identity.public().0;

    for row in rows {
        if row.target_kind != OutboxTargetKind::Direct {
            continue; // the mailbox lane owns its own retries
        }

        // The envelope ts lives on the message, joined by
        // outbox.message_id == messages.envelope_id. A row whose message has
        // been pruned has no age to judge, so it is left alone.
        let envelope_ts = match messages.envelope_ts(&row.message_id) {
            Ok(Some(ts)) => ts,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(
                    target: "skattr::delivery::outbox_sweep",
                    error = %e,
                    "envelope_ts failed"
                );
                continue;
            }
        };
        if now.saturating_sub(envelope_ts) < DIRECT_EXPIRY_MS {
            continue; // still inside the window
        }

        let Ok(target) = <[u8; 32]>::try_from(row.target.as_slice()) else {
            continue;
        };
        let peer = PublicKey(target);

        let has_mailbox = match mailboxes.list_for_contact(&peer) {
            Ok(m) => !m.is_empty(),
            Err(e) => {
                tracing::warn!(
                    target: "skattr::delivery::outbox_sweep",
                    error = %e,
                    "list_for_contact failed; leaving row for the next sweep"
                );
                continue;
            }
        };

        if has_mailbox {
            // Rule 1. Hand the row to the existing orchestrator rather than
            // only flipping `target_kind`: it retargets AND deposits AND fails
            // over across mailboxes, so the row can reach `Deposited` on this
            // tick instead of waiting for the next `mailbox_sweeper` pass. On
            // failure it leaves the row mailbox-kind for that sweeper — which
            // is the point of rule 1: it is never expired here.
            crate::delivery::hub::run_mailbox_fallback(
                pool,
                shared,
                peer,
                MessageId(row.message_id),
                row.payload,
            )
            .await;
            continue;
        }

        // Rule 2. Delete first, then record. If the process dies between the
        // two the row is gone and the message shows as unknown, which is
        // recoverable; the reverse would leave a failed message that is still
        // being retried.
        match outbox.delete_by_id(row.id) {
            Ok(true) => {}
            Ok(false) => continue, // another sweep won the race
            Err(e) => {
                tracing::warn!(
                    target: "skattr::delivery::outbox_sweep",
                    error = %e,
                    "delete failed"
                );
                continue;
            }
        }
        if let Err(e) = messages.mark_failed(&row.message_id, &self_pubkey, NO_MAILBOX_REASON) {
            tracing::warn!(
                target: "skattr::delivery::outbox_sweep",
                error = %e,
                "mark_failed failed"
            );
        }
        // Redaction: no pubkey, no onion, no body.
        tracing::info!(
            target: "skattr::delivery::outbox_sweep",
            "outbox: gave up on a direct message — contact advertises no mailbox"
        );
        let _ = shared
            .events
            .send(crate::daemon::events::Event::DeliveryStatusChanged {
                message: MessageId(row.message_id),
                status: crate::daemon::events::DeliveryStatus::Failed(
                    NO_MAILBOX_REASON.to_string(),
                ),
            });
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::contact::card::{ContactCard, ContactCardBody};
    use crate::contact::Contact;
    use crate::daemon::events::{DeliveryStatus, Event};
    use crate::envelope::{Envelope, Kind, MessageId};
    use crate::error::Result;
    use crate::identity::{IdentityKey, PublicKey, Signature};
    use crate::mailbox::client::MailboxClient;
    use crate::mailbox::codec::{MailboxFrame, MailboxFrameCodec};
    use crate::mailbox::poll::{MailboxConnectFactory, MailboxStream};
    use crate::mailbox::protocol::DepositOk;
    use crate::storage::messages::{InsertParams, MessageRepo};
    use crate::storage::outbox::{OutboxRepo, OutboxTargetKind};
    use crate::storage::ContactRepo;
    use futures::{SinkExt, StreamExt};
    use std::collections::HashMap as StdHashMap;
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::broadcast;
    use tokio_util::codec::Framed;

    // ── factories ────────────────────────────────────────────────────────────

    /// Never connects. Models the field case the sweeper actually meets: the
    /// contact advertises a mailbox but it is unreachable right now.
    struct UnreachableFactory;

    #[async_trait::async_trait]
    impl MailboxConnectFactory for UnreachableFactory {
        async fn connect(&self, _onion: &str) -> Result<MailboxClient<Box<dyn MailboxStream>>> {
            Err(crate::error::CoreError::MailboxClient(
                crate::error::MailboxClientErrorKind::Unreachable,
            ))
        }
    }

    /// Copied from `mailbox_sweeper.rs`'s test module: per-onion, hands out one
    /// in-process duplex peer with a tiny inline server that replies
    /// `DepositOk` to one Deposit.
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

    // ── fixture ──────────────────────────────────────────────────────────────

    struct Fx {
        pool: Arc<Pool>,
        shared: MailboxFallbackShared,
        events: broadcast::Receiver<Event>,
        /// Our own identity pubkey — an outgoing message row is one whose
        /// `sender` is this, and `mark_failed` is scoped on it.
        me: PublicKey,
    }

    fn fixture(factory: Arc<dyn MailboxConnectFactory>) -> Fx {
        let pool = Arc::new(Pool::in_memory());
        let (events_tx, events) = broadcast::channel::<Event>(8);
        let identity = Arc::new(IdentityKey::generate().unwrap());
        let me = identity.public();
        Fx {
            pool,
            shared: MailboxFallbackShared {
                factory,
                events: events_tx,
                identity,
            },
            events,
            me,
        }
    }

    fn fixture_no_mailbox() -> Fx {
        fixture(Arc::new(UnreachableFactory))
    }

    /// A contact row with no card at all → `list_for_contact` is empty.
    fn seed_contact_without_mailbox(pool: &Pool, peer: PublicKey) {
        ContactRepo::new(pool)
            .upsert(&Contact {
                identity: peer,
                display_name: None,
                added_at: 0,
                card: None,
                muted: false,
            })
            .unwrap();
    }

    /// A contact whose signed card advertises `mailboxes`.
    fn seed_contact_with_mailboxes(pool: &Pool, peer: PublicKey, mailboxes: Vec<String>) {
        seed_contact_without_mailbox(pool, peer);
        ContactRepo::new(pool)
            .put_card(&ContactCard {
                body: ContactCardBody {
                    identity: peer,
                    onion: "peer.onion".into(),
                    mailboxes,
                    version: 1,
                    expires_at: 9_999_999_999,
                },
                // Storage tests skip signature verification.
                signature: Signature([0u8; 64]),
            })
            .unwrap();
    }

    /// A message row that looks like one *we* sent, at wall-clock `ts`.
    fn seed_outgoing_message(pool: &Pool, me: PublicKey, ts: i64) -> [u8; 16] {
        let env = Envelope {
            v: 1,
            id: MessageId::generate(),
            ts,
            reply_to: None,
            kind: Kind::Text {
                body: "hi".to_string(),
            },
        };
        MessageRepo::new(pool)
            .insert(InsertParams {
                group_id: &[0x01; 32],
                sender: &me.0,
                envelope: &env,
                mls_generation: 0,
                ts_daemon_recv: ts,
            })
            .unwrap();
        env.id.0
    }

    /// A direct outbox row that is **due** — `next_retry_at = 0`. The sweep
    /// reads through `OutboxRepo::due(now, ..)`, so a row seeded with a future
    /// `next_retry_at` is invisible to it and every assertion below would
    /// pass vacuously.
    fn seed_due_direct_outbox_row(pool: &Pool, peer: PublicKey, eid: [u8; 16]) -> i64 {
        OutboxRepo::new(pool)
            .insert(&peer.0, &eid, b"ciphertext", 0)
            .unwrap()
            .expect("fresh row")
    }

    fn failed_reason(pool: &Pool, eid: &[u8; 16]) -> Option<String> {
        MessageRepo::new(pool).delivery_outcome(eid).unwrap().1
    }

    // ── tests ────────────────────────────────────────────────────────────────

    /// No mailbox: direct is the only lane, and past the ts window it provably
    /// cannot deliver. The row is deleted and the message marked failed with a
    /// reason naming the actual remedy.
    #[tokio::test]
    async fn aged_row_without_mailbox_expires_with_a_reason() {
        let mut fx = fixture_no_mailbox();
        let peer = PublicKey([0x11; 32]);
        seed_contact_without_mailbox(&fx.pool, peer);
        let eid = seed_outgoing_message(&fx.pool, fx.me, 0);
        seed_due_direct_outbox_row(&fx.pool, peer, eid);

        run_outbox_sweep(&fx.pool, &fx.shared, DIRECT_EXPIRY_MS + 1, 32).await;

        assert!(
            OutboxRepo::new(&fx.pool)
                .due(i64::MAX, 10)
                .unwrap()
                .is_empty(),
            "an unreachable row must not be retried forever"
        );
        let reason = failed_reason(&fx.pool, &eid).expect("failure reason must be stored");
        assert!(
            reason.contains("mailbox"),
            "the reason must name the remedy; got {reason:?}"
        );

        match fx.events.try_recv() {
            Ok(Event::DeliveryStatusChanged {
                message,
                status: DeliveryStatus::Failed(_),
            }) => assert_eq!(message.0, eid, "the event must name the failed message"),
            other => panic!("expected exactly one Failed event, got {other:?}"),
        }
        assert!(fx.events.try_recv().is_err(), "exactly one event");
    }

    /// Decision 1: with a mailbox, a queued message NEVER expires. It is
    /// retargeted onto the mailbox lane, which terminates on its own at
    /// `Deposited`. Expiring it here would defeat the point of having a
    /// mailbox at all.
    ///
    /// The mailbox is unreachable on this pass — the hostile case for the
    /// rule, since nothing terminates the row and a "tidy up" that expires
    /// aged rows would eat it.
    #[tokio::test]
    async fn aged_row_with_mailbox_is_retargeted_never_expired() {
        let fx = fixture(Arc::new(UnreachableFactory));
        let peer = PublicKey([0x22; 32]);
        seed_contact_with_mailboxes(&fx.pool, peer, vec!["mb1.onion".into()]);
        let eid = seed_outgoing_message(&fx.pool, fx.me, 0);
        let row_id = seed_due_direct_outbox_row(&fx.pool, peer, eid);

        run_outbox_sweep(&fx.pool, &fx.shared, DIRECT_EXPIRY_MS + 1, 32).await;

        let row = OutboxRepo::new(&fx.pool)
            .get(row_id)
            .unwrap()
            .expect("a mailbox contact's row must NOT be deleted");
        assert_eq!(
            row.target_kind,
            OutboxTargetKind::Mailbox,
            "an aged direct row for a mailbox contact must be retargeted"
        );
        assert_eq!(
            failed_reason(&fx.pool, &eid),
            None,
            "a mailbox contact's message must not fail"
        );
    }

    /// The same rule on the happy path: a reachable mailbox takes the deposit,
    /// the row reaches its terminal state, and the message is still not failed.
    #[tokio::test]
    async fn aged_row_with_reachable_mailbox_is_deposited_not_failed() {
        let factory = Arc::new(StubFactory::new());
        let server = factory.seed("mb1.onion");
        let mut fx = fixture(factory);
        let peer = PublicKey([0x23; 32]);
        seed_contact_with_mailboxes(&fx.pool, peer, vec!["mb1.onion".into()]);
        let eid = seed_outgoing_message(&fx.pool, fx.me, 0);
        let row_id = seed_due_direct_outbox_row(&fx.pool, peer, eid);

        run_outbox_sweep(&fx.pool, &fx.shared, DIRECT_EXPIRY_MS + 1, 32).await;

        assert!(
            OutboxRepo::new(&fx.pool).get(row_id).unwrap().is_none(),
            "a deposited row is deleted by the mailbox lane"
        );
        assert_eq!(
            failed_reason(&fx.pool, &eid),
            None,
            "a deposited message must not be marked failed"
        );
        match fx.events.try_recv() {
            Ok(Event::DeliveryStatusChanged {
                status: DeliveryStatus::Deposited,
                ..
            }) => {}
            other => panic!("expected Deposited, got {other:?}"),
        }
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), server).await;
    }

    /// Rows inside the window are untouched on both lanes.
    #[tokio::test]
    async fn fresh_row_is_left_alone() {
        let mut fx = fixture_no_mailbox();
        let peer = PublicKey([0x33; 32]);
        seed_contact_without_mailbox(&fx.pool, peer);
        let eid = seed_outgoing_message(&fx.pool, fx.me, 0);
        let row_id = seed_due_direct_outbox_row(&fx.pool, peer, eid);

        run_outbox_sweep(&fx.pool, &fx.shared, DIRECT_EXPIRY_MS - 60_000, 32).await;

        let row = OutboxRepo::new(&fx.pool)
            .get(row_id)
            .unwrap()
            .expect("a fresh row must survive");
        assert_eq!(row.target_kind, OutboxTargetKind::Direct);
        assert_eq!(failed_reason(&fx.pool, &eid), None);
        assert!(fx.events.try_recv().is_err(), "no event for a fresh row");
    }

    /// Sweeping twice must not emit a second Failed.
    #[tokio::test]
    async fn expiry_is_idempotent() {
        let mut fx = fixture_no_mailbox();
        let peer = PublicKey([0x44; 32]);
        seed_contact_without_mailbox(&fx.pool, peer);
        let eid = seed_outgoing_message(&fx.pool, fx.me, 0);
        seed_due_direct_outbox_row(&fx.pool, peer, eid);

        run_outbox_sweep(&fx.pool, &fx.shared, DIRECT_EXPIRY_MS + 1, 32).await;
        let _ = fx.events.try_recv();
        run_outbox_sweep(&fx.pool, &fx.shared, DIRECT_EXPIRY_MS + 1, 32).await;
        assert!(
            fx.events.try_recv().is_err(),
            "no duplicate Failed on re-sweep"
        );
    }
}
