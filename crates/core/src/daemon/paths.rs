// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! The single source of truth for on-disk path resolution.
//!
//! Both frontends (UI and CLI) resolve the data directory and the IPC
//! endpoint **only** through these functions. The canonical data dir is the
//! platform *local* (non-roaming) data dir joined with the literal `skattr`
//! — deliberately identifier-independent (no reverse-DNS, no `ProjectDirs`),
//! so the path is identical regardless of the Tauri bundle id. The IPC
//! endpoint lives in the platform runtime dir, never under the data dir.

use std::path::PathBuf;

use crate::error::{CoreError, Result};

/// Canonical per-user data directory: `<local-data>/skattr`.
///
/// - Windows: `%LOCALAPPDATA%\skattr` (non-roaming — identity/DB/onion key
///   must not sync across machines via a roaming profile).
/// - Linux: `$XDG_DATA_HOME/skattr` or `~/.local/share/skattr`.
/// - macOS: `~/Library/Application Support/skattr` (data_local_dir == data_dir there).
///
/// Writable without admin rights and deterministic across launches. Errors
/// only when no home directory can be determined.
pub fn data_dir() -> Result<PathBuf> {
    let base = directories::BaseDirs::new()
        .ok_or_else(|| CoreError::Config("cannot determine home directory".into()))?;
    Ok(base.data_local_dir().join("skattr"))
}

/// Compose the IPC endpoint path under a resolved runtime `base`,
/// appending `skattr/<ENDPOINT_FILENAME>`.
#[cfg(unix)]
fn ipc_endpoint_for_base(base: std::path::PathBuf) -> PathBuf {
    base.join("skattr")
        .join(crate::daemon::ipc::ENDPOINT_FILENAME)
}

/// The default IPC endpoint path, in the platform **runtime** dir.
///
/// - Unix (Linux/macOS): `$XDG_RUNTIME_DIR/skattr/ipc.sock`, falling back to
///   `$TMPDIR/skattr/ipc.sock`, then `/tmp/skattr-<uid>/ipc.sock` (uid-scoped
///   so two users on the same host never share a socket path).
/// - Windows: `%TEMP%\skattr\ipc.endpoint` — the named-pipe *discovery* file
///   (the pipe itself is a kernel object, not a file).
#[cfg(unix)]
pub fn default_ipc_endpoint() -> Result<PathBuf> {
    // XDG_RUNTIME_DIR and TMPDIR both get the standard `skattr/<filename>`
    // suffix. The bare-/tmp fallback uses a uid-scoped dir to prevent
    // cross-user socket collisions on shared systems.
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
    {
        return Ok(ipc_endpoint_for_base(xdg));
    }
    if let Some(tmpdir) = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
    {
        return Ok(ipc_endpoint_for_base(tmpdir));
    }
    // SAFETY: getuid() is always safe to call — it has no preconditions and
    // cannot fail.
    #[allow(unsafe_code)]
    let uid = unsafe { libc::getuid() };
    Ok(PathBuf::from(format!("/tmp/skattr-{uid}")).join(crate::daemon::ipc::ENDPOINT_FILENAME))
}

/// The default IPC endpoint path, in the platform **runtime** dir.
///
/// Windows: `%TEMP%\skattr\ipc.endpoint` — the named-pipe *discovery* file
/// (the pipe itself is a kernel object, not a file), kept out of the data dir.
#[cfg(windows)]
pub fn default_ipc_endpoint() -> Result<PathBuf> {
    Ok(std::env::temp_dir()
        .join("skattr")
        .join(crate::daemon::ipc::ENDPOINT_FILENAME))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_ends_in_bare_skattr_no_identifier() {
        let p = data_dir().expect("home should resolve in test env");
        assert_eq!(p.file_name().unwrap(), "skattr");
        // Identifier-independent: the reverse-DNS bundle id must not appear.
        assert!(
            !p.to_string_lossy().contains("net.myggiz"),
            "data dir must not contain the bundle identifier: {}",
            p.display()
        );
    }

    #[cfg(unix)]
    #[test]
    fn ipc_endpoint_for_base_joins_skattr_and_filename() {
        let ep = ipc_endpoint_for_base(PathBuf::from("/run/user/4242"));
        assert_eq!(ep, PathBuf::from("/run/user/4242/skattr/ipc.sock"));
        assert_eq!(
            ep.file_name().unwrap(),
            crate::daemon::ipc::ENDPOINT_FILENAME
        );
    }
}
