// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Receiver-side ingress: timestamp check, dedup, persist, ACK.
//!
//! Dedup is a sliding 24-hour `(sender, message_id)` index; ordering
//! is authoritative per the MLS generation number — not per the
//! envelope timestamp (`Envelope.ts` is display-only with a ±1 h
//! replay window).

use crate::envelope::{Envelope, MessageId};
use crate::error::{CoreError, Result};
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
    /// Carries the persisted `row_id`, the `(sender, group_id)` key,
    /// and the authoritative ordering/arrival metadata so the
    /// `InboundDispatch` caller can broadcast `Event::MessageReceived`
    /// after the ACK succeeds.
    New {
        /// The decrypted envelope that was persisted.
        envelope: Envelope,
        /// SQLite `rowid` of the inserted `messages` row.
        row_id: i64,
        /// Noise static public key of the sending peer.
        sender: PublicKey,
        /// MLS group id (opaque bytes) that authored this message.
        group_id: [u8; 32],
        /// MLS epoch of the decrypting group at decrypt time — the
        /// authoritative ordering signal (not `envelope.ts`).
        mls_generation: u64,
        /// Local daemon wall-clock arrival time, in seconds since the
        /// UNIX epoch. Used for retention sweeps and diagnostics.
        ts_daemon_recv: i64,
    },
    /// We've already seen `(sender, message_id)`; caller should still send
    /// an ACK (the sender may have retried because their ACK was lost).
    Duplicate,
    /// Rejected (ts out of replay window). Caller must NOT ACK.
    Rejected(String),
}

/// Ingest one decrypted envelope from `sender` into storage, inside
/// the caller's transaction.
///
/// This is the transactional core of `receive`: it writes to
/// `seen_messages` and `messages` using the provided `tx` without
/// opening a new pool transaction. Use this when the seen-row and
/// message row must commit atomically with other operations (e.g.
/// MLS snapshot via `Group::save_in_tx`).
///
/// Parameters are the same as [`receive`]; see that function for docs.
#[allow(clippy::too_many_arguments)]
pub(crate) fn receive_in_tx(
    tx: &rusqlite::Transaction<'_>,
    sender: &PublicKey,
    group_id: &[u8],
    envelope: Envelope,
    now_ms: i64,
    mls_generation: u64,
    ts_daemon_recv: i64,
    seen: &SeenMessagesRepo<'_>,
    messages: &MessageRepo<'_>,
) -> Result<ReceiveOutcome> {
    if envelope.ts.saturating_sub(now_ms).saturating_abs() > REPLAY_WINDOW_MS {
        return Ok(ReceiveOutcome::Rejected(format!(
            "ts outside ±1h window: envelope ts={}, now={}",
            envelope.ts, now_ms
        )));
    }
    let is_new = seen.insert_in_tx(tx, &sender.0, &envelope.id.0, now_ms)?;
    if !is_new {
        return Ok(ReceiveOutcome::Duplicate);
    }
    let gid_arr: [u8; 32] = group_id.try_into().map_err(|_| {
        CoreError::Storage(crate::storage::StorageErrorKind::Other(
            "group_id must be 32 bytes".into(),
        ))
    })?;
    let row_id = messages.insert_in_tx(
        tx,
        crate::storage::messages::InsertParams {
            group_id,
            sender: &sender.0,
            envelope: &envelope,
            mls_generation,
            ts_daemon_recv,
        },
    )?;
    Ok(ReceiveOutcome::New {
        envelope,
        row_id,
        sender: *sender,
        group_id: gid_arr,
        mls_generation,
        ts_daemon_recv,
    })
}

