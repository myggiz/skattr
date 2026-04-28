// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Operator caps + token-bucket rate limiter.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::error::{MailboxError, PolicyErrorKind};

/// Operator-tunable mailbox policy. Defaults from
/// [`Policy::recommended`] match the Phase 2 decomposition spec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    /// Max bytes of `ciphertext` accepted in one Deposit.
    pub max_deposit_size: u64,
    /// Lower clamp on `ttl_request` (non-zero requests below this fail).
    pub min_ttl_secs: u32,
    /// Upper clamp on `ttl_request`.
    pub max_ttl_secs: u32,
    /// Server-assigned TTL when `ttl_request == 0`.
    pub default_ttl_secs: u32,
    /// Maximum bytes stored per recipient hash.
    pub recipient_cap_bytes: u64,
    /// Token-bucket fill rate for Deposits, per connection.
    pub per_conn_deposits_per_min: u32,
    /// Token-bucket fill rate for Fetches, per connection.
    pub per_conn_fetches_per_min: u32,
    /// Server-wide token bucket for Deposits across all connections.
    pub global_deposits_per_min: u32,
}

impl Policy {
    /// Recommended defaults from the Phase 2.A design spec.
    #[must_use]
    pub fn recommended() -> Self {
        Self {
            max_deposit_size: 1_048_576,
            min_ttl_secs: 3_600,
            max_ttl_secs: 2_592_000,
            default_ttl_secs: 604_800,
            recipient_cap_bytes: 268_435_456,
            per_conn_deposits_per_min: 30,
            per_conn_fetches_per_min: 6,
            global_deposits_per_min: 1_000,
        }
    }

    /// Clamp `ttl_request` to `[min_ttl_secs, max_ttl_secs]`. `0` is
    /// shorthand for `default_ttl_secs`. Returns the resolved TTL or
    /// the appropriate [`PolicyErrorKind`].
    pub fn resolve_ttl(&self, ttl_request: u32) -> Result<u32, MailboxError> {
        if ttl_request == 0 {
            return Ok(self.default_ttl_secs);
        }
        if ttl_request < self.min_ttl_secs {
            return Err(MailboxError::Policy(PolicyErrorKind::TtlTooShort));
        }
        if ttl_request > self.max_ttl_secs {
            return Err(MailboxError::Policy(PolicyErrorKind::TtlTooLong));
        }
        Ok(ttl_request)
    }
}

/// Token-bucket rate limiter.
///
/// Refills at `tokens_per_min` over wall time; checks consume one
/// token. The bucket is monotonic in time — no "burst credit" beyond
/// the per-minute cap.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    capacity: f64,
    fill_per_sec: f64,
    available: f64,
    last_refill: f64,
}

impl TokenBucket {
    /// Construct a bucket starting full at `tokens_per_min` capacity.
    /// `now_secs` seeds the refill clock.
    #[must_use]
    pub fn new(tokens_per_min: u32, now_secs: f64) -> Self {
        let capacity = f64::from(tokens_per_min);
        Self {
            capacity,
            fill_per_sec: capacity / 60.0,
            available: capacity,
            last_refill: now_secs,
        }
    }

    /// Try to consume one token at time `now_secs`. Returns `Ok(())`
    /// on success, `Err(RateLimited)` when the bucket is empty.
    pub fn try_acquire(&mut self, now_secs: f64) -> Result<(), MailboxError> {
        let elapsed = (now_secs - self.last_refill).max(0.0);
        self.available = (self.available + elapsed * self.fill_per_sec).min(self.capacity);
        self.last_refill = now_secs;
        if self.available >= 1.0 {
            self.available -= 1.0;
            Ok(())
        } else {
            Err(MailboxError::Policy(PolicyErrorKind::RateLimited))
        }
    }

    /// Current available tokens; for tests/metrics.
    #[must_use]
    pub fn available(&self) -> f64 {
        self.available
    }
}

/// Per-connection rate limiter holding the deposit + fetch buckets.
#[derive(Debug)]
pub struct ConnRateLimiter {
    /// Token bucket for `Deposit` requests on this connection.
    pub deposits: TokenBucket,
    /// Token bucket for `Fetch` requests on this connection.
    pub fetches: TokenBucket,
}

