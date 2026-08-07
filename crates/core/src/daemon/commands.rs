// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic))]

//! Commands submitted into the daemon from the UI / CLI.
//!
//! This is the forward half of the daemon's public API. See
//! [`super::events`] for the reverse (events emitted by the daemon).

use serde::{Deserialize, Serialize};

use crate::daemon::hex::{Hex16, Hex32};
use crate::envelope::Kind;
use crate::identity::PublicKey;
use crate::invite::InviteLink;

/// Mode controlling what notification body is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
pub enum NotificationMode {
    /// Sender nickname + body preview ("Alice: hey, can you...")
    #[default]
    Full,
    /// Sender only ("Alice").
    Minimal,
    /// Placeholder only ("New message").
    Generic,
    /// No notifications at all.
    Off,
}

/// Tracing log level, projected onto the wire so the UI logs viewer can
/// colour-code records. Mirrors `tracing::Level` but is `Serialize`able.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
pub enum LogLevel {
    /// Trace level.
    Trace,
    /// Debug level.
    Debug,
    /// Info level.
    Info,
    /// Warn level.
    Warn,
    /// Error level.
    Error,
}

/// One redacted log record streamed from the daemon ring buffer.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
pub struct LogRecord {
    /// Monotonic per-buffer sequence number; UI uses this as the
    /// `since_seq` cursor for incremental tail.
    pub seq: u64,
    /// Wall-clock at the time the record was emitted.
    pub ts_unix_ms: u64,
    /// Log level of this record.
    pub level: LogLevel,
    /// e.g. "skattr_core::delivery::hub"
    pub target: String,
    /// Already-redacted message body (no pubkeys / onions / message
    /// contents above the `debug` level).
    pub message: String,
}

/// Snapshot of all UI-relevant config knobs. Sensitive paths
/// (`data_dir`, `ipc_socket`) are intentionally NOT projected — the UI
/// reads them via `Command::DaemonInfo`.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
pub struct ConfigSnapshot {
    /// Message history retention in days.
    pub history_retention_days: u32,
    /// Direct peer timeout in seconds.
    pub direct_timeout_secs: u32,
    /// Notification display mode.
    pub notification_mode: NotificationMode,
    /// Whether to minimize to tray on close.
    pub close_to_tray: bool,
    /// Whether to start the app minimised.
    pub start_minimised: bool,
    /// Whether to persist logs to disk.
    pub persist_logs_to_disk: bool,
}

/// Patch sent by `Command::SetConfig`. Each field is `Option<T>`; the
/// daemon applies only `Some(_)` fields, validates each, then atomically
/// rewrites `config.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
pub struct ConfigPatch {
    /// If `Some`, update message history retention in days.
    #[serde(default)]
    pub history_retention_days: Option<u32>,
    /// If `Some`, update direct peer timeout in seconds.
    #[serde(default)]
    pub direct_timeout_secs: Option<u32>,
    /// If `Some`, update notification display mode.
    #[serde(default)]
    pub notification_mode: Option<NotificationMode>,
    /// If `Some`, update whether to minimize to tray on close.
    #[serde(default)]
    pub close_to_tray: Option<bool>,
    /// If `Some`, update whether to start the app minimised.
    #[serde(default)]
    pub start_minimised: Option<bool>,
    /// If `Some`, update whether to persist logs to disk.
    #[serde(default)]
    pub persist_logs_to_disk: Option<bool>,
    /// If `Some`, set the attachment download directory. New in 3.B.
    #[serde(default)]
    #[ts(type = "string | null")]
    pub download_dir: Option<std::path::PathBuf>,
}

