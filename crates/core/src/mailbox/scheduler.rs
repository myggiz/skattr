// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Adaptive polling scheduler for mailbox fetch.
//!
//! Strategy:
//!
//! - Active interval (recent traffic): 10–30 s with ±25% jitter.
//! - Idle interval (no recent activity): up to 5 minutes with jitter.
//! - Exponential backoff to idle cap over ~15 minutes of inactivity.
//!
//! Jitter is essential — without it, a passive adversary can correlate
//! poll-arrival timing across mailboxes.

use std::time::Duration;

/// Minimum interval between polls when the user is actively engaging.
pub const ACTIVE_MIN: Duration = Duration::from_secs(10);
/// Maximum interval in the active regime.
pub const ACTIVE_MAX: Duration = Duration::from_secs(30);
/// Idle regime ceiling.
pub const IDLE_CEILING: Duration = Duration::from_secs(5 * 60);

/// Compute the next sleep duration given current conditions.
///
/// Pure function so it's unit-testable without clocks.
#[must_use]
pub fn next_interval(active: bool, consecutive_idle_polls: u32) -> Duration {
    let _ = (active, consecutive_idle_polls);
    todo!("piecewise formula with ±25% jitter")
}
