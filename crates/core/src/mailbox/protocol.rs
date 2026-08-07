// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Wire types shared between `skattr-core` (client, 2.B) and
//! `skattr-mailbox` (server, 2.A).
//!
//! All messages are CBOR (canonical: sorted keys, definite lengths).
//! Frames sit inside the shared `length_u32 || type_u8 || payload`
//! framing layout (see `core::transport::frame` for the peer-to-peer
//! codec; the mailbox ships its own `MailboxFrameCodec` because the
//! type bytes are disjoint).
//!
//! # Freezing rule
//!
//! These types are frozen once Phase 2.A merges. Incompatible changes
//! ship as a parallel `MAILBOX_PROTOCOL_VERSION = 2` set; v1 stays
//! supported until v2 is universally deployed. See ADR
//! `docs/adr/0006-mailbox-protocol-v1.md`.
//!
//! # Authentication
//!
//! `Fetch` and `Delete` carry an Ed25519 signature over the
//! domain-separated string:
//!
//! ```text
//! "skattr-mailbox-auth-v1" || nonce || op_byte || sha256(canonical_cbor(payload_minus_signature))
//! ```
//!
//! - `nonce` is the 32-byte server-issued challenge.
//! - `op_byte` is the wire frame-type byte: `0x86` for [`Fetch`],
//!   `0x88` for [`Delete`].
//! - `payload_minus_signature` is the request body with the
//!   `signature` field omitted, encoded as canonical CBOR (sorted
//!   keys, definite lengths). The mailbox server computes the same
//!   digest before verifying.
//!
//! See ADR `docs/adr/0006-mailbox-protocol-v1.md` for the freeze
//! record.

use serde::{Deserialize, Serialize};

/// Protocol version tag carried on every C→S request body.
pub const PROTOCOL_VERSION: u16 = 1;

/// 16-byte server-issued opaque deposit identifier.
pub type DepositId = [u8; 16];

/// 32-byte SHA-256 of a recipient's Ed25519 identity pubkey.
pub type RecipientHash = [u8; 32];

/// 32-byte challenge nonce.
pub type Nonce = [u8; 32];

// === Deposit (C → S) ===========================================

/// Store an MLS-encrypted blob for a recipient identity hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deposit {
    /// Wire version (must equal [`PROTOCOL_VERSION`]).
    pub version: u16,
    /// SHA-256 of the recipient's identity pubkey.
    pub recipient_hash: RecipientHash,
    /// Ciphertext blob; size capped by operator policy.
    pub ciphertext: Vec<u8>,
    /// Requested TTL in seconds; clamped server-side. `0` requests the
    /// operator default.
    pub ttl_request: u32,
}

/// Server response to a successful [`Deposit`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepositOk {
    /// Server-generated 16 random bytes.
    pub deposit_id: DepositId,
    /// Final expiry timestamp (Unix seconds).
    pub expires_at: i64,
}

// === Challenge (C → S) =========================================

/// Request a server-issued nonce for a Fetch/Delete that follows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Challenge {
    /// Wire version.
    pub version: u16,
    /// SHA-256 of the identity that will sign the auth string.
    pub identity_hash: RecipientHash,
}

/// Server response carrying the nonce to sign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChallengeNonce {
    /// 32 random bytes; valid for 30 seconds.
    pub nonce: Nonce,
    /// Server wall-clock issuance time (Unix seconds).
    pub issued_at: i64,
}

// === Fetch (C → S) =============================================

/// Retrieve all pending deposits for an authenticated identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fetch {
    /// Wire version.
    pub version: u16,
    /// Recipient's Ed25519 public key (32 bytes).
    pub identity_pubkey: [u8; 32],
    /// Nonce being authenticated against.
    pub nonce: Nonce,
    /// Ed25519 signature over the auth string (see module docs).
    #[serde(with = "serde_big_array::BigArray")]
    pub signature: [u8; 64],
}

/// One pending deposit returned in [`FetchResponse`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingDeposit {
    /// Server-issued id (echo of [`DepositOk::deposit_id`]).
    pub deposit_id: DepositId,
    /// Stored ciphertext.
    pub ciphertext: Vec<u8>,
    /// When the server first received this deposit (Unix seconds).
    pub received_at: i64,
}

/// Successful response to a [`Fetch`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchResponse {
    /// Zero or more pending deposits.
    pub deposits: Vec<PendingDeposit>,
}

// === Delete (C → S) ============================================

/// Remove deposits the recipient has already acknowledged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delete {
    /// Wire version.
    pub version: u16,
    /// Recipient's Ed25519 public key.
    pub identity_pubkey: [u8; 32],
    /// Nonce being authenticated against.
    pub nonce: Nonce,
    /// Ed25519 signature over the auth string (see module docs).
    #[serde(with = "serde_big_array::BigArray")]
    pub signature: [u8; 64],
    /// Deposit ids to remove. Unknown ids count toward `not_found`.
    pub deposit_ids: Vec<DepositId>,
}

/// Successful response to a [`Delete`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteOk {
    /// Number of rows deleted.
    pub deleted: u32,
    /// Number of ids that did not match a stored deposit (already
    /// expired or never existed).
    pub not_found: u32,
}