/// Request sent into the daemon.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "cmd", rename_all = "snake_case")]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
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
    /// Send a file attachment to a contact. Strips metadata, chunks, stages,
    /// persists the manifest, and announces it via a `Kind::File` MLS message;
    /// chunk bytes transfer pull-driven over the direct transport (3.B).
    SendFile {
        /// Recipient identity pubkey.
        contact: PublicKey,
        /// Local filesystem path of the file to send.
        path: String,
    },
    /// Return recent persisted messages, optionally filtered by contact.
    RecentMessages {
        /// If `Some`, only messages with this peer (either direction).
        contact: Option<PublicKey>,
        /// Max rows to return.
        limit: u32,
        /// Pagination cursor — return rows with `row_id < before_id`.
        /// `None` = first page (most-recent).
        #[serde(default)]
        before_id: Option<i64>,
        /// Opt-in to the paged response variant `MessagesPage`. CLI
        /// callers omit and receive `Messages(Vec)` unchanged.
        #[serde(default)]
        paged: bool,
    },
    /// Start a new MLS group with the given initial members.
    ///
    /// Not implemented: multi-member (>2) groups are deferred to v1.1, so the
    /// daemon answers `IpcError::UnknownCommand`.
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
    /// Set or clear the local nickname for `contact`. Local-only —
    /// does not propagate to the peer's `ContactCard`.
    /// Validation: empty / whitespace-only after trim → InvalidArgument;
    /// nickname > 64 chars → InvalidArgument.
    RenameContact {
        /// Peer identity pubkey.
        contact: PublicKey,
        /// `Some(nick)` sets; `None` clears.
        nickname: Option<String>,
    },
    /// Soft-delete a contact (`contacts.hidden = 1`). MLS group state,
    /// messages, outbox, mailbox, and read-state rows are preserved.
    /// Idempotent: re-archiving a hidden contact returns `Ok`.
    RemoveContact {
        /// Peer identity pubkey.
        contact: PublicKey,
    },
    /// Like `ListContacts` but with explicit `include_hidden` opt-in.
    /// `ListContacts` (the existing unit variant) implicitly passes
    /// `include_hidden = false`.
    ListContactsWithFilter {
        /// If true, include hidden contacts.
        include_hidden: bool,
    },
    /// Read the current config snapshot.
    GetConfig,

    /// Apply a partial config patch. Daemon validates each field, then
    /// atomically rewrites config.toml. UI consumers debounce ~500ms so
    /// rapid edits don't thrash the disk.
    SetConfig {
        /// Partial config patch to apply.
        patch: ConfigPatch,
    },

    /// Re-encrypt the identity vault and storage age key under a new
    /// passphrase. Stage-then-rename atomicity; recovery on boot is
    /// deterministic. See `core::daemon::passphrase`.
    ChangePassphrase {
        /// Current passphrase. Wrapped in `Zeroizing<String>` server-side
        /// as soon as decoded.
        old: String,
        /// New passphrase. Wrapped in `Zeroizing<String>` server-side as
        /// soon as decoded.
        new: String,
    },

    /// Toggle desktop-notification + unread-badge suppression for a
    /// single contact. Persisted in `contacts.muted`.
    SetContactMuted {
        /// Peer identity pubkey.
        contact: PublicKey,
        /// If true, suppress notifications for this contact.
        muted: bool,
    },

    /// Stream the most recent log records from the in-memory ring
    /// buffer. UI consumes this on Settings → Advanced → Logs open;
    /// live-tail uses `EventFilter::Logs`.
    TailLogs {
        /// `None` = "from the oldest record currently in the buffer".
        #[serde(default)]
        since_seq: Option<u64>,
        /// Hard cap; daemon clamps to ≤ 1000.
        limit: u32,
    },

    /// Read the most recent `passphrase_audit` row's `ts_unix`.
    GetPassphraseAuditLatest,

    /// Stop accepting IPC, drop the storage Pool, remove `data_dir`,
    /// then `process::exit(0)`. Reply is sent BEFORE the teardown.
    WipeAllData,

    /// Export an encrypted backup archive of the live state to `dest_path`.
    ExportBackup {
        /// Absolute destination path for the archive.
        dest_path: String,
    },
    /// Decrypt a completed inbound attachment into the managed open-cache and
    /// return its path (the UI shell then opens it). Plaintext is ephemeral —
    /// the cache is wiped on daemon start + clean shutdown.
    OpenAttachment {
        /// 16-byte attachment id.
        attachment_id: crate::daemon::hex::Hex16,
    },
    /// Decrypt a completed inbound attachment to a user-chosen path (the
    /// intentional plaintext export).
    SaveAttachment {
        /// 16-byte attachment id.
        attachment_id: crate::daemon::hex::Hex16,
        /// Absolute destination path chosen by the user.
        dest_path: String,
    },
    /// Report whether a completed, decryptable inbound attachment exists for
    /// this id (drives UI rehydration after restart).
    AttachmentAvailable {
        /// 16-byte attachment id.
        attachment_id: crate::daemon::hex::Hex16,
    },
}

