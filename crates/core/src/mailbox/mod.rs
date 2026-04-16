// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Mailbox client: offline delivery pickup.
//!
//! `core` contains only the client side. The mailbox server binary
//! lives in the `skattr-mailbox` crate and shares the wire-type
//! definitions in [`protocol`].

pub(crate) mod client;
pub mod protocol;
pub(crate) mod scheduler;

pub(crate) use client::MailboxClient;