// === Error (S → C) =============================================

/// Typed error reply. The connection is **not** closed on receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// Stable machine-readable code.
    pub code: ErrorCode,
    /// Human-readable description; never includes a hash, pubkey, or
    /// ciphertext.
    pub message: String,
}

/// Stable error taxonomy for the mailbox wire protocol. Every variant
/// has at least one triggering test in `crates/mailbox/tests/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    /// Frame version did not equal [`PROTOCOL_VERSION`].
    UnsupportedVersion,
    /// CBOR did not parse, or required fields were missing.
    MalformedRequest,
    /// Ciphertext exceeded the operator's `max_deposit_size`.
    TooLarge,
    /// Per-connection or global rate limit exhausted.
    RateLimited,
    /// Per-recipient byte cap reached and no expired rows could be
    /// reclaimed to make room.
    RecipientFull,
    /// `ttl_request` exceeded `max_ttl_secs`.
    TtlTooLong,
    /// `ttl_request` was below `min_ttl_secs` (and non-zero — zero
    /// requests the default).
    TtlTooShort,
    /// Ed25519 signature did not verify under `identity_pubkey`.
    InvalidSignature,
    /// `sha256(identity_pubkey) != recipient/identity_hash`.
    HashMismatch,
    /// Nonce was unknown or older than the 30 s TTL.
    NonceExpired,
    /// At least one `deposit_id` was unknown for this recipient (only
    /// when used as a hard error; Delete tolerates not-found via
    /// [`DeleteOk::not_found`]).
    NotFound,
    /// Server-side problem; client should retry. Never includes a
    /// recipient hash or pubkey.
    Internal,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn cbor_round_trip<T>(v: &T) -> T
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let mut buf = Vec::new();
        ciborium::into_writer(v, &mut buf).expect("encode");
        ciborium::from_reader(&buf[..]).expect("decode")
    }

    #[test]
    fn protocol_version_is_one() {
        assert_eq!(PROTOCOL_VERSION, 1);
    }

    #[test]
    fn deposit_round_trips() {
        let d = Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash: [0xAB; 32],
            ciphertext: vec![1, 2, 3],
            ttl_request: 86_400,
        };
        assert_eq!(cbor_round_trip(&d), d);
    }

    #[test]
    fn deposit_ok_round_trips() {
        let d = DepositOk {
            deposit_id: [0x42; 16],
            expires_at: 1_700_000_000,
        };
        assert_eq!(cbor_round_trip(&d), d);
    }

    #[test]
    fn challenge_round_trips() {
        let c = Challenge {
            version: PROTOCOL_VERSION,
            identity_hash: [0xCD; 32],
        };
        assert_eq!(cbor_round_trip(&c), c);
    }

    #[test]
    fn challenge_nonce_round_trips() {
        let c = ChallengeNonce {
            nonce: [0x55; 32],
            issued_at: 1_700_000_000,
        };
        assert_eq!(cbor_round_trip(&c), c);
    }

    #[test]
    fn fetch_round_trips() {
        let f = Fetch {
            version: PROTOCOL_VERSION,
            identity_pubkey: [0x11; 32],
            nonce: [0x22; 32],
            signature: [0x33; 64],
        };
        assert_eq!(cbor_round_trip(&f), f);
    }

    #[test]
    fn fetch_response_round_trips() {
        let r = FetchResponse {
            deposits: vec![PendingDeposit {
                deposit_id: [0x44; 16],
                ciphertext: vec![9, 9, 9],
                received_at: 1_700_000_000,
            }],
        };
        assert_eq!(cbor_round_trip(&r), r);
    }

    #[test]
    fn delete_round_trips() {
        let d = Delete {
            version: PROTOCOL_VERSION,
            identity_pubkey: [0x66; 32],
            nonce: [0x77; 32],
            signature: [0x88; 64],
            deposit_ids: vec![[0x99; 16], [0xAA; 16]],
        };
        assert_eq!(cbor_round_trip(&d), d);
    }

    #[test]
    fn delete_ok_round_trips() {
        let d = DeleteOk {
            deleted: 3,
            not_found: 1,
        };
        assert_eq!(cbor_round_trip(&d), d);
    }

    #[test]
    fn error_body_round_trips() {
        let e = ErrorBody {
            code: ErrorCode::RateLimited,
            message: "slow down".into(),
        };
        assert_eq!(cbor_round_trip(&e), e);
    }

    #[test]
    fn every_error_code_round_trips() {
        for code in [
            ErrorCode::UnsupportedVersion,
            ErrorCode::MalformedRequest,
            ErrorCode::TooLarge,
            ErrorCode::RateLimited,
            ErrorCode::RecipientFull,
            ErrorCode::TtlTooLong,
            ErrorCode::TtlTooShort,
            ErrorCode::InvalidSignature,
            ErrorCode::HashMismatch,
            ErrorCode::NonceExpired,
            ErrorCode::NotFound,
            ErrorCode::Internal,
        ] {
            let e = ErrorBody {
                code,
                message: String::new(),
            };
            assert_eq!(cbor_round_trip(&e), e);
        }
    }
}
