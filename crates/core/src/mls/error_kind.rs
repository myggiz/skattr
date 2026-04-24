// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Typed MLS-layer error kinds. Replaces free-form String payloads so
//! CoreError::kind() can project via a structural match.

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MlsErrorKind {
    #[error("group corrupt")]
    GroupCorrupt,
    #[error("mls: {0}")]
    Other(String),
}
