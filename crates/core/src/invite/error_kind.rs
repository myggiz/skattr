// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Typed invite-layer error kinds. Replaces free-form String payloads
//! so CoreError::kind() can project via a structural match.

use thiserror::Error;

/// Typed variants for invite-layer errors.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum InviteErrorKind {
    /// The invite's `expires_at` timestamp is in the past.
    #[error("invite expired")]
    Expired,
    /// The invite's single-use KeyPackage has already been consumed.
    #[error("invite consumed")]
    Consumed,
    /// The invite's Ed25519 signature did not verify.
    #[error("invite signature invalid")]
    SignatureInvalid,
    /// Everything else — catch-all escape hatch during the Phase 1.H
    /// refactor. Prefer adding a typed variant over populating this.
    #[error("invite: {0}")]
    Other(String),
}
