// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Typed delivery-layer error kinds. Replaces free-form String payloads
//! so CoreError::kind() can project via a structural match.

use thiserror::Error;

#[derive(Debug, Error, ts_rs::TS)]
#[non_exhaustive]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
pub enum DeliveryErrorKind {
    #[error("delivery timeout")]
    Timeout,
    #[error("delivery: {0}")]
    Other(String),
}
