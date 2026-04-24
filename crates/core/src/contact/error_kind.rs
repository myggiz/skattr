// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Typed contact-layer error kinds. Replaces free-form `String` payloads
//! so `CoreError::kind()` can project via a structural match instead of
//! `str::contains`.

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ContactErrorKind {
    /// No contact row found for the given identity key or prefix.
    #[error("contact not found")]
    NotFound,
    /// A prefix lookup matched more than one contact.
    #[error("contact ambiguous ({matches} matches)")]
    Ambiguous { matches: u32 },
    /// Everything else — catch-all escape hatch during the Phase 1.H
    /// refactor. Prefer adding a typed variant over populating this.
    #[error("contact: {0}")]
    Other(String),
}
