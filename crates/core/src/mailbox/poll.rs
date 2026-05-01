// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Adaptive polling scheduler for our mailboxes.
//!
//! Per-mailbox actor with an Idle (60 s) ↔ Active (15 s) state machine.
//! ±25 % jitter per tick to break timing correlation across mailboxes.
//! Idle ceiling = 5 min when a mailbox is `Unreachable`.

use std::time::Duration;

use rand::Rng;

pub(crate) const ACTIVE_BASE: Duration = Duration::from_secs(15);
pub(crate) const IDLE_BASE: Duration = Duration::from_secs(60);
pub(crate) const IDLE_CEILING: Duration = Duration::from_secs(5 * 60);
pub(crate) const ACTIVE_HOLD: Duration = Duration::from_secs(5 * 60);

/// Compute the next sleep before the per-mailbox actor's next tick.
///
/// Pure function — Task 14 wraps the per-mailbox actor around it.
#[must_use]
pub(crate) fn next_interval(active: bool, unreachable: bool, rng: &mut impl Rng) -> Duration {
    let base = match (active, unreachable) {
        (_, true) => IDLE_CEILING,
        (true, false) => ACTIVE_BASE,
        (false, false) => IDLE_BASE,
    };
    let nanos = base.as_nanos() as i128;
    let jitter_range: i128 = nanos / 4; // ±25 %
    let delta = rng.gen_range(-jitter_range..=jitter_range);
    let out = (nanos + delta).max(0) as u64;
    Duration::from_nanos(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rand::{rngs::StdRng, SeedableRng};

    #[test]
    fn active_interval_within_active_band() {
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..1000 {
            let d = next_interval(true, false, &mut rng);
            assert!(
                d >= Duration::from_millis(11_250) && d <= Duration::from_millis(18_750),
                "active out of band: {d:?}"
            );
        }
    }

    #[test]
    fn idle_interval_within_idle_band() {
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..1000 {
            let d = next_interval(false, false, &mut rng);
            assert!(
                d >= Duration::from_millis(45_000) && d <= Duration::from_millis(75_000),
                "idle out of band: {d:?}"
            );
        }
    }

    #[test]
    fn unreachable_interval_locks_to_idle_ceiling() {
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..100 {
            let d = next_interval(false, true, &mut rng);
            assert!(
                d >= Duration::from_millis(225_000) && d <= Duration::from_millis(375_000)
            );
        }
    }

    #[test]
    fn active_overrides_unreachable_ceiling() {
        // Even with `active=true`, if the actor is `unreachable=true` the ceiling wins.
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..100 {
            let d = next_interval(true, true, &mut rng);
            assert!(d >= Duration::from_millis(225_000));
        }
    }
}
