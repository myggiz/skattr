// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, unsafe_code))]

//! One-time migration of an existing identity into the canonical data dir.
//!
//! The Windows `os error 5` blocker was caused by the old data-dir resolver
//! falling back to the CWD (the install dir / `Program Files`) when `HOME`
//! was unset. Switching to the platform local-data dir also *moves* where the
//! app looks — so any pre-existing identity must be carried over, or the fix
//! would itself trigger fresh onboarding and orphan the real identity.
//!
//! This scans the locations state has historically landed in and moves the
//! first complete set into the canonical dir. It is idempotent (a no-op once
//! the canonical dir holds an `identity.vault`) and fail-loud (a partial move
//! aborts rather than silently onboarding anew).

use std::path::{Path, PathBuf};

const VAULT: &str = "identity.vault";

/// Errors that can occur during legacy migration.
#[derive(Debug)]
pub enum MigrateError {
    /// Failed to read a legacy directory.
    ReadDir {
        /// The directory that could not be read.
        dir: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// Failed to move an entry from the legacy directory.
    Move {
        /// The source directory containing the entry that failed to move.
        from: PathBuf,
        /// The name of the entry (file or subdirectory) that failed.
        name: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
}

impl std::fmt::Display for MigrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrateError::ReadDir { dir, source } => {
                write!(f, "migrate: read legacy dir {}: {source}", dir.display())
            }
            MigrateError::Move { from, name, source } => {
                write!(f, "migrate: move {name} from {}: {source}", from.display())
            }
        }
    }
}

impl std::error::Error for MigrateError {}

/// Ordered legacy locations to scan (most-likely-real first). Each is a dir
/// that may contain a complete state set from a previous layout.
fn legacy_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();

    #[cfg(windows)]
    {
        if let Some(up) = std::env::var_os("USERPROFILE") {
            out.push(PathBuf::from(&up).join("Downloads").join("skattr"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            out.push(
                PathBuf::from(&local)
                    .join("VirtualStore")
                    .join("Program Files")
                    .join("Skattr"),
            );
            // The old CWD-fallback could also land `.\skattr` beside the exe;
            // and the install dir itself.
            out.push(PathBuf::from(r"C:\Program Files\Skattr"));
            out.push(PathBuf::from(r"C:\Program Files\Skattr\skattr"));
        }
        if let Some(appdata) = std::env::var_os("APPDATA") {
            // Old CLI ProjectDirs path: %APPDATA%\myggiz\skattr.
            out.push(PathBuf::from(&appdata).join("myggiz").join("skattr"));
        }
    }

    #[cfg(unix)]
    {
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
        if let Some(dh) = data_home {
            // Old CLI ProjectDirs path: ~/.local/share/net.myggiz.skattr.
            out.push(dh.join("net.myggiz.skattr"));
            // Older UI nested layout: ~/.local/share/net.myggiz.skattr/skattr.
            out.push(dh.join("net.myggiz.skattr").join("skattr"));
        }
    }

    out
}

/// Move the whole contents of `from` into `to`, preserving the set. Falls
/// back to copy+remove across filesystems. Fail-loud on any entry.
fn move_dir_contents(from: &Path, to: &Path) -> Result<(), MigrateError> {
    let entries = std::fs::read_dir(from).map_err(|e| MigrateError::ReadDir {
        dir: from.to_path_buf(),
        source: e,
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| MigrateError::ReadDir {
            dir: from.to_path_buf(),
            source: e,
        })?;
        let name = entry.file_name();
        let dst = to.join(&name);
        let src = entry.path();
        if let Err(e) = std::fs::rename(&src, &dst) {
            // Cross-device or other rename failure: try copy then remove.
            if let Err(copy_err) = copy_recursive(&src, &dst) {
                return Err(MigrateError::Move {
                    from: from.to_path_buf(),
                    name: name.to_string_lossy().into_owned(),
                    source: copy_err,
                });
            }
            let _ = remove_path(&src);
            let _ = e; // original rename error superseded by successful copy
        }
    }
    Ok(())
}

fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &dst.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dst).map(|_| ())
    }
}

fn remove_path(p: &Path) -> std::io::Result<()> {
    if p.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    }
}

fn set_user_only_perms(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)) {
            tracing::warn!(error = %e, "migrate: could not set data dir to 0700 (non-fatal)");
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir; // %LOCALAPPDATA% is already per-user on Windows.
    }
}

