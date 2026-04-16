// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Processing of MLS Welcome messages.

use crate::error::Result;
use crate::mls::Group;

/// Consume a Welcome blob and return a newly-joined [`Group`].
///
/// Validates:
///
/// - Ciphersuite matches Skattr's locked choice.
/// - Our KeyPackage in the Welcome has not been used before (single-use).
/// - The inviter's signature key matches a known contact (if any).
pub fn process_welcome(_welcome: &[u8]) -> Result<Group> {
    todo!("openmls::MlsGroup::new_from_welcome, validate ciphersuite + KeyPackage freshness")
}
