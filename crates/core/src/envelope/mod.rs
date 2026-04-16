// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Application payloads carried as MLS `application_data`.
//!
//! Wire format is canonical CBOR (via `ciborium`) — deterministic, small,
//! and fuzzing-friendly. Every envelope carries a version field, a
//! 16-byte random message id, a sender-clock timestamp, and a [`Kind`]
//! discriminator. See design §1.6.

pub mod kinds;
pub mod message;

pub use kinds::Kind;
pub use message::{Envelope, MessageId};
