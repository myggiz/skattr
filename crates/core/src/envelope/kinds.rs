// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Envelope kinds.
//!
//! `Kind` is open for extension: clients ignore unknown variants rather
//! than error, to allow forward-compatible rollout of new message types.

use serde::{Deserialize, Serialize};

use crate::envelope::message::MessageId;

/// Payload discriminator.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
pub enum Kind {
    /// Plain UTF-8 text.
    Text {
        /// Message body.
        body: String,
    },
    /// File attachment manifest (chunk hashes live in [`Self::File::manifest`]).
    File {
        /// CBOR-encoded attachment manifest. `serde_bytes` serializes this as a
        /// CBOR byte *string* (~1 byte/byte) rather than an array of integers
        /// (~1.9 bytes/byte); without it a large attachment's manifest nearly
        /// doubles and overflows the 65519-byte transport frame cap (issue #75,
        /// ADR 0011). JSON contexts (serde_json / ts-rs) are unaffected — bytes
        /// still serialize as a number array, which the UI decoder expects.
        #[serde(with = "serde_bytes")]
        #[ts(type = "string")]
        manifest: Vec<u8>,
    },
    /// Emoji reaction to an earlier message.
    Reaction {
        /// Target message id.
        target: MessageId,
        /// The emoji, as a short UTF-8 string.
        emoji: String,
    },
    /// Edit of an earlier message.
    Edit {
        /// Target message id.
        target: MessageId,
        /// New body.
        body: String,
    },
    /// Delete-for-everyone tombstone (advisory — recipients cooperate).
    Delete {
        /// Target message id.
        target: MessageId,
    },
    /// Typing indicator (ephemeral, not persisted).
    Typing,
    /// Self-published `ContactCard` (rotation, mailbox-list change).
    /// 2.B carries these inside MLS app messages so rotation reuses the
    /// direct→mailbox fallback path with no new transport frame.
    ContactCardUpdate {
        /// Signed card carrying the new onion + mailbox list. Verified
        /// against the sender's identity by the receiver's inbound
        /// dispatcher. Boxed to keep `Kind`'s size reasonable.
        ///
        /// Skipped in TS export: the UI receives this via `Event::ContactCardReceived`
        /// and re-fetches the summary; it does not need the raw card bytes.
        #[ts(skip)]
        card: Box<crate::contact::ContactCard>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contact::card::{ContactCard, ContactCardBody};
    use crate::identity::{PublicKey, Signature};

    #[allow(clippy::unwrap_used)]
    #[test]
    fn file_manifest_cbor_encodes_as_compact_byte_string() {
        // The manifest rides in the MLS envelope as `Kind::File`. If it
        // serializes as a CBOR *array of integers* (~1.9 bytes/byte) instead
        // of a byte *string* (~1 byte/byte) it nearly doubles, and a large
        // attachment's manifest then exceeds the 65519-byte transport frame
        // cap and the send is silently dropped (issue #75). Every byte here is
        // >= 24 so the array encoding would spend 2 CBOR bytes per element,
        // making the distinction unambiguous.
        let manifest: Vec<u8> = (0..1000).map(|i| 24 + (i % 200) as u8).collect();
        let kind = Kind::File {
            manifest: manifest.clone(),
        };
        let mut buf = Vec::new();
        ciborium::into_writer(&kind, &mut buf).unwrap();

        // Byte-string encoding: ~1000 + small header + enum-tag map overhead.
        // Integer-array encoding would be ~2000+ and fail this bound.
        assert!(
            buf.len() < manifest.len() + 100,
            "manifest encoded to {} bytes for a {}-byte payload — not a compact \
             byte string (missing serde_bytes?)",
            buf.len(),
            manifest.len()
        );

        // And it must round-trip byte-identically.
        let back: Kind = ciborium::from_reader(&buf[..]).unwrap();
        assert!(
            matches!(back, Kind::File { manifest: got } if got == manifest),
            "manifest did not round-trip byte-identically",
        );
    }

    #[allow(clippy::unwrap_used)]
    #[test]
    fn file_manifest_decodes_legacy_integer_array_encoding() {
        // Backward-read compatibility (ADR 0011): `serde_bytes` on deserialize
        // accepts BOTH a CBOR byte string and a u8 sequence, so a peer on the
        // new encoding can still read a manifest sent under the old array
        // encoding. Reproduce the old wire shape via a mirror enum whose
        // `manifest` has no `serde_bytes` (thus encodes as an integer array),
        // then decode it as the real `Kind`.
        #[derive(Serialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum LegacyKind {
            File { manifest: Vec<u8> },
        }
        let manifest: Vec<u8> = (0..300).map(|i| 24 + (i % 200) as u8).collect();
        let legacy = LegacyKind::File {
            manifest: manifest.clone(),
        };
        let mut buf = Vec::new();
        ciborium::into_writer(&legacy, &mut buf).unwrap();
        // Sanity: the legacy form really is the bloated array encoding.
        assert!(
            buf.len() > manifest.len() + 200,
            "legacy form should be an array"
        );

        let back: Kind = ciborium::from_reader(&buf[..]).unwrap();
        assert!(
            matches!(back, Kind::File { manifest: got } if got == manifest),
            "manifest did not round-trip byte-identically",
        );
    }

    #[allow(clippy::unwrap_used)]
    #[test]
    fn contact_card_update_round_trips_cbor() {
        let card = ContactCard {
            body: ContactCardBody {
                identity: PublicKey([7; 32]),
                onion: "aaaa.onion".into(),
                mailboxes: vec!["bbbb.onion".into()],
                version: 3,
                expires_at: 1_700_000_000,
            },
            signature: Signature([0; 64]),
        };
        let kind = Kind::ContactCardUpdate {
            card: Box::new(card),
        };
        let mut buf = Vec::new();
        ciborium::into_writer(&kind, &mut buf).unwrap();
        let back: Kind = ciborium::from_reader(&buf[..]).unwrap();
        assert!(matches!(back, Kind::ContactCardUpdate { .. }));
    }
}
