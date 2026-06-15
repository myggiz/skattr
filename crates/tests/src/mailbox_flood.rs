// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 2.D exit criterion: the mailbox server bounds disk under an
//! anonymous flood and a targeted victim-fill, and a fresh legit deposit
//! still lands once expiry frees space (no permanent eviction/lockout).
//!
//! This covers the global storage-cap + after-expiry path. The other 2.D
//! limits are exercised by per-task unit tests: connection-semaphore shed
//! (`server::tests::serve_connection_sheds_when_at_capacity`), idle timeout
//! (`server::tests::idle_connection_is_closed_after_timeout`), bounded
//! `Delete` (`dispatch::tests::delete_rejects_oversize_id_list`), and the
//! daemon accept-loop bound (`daemon::accept::tests`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use skattr_mailbox::{Policy, Store};

#[tokio::test]
async fn global_cap_bounds_disk_under_flood_and_legit_deposit_lands_after_expiry() {
    let mut policy = Policy::recommended();
    policy.global_storage_cap_bytes = 4096;
    policy.recipient_cap_bytes = 4096;
    policy.max_recipients = 1000;
    let store = Arc::new(Store::in_memory().expect("in-memory store"));

    // Flood: many recipients, short TTL (expires_at = 1000), now = 100.
    let mut accepted = 0u32;
    for i in 0..200u32 {
        let mut rec = [0u8; 32];
        rec[..4].copy_from_slice(&i.to_be_bytes());
        if store
            .insert(
                rec,
                vec![0xAB; 512],
                100,
                1000,
                policy.recipient_cap_bytes,
                policy.global_storage_cap_bytes,
                policy.max_recipients,
                100,
            )
            .is_ok()
        {
            accepted += 1;
        }
    }
    assert!(
        (1..=8).contains(&accepted),
        "global cap bounds accepted deposits (~4096/512); got {accepted}"
    );
    assert!(
        store.storage_bytes().expect("storage bytes") <= 4096,
        "disk stays bounded under flood"
    );

    // After those expire (now=2000), a fresh legit deposit lands (expired rows
    // evicted to make room — no permanent lockout).
    let fresh = [0x01u8; 32];
    store
        .insert(
            fresh,
            vec![0xCD; 512],
            2000,
            9_999_999,
            policy.recipient_cap_bytes,
            policy.global_storage_cap_bytes,
            policy.max_recipients,
            2000,
        )
        .expect("fresh legit deposit must land after expiry frees space");
}