/// Outcome of a `SendMessage` command after the inline-delivery wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
pub enum SendStatus {
    /// Hub accepted the ciphertext; ACK not seen within the inline wait.
    Queued,
    /// Hub reported delivery ACK within the inline wait.
    Delivered,
}

/// Direction of a stored message relative to the local identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
pub enum Direction {
    /// Received from peer.
    Incoming,
    /// Sent to peer.
    Outgoing,
}

/// Wire-safe stringly projection of `mls::state::GroupState`.
/// Mirrors the three concrete variants in `state_machine.rs` as
/// of Phase 1.C — `Active`, `PendingJoin`, `Corrupt`. Future
/// state-machine variants extend this enum at the same time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
pub enum MlsGroupStateLabel {
    /// Group is fully established and can send/receive messages.
    Active,
    /// Awaiting the Welcome/Commit that completes group formation.
    PendingJoin,
    /// Group state is unrecoverable; user must re-add the contact.
    Corrupt,
}

/// Wire-safe projection of a contact row + latest card.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
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
    /// Number of unread messages in this contact's group, counted
    /// against the per-group `read_state` cursor. `0` for fresh
    /// contacts.
    #[serde(default)]
    pub unread_count: u64,
    /// First ≤80 Unicode code points of the latest message body.
    /// `None` when the latest message is not `Kind::Text`, or when
    /// the contact has no messages.
    #[serde(default)]
    pub last_message_preview: Option<String>,
    /// `MAX(ts_daemon_recv)` across both directions in this
    /// contact's group; `None` if zero messages.
    #[serde(default)]
    pub last_ts_recv: Option<u64>,
    /// MLS group state at summary-build time. `None` for fresh
    /// contacts whose KeyPackage exchange is in flight.
    #[serde(default)]
    pub group_state: Option<MlsGroupStateLabel>,
    /// Highest message-table `id` marked read for this contact's
    /// group (from the `read_state` cursor). UI uses this to
    /// anchor the frozen "Unread" separator at conversation-open.
    /// `None` for fresh contacts with no cursor yet.
    #[serde(default)]
    pub last_read_row_id: Option<i64>,
    /// Per-contact desktop-notification + unread-badge mute. New in
    /// 2.F. `false` for clients that don't yet honour the field.
    #[serde(default)]
    pub muted: bool,
    /// Onions advertised by the latest verified `ContactCard.body.mailboxes`
    /// for this contact. New in 2.F. Empty for contacts whose card has
    /// no mailboxes or whose card is missing.
    #[serde(default)]
    pub peer_mailboxes: Vec<String>,
    /// #107: the first-contact Welcome exceeded MAX_WELCOME_AGE without an Ack.
    /// The contact is still pending (never Active) but cannot complete — the UI
    /// prompts remove + re-invite. false for any non-pending or still-retrying
    /// contact.
    #[serde(default)]
    pub welcome_failed: bool,
}

