// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Out-of-band contact exchange via signed invite links.
//!
//! The URI scheme is `skattr://invite/v1#<params>`. Parameters are in
//! the URL fragment so that if a link leaks through a web browser or
//! chat tool, none of them end up in Referer headers or access logs.
//!
//! See design §1.4 for the exact field semantics.

pub mod link;

#[cfg(feature = "qr")]
pub mod qr;

pub use link::{InviteLink, InviteLinkBody, InvitePsk};
