// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 0 exit-criterion test: two daemons echo bytes over real Tor.
//!
//! Ignored by default. Run with:
//!
//! ```bash
//! cargo test -p skattr-tests --release -- --ignored
//! ```
//!
//! Takes ~3-10 min on first run (two fresh Arti bootstraps).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use skattr_core::identity::Seed;
use skattr_core::test_exports::{OnionListener, TorConfig, TorRuntime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Create a tempdir with 0700 permissions — Arti 0.41 refuses to open
/// a `state_dir` that's group- or world-readable.
fn arti_friendly_tempdir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(tmp.path()).unwrap().permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(tmp.path(), perms).unwrap();
    }
    tmp
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real Tor network; ~3-10 min; run with --ignored"]
async fn two_daemons_echo_bytes_over_tor() {
    let tmp_a = arti_friendly_tempdir();
    let tmp_b = arti_friendly_tempdir();

    // Daemon A.
    let mut rt_a = TorRuntime::bootstrap(TorConfig {
        state_dir: tmp_a.path().to_path_buf(),
        socks_port: None,
        trust_dir_permissions: true,
    })
    .await
    .expect("A: bootstrap");

    let seed_a = Seed::generate().unwrap();
    let onion = rt_a
        .publish_onion(&tmp_a.path().join("hs.key.age"), &seed_a, "skattr-exit-a")
        .await
        .expect("A: publish");
    eprintln!("A listening on: {onion}");

    // Hand the rend request stream to an OnionListener that echoes.
    let rend_requests = rt_a
        .rend_requests_take()
        .expect("A: rend_requests must be populated after publish");
    let mut listener = OnionListener::spawn(rend_requests, 8);
    let echo_task = tokio::spawn(async move {
        if let Some(mut stream) = listener.accepted.recv().await {
            let mut buf = [0u8; 64];
            let n = stream.read(&mut buf).await.expect("A: read");
            stream.write_all(&buf[..n]).await.expect("A: write echo");
            let _ = stream.shutdown().await;
        }
    });

    // Daemon B.
    let rt_b = TorRuntime::bootstrap(TorConfig {
        state_dir: tmp_b.path().to_path_buf(),
        socks_port: None,
        trust_dir_permissions: true,
    })
    .await
    .expect("B: bootstrap");

    let mut stream = rt_b.connect(&onion, 1).await.expect("B: connect");
    const MSG: &[u8] = b"hello skattr phase-0 exit";
    stream.write_all(MSG).await.expect("B: write");
    let mut buf = [0u8; 64];
    let n = stream.read(&mut buf).await.expect("B: read");
    assert_eq!(&buf[..n], MSG, "echo mismatch");

    echo_task.await.expect("A: echo task");
    rt_a.shutdown().await.expect("A: shutdown");
    rt_b.shutdown().await.expect("B: shutdown");
}
