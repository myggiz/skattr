// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg(target_os = "windows")]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic))]
// This module is the sole exception to the crate-wide `deny(unsafe_code)` rule.
// It must call Win32 FFI (OpenProcessToken, GetTokenInformation, CopySid) to
// retrieve the process user SID for peer-auth. Every `unsafe` block carries an
// explicit SAFETY comment; no other module in this crate uses unsafe code.
#![allow(unsafe_code)]

//! IPC server, Windows half. Binds a Named Pipe with an
//! owner-SID-only DACL and post-accept SID equality check.

use std::io;
use std::path::{Path, PathBuf};

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

// --- Win32 FFI ---
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::{
    CopySid, GetLengthSid, GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

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
    // SAFETY: All FFI is to documented Win32 APIs. We close the
    // process-token handle on every exit path. The TOKEN_USER buffer is
    // sized via the standard two-call pattern. CopySid into a fresh Vec
    // gives us a stable, owned SID that outlives the process token.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            // Must not happen for our own process; fall back to an empty
            // SID which `check_peer_sid` will reject.
            tracing::error!(
                "OpenProcessToken on self failed: {}",
                io::Error::last_os_error()
            );
            return Vec::new();
        }

        // Two-call pattern: first probe for required buffer length.
        let mut len = 0u32;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
        if len == 0 {
            CloseHandle(token);
            tracing::error!("GetTokenInformation probe returned 0 length");
            return Vec::new();
        }

        let mut buf = vec![0u8; len as usize];
        if GetTokenInformation(
            token,
            TokenUser,
            buf.as_mut_ptr() as *mut _,
            len,
            &mut len,
        ) == 0
        {
            CloseHandle(token);
            tracing::error!(
                "GetTokenInformation failed: {}",
                io::Error::last_os_error()
            );
            return Vec::new();
        }

        let token_user = buf.as_ptr() as *const TOKEN_USER;
        let sid_ptr = (*token_user).User.Sid;
        if sid_ptr.is_null() {
            CloseHandle(token);
            tracing::error!("TOKEN_USER.Sid is null");
            return Vec::new();
        }
        let sid_len = GetLengthSid(sid_ptr);
        let mut sid_bytes = vec![0u8; sid_len as usize];
        if CopySid(sid_len, sid_bytes.as_mut_ptr() as *mut _, sid_ptr) == 0 {
            CloseHandle(token);
            tracing::error!("CopySid failed: {}", io::Error::last_os_error());
            return Vec::new();
        }

        CloseHandle(token);
        sid_bytes
    }
}

pub(crate) fn check_peer_sid(_peer: &[u8], _expected: &[u8]) -> io::Result<()> {
    todo!("Phase 2.H Task 9: EqualSid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_sid_returns_non_empty_well_formed_sid() {
        let sid = current_sid();
        assert!(!sid.is_empty(), "current_sid returned empty Vec");
        // SID layout: revision (1) + sub_authority_count (1) + identifier_authority (6) +
        // sub_authorities (4 * count). Minimum well-formed length is 8 + 4 = 12 bytes
        // for a single-sub-authority SID.
        assert!(sid.len() >= 12, "current_sid too short: {} bytes", sid.len());
        // Revision byte must be 1 (Microsoft's only defined value).
        assert_eq!(sid[0], 1, "SID revision must be 1");
        let sub_authority_count = sid[1] as usize;
        assert_eq!(
            sid.len(),
            8 + 4 * sub_authority_count,
            "SID length must equal 8 + 4 * sub_authority_count"
        );
    }
}
