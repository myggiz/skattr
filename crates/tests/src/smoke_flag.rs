// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 2.G: end-to-end test of `skattr-ui --smoke-test`. Spawns the
//! UI binary as a subprocess (so the argv branch + tokio runtime
//! creation paths are exercised exactly as in CI release smoke), waits
//! for it to exit, and asserts the data_dir was correctly populated.
//!
//! `#[ignore]`-gated; spawns real Arti.
//!
//! On dev hosts where Arti's fs-mistrust rejects user `~/.cache/`
//! paths, set `ARTI_FS_DISABLE_PERMISSION_CHECKS=true` before
//! invoking cargo. CI's `$RUNNER_TEMP` works without the env var.
//!
//! Run with:
//!
//! ```bash
//! cargo build -p skattr-ui --release
//! cargo test -p skattr-tests --release -- --ignored smoke_flag
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;
use std::time::{Duration, Instant};

/// Allocate a tempdir anchored at `$HOME/.cache/skattr-smoke-test/`
/// (NOT `/tmp`, which is world-writable and rejected by Arti's
/// fs-mistrust on Linux).
fn safe_tempdir() -> tempfile::TempDir {
    let cache_root = std::env::var_os("HOME")
        .map(|h| {
            std::path::PathBuf::from(h)
                .join(".cache")
                .join("skattr-smoke-test")
        })
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/skattr-smoke-test-no-home"));
    std::fs::create_dir_all(&cache_root).expect("cache_root mkdir");
    tempfile::Builder::new()
        .prefix("integ-")
        .tempdir_in(&cache_root)
        .expect("tempdir_in cache")
}

/// Locate the workspace's `target/release/skattr-ui` binary.
///
/// Honours `CARGO_TARGET_DIR` if set; otherwise resolves relative
/// to this crate's manifest dir (`crates/tests` → workspace root →
/// `target/`).
fn skattr_ui_release_bin() -> std::path::PathBuf {
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            // crates/tests -> ../.. = workspace root
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(2)
                .expect("ancestors(2) on CARGO_MANIFEST_DIR")
                .join("target")
        });
    target_dir.join("release").join("skattr-ui")
}

#[test]
#[ignore = "spawns real Arti; build skattr-ui first then run with: \
            cargo test -p skattr-tests --release -- --ignored smoke_flag"]
fn skattr_ui_smoke_test_exits_zero() {
    let tmp = safe_tempdir();
    let data_dir = tmp.path().to_path_buf();

    let bin = skattr_ui_release_bin();
    assert!(
        bin.exists(),
        "skattr-ui release binary missing at {} \
         (build first: cargo build -p skattr-ui --release)",
        bin.display()
    );

    let started = Instant::now();
    let output = Command::new(&bin)
        .args([
            "--smoke-test",
            "--data-dir",
            data_dir.to_str().unwrap(),
            "--timeout-secs",
            "240",
        ])
        .output()
        .expect("spawn skattr-ui");

    let elapsed = started.elapsed();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "smoke exited {}: stdout={} stderr={}",
        output.status,
        stdout,
        stderr
    );
    assert!(
        elapsed <= Duration::from_secs(260),
        "smoke took {elapsed:?}; should complete within ~240s + slack"
    );
    assert!(
        stdout.contains("smoke OK"),
        "stdout missing 'smoke OK': {stdout}"
    );

    // Vault should have been created.
    assert!(
        data_dir.join("identity.vault").exists(),
        "smoke must leave identity.vault at {}",
        data_dir.display()
    );
}
