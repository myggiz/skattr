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
    /// The 2-party group is not yet fully established from our side: either we
    /// (the joiner) hold a Welcome we haven't processed, or we (the committer)
    /// committed the genesis but the invited peer has not joined/Ack'd yet.
    /// `can_send()` is false here so no app frames go to a not-yet-joined peer.
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

    /// Whether inbound messages (application messages + commits) may be
    /// processed from this state. At least every state where [`can_send`] is
    /// true; receiving is valid in `Active` (v1.0 has no other receiving
    /// state). Kept as a SEPARATE predicate from `can_send` so future states
    /// (e.g. a receive-only `CatchingUp`) can diverge without re-gating sends.
    ///
    /// [`can_send`]: Self::can_send
    #[must_use]
    pub fn can_receive(&self) -> bool {
        matches!(self, GroupState::Active { .. })
    }

    /// Whether the group is in any recoverable state.
    #[must_use]
    pub fn is_recoverable(&self) -> bool {
        !matches!(self, GroupState::Corrupt { .. })
    }
}
