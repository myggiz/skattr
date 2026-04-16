// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Repository for the `mailboxes` table.
//!
//! Stores both:
//!
//! - `role = 'mine'`: mailboxes we've registered with (for inbound).
//! - `role = 'theirs'`: mailboxes learned from contact cards.

use crate::error::Result;

/// Role of a stored mailbox record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxRole {
    /// We've registered with this mailbox and poll it for inbound messages.
    Mine,
    /// Belongs to a contact; we deposit here when they're offline.
    Theirs,
}

/// Mailbox repository.
#[derive(Debug)]
pub struct MailboxRepo {
    _private: (),
}

impl MailboxRepo {
    /// Register a mailbox record.
    pub fn insert(&self, _onion: &str, _role: MailboxRole) -> Result<i64> {
        todo!("INSERT INTO mailboxes …")
    }

    /// All mailboxes of a given role.
    pub fn list(&self, _role: MailboxRole) -> Result<Vec<String>> {
        todo!("SELECT onion FROM mailboxes WHERE role = ?")
    }

    /// Remove a mailbox by onion.
    pub fn remove(&self, _onion: &str) -> Result<()> {
        todo!("DELETE FROM mailboxes WHERE onion = ?")
    }
}
