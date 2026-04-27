// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Operator caps + token-bucket rate limiter (final shape in Task 9).

#![allow(missing_docs)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub max_deposit_size: u64,
    pub min_ttl_secs: u32,
    pub max_ttl_secs: u32,
    pub default_ttl_secs: u32,
    pub recipient_cap_bytes: u64,
    pub per_conn_deposits_per_min: u32,
    pub per_conn_fetches_per_min: u32,
    pub global_deposits_per_min: u32,
}

impl Policy {
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
}

#[derive(Debug, Default)]
pub struct TokenBucket;
