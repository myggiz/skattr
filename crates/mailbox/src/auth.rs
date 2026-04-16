// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Challenge-response authentication for FETCH / DELETE.
//!
//! We never accept a raw pubkey as an identity claim; clients must sign
//! a server-issued nonce. This binds each FETCH to a specific server
//! session and defends against replay.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;

/// How long a challenge nonce stays valid.
pub const CHALLENGE_TTL: Duration = Duration::from_secs(30);

/// Pending-challenge table; keyed by the 32-byte nonce.
#[derive(Debug, Default)]
pub struct Challenges {
    // nonce → (identity_hash, issued_at_unix_seconds)
    inner: HashMap<[u8; 32], ([u8; 32], i64)>,
}

impl Challenges {
    /// Mint a new challenge nonce for a recipient hash.
    pub fn issue(&mut self, _identity_hash: [u8; 32]) -> [u8; 32] {
        todo!("Phase 2")
    }

    /// Consume a challenge: verify the signature, drop the entry on success.
    pub fn verify(
        &mut self,
        _nonce: &[u8; 32],
        _signature: &[u8; 64],
        _expected_identity_pubkey: &[u8; 32],
    ) -> Result<()> {
        todo!("Phase 2")
    }

    /// Evict expired challenges. Call periodically.
    pub fn sweep(&mut self, _now: i64) -> u64 {
        todo!("Phase 2")
    }
}
