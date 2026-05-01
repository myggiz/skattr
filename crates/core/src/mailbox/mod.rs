// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Mailbox client: offline delivery pickup.
//!
//! `core` contains only the client side. The mailbox server binary
//! lives in the `skattr-mailbox` crate and shares the wire-type
//! definitions in [`protocol`].

pub mod auth;
pub(crate) mod client;
pub(crate) mod codec;
pub(crate) mod poll;
pub mod protocol;

pub(crate) use client::{map_error, recipient_hash_from_pubkey, MailboxClient};
