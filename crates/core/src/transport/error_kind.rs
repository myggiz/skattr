// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Typed transport-layer error kinds. Replaces free-form String payloads
//! so CoreError::kind() can project via a structural match.

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TransportErrorKind {
    #[error("tor not ready")]
    TorNotReady,
    #[error("transport: {0}")]
    Other(String),
}
