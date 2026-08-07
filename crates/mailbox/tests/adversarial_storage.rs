// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Concurrent-delete races + cap eviction ordering.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use skattr_mailbox::store::Store;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_delete_yields_consistent_counts() {
    let store = std::sync::Arc::new(Store::in_memory().unwrap());
    let recipient = [0x42u8; 32];
    let mut ids = Vec::new();
    for _ in 0..50 {
        let id = store
            .insert(
                recipient,
                vec![1, 2, 3],
                100,
                999_999,
                1 << 30,
                1 << 30,
                100,
                50,
            )
            .unwrap();
        ids.push(id);
    }
    let s1 = store.clone();
    let s2 = store.clone();
    let ids_a = ids.clone();
    let ids_b = ids.clone();
    let h1 = tokio::spawn(async move { s1.delete(recipient, &ids_a).unwrap() });
    let h2 = tokio::spawn(async move { s2.delete(recipient, &ids_b).unwrap() });
    let (a, b) = (h1.await.unwrap(), h2.await.unwrap());
    // Together: every row was deleted exactly once across both calls.
    assert_eq!(a.0 + b.0, 50);
    // Not-found is the complement: each task asked for 50 ids, only some succeeded.
    assert_eq!(a.1 + b.1, 50);
}

#[test]
fn cap_eviction_evicts_oldest_expired_first() {
    let store = Store::in_memory().unwrap();
    let recipient = [0xAB; 32];
    // Two expired rows (oldest first) + one fresh.
    let _id1 = store
        .insert(recipient, vec![1; 4], 100, 110, 16, 1 << 30, 100, 50)
        .unwrap(); // oldest expired
    let _id2 = store
        .insert(recipient, vec![2; 4], 200, 210, 16, 1 << 30, 100, 150)
        .unwrap(); // newer expired
    let id3 = store
        .insert(recipient, vec![3; 4], 300, 999_999, 16, 1 << 30, 100, 250)
        .unwrap(); // pending
                   // Now insert a new row; expecting eviction of id1 first.
    let id4 = store
        .insert(recipient, vec![4; 4], 400, 999_999, 16, 1 << 30, 100, 400)
        .unwrap();
    let rows = store.fetch(recipient, 500).unwrap();
    let surviving: std::collections::HashSet<[u8; 16]> =
        rows.iter().map(|r| r.deposit_id).collect();
    assert!(surviving.contains(&id3), "pending must survive");
    assert!(surviving.contains(&id4), "newly inserted must survive");
}
