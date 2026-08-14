// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! #180: a SIGTERM'd daemon must still run its teardown — encrypt the
//! database and wipe decrypted attachment plaintext. Spawns the real
//! `skattr` binary so the signal path is exercised exactly as in the field.
//!
//! `#[ignore]`-gated: the daemon only begins polling its shutdown future
//! after Tor bootstrap completes (Step 8 of `run_with_transport`), so the
//! test must wait for readiness before signalling.
//!
//! Run with:
//!
//! ```bash
//! cargo build -p skattr-cli --release
//! cargo test -p skattr-tests --release -- --ignored signal_teardown
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

/// Tempdir under `$HOME/.cache/` — Arti's fs-mistrust rejects world-writable
/// `/tmp` on Linux. Mirrors `smoke_flag.rs`.
fn safe_tempdir() -> tempfile::TempDir {
    let cache_root = std::env::var_os("HOME")
        .map(|h| {
            std::path::PathBuf::from(h)
                .join(".cache")
                .join("skattr-signal-test")
        })
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/skattr-signal-test-no-home"));
    std::fs::create_dir_all(&cache_root).expect("cache_root mkdir");
    tempfile::Builder::new()
        .prefix("sig-")
        .tempdir_in(&cache_root)
        .expect("tempdir_in cache")
}

fn skattr_bin() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p.join("target").join("release").join("skattr")
}

#[test]
#[ignore = "spawns a real daemon (Tor bootstrap); build skattr-cli --release first"]
#[cfg(unix)]
fn sigterm_leaves_no_plaintext_and_a_fresh_age() {
    let dir = safe_tempdir();
    let data_dir = dir.path();
    let pass_path = data_dir.join("pass.txt");
    std::fs::write(&pass_path, "correct horse battery staple").unwrap();

    let bin = skattr_bin();
    assert!(
        bin.exists(),
        "build first: cargo build -p skattr-cli --release"
    );

    // `skattr init` is deliberately interactive-only (it always prompts on
    // /dev/tty to choose+confirm a fresh passphrase; per CHANGELOG only
    // `daemon` was ever wired to `--passphrase-file`/`$SKATTR_PASSPHRASE_FILE`
    // for automation) — there's no headless way to drive it from a test
    // harness with no controlling tty. Create the vault directly via the
    // same core API `init` itself calls (mirrors `cli_real_tor.rs`), then
    // hand off to the real `skattr daemon` binary, which does honor
    // `--passphrase-file`. This keeps the thing actually under test — SIGTERM
    // handling in the real daemon process — faithful to the field.
    let seed = skattr_core::identity::Seed::generate().expect("seed generate");
    let identity =
        skattr_core::identity::IdentityKey::from_seed(&seed).expect("identity from seed");
    skattr_core::identity::Vault::create(
        &data_dir.join("identity.vault"),
        identity,
        "correct horse battery staple",
    )
    .expect("vault create");

    let started = SystemTime::now();
    let mut child = Command::new(&bin)
        .args(["daemon", "--data-dir"])
        .arg(data_dir)
        .arg("--passphrase-file")
        .arg(&pass_path)
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn daemon");

    // Wait for readiness. The daemon only polls its shutdown future after
    // Step 8, so signalling earlier would test the default signal action.
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.contains("Ctrl-C to shut down") {
                let _ = tx.send(());
                break;
            }
        }
    });
    rx.recv_timeout(Duration::from_secs(240))
        .expect("daemon never became ready within 240s");

    // The plaintext DB must exist right now — otherwise the assertions below
    // would pass vacuously against a daemon that never wrote anything.
    assert!(
        data_dir.join("skattr.sqlite").exists(),
        "precondition: plaintext DB should exist while running"
    );

    let pid = child.id();
    let kill = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .expect("send SIGTERM");
    assert!(kill.success(), "kill -TERM failed");

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("daemon did not exit within 60s of SIGTERM");
            }
            None => std::thread::sleep(Duration::from_millis(200)),
        }
    }

    for leftover in [
        "skattr.sqlite",
        "skattr.sqlite-wal",
        "skattr.sqlite-shm",
        "skattr.sqlite.open",
    ] {
        assert!(
            !data_dir.join(leftover).exists(),
            "{leftover} must not survive a SIGTERM'd shutdown"
        );
    }

    let age = data_dir.join("skattr.sqlite.age");
    assert!(age.exists(), "encrypted DB must exist after shutdown");
    let mtime = std::fs::metadata(&age).unwrap().modified().unwrap();
    assert!(
        mtime >= started,
        "skattr.sqlite.age is stale — this session was never encrypted"
    );

    let cache_open = data_dir.join("cache").join("open");
    let remaining = std::fs::read_dir(&cache_open)
        .map(|rd| rd.flatten().count())
        .unwrap_or(0);
    assert_eq!(remaining, 0, "decrypted plaintext left in cache/open");
}
