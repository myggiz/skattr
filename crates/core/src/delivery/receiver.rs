// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Receiver-side ingress: timestamp check, dedup, persist, ACK.
//!
//! Dedup is a sliding 24-hour `(sender, message_id)` index; ordering
//! is authoritative per the MLS generation number — not per the
//! envelope timestamp (`Envelope.ts` is display-only with a ±1 h
//! replay window).

use crate::envelope::{Envelope, MessageId};
use crate::error::Result;
use crate::identity::PublicKey;
use crate::storage::messages::MessageRepo;
use crate::storage::seen_messages::SeenMessagesRepo;

/// Replay window: an envelope whose `ts` is more than ±1 h from the
/// local clock is rejected with no ACK. Millis.
pub const REPLAY_WINDOW_MS: i64 = 60 * 60 * 1000;

/// Outcome of processing an incoming envelope.
#[derive(Debug, Clone)]
pub enum ReceiveOutcome {
    /// Fresh message persisted; caller should surface to UI and send an ACK.
    New(Envelope),
    /// We've already seen `(sender, message_id)`; caller should still send
    /// an ACK (the sender may have retried because their ACK was lost).
    Duplicate,
    /// Rejected (ts out of replay window). Caller must NOT ACK.
    Rejected(String),
}

/// Ingest one decrypted envelope from `sender` into storage.
///
/// Does not perform MLS decryption — callers have already run
/// `Group::decrypt` on the ciphertext. Pure side effects on
/// `seen_messages` and `messages`.
pub fn receive(
    sender: &PublicKey,
    group_id: &[u8],
    envelope: Envelope,
    now_ms: i64,
    seen: &SeenMessagesRepo<'_>,
    messages: &MessageRepo<'_>,
) -> Result<ReceiveOutcome> {
    if envelope.ts.saturating_sub(now_ms).saturating_abs() > REPLAY_WINDOW_MS {
        return Ok(ReceiveOutcome::Rejected(format!(
            "ts outside ±1h window: envelope ts={}, now={}",
            envelope.ts, now_ms
        )));
    }
    let is_new = seen.insert(&sender.0, &envelope.id.0, now_ms)?;
    if !is_new {
        return Ok(ReceiveOutcome::Duplicate);
    }
    let _ = messages.insert(crate::storage::messages::InsertParams {
        group_id,
        sender: &sender.0,
        envelope: &envelope,
        mls_generation: 0,      // populated by Task 13
        ts_daemon_recv: now_ms, // best-effort placeholder; Task 13 splits ts_daemon_recv from the replay-window ts argument
    })?;
    Ok(ReceiveOutcome::New(envelope))
}

/// Build an ACK frame payload for `message_id`.
#[must_use]
pub fn build_ack(message_id: MessageId) -> MessageId {
    message_id
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::envelope::{Kind, MessageId};
    use crate::storage::Pool;

    fn env(id: u8, ts: i64) -> Envelope {
        Envelope {
            v: 1,
            id: MessageId([id; 16]),
            ts,
            reply_to: None,
            kind: Kind::Text {
                body: "hello".into(),
            },
        }
    }

    #[test]
    fn first_receive_returns_new_and_persists() {
        let pool = Pool::in_memory();
        let seen = SeenMessagesRepo::new(&pool);
        let msgs = MessageRepo::new(&pool);
        let sender = PublicKey([0xAA; 32]);
        let out = receive(&sender, &[0x01; 16], env(0x01, 1000), 1000, &seen, &msgs).unwrap();
        assert!(matches!(out, ReceiveOutcome::New(_)));
        assert!(seen.contains(&sender.0, &[0x01; 16]).unwrap());
    }

    #[test]
    fn second_receive_same_id_returns_duplicate_and_does_not_double_insert() {
        let pool = Pool::in_memory();
        let seen = SeenMessagesRepo::new(&pool);
        let msgs = MessageRepo::new(&pool);
        let sender = PublicKey([0xAA; 32]);
        let e = env(0x01, 1000);
        receive(&sender, &[0x01; 16], e.clone(), 1000, &seen, &msgs).unwrap();
        let out = receive(&sender, &[0x01; 16], e, 1000, &seen, &msgs).unwrap();
        assert!(matches!(out, ReceiveOutcome::Duplicate));
        let rows = msgs.recent(&[0x01; 16], 10).unwrap();
        assert_eq!(rows.len(), 1, "dup must not insert a second messages row");
    }

    #[test]
    fn ts_more_than_one_hour_in_the_past_is_rejected() {
        let pool = Pool::in_memory();
        let seen = SeenMessagesRepo::new(&pool);
        let msgs = MessageRepo::new(&pool);
        let sender = PublicKey([0xAA; 32]);
        let now = 10_000_000i64;
        let old = now - (REPLAY_WINDOW_MS + 1);
        let out = receive(&sender, &[0x01; 16], env(0x01, old), now, &seen, &msgs).unwrap();
        assert!(matches!(out, ReceiveOutcome::Rejected(_)));
        assert!(!seen.contains(&sender.0, &[0x01; 16]).unwrap());
    }

    #[test]
    fn ts_more_than_one_hour_in_the_future_is_rejected() {
        let pool = Pool::in_memory();
        let seen = SeenMessagesRepo::new(&pool);
        let msgs = MessageRepo::new(&pool);
        let sender = PublicKey([0xAA; 32]);
        let now = 10_000_000i64;
        let future = now + (REPLAY_WINDOW_MS + 1);
        let out = receive(&sender, &[0x01; 16], env(0x01, future), now, &seen, &msgs).unwrap();
        assert!(matches!(out, ReceiveOutcome::Rejected(_)));
    }

    #[test]
    fn build_ack_returns_input_id() {
        let id = MessageId([0x77; 16]);
        assert_eq!(build_ack(id), id);
    }
}
