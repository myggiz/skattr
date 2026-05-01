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
    /// Full-text search over persisted messages.
    SearchMessages {
        /// Free-form FTS5 query string. Whitespace-only queries
        /// short-circuit to an empty `SearchResults(vec![])` without
        /// hitting FTS5. Malformed queries that reach FTS5 and the
        /// engine rejects surface as `DaemonErrorKind::SearchSyntax`.
        query: String,
        /// If `Some`, restrict results to messages exchanged with this peer.
        contact: Option<crate::identity::PublicKey>,
        /// Max rows to return (page size).
        limit: u32,
        /// Number of leading rows to skip (paging cursor).
        offset: u32,
        /// If `true`, sort newest-first by `ts_daemon_recv`; otherwise FTS rank.
        newest_first: bool,
    },
    /// Advance the per-contact read cursor up to and including
    /// `up_to_message_id` (the message-table primary key, not the
    /// 16-byte wire id).
    MarkRead {
        /// Peer whose conversation cursor is being advanced.
        contact: crate::identity::PublicKey,
        /// Message-table `id` (primary key) up to which messages are
        /// considered read.
        up_to_message_id: i64,
    },
    /// Delete persisted messages matching the given retention rule.
    /// Exactly one of `before_ts_recv` / `keep_last` is expected.
    PruneHistory {
        /// If `Some`, only prune within this peer's conversation.
        contact: Option<crate::identity::PublicKey>,
        /// Delete messages with `ts_daemon_recv < this`. Unix seconds.
        before_ts_recv: Option<i64>,
        /// Keep at most this many most-recent rows per conversation.
        keep_last: Option<u64>,
    },
    /// Register a `'mine'` mailbox: probe it for liveness, persist it
    /// as `Reachable`, notify the `PollScheduler`, and republish the
    /// self-card carrying the new mailbox onion to every contact.
    AddMailbox {
        /// Operator's onion address (without `:port` suffix).
        onion: String,
    },
    /// Remove a previously registered `'mine'` mailbox. Marks the row
    /// `pending_removal`, attempts a best-effort final drain (fetch +
    /// server-side delete), then marks `removed`, stops the poll actor,
    /// and republishes the self-card so contacts stop depositing there.
    RemoveMailbox {
        /// Primary-key `id` of the mailbox row to remove.
        id: i64,
    },
    /// List every `'mine'` mailbox row with its current status.
    ListMailboxes,
    /// Return runtime metadata for the UI's About screen + first-paint
    /// store hydration: identity pubkey, current onion (None until
    /// Tor bootstraps), daemon version, schema version.
    DaemonInfo,
    /// Export a paged window of persisted messages for the given peer.
    ExportHistory {
        /// Peer whose history to export.
        contact: crate::identity::PublicKey,
        /// Page cursor: return rows with `id > after_id`. `None` starts
        /// from the beginning.
        after_id: Option<i64>,
        /// Max rows per page.
        limit: u32,
    },
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
    /// SQLite primary key — stable within one node; used by UI for scroll
    /// anchoring, mark_read cursor targeting, and trace correlation.
    pub row_id: i64,
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

impl MessageRecord {
    /// Project a stored row + decrypt-time metadata into the wire type.
    ///
    /// `direction` is `Incoming` for receiver-side rows, `Outgoing` for
    /// sender-side. `mls_generation` is the post-encrypt/post-decrypt
    /// epoch. `ts_daemon_recv` is the local clock at persist time. Both
    /// are carried straight to the wire — no aliasing back to `envelope.ts`.
    ///
    /// `row_id` is the SQLite primary key surfaced for UI scroll anchoring,
    /// mark_read cursor targeting, and trace correlation. `contact` is the
    /// peer pubkey.
    pub fn project(
        row_id: i64,
        envelope: &crate::envelope::Envelope,
        contact: crate::identity::PublicKey,
        mls_generation: u64,
        ts_daemon_recv: i64,
        direction: Direction,
    ) -> Self {
        Self {
            row_id,
            message_id: Hex16::from(envelope.id.0),
            contact,
            direction,
            kind: envelope.kind.clone(),
            mls_generation,
            ts_daemon_recv: u64::try_from(ts_daemon_recv).unwrap_or(0),
            ts_envelope: envelope.ts,
        }
    }
}

/// One full-text search hit returned by `Command::SearchMessages`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHitRecord {
    /// Underlying message row projection.
    pub record: MessageRecord,
    /// FTS5 BM25 rank score (lower = better; negative is normal for BM25).
    pub bm25: f64,
    /// FTS5-rendered snippet around the matched terms.
    pub snippet: String,
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
    /// [`Command::SearchMessages`] completed.
    SearchResults(Vec<SearchHitRecord>),
    /// [`Command::MarkRead`] completed.
    MarkedRead {
        /// Message-table `id` (primary key) of the highest message marked read.
        up_to: i64,
    },
    /// [`Command::PruneHistory`] completed.
    Pruned {
        /// Number of message rows actually deleted.
        rows_deleted: u64,
    },
    /// One page of [`Command::ExportHistory`] results.
    ExportPage {
        /// Records in this page.
        records: Vec<MessageRecord>,
        /// Cursor for the next page; `None` if this was the last page.
        next_after_id: Option<i64>,
    },
    /// [`Command::ListMailboxes`] completed.
    Mailboxes(Vec<MailboxSummary>),
    /// [`Command::DaemonInfo`] completed.
    DaemonInfo {
        /// Local Ed25519 identity pubkey.
        local_pubkey: PublicKey,
        /// Current v3 onion address (without `:port`). `None` while
        /// Tor is still bootstrapping.
        current_onion: Option<String>,
        /// `env!("CARGO_PKG_VERSION")` of `skattr-core`.
        daemon_version: String,
        /// Latest applied storage migration version.
        schema_version: u32,
    },
}

