// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! QR code rendering for invite links (feature-gated on `qr`).

use crate::error::Result;
use crate::invite::InviteLink;

/// Render an [`InviteLink`] to SVG.
///
/// Error correction level is `M` (15% tolerance) — invites are short so
/// we don't need `L`.
pub fn render_svg(_invite: &InviteLink) -> Result<String> {
    todo!("qrcode::QrCode::new(link.to_url()).render()")
}

/// Render an [`InviteLink`] to a PNG byte buffer.
pub fn render_png(_invite: &InviteLink) -> Result<Vec<u8>> {
    todo!("render via qrcode crate + png encoder")
}
