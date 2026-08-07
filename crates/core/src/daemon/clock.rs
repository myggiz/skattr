// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Clock helpers for the daemon. Hoisted from a per-module copy in
//! `daemon::inbound` + three integration-test copies. Saturates to 0 on
//! any system-clock failure so callers don't have to propagate it.

use std::time::{SystemTime, UNIX_EPOCH};

/// Unix seconds from the system clock, saturating to 0 on error.
#[must_use]
pub fn now_unix_seconds() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    )
    .unwrap_or(0)
}

/// Current UNIX time in milliseconds (display/scheduling only — not
/// authoritative ordering). Returns 0 if the clock is before the epoch.
///
/// This is the clock for the outbox `next_retry_at` scheduling column,
/// which is denominated in milliseconds — matching `delivery::peer::now_ms`
/// and `Outbox::reschedule`'s `backoff(..).as_millis()`. Do not mix with
/// [`now_unix_seconds`] when computing or comparing `next_retry_at`.
#[must_use]
pub(crate) fn now_unix_millis() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
    )
    .unwrap_or(0)
}