impl ConnRateLimiter {
    /// Construct from a [`Policy`] at a given wall time.
    #[must_use]
    pub fn from_policy(p: &Policy, now_secs: f64) -> Self {
        Self {
            deposits: TokenBucket::new(p.per_conn_deposits_per_min, now_secs),
            fetches: TokenBucket::new(p.per_conn_fetches_per_min, now_secs),
        }
    }
}

/// Server-wide global token bucket, wrapped for shared mutability
/// across per-connection accept loops.
#[derive(Debug, Clone)]
pub struct GlobalRateLimiter {
    inner: Arc<Mutex<TokenBucket>>,
}

impl GlobalRateLimiter {
    /// Construct from a [`Policy`].
    #[must_use]
    pub fn from_policy(p: &Policy, now_secs: f64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(TokenBucket::new(
                p.global_deposits_per_min,
                now_secs,
            ))),
        }
    }

    /// Try to consume one global deposit token.
    pub fn try_acquire(&self, now_secs: f64) -> Result<(), MailboxError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| MailboxError::Storage(crate::error::StorageErrorKind::Poisoned))?;
        g.try_acquire(now_secs)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn resolve_ttl_accepts_zero_as_default() {
        let p = Policy::recommended();
        assert_eq!(p.resolve_ttl(0).unwrap(), p.default_ttl_secs);
    }

    #[test]
    fn resolve_ttl_rejects_below_min() {
        let p = Policy::recommended();
        let err = p.resolve_ttl(60).expect_err("must reject");
        assert!(matches!(
            err,
            MailboxError::Policy(PolicyErrorKind::TtlTooShort)
        ));
    }

    #[test]
    fn resolve_ttl_rejects_above_max() {
        let p = Policy::recommended();
        let err = p.resolve_ttl(60 * 60 * 24 * 365).expect_err("must reject");
        assert!(matches!(
            err,
            MailboxError::Policy(PolicyErrorKind::TtlTooLong)
        ));
    }

    #[test]
    fn resolve_ttl_accepts_within_bounds() {
        let p = Policy::recommended();
        assert_eq!(p.resolve_ttl(86_400).unwrap(), 86_400);
    }

    #[test]
    fn token_bucket_fills_up_to_capacity() {
        let mut b = TokenBucket::new(60, 0.0);
        for i in 0..60 {
            b.try_acquire(0.0)
                .unwrap_or_else(|_| panic!("token {i} should be available"));
        }
        let err = b.try_acquire(0.0).expect_err("must reject when empty");
        assert!(matches!(
            err,
            MailboxError::Policy(PolicyErrorKind::RateLimited)
        ));
    }

    #[test]
    fn token_bucket_refills_over_time() {
        let mut b = TokenBucket::new(60, 0.0);
        for _ in 0..60 {
            b.try_acquire(0.0).unwrap();
        }
        b.try_acquire(1.0)
            .unwrap_or_else(|_| panic!("after 1.0s @ 1 token/sec we should have a token"));
    }

    #[test]
    fn token_bucket_caps_refill_at_capacity() {
        let mut b = TokenBucket::new(60, 0.0);
        for _ in 0..30 {
            b.try_acquire(0.0).unwrap();
        }
        // 100 seconds is far past capacity; available must clamp.
        b.try_acquire(100.0).unwrap();
        assert!(
            b.available() <= 60.0,
            "available exceeded capacity: {}",
            b.available()
        );
    }

    #[test]
    fn global_limiter_shares_state_across_clones() {
        let g = GlobalRateLimiter::from_policy(&Policy::recommended(), 0.0);
        let g2 = g.clone();
        for _ in 0..1_000 {
            g.try_acquire(0.0).unwrap();
        }
        let err = g2.try_acquire(0.0).expect_err("clone shares bucket");
        assert!(matches!(
            err,
            MailboxError::Policy(PolicyErrorKind::RateLimited)
        ));
    }
}
