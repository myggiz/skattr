// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Mailbox client API used by the delivery layer.

use crate::error::Result;
use crate::mailbox::protocol::{PendingDeposit, PROTOCOL_VERSION};

/// Single-mailbox client, bound to one onion address.
pub struct MailboxClient {
    /// Onion address of the mailbox.
    pub onion: String,
}

impl MailboxClient {
    /// Connect to a mailbox and verify its protocol version.
    pub async fn connect(onion: String) -> Result<Self> {
        let _ = PROTOCOL_VERSION;
        Ok(Self { onion })
    }

    /// Register this user's identity with the mailbox (first-time
    /// setup). Returns OK if the mailbox accepts the identity.
    pub async fn register(&self) -> Result<()> {
        todo!("send Register frame, await OK")
    }

    /// Deposit an MLS ciphertext for a recipient identity hash.
    pub async fn deposit(
        &self,
        _recipient_hash: [u8; 32],
        _ciphertext: Vec<u8>,
        _expires_at: i64,
    ) -> Result<[u8; 16]> {
        todo!("CHALLENGE-less DEPOSIT; return deposit id on OK")
    }

    /// Fetch all pending deposits for our identity.
    pub async fn fetch(&self) -> Result<Vec<PendingDeposit>> {
        todo!("CHALLENGE → sign nonce → FETCH → parse response")
    }

    /// Delete a set of received deposits.
    pub async fn delete(&self, _ids: Vec<[u8; 16]>) -> Result<()> {
        todo!("CHALLENGE → sign (nonce || ids) → DELETE")
    }
}
