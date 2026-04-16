// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! The local contact record.

use serde::{Deserialize, Serialize};

use crate::contact::ContactCard;
use crate::identity::PublicKey;

/// Locally stored contact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    /// Long-term Ed25519 identity pubkey.
    pub identity: PublicKey,
    /// User-set nickname; not cryptographically meaningful.
    pub display_name: Option<String>,
    /// Unix timestamp when the contact was first added.
    pub added_at: i64,
    /// Most recent verified card from this contact (onion + mailboxes + version).
    pub card: Option<ContactCard>,
}
