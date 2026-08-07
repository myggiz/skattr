# Phase 2.G Packaging & Distribution — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship Linux (`.deb` + AppImage + Flatpak source) and macOS (`.dmg`) bundles via a CI release flow gated on a per-platform smoke test, signed-checksum verification (minisign), and an exact-pinned reproducible build recipe.

**Architecture:** A new `core::daemon::smoke` module exposes `run_smoke(SmokeConfig)` that vault-inits a throwaway identity, runs `Daemon::run` with a `Ready`-driven shutdown trigger, and exits 0/1. The `skattr-ui` binary gets an argv-level `--smoke-test` branch that calls into it without ever opening Tauri's webview. CI matrix-builds on `ubuntu-latest` + `macos-latest`, runs the smoke flag against the *installed* artefact (not the build tree), and only then signs `SHA256SUMS` with minisign and creates the GitHub Release. Bundle metadata, the `skattr://` URL scheme, an explicitly-disabled Tauri updater, and Flatpak/AppStream/install docs round it out. **Wire-format-NEUTRAL by design** — no new `Command` / `CommandResult` / `Event` variants.

**Tech Stack:** Tauri 2.11 (Rust + JS), `cargo-tauri` CLI, `tauri-plugin-deep-link`, `tauri-plugin-single-instance`, `flatpak-builder`, `minisign`, GitHub Actions, WiX 4 (Phase 2.H only).

**Spec:** `docs/superpowers/specs/2026-05-04-phase-2g-packaging-design.md`

**Branch:** `phase-2g-packaging` (worktree at `/home/myggiz/development/skattr-phase-2g`)

**Spec correction baked in:** the spec references `licenseFile: "../../COPYING"`. The repo has `LICENSE-GPL3` and `LICENSE-AGPL3`, no `COPYING`. This plan uses `LICENSE-GPL3` for `crates/ui/tauri.conf.json` and adds a comment in `tauri.conf.json` noting the source path.

---

## Task 0: Worktree + branch setup

**Files:**
- New worktree: `/home/myggiz/development/skattr-phase-2g`
- New branch: `phase-2g-packaging`

- [ ] **Step 0.1: Confirm working directory is clean**

Run from `/home/myggiz/development/skattr`:

```bash
cd /home/myggiz/development/skattr
git status
git fetch origin
git log --oneline origin/master..HEAD
git log --oneline HEAD..origin/master
```

Expected: working tree clean, no commits ahead/behind master.

- [ ] **Step 0.2: Create the worktree on a fresh branch off master**

```bash
git worktree add -b phase-2g-packaging /home/myggiz/development/skattr-phase-2g master
```

Expected: worktree directory created, branch `phase-2g-packaging` checked out there.

- [ ] **Step 0.3: All subsequent work happens in the worktree**

```bash
cd /home/myggiz/development/skattr-phase-2g
pwd
git rev-parse --abbrev-ref HEAD
```

Expected: `phase-2g-packaging` printed.

---

## Task 1: Pin exact toolchain version

**Files:**
- Modify: `rust-toolchain.toml`

- [ ] **Step 1.1: Capture the maintainer's stable rustc version**

```bash
rustup update stable
rustc +stable --version
```

Expected output format: `rustc 1.84.1 (e71f9a9a9 2025-01-27)` — record the exact `1.x.y` part.

- [ ] **Step 1.2: Pin it in `rust-toolchain.toml`**

Replace the file contents with (substituting the exact version captured in 1.1):

```toml
[toolchain]
channel = "stable"
version = "1.84.1"
components = ["rustfmt", "clippy"]
profile = "minimal"
```

(Leave `channel = "stable"` so `rustup` keeps the channel-tracking behaviour for new dev environments. The `version` field pins the resolved version for reproducibility per Phase 2.G spec §"Locked decisions" item 4.)

- [ ] **Step 1.3: Verify the toolchain still resolves**

```bash
cd /home/myggiz/development/skattr-phase-2g
. "$HOME/.cargo/env"
cargo --version
rustc --version
```

Expected: rustc version matches what was pinned.

- [ ] **Step 1.4: Commit**

```bash
git add rust-toolchain.toml
git commit -m "build: pin exact rustc stable version

Phase 2.G locks the toolchain version explicitly per spec §locked
decision 4 — \`channel = stable\` is too loose for a recipe-based
reproducibility claim.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Pin Tauri to an exact patch version (Rust + JS)

**Files:**
- Modify: `crates/ui/Cargo.toml`
- Modify: `crates/ui/src-svelte/package.json`

- [ ] **Step 2.1: Look up the resolved Tauri version**

```bash
cd /home/myggiz/development/skattr-phase-2g
grep -A1 '^name = "tauri"$' Cargo.lock | head -5
```

Expected: a `version = "2.11.x"` line. Record the exact `2.x.y`.

- [ ] **Step 2.2: Pin Tauri in `crates/ui/Cargo.toml`**

Replace lines:

```toml
[build-dependencies]
tauri-build = { version = "2", features = [] }
```

with (exact version from step 2.1; this example uses 2.11.0):

```toml
[build-dependencies]
tauri-build = { version = "=2.11.0", features = [] }
```

And replace:

```toml
tauri = { version = "2", features = ["tray-icon"] }
```

with:

```toml
tauri = { version = "=2.11.0", features = ["tray-icon"] }
```

- [ ] **Step 2.3: Pin `@tauri-apps/api` in `crates/ui/src-svelte/package.json`**

Find the line:

```json
"@tauri-apps/api": "2.0.0",
```

The exact JS-side version that pairs with Tauri 2.11 may differ from the Rust patch (the JS package follows its own minor track). Run:

```bash
cd /home/myggiz/development/skattr-phase-2g/crates/ui/src-svelte
pnpm view @tauri-apps/api@'^2' versions --json | tail -5
```

Choose the highest 2.x version published *before* the Tauri 2.11.0 Rust release (so the API surface matches). If unclear, leave at `2.0.0` for now and verify with a Tauri build in step 2.5.

For now (with 2.0.0 already pinned to an exact version), no edit is needed — the package.json already has `"@tauri-apps/api": "2.0.0"` (exact, no `^`/`~`).

- [ ] **Step 2.4: Refresh Cargo.lock (no actual version change yet, just verify)**

```bash
cd /home/myggiz/development/skattr-phase-2g
cargo update -p tauri --precise 2.11.0
cargo update -p tauri-build --precise 2.11.0
git diff Cargo.lock
```

Expected: minimal diff (the lock already had 2.11.0 resolved; this step just verifies pinning didn't accidentally upgrade something else).

- [ ] **Step 2.5: Build + clippy to confirm pin works**

```bash
cd /home/myggiz/development/skattr-phase-2g
cargo clippy -p skattr-ui --all-targets --all-features -- -D warnings
```

Expected: no warnings, clean exit.

- [ ] **Step 2.6: Commit**

```bash
git add crates/ui/Cargo.toml Cargo.lock
git commit -m "build(ui): pin Tauri to exact patch version

Phase 2.G spec §locked decision 3: \`tauri = \"2\"\` is too loose
for reproducibility. Lock to =2.11.0 (Rust side) so a future
\`cargo update\` can't silently float to 2.12.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Smoke module — define error type (TDD round 1)

**Files:**
- Create: `crates/core/src/daemon/smoke.rs`
- Modify: `crates/core/src/daemon/mod.rs`

- [ ] **Step 3.1: Write the failing test for `SmokeError` Debug + Display**

Add to a new file `crates/core/src/daemon/smoke.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Smoke-test entry point for release artefacts.
//!
//! `run_smoke` initialises a throwaway vault, boots the daemon, waits
//! for `TorStatus::Ready`, then triggers a graceful shutdown. Used by
//! the `skattr-ui --smoke-test` argv branch in CI release pipelines
//! to verify the bundled binary actually starts on each platform.

use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

/// Configuration for [`run_smoke`].
#[derive(Debug, Clone)]
pub struct SmokeConfig {
    /// Empty or non-existent directory the smoke owns. The smoke
    /// refuses to run if any user state is present.
    pub data_dir: PathBuf,
    /// Maximum time to wait for `TorStatus::Ready` before failing.
    pub tor_ready_timeout: Duration,
    /// Override the throwaway seed entropy. `[0u8; 32]` (the default)
    /// means "generate from `OsRng`".
    pub seed_bytes: [u8; 32],
}

impl Default for SmokeConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::new(),
            tor_ready_timeout: Duration::from_secs(240),
            seed_bytes: [0u8; 32],
        }
    }
}

/// Result emitted on a successful smoke run.
#[derive(Debug, Clone)]
pub struct SmokeReport {
    /// The published v3 onion address.
    pub onion: String,
    /// Wall-clock time from `run_smoke` start to `Ready`.
    pub duration: Duration,
}

