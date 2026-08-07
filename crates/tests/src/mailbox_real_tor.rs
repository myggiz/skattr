// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Real-Tor smoke test: spawn the `skattr-mailbox` binary, publish a
//! v3 onion, drive a Deposit + Challenge round-trip from a client
//! that talks via `arti-client`. `#[ignore]`-gated; run with
//!
//! ```bash
//! cargo build -p skattr-mailbox --release   # must run first
//! cargo test  -p skattr-tests   --release -- --ignored deposit_and_challenge_over_real_tor
//! ```
//!
//! Cargo does not auto-build the `skattr-mailbox` binary as a side-effect
//! of testing this crate (that would require unstable
//! artifact-dependencies). The test resolves the binary path
//! heuristically from `CARGO_MANIFEST_DIR` and falls back to the
//! workspace `target/{release,debug}/skattr-mailbox` layout. If the
//! binary is missing, the test emits a clear actionable error rather
//! than a cryptic spawn failure.
//!
//! Mirrors `delivery_real_tor.rs` in shape: the client side bootstraps
//! its own Arti runtime in a 0700 tempdir (Arti 0.41 refuses to open a
//! group- or world-readable `state_dir`), connects to the spawned
//! mailbox's published onion, then exercises one Deposit and one
//! Challenge round-trip.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Result};
use futures::{SinkExt, StreamExt};
use skattr_core::mailbox::protocol::{Challenge, Deposit, PROTOCOL_VERSION};
use skattr_mailbox::codec::{MailboxFrame, MailboxFrameCodec};
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::process::{Child, Command};
use tokio_util::codec::Framed;

