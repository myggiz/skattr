// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Contacts and their routing info.
//!
//! A [`Contact`] is the local view of someone we can talk to. It holds
//! their identity pubkey, a human-readable nickname, and the most recent
//! [`card::ContactCard`] we've verified — the card is the signed
//! self-published routing record (onion address, mailboxes, version).

pub mod card;
// Spec names this file `contact.rs` under `contact/`; clippy's
// module_inception lint doesn't like the layout but the prompt is explicit.
#[allow(clippy::module_inception)]
pub mod contact;
pub mod rotation;

pub use card::ContactCard;
pub use contact::Contact;
