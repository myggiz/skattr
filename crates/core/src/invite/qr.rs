// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! QR code rendering for invite links (feature-gated on `qr`).
//!
//! 1.D ships SVG only. A PNG path can be added later; for the CLI +
//! Tauri consumers we already have, SVG is sufficient.

use qrcode::render::svg;
use qrcode::{EcLevel, QrCode};

use crate::error::{CoreError, Result};
use crate::invite::{InviteErrorKind, InviteLink};

/// Render an [`InviteLink`] to SVG markup.
///
/// Error correction level is `M` (15% tolerance); invite URLs are short
/// enough that `L` is not worth the resilience tradeoff.
pub fn render_svg(invite: &InviteLink) -> Result<String> {
    let url = invite
        .to_url()
        .map_err(|e| CoreError::Invite(InviteErrorKind::Other(format!("qr: url: {e}"))))?;
    let code = QrCode::with_error_correction_level(url.as_bytes(), EcLevel::M)
        .map_err(|e| CoreError::Invite(InviteErrorKind::Other(format!("qr: encode: {e}"))))?;
    Ok(code
        .render::<svg::Color<'_>>()
        .min_dimensions(200, 200)
        .build())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::identity::IdentityKey;

    #[test]
    fn render_svg_produces_non_empty_svg_document() {
        let inviter = IdentityKey::generate().unwrap();
        let card = crate::contact::ContactCard::sign(
            &inviter,
            "a.onion".into(),
            vec![],
            1,
            86_400,
            1_000_000,
        )
        .unwrap();
        let invite = InviteLink::generate(
            &inviter,
            card,
            (0..64u8).collect(),
            [0x11; 32],
            3600,
            1_000_000,
        )
        .unwrap();
        let svg = render_svg(&invite).unwrap();
        assert!(
            svg.starts_with("<svg") || svg.contains("<svg"),
            "got: {svg}"
        );
        assert!(svg.len() > 100, "svg should be non-trivial");
    }
}