/// Core migration logic: scan `candidates` in order, move the first complete
/// legacy set into `canonical`. Separated from env-reading for testability.
///
/// Idempotent: if `canonical/identity.vault` already exists, returns `Ok(())`
/// immediately. Fail-loud: any move error returns `Err` so the caller can
/// abort startup rather than silently onboard anew.
fn migrate_from_candidates(canonical: &Path, candidates: &[PathBuf]) -> Result<(), MigrateError> {
    if canonical.join(VAULT).exists() {
        return Ok(());
    }
    for cand in candidates {
        if cand == canonical {
            continue;
        }
        if cand.join(VAULT).exists() {
            std::fs::create_dir_all(canonical).map_err(|e| MigrateError::ReadDir {
                dir: canonical.to_path_buf(),
                source: e,
            })?;
            move_dir_contents(cand, canonical)?;
            set_user_only_perms(canonical);
            tracing::info!(
                from = %cand.display(),
                to = %canonical.display(),
                "migrated legacy identity into canonical data dir"
            );
            return Ok(());
        }
    }
    Ok(())
}

/// Idempotent, fail-loud migration of the first complete legacy state set
/// into `canonical`. No-op once `canonical` holds an identity vault.
pub fn migrate_legacy_into(canonical: &Path) -> Result<(), MigrateError> {
    migrate_from_candidates(canonical, &legacy_candidates())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No-op when canonical already contains a vault — never re-migrates.
    #[test]
    fn noop_when_canonical_already_has_vault() {
        let canonical = tempfile::tempdir().unwrap();
        std::fs::write(canonical.path().join(VAULT), b"x").unwrap();
        // No candidates needed: the idempotency check fires before scanning.
        migrate_from_candidates(canonical.path(), &[]).unwrap();
        assert!(canonical.path().join(VAULT).exists());
    }

    /// Moves a complete legacy set (vault + db + arti subdir) into canonical
    /// and leaves no vault behind in the legacy source.
    #[test]
    fn moves_complete_set_from_legacy_and_leaves_nothing_behind() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("legacy");
        let canonical = root.path().join("canonical");

        // Populate legacy with a complete state set.
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join(VAULT), b"vault").unwrap();
        std::fs::write(legacy.join("skattr.sqlite.age"), b"db").unwrap();
        std::fs::create_dir_all(legacy.join("arti")).unwrap();
        std::fs::write(legacy.join("arti").join("state"), b"tor").unwrap();

        migrate_from_candidates(canonical.as_path(), std::slice::from_ref(&legacy)).unwrap();

        assert!(canonical.join(VAULT).exists(), "vault must be moved");
        assert!(
            canonical.join("skattr.sqlite.age").exists(),
            "db must be moved"
        );
        assert!(
            canonical.join("arti").join("state").exists(),
            "arti subdir must be moved"
        );
        assert!(
            !legacy.join(VAULT).exists(),
            "legacy vault must be gone after move"
        );
    }

    /// Empty candidate list (or none with a vault) → clean first-run, no
    /// vault appears in canonical.
    #[test]
    fn no_legacy_means_clean_first_run() {
        let canonical = tempfile::tempdir().unwrap();
        // Pass an empty candidate slice — no legacy dirs exist.
        migrate_from_candidates(canonical.path(), &[]).unwrap();
        assert!(
            !canonical.path().join(VAULT).exists(),
            "no vault must appear from nowhere"
        );
    }

    /// A move failure (canonical not writable) must surface as `MigrateError::Move`,
    /// not be silently swallowed. The legacy vault must still be present.
    #[cfg(unix)]
    #[test]
    fn move_failure_is_fail_loud() {
        // SAFETY: getuid is always safe.
        if unsafe { libc::getuid() } == 0 {
            return; // root bypasses 0500 perms; injection wouldn't fire.
        }
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("legacy");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("identity.vault"), b"vault").unwrap();
        let canonical = root.path().join("canonical");
        std::fs::create_dir_all(&canonical).unwrap();
        // Make canonical read+execute only: rename INTO it and copy INTO it both fail.
        std::fs::set_permissions(&canonical, std::fs::Permissions::from_mode(0o500)).unwrap();
        let err = migrate_from_candidates(&canonical, std::slice::from_ref(&legacy)).unwrap_err();
        assert!(
            matches!(err, MigrateError::Move { .. }),
            "expected Move error, got {err:?}"
        );
        // Restore perms so tempdir cleanup can remove it.
        std::fs::set_permissions(&canonical, std::fs::Permissions::from_mode(0o700)).unwrap();
        // The legacy vault must still be present (move did not silently succeed).
        assert!(legacy.join("identity.vault").exists());
    }

    /// `copy_recursive` faithfully replicates a nested directory tree.
    #[test]
    fn copy_recursive_replicates_nested_tree() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("src");
        std::fs::create_dir_all(src.join("a/b")).unwrap();
        std::fs::write(src.join("a/b/file.txt"), b"hello").unwrap();
        std::fs::write(src.join("top.txt"), b"top").unwrap();
        let dst = root.path().join("dst");
        copy_recursive(&src, &dst).unwrap();
        assert_eq!(std::fs::read(dst.join("a/b/file.txt")).unwrap(), b"hello");
        assert_eq!(std::fs::read(dst.join("top.txt")).unwrap(), b"top");
    }
}
