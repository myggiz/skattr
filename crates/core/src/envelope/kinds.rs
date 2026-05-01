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
        /// CBOR-encoded attachment manifest (raw bytes, base64 in JSON contexts).
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