/// Wire-safe projection of a persisted message row.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
pub struct SearchHitRecord {
    /// Underlying message row projection.
    pub record: MessageRecord,
    /// FTS5 BM25 rank score (lower = better; negative is normal for BM25).
    pub bm25: f64,
    /// FTS5-rendered snippet around the matched terms.
    pub snippet: String,
}

/// Response returned for a completed [`Command`].
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "result", content = "data", rename_all = "snake_case")]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
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
        /// Canonical sender-side `MessageRecord` projection. `None`
        /// only on the idempotent-retry branch where the original
        /// row id is not easily recoverable. UI's optimistic
        /// placeholder reconciles to `Some(record)` when present.
        #[serde(default)]
        record: Option<MessageRecord>,
    },
    /// A file attachment was staged + its manifest announced.
    FileQueued {
        /// Message id of the `Kind::File` manifest message.
        message_id: Hex16,
        /// 16-byte attachment id.
        attachment_id: Hex16,
        /// Number of chunks staged.
        total_chunks: u32,
    },
    /// [`Command::RecentMessages`] completed. Most-recent first.
    Messages(Vec<MessageRecord>),
    /// [`Command::RecentMessages`] completed with `paged: true`.
    /// Most-recent first within the page; `next_before_id` is the
    /// cursor for the next older page (`None` if this was the
    /// last page).
    MessagesPage {
        /// Message records for this page, most-recent first.
        records: Vec<MessageRecord>,
        /// Cursor for the next older page; `None` if this was the last page.
        next_before_id: Option<i64>,
    },
    /// Acknowledges a `Subscribe` request. No payload.
    Subscribed,
    /// No-payload acknowledgement (rotate, shutdown, etc.).
    Ok,
    /// Result of `RemoveContact`. `hard = true` means the contact was
    /// pending/unconnected and its local state was fully wiped (#109);
    /// `hard = false` means a connected contact was soft-archived.
    ContactRemoved {
        /// Whether the removal was a full hard wipe (pending) vs soft archive.
        hard: bool,
    },
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
    /// Reply for `Command::GetConfig`.
    Config(ConfigSnapshot),
    /// Reply for `Command::ChangePassphrase` (success).
    PassphraseChanged,
    /// Reply for `Command::TailLogs`.
    Logs {
        /// Log records, most-recent first.
        records: Vec<LogRecord>,
        /// Cursor for the next tail batch; UI uses this as `since_seq` on
        /// the next `TailLogs` call.
        next_since_seq: u64,
    },
    /// Reply for `Command::GetPassphraseAuditLatest`.
    PassphraseAudit {
        /// Unix seconds when the passphrase was last changed. `None` if
        /// never changed (i.e., still the original passphrase from init).
        last_changed_unix: Option<u64>,
    },
    /// Path of a freshly decrypted attachment in the managed open-cache.
    AttachmentDecrypted {
        /// Absolute cache path.
        path: String,
    },
    /// Availability answer for `Command::AttachmentAvailable`.
    AttachmentAvailability {
        /// True iff a completed inbound attachment exists for the id.
        available: bool,
    },
}

