// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Exponential backoff with ±25 % jitter, capped at 5 minutes.
//!
//! Used by the per-peer delivery actor when rescheduling a failed
//! delivery. Pure function; no I/O.

use std::time::Duration;

/// Base delay: 1 second.
const BASE: Duration = Duration::from_secs(1);

/// Cap at 5 minutes.
pub(crate) const CAP: Duration = Duration::from_secs(300);

/// Return the next delay for a delivery that has failed `attempts` times.
///
/// `attempts = 0` is the "we just failed for the first time" case and
/// returns approximately 1 s. Doubles each subsequent attempt up to
/// [`CAP`], then stays capped. All values are perturbed by uniform
/// random jitter in `[-25 %, +25 %]`.
#[must_use]
pub(crate) fn backoff(attempts: u32) -> Duration {
    use rand::Rng;

    // Double: 1s, 2s, 4s, … cap at 5 min. `checked_shl` guards against
    // overflow for very large `attempts` values.
    let shifted = BASE.as_millis().checked_shl(attempts).unwrap_or(u128::MAX);
    let capped_ms = u64::try_from(shifted.min(CAP.as_millis())).unwrap_or(u64::MAX);
    let base = Duration::from_millis(capped_ms);

    // ±25 % jitter. Uniform in [0.75, 1.25].
    let factor: f64 = rand::rngs::OsRng.gen_range(0.75..=1.25);
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let jittered_ms = (base.as_millis() as f64 * factor) as u64;
    Duration::from_millis(jittered_ms)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn first_attempt_is_near_one_second() {
        for _ in 0..50 {
            let d = backoff(0);
            assert!(
                d >= Duration::from_millis(750) && d <= Duration::from_millis(1250),
                "attempt 0 must be in [0.75s, 1.25s]; got {d:?}"
            );
        }
    }

    #[test]
    fn doubles_until_cap() {
        // Sample the mean of many draws to smooth out jitter.
        fn mean_ms(attempts: u32, samples: usize) -> u64 {
            let sum: u64 = (0..samples)
                .map(|_| backoff(attempts).as_millis() as u64)
                .sum();
            sum / samples as u64
        }
        let m0 = mean_ms(0, 200);
        let m1 = mean_ms(1, 200);
        let m2 = mean_ms(2, 200);
        // Means should be roughly 1000, 2000, 4000.
        assert!(m0 > 800 && m0 < 1200, "mean attempt 0 ≈ 1000 ms, got {m0}");
        assert!(m1 > 1700 && m1 < 2300, "mean attempt 1 ≈ 2000 ms, got {m1}");
        assert!(m2 > 3400 && m2 < 4600, "mean attempt 2 ≈ 4000 ms, got {m2}");
    }

    #[test]
    fn caps_at_five_minutes_plus_jitter() {
        // attempts so large the shift overflows: must still return within the
        // jittered cap band.
        for attempts in [10u32, 20, 32, 64, 100, u32::MAX] {
            let d = backoff(attempts);
            // Cap is 300s; ±25 % band is [225s, 375s].
            assert!(
                d >= Duration::from_secs(225) && d <= Duration::from_secs(375),
                "attempts={attempts}: d={d:?} outside cap band"
            );
        }
    }
}
