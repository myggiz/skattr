// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 3.C: deposit due attachment chunks into recipients' mailboxes.
//!
//! Sibling to `delivery::mailbox_sweeper`. Reads due `attachment_deposits`
//! rows, resolves the recipient's mailboxes (reusing the message-fallback
//! resolution), deposits the chunk ciphertext (from `ChunkStore`) via the
//! frozen `MailboxClient::deposit`, and marks the row `deposited`. On
//! all-deposited, prunes the rows + staged chunks. Mailbox unreachable / full
//! reschedules the row with backoff (the next sweep retries / failovers).

use sha2::{Digest, Sha256};

use crate::attachment::store::ChunkStore;
use crate::delivery::hub::MailboxFallbackShared;
use crate::storage::attachments::{AttachmentDepositRepo, AttachmentRepo};
use crate::storage::mailboxes::MailboxRepo;
use crate::storage::Pool;

/// Per-row backoff schedule (ms): index by min(attempts, len-1).
const BACKOFF_MS: &[i64] = &[15_000, 60_000, 300_000, 900_000];

pub(crate) async fn run_chunk_sweep(
    pool: &Pool,
    shared: &MailboxFallbackShared,
    chunk_store: &ChunkStore,
    now_ms: i64,
    batch: usize,
) {
    let deposit_repo = AttachmentDepositRepo::new(pool);
    let due = match deposit_repo.due(now_ms, batch) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(target: "skattr::delivery::chunk_sweep", error = %e, "due() failed");
            return;
        }
    };
    for row in due {
        let recipient = crate::identity::PublicKey(row.recipient);
        // Resolve the recipient's advertised mailboxes (reuse message resolution).
        let onions = match MailboxRepo::new(pool).list_for_contact(&recipient) {
            Ok(v) if !v.is_empty() => v,
            _ => {
                // No mailbox known → back off; maybe a card update lands later.
                reschedule(&deposit_repo, &row, now_ms);
                continue;
            }
        };
        let chunk = match chunk_store.get_chunk(&row.attachment_id, row.chunk_index) {
            Ok(c) => c,
            Err(_) => {
                // Staged chunk missing (pruned?) — drop the row to avoid a hot loop.
                if let Err(e) = deposit_repo.mark_deposited(&row.attachment_id, row.chunk_index) {
                    tracing::warn!(target: "skattr::delivery::chunk_sweep", error = %e, "mark_deposited (missing-chunk drop) failed");
                }
                continue;
            }
        };
        let recipient_hash: [u8; 32] = Sha256::digest(recipient.0).into();

        // Walk mailboxes with failover; deposit on first reachable.
        let mut deposited = false;
        let n = onions.len();
        let primary = (row.chunk_index as usize) % n; // spread across mailboxes
        for offset in 0..n {
            let onion = &onions[(primary + offset) % n];
            let mut client = match shared.factory.connect(onion).await {
                Ok(c) => c,
                Err(_) => continue,
            };
            match client.deposit(recipient_hash, chunk.clone(), 0).await {
                Ok(_ok) => {
                    if let Err(e) = deposit_repo.mark_deposited(&row.attachment_id, row.chunk_index)
                    {
                        tracing::warn!(target: "skattr::delivery::chunk_sweep", error = %e, "mark_deposited failed");
                    }
                    deposited = true;
                    break;
                }
                Err(_) => continue, // RecipientFull / ServerFull / protocol — try next
            }
        }
        if !deposited {
            reschedule(&deposit_repo, &row, now_ms);
            continue;
        }

        // If that was the last pending chunk, prune + clean staging + finalize 'out'.
        if deposit_repo
            .all_deposited(&row.attachment_id)
            .unwrap_or(false)
        {
            if let Err(e) = deposit_repo.delete_for_attachment(&row.attachment_id) {
                tracing::warn!(target: "skattr::delivery::chunk_sweep", error = %e, "delete_for_attachment failed");
            }
            if let Err(e) = chunk_store.remove(&row.attachment_id) {
                tracing::warn!(target: "skattr::delivery::chunk_sweep", error = %e, "chunk_store remove failed");
            }
            if let Err(e) = AttachmentRepo::new(pool).set_status(&row.attachment_id, "complete") {
                tracing::warn!(target: "skattr::delivery::chunk_sweep", error = %e, "set_status complete failed");
            }
        }
    }
}

