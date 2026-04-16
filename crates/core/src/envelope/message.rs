// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Envelope struct and CBOR serialization.

use serde::{Deserialize, Serialize};

use crate::envelope::kinds::Kind;
use crate::error::Result;

/// Random 16-byte per-message identifier.
///
/// Used for ACK correlation and receiver-side deduplication over a
/// 24-hour sliding window per sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageId(pub [u8; 16]);

impl MessageId {
    /// Generate a fresh random id.
    pub fn generate() -> Self {
        use rand::RngCore;
        let mut bytes = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        Self(bytes)
    }
}

/// Wire envelope carried inside MLS application messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// Wire format version. Currently `1`.
    pub v: u8,
    /// Per-message id.
    pub id: MessageId,
    /// Sender's wall-clock time, millis since Unix epoch.
    ///
    /// Display-only. Ordering comes from MLS generation numbers, not from
    /// this field. Receivers reject envelopes whose `ts` is more than
    /// one hour away from local clock (replay resistance).
    pub ts: i64,
    /// Optional reply target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<MessageId>,
    /// Payload discriminator and body.
    pub kind: Kind,
}

impl Envelope {
    /// CBOR-encode.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        ciborium::ser::into_writer(self, &mut out)
            .map_err(|e| crate::error::CoreError::CborEncode(e.to_string()))?;
        Ok(out)
    }

    /// CBOR-decode.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        ciborium::de::from_reader(bytes)
            .map_err(|e| crate::error::CoreError::CborDecode(e.to_string()))
    }
}