/// Ingest one decrypted envelope from `sender` into storage.
///
/// Does not perform MLS decryption — callers have already run
/// `Group::decrypt` on the ciphertext. Pure side effects on
/// `seen_messages` and `messages`.
///
/// Parameters:
/// - `now_ms`: local clock in **milliseconds** since the UNIX epoch,
///   used for the ±1h replay window check against `envelope.ts`
///   (both are millis per [`Envelope::ts`](crate::envelope::Envelope)).
/// - `mls_generation`: the decrypting group's epoch at the moment of
///   decrypt — authoritative ordering, not the envelope `ts`.
/// - `ts_daemon_recv`: the local daemon's wall-clock arrival time in
///   **seconds** since the UNIX epoch. Persisted on the messages row
///   and surfaced on the wire in `MessageRecord`; used for retention
///   sweeps and diagnostics.
#[allow(clippy::too_many_arguments)]
pub fn receive(
    sender: &PublicKey,
    group_id: &[u8],
    envelope: Envelope,
    now_ms: i64,
    mls_generation: u64,
    ts_daemon_recv: i64,
    seen: &SeenMessagesRepo<'_>,
    messages: &MessageRepo<'_>,
) -> Result<ReceiveOutcome> {
    seen.pool().transaction(|tx| {
        receive_in_tx(
            tx,
            sender,
            group_id,
            envelope,
            now_ms,
            mls_generation,
            ts_daemon_recv,
            seen,
            messages,
        )
    })
}

/// Build an ACK frame payload for `message_id`.
#[must_use]
pub fn build_ack(message_id: MessageId) -> MessageId {
    message_id
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
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
        let out = receive(
            &sender,
            &[0x01; 32],
            env(0x01, 1000),
            1000,
            0,
            1000,
            &seen,
            &msgs,
        )
        .unwrap();
        assert!(matches!(out, ReceiveOutcome::New { .. }));
        assert!(seen.contains(&sender.0, &[0x01; 16]).unwrap());
    }

    #[test]
    fn second_receive_same_id_returns_duplicate_and_does_not_double_insert() {
        let pool = Pool::in_memory();
        let seen = SeenMessagesRepo::new(&pool);
        let msgs = MessageRepo::new(&pool);
        let sender = PublicKey([0xAA; 32]);
        let e = env(0x01, 1000);
        receive(&sender, &[0x01; 32], e.clone(), 1000, 0, 1000, &seen, &msgs).unwrap();
        let out = receive(&sender, &[0x01; 32], e, 1000, 0, 1000, &seen, &msgs).unwrap();
        assert!(matches!(out, ReceiveOutcome::Duplicate));
        let rows = msgs.recent(&[0x01; 32], 10).unwrap();
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
        let out = receive(
            &sender,
            &[0x01; 32],
            env(0x01, old),
            now,
            0,
            now,
            &seen,
            &msgs,
        )
        .unwrap();
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
        let out = receive(
            &sender,
            &[0x01; 32],
            env(0x01, future),
            now,
            0,
            now,
            &seen,
            &msgs,
        )
        .unwrap();
        assert!(matches!(out, ReceiveOutcome::Rejected(_)));
    }

    #[test]
    fn build_ack_returns_input_id() {
        let id = MessageId([0x77; 16]);
        assert_eq!(build_ack(id), id);
    }

    #[test]
    fn receive_carries_mls_generation_and_ts_daemon_recv_into_outcome() {
        let pool = Pool::in_memory();
        let seen = SeenMessagesRepo::new(&pool);
        let msgs = MessageRepo::new(&pool);
        let sender = PublicKey([0xAA; 32]);

        let out = receive(
            &sender,
            &[0x01; 32],
            env(0x01, 1000),
            1000,          // now_ms (replay window check)
            9,             // mls_generation
            1_700_000_777, // ts_daemon_recv (seconds, local clock)
            &seen,
            &msgs,
        )
        .unwrap();

        match out {
            ReceiveOutcome::New {
                row_id,
                mls_generation,
                ts_daemon_recv,
                ..
            } => {
                assert!(row_id > 0);
                assert_eq!(mls_generation, 9);
                assert_eq!(ts_daemon_recv, 1_700_000_777);
            }
            other => panic!("expected New, got {other:?}"),
        }
    }
}
