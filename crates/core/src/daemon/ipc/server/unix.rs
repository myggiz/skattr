// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic))]

//! IPC server, Unix half. Binds an AF_UNIX socket with mode `0600`
//! and a `0700` parent directory; peer-cred-checks every accept.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tokio::net::UnixListener;

use crate::daemon::ipc::wire::IpcError;
use crate::error::{CoreError, Result};

/// Server bound to a local Unix socket.
pub struct Server {
    listener: UnixListener,
    path: PathBuf,
    allowed_uid: u32,
}

impl Server {
    /// Bind a `Server` at `path`. Creates parents with mode `0700`,
    /// unlinks any stale file at `path`, then binds and chmods the
    /// socket to mode `0600`. `allowed_uid` is the UID that every
    /// accepted connection's peer-cred must match.
    pub fn bind(path: &Path, allowed_uid: u32) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(CoreError::Io)?;
            let mut perms = std::fs::metadata(parent)
                .map_err(CoreError::Io)?
                .permissions();
            perms.set_mode(0o700);
            std::fs::set_permissions(parent, perms).map_err(CoreError::Io)?;
        }
        // Remove stale file (a crashed prior daemon). Ignore errors if
        // it didn't exist.
        let _ = std::fs::remove_file(path);

        let listener = UnixListener::bind(path).map_err(CoreError::Io)?;

        // Tighten the socket file to 0600 immediately after bind.
        let mut perms = std::fs::metadata(path)
            .map_err(CoreError::Io)?
            .permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(path, perms).map_err(CoreError::Io)?;

        Ok(Self {
            listener,
            path: path.to_path_buf(),
            allowed_uid,
        })
    }

    /// Path the socket file is bound to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Wait for the next incoming connection. Returns the accepted
    /// stream only if its peer-cred UID matches `allowed_uid`; else
    /// closes immediately and returns `Err(IpcError::AuthDenied)`.
    pub async fn accept_one(&self) -> std::result::Result<tokio::net::UnixStream, IpcError> {
        let (stream, _) = self
            .listener
            .accept()
            .await
            .map_err(|e| IpcError::Internal(format!("accept: {e}")))?;
        let cred = stream
            .peer_cred()
            .map_err(|e| IpcError::Internal(format!("peer_cred: {e}")))?;
        check_peer_uid(Some(cred.uid()), self.allowed_uid).map_err(|_| IpcError::AuthDenied)?;
        Ok(stream)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Best-effort unlink. Errors are ignored (log-worthy but not
        // fatal); the OS will reap the file on logout if we miss it.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Return the effective UID of the current process without using `unsafe`.
///
/// On Linux we stat `/proc/self`; on other platforms we fall back to
/// the `$UID` environment variable then `0` (suitable for tests).
pub(crate) fn current_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .or_else(|_| {
            std::env::var("UID")
                .ok()
                .and_then(|s| s.parse().ok())
                .ok_or(())
        })
        .unwrap_or(0)
}

/// Check that `peer_uid` matches `expected`. Unit-testable in isolation
/// from the `UnixStream` accept path.
pub(crate) fn check_peer_uid(peer_uid: Option<u32>, expected: u32) -> io::Result<()> {
    match peer_uid {
        Some(uid) if uid == expected => Ok(()),
        Some(uid) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("peer uid {uid} != expected {expected}"),
        )),
        None => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "peer uid unavailable",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn check_peer_uid_accepts_matching_uid() {
        assert!(check_peer_uid(Some(1000), 1000).is_ok());
    }

    #[test]
    fn check_peer_uid_rejects_mismatched_uid() {
        assert!(check_peer_uid(Some(999), 1000).is_err());
    }

    #[test]
    fn check_peer_uid_rejects_missing_uid() {
        assert!(check_peer_uid(None, 1000).is_err());
    }

    #[tokio::test]
    async fn bind_sets_socket_mode_0600_and_parent_0700() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("skattr").join("daemon.sock");
        let server = Server::bind(&sock, 1000).unwrap();

        let sock_mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            sock_mode, 0o600,
            "socket mode must be 0600; got {sock_mode:o}"
        );

        let parent_mode = std::fs::metadata(sock.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            parent_mode, 0o700,
            "parent mode must be 0700; got {parent_mode:o}"
        );

        drop(server);
        // Socket file removed on drop.
        assert!(!sock.exists(), "socket file must be unlinked on drop");
    }

    #[tokio::test]
    async fn bind_unlinks_stale_socket_file() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("daemon.sock");
        // Pre-create a stale socket file.
        std::fs::write(&sock, b"stale").unwrap();
        assert!(sock.exists());
        let server = Server::bind(&sock, 1000).unwrap();
        // Bind succeeded; socket now a real Unix listener.
        drop(server);
    }
}
