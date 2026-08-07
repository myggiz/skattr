// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg(target_os = "windows")]
#![allow(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic))]

//! IPC client, Windows half. Reads the discovery file and connects
//! to the named pipe.

use std::io;
use std::path::Path;

use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY};
use windows_sys::Win32::System::Pipes::WaitNamedPipeW;

use super::{IpcClient, IpcClientError};

impl IpcClient<NamedPipeClient> {
    /// Connect to the daemon by reading the named-pipe name from the discovery file at `path`.
    pub async fn connect(path: &Path) -> std::result::Result<Self, IpcClientError> {
        // Step 1: read discovery file.
        let pipe_name = match std::fs::read_to_string(path) {
            Ok(s) => s.trim().to_string(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(IpcClientError::DaemonNotRunning);
            }
            Err(e) => return Err(IpcClientError::Io(e)),
        };
        if pipe_name.is_empty() {
            return Err(IpcClientError::DaemonNotRunning);
        }

        // Step 2: try to open. Retry once on ERROR_PIPE_BUSY (5 s wait).
        for attempt in 0..2u32 {
            match ClientOptions::new().open(&pipe_name) {
                Ok(stream) => return Ok(Self::from_stream(stream)),
                Err(e) if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) => {
                    return Err(IpcClientError::DaemonNotRunning);
                }
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) && attempt == 0 => {
                    // Wait up to 5 s for an instance to become available.
                    let wide: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(
                        std::ffi::OsStr::new(&pipe_name),
                    )
                    .chain(std::iter::once(0))
                    .collect();
                    // SAFETY: WaitNamedPipeW takes a wide null-terminated
                    // string and a millisecond timeout. `wide` is owned;
                    // its pointer is valid for the call and the function
                    // returns before `wide` is dropped.
                    let waited = unsafe { WaitNamedPipeW(wide.as_ptr(), 5_000) };
                    if waited == 0 {
                        return Err(IpcClientError::DaemonNotRunning);
                    }
                    // Retry once.
                    continue;
                }
                Err(e) => return Err(IpcClientError::Io(e)),
            }
        }
        // Both attempts failed without a more specific error.
        Err(IpcClientError::DaemonNotRunning)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::ipc::IpcClient;

    #[tokio::test]
    async fn connect_missing_discovery_returns_daemon_not_running() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("no-such-endpoint");
        let err = IpcClient::connect(&missing).await.unwrap_err();
        assert!(
            matches!(err, IpcClientError::DaemonNotRunning),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn connect_pipe_named_in_discovery_but_no_server_returns_daemon_not_running() {
        let tmp = tempfile::tempdir().unwrap();
        let endpoint = tmp.path().join("ipc.endpoint");
        std::fs::write(&endpoint, "\\\\.\\pipe\\skattr-not-actually-bound\n").unwrap();
        let err = IpcClient::connect(&endpoint).await.unwrap_err();
        assert!(
            matches!(err, IpcClientError::DaemonNotRunning),
            "got {err:?}"
        );
    }
}
