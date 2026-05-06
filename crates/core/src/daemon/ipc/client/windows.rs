// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg(target_os = "windows")]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic))]

//! IPC client, Windows half. Reads the discovery file and connects
//! to the named pipe.

use std::path::Path;

use tokio::net::windows::named_pipe::NamedPipeClient;

use super::{IpcClient, IpcClientError};

impl IpcClient<NamedPipeClient> {
    pub async fn connect(_path: &Path) -> std::result::Result<Self, IpcClientError> {
        todo!("Phase 2.H Task 13: Windows discovery-file + NamedPipeClient::connect")
    }
}
