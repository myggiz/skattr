// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Receiver: dedup, order, ack.
//!
//! Dedup is a sliding 24-hour `(sender, message_id)` index in SQLite.
//! Ordering is authoritative per the MLS generation number — not per the
//! envelope timestamp.

use crate::envelope::{Envelope, MessageId};
use crate::error::Result;
use crate::identity::PublicKey;

/// Outcome of processing an incoming envelope.
#[derive(Debug, Clone)]
pub enum ReceiveOutcome {
    /// Fresh message that should be surfaced to the UI.
    New(Envelope),
    /// We've seen this `(sender, message_id)` recently — dropped silently.
    Duplicate,
    /// The envelope failed validation (ts out of window, malformed, etc.).
    Rejected(String),
}

/// Process an incoming envelope from a given authenticated sender.
pub fn receive(_sender: PublicKey, _envelope: Envelope) -> Result<ReceiveOutcome> {
    todo!("check ts window, check dedup table, insert, return outcome")
}

/// Build an ACK for a message we've successfully persisted.
pub fn build_ack(message_id: MessageId) -> MessageId {
    message_id
}