/// Smoke-test failure modes.
#[derive(Debug, Error)]
pub enum SmokeError {
    /// `data_dir` already contains user state; refuse to clobber.
    #[error("smoke: data_dir not empty (found {found})")]
    DataDirNotEmpty {
        /// Description of the offending entry (e.g. "identity.vault").
        found: String,
    },
    /// Vault creation failed.
    #[error("smoke: vault create: {0}")]
    VaultCreate(String),
    /// Daemon failed to start.
    #[error("smoke: daemon start: {0}")]
    DaemonStart(String),
    /// `TorStatus::Ready` did not arrive within the configured timeout.
    #[error("smoke: tor bootstrap timed out after {waited:?}")]
    TorTimeout {
        /// Elapsed time before the timeout fired.
        waited: Duration,
    },
    /// Something else went wrong (I/O, channel close, etc.).
    #[error("smoke: {0}")]
    Other(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_error_displays_data_dir_not_empty() {
        let e = SmokeError::DataDirNotEmpty {
            found: "identity.vault".into(),
        };
        let s = format!("{e}");
        assert!(s.contains("data_dir not empty"));
        assert!(s.contains("identity.vault"));
    }

    #[test]
    fn smoke_error_displays_tor_timeout() {
        let e = SmokeError::TorTimeout {
            waited: Duration::from_secs(240),
        };
        let s = format!("{e}");
        assert!(s.contains("tor bootstrap timed out"));
    }

    #[test]
    fn smoke_config_default_uses_240s_timeout() {
        let c = SmokeConfig::default();
        assert_eq!(c.tor_ready_timeout, Duration::from_secs(240));
        assert_eq!(c.seed_bytes, [0u8; 32]);
    }
}
```

- [ ] **Step 3.2: Wire the new module into `daemon/mod.rs`**

Add to `crates/core/src/daemon/mod.rs` after `pub mod retention;` (alphabetical position):

```rust
pub mod smoke;
```

- [ ] **Step 3.3: Run the tests — should pass (no logic yet, just the type)**

```bash
cd /home/myggiz/development/skattr-phase-2g
. "$HOME/.cargo/env"
cargo test -p skattr-core --features test-harness daemon::smoke -- --nocapture
```

Expected: 3 tests pass.

- [ ] **Step 3.4: Commit**

```bash
git add crates/core/src/daemon/mod.rs crates/core/src/daemon/smoke.rs
git commit -m "core(smoke): introduce SmokeConfig + SmokeError types

Phase 2.G Task 3: scaffolds the smoke module with the typed error
surface. Implementation lands in subsequent tasks.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Smoke module — `data_dir_has_user_state` helper (TDD round 2)

**Files:**
- Modify: `crates/core/src/daemon/smoke.rs`

- [ ] **Step 4.1: Write failing tests for the helper**

Append to the `mod tests {}` block in `crates/core/src/daemon/smoke.rs`:

```rust
    #[test]
    fn data_dir_check_passes_for_nonexistent_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let result = check_data_dir_clean(&missing);
        assert!(result.is_ok(), "non-existent dir must be acceptable");
    }

    #[test]
    fn data_dir_check_passes_for_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let result = check_data_dir_clean(tmp.path());
        assert!(result.is_ok(), "empty dir must be acceptable");
    }

    #[test]
    fn data_dir_check_rejects_existing_vault() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("identity.vault"), b"x").unwrap();
        let err = check_data_dir_clean(tmp.path()).unwrap_err();
        match err {
            SmokeError::DataDirNotEmpty { found } => {
                assert!(found.contains("identity.vault"));
            }
            other => panic!("expected DataDirNotEmpty, got {other:?}"),
        }
    }

    #[test]
    fn data_dir_check_ignores_hidden_files() {
        let tmp = tempfile::tempdir().unwrap();
        // Hidden / dotfiles created by editors or VCS shouldn't trip the gate.
        std::fs::write(tmp.path().join(".DS_Store"), b"x").unwrap();
        let result = check_data_dir_clean(tmp.path());
        assert!(
            result.is_ok(),
            "hidden file must not block smoke; got {result:?}"
        );
    }

    #[test]
    fn data_dir_check_rejects_arbitrary_user_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("notes.txt"), b"important").unwrap();
        let err = check_data_dir_clean(tmp.path()).unwrap_err();
        assert!(matches!(err, SmokeError::DataDirNotEmpty { .. }));
    }
```

Verify `tempfile` is already a dev-dep on `skattr-core`:

```bash
grep -n tempfile /home/myggiz/development/skattr-phase-2g/crates/core/Cargo.toml
```

If not present in `[dev-dependencies]`, add `tempfile = "3"` there.

- [ ] **Step 4.2: Run — should fail (helper not defined)**

```bash
cd /home/myggiz/development/skattr-phase-2g
cargo test -p skattr-core --features test-harness daemon::smoke
```

Expected: compile error "cannot find function `check_data_dir_clean`".

- [ ] **Step 4.3: Implement the helper**

Add to `crates/core/src/daemon/smoke.rs` after the `SmokeError` enum:

```rust
/// Verify the data_dir is safe to use for a smoke run.
///
/// Accepts a non-existent directory or a directory whose only
/// entries are hidden (dotfile-prefixed). Rejects any directory
/// containing visible files / subdirectories — particularly an
/// existing `identity.vault`.
pub(crate) fn check_data_dir_clean(data_dir: &std::path::Path) -> Result<(), SmokeError> {
    if !data_dir.exists() {
        return Ok(());
    }
    let entries = std::fs::read_dir(data_dir).map_err(|e| {
        SmokeError::Other(format!("read_dir {}: {e}", data_dir.display()))
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| SmokeError::Other(format!("dir entry: {e}")))?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        // Skip hidden / dotfile entries (editor + macOS metadata).
        if name_str.starts_with('.') {
            continue;
        }
        return Err(SmokeError::DataDirNotEmpty {
            found: name_str.into_owned(),
        });
    }
    Ok(())
}
```

- [ ] **Step 4.4: Run — tests pass**

```bash
cargo test -p skattr-core --features test-harness daemon::smoke
```

Expected: 8 tests pass (3 from Task 3, 5 new).

- [ ] **Step 4.5: Commit**

```bash
git add crates/core/src/daemon/smoke.rs crates/core/Cargo.toml
git commit -m "core(smoke): refuse to run over populated data_dir

Hidden / dotfile entries are tolerated (editor + macOS metadata);
visible files trigger SmokeError::DataDirNotEmpty.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: Smoke module — `run_smoke` happy-path (TDD round 3)

**Files:**
- Modify: `crates/core/src/daemon/smoke.rs`

- [ ] **Step 5.1: Write a `#[ignore]`-gated integration test for `run_smoke`**

Append to the `mod tests {}` block in `crates/core/src/daemon/smoke.rs`:

```rust
    /// `#[ignore]`-gated: spawns real Arti. Run with:
    ///   cargo test -p skattr-core --features test-harness --release \
    ///       -- --ignored daemon::smoke::run_smoke_real_tor
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "spawns real Arti"]
    async fn run_smoke_real_tor() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = SmokeConfig {
            data_dir: tmp.path().to_path_buf(),
            tor_ready_timeout: Duration::from_secs(240),
            ..SmokeConfig::default()
        };
        let report = run_smoke(cfg).await.unwrap();
        assert!(report.onion.ends_with(".onion"), "got onion {}", report.onion);
        assert!(report.duration <= Duration::from_secs(240));
        // Vault must have been created.
        assert!(tmp.path().join("identity.vault").exists());
    }

    #[tokio::test]
    async fn run_smoke_rejects_existing_vault() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("identity.vault"), b"x").unwrap();
        let cfg = SmokeConfig {
            data_dir: tmp.path().to_path_buf(),
            tor_ready_timeout: Duration::from_secs(1),
            ..SmokeConfig::default()
        };
        let err = run_smoke(cfg).await.unwrap_err();
        assert!(matches!(err, SmokeError::DataDirNotEmpty { .. }));
    }

    #[tokio::test]
    async fn run_smoke_zero_timeout_yields_tor_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = SmokeConfig {
            data_dir: tmp.path().to_path_buf(),
            tor_ready_timeout: Duration::from_millis(1),
            ..SmokeConfig::default()
        };
        let err = run_smoke(cfg).await.unwrap_err();
        assert!(
            matches!(err, SmokeError::TorTimeout { .. }),
            "expected TorTimeout, got {err:?}"
        );
    }
```

- [ ] **Step 5.2: Run — fails (function not defined)**

```bash
cargo test -p skattr-core --features test-harness daemon::smoke
```

Expected: compile error "cannot find function `run_smoke`".

- [ ] **Step 5.3: Implement `run_smoke`**

Append to `crates/core/src/daemon/smoke.rs` (after the `check_data_dir_clean` helper):

```rust
use std::time::Instant;

use tokio::sync::oneshot;
use zeroize::Zeroizing;

use crate::daemon::config::Config;
use crate::daemon::state::Daemon;
use crate::identity::vault::Vault;
use crate::identity::{IdentityKey, Seed};

/// Random throwaway passphrase for a smoke vault.
fn make_throwaway_passphrase() -> Zeroizing<String> {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Zeroizing::new(hex)
}

/// Build a `Seed` from optional fixed entropy. `[0u8; 32]` falls back
/// to `Seed::generate()` (OS CSPRNG).
///
/// We round-trip through `bip39::Mnemonic` so we go through the
/// canonical entropy path (`Seed::from_mnemonic`) rather than
/// constructing a `Seed` directly from raw bytes — that path's
/// constructor is private to the `identity` module.
fn make_seed(seed_bytes: [u8; 32]) -> Result<Seed, SmokeError> {
    if seed_bytes == [0u8; 32] {
        return Seed::generate().map_err(|e| SmokeError::VaultCreate(e.to_string()));
    }
    let mnemonic = bip39::Mnemonic::from_entropy_in(bip39::Language::English, &seed_bytes)
        .map_err(|e| SmokeError::VaultCreate(format!("seed: {e}")))?;
    let words = mnemonic.to_string();
    let mn = crate::identity::Mnemonic::parse(&words);
    Seed::from_mnemonic(&mn).map_err(|e| SmokeError::VaultCreate(format!("from_mnemonic: {e}")))
}

/// Run a one-shot smoke test: init throwaway vault, boot daemon,
/// wait for `TorStatus::Ready`, then trigger graceful shutdown.
pub async fn run_smoke(cfg: SmokeConfig) -> Result<SmokeReport, SmokeError> {
    let started = Instant::now();

    // Step 1: refuse to clobber existing user state.
    check_data_dir_clean(&cfg.data_dir)?;
    std::fs::create_dir_all(&cfg.data_dir).map_err(|e| {
        SmokeError::Other(format!("mkdir {}: {e}", cfg.data_dir.display()))
    })?;

    // Step 2: create a throwaway vault.
    let passphrase = make_throwaway_passphrase();
    let seed = make_seed(cfg.seed_bytes)?;
    let identity =
        IdentityKey::from_seed(&seed).map_err(|e| SmokeError::VaultCreate(e.to_string()))?;
    let vault_path = cfg.data_dir.join("identity.vault");
    Vault::create(&vault_path, identity, passphrase.as_str())
        .map_err(|e| SmokeError::VaultCreate(e.to_string()))?;

    // Step 3: build a daemon Config that points at our smoke data_dir
    // and a smoke-local IPC socket (avoid colliding with a real daemon).
    let mut config = Config::defaults().map_err(|e| SmokeError::Other(e.to_string()))?;
    config.data_dir = cfg.data_dir.clone();
    config.ipc_socket = Some(cfg.data_dir.join("smoke.sock"));

    // Step 4: spawn the daemon with a shutdown trigger we control.
    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let shutdown_fut = async move {
        let _ = shutdown_rx.await;
    };

    let data_dir_owned = cfg.data_dir.clone();
    let pp_owned = passphrase.clone();
    let cfg_owned = config.clone();
    let daemon_task = tokio::spawn(async move {
        Daemon::run(
            &data_dir_owned,
            &pp_owned,
            cfg_owned,
            // Smoke runs don't persist config changes — point the
            // SetConfig writer at a throwaway path inside data_dir.
            data_dir_owned.join("config.toml"),
            ready_tx,
            shutdown_fut,
        )
        .await
    });

    // Step 5: wait for Ready or time out.
    let ready = match tokio::time::timeout(cfg.tor_ready_timeout, ready_rx).await {
        Ok(Ok(r)) => r,
        Ok(Err(_recv_err)) => {
            // Daemon dropped ready_tx — usually because Daemon::run errored.
            let join_result = daemon_task.await;
            let err_msg = match join_result {
                Ok(Err(e)) => e.to_string(),
                Ok(Ok(())) => "daemon exited cleanly without sending Ready".to_string(),
                Err(join) => format!("daemon task panic: {join}"),
            };
            return Err(SmokeError::DaemonStart(err_msg));
        }
        Err(_timeout) => {
            let _ = shutdown_tx.send(());
            // Best-effort drain of the daemon task; ignore its result.
            let _ = daemon_task.await;
            return Err(SmokeError::TorTimeout {
                waited: started.elapsed(),
            });
        }
    };

    // Step 6: graceful shutdown.
    let _ = shutdown_tx.send(());
    match daemon_task.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(SmokeError::Other(format!("daemon shutdown: {e}"))),
        Err(join) => return Err(SmokeError::Other(format!("daemon task panic: {join}"))),
    }

    Ok(SmokeReport {
        onion: ready.onion,
        duration: started.elapsed(),
    })
}
```

- [ ] **Step 5.4: Run unit tests (skipping the `#[ignore]`'d real-Tor one)**

```bash
cd /home/myggiz/development/skattr-phase-2g
cargo test -p skattr-core --features test-harness daemon::smoke
```

Expected: 10 tests pass (`run_smoke_real_tor` is ignored).

- [ ] **Step 5.5: Run clippy**

```bash
cargo clippy -p skattr-core --all-targets --all-features -- -D warnings
```

Expected: clean.

- [ ] **Step 5.6: Run the real-Tor test (sanity check)**

```bash
cargo test -p skattr-core --features test-harness --release -- --ignored daemon::smoke::run_smoke_real_tor
```

Expected: passes within 240s. If Tor bootstrap is unusually slow on the maintainer's network, this can be re-run; the timeout is configurable in CI.

- [ ] **Step 5.7: Commit**

```bash
git add crates/core/src/daemon/smoke.rs
git commit -m "core(smoke): implement run_smoke + Ready-driven shutdown

Smoke spawns Daemon::run with a smoke-local IPC socket
(\${data_dir}/smoke.sock) so it never collides with a real
daemon's socket. On Ready, fires shutdown_tx and awaits the
daemon task; on timeout, fires shutdown and surfaces TorTimeout.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: Wire `--smoke-test` argv branch into `skattr-ui`

**Files:**
- Modify: `crates/ui/Cargo.toml`
- Modify: `crates/ui/src/main.rs`

- [ ] **Step 6.1: Add a unit test for argv parsing**

Add to `crates/ui/src/main.rs` at the end of the file:

```rust
#[cfg(test)]
mod smoke_argv_tests {
    use super::*;

    #[test]
    fn smoke_argv_detects_flag() {
        let argv = vec![
            "skattr-ui".to_string(),
            "--smoke-test".to_string(),
            "--data-dir".to_string(),
            "/tmp/foo".to_string(),
        ];
        let parsed = parse_smoke_argv(&argv).expect("parses cleanly");
        assert_eq!(parsed.data_dir.to_str(), Some("/tmp/foo"));
        assert_eq!(parsed.timeout_secs, 240);
    }

    #[test]
    fn smoke_argv_accepts_timeout_override() {
        let argv = vec![
            "skattr-ui".to_string(),
            "--smoke-test".to_string(),
            "--data-dir".to_string(),
            "/tmp/foo".to_string(),
            "--timeout-secs".to_string(),
            "60".to_string(),
        ];
        let parsed = parse_smoke_argv(&argv).unwrap();
        assert_eq!(parsed.timeout_secs, 60);
    }

