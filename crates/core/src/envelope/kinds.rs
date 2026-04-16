// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Envelope kinds.
//!
//! `Kind` is open for extension: clients ignore unknown variants rather
//! than error, to allow forward-compatible rollout of new message types.

use serde::{Deserialize, Serialize};

use crate::envelope::message::MessageId;

/// Payload discriminator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Kind {
    /// Plain UTF-8 text.
    Text {
        /// Message body.
        body: String,
    },
    /// File attachment manifest (chunk hashes live in [`Self::File::manifest`]).
    File {
        /// CBOR-encoded attachment manifest.
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
}
