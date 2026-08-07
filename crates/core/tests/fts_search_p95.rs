// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Phase 1.G validation: FTS5 search over 100k rows, p95 < 50 ms.
//!
//! `#[ignore]`-gated. Run with:
//!
//!   cargo test -p skattr-core --release --features test-harness \
//!     --test fts_search_p95 -- --ignored --nocapture
//!
//! Logs p50 / p95 / p99 via eprintln! so trends can be eyeballed
//! without pulling in criterion.

#![cfg(feature = "test-harness")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Instant;

use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use skattr_core::envelope::{Envelope, Kind, MessageId};
use skattr_core::test_exports::{InsertParams, MessageRepo, Pool};

const VOCAB: &[&str] = &[
    "alpha",
    "bravo",
    "charlie",
    "delta",
    "echo",
    "foxtrot",
    "golf",
    "hotel",
    "india",
    "juliet",
    "kilo",
    "lima",
    "mike",
    "november",
    "oscar",
    "papa",
    "quebec",
    "romeo",
    "sierra",
    "tango",
    "uniform",
    "victor",
    "whiskey",
    "xray",
    "yankee",
    "zulu",
    "search",
    "merge",
    "conflict",
    "rebase",
    "branch",
    "commit",
    "stash",
    "cherry",
    "pick",
    "deploy",
    "rollback",
    "feature",
    "fix",
    "tor",
    "arti",
    "noise",
    "handshake",
    "mls",
    "epoch",
    "ratchet",
    "envelope",
    "frame",
    "codec",
    "identity",
    "key",
];

#[test]
#[ignore = "100k rows + 100 queries — run with --release --ignored"]
fn fts_search_p95_under_50ms_over_100k_rows() {
    let pool = Pool::in_memory();
    let mut rng = rand::rngs::StdRng::seed_from_u64(42);

    eprintln!("seeding 100k synthetic text messages...");
    let t0 = Instant::now();
    let repo = MessageRepo::new(&pool);
    for i in 0..100_000i64 {
        let body = (0..rng.gen_range(3..12))
            .map(|_| VOCAB[rng.gen_range(0..VOCAB.len())])
            .collect::<Vec<_>>()
            .join(" ");
        let env = Envelope {
            v: 1,
            id: MessageId::generate(),
            ts: 1_700_000_000 + i,
            reply_to: None,
            kind: Kind::Text { body },
        };
        repo.insert(InsertParams {
            group_id: &[0x55; 32],
            sender: &[0u8; 32],
            envelope: &env,
            mls_generation: u64::try_from(i / 100).unwrap(),
            ts_daemon_recv: 1_700_000_000 + i,
        })
        .unwrap();
    }
    eprintln!("seed complete in {:?}", t0.elapsed());

    // 100 queries: 50 single-token, 50 two-token AND.
    let queries: Vec<String> = (0..100)
        .map(|i| {
            if i < 50 {
                (*VOCAB.choose(&mut rng).unwrap()).to_string()
            } else {
                let a = VOCAB.choose(&mut rng).unwrap();
                let b = VOCAB.choose(&mut rng).unwrap();
                format!("{a} {b}")
            }
        })
        .collect();

    let mut samples_us: Vec<u128> = Vec::with_capacity(queries.len());
    for q in &queries {
        let t = Instant::now();
        let _ = repo.search(q, None, 50, 0, false).unwrap();
        samples_us.push(t.elapsed().as_micros());
    }
    samples_us.sort_unstable();
    let p50 = samples_us[samples_us.len() / 2];
    let p95 = samples_us[(samples_us.len() * 95) / 100];
    let p99 = samples_us[(samples_us.len() * 99) / 100];

    eprintln!("100k-row FTS p50={p50}us p95={p95}us p99={p99}us");
    assert!(
        p95 < 50_000,
        "p95 must be under 50_000us (50ms); got {p95}us"
    );
}
