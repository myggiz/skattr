// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used))]

//! Commands submitted into the daemon from the UI / CLI.
//!
//! This is the forward half of the daemon's public API. See
//! [`super::events`] for the reverse (events emitted by the daemon).

use serde::{Deserialize, Serialize};

use crate::daemon::hex::{Hex16, Hex32};
use crate::envelope::Kind;
use crate::identity::PublicKey;
use crate::invite::InviteLink;

/// Request sent into the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Generate a fresh invite link and surface it for display / QR.
    CreateInvite {
        /// Optional human-readable nickname embedded in the welcome UX.
        nickname: Option<String>,
        /// Optional TTL in seconds. `None` uses the default (24 h).
        #[serde(default)]
        ttl_secs: Option<u64>,
    },
    /// Consume an invite link from another user.
    AddContact {
        /// Full `skattr://invite/v1#...` URL.
        invite_url: String,
    },
    /// List every known contact with latest card + group link.
    ListContacts,
    /// Send a payload to a contact.
    SendMessage {
        /// Recipient identity pubkey.
        contact: PublicKey,
        /// Envelope payload.
        kind: Kind,
    },
    /// Return recent persisted messages, optionally filtered by contact.
    RecentMessages {
        /// If `Some`, only messages with this peer (either direction).
        contact: Option<PublicKey>,
        /// Max rows to return.
        limit: u32,
    },
    /// Start a new MLS group with the given initial members. Reserved
    /// for Phase 2; 1.F server answers `IpcError::UnknownCommand`.
    CreateGroup {
        /// Initial group members.
        members: Vec<PublicKey>,
        /// Human-readable group name.
        name: String,
    },
    /// Rotate the onion service address.
    RotateOnion,
    /// Graceful daemon shutdown.
    Shutdown,
}

/// Outcome of a `SendMessage` command after the inline-delivery wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendStatus {
    /// Hub accepted the ciphertext; ACK not seen within the inline wait.
    Queued,
    /// Hub reported delivery ACK within the inline wait.
    Delivered,
}

/// Direction of a stored message relative to the local identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Received from peer.
    Incoming,
    /// Sent to peer.
    Outgoing,
}

/// Wire-safe projection of a contact row + latest card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactSummary {
    /// Ed25519 identity pubkey.
    pub pubkey: PublicKey,
    /// User-settable local nickname.
    pub nickname: Option<String>,
    /// Onion address from the latest verified `ContactCard`.
    pub onion: String,
    /// Version of the latest known `ContactCard`.
    pub card_version: u64,
    /// Unix seconds when the contact was first added locally.
    pub added_at: u64,
}

/// Wire-safe projection of a persisted message row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRecord {
    /// 16-byte per-message id.
    pub message_id: Hex16,
    /// Peer identity pubkey.
    pub contact: PublicKey,
    /// Incoming or outgoing.
    pub direction: Direction,
    /// Envelope payload.
    pub kind: Kind,
    /// MLS generation number (0 until 1.G/2.x populate it).
    pub mls_generation: u64,
    /// Authoritative local-clock receive timestamp (unix seconds).
    pub ts_daemon_recv: u64,
    /// Sender-claimed timestamp — display only (unix seconds signed).
    pub ts_envelope: i64,
}

/// Response returned for a completed [`Command`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", content = "data", rename_all = "snake_case")]
pub enum CommandResult {
    /// The invite link for [`Command::CreateInvite`].
    InviteCreated {
        /// Canonical `skattr://invite/v1#...` URL.
        url: String,
        /// 32-byte KeyPackage hash (the single-use id).
        key_package_id: Hex32,
        /// Unix seconds when the invite expires.
        expires_at: u64,
    },
    /// [`Command::AddContact`] completed; full summary returned so the
    /// CLI can render the new contact without a follow-up query.
    ContactAdded(ContactSummary),
    /// [`Command::ListContacts`] completed.
    Contacts(Vec<ContactSummary>),
    /// [`Command::SendMessage`] completed (either Queued or Delivered).
    MessageSent {
        /// 16-byte per-message id (for correlation with later
        /// `Event::DeliveryStatusChanged`).
        message_id: Hex16,
        /// Outcome after the inline wait.
        status: SendStatus,
    },
    /// [`Command::RecentMessages`] completed. Most-recent first.
    Messages(Vec<MessageRecord>),
    /// Acknowledges a `Subscribe` request. No payload.
    Subscribed,
    /// No-payload acknowledgement (rotate, shutdown, etc.).
    Ok,
}

impl From<InviteLink> for CommandResult {
    fn from(link: InviteLink) -> Self {
        #[allow(clippy::expect_used)]
        let url = link.to_url().expect("valid InviteLink serializes cleanly");
        Self::InviteCreated {
            url,
            // The real dispatcher populates these two fields; this
            // impl stays for backward-compatibility with existing
            // test callers that only care about the URL.
            key_package_id: Hex32::from([0u8; 32]),
            expires_at: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::hex::Hex32;
    use crate::envelope::Kind;

    fn roundtrip<T>(value: &T) -> T
    where
        T: serde::Serialize + for<'de> serde::Deserialize<'de>,
    {
        let mut buf = Vec::new();
        ciborium::ser::into_writer(value, &mut buf).unwrap();
        ciborium::de::from_reader(&buf[..]).unwrap()
    }

    #[test]
    fn new_command_variants_serde_roundtrip() {
        let cmds: Vec<Command> = vec![
            Command::ListContacts,
            Command::RecentMessages {
                contact: None,
                limit: 50,
            },
            Command::RecentMessages {
                contact: Some(crate::identity::PublicKey([1; 32])),
                limit: 10,
            },
            Command::CreateInvite {
                nickname: Some("alice".into()),
                ttl_secs: Some(3600),
            },
        ];
        for cmd in &cmds {
            let _back: Command = roundtrip(cmd);
        }
    }

    #[test]
    fn new_result_variants_serde_roundtrip() {
        let results: Vec<CommandResult> = vec![
            CommandResult::Contacts(vec![ContactSummary {
                pubkey: crate::identity::PublicKey([7; 32]),
                nickname: Some("bob".into()),
                onion: "bbbb.onion".into(),
                card_version: 1,
                added_at: 1_700_000_000,
            }]),
            CommandResult::Messages(vec![MessageRecord {
                message_id: crate::daemon::hex::Hex16::from([2; 16]),
                contact: crate::identity::PublicKey([7; 32]),
                direction: Direction::Incoming,
                kind: Kind::Text { body: "hi".into() },
                mls_generation: 0,
                ts_daemon_recv: 1_700_000_100,
                ts_envelope: 1_700_000_000,
            }]),
            CommandResult::MessageSent {
                message_id: crate::daemon::hex::Hex16::from([3; 16]),
                status: SendStatus::Queued,
            },
            CommandResult::Subscribed,
            CommandResult::InviteCreated {
                url: "skattr://invite/v1#...".into(),
                key_package_id: Hex32::from([9; 32]),
                expires_at: 1_700_003_600,
            },
        ];
        for r in &results {
            let _back: CommandResult = roundtrip(r);
        }
    }
}