    #[test]
    fn smoke_argv_rejects_missing_data_dir() {
        let argv = vec!["skattr-ui".to_string(), "--smoke-test".to_string()];
        let err = parse_smoke_argv(&argv).unwrap_err();
        assert!(err.contains("--data-dir"));
    }

    #[test]
    fn smoke_argv_returns_none_when_flag_absent() {
        let argv = vec!["skattr-ui".to_string()];
        let result = detect_smoke_test_flag(&argv);
        assert!(!result, "no flag => false");
    }

    #[test]
    fn smoke_argv_returns_true_when_flag_present() {
        let argv = vec!["skattr-ui".to_string(), "--smoke-test".to_string()];
        let result = detect_smoke_test_flag(&argv);
        assert!(result, "flag present => true");
    }
}
```

- [ ] **Step 6.2: Run — fails (no parser)**

```bash
cd /home/myggiz/development/skattr-phase-2g
cargo test -p skattr-ui smoke_argv_tests
```

Expected: compile errors for `parse_smoke_argv`, `detect_smoke_test_flag`, and the `SmokeArgs` struct.

- [ ] **Step 6.3: Implement the parser + the argv branch**

Add to `crates/ui/src/main.rs` near the top of the file (after the `mod` declarations, before `CloseToTraySentinel`):

```rust
/// Parsed `--smoke-test` arguments.
struct SmokeArgs {
    data_dir: std::path::PathBuf,
    timeout_secs: u64,
}

fn detect_smoke_test_flag(argv: &[String]) -> bool {
    argv.iter().any(|a| a == "--smoke-test")
}

fn parse_smoke_argv(argv: &[String]) -> Result<SmokeArgs, String> {
    let mut data_dir: Option<std::path::PathBuf> = None;
    let mut timeout_secs: u64 = 240;
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--data-dir" => {
                let v = argv
                    .get(i + 1)
                    .ok_or_else(|| "--data-dir requires a value".to_string())?;
                data_dir = Some(std::path::PathBuf::from(v));
                i += 2;
            }
            "--timeout-secs" => {
                let v = argv
                    .get(i + 1)
                    .ok_or_else(|| "--timeout-secs requires a value".to_string())?;
                timeout_secs = v
                    .parse::<u64>()
                    .map_err(|e| format!("--timeout-secs not a number: {e}"))?;
                i += 2;
            }
            _ => i += 1,
        }
    }
    Ok(SmokeArgs {
        data_dir: data_dir.ok_or_else(|| "--smoke-test requires --data-dir".to_string())?,
        timeout_secs,
    })
}

/// Argv branch invoked from `main()` when `--smoke-test` is present.
/// Builds a `SmokeConfig`, runs `core::daemon::smoke::run_smoke` on a
/// fresh Tokio runtime, prints the report (or error), and exits.
fn run_smoke_and_exit(argv: Vec<String>) -> ! {
    let parsed = match parse_smoke_argv(&argv) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("smoke: argv error: {e}");
            std::process::exit(2);
        }
    };
    let cfg = skattr_core::daemon::smoke::SmokeConfig {
        data_dir: parsed.data_dir,
        tor_ready_timeout: std::time::Duration::from_secs(parsed.timeout_secs),
        ..Default::default()
    };
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("smoke: tokio runtime build failed: {e}");
            std::process::exit(1);
        }
    };
    match runtime.block_on(skattr_core::daemon::smoke::run_smoke(cfg)) {
        Ok(report) => {
            println!(
                "smoke OK: onion={} duration={:?}",
                report.onion, report.duration
            );
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("smoke FAIL: {e}");
            std::process::exit(1);
        }
    }
}
```

Modify the existing `fn main()` to branch on the smoke flag *before* `tracing_subscriber` is initialised (smoke logs to stderr/stdout directly, no ring buffer):

Replace the first lines of `fn main()`:

```rust
fn main() {
    use skattr_core::daemon::logs::{LogSink, RingBufferLayer};
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
```

with:

```rust
fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if detect_smoke_test_flag(&argv) {
        run_smoke_and_exit(argv);
    }

    use skattr_core::daemon::logs::{LogSink, RingBufferLayer};
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
```

- [ ] **Step 6.4: Run tests — should pass**

```bash
cd /home/myggiz/development/skattr-phase-2g
cargo test -p skattr-ui smoke_argv_tests
```

Expected: 5 tests pass.

- [ ] **Step 6.5: Run clippy**

```bash
cargo clippy -p skattr-ui --all-targets --all-features -- -D warnings
```

Expected: clean.

- [ ] **Step 6.6: Smoke the smoke flag locally**

```bash
SMOKE_TMP=$(mktemp -d -t skattr-smoke-XXXXXX)
cargo run -p skattr-ui --release -- --smoke-test --data-dir "$SMOKE_TMP" --timeout-secs 240
echo "exit=$?"
ls -la "$SMOKE_TMP"
rm -rf "$SMOKE_TMP"
```

Expected: prints `smoke OK: onion=… duration=…`, exits 0, `identity.vault` was created in the tmp dir.

- [ ] **Step 6.7: Commit**

```bash
git add crates/ui/src/main.rs
git commit -m "ui(smoke): branch on --smoke-test argv before Tauri::Builder

Bypasses webview entirely; calls into core::daemon::smoke::run_smoke
on a fresh Tokio runtime and exits 0/1 based on the report.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: CLI escape hatch — `skattr daemon --smoke-test`

**Files:**
- Modify: `crates/cli/src/main.rs`

- [ ] **Step 7.1: Add the flag to the existing `Daemon` clap variant**

In `crates/cli/src/main.rs`, locate:

```rust
    /// Start the daemon (Tor bootstrap + onion publish + accept loop).
    Daemon {
        /// Detach to a background process after startup.
        #[arg(long)]
        detach: bool,
    },
```

Replace with:

```rust
    /// Start the daemon (Tor bootstrap + onion publish + accept loop).
    Daemon {
        /// Detach to a background process after startup.
        #[arg(long)]
        detach: bool,
        /// Run a one-shot smoke test (init throwaway vault, boot,
        /// wait for Tor::Ready, exit 0). For CI release smoke; not
        /// for real use.
        #[arg(long)]
        smoke_test: bool,
        /// Smoke-only: timeout for Tor::Ready. Ignored without
        /// `--smoke-test`.
        #[arg(long, value_name = "SECS", default_value_t = 240)]
        smoke_timeout_secs: u64,
    },
```

- [ ] **Step 7.2: Update the match arm**

Locate:

```rust
        Command::Daemon { detach } => {
            daemon(detach, cli.data_dir.as_deref(), passphrase_file, log_sink).await
        }
```

Replace with:

```rust
        Command::Daemon {
            detach,
            smoke_test,
            smoke_timeout_secs,
        } => {
            if smoke_test {
                cli_smoke(cli.data_dir.as_deref(), smoke_timeout_secs).await
            } else {
                daemon(detach, cli.data_dir.as_deref(), passphrase_file, log_sink).await
            }
        }
```

- [ ] **Step 7.3: Add the `cli_smoke` function**

Add at the end of `crates/cli/src/main.rs` (before the `#[cfg(test)] mod tests`):

```rust
/// Invoke the same smoke entry point the UI uses, from the CLI.
async fn cli_smoke(
    data_dir_override: Option<&std::path::Path>,
    timeout_secs: u64,
) -> Result<()> {
    use skattr_core::daemon::smoke::{run_smoke, SmokeConfig};

    let data_dir = match data_dir_override {
        Some(d) => d.to_path_buf(),
        None => {
            // No override -> create a fresh tempdir (dev escape hatch
            // -- never use a real default, that would clobber state).
            tempfile::tempdir()?.into_path()
        }
    };
    let cfg = SmokeConfig {
        data_dir,
        tor_ready_timeout: std::time::Duration::from_secs(timeout_secs),
        ..Default::default()
    };
    match run_smoke(cfg).await {
        Ok(report) => {
            println!(
                "smoke OK: onion={} duration={:?}",
                report.onion, report.duration
            );
            Ok(())
        }
        Err(e) => {
            eprintln!("smoke FAIL: {e}");
            std::process::exit(1);
        }
    }
}
```

Add `tempfile = "3"` to `crates/cli/Cargo.toml` `[dependencies]` if not already present. Run:

```bash
grep -n tempfile /home/myggiz/development/skattr-phase-2g/crates/cli/Cargo.toml
```

If it's only in `[dev-dependencies]`, move it (or duplicate the entry) into `[dependencies]`. The `tempfile::tempdir()` call in `cli_smoke` runs in the production binary path.

- [ ] **Step 7.4: Run clippy + tests**

```bash
cd /home/myggiz/development/skattr-phase-2g
cargo clippy -p skattr-cli --all-targets --all-features -- -D warnings
cargo test -p skattr-cli
```

Expected: clean.

- [ ] **Step 7.5: Smoke the CLI flag**

```bash
. "$HOME/.cargo/env"
SMOKE_TMP=$(mktemp -d -t skattr-cli-smoke-XXXXXX)
cargo run -p skattr-cli --release -- --data-dir "$SMOKE_TMP" daemon --smoke-test
echo "exit=$?"
rm -rf "$SMOKE_TMP"
```

Expected: prints `smoke OK …`, exits 0.

- [ ] **Step 7.6: Commit**

```bash
git add crates/cli/src/main.rs crates/cli/Cargo.toml
git commit -m "cli(smoke): add --smoke-test escape hatch on the daemon subcommand

Reuses core::daemon::smoke::run_smoke. Developer convenience for
local smoke testing; the canonical release-pipeline path goes
through skattr-ui.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: Integration test for the bundled smoke flag

**Files:**
- Create: `crates/tests/src/smoke_flag.rs`
- Modify: `crates/tests/src/lib.rs`

- [ ] **Step 8.1: Write the integration test**

Create `crates/tests/src/smoke_flag.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Phase 2.G: end-to-end test of `skattr-ui --smoke-test`. Spawns the
//! UI binary as a subprocess (so the argv branch + tokio runtime
//! creation paths are exercised exactly as in CI release smoke), waits
//! for it to exit, and asserts the data_dir was correctly populated.
//!
//! `#[ignore]`-gated; spawns real Arti.
//!
//! Run with:
//!
//! ```bash
//! cargo test -p skattr-tests --release -- --ignored smoke_flag
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;
use std::time::{Duration, Instant};

#[test]
#[ignore = "spawns real Arti; run with: cargo test -p skattr-tests --release -- --ignored smoke_flag"]
fn skattr_ui_smoke_test_exits_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_path_buf();

    // Locate the skattr-ui binary in the workspace target dir.
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            // crates/tests -> ../../target
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(2)
                .unwrap()
                .join("target")
        });
    let bin = target_dir.join("release").join("skattr-ui");
    assert!(
        bin.exists(),
        "skattr-ui release binary missing at {}",
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
    assert!(stdout.contains("smoke OK"), "stdout missing 'smoke OK': {stdout}");

    // Vault should have been created.
    assert!(
        data_dir.join("identity.vault").exists(),
        "smoke must leave identity.vault at {}",
        data_dir.display()
    );
}
```

- [ ] **Step 8.2: Wire the new file into `crates/tests/src/lib.rs`**

Add (alphabetical position):

```rust
#[cfg(test)]
mod smoke_flag;
```

- [ ] **Step 8.3: Build the release binary first (smoke test depends on it)**

```bash
cd /home/myggiz/development/skattr-phase-2g
cargo build -p skattr-ui --release
```

Expected: clean build.

- [ ] **Step 8.4: Run the smoke integration test**

```bash
cargo test -p skattr-tests --release -- --ignored smoke_flag
```

Expected: passes within ~260s.

- [ ] **Step 8.5: Commit**

```bash
git add crates/tests/src/smoke_flag.rs crates/tests/src/lib.rs
git commit -m "tests(smoke): integration coverage for skattr-ui --smoke-test

Spawns the release binary as a subprocess and asserts exit 0 +
identity.vault present. #[ignore]-gated; run with:
cargo test -p skattr-tests --release -- --ignored smoke_flag

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 9: Generate missing icon sizes

**Files:**
- Create: `crates/ui/icons/16x16.png`
- Create: `crates/ui/icons/64x64.png`
- Create: `crates/ui/icons/256x256.png`
- Create: `crates/ui/icons/512x512.png`
- Create: `crates/ui/icons/icon.svg` (master, optional)

