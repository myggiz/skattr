// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg(unix)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic))]

//! IPC client, Unix half. `connect(&Path)` over `UnixStream`.

use std::io;
use std::path::Path;

use tokio::net::UnixStream;

use super::{IpcClient, IpcClientError};

impl IpcClient<UnixStream> {
    /// Connect to the daemon at `path`. Maps a missing socket file or
    /// connection-refused to [`IpcClientError::DaemonNotRunning`].
    pub async fn connect(path: &Path) -> std::result::Result<Self, IpcClientError> {
        match UnixStream::connect(path).await {
            Ok(stream) => Ok(Self::from_stream(stream)),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                Err(IpcClientError::DaemonNotRunning)
            }
            Err(e) => Err(IpcClientError::Io(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_missing_socket_returns_daemon_not_running() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("no-such-socket");
        let err = IpcClient::connect(&missing).await.unwrap_err();
        assert!(
            matches!(err, IpcClientError::DaemonNotRunning),
            "got {err:?}"
        );
    }
}
