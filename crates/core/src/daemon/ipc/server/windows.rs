// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg(target_os = "windows")]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic))]

//! IPC server, Windows half. Binds a Named Pipe with an
//! owner-SID-only DACL and post-accept SID equality check.

use std::io;
use std::path::{Path, PathBuf};

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use crate::daemon::ipc::wire::IpcError;
use crate::daemon::ipc::PeerId;
use crate::error::Result;

pub struct Server {
    listener: NamedPipeServer,
    discovery_path: PathBuf,
    pipe_name: String,
    allowed: PeerId,
}

impl Server {
    pub fn bind(_discovery_path: &Path, _allowed: PeerId) -> Result<Self> {
        todo!("Phase 2.H Task 10: Windows pipe bind")
    }

    pub fn path(&self) -> &Path {
        &self.discovery_path
    }

    pub async fn accept_one(&self) -> std::result::Result<NamedPipeServer, IpcError> {
        todo!("Phase 2.H Task 11: Windows accept + post-accept SID check")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.discovery_path);
    }
}

pub(crate) fn current_sid() -> PeerId {
    todo!("Phase 2.H Task 8: GetCurrentProcessToken → TokenUser → SID")
}

pub(crate) fn check_peer_sid(_peer: &[u8], _expected: &[u8]) -> io::Result<()> {
    todo!("Phase 2.H Task 9: EqualSid")
}
