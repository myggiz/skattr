// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Commands submitted into the daemon from the UI / CLI.
//!
//! This is the forward half of the daemon's public API. See
//! [`super::events`] for the reverse (events emitted by the daemon).

use serde::{Deserialize, Serialize};

use crate::envelope::{Kind, MessageId};
use crate::identity::PublicKey;
use crate::invite::InviteLink;

/// Request sent into the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Generate a fresh invite link and surface it for display / QR.
    CreateInvite {
        /// Optional human-readable nickname embedded in the welcome UX.
        nickname: Option<String>,
    },
    /// Consume an invite link from another user.
    AddContact {
        /// Full `skattr://invite/v1#...` URL.
        invite_url: String,
    },
    /// Send a payload to a contact.
    SendMessage {
        /// Recipient identity pubkey.
        contact: PublicKey,
        /// Envelope payload.
        kind: Kind,
    },
    /// Start a new MLS group with the given initial members.
    CreateGroup {
        /// Initial group members.
        members: Vec<PublicKey>,
        /// Human-readable group name.
        name: String,
    },
    /// Rotate the onion service address.
    RotateOnion,
    /// Graceful daemon shutdown.
    Shutdown,
}

/// Response returned for a completed [`Command`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum CommandResult {
    /// The invite link for [`Command::CreateInvite`].
    InviteCreated {
        /// Canonical `skattr://invite/v1#...` URL.
        url: String,
    },
    /// [`Command::AddContact`] completed; identity pubkey returned for
    /// future references.
    ContactAdded {
        /// The contact's identity pubkey.
        contact: PublicKey,
    },
    /// [`Command::SendMessage`] accepted and enqueued; final delivery
    /// status arrives via [`super::events::Event::DeliveryStatusChanged`].
    MessageEnqueued {
        /// Message id for tracking.
        message_id: MessageId,
    },
    /// No-payload acknowledgement (rotate, shutdown, etc.).
    Ok,
}

impl From<InviteLink> for CommandResult {
    fn from(link: InviteLink) -> Self {
        #[allow(clippy::expect_used)]
        let url = link.to_url().expect("valid InviteLink serializes cleanly");
        Self::InviteCreated { url }
    }
}
