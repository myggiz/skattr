// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! MLS Commit construction and processing.

use crate::error::Result;
use crate::mls::Group;

/// Build a no-op self-Commit to advance the epoch for post-compromise
/// security. Called periodically (every 24 h or every 100 messages,
/// whichever first).
pub fn build_pcs_commit(_group: &mut Group) -> Result<Vec<u8>> {
    todo!("openmls propose empty Commit to ratchet epoch")
}

/// Process an incoming Commit blob, advancing the group's epoch.
///
/// If the commit is from a future epoch beyond what we can apply
/// directly, returns without error but transitions the group into
/// [`super::state_machine::GroupState::CatchingUp`]; caller is expected
/// to fetch missing Commits and replay in order.
pub fn process_commit(_group: &mut Group, _commit: &[u8]) -> Result<()> {
    todo!("MlsGroup::process_message for a Commit; update state machine")
}