/// Wire-safe projection of a `mailboxes` row for CLI / UI display.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
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
                before_id: None,
                paged: false,
            },
            Command::RecentMessages {
                contact: Some(crate::identity::PublicKey([1; 32])),
                limit: 10,
                before_id: None,
                paged: false,
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
                unread_count: 0,
                last_message_preview: None,
                last_ts_recv: None,
                group_state: None,
                last_read_row_id: None,
                muted: false,
                peer_mailboxes: Vec::new(),
                welcome_failed: false,
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
                record: None,
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

    #[test]
    fn contact_summary_2c_preview_and_unread_round_trips() {
        let s = ContactSummary {
            pubkey: PublicKey([0x99; 32]),
            nickname: None,
            onion: "x.onion".into(),
            card_version: 1,
            added_at: 1_700_000_000,
            unread_count: 3,
            last_message_preview: Some("hello".into()),
            last_ts_recv: Some(1_700_000_500),
            group_state: None,
            last_read_row_id: None,
            muted: false,
            peer_mailboxes: Vec::new(),
            welcome_failed: false,
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&s, &mut buf).unwrap();
        let back: ContactSummary = ciborium::de::from_reader(&buf[..]).unwrap();
        assert_eq!(back.unread_count, 3);
        assert_eq!(back.last_message_preview.as_deref(), Some("hello"));
        assert_eq!(back.last_ts_recv, Some(1_700_000_500));
    }

    #[test]
    fn contact_summary_2d_group_state_and_read_cursor_round_trips_cbor() {
        let s = ContactSummary {
            pubkey: crate::identity::PublicKey([7; 32]),
            nickname: Some("bob".into()),
            onion: "bbbb.onion".into(),
            card_version: 1,
            added_at: 1_700_000_000,
            unread_count: 3,
            last_message_preview: Some("hi".into()),
            last_ts_recv: Some(1_700_000_500),
            group_state: Some(MlsGroupStateLabel::Active),
            last_read_row_id: Some(42),
            muted: false,
            peer_mailboxes: Vec::new(),
            welcome_failed: false,
        };
        let back: ContactSummary = roundtrip(&s);
        assert_eq!(back.pubkey.0, [7; 32]);
        assert_eq!(back.nickname.as_deref(), Some("bob"));
        assert_eq!(back.onion, "bbbb.onion");
        assert_eq!(back.card_version, 1);
        assert_eq!(back.added_at, 1_700_000_000);
        assert_eq!(back.unread_count, 3);
        assert_eq!(back.last_message_preview.as_deref(), Some("hi"));
        assert_eq!(back.last_ts_recv, Some(1_700_000_500));
        assert_eq!(back.group_state, Some(MlsGroupStateLabel::Active));
        assert_eq!(back.last_read_row_id, Some(42));
    }

    #[test]
    fn contact_summary_decodes_legacy_payload_without_new_fields() {
        // Build a CBOR map missing `group_state` / `last_read_row_id`.
        let legacy_cbor = {
            let mut buf = Vec::new();
            let v = ciborium::value::Value::Map(vec![
                (
                    "pubkey".into(),
                    ciborium::value::Value::Bytes([0u8; 32].to_vec()),
                ),
                ("nickname".into(), ciborium::value::Value::Null),
                (
                    "onion".into(),
                    ciborium::value::Value::Text("o.onion".into()),
                ),
                (
                    "card_version".into(),
                    ciborium::value::Value::Integer(0.into()),
                ),
                ("added_at".into(), ciborium::value::Value::Integer(0.into())),
            ]);
            ciborium::ser::into_writer(&v, &mut buf).unwrap();
            buf
        };
        let back: ContactSummary = ciborium::de::from_reader(&legacy_cbor[..]).unwrap();
        assert_eq!(back.group_state, None);
        assert_eq!(back.last_read_row_id, None);
    }

    #[test]
    fn contact_summary_decodes_old_payload_with_defaults() {
        #[derive(serde::Serialize)]
        struct OldShape {
            pubkey: PublicKey,
            nickname: Option<String>,
            onion: String,
            card_version: u64,
            added_at: u64,
        }
        let old = OldShape {
            pubkey: PublicKey([0x22; 32]),
            nickname: Some("legacy".into()),
            onion: "y.onion".into(),
            card_version: 7,
            added_at: 1_700_000_000,
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&old, &mut buf).unwrap();
        let back: ContactSummary = ciborium::de::from_reader(&buf[..]).unwrap();
        assert_eq!(back.unread_count, 0);
        assert!(back.last_message_preview.is_none());
        assert!(back.last_ts_recv.is_none());
        assert_eq!(back.nickname.as_deref(), Some("legacy"));
    }

    #[test]
    fn messages_page_round_trips_cbor() {
        let p = CommandResult::MessagesPage {
            records: vec![MessageRecord {
                row_id: 7,
                message_id: Hex16::from([2; 16]),
                contact: crate::identity::PublicKey([7; 32]),
                direction: Direction::Incoming,
                kind: Kind::Text { body: "hi".into() },
                mls_generation: 1,
                ts_daemon_recv: 100,
                ts_envelope: 99,
            }],
            next_before_id: Some(6),
        };
        let back: CommandResult = roundtrip(&p);
        match back {
            CommandResult::MessagesPage {
                next_before_id: Some(6),
                records,
            } => {
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].row_id, 7);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn messages_page_with_null_cursor_round_trips() {
        let p = CommandResult::MessagesPage {
            records: vec![],
            next_before_id: None,
        };
        let back: CommandResult = roundtrip(&p);
        assert!(matches!(
            back,
            CommandResult::MessagesPage {
                next_before_id: None,
                ..
            }
        ));
    }

    #[test]
    fn message_sent_with_record_round_trips() {
        let rec = MessageRecord {
            row_id: 11,
            message_id: Hex16::from([3; 16]),
            contact: crate::identity::PublicKey([4; 32]),
            direction: Direction::Outgoing,
            kind: Kind::Text { body: "hi".into() },
            mls_generation: 1,
            ts_daemon_recv: 200,
            ts_envelope: 199,
        };
        let r = CommandResult::MessageSent {
            message_id: Hex16::from([3; 16]),
            status: SendStatus::Delivered,
            record: Some(rec),
        };
        let back: CommandResult = roundtrip(&r);
        match back {
            CommandResult::MessageSent {
                status: SendStatus::Delivered,
                record: Some(rec),
                ..
            } => assert_eq!(rec.row_id, 11),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn rename_contact_command_round_trips_cbor() {
        let cmd = Command::RenameContact {
            contact: PublicKey([0x44; 32]),
            nickname: Some("Alice".into()),
        };
        let back: Command = roundtrip(&cmd);
        assert!(matches!(
            back,
            Command::RenameContact { nickname: Some(ref s), .. } if s == "Alice"
        ));
    }

    #[test]
    fn rename_contact_command_with_none_round_trips_cbor() {
        let cmd = Command::RenameContact {
            contact: PublicKey([0x55; 32]),
            nickname: None,
        };
        let back: Command = roundtrip(&cmd);
        assert!(matches!(
            back,
            Command::RenameContact { nickname: None, .. }
        ));
    }

    #[test]
    fn remove_contact_command_round_trips_cbor() {
        let cmd = Command::RemoveContact {
            contact: PublicKey([0x66; 32]),
        };
        let back: Command = roundtrip(&cmd);
        assert!(matches!(back, Command::RemoveContact { .. }));
    }

    #[test]
    fn list_contacts_with_filter_round_trips_cbor() {
        let cmd = Command::ListContactsWithFilter {
            include_hidden: true,
        };
        let back: Command = roundtrip(&cmd);
        assert!(matches!(
            back,
            Command::ListContactsWithFilter {
                include_hidden: true
            }
        ));
    }

    #[test]
    fn message_sent_legacy_payload_decodes_with_none_record() {
        // Build a CBOR `MessageSent` payload that lacks the `record` field
        // (simulating a daemon that predates this field). `message_id` is
        // serialised as a hex string by `Hex16`'s custom serde impl.
        let legacy_cbor = {
            let mut buf = Vec::new();
            let v = ciborium::value::Value::Map(vec![
                (
                    "result".into(),
                    ciborium::value::Value::Text("message_sent".into()),
                ),
                (
                    "data".into(),
                    ciborium::value::Value::Map(vec![
                        (
                            "message_id".into(),
                            ciborium::value::Value::Text("03030303030303030303030303030303".into()),
                        ),
                        (
                            "status".into(),
                            ciborium::value::Value::Text("queued".into()),
                        ),
                    ]),
                ),
            ]);
            ciborium::ser::into_writer(&v, &mut buf).unwrap();
            buf
        };
        let back: CommandResult = ciborium::de::from_reader(&legacy_cbor[..]).unwrap();
        assert!(matches!(
            back,
            CommandResult::MessageSent {
                record: None,
                status: SendStatus::Queued,
                ..
            }
        ));
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

    #[test]
    fn recent_messages_with_before_id_and_paged_round_trips() {
        fn roundtrip<T>(value: &T) -> T
        where
            T: serde::Serialize + for<'de> serde::Deserialize<'de>,
        {
            let mut buf = Vec::new();
            ciborium::ser::into_writer(value, &mut buf).unwrap();
            ciborium::de::from_reader(&buf[..]).unwrap()
        }
        let cmd = Command::RecentMessages {
            contact: Some(crate::identity::PublicKey([1; 32])),
            limit: 50,
            before_id: Some(123),
            paged: true,
        };
        let back: Command = roundtrip(&cmd);
        match back {
            Command::RecentMessages {
                before_id: Some(123),
                paged: true,
                limit: 50,
                ..
            } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn recent_messages_without_new_fields_decodes_legacy() {
        let legacy_cbor = {
            let mut buf = Vec::new();
            let v = ciborium::value::Value::Map(vec![
                (
                    "cmd".into(),
                    ciborium::value::Value::Text("recent_messages".into()),
                ),
                ("contact".into(), ciborium::value::Value::Null),
                ("limit".into(), ciborium::value::Value::Integer(50.into())),
            ]);
            ciborium::ser::into_writer(&v, &mut buf).unwrap();
            buf
        };
        let back: Command = ciborium::de::from_reader(&legacy_cbor[..]).unwrap();
        match back {
            Command::RecentMessages {
                before_id: None,
                paged: false,
                limit: 50,
                ..
            } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn config_patch_default_is_all_none() {
        let p = ConfigPatch::default();
        assert!(p.history_retention_days.is_none());
        assert!(p.notification_mode.is_none());
        assert!(p.close_to_tray.is_none());
    }

    #[test]
    fn config_patch_serde_roundtrip() {
        let p = ConfigPatch {
            history_retention_days: Some(30),
            notification_mode: Some(NotificationMode::Minimal),
            ..Default::default()
        };
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&p, &mut bytes).unwrap();
        let back: ConfigPatch = ciborium::de::from_reader(bytes.as_slice()).unwrap();
        assert_eq!(back.history_retention_days, Some(30));
        assert!(matches!(
            back.notification_mode,
            Some(NotificationMode::Minimal)
        ));
        assert!(back.close_to_tray.is_none());
    }

    #[test]
    fn notification_mode_serde_lowercase_kebab() {
        let m = NotificationMode::Generic;
        let s = serde_json::to_string(&m).unwrap();
        assert_eq!(s, "\"generic\"");
    }

    #[test]
    fn attachment_ipc_variants_serde_roundtrip() {
        fn rt<T: serde::Serialize + for<'de> serde::Deserialize<'de>>(v: &T) -> T {
            let mut buf = Vec::new();
            ciborium::ser::into_writer(v, &mut buf).unwrap();
            ciborium::de::from_reader(&buf[..]).unwrap()
        }
        use crate::daemon::hex::Hex16;
        let cmd = Command::AttachmentAvailable {
            attachment_id: Hex16::from([7u8; 16]),
        };
        let back: Command = rt(&cmd);
        assert!(matches!(back, Command::AttachmentAvailable { .. }));

        let result = CommandResult::AttachmentAvailability { available: true };
        let back: CommandResult = rt(&result);
        assert!(matches!(
            back,
            CommandResult::AttachmentAvailability { available: true }
        ));
    }
}
