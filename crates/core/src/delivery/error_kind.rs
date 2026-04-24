// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Typed delivery-layer error kinds. Replaces free-form String payloads
//! so CoreError::kind() can project via a structural match.

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DeliveryErrorKind {
    #[error("delivery timeout")]
    Timeout,
    #[error("delivery: {0}")]
    Other(String),
}