/// Wire-safe projection of a `mailboxes` row for CLI / UI display.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MailboxSummary {
    /// SQLite primary key of the `mailboxes` row.
    pub id: i64,
    /// Onion address (without `:port`).
    pub onion: String,
    /// Current lifecycle status.
    pub status: crate::storage::MailboxStatus,
    /// Unix seconds when the row was first created.
    pub registered_at: u64,
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
    fn message_record_project_uses_real_columns_not_placeholders() {
        use crate::envelope::{Envelope, Kind, MessageId};
        use crate::identity::PublicKey;

        let env = Envelope {
            v: 1,
            id: MessageId([0xAA; 16]),
            ts: 1_700_000_000,
            reply_to: None,
            kind: Kind::Text { body: "hi".into() },
        };
        let contact = PublicKey([0x33; 32]);

        let rec = MessageRecord::project(
            42, // row id (not used on the wire — only for tracing)
            &env,
            contact,
            7,             // mls_generation (must be carried, not zeroed)
            1_700_000_500, // ts_daemon_recv (must be carried, not aliased to env.ts)
            Direction::Incoming,
        );

        assert_eq!(rec.mls_generation, 7);
        assert_eq!(rec.ts_daemon_recv, 1_700_000_500);
        assert_eq!(rec.ts_envelope, 1_700_000_000);
        assert!(matches!(rec.direction, Direction::Incoming));
        assert!(matches!(rec.kind, Kind::Text { .. }));
        assert_eq!(rec.contact.0, [0x33; 32]);
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
                row_id: 0, // row_id irrelevant in this test
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

    #[test]
    fn daemon_info_command_round_trips_cbor() {
        let cmd = Command::DaemonInfo;
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&cmd, &mut buf).unwrap();
        let back: Command = ciborium::de::from_reader(&buf[..]).unwrap();
        assert!(matches!(back, Command::DaemonInfo));
    }

    #[test]
    fn daemon_info_result_round_trips_cbor() {
        let r = CommandResult::DaemonInfo {
            local_pubkey: PublicKey([0xAB; 32]),
            current_onion: Some("abcd.onion".into()),
            daemon_version: "0.0.1".into(),
            schema_version: 9,
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&r, &mut buf).unwrap();
        let back: CommandResult = ciborium::de::from_reader(&buf[..]).unwrap();
        match back {
            CommandResult::DaemonInfo {
                current_onion,
                schema_version,
                ..
            } => {
                assert_eq!(current_onion.as_deref(), Some("abcd.onion"));
                assert_eq!(schema_version, 9);
            }
            other => panic!("expected DaemonInfo, got {other:?}"),
        }
    }

    #[test]
    fn daemon_info_result_with_none_onion_round_trips() {
        let r = CommandResult::DaemonInfo {
            local_pubkey: PublicKey([0xCD; 32]),
            current_onion: None,
            daemon_version: "0.0.1".into(),
            schema_version: 9,
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&r, &mut buf).unwrap();
        let back: CommandResult = ciborium::de::from_reader(&buf[..]).unwrap();
        match back {
            CommandResult::DaemonInfo { current_onion, .. } => {
                assert!(current_onion.is_none());
            }
            other => panic!("expected DaemonInfo, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod phase_1g_wire_tests {
    use super::*;

    #[test]
    fn search_messages_command_round_trips_cbor() {
        let cmd = Command::SearchMessages {
            query: "alpha bravo".into(),
            contact: None,
            limit: 20,
            offset: 0,
            newest_first: false,
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&cmd, &mut buf).unwrap();
        let back: Command = ciborium::de::from_reader(&buf[..]).unwrap();
        assert!(matches!(back, Command::SearchMessages { .. }));
    }

    #[test]
    fn mark_read_command_round_trips_cbor() {
        let cmd = Command::MarkRead {
            contact: crate::identity::PublicKey([0x11; 32]),
            up_to_message_id: 42,
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&cmd, &mut buf).unwrap();
        let back: Command = ciborium::de::from_reader(&buf[..]).unwrap();
        assert!(matches!(
            back,
            Command::MarkRead {
                up_to_message_id: 42,
                ..
            }
        ));
    }

    #[test]
    fn prune_history_command_round_trips_cbor() {
        let cmd = Command::PruneHistory {
            contact: None,
            before_ts_recv: Some(1_700_000_000),
            keep_last: None,
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&cmd, &mut buf).unwrap();
        let back: Command = ciborium::de::from_reader(&buf[..]).unwrap();
        assert!(matches!(back, Command::PruneHistory { .. }));
    }

    #[test]
    fn export_history_command_round_trips_cbor() {
        let cmd = Command::ExportHistory {
            contact: crate::identity::PublicKey([0x22; 32]),
            after_id: Some(100),
            limit: 1000,
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&cmd, &mut buf).unwrap();
        let back: Command = ciborium::de::from_reader(&buf[..]).unwrap();
        assert!(matches!(
            back,
            Command::ExportHistory {
                after_id: Some(100),
                ..
            }
        ));
    }

    #[test]
    fn search_results_round_trips() {
        let res = CommandResult::SearchResults(vec![]);
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&res, &mut buf).unwrap();
        let back: CommandResult = ciborium::de::from_reader(&buf[..]).unwrap();
        assert!(matches!(back, CommandResult::SearchResults(_)));
    }
}