/// Create a tempdir with 0700 permissions — Arti 0.41 refuses to open
/// a `state_dir` that is group- or world-readable. Mirrors
/// `delivery_real_tor.rs::arti_friendly_tempdir`.
fn arti_friendly_tempdir() -> TempDir {
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

/// Spawn the `skattr-mailbox` binary with a synthesised config, tail
/// its stderr until the `mailbox onion published onion=...` log line
/// appears, then return the child handle, the parsed onion address,
/// and the data-dir guard.
async fn spawn_mailbox_with_published_onion() -> Result<(Child, String, TempDir)> {
    let dir = arti_friendly_tempdir();
    let cfg_path = dir.path().join("mailbox.toml");
    let body = format!(
        r#"
[server]
data_dir = "{}"

[policy]
max_deposit_size = 1048576
min_ttl_secs = 3600
max_ttl_secs = 2592000
default_ttl_secs = 604800
recipient_cap_bytes = 268435456
per_conn_deposits_per_min = 30
per_conn_fetches_per_min = 6
global_deposits_per_min = 1000
"#,
        dir.path().display()
    );
    std::fs::write(&cfg_path, body)?;

    let bin = locate_mailbox_binary()?;
    let mut child = Command::new(&bin)
        .arg("--config")
        .arg(&cfg_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    // Tail stderr until the `mailbox onion published onion=<addr>.onion`
    // line shows up. `crates/mailbox/src/arti.rs` emits this line via
    // `tracing::info!(onion = %addr.display_unredacted(), "mailbox onion
    // published")`; the default `tracing_subscriber::fmt` formatter
    // renders it as `... mailbox onion published onion=<addr>.onion`.
    let stderr = child.stderr.take().expect("stderr piped");
    let mut reader = tokio::io::BufReader::new(stderr).lines();
    let mut onion: Option<String> = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    while std::time::Instant::now() < deadline {
        let line_fut = reader.next_line();
        match tokio::time::timeout(Duration::from_secs(5), line_fut).await {
            Ok(Ok(Some(line))) => {
                eprintln!("mailbox stderr: {line}");
                if let Some(idx) = line.find("onion=") {
                    let val = &line[idx + "onion=".len()..];
                    let end = val.find(' ').unwrap_or(val.len());
                    onion = Some(val[..end].trim_matches('"').to_string());
                    break;
                }
            }
            Ok(Ok(None)) => bail!("mailbox stderr closed before publishing an onion"),
            Ok(Err(e)) => bail!("read mailbox stderr: {e}"),
            Err(_) => continue, // 5 s tick; loop until deadline.
        }
    }
    let onion = match onion {
        Some(s) => s,
        None => bail!("mailbox did not publish an onion within 180s"),
    };
    Ok((child, onion, dir))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "real Tor bootstrap + mailbox HS publish + dial, ~2-5 min; run with --ignored"]
async fn deposit_and_challenge_over_real_tor() -> Result<()> {
    let (_child, onion, _dir) = spawn_mailbox_with_published_onion().await?;
    eprintln!("mailbox onion: {onion}");

    // Bootstrap the client's own Arti runtime in a 0700 tempdir; the
    // mailbox runs its own Arti instance in `_dir`, so this client
    // needs an isolated state dir.
    let client_state = arti_friendly_tempdir();
    let client_cache = client_state.path().join("cache");
    std::fs::create_dir_all(&client_cache)?;

    // Mirror `core::transport::tor::TorRuntime::bootstrap` — workspace
    // standardises on `TokioRustlsRuntime::current()` rather than
    // `PreferredRuntime::current()` because the surrounding code
    // already requires a Tokio reactor.
    let runtime = tor_rtcompat::tokio::TokioRustlsRuntime::current()
        .map_err(|e| anyhow::anyhow!("arti runtime: requires tokio reactor: {e}"))?;
    let cfg = arti_client::config::TorClientConfigBuilder::from_directories(
        client_state.path(),
        &client_cache,
    )
    .build()
    .map_err(|e| anyhow::anyhow!("arti config: {e}"))?;
    let tor = arti_client::TorClient::with_runtime(runtime)
        .config(cfg)
        .create_unbootstrapped_async()
        .await?;
    tor.bootstrap().await?;

    // `TorClient::connect` accepts `&str` directly via `IntoTorAddr`;
    // `core::transport::tor` uses the same shape (`format!("{onion}:{port}")`).
    //
    // The mailbox's `mailbox onion published` log line fires when Arti
    // *launches* the service, but the HSDir upload happens
    // asynchronously (typically 10–60s after launch). Connecting
    // immediately fails with "Unable to download hidden service
    // descriptor"; retry with backoff until the descriptor lands.
    let target = format!("{onion}:1");
    let mut attempts = 0;
    let stream = loop {
        attempts += 1;
        match tor.connect(target.as_str()).await {
            Ok(s) => break s,
            Err(e) if attempts < 18 => {
                eprintln!(
                    "client connect attempt {attempts} failed (descriptor likely not yet \
                     uploaded): {e}; retrying in 10s"
                );
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
            Err(e) => bail!("client connect failed after {attempts} attempts: {e}"),
        }
    };

    // Drive the codec: one Deposit (must reply `DepositOk`) and one
    // Challenge (must reply `ChallengeNonce`).
    let mut framed = Framed::new(stream, MailboxFrameCodec::new());
    framed
        .send(MailboxFrame::Deposit(Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash: [0xAA; 32],
            ciphertext: vec![1, 2, 3, 4],
            ttl_request: 86_400,
        }))
        .await?;
    let resp = framed
        .next()
        .await
        .ok_or_else(|| anyhow::anyhow!("no reply to Deposit"))??;
    assert!(
        matches!(resp, MailboxFrame::DepositOk(_)),
        "unexpected reply to Deposit: {resp:?}"
    );

    framed
        .send(MailboxFrame::Challenge(Challenge {
            version: PROTOCOL_VERSION,
            identity_hash: [0xBB; 32],
        }))
        .await?;
    let resp = framed
        .next()
        .await
        .ok_or_else(|| anyhow::anyhow!("no reply to Challenge"))??;
    assert!(
        matches!(resp, MailboxFrame::ChallengeNonce(_)),
        "unexpected reply to Challenge: {resp:?}"
    );

    Ok(())
}

/// Resolve the `skattr-mailbox` binary on disk.
///
/// `CARGO_BIN_EXE_<name>` is only set for integration tests of the
/// crate that defines the binary; this test lives in the sibling
/// `skattr-tests` crate, so we walk the workspace `target/` layout
/// instead. Prefer `release` (test command in the docstring builds
/// `--release`) but fall back to `debug` if only that one exists.
fn locate_mailbox_binary() -> Result<PathBuf> {
    // `CARGO_MANIFEST_DIR` of skattr-tests is `<workspace>/crates/tests`.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| anyhow::anyhow!("can't derive workspace root from CARGO_MANIFEST_DIR"))?;
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target"));
    let exe = if cfg!(windows) {
        "skattr-mailbox.exe"
    } else {
        "skattr-mailbox"
    };
    for profile in ["release", "debug"] {
        let candidate = target.join(profile).join(exe);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!(
        "skattr-mailbox binary not found under {}; run `cargo build -p skattr-mailbox --release` first",
        target.display()
    );
}