fn reschedule(
    repo: &AttachmentDepositRepo<'_>,
    row: &crate::storage::attachments::DepositDue,
    now_ms: i64,
) {
    let attempts = row.attempts.saturating_add(1);
    let idx = (attempts as usize).min(BACKOFF_MS.len()) - 1;
    let next = now_ms + BACKOFF_MS[idx];
    if let Err(e) = repo.reschedule(&row.attachment_id, row.chunk_index, attempts, next) {
        tracing::warn!(target: "skattr::delivery::chunk_sweep", error = %e, "reschedule failed");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::contact::card::{ContactCard, ContactCardBody};
    use crate::contact::Contact;
    use crate::identity::{IdentityKey, PublicKey, Signature};
    use crate::mailbox::client::MailboxClient;
    use crate::mailbox::codec::{MailboxFrame, MailboxFrameCodec};
    use crate::mailbox::poll::{MailboxConnectFactory, MailboxStream};
    use crate::mailbox::protocol::DepositOk;
    use crate::storage::attachments::{AttachmentDepositRepo, AttachmentRepo};
    use crate::storage::ContactRepo;
    use futures::{SinkExt, StreamExt};
    use std::sync::Arc;
    use std::sync::Mutex as StdMutex;
    use tokio_util::codec::Framed;

    // Inline mailbox server: accept one Deposit, reply DepositOk.
    async fn deposit_ok_server(server: tokio::io::DuplexStream) {
        let mut framed = Framed::new(server, MailboxFrameCodec::new());
        if let Some(Ok(MailboxFrame::Deposit(_))) = framed.next().await {
            let _ = framed
                .send(MailboxFrame::DepositOk(DepositOk {
                    deposit_id: [0xAB; 16],
                    expires_at: 9_999,
                }))
                .await;
        }
    }

    // Stub factory: hands out one pre-seeded duplex per onion.
    struct StubFactory {
        slots: StdMutex<Vec<tokio::io::DuplexStream>>,
    }
    #[async_trait::async_trait]
    impl MailboxConnectFactory for StubFactory {
        async fn connect(
            &self,
            onion: &str,
        ) -> crate::error::Result<MailboxClient<Box<dyn MailboxStream>>> {
            let s = self.slots.lock().unwrap().pop();
            match s {
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

    #[tokio::test]
    async fn deposit_ok_deposits_chunk_and_prunes_row() {
        let pool = Arc::new(crate::storage::Pool::in_memory());
        let recipient = PublicKey([0x42; 32]);

        // Seed contact + ContactCard listing one mailbox onion.
        let contacts = ContactRepo::new(&pool);
        contacts
            .upsert(&Contact {
                identity: recipient,
                display_name: None,
                added_at: 0,
                card: None,
                muted: false,
            })
            .unwrap();
        contacts
            .put_card(&ContactCard {
                body: ContactCardBody {
                    identity: recipient,
                    onion: "self.onion".into(),
                    mailboxes: vec!["mb1.onion".into()],
                    version: 1,
                    expires_at: 9_999_999_999,
                },
                signature: Signature([0u8; 64]),
            })
            .unwrap();

        // Stage one chunk in a temp ChunkStore.
        let tmp = tempfile::tempdir().unwrap();
        let store = ChunkStore::new(tmp.path());
        let aid = [0x55u8; 16];
        store.put(&aid, 0, b"chunk-data").unwrap();

        // Insert the attachment row (required for set_status at prune step).
        AttachmentRepo::new(&pool)
            .insert(&aid, "out", b"manifest", 1, 0)
            .unwrap();

        // Enqueue the deposit row.
        AttachmentDepositRepo::new(&pool)
            .enqueue_all(&aid, &recipient.0, 1, 0)
            .unwrap();

        // Build MailboxFallbackShared with a StubFactory that has one slot for mb1.onion.
        let (client_side, server_side) = tokio::io::duplex(64 * 1024);
        let server_task = tokio::spawn(deposit_ok_server(server_side));
        let factory = Arc::new(StubFactory {
            slots: StdMutex::new(vec![client_side]),
        });
        let (events_tx, _events_rx) =
            tokio::sync::broadcast::channel::<crate::daemon::events::Event>(8);
        let identity = Arc::new(IdentityKey::generate().unwrap());
        let shared = crate::delivery::hub::MailboxFallbackShared {
            factory: factory.clone(),
            events: events_tx,
            identity,
        };

        // Run the sweep with now=1 (row is due at 0).
        run_chunk_sweep(&pool, &shared, &store, 1, 10).await;

        // Assert all_deposited is true and the row is pruned (due returns empty).
        assert!(
            AttachmentDepositRepo::new(&pool)
                .all_deposited(&aid)
                .unwrap(),
            "chunk must be marked deposited"
        );
        let still_due = AttachmentDepositRepo::new(&pool).due(10_000, 10).unwrap();
        assert!(
            still_due.is_empty(),
            "deposit row must be pruned after all-deposited"
        );

        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), server_task).await;
    }
}
