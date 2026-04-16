// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Repository for `contacts` and `onion_addresses` tables.

use crate::contact::Contact;
use crate::error::Result;
use crate::identity::PublicKey;

/// Contact repository.
#[derive(Debug)]
pub struct ContactRepo {
    _private: (),
}

impl ContactRepo {
    /// Upsert a contact (add if new, update display name/card if existing).
    pub fn upsert(&self, _contact: &Contact) -> Result<()> {
        todo!("INSERT … ON CONFLICT(identity_pubkey) DO UPDATE")
    }

    /// Look up by identity pubkey.
    pub fn get(&self, _identity: &PublicKey) -> Result<Option<Contact>> {
        todo!("SELECT * FROM contacts WHERE identity_pubkey = ?")
    }

    /// Enumerate all contacts in alphabetical order by display name.
    pub fn list(&self) -> Result<Vec<Contact>> {
        todo!("SELECT * FROM contacts ORDER BY display_name COLLATE NOCASE")
    }

    /// Delete a contact and cascade its onion addresses.
    pub fn remove(&self, _identity: &PublicKey) -> Result<()> {
        todo!("DELETE FROM contacts WHERE identity_pubkey = ?")
    }
}
