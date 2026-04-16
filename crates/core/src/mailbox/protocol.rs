// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Wire types shared between `skattr-core` (client) and `skattr-mailbox`
//! (server).
//!
//! All messages are CBOR. The mailbox protocol is minimal by design —
//! DEPOSIT (open to anyone), CHALLENGE/FETCH (authenticated by Ed25519
//! signature over a server-issued nonce), DELETE (same auth). See
//! design §3.2.

use serde::{Deserialize, Serialize};

/// Protocol version tag. Bumped on incompatible changes.
pub const PROTOCOL_VERSION: u16 = 1;

/// Deposit a ciphertext blob for a recipient identity hash.
///
/// Open to any Tor client; no authentication required. The mailbox
/// learns only a pubkey hash, not which identity is talking to whom.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deposit {
    /// SHA-256 of the recipient's identity pubkey.
    pub recipient_hash: [u8; 32],
    /// MLS-encrypted envelope.
    pub ciphertext: Vec<u8>,
    /// Requested TTL (Unix seconds); capped by server policy.
    pub expires_at: i64,
}

/// Response to a [`Deposit`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepositAck {
    /// Server-generated deposit id (16 bytes, random).
    pub deposit_id: [u8; 16],
}

/// Challenge request: ask the server for a nonce to sign.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Challenge {
    /// The identity hash we're about to fetch for (so the server can
    /// scope the challenge and rate-limit per-identity).
    pub identity_hash: [u8; 32],
}

/// Challenge response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeNonce {
    /// 32-byte server-random nonce.
    pub nonce: [u8; 32],
    /// Server's wall-clock time; the signed payload includes this.
    pub issued_at: i64,
}

/// Fetch all pending deposits for the authenticated identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fetch {
    /// Nonce we're responding to.
    pub nonce: [u8; 32],
    /// Ed25519 signature over `nonce || issued_at_be`.
    #[serde(with = "serde_big_array::BigArray")]
    pub signature: [u8; 64],
}

/// A single pending deposit in a [`FetchResponse`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingDeposit {
    /// Server's deposit id.
    pub deposit_id: [u8; 16],
    /// Ciphertext.
    pub ciphertext: Vec<u8>,
    /// Unix timestamp when the server received this deposit.
    pub received_at: i64,
}

/// Fetch response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchResponse {
    /// Zero or more pending deposits.
    pub deposits: Vec<PendingDeposit>,
}

/// Delete request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delete {
    /// Nonce we're responding to.
    pub nonce: [u8; 32],
    /// Ed25519 signature over `nonce || deposit_ids`.
    #[serde(with = "serde_big_array::BigArray")]
    pub signature: [u8; 64],
    /// The deposits to remove.
    pub deposit_ids: Vec<[u8; 16]>,
}