- [ ] **Step 9.1: Generate from the existing 128x128.png as the source**

The repo currently has `32x32.png`, `128x128.png`, `icon.icns`, `icon.ico` in `crates/ui/icons/`. We need 16x16, 64x64, 256x256, 512x512 PNGs to match the bundle metadata in Task 10.

Use ImageMagick (`convert` or `magick`) — available on most Linux distros. From `/home/myggiz/development/skattr-phase-2g`:

```bash
cd crates/ui/icons
which magick || which convert
# Use the larger source (128x128) for upscale targets and the smaller for downscale, or
# all-from-128 if quality is OK.
magick 128x128.png -resize 16x16    16x16.png
magick 128x128.png -resize 64x64    64x64.png
magick 128x128.png -resize 256x256  256x256.png
magick 128x128.png -resize 512x512  512x512.png
ls -la
```

If only `convert` (legacy ImageMagick 6) is available, swap `magick` for `convert`.

If neither is available, install with:

```bash
sudo pacman -S imagemagick     # Arch/Manjaro
# or:
sudo apt-get install imagemagick   # Debian/Ubuntu
```

Verify the resulting files are valid PNGs:

```bash
file 16x16.png 64x64.png 256x256.png 512x512.png
```

Expected: each line says "PNG image data, NxN, …".

- [ ] **Step 9.2: Commit (no SVG yet — leave that for a future polish pass)**

```bash
cd /home/myggiz/development/skattr-phase-2g
git add crates/ui/icons/16x16.png crates/ui/icons/64x64.png crates/ui/icons/256x256.png crates/ui/icons/512x512.png
git commit -m "ui(icons): add 16/64/256/512 PNG sizes for bundle metadata

Phase 2.G spec §bundle-metadata locks six PNG sizes; the existing
32 + 128 stayed and the four new sizes are downscaled from the
existing 128x128 source via ImageMagick.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 10: Tauri bundle config — metadata, icons, OS-specific options, updater disabled

**Files:**
- Modify: `crates/ui/tauri.conf.json`

- [ ] **Step 10.1: Replace `crates/ui/tauri.conf.json` with the expanded config**

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "Skattr",
  "version": "0.0.1",
  "identifier": "net.myggiz.skattr",
  "build": {
    "frontendDist": "../src-svelte/build",
    "devUrl": "http://localhost:1420",
    "beforeDevCommand": "pnpm --dir src-svelte dev",
    "beforeBuildCommand": "pnpm --dir src-svelte build"
  },
  "app": {
    "windows": [
      {
        "title": "Skattr",
        "width": 1100,
        "height": 720,
        "minWidth": 720,
        "minHeight": 480,
        "decorations": true,
        "resizable": true
      }
    ],
    "security": {
      "csp": "default-src 'self'; img-src 'self' data:; font-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self' ipc: tauri:; script-src 'self'"
    }
  },
  "bundle": {
    "active": true,
    "publisher": "Myggiz B.V.",
    "copyright": "© 2026 Myggiz B.V.",
    "license": "GPL-3.0-or-later",
    "licenseFile": "../../LICENSE-GPL3",
    "shortDescription": "Metadata-resistant P2P encrypted messenger.",
    "longDescription": "Skattr is a desktop-first, metadata-resistant P2P encrypted messenger. All traffic over Tor v3 onion services. MLS for message encryption.",
    "category": "SocialNetworking",
    "icon": [
      "icons/16x16.png",
      "icons/32x32.png",
      "icons/64x64.png",
      "icons/128x128.png",
      "icons/256x256.png",
      "icons/512x512.png",
      "icons/icon.ico",
      "icons/icon.icns"
    ],
    "linux": {
      "deb": {
        "depends": [
          "libwebkit2gtk-4.1-0",
          "libayatana-appindicator3-1"
        ],
        "section": "net",
        "priority": "optional"
      },
      "appimage": {
        "bundleMediaFramework": false
      }
    },
    "macOS": {
      "minimumSystemVersion": "12.0",
      "category": "public.app-category.social-networking"
    }
  },
  "plugins": {
    "updater": {
      "active": false
    }
  }
}
```

Note: the `licenseFile` path is relative to `crates/ui/tauri.conf.json` — `../../LICENSE-GPL3` resolves to `<repo>/LICENSE-GPL3`.

- [ ] **Step 10.2: Run a UI build to verify the config parses**

```bash
cd /home/myggiz/development/skattr-phase-2g
cargo build -p skattr-ui
```

Expected: clean. (The `tauri-build` crate validates `tauri.conf.json` schema at build time.)

- [ ] **Step 10.3: Commit**

```bash
git add crates/ui/tauri.conf.json
git commit -m "ui(bundle): publisher, copyright, license, six icon sizes, deb deps

Locks the bundle metadata per Phase 2.G spec §bundle-metadata.
Tauri updater explicitly disabled so Phase 5's enable is a clean
one-line diff.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 11: `skattr://` URL scheme — Tauri config + plugins

**Files:**
- Modify: `crates/ui/Cargo.toml`
- Modify: `crates/ui/src-svelte/package.json`
- Modify: `crates/ui/tauri.conf.json`
- Modify: `crates/ui/src/main.rs`

This task adds the runtime plumbing for `skattr://invite/v1#…` deep links. Bundle-side declaration goes in `tauri.conf.json`; Linux `.desktop` `MimeType` is generated automatically by Tauri from the deep-link plugin's config; macOS `Info.plist` `CFBundleURLTypes` likewise.

- [ ] **Step 11.1: Add `tauri-plugin-deep-link` and `tauri-plugin-single-instance` to the Rust crate**

Add to `crates/ui/Cargo.toml` `[dependencies]`:

```toml
tauri-plugin-deep-link = "2"
tauri-plugin-single-instance = { version = "2", features = ["deep-link"] }
```

