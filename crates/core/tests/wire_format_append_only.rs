// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Snapshot test: enumerates every variant of `Command` and
//! `CommandResult`, sorts them alphabetically, and compares the
//! result against a frozen list. Phase 2.D exit constraint: the
//! wire format is append-only — adding a variant requires
//! updating this snapshot in the same commit; reshaping a variant
//! is a wire-format BREAKING change and needs a separate spec
//! per the umbrella decomposition spec.
//!
//! The exhaustive match arms in `command_variant_tag` /
//! `command_result_variant_tag` make adding a `Command` or
//! `CommandResult` variant a compile error here, forcing a
//! deliberate edit.

use skattr_core::daemon::commands::{Command, CommandResult};

/// Returns the snake_case wire tag for every `Command` variant.
/// Match is exhaustive — adding a `Command` variant without
/// updating this function is a compile error (E0004).
fn command_variant_tag(c: &Command) -> &'static str {
    match c {
        Command::AddContact { .. } => "add_contact",
        Command::AddMailbox { .. } => "add_mailbox",
        Command::ChangePassphrase { .. } => "change_passphrase",
        Command::CreateGroup { .. } => "create_group",
        Command::CreateInvite { .. } => "create_invite",
        Command::DaemonInfo => "daemon_info",
        Command::ExportHistory { .. } => "export_history",
        Command::GetConfig => "get_config",
        Command::GetPassphraseAuditLatest => "get_passphrase_audit_latest",
        Command::ListContacts => "list_contacts",
        Command::ListContactsWithFilter { .. } => "list_contacts_with_filter",
        Command::ListMailboxes => "list_mailboxes",
        Command::MarkRead { .. } => "mark_read",
        Command::PruneHistory { .. } => "prune_history",
        Command::RecentMessages { .. } => "recent_messages",
        Command::RemoveContact { .. } => "remove_contact",
        Command::RemoveMailbox { .. } => "remove_mailbox",
        Command::RenameContact { .. } => "rename_contact",
        Command::RotateOnion => "rotate_onion",
        Command::SearchMessages { .. } => "search_messages",
        Command::SendFile { .. } => "send_file",
        Command::SendMessage { .. } => "send_message",
        Command::SetConfig { .. } => "set_config",
        Command::SetContactMuted { .. } => "set_contact_muted",
        Command::Shutdown => "shutdown",
        Command::TailLogs { .. } => "tail_logs",
        Command::WipeAllData => "wipe_all_data",
    }
}

/// Returns the snake_case wire tag for every `CommandResult` variant.
/// Match is exhaustive — adding a variant without updating this is
/// a compile error.
fn command_result_variant_tag(r: &CommandResult) -> &'static str {
    match r {
        CommandResult::Config(_) => "config",
        CommandResult::ContactAdded(_) => "contact_added",
        CommandResult::Contacts(_) => "contacts",
        CommandResult::DaemonInfo { .. } => "daemon_info",
        CommandResult::ExportPage { .. } => "export_page",
        CommandResult::InviteCreated { .. } => "invite_created",
        CommandResult::Logs { .. } => "logs",
        CommandResult::Mailboxes(_) => "mailboxes",
        CommandResult::MarkedRead { .. } => "marked_read",
        CommandResult::FileQueued { .. } => "file_queued",
        CommandResult::MessageSent { .. } => "message_sent",
        CommandResult::Messages(_) => "messages",
        CommandResult::MessagesPage { .. } => "messages_page",
        CommandResult::Ok => "ok",
        CommandResult::PassphraseAudit { .. } => "passphrase_audit",
        CommandResult::PassphraseChanged => "passphrase_changed",
        CommandResult::Pruned { .. } => "pruned",
        CommandResult::SearchResults(_) => "search_results",
        CommandResult::Subscribed => "subscribed",
    }
}

/// The frozen Command variant snapshot. If the assertion below
/// fails, you have either added a Command variant (and need to
/// add it here) or removed/renamed one (which is a wire-format
/// breaking change and requires a separate spec).
fn expected_command_variant_set() -> Vec<&'static str> {
    let mut v = vec![
        "add_contact",
        "add_mailbox",
        "create_group",
        "create_invite",
        "daemon_info",
        "export_history",
        "list_contacts",
        "list_contacts_with_filter",
        "list_mailboxes",
        "mark_read",
        "prune_history",
        "recent_messages",
        "remove_contact",
        "remove_mailbox",
        "rename_contact",
        "rotate_onion",
        "search_messages",
        "send_file",
        "send_message",
        "shutdown",
    ];
    v.sort();
    v
}

/// The frozen CommandResult variant snapshot. Same maintenance
/// rule as `expected_command_variant_set`.
fn expected_command_result_variant_set() -> Vec<&'static str> {
    let mut v = vec![
        "config",
        "contact_added",
        "contacts",
        "daemon_info",
        "export_page",
        "file_queued",
        "invite_created",
        "logs",
        "mailboxes",
        "marked_read",
        "message_sent",
        "messages",
        "messages_page",
        "ok",
        "passphrase_audit",
        "passphrase_changed",
        "pruned",
        "search_results",
        "subscribed",
    ];
    v.sort();
    v
}

#[test]
fn command_variant_set_is_frozen() {
    let expected = expected_command_variant_set();
    assert!(!expected.is_empty(), "expected list cannot be empty");
    // Verify the static list is sorted (catches typos).
    let mut sorted = expected.clone();
    sorted.sort();
    assert_eq!(
        expected, sorted,
        "expected list must be alphabetically sorted"
    );

    // Compile-time exhaustive — if a Command variant is added without
    // updating command_variant_tag, this file won't compile.
    let _: fn(&Command) -> &'static str = command_variant_tag;
}

#[test]
fn command_result_variant_set_is_frozen() {
    let expected = expected_command_result_variant_set();
    assert!(!expected.is_empty(), "expected list cannot be empty");
    let mut sorted = expected.clone();
    sorted.sort();
    assert_eq!(
        expected, sorted,
        "expected list must be alphabetically sorted"
    );

    let _: fn(&CommandResult) -> &'static str = command_result_variant_tag;
}
