// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used))]

//! Events emitted by the daemon to subscribers.

use serde::{Deserialize, Serialize};

use crate::envelope::MessageId;
use crate::identity::PublicKey;

pub use crate::storage::mailboxes::MailboxStatus;

/// Tor-layer status, surfaced for UI bootstrap progress bars.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
pub enum Event {
    /// Bootstrap progress / final status.
    TorStatusChanged(TorStatus),
    /// A message arrived from a peer, was decrypted, and has been
    /// persisted. The emitted `record` is the canonical
    /// `MessageRecord` projection — already carrying the authoritative
    /// `mls_generation` and local-clock `ts_daemon_recv` — so the
    /// `tail --follow` / `chat` renderers don't have to re-derive the
    /// wire shape themselves. `contact` is the peer pubkey (same as
    /// the sender in 2-member groups).
    MessageReceived {
        /// Peer identity pubkey.
        contact: PublicKey,
        /// Canonical message-row projection at receive time.
        record: crate::daemon::commands::MessageRecord,
    },
    /// A contact's card / nickname / online state changed.
    ContactUpdated(PublicKey),
    /// A contact was removed. For a pending/unconnected contact this is a
    /// full local wipe (#109); for a connected contact it is a soft-archive.
    /// Live UIs should drop the row on this event rather than re-fetch.
    ContactRemoved(PublicKey),
    /// An outbound message's delivery state changed.
    DeliveryStatusChanged {
        /// Message id, mirrors the one returned when the send was queued.
        message: MessageId,
        /// Current status.
        status: DeliveryStatus,
    },
    /// One of our `'mine'` mailboxes changed reachability/auth status.
    MailboxStatusChanged {
        /// Row id from the `mailboxes` table.
        mailbox_id: i64,
        /// New status.
        status: MailboxStatus,
    },
    /// A peer published a higher-version `ContactCard`. UI re-fetches the
    /// contact summary on receipt.
    ContactCardReceived {
        /// Peer identity pubkey.
        contact: PublicKey,
        /// New monotonic version (always strictly greater than the
        /// previously-stored card's version, per `ContactRepo::put_card`).
        version: u64,
    },
    /// One redacted log record. Streamed only when the subscriber's
    /// filter includes `EventFilter::Logs`.
    LogRecord(crate::daemon::commands::LogRecord),
    /// An inbound attachment finished transferring and is available (encrypted at rest).
    AttachmentReceived {
        /// Sending peer.
        contact: PublicKey,
        /// 16-byte attachment id.
        attachment_id: crate::daemon::hex::Hex16,
        /// Sanitized filename.
        filename: String,
        /// Effective MIME type (post-strip).
        mime: String,
        /// File size in bytes.
        size: u64,
    },
    /// Incremental attachment transfer progress (throttled).
    AttachmentProgress {
        /// 16-byte attachment id.
        attachment_id: crate::daemon::hex::Hex16,
        /// Chunks received so far.
        received: u32,
        /// Total chunks.
        total: u32,
    },
    /// An attachment transfer failed (retry budget exhausted, hard nack).
    AttachmentFailed {
        /// 16-byte attachment id.
        attachment_id: crate::daemon::hex::Hex16,
        /// Human-readable, non-sensitive reason.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_status_changed_round_trips_cbor() {
        let e = Event::MailboxStatusChanged {
            mailbox_id: 42,
            status: MailboxStatus::Reachable,
        };
        let mut buf = Vec::new();
        ciborium::into_writer(&e, &mut buf).unwrap();
        let back: Event = ciborium::from_reader(&buf[..]).unwrap();
        assert!(matches!(
            back,
            Event::MailboxStatusChanged { mailbox_id: 42, .. }
        ));
    }

    #[test]
    fn contact_card_received_round_trips_cbor() {
        let e = Event::ContactCardReceived {
            contact: PublicKey([7; 32]),
            version: 5,
        };
        let mut buf = Vec::new();
        ciborium::into_writer(&e, &mut buf).unwrap();
        let back: Event = ciborium::from_reader(&buf[..]).unwrap();
        assert!(matches!(
            back,
            Event::ContactCardReceived { version: 5, .. }
        ));
    }
}
