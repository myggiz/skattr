// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Events emitted by the daemon to subscribers.

use serde::{Deserialize, Serialize};

use crate::envelope::{Envelope, MessageId};
use crate::identity::PublicKey;

/// Tor-layer status, surfaced for UI bootstrap progress bars.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TorStatus {
    /// Bootstrap has not started.
    Idle,
    /// Bootstrapping; percentage 0–100.
    Bootstrapping(u8),
    /// Fully ready, onion published.
    Ready,
    /// Terminal failure.
    Failed(String),
}

/// Per-message delivery outcome.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DeliveryStatus {
    /// Queued; first attempt has not fired yet.
    Queued,
    /// Sent directly to the peer and acknowledged.
    Delivered,
    /// Deposited to one or more of the recipient's mailboxes.
    Deposited,
    /// Giving up after exhausting retries.
    Failed(String),
}

/// Event emitted by the daemon.
///
/// Adjacently tagged (`tag = "event", content = "data"`) so that
/// ciborium can round-trip newtype variants (e.g. `TorStatusChanged`)
/// without hitting the internally-tagged / non-map-value limitation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum Event {
    /// Bootstrap progress / final status.
    TorStatusChanged(TorStatus),
    /// A message arrived from `from` and has been persisted.
    MessageReceived {
        /// Sender identity pubkey.
        from: PublicKey,
        /// Decrypted payload.
        envelope: Envelope,
    },
    /// A contact's card / nickname / online state changed.
    ContactUpdated(PublicKey),
    /// An outbound message's delivery state changed.
    DeliveryStatusChanged {
        /// Message id, mirrors the one returned when the send was queued.
        message: MessageId,
        /// Current status.
        status: DeliveryStatus,
    },
}
