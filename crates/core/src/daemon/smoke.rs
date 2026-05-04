// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Smoke-test entry point for release artefacts.
//!
//! `run_smoke` initialises a throwaway vault, boots the daemon, waits
//! for `TorStatus::Ready`, then triggers a graceful shutdown. Used by
//! the `skattr-ui --smoke-test` argv branch in CI release pipelines
//! to verify the bundled binary actually starts on each platform.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::sync::oneshot;
use zeroize::Zeroizing;

use crate::daemon::config::Config;
use crate::daemon::state::Daemon;
use crate::identity::vault::Vault;
use crate::identity::{IdentityKey, Seed};

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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
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

    /// `#[ignore]`-gated: spawns real Arti. Run with:
    ///   cargo test -p skattr-core --features test-harness --release \
    ///       -- --ignored daemon::smoke::tests::run_smoke_real_tor
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
        assert!(
            report.onion.ends_with(".onion"),
            "got onion {}",
            report.onion
        );
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

    /// Verifies the smoke surface fails fast (rather than hanging)
    /// when given a timeout that's too short for any real
    /// `TorStatus::Ready` arrival. Either `TorTimeout` (timer wins)
    /// or `DaemonStart` (daemon errors first — e.g. Arti
    /// rejects state-dir permissions) is acceptable evidence that
    /// the smoke layer routed the failure correctly.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn run_smoke_short_timeout_fails_fast() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = SmokeConfig {
            data_dir: tmp.path().to_path_buf(),
            tor_ready_timeout: Duration::from_millis(50),
            ..SmokeConfig::default()
        };
        let err = run_smoke(cfg).await.unwrap_err();
        assert!(
            matches!(
                err,
                SmokeError::TorTimeout { .. } | SmokeError::DaemonStart(_)
            ),
            "expected TorTimeout or DaemonStart, got {err:?}"
        );
    }
}
