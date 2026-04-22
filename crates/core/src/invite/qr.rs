// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! QR code rendering for invite links (feature-gated on `qr`).
//!
//! 1.D ships SVG only. A PNG path can be added later; for the CLI +
//! Tauri consumers we already have, SVG is sufficient.

use crate::error::Result;
use crate::invite::InviteLink;

/// Render an [`InviteLink`] to SVG markup.
///
/// Error correction level is `M` (15% tolerance); invite URLs are short
/// enough that `L` is not worth the resilience tradeoff.
pub fn render_svg(_invite: &InviteLink) -> Result<String> {
    todo!("Task 11")
}