(Versions track Tauri 2 plugin workspace; consult https://github.com/tauri-apps/plugins-workspace if the resolved version disagrees.)

- [ ] **Step 11.2: Add the JS-side plugin packages**

In `crates/ui/src-svelte/package.json` `dependencies`:

```json
"@tauri-apps/plugin-deep-link": "2.0.0"
```

Run:

```bash
cd /home/myggiz/development/skattr-phase-2g/crates/ui/src-svelte
pnpm install
git diff pnpm-lock.yaml
```

Expected: lockfile updated for the new package.

- [ ] **Step 11.3: Declare the URL scheme in `tauri.conf.json`**

Edit `crates/ui/tauri.conf.json` `plugins` block — replace:

```json
"plugins": {
  "updater": { "active": false }
}
```

with:

```json
"plugins": {
  "updater": { "active": false },
  "deep-link": {
    "desktop": {
      "schemes": ["skattr"]
    }
  }
}
```

And inside `bundle.macOS`, add:

```json
"macOS": {
  "minimumSystemVersion": "12.0",
  "category": "public.app-category.social-networking",
  "urlSchemes": ["skattr"]
}
```

- [ ] **Step 11.4: Wire the plugins in `crates/ui/src/main.rs`**

Inside the `tauri::Builder::default()` chain (right after `.manage(app_state)` and before `.invoke_handler(...)`), add:

```rust
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // Forward subsequent skattr:// invocations into the
            // existing process. The deep-link plugin will fire its
            // own event for any URL in argv; we just need the
            // window to come back into focus.
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
            // The `argv` param surfaces any URL passed at second-launch
            // for platforms that hand it off as a CLI arg.
            tracing::debug!(?argv, "single-instance: forwarded launch");
        }))
        .plugin(tauri_plugin_deep_link::init())
```

And in the `setup` closure (after `crate::tray::install(app.handle())?;`), add:

```rust
            // Forward incoming deep-link events into the SvelteKit
            // shell as a custom DOM event the Add-Contact dialog
            // already listens for.
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let app_for_links = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    let urls: Vec<String> =
                        event.urls().iter().map(|u| u.to_string()).collect();
                    if urls.is_empty() {
                        return;
                    }
                    if let Some(wv) = app_for_links.get_webview_window("main") {
                        let _ = wv.show();
                        let _ = wv.set_focus();
                        // Dispatch a CustomEvent the SvelteKit
                        // add-contact dialog already handles.
                        let payload = serde_json::to_string(&urls[0])
                            .unwrap_or_else(|_| "\"\"".to_string());
                        let js = format!(
                            "window.dispatchEvent(new CustomEvent('skattr:deep-link', {{ detail: {payload} }}));"
                        );
                        let _ = wv.eval(&js);
                    }
                });
            }
```

- [ ] **Step 11.5: Hook the SvelteKit shell to listen for `skattr:deep-link`**

Open `crates/ui/src-svelte/src/routes/+layout.svelte` (or whichever component already wires the Add-Contact dialog). Use Grep to confirm:

```bash
cd /home/myggiz/development/skattr-phase-2g
```

```
Grep pattern="add.contact|AddContact" path="crates/ui/src-svelte/src" type="svelte"
```

Locate the component that owns the `Add Contact` dialog opener (likely `routes/+layout.svelte` or `components/AddContactDialog.svelte`). Add an `onMount` listener that dispatches the dialog open + prefills the URL:

```svelte
<script lang="ts">
  import { onMount, onDestroy } from 'svelte';

  // … existing imports / state …

  let deepLinkHandler: ((e: Event) => void) | null = null;

  onMount(() => {
    deepLinkHandler = (e: Event) => {
      const url = (e as CustomEvent<string>).detail;
      if (typeof url === 'string' && url.startsWith('skattr://invite/v1')) {
        // Trigger the existing add-contact dialog with the URL pre-filled.
        // The exact API depends on how the dialog is opened in 2.E —
        // consult the existing onOpenAddContact() / openAddContactWithUrl().
        openAddContactDialog(url);
      }
    };
    window.addEventListener('skattr:deep-link', deepLinkHandler);
  });

  onDestroy(() => {
    if (deepLinkHandler) {
      window.removeEventListener('skattr:deep-link', deepLinkHandler);
    }
  });
</script>
```

If no `openAddContactDialog(url: string)` helper exists yet, add one — it should mirror the paste-tab handler in `AddContactDialog.svelte`. Do not duplicate logic; export the handler from the component that owns the dialog state and import it at the layout level.

- [ ] **Step 11.6: Build + Vitest**

```bash
cd /home/myggiz/development/skattr-phase-2g
cargo build -p skattr-ui
cd crates/ui/src-svelte && pnpm install --frozen-lockfile && pnpm test
```

Expected: clean build + all Vitest specs pass.

- [ ] **Step 11.7: Manual verification of the URL scheme handler**

Build the bundle locally, install the `.deb`, then run:

```bash
xdg-open 'skattr://invite/v1#id=AAAA'
```

Expected: Skattr launches (or focuses if already running), Add-Contact dialog opens with the URL pre-filled.

If `xdg-open` doesn't dispatch to skattr, check `~/.local/share/applications/Skattr.desktop` for `MimeType=x-scheme-handler/skattr;`. The Tauri deep-link plugin should add this; if it doesn't, file a follow-up.

- [ ] **Step 11.8: Commit**

```bash
git add crates/ui/Cargo.toml crates/ui/tauri.conf.json crates/ui/src/main.rs \
        crates/ui/src-svelte/package.json crates/ui/src-svelte/pnpm-lock.yaml \
        crates/ui/src-svelte/src/routes/+layout.svelte
git commit -m "ui(deep-link): register skattr:// URL scheme

Adds tauri-plugin-deep-link + tauri-plugin-single-instance.
Bundle metadata declares the scheme on Linux and macOS; runtime
forwards skattr://invite/v1#... into the existing AddContact
dialog via a custom DOM event.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 12: Generate the minisign keypair (manual, off-CI)

**Files:**
- Create: `docs/install/minisign.pub`

**This task runs on the maintainer's machine, not in CI.** The secret key never enters the repo or CI logs.

- [ ] **Step 12.1: Install minisign locally if not already present**

```bash
which minisign || sudo pacman -S minisign     # Arch
# or:
which minisign || sudo apt-get install minisign   # Debian/Ubuntu
# or:
which minisign || brew install minisign           # macOS
```

- [ ] **Step 12.2: Generate the keypair (passphrase-protected)**

```bash
mkdir -p ~/.private
minisign -G \
    -p /home/myggiz/development/skattr-phase-2g/docs/install/minisign.pub \
    -s ~/.private/skattr-minisign-secret.key
```

Choose a strong passphrase. Record it in your password manager — you'll need it to set the `MINISIGN_PASSWORD` GitHub Actions secret in step 12.4.

- [ ] **Step 12.3: Verify the public key file**

```bash
cat /home/myggiz/development/skattr-phase-2g/docs/install/minisign.pub
```

Expected: starts with `untrusted comment:` then a base64-looking pubkey line.

- [ ] **Step 12.4: Set GitHub Actions secrets**

The maintainer (Roberth Lindholm) opens the `myggiz/skattr` repo settings → Secrets and variables → Actions → New repository secret, and adds:

- Name: `MINISIGN_SECRET_KEY`
  Value: `base64 -w0 ~/.private/skattr-minisign-secret.key` (the encoded encrypted-key file)
- Name: `MINISIGN_PASSWORD`
  Value: the passphrase from step 12.2

Verify:

```bash
gh secret list --repo myggiz/skattr | grep MINISIGN
```

Expected: both secrets listed.

- [ ] **Step 12.5: Commit the public key**

```bash
cd /home/myggiz/development/skattr-phase-2g
git add docs/install/minisign.pub
git commit -m "release(minisign): commit the public key for SHA256SUMS verification

Phase 2.G release flow signs SHA256SUMS with this key. Secret key
is held by the maintainer offline; CI accesses it via GitHub
Actions secrets MINISIGN_SECRET_KEY + MINISIGN_PASSWORD.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 13: CI release workflow — build job

**Files:**
- Create: `.github/workflows/release.yml`

- [ ] **Step 13.1: Create the workflow with the build matrix**

Create `.github/workflows/release.yml`:

```yaml
name: Release

on:
  push:
    tags:
      - 'v*'
  workflow_dispatch:
    inputs:
      dry_run:
        description: 'Build + smoke only; skip the GitHub Release step'
        required: false
        default: 'true'
        type: choice
        options: ['true', 'false']

env:
  CARGO_TERM_COLOR: always
  RUSTFLAGS: -D warnings
  RUST_BACKTRACE: 1
  # Phase 2.G locked decision 4: pinned via rust-toolchain.toml.
  # Phase 2.G locked decision 5: bundle the same source-date-epoch
  # so multiple builders converge on identical timestamps in the
  # bundle metadata.

jobs:
  build:
    name: build (${{ matrix.os }})
    strategy:
      fail-fast: false
      matrix:
        # Phase 2.G ships Linux + macOS only.
        # Windows is carved out to Phase 2.H (see ADR / spec).
        os: [ubuntu-latest, macos-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - name: Compute SOURCE_DATE_EPOCH from the tag commit
        id: sde
        shell: bash
        run: |
          set -euo pipefail
          SDE=$(git log -1 --format=%ct)
          echo "epoch=$SDE" >> "$GITHUB_OUTPUT"
          echo "SOURCE_DATE_EPOCH=$SDE" >> "$GITHUB_ENV"

      - name: Install Tauri 2 Linux deps
        if: matrix.os == 'ubuntu-latest'
        run: |
          sudo apt-get update
          sudo apt-get install -y --no-install-recommends \
            libwebkit2gtk-4.1-dev \
            libayatana-appindicator3-dev \
            librsvg2-dev \
            libsoup-3.0-dev \
            libjavascriptcoregtk-4.1-dev \
            libxdo-dev \
            libssl-dev \
            build-essential \
            pkg-config

      - uses: dtolnay/rust-toolchain@stable

      - uses: Swatinem/rust-cache@v2
        with:
          key: release-${{ matrix.os }}

      - uses: actions/setup-node@v4
        with:
          node-version: 20

      - name: Activate pnpm via corepack
        run: |
          corepack enable
          corepack prepare pnpm@10 --activate

      - name: Cache pnpm store
        uses: actions/cache@v4
        with:
          path: ~/.local/share/pnpm/store
          key: pnpm-${{ runner.os }}-${{ hashFiles('crates/ui/src-svelte/pnpm-lock.yaml') }}
          restore-keys: |
            pnpm-${{ runner.os }}-

      - name: Generate ts-rs bindings
        run: cargo test -p skattr-core --features test-harness

      - name: Install frontend deps
        working-directory: crates/ui/src-svelte
        run: pnpm install --frozen-lockfile

      - name: Build frontend
        working-directory: crates/ui/src-svelte
        run: pnpm build

      - name: Install tauri-cli
        run: cargo install tauri-cli --version '=2.11.0' --locked

      - name: Build bundle
        run: cargo tauri build
        env:
          # macOS-side: don't open the simulator / xcrun.
          CI: true

      - name: Stage bundles
        shell: bash
        run: |
          set -euo pipefail
          mkdir -p stage
          if [[ "${{ matrix.os }}" == "ubuntu-latest" ]]; then
            cp target/release/bundle/deb/*.deb stage/
            cp target/release/bundle/appimage/*.AppImage stage/
          else
            cp target/release/bundle/dmg/*.dmg stage/
          fi
          ls -la stage

      - name: Upload bundles
        uses: actions/upload-artifact@v4
        with:
          name: bundles-${{ matrix.os }}
          path: stage/*
          if-no-files-found: error
          retention-days: 7
```

- [ ] **Step 13.2: Run a syntax check via `act` or push to a feature branch**

`act` is optional; the canonical check is to push the branch and observe a `workflow_dispatch` invocation:

```bash
cd /home/myggiz/development/skattr-phase-2g
git add .github/workflows/release.yml
git commit -m "ci(release): build matrix on Linux + macOS

Phase 2.G locked decision 6 amended: Windows carved to Phase 2.H.
Build job per matrix OS; Tauri-CLI pinned to =2.11.0; bundle
artefacts staged and uploaded as bundles-\${{ matrix.os }}.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
git push -u origin phase-2g-packaging
```

Then on GitHub: Actions → Release → Run workflow on `phase-2g-packaging` (workflow_dispatch). Expected: the `build` job completes successfully, bundles appear as artefacts.

- [ ] **Step 13.3: Iterate on any missed dependencies**

If the build job fails on Linux for a missing apt package, add it to the apt-get list and re-push. Common culprits: `libgtk-3-dev` (sometimes needed alongside webkit2gtk-4.1), `wget` (for AppImage tooling).

If the build job fails on macOS for missing tauri-cli binaries (e.g. `dmgbuild`), check the cargo-install line — Tauri 2.11 may want `tauri-cli@2.x.y` distinct from the Rust-side Tauri patch.

---

## Task 14: CI release workflow — smoke job

**Files:**
- Modify: `.github/workflows/release.yml`

- [ ] **Step 14.1: Append the smoke job to `release.yml`**

Add after the `build` job (still under `jobs:`):

```yaml
  smoke:
    name: smoke (${{ matrix.os }})
    needs: build
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - name: Download bundles
        uses: actions/download-artifact@v4
        with:
          name: bundles-${{ matrix.os }}
          path: bundles

      - name: List bundles
        shell: bash
        run: ls -la bundles

      - name: Install bundle (Linux .deb)
        if: matrix.os == 'ubuntu-latest'
        shell: bash
        run: |
          set -euo pipefail
          sudo apt-get update
          sudo apt-get install -y libwebkit2gtk-4.1-0 libayatana-appindicator3-1
          sudo dpkg -i bundles/*.deb || sudo apt-get -f install -y
          which skattr-ui

      - name: Install bundle (macOS .dmg)
        if: matrix.os == 'macos-latest'
        shell: bash
        run: |
          set -euo pipefail
          DMG=$(ls bundles/*.dmg | head -1)
          hdiutil attach "$DMG" -nobrowse -mountpoint /Volumes/skattr-mount
          cp -R /Volumes/skattr-mount/Skattr.app /tmp/Skattr.app
          hdiutil detach /Volumes/skattr-mount
          ls /tmp/Skattr.app/Contents/MacOS/

      - name: Run smoke (Linux)
        if: matrix.os == 'ubuntu-latest'
        shell: bash
        run: |
          set -euo pipefail
          mkdir -p "$RUNNER_TEMP/smoke"
          # The Linux .deb installs skattr-ui at /usr/bin/skattr-ui.
          /usr/bin/skattr-ui --smoke-test \
              --data-dir "$RUNNER_TEMP/smoke" \
              --timeout-secs 240
          echo "smoke OK on ubuntu-latest" >> "$GITHUB_STEP_SUMMARY"

      - name: Run smoke (macOS)
        if: matrix.os == 'macos-latest'
        shell: bash
        run: |
          set -euo pipefail
          mkdir -p "$RUNNER_TEMP/smoke"
          /tmp/Skattr.app/Contents/MacOS/skattr-ui --smoke-test \
              --data-dir "$RUNNER_TEMP/smoke" \
              --timeout-secs 240
          echo "smoke OK on macos-latest" >> "$GITHUB_STEP_SUMMARY"

      - name: Run smoke (Linux AppImage)
        if: matrix.os == 'ubuntu-latest'
        shell: bash
        run: |
          set -euo pipefail
          APPIMG=$(ls bundles/*.AppImage | head -1)
          chmod +x "$APPIMG"
          mkdir -p "$RUNNER_TEMP/smoke-appimage"
          # AppImage forwards CLI args to the embedded binary.
          "$APPIMG" --appimage-extract-and-run \
              --smoke-test \
              --data-dir "$RUNNER_TEMP/smoke-appimage" \
              --timeout-secs 240
          echo "AppImage smoke OK on ubuntu-latest" >> "$GITHUB_STEP_SUMMARY"

      - name: Upload smoke logs on failure
        if: failure()
        uses: actions/upload-artifact@v4
        with:
          name: smoke-failure-${{ matrix.os }}
          path: |
            ${{ runner.temp }}/smoke/**
            ${{ runner.temp }}/smoke-appimage/**
          if-no-files-found: ignore
          retention-days: 14
```

- [ ] **Step 14.2: Verify the smoke job runs on workflow_dispatch**

```bash
git add .github/workflows/release.yml
git commit -m "ci(release): smoke each bundle after install

Linux: dpkg -i + run /usr/bin/skattr-ui --smoke-test, plus an
AppImage --appimage-extract-and-run pass.
macOS: hdiutil mount + run skattr-ui inside the .app.
Failures upload \$RUNNER_TEMP/smoke as smoke-failure-{os} artefact.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
git push
```

Trigger Run workflow → workflow_dispatch from the GitHub UI. Expected: build + smoke jobs all green.

- [ ] **Step 14.3: Iterate on smoke failures**

The most likely failure modes:
- AppImage doesn't forward args via `--appimage-extract-and-run` on some runner kernels — fall back to `chmod +x && ./bundle.AppImage --smoke-test …` directly.
- macOS Tor bootstrap is slow — bump `--timeout-secs 360` if observed.

---

## Task 15: CI release workflow — release job (minisign + GH Release)

**Files:**
- Modify: `.github/workflows/release.yml`

- [ ] **Step 15.1: Add the `release` job**

Append to `.github/workflows/release.yml` under `jobs:`:

```yaml
  release:
    name: release
    needs: smoke
    runs-on: ubuntu-latest
    if: github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@v4

      - name: Download all bundle artefacts
        uses: actions/download-artifact@v4
        with:
          pattern: bundles-*
          path: dist
          merge-multiple: true

      - name: List bundles
        shell: bash
        run: ls -la dist

      - name: Compute SHA256SUMS
        shell: bash
        working-directory: dist
        run: |
          set -euo pipefail
          sha256sum *.deb *.AppImage *.dmg | sort > SHA256SUMS
          cat SHA256SUMS

      - name: Install minisign
        run: sudo apt-get update && sudo apt-get install -y minisign

      - name: Sign SHA256SUMS
        shell: bash
        working-directory: dist
        env:
          MINISIGN_SECRET_KEY: ${{ secrets.MINISIGN_SECRET_KEY }}
          MINISIGN_PASSWORD: ${{ secrets.MINISIGN_PASSWORD }}
        run: |
          set -eu
          # Disable command echo so the password never leaks.
          set +x
          KEY_FILE=$(mktemp -p "$RUNNER_TEMP")
          printf '%s' "$MINISIGN_SECRET_KEY" | base64 -d > "$KEY_FILE"
          # minisign reads passphrase from stdin when -W is not set.
          printf '%s\n' "$MINISIGN_PASSWORD" | minisign -Sm SHA256SUMS -s "$KEY_FILE"
          shred -u "$KEY_FILE"
          ls -la SHA256SUMS SHA256SUMS.minisig

      - name: Verify the signature (sanity-check)
        shell: bash
        working-directory: dist
        run: |
          set -euo pipefail
          minisign -Vm SHA256SUMS -P "$(cat ../docs/install/minisign.pub | tail -1)"

      - name: Extract release notes from CHANGELOG
        shell: bash
        run: |
          set -euo pipefail
          TAG="${GITHUB_REF#refs/tags/}"
          # Extract the section for the current tag from CHANGELOG.md.
          # Convention: each release is "## [TAG] — DATE" followed by bullets.
          awk -v tag="$TAG" '
            $0 ~ "^## \\[" tag "\\]" { in_section = 1; print; next }
            in_section && $0 ~ "^## \\[" { exit }
            in_section { print }
          ' CHANGELOG.md > release-notes.md
          if [ ! -s release-notes.md ]; then
            echo "## $TAG" > release-notes.md
            echo "" >> release-notes.md
            echo "(release notes not found in CHANGELOG.md)" >> release-notes.md
          fi
          cat release-notes.md

      - name: Create GitHub Release
        uses: softprops/action-gh-release@v2
        with:
          files: |
            dist/*.deb
            dist/*.AppImage
            dist/*.dmg
            dist/SHA256SUMS
            dist/SHA256SUMS.minisig
          body_path: release-notes.md
          draft: false
          prerelease: false
```

- [ ] **Step 15.2: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci(release): minisign-sign SHA256SUMS and create GH Release

Triggered only on push of a v* tag (workflow_dispatch stops at
smoke). Secret key is base64-decoded into a tmpfs file, used,
then shred-unlinked. Pubkey verification step double-checks
the signature before the upload.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
git push
```

- [ ] **Step 15.3: Optional dry-run via `workflow_dispatch`**

The `release` job has `if: github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')`. For dry runs (manual dispatch), build + smoke run but `release` skips. The first real test of the release job comes from a real `v0.1.0-rc1` tag — expect to iterate on minor formatting issues.

---

## Task 16: Install docs — top-level README

**Files:**
- Create: `docs/install/README.md`

- [ ] **Step 16.1: Write the doc**

Create `docs/install/README.md`:

````markdown
# Installing Skattr

Skattr is distributed as unsigned bundles whose checksums are
signed with [minisign](https://jedisct1.github.io/minisign/).
Verify both before running the binary on any machine where you
care about provenance.

## Verification flow

Every Skattr release attaches three "supply-chain" files alongside
the platform bundles:

- `SHA256SUMS` — one line per bundle: `<hash>  <filename>`.
- `SHA256SUMS.minisig` — minisign signature over `SHA256SUMS`.
- (the bundles themselves)

The skattr maintainer's minisign public key is committed in this
repository at `docs/install/minisign.pub`. The same key is also
displayed below for offline reference:

```
TODO: paste the contents of docs/install/minisign.pub here at v0.1.0 cut
```

### Step 1 — download

From the [Releases page](https://github.com/myggiz/skattr/releases),
pull the bundle for your platform plus `SHA256SUMS` and
`SHA256SUMS.minisig`.

### Step 2 — verify the bundle hash

```bash
sha256sum -c SHA256SUMS --ignore-missing
```

Expected output (one matching line per file you actually
downloaded):

```
skattr_0.0.1_amd64.deb: OK
```

If you see `FAILED`, **do not run the binary**. Re-download or
file an issue.

### Step 3 — verify the minisign signature

Save the public key above to a file (or use the one in this repo):

```bash
minisign -Vm SHA256SUMS -P "$(cat path/to/minisign.pub)"
```

Expected: `Signature and comment signature verified`.

If the signature does not verify, the `SHA256SUMS` file was
tampered with after the maintainer signed it. **Do not run the
binary.** Report the discrepancy.

### Step 4 — install + first run

See the per-platform docs:

- [Linux](linux.md) — `.deb`, AppImage, Flatpak (build-from-source).
- [macOS](macos.md) — `.dmg`.
- Windows — deferred to Phase 2.H (see project status in `CLAUDE.md`).

## Why minisign and not GPG?

Minisign signatures are 116 bytes; the public key is 56 bytes.
Verification is a single Ed25519 check with no Web-of-Trust /
keyserver coordination. This is enough for "the same person who
controls this GitHub repo signed this release" without the
moving parts of OpenPGP.

GPG support is on the roadmap but not required for v0.1.

## Key rotation

If the maintainer's minisign key is compromised or rotated, the
new public key will be:

1. Committed at `docs/install/minisign.pub` (same path).
2. Signed by the *old* key in a `SHA256SUMS.minisig` of a
   transition release, alongside a `KEYROTATION.md` document
   that explains what changed and when.

Until that document is published, treat the in-repo public key
as the authoritative one.
````

- [ ] **Step 16.2: Replace the `TODO:` placeholder**

```bash
cd /home/myggiz/development/skattr-phase-2g
PUBKEY=$(cat docs/install/minisign.pub)
# Edit docs/install/README.md and paste the actual key into the code block.
# (Manual edit; alternatively use sed if confident in escaping.)
```

Open `docs/install/README.md`, find the placeholder block, and replace it with the contents of `docs/install/minisign.pub` (verbatim, including the `untrusted comment:` lines).

- [ ] **Step 16.3: Commit**

```bash
git add docs/install/README.md
git commit -m "docs(install): top-level verification flow

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 17: Install docs — Linux

**Files:**
- Create: `docs/install/linux.md`

- [ ] **Step 17.1: Write `docs/install/linux.md`**

```markdown
# Installing Skattr on Linux

Skattr ships three Linux variants in the same release:

| Format | When to use | Install |
|--------|-------------|---------|
| `.deb`     | Debian, Ubuntu, Mint, Pop!_OS, anything apt-based. Integrates with apt; future apt-style updates are simplest. | `sudo apt install ./skattr_<version>_amd64.deb` |
| AppImage   | Single-file portable; works on any glibc-based distro. No installation; just `chmod +x` and run. | `chmod +x Skattr_<version>_amd64.AppImage && ./Skattr_<version>_amd64.AppImage` |
| Flatpak (build-from-source) | Strongest sandboxing; best for hostile-network environments. Requires `flatpak-builder` and a working network. | See "Flatpak" below. |

Verify the bundle first using the steps in
[`docs/install/README.md`](README.md). The instructions below
assume you've already done so.

## Required runtime libraries

Tauri 2 + WebKitGTK 4.1 require:

- `libwebkit2gtk-4.1-0` (≥ 2.40)
- `libayatana-appindicator3-1`

The `.deb` declares these as dependencies; on AppImage you may need
to install them manually if the host distro is older. On Fedora 39+
the equivalents are `webkit2gtk4.1` and `libayatana-appindicator-gtk3`.

## Wayland tray caveat

Skattr's tray icon uses the StatusNotifier / Ayatana Indicator
protocol. On bare Wayland desktops *without* a StatusNotifier host,
the tray icon will not be displayed; close-to-tray falls back to
"quit on close" (logged at WARN).

Common StatusNotifier hosts:

- **GNOME** — works out-of-the-box (extension required on some
  GNOME versions).
- **KDE Plasma** — works out-of-the-box.
- **Sway** / **Hyprland** — install `waybar` or another
  StatusNotifier-aware bar; the tray icon appears once the bar is
  running.
- **plain Sway / no bar** — close-to-tray falls back to quit.
  This is documented behaviour, not a bug.

## `.deb` install

```bash
sudo apt install ./skattr_<version>_amd64.deb
# or, on a system without apt-add-repository network access:
sudo dpkg -i skattr_<version>_amd64.deb
sudo apt-get install -f       # pull missing deps
```

Launcher: `Skattr` appears in the Applications menu under
"Internet" (`Categories=Network;InstantMessaging;`).

CLI: `skattr-ui` is at `/usr/bin/skattr-ui`. The CLI tool
`skattr` is **not** included in the `.deb`; it ships in a
separate `.deb` planned for a later release.

## AppImage install

```bash
chmod +x Skattr_<version>_amd64.AppImage
./Skattr_<version>_amd64.AppImage
```

Optional desktop integration:

```bash
mkdir -p ~/Applications
mv Skattr_<version>_amd64.AppImage ~/Applications/
~/Applications/Skattr_<version>_amd64.AppImage --appimage-integrate
# or use `appimaged` if installed
```

## Flatpak (build-from-source)

Flathub publication is on the roadmap; for v0.1 you build from
the in-repo manifest:

```bash
git clone https://github.com/myggiz/skattr.git
cd skattr
flatpak install --user flathub org.freedesktop.Platform//23.08 \
                                org.freedesktop.Sdk//23.08 \
                                org.freedesktop.Sdk.Extension.rust-stable//23.08
flatpak-builder --user --install --force-clean build \
    packaging/flatpak/net.myggiz.skattr.yml
flatpak run net.myggiz.skattr
```

Build time: ~10–20 minutes on first run (downloads Rust + Node deps
inside the sandbox).

## `skattr://` URL handler

The `.deb`, AppImage (with `--appimage-integrate`), and Flatpak all
register `skattr://` as a URL scheme handler. Clicking a
`skattr://invite/v1#…` link in your browser launches Skattr (or
focuses an existing window) and opens the Add-Contact dialog with
the URL pre-filled.

To test:

```bash
xdg-open 'skattr://invite/v1#id=AAAA'
```

If this opens Skattr, the handler is live. If it opens a different
app or shows a "no handler" dialog, run:

```bash
xdg-mime default Skattr.desktop x-scheme-handler/skattr
```

## Logs

By default, Skattr keeps an in-memory ring buffer of recent log
records (visible in Settings → Advanced → Logs).
Enable on-disk log persistence in Settings → Advanced; logs are
written to `~/.local/share/skattr/logs/skattr.log` after a daemon
restart.
```

- [ ] **Step 17.2: Commit**

```bash
git add docs/install/linux.md
git commit -m "docs(install): Linux .deb / AppImage / Flatpak guide

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 18: Install docs — macOS

**Files:**
- Create: `docs/install/macos.md`

- [ ] **Step 18.1: Write `docs/install/macos.md`**

````markdown
# Installing Skattr on macOS

Skattr v0.1 is unsigned and unnotarised. macOS Gatekeeper will
warn you on first launch; the workaround is documented below.
Code signing + notarisation are tracked for Phase 5.

## Supported macOS versions

- macOS 12 (Monterey) and newer.
- Apple Silicon only for v0.1. Intel-Mac (`x86_64`) bundles are
  tracked for a follow-up release.

## Verify first

See [`docs/install/README.md`](README.md) for the
SHA256 + minisign verification steps. The rest of this guide
assumes both checks passed.

## Install

1. Double-click `Skattr_<version>_arm64.dmg` to mount it.
2. Drag `Skattr.app` into `/Applications`.
3. Eject the DMG.

## First-launch Gatekeeper warning

The first time you launch `Skattr.app`, macOS shows:

> "Skattr.app" can't be opened because it is from an
> unidentified developer.

This is expected on an unsigned bundle. To bypass:

### Option A — right-click → Open

1. **Right-click** (or Control-click) `Skattr.app` in
   `/Applications`.
2. Choose **Open** from the context menu.
3. macOS shows a slightly different dialog with an **Open**
   button. Click it.
4. macOS remembers your decision; subsequent launches don't
   re-prompt.

### Option B — Terminal (power user)

Strip the quarantine flag:

```bash
xattr -d com.apple.quarantine /Applications/Skattr.app
open /Applications/Skattr.app
```

### Option C — System Settings (macOS 13+)

If the right-click trick fails on an MDM-managed Mac:

1. Try to launch Skattr; the warning dialog appears.
2. Open **System Settings** → **Privacy & Security**.
3. Scroll to the bottom; click **Open Anyway** next to the
   Skattr line.
4. Re-launch.

## What macOS sees

Without notarisation, Skattr is a "downloaded by Safari" app
without a Developer ID. We're aware this is a friction point
and Phase 5 will close it.

For now, the *signature* you should trust is the
minisign signature on `SHA256SUMS` — the same supply-chain
guarantee Linux users get. The macOS Gatekeeper warning is
about Apple's signing chain, which is orthogonal.

## `skattr://` URL handler

The `.dmg` registers `skattr://` as a URL scheme handler. Click
`skattr://invite/v1#…` in any macOS app and Skattr launches
(or focuses) with the Add-Contact dialog open.

To test from Terminal:

```bash
open 'skattr://invite/v1#id=AAAA'
```

## Logs

By default, Skattr keeps an in-memory ring buffer of recent log
records (visible in Settings → Advanced → Logs).
Enable on-disk log persistence in Settings → Advanced; logs are
written to `~/Library/Application Support/skattr/logs/skattr.log`
after a daemon restart.
````

- [ ] **Step 18.2: Commit**

```bash
git add docs/install/macos.md
git commit -m "docs(install): macOS .dmg + Gatekeeper bypass

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 19: Flatpak manifest + AppStream metainfo

**Files:**
- Create: `packaging/flatpak/net.myggiz.skattr.yml`
- Create: `packaging/flatpak/net.myggiz.skattr.metainfo.xml`

- [ ] **Step 19.1: Write the Flatpak manifest**

Create `packaging/flatpak/net.myggiz.skattr.yml`:

```yaml
# Phase 2.G in-repo Flatpak manifest. Builds skattr from a local
# checkout (source: type: dir, path: ../..). Flathub publication is
# deferred — the Flathub manifest variant is documented in
# docs/build/flatpak.md and would substitute a tag-based source.
app-id: net.myggiz.skattr
runtime: org.freedesktop.Platform
runtime-version: '23.08'
sdk: org.freedesktop.Sdk
sdk-extensions:
  - org.freedesktop.Sdk.Extension.rust-stable
  - org.freedesktop.Sdk.Extension.node20
command: skattr-ui
finish-args:
  - --share=network          # Tor connectivity
  - --socket=fallback-x11
  - --socket=wayland
  - --share=ipc
  - --device=dri
  - --socket=session-bus     # tray + notifications via D-Bus
  - --filesystem=xdg-data/skattr:create
  - --filesystem=xdg-config/skattr:create
modules:
  - name: skattr
    buildsystem: simple
    build-options:
      append-path: /usr/lib/sdk/rust-stable/bin:/usr/lib/sdk/node20/bin
      env:
        CARGO_HOME: /run/build/skattr/cargo
        PNPM_HOME: /run/build/skattr/pnpm
    sources:
      - type: dir
        path: ../..
    build-commands:
      - cargo install tauri-cli --version =2.11.0 --root /app
      - corepack enable
      - corepack prepare pnpm@10 --activate
      - cd crates/ui/src-svelte && pnpm install --frozen-lockfile
      - /app/bin/cargo-tauri build --no-bundle --release
      # Install the binary, .desktop, icons, AppStream metadata.
      - install -D target/release/skattr-ui /app/bin/skattr-ui
      - install -D crates/ui/icons/128x128.png /app/share/icons/hicolor/128x128/apps/net.myggiz.skattr.png
      - install -D crates/ui/icons/256x256.png /app/share/icons/hicolor/256x256/apps/net.myggiz.skattr.png
      - install -D crates/ui/icons/512x512.png /app/share/icons/hicolor/512x512/apps/net.myggiz.skattr.png
      - install -D packaging/flatpak/net.myggiz.skattr.metainfo.xml /app/share/metainfo/net.myggiz.skattr.metainfo.xml
      - install -D packaging/flatpak/net.myggiz.skattr.desktop /app/share/applications/net.myggiz.skattr.desktop
```

Note: `packaging/flatpak/net.myggiz.skattr.desktop` is referenced — Tauri's `cargo tauri build` step normally generates a `.desktop` file, but inside the Flatpak sandbox we install a hand-authored one for predictability. Add it as Step 19.3.

- [ ] **Step 19.2: Write the AppStream metainfo**

Create `packaging/flatpak/net.myggiz.skattr.metainfo.xml`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<component type="desktop-application">
  <id>net.myggiz.skattr</id>
  <metadata_license>CC0-1.0</metadata_license>
  <project_license>GPL-3.0-or-later</project_license>
  <name>Skattr</name>
  <summary>Metadata-resistant P2P encrypted messenger</summary>
  <description>
    <p>
      Skattr is a desktop-first, metadata-resistant P2P encrypted
      messenger. All traffic flows over Tor v3 onion services
      (via Arti); message encryption uses MLS (RFC 9420). Identity
      is derived from a BIP39 seed phrase you control.
    </p>
    <p>
      No central server. Mailboxes exist for offline message
      delivery and are semi-trusted: they can see when ciphertext
      moves but never the contents.
    </p>
  </description>
  <launchable type="desktop-id">net.myggiz.skattr.desktop</launchable>
  <provides>
    <binary>skattr-ui</binary>
    <mediatype>x-scheme-handler/skattr</mediatype>
  </provides>
  <url type="homepage">https://skattr.org</url>
  <url type="bugtracker">https://github.com/myggiz/skattr/issues</url>
  <url type="vcs-browser">https://github.com/myggiz/skattr</url>
  <developer_name>Myggiz B.V.</developer_name>
  <categories>
    <category>Network</category>
    <category>InstantMessaging</category>
  </categories>
  <content_rating type="oars-1.1">
    <content_attribute id="social-chat">intense</content_attribute>
  </content_rating>
  <releases>
    <release version="0.0.1" date="2026-05-04">
      <description>
        <p>Phase 2.G: first packaged release. Linux + macOS bundles.</p>
      </description>
    </release>
  </releases>
</component>
```

- [ ] **Step 19.3: Hand-author the `.desktop` file for Flatpak**

Create `packaging/flatpak/net.myggiz.skattr.desktop`:

```
[Desktop Entry]
Type=Application
Version=1.0
Name=Skattr
GenericName=Encrypted Messenger
Comment=Metadata-resistant P2P encrypted messenger
Exec=skattr-ui
Icon=net.myggiz.skattr
Terminal=false
Categories=Network;InstantMessaging;
MimeType=x-scheme-handler/skattr;
StartupNotify=true
```

- [ ] **Step 19.4: Optional local validation (manual; only if `flatpak-builder` is installed)**

```bash
cd /home/myggiz/development/skattr-phase-2g
flatpak install --user flathub org.freedesktop.Platform//23.08 \
                              org.freedesktop.Sdk//23.08 \
                              org.freedesktop.Sdk.Extension.rust-stable//23.08 \
                              org.freedesktop.Sdk.Extension.node20//23.08 || true
flatpak-builder --user --force-clean --install build \
    packaging/flatpak/net.myggiz.skattr.yml
flatpak run net.myggiz.skattr --smoke-test --data-dir /tmp/flatpak-smoke
```

Expected: 10–20 minutes for first build; smoke exits 0 once Tor bootstraps. If the build fails because deps were not pre-fetched (Flatpak sandbox blocks network for `cargo`), add a follow-up note in `docs/build/flatpak.md` mentioning `flatpak-cargo-generator.py` + `flatpak-node-generator.py`.

- [ ] **Step 19.5: Commit**

```bash
git add packaging/flatpak/net.myggiz.skattr.yml \
        packaging/flatpak/net.myggiz.skattr.metainfo.xml \
        packaging/flatpak/net.myggiz.skattr.desktop
git commit -m "packaging(flatpak): in-repo manifest + AppStream + .desktop

Phase 2.G: builds from local source via flatpak-builder. Flathub
publication is deferred — see docs/build/flatpak.md (Task 20)
for the tag-based variant.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 20: Reproducible-build doc + Flatpak doc

**Files:**
- Create: `docs/build/reproducible.md`
- Create: `docs/build/flatpak.md`

- [ ] **Step 20.1: Write `docs/build/reproducible.md`**

```markdown
# Reproducible-build recipe

Phase 2.G's reproducibility goal is **inputs are pinned and the
recipe is documented**, not byte-identical output. Phase 4 closes
the byte-identical claim.

## Recipe

```bash
# 1. Use the pinned toolchain (rust-toolchain.toml does this
#    automatically when a contemporary rustup is installed).
rustup show

# 2. Set SOURCE_DATE_EPOCH from the commit's timestamp.
export SOURCE_DATE_EPOCH=$(git log -1 --format=%ct)

# 3. (Linux only) drop the build-id so binaries don't bake in
#    a timestamp-derived value.
export RUSTFLAGS="-C link-arg=-Wl,--build-id=none"

# 4. Install the pinned tauri-cli.
cargo install tauri-cli --version '=2.11.0' --locked

# 5. Build the frontend deterministically.
pnpm --dir crates/ui/src-svelte install --frozen-lockfile
pnpm --dir crates/ui/src-svelte build

# 6. Build the Tauri bundles.
cargo tauri build
```

## Pinned versions

| Component         | Version pin                           | Lock location                       |
|-------------------|---------------------------------------|-------------------------------------|
| rustc             | exact stable (e.g. 1.84.1)            | `rust-toolchain.toml` `version`     |
| Tauri (Rust)      | `=2.11.0`                             | `crates/ui/Cargo.toml`              |
| `tauri-cli`       | `=2.11.0`                             | `cargo install …` step              |
| Tauri (JS)        | `2.0.0` (exact)                       | `crates/ui/src-svelte/package.json` |
| Node              | `20.x` (LTS)                          | `.github/workflows/release.yml`     |
| pnpm              | `10`                                  | `package.json` `packageManager`     |
| Rust deps         | per `Cargo.lock`                      | committed                           |
| JS deps           | per `pnpm-lock.yaml`                  | committed                           |

## Caveats

- **WebKit / WKWebView versions are platform-supplied** and not
  pinned by this recipe. Upgrading WebKitGTK on the build host
  changes the runtime behaviour even if the bundle is bit-identical.
- **GLIBC** (Linux): the build host's glibc minor version sets the
  AppImage's effective floor.
- **macOS SDK**: the bundle's `LC_BUILD_VERSION` reflects the
  Xcode SDK version of the build host. Two builds on different
  Xcode versions will not be byte-identical.
- **Cargo.lock + pnpm-lock.yaml are CI-enforced** (already true
  from earlier phases) — every PR that drifts the lockfiles fails
  CI.

## Phase 4 follow-up

Phase 4 will:

- Pin a containerised build environment (Nix flake or Docker
  image with frozen system libraries).
- Verify byte-identical reproducibility across two independent
  builds.
- Publish a reproducer recipe alongside each release.

For Phase 2.G, the recipe above is the contract.
```

- [ ] **Step 20.2: Write `docs/build/flatpak.md`**

```markdown
# Flatpak build notes

The in-repo manifest at `packaging/flatpak/net.myggiz.skattr.yml`
builds Skattr from a local checkout. This is convenient for
development and testing but is **not** suitable for Flathub
submission — Flathub requires reproducible, tag-based source
references.

## Building from a checkout

```bash
flatpak install --user flathub \
    org.freedesktop.Platform//23.08 \
    org.freedesktop.Sdk//23.08 \
    org.freedesktop.Sdk.Extension.rust-stable//23.08 \
    org.freedesktop.Sdk.Extension.node20//23.08

cd /path/to/skattr
flatpak-builder --user --force-clean --install build \
    packaging/flatpak/net.myggiz.skattr.yml

flatpak run net.myggiz.skattr
```

## Flathub-publication variant (deferred)

When we publish to Flathub, the manifest in the
`flathub/net.myggiz.skattr` repo replaces the local `dir` source
with a tag-based git source plus pre-fetched dependency
manifests. The shape:

```yaml
sources:
  - type: git
    url: https://github.com/myggiz/skattr.git
    tag: v0.1.0
    commit: <sha-of-the-tag>
  - cargo-sources.json     # generated by flatpak-cargo-generator.py
  - node-sources.json      # generated by flatpak-node-generator.py
```

Generation:

```bash
# Cargo deps (replace v0.1.0 with the actual tag).
git clone https://github.com/flatpak/flatpak-builder-tools.git
python flatpak-builder-tools/cargo/flatpak-cargo-generator.py \
    Cargo.lock -o cargo-sources.json

# Node deps.
python flatpak-builder-tools/node/flatpak-node-generator.py npm \
    crates/ui/src-svelte/pnpm-lock.yaml \
    -o node-sources.json
```

The generated `*-sources.json` files plus the manifest live in
the `flathub/net.myggiz.skattr` repo, not the main `myggiz/skattr`
repo, to keep the main repo's manifest local-source-only.

## Why both?

- The local-source manifest in this repo lets contributors test
  Flatpak builds without any extra tooling.
- The tag-source manifest in the Flathub repo lets Flathub's CI
  build sandbox-isolated bundles with no network access.

Mixing them in one file would force every PR to regenerate the
sources lockfiles; the split keeps the main repo's manifest
contributor-friendly.
```

- [ ] **Step 20.3: Commit**

```bash
git add docs/build/reproducible.md docs/build/flatpak.md
git commit -m "docs(build): reproducible recipe + Flatpak flathub variant

Phase 2.G claims \"inputs are pinned and the recipe is
documented\" — Phase 4 closes byte-identical reproducibility.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 21: CHANGELOG entry + CLAUDE.md status update

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `CLAUDE.md`

- [ ] **Step 21.1: Append a Phase 2.G section to `CHANGELOG.md`**

Open `CHANGELOG.md`. Find the most recent `## [Unreleased]` or insert a new `## [0.1.0] — 2026-05-04` (date of the merge to master) section near the top, below `# Changelog`:

```markdown
## [0.1.0] — 2026-05-04

### Added

- Phase 2.G: first packaged release.
- New `core::daemon::smoke` module: `run_smoke(SmokeConfig)` initialises a throwaway vault, boots the daemon, waits for `TorStatus::Ready`, and exits 0/1 — used by CI release smoke.
- `skattr-ui --smoke-test --data-dir <tmp> [--timeout-secs N]` argv-level branch (no webview opened).
- `skattr daemon --smoke-test` developer escape hatch on the CLI.
- Linux `.deb` + AppImage bundles via `cargo tauri build`.
- macOS `.dmg` bundle (Apple Silicon only for v0.1).
- In-repo Flatpak manifest at `packaging/flatpak/net.myggiz.skattr.yml` (Flathub publication deferred).
- AppStream metainfo at `packaging/flatpak/net.myggiz.skattr.metainfo.xml`.
- `skattr://` URL scheme handler — invite paste becomes invite click.
- `.github/workflows/release.yml`: matrix-build (Linux + macOS), per-platform smoke gate, `SHA256SUMS` + `SHA256SUMS.minisig` (minisign), GitHub Release auto-creation on `v*` tag.
- `docs/install/{README,linux,macos}.md` — verification + install + first-run.
- `docs/build/{reproducible,flatpak}.md` — pinned-version recipe + Flatpak notes.

### Changed

- `tauri = "2"` → `tauri = "=2.11.0"` (Phase 2.G locked decision 3).
- `rust-toolchain.toml` gains an explicit `version = "x.y.z"` line (Phase 2.G locked decision 4).
- Tauri updater plugin explicitly disabled in `tauri.conf.json` (Phase 5 will enable).

### Deferred to Phase 2.H

- Windows IPC port (Named Pipes + DACL peer auth) and `.msi` bundle.
- macOS Intel (`x86_64`) bundle.

### Wire format

No changes. Phase 2.G is wire-format-NEUTRAL by design.
```

- [ ] **Step 21.2: Update CLAUDE.md "Repository state" paragraph**

Open `CLAUDE.md`. The first paragraph after `## Repository state` currently lists Phase 2.F as the latest. Replace its first sentence:

```
**Phase 0 is complete; Phase 1 is complete (1.H merged 2026-04-24);
Phase 2.A (mailbox server) is complete; Phase 2.B (mailbox client +
ContactCard rotation) is complete (merged 2026-05-01); Phase 2.C
(UI bootstrap, read-only conversation MVP) is complete (merged
2026-05-02); Phase 2.D (conversation view) is complete (merged
2026-05-02); Phase 2.E (invite & contact UX) is complete (merged
2026-05-03); Phase 2.F (settings & history) is complete (merged
2026-05-04).**
```

with:

```
**Phase 0 is complete; Phase 1 is complete (1.H merged 2026-04-24);
Phase 2.A (mailbox server) is complete; Phase 2.B (mailbox client +
ContactCard rotation) is complete (merged 2026-05-01); Phase 2.C
(UI bootstrap, read-only conversation MVP) is complete (merged
2026-05-02); Phase 2.D (conversation view) is complete (merged
2026-05-02); Phase 2.E (invite & contact UX) is complete (merged
2026-05-03); Phase 2.F (settings & history) is complete (merged
2026-05-04); Phase 2.G (packaging & distribution) is complete
(merged <DATE-OF-MERGE>) on Linux + macOS; Phase 2.H (Windows
port) is the remaining Phase 2 sub-project before the umbrella
exit criteria are fully met.**
```

Update `<DATE-OF-MERGE>` at merge time, not before.

Add a new bullet to the "What this doc does NOT cover" or in a phase-2g description block, after the Phase 2.F paragraph:

```
Phase 2.G (packaging & distribution) merged at the head of
`phase-2g-packaging`. New `core::daemon::smoke` module (run_smoke
+ SmokeConfig + SmokeError); `skattr-ui --smoke-test` argv branch
that boots the daemon without opening Tauri's webview; CLI escape
hatch on `skattr daemon --smoke-test`. CI release flow at
`.github/workflows/release.yml` triggered on `v*` tags: matrix
build on ubuntu-latest + macos-latest → per-platform smoke
(`/usr/bin/skattr-ui --smoke-test` after `dpkg -i`; AppImage
extract-and-run; `.app` from mounted `.dmg`) → `SHA256SUMS` +
`SHA256SUMS.minisig` (minisign secret in repo secrets) → GitHub
Release. Bundle metadata locked: `net.myggiz.skattr` identifier,
six PNG icon sizes, `skattr://` URL scheme via
`tauri-plugin-deep-link` + `tauri-plugin-single-instance`, Tauri
updater explicitly disabled. Linux `.deb` + AppImage + Flatpak
(build-from-source); macOS `.dmg` (ARM64 only). Install docs at
`docs/install/{README,linux,macos}.md`; reproducible-build recipe
at `docs/build/reproducible.md`. Tauri Rust pinned to `=2.11.0`;
`@tauri-apps/api` matched in `package.json`; `rust-toolchain.toml`
gains explicit `version = "x.y.z"`. Wire-format-NEUTRAL by
design — no new `Command` / `CommandResult` / `Event` variants.
**Windows is carved out to Phase 2.H** (Named Pipes + DACL port
of `core::daemon::ipc`; `.msi` bundle); 2.H lands before any
"v0.2" tag.
```

- [ ] **Step 21.3: Commit**

```bash
git add CHANGELOG.md CLAUDE.md
git commit -m "docs: Phase 2.G CHANGELOG + CLAUDE.md status update

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 22: Verification before merge

**Files:** none (this is a checklist task).

- [ ] **Step 22.1: Full lint + test sweep on the worktree**

```bash
cd /home/myggiz/development/skattr-phase-2g
. "$HOME/.cargo/env"
cargo fmt --all --check
cargo clippy --workspace --exclude skattr-ui --all-targets --all-features -- -D warnings
cargo clippy -p skattr-ui --all-targets --all-features -- -D warnings
cargo test --workspace --exclude skattr-ui --all-targets
cargo test -p skattr-ui --all-targets
cargo deny check
```

Expected: all green.

- [ ] **Step 22.2: SvelteKit build + tests**

```bash
cd crates/ui/src-svelte
pnpm install --frozen-lockfile
pnpm test
pnpm build
cd ../../..
```

Expected: green.

- [ ] **Step 22.3: Real-Tor smoke (the integration test we wrote in Task 8)**

```bash
. "$HOME/.cargo/env"
cargo build -p skattr-ui --release
cargo test -p skattr-tests --release -- --ignored smoke_flag
```

Expected: passes within ~260s.

- [ ] **Step 22.4: Local Tauri build dry-run**

```bash
cargo install tauri-cli --version '=2.11.0' --locked
cargo tauri build
ls -la target/release/bundle/
```

Expected: `target/release/bundle/{deb,appimage}/` populated on Linux; `target/release/bundle/dmg/` on macOS. Sizes look reasonable (each bundle ≥ 30 MB; no zero-byte artefacts).

- [ ] **Step 22.5: Wire-format snapshot test unchanged**

```bash
cargo test -p skattr-core --features test-harness -- wire_format_append_only
```

Expected: passes (the test is unchanged from 2.F merge — the Phase 2.G goal is to leave it unchanged).

- [ ] **Step 22.6: Push the branch + open PR (if you're following PR-based merge)**

```bash
git push -u origin phase-2g-packaging
gh pr create --base master --head phase-2g-packaging \
    --title "Phase 2.G: packaging & distribution" \
    --body-file - <<'EOF'
## Summary

- Linux + macOS bundles via `cargo tauri build`; CI release flow on `v*` tags.
- New `core::daemon::smoke` module + `skattr-ui --smoke-test` argv branch.
- Bundle metadata, `skattr://` URL scheme, Flatpak manifest, install docs.
- Tauri pinned to `=2.11.0`; rustc pinned to exact stable.
- Windows carved out to Phase 2.H (separate sub-project).
- Wire-format-NEUTRAL; `wire_format_append_only` snapshot unchanged.

## Test plan

- [ ] `cargo fmt --check` ✅
- [ ] `cargo clippy --workspace -- -D warnings` ✅
- [ ] `cargo test --workspace` ✅
- [ ] `cargo deny check` ✅
- [ ] `cargo test -p skattr-tests --release -- --ignored smoke_flag` ✅
- [ ] `cargo tauri build` succeeds locally (Linux) ✅
- [ ] CI Release workflow_dispatch on this branch produces bundles ✅
- [ ] CI smoke job runs `<bundle> --smoke-test` and passes ✅

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
```

---

## Spec → task coverage map

| Spec section / requirement                               | Task |
|----------------------------------------------------------|------|
| Smoke module: `SmokeConfig` / `SmokeError` / `SmokeReport` | 3 |
| Smoke `data_dir` empty/non-existent gate                 | 4 |
| Smoke `run_smoke` happy path + Ready-driven shutdown     | 5 |
| `skattr-ui --smoke-test` argv branch                     | 6 |
| `skattr daemon --smoke-test` CLI escape hatch            | 7 |
| Smoke integration test                                   | 8 |
| Six-size icon set                                        | 9 |
| Bundle metadata (publisher, copyright, license, icons, deb deps, macOS minSysVer, updater disabled) | 10 |
| `skattr://` URL scheme via deep-link plugin              | 11 |
| Minisign keypair + GH secrets + pubkey commit            | 12 |
| CI build job                                             | 13 |
| CI smoke job                                             | 14 |
| CI release job (minisign + GH Release)                   | 15 |
| Install docs README                                      | 16 |
| Install docs Linux                                       | 17 |
| Install docs macOS                                       | 18 |
| Flatpak manifest + AppStream                             | 19 |
| Reproducible-build doc + Flatpak doc                     | 20 |
| CHANGELOG + CLAUDE.md status update                      | 21 |
| Pinning Tauri Rust + JS                                  | 2 |
| Pinning rustc toolchain                                  | 1 |
| Verification before merge                                | 22 |
| Wire-format-NEUTRAL invariant                            | 22.5 (snapshot run) |
| Carve-out: Windows → Phase 2.H                           | not implemented in 2.G; documented in 21 |
| Wayland tray caveat                                      | 17 (in linux.md) |
| AppImage `bundleMediaFramework: false` + WebKit version note | 10 + 17 |
| Apple Silicon-only macOS                                 | 18 (documented limitation) |

## Self-review notes (corrections applied inline)

- Spec referenced `licenseFile: "../../COPYING"`; this plan uses `../../LICENSE-GPL3` (the actual file in the repo).
- `make_seed` round-trips through `bip39::Mnemonic` to share the canonical entropy path.
- `smoke.sock` IPC path (inside `data_dir`) avoids collision with a real daemon's `XDG_RUNTIME_DIR`-derived socket.
- The CI smoke step uses `--appimage-extract-and-run` first; documented fallback to direct AppImage execution if the runner's kernel rejects FUSE.
- Both `parse_smoke_argv` and `detect_smoke_test_flag` are unit-tested before any wiring lands in `main()`.
- `tempfile` may need to move from `[dev-dependencies]` → `[dependencies]` on `skattr-cli` for the `cli_smoke` helper; Step 7.3 calls this out.
- Bundle naming on `.deb` vs AppImage vs `.dmg` follows Tauri's defaults; the staging step in CI globs `*.deb` etc. so the exact filenames don't need to be hard-coded.
- The `wire_format_append_only` snapshot test is run as part of Task 22; no Phase 2.G task touches `commands.rs` / `events.rs`.
