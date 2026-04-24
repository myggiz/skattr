// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

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
