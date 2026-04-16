// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Onion / mailbox address rotation.
//!
//! Rotation happens by publishing a newer [`super::ContactCard`] with a
//! higher `version`. During a grace period the old onion service remains
//! published so in-flight connections don't break; after the grace
//! period it is unpublished and its HS key is deleted.

use std::time::Duration;

use crate::error::Result;

/// Default grace period during which the old onion stays published.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(24 * 60 * 60);

/// Kick off a rotation: generate new HS key, publish, queue new card to contacts.
///
/// Returns the address of the new onion once it's published.
pub async fn rotate_onion(_grace: Duration) -> Result<String> {
    todo!("generate new HS key, publish, emit new ContactCard v+1, schedule old teardown")
}
