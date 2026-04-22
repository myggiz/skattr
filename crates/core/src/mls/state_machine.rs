// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Explicit group lifecycle states.
//!
//! MLS is stateful and unforgiving. Modeling the phases explicitly lets
//! us reject operations on a group that isn't in a valid state and gives
//! the UI something meaningful to display instead of raw OpenMLS errors.
//!
//! Phase 1.C restricts the enum to three variants. `PendingCommit`,
//! `CatchingUp`, and `Removed` land in 1.E (delivery) / Phase 2
//! (multi-member) when they're actually reachable.

/// Lifecycle of a single MLS group from our perspective.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupState {
    /// Steady-state: the group has an accepted epoch and we can send.
    Active {
        /// Current MLS epoch number.
        epoch: u64,
    },
    /// We have a Welcome but haven't processed it yet.
    PendingJoin,
    /// State is irrecoverably corrupt; only option is recreate.
    Corrupt {
        /// Human-readable, non-sensitive reason.
        reason: String,
    },
}

impl GroupState {
    /// Whether sends are permitted from this state.
    #[must_use]
    pub fn can_send(&self) -> bool {
        matches!(self, GroupState::Active { .. })
    }

    /// Whether the group is in any recoverable state.
    #[must_use]
    pub fn is_recoverable(&self) -> bool {
        !matches!(self, GroupState::Corrupt { .. })
    }
}
