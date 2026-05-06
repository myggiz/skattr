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
    AddAccessAllowedAce, CopySid, EqualSid, GetLengthSid, GetTokenInformation, InitializeAcl,
    InitializeSecurityDescriptor, IsValidSecurityDescriptor, SetSecurityDescriptorDacl, TokenUser,
    ACL, ACL_REVISION, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_GENERIC_READ, FILE_GENERIC_WRITE};
use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// Windows SDK constant: the only defined SECURITY_DESCRIPTOR revision.
/// Lives in Win32_System_SystemServices (not an enabled feature); define here.
const SECURITY_DESCRIPTOR_REVISION: u32 = 1;

use crate::daemon::ipc::wire::IpcError;
use crate::daemon::ipc::PeerId;
use crate::error::Result;

// ---------------------------------------------------------------------------
// Owner-only security descriptor builder
// ---------------------------------------------------------------------------

/// Holds a `SECURITY_DESCRIPTOR` + DACL buffer + `SECURITY_ATTRIBUTES` alive
/// together as a single heap-allocated unit.
///
/// Invariant: `sa.lpSecurityDescriptor` points at `sd`; `sd`'s DACL pointer
/// points into `dacl_buf`. Neither field must be mutated after construction.
/// `as_raw()` re-anchors `lpSecurityDescriptor` before returning so that
/// moving `OwnerOnlySa` on the stack does not stale the pointer.
struct OwnerOnlySa {
    sd: Box<SECURITY_DESCRIPTOR>,
    dacl_buf: Vec<u8>,
    sa: SECURITY_ATTRIBUTES,
}

impl OwnerOnlySa {
    /// Build a `SECURITY_ATTRIBUTES` that grants
    /// `FILE_GENERIC_READ | FILE_GENERIC_WRITE` to `allowed_sid` and
    /// nothing else (no NULL DACL, no default DACL, no inheritance).
    fn new(allowed_sid: &[u8]) -> io::Result<Self> {
        if allowed_sid.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "allowed_sid must not be empty",
            ));
        }

        // ACL buffer layout (all sizes in bytes):
        //   ACL header                         8
        //   ACCESS_ALLOWED_ACE header          4
        //   AccessMask (u32)                   4
        //   SID (full byte slice)              sid_len
        //   -------
        //   sub-total                          16 + sid_len
        // Round up to DWORD boundary.
        let dacl_size = (16 + allowed_sid.len() + 3) & !3usize;
        let mut dacl_buf = vec![0u8; dacl_size];

        // SAFETY: `dacl_buf` is sized by the formula above; we never write
        // past `dacl_size`. `sd` comes from a `Box`, so its heap address is
        // stable and will not move even if `OwnerOnlySa` is moved.
        unsafe {
            let dacl_ptr = dacl_buf.as_mut_ptr() as *mut ACL;
            if InitializeAcl(dacl_ptr, dacl_size as u32, ACL_REVISION) == 0 {
                return Err(io::Error::last_os_error());
            }
            let access = FILE_GENERIC_READ | FILE_GENERIC_WRITE;
            if AddAccessAllowedAce(
                dacl_ptr,
                ACL_REVISION,
                access,
                allowed_sid.as_ptr() as *mut _,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }

            let mut sd: Box<SECURITY_DESCRIPTOR> = Box::new(std::mem::zeroed());
            if InitializeSecurityDescriptor(
                (&mut *sd) as *mut SECURITY_DESCRIPTOR as *mut _,
                SECURITY_DESCRIPTOR_REVISION,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            // Attach the DACL: present=TRUE, dacl=dacl_ptr, defaulted=FALSE.
            if SetSecurityDescriptorDacl(
                (&mut *sd) as *mut SECURITY_DESCRIPTOR as *mut _,
                1,
                dacl_ptr,
                0,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            if IsValidSecurityDescriptor((&mut *sd) as *mut SECURITY_DESCRIPTOR as *mut _) == 0 {
                return Err(io::Error::other("IsValidSecurityDescriptor returned FALSE"));
            }

            let sa = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: (&mut *sd) as *mut SECURITY_DESCRIPTOR as *mut _,
                bInheritHandle: 0,
            };

            Ok(Self { sd, dacl_buf, sa })
        }
    }

    /// Return a raw pointer to the `SECURITY_ATTRIBUTES`.
    ///
    /// Re-anchors `lpSecurityDescriptor` each time so that the pointer
    /// remains valid even if `self` has been moved since construction.
    ///
    /// SAFETY: the returned pointer is valid for as long as `self` lives.
    fn as_raw(&mut self) -> *mut std::ffi::c_void {
        self.sa.lpSecurityDescriptor = (&mut *self.sd) as *mut SECURITY_DESCRIPTOR as *mut _;
        &mut self.sa as *mut SECURITY_ATTRIBUTES as *mut _
    }
}

/// Server bound to a Windows named pipe with an owner-SID-only DACL.
pub struct Server {
    listener: std::sync::Mutex<Option<NamedPipeServer>>,
    discovery_path: PathBuf,
    pipe_name: String,
    allowed: PeerId,
}

impl Server {
    /// Bind a server to a fresh random named pipe, write its name to
    /// `discovery_path`, and gate accept-time access with the owner SID `allowed`.
    pub fn bind(discovery_path: &Path, allowed: PeerId) -> Result<Self> {
        use rand::RngCore as _;

        if let Some(parent) = discovery_path.parent() {
            std::fs::create_dir_all(parent).map_err(crate::error::CoreError::Io)?;
        }
        // Best-effort: remove any stale discovery file from a previous run.
        let _ = std::fs::remove_file(discovery_path);

        // 12 random bytes → 24 hex-char suffix, e.g. `\\.\pipe\skattr-a3f8…`
        let mut entropy = [0u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut entropy);
        let hex: String = entropy.iter().map(|b| format!("{b:02x}")).collect();
        let pipe_name = format!(r"\\.\pipe\skattr-{hex}");

        // Build a SECURITY_ATTRIBUTES with an owner-only DACL. Pass the
        // inner io::Error through directly so its ErrorKind survives the
        // wrap (e.g. `InvalidInput` for an empty SID).
        let mut sa = OwnerOnlySa::new(&allowed).map_err(crate::error::CoreError::Io)?;

        // SAFETY: `sa.as_raw()` returns a pointer into `sa`, which lives on
        // this stack frame and therefore outlives the `create_with_security_attributes_raw`
        // call. `pipe_name` is a valid Win32 named-pipe path.
        let listener = unsafe {
            ServerOptions::new()
                .first_pipe_instance(true)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(&pipe_name, sa.as_raw())
        }
        .map_err(crate::error::CoreError::Io)?;

        // Atomic write: write to a sibling `.tmp` file then rename into place.
        // `OsString::push` appends without replacing any extension component.
        let mut tmp_os = discovery_path.as_os_str().to_os_string();
        tmp_os.push(".tmp");
        let tmp: PathBuf = tmp_os.into();
        std::fs::write(&tmp, format!("{pipe_name}\n")).map_err(crate::error::CoreError::Io)?;
        std::fs::rename(&tmp, discovery_path).map_err(crate::error::CoreError::Io)?;

        Ok(Self {
            listener: std::sync::Mutex::new(Some(listener)),
            discovery_path: discovery_path.to_path_buf(),
            pipe_name,
            allowed,
        })
    }

    /// Path the discovery file is written to.
    pub fn path(&self) -> &Path {
        &self.discovery_path
    }

    /// Create a fresh `NamedPipeServer` for the same pipe name, ready to
    /// accept the *next* client connection.
    ///
    /// Note: `.first_pipe_instance(true)` is intentionally omitted here —
    /// that flag is only valid for the very first instance; setting it on
    /// subsequent instances causes `CreateNamedPipeW` to return
    /// `ERROR_ACCESS_DENIED`.
    fn next_listener(&self) -> io::Result<NamedPipeServer> {
        let mut sa = OwnerOnlySa::new(&self.allowed)
            .map_err(|e| io::Error::other(format!("OwnerOnlySa: {e}")))?;
        // SAFETY: `pipe_name` is stable for the lifetime of `Server`;
        // `sa.as_raw()` returns a pointer into `sa` which lives until the
        // end of this function (after `create_with_security_attributes_raw`
        // returns and the result is bound).
        let listener = unsafe {
            ServerOptions::new()
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(&self.pipe_name, sa.as_raw() as *mut _)
        }?;
        Ok(listener)
    }

    /// Accept a single client. Validates the connecting process's user SID against
    /// the bind-time `allowed` value; rebuilds the listener for the next accept.
    pub async fn accept_one(&self) -> std::result::Result<NamedPipeServer, IpcError> {
        use std::os::windows::io::AsRawHandle;

        // Pre-build the next listener BEFORE consuming the current one. If
        // refill construction fails, the slot stays populated and the next
        // accept call can retry — keeping the daemon alive across transient
        // resource failures.
        let next = self
            .next_listener()
            .map_err(|e| IpcError::Internal(format!("next_listener: {e}")))?;

        // Take the current listener out of the slot. The slot is empty only
        // for the duration of `connect().await` plus the SID check; the
        // `next` instance refills it before this function returns on any
        // path below.
        let listener = {
            // The Mutex can only become poisoned if a previous critical
            // section panicked. None of our critical sections (`take`,
            // `*guard = Some(_)`) can panic, so this branch is unreachable
            // in practice; we still surface a typed Internal error rather
            // than panicking, to keep the daemon's `serve()` loop alive.
            let mut guard = self.listener.lock().map_err(|e| {
                IpcError::Internal(format!("listener Mutex unexpectedly poisoned: {e}"))
            })?;
            match guard.take() {
                Some(l) => l,
                None => {
                    // The slot can only be empty if a prior accept_one is
                    // still running on another task. The current shape of
                    // serve() drives accept_one serially, so this is
                    // unreachable in practice.
                    *guard = Some(next);
                    return Err(IpcError::Internal(
                        "accept_one is not re-entrant: previous call still in flight".into(),
                    ));
                }
            }
        };

        // Wait for a client. Refill the slot regardless of how connect()
        // resolves so a connect failure doesn't strand the slot.
        let connect_result = listener.connect().await;
        {
            // The Mutex can only become poisoned if a previous critical
            // section panicked. None of our critical sections (`take`,
            // `*guard = Some(_)`) can panic, so this branch is unreachable
            // in practice; we still surface a typed Internal error rather
            // than panicking, to keep the daemon's `serve()` loop alive.
            let mut guard = self.listener.lock().map_err(|e| {
                IpcError::Internal(format!("listener Mutex unexpectedly poisoned: {e}"))
            })?;
            *guard = Some(next);
        }
        connect_result.map_err(|e| IpcError::Internal(format!("connect: {e}")))?;

        // Capture the SID of the connecting client.
        // SAFETY: `as_raw_handle` returns a valid pipe handle for the
        // lifetime of `listener`. `peer_sid_for` does not close it.
        let raw = AsRawHandle::as_raw_handle(&listener) as windows_sys::Win32::Foundation::HANDLE;
        let peer = unsafe { peer_sid_for(raw) }
            .map_err(|e| IpcError::Internal(format!("peer_sid_for: {e}")))?;
        if check_peer_sid(&peer, &self.allowed).is_err() {
            return Err(IpcError::AuthDenied);
        }

        Ok(listener)
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
        if GetTokenInformation(token, TokenUser, buf.as_mut_ptr() as *mut _, len, &mut len) == 0 {
            CloseHandle(token);
            tracing::error!("GetTokenInformation failed: {}", io::Error::last_os_error());
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

pub(crate) fn check_peer_sid(peer: &[u8], expected: &[u8]) -> io::Result<()> {
    if peer.is_empty() || expected.is_empty() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "empty SID"));
    }
    // SAFETY: EqualSid takes two PSIDs (raw byte pointers). Both inputs
    // are non-empty Vec<u8> slices owned by the caller; their pointers
    // are valid for the call.
    let eq = unsafe { EqualSid(peer.as_ptr() as *mut _, expected.as_ptr() as *mut _) };
    if eq != 0 {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "peer SID != expected",
        ))
    }
}

/// Extract the SID of the process on the other end of `pipe_handle`.
/// `pipe_handle` must be a connected NamedPipeServer raw handle.
///
/// SAFETY: caller must ensure `pipe_handle` is a live, connected named
/// pipe server handle. The function does not close it.
pub(crate) unsafe fn peer_sid_for(pipe_handle: HANDLE) -> io::Result<Vec<u8>> {
    let mut pid = 0u32;
    if GetNamedPipeClientProcessId(pipe_handle, &mut pid) == 0 {
        return Err(io::Error::last_os_error());
    }
    let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }

    let mut token: HANDLE = std::ptr::null_mut();
    if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
        let err = io::Error::last_os_error();
        CloseHandle(process);
        return Err(err);
    }

    let mut len = 0u32;
    GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
    if len == 0 {
        CloseHandle(token);
        CloseHandle(process);
        return Err(io::Error::other("TOKEN_USER probe failed"));
    }
    let mut buf = vec![0u8; len as usize];
    if GetTokenInformation(token, TokenUser, buf.as_mut_ptr() as *mut _, len, &mut len) == 0 {
        let err = io::Error::last_os_error();
        CloseHandle(token);
        CloseHandle(process);
        return Err(err);
    }

    let token_user = buf.as_ptr() as *const TOKEN_USER;
    let sid_ptr = (*token_user).User.Sid;
    if sid_ptr.is_null() {
        CloseHandle(token);
        CloseHandle(process);
        return Err(io::Error::other("TOKEN_USER.Sid is null"));
    }
    let sid_len = GetLengthSid(sid_ptr);
    let mut sid_bytes = vec![0u8; sid_len as usize];
    if CopySid(sid_len, sid_bytes.as_mut_ptr() as *mut _, sid_ptr) == 0 {
        let err = io::Error::last_os_error();
        CloseHandle(token);
        CloseHandle(process);
        return Err(err);
    }

    CloseHandle(token);
    CloseHandle(process);
    Ok(sid_bytes)
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
        assert!(
            sid.len() >= 12,
            "current_sid too short: {} bytes",
            sid.len()
        );
        // Revision byte must be 1 (Microsoft's only defined value).
        assert_eq!(sid[0], 1, "SID revision must be 1");
        let sub_authority_count = sid[1] as usize;
        assert_eq!(
            sid.len(),
            8 + 4 * sub_authority_count,
            "SID length must equal 8 + 4 * sub_authority_count"
        );
    }

    #[test]
    fn check_peer_sid_accepts_matching_sid() {
        let sid = current_sid();
        assert!(!sid.is_empty());
        assert!(check_peer_sid(&sid, &sid).is_ok());
    }

    #[test]
    fn check_peer_sid_rejects_mismatched_sid() {
        let a = current_sid();
        let mut b = a.clone();
        // Flip the last sub-authority byte to invalidate the SID match.
        let last = b.len() - 1;
        b[last] = b[last].wrapping_add(1);
        assert!(check_peer_sid(&a, &b).is_err());
    }

    #[test]
    fn check_peer_sid_rejects_empty_peer() {
        let me = current_sid();
        assert!(check_peer_sid(&[], &me).is_err());
    }

    #[tokio::test]
    async fn bind_writes_discovery_file_with_pipe_name() {
        let tmp = tempfile::tempdir().unwrap();
        let endpoint = tmp.path().join("ipc.endpoint");
        let allowed = current_sid();
        let server = Server::bind(&endpoint, allowed).unwrap();
        let written = std::fs::read_to_string(&endpoint).unwrap();
        let trimmed = written.trim();
        assert!(
            trimmed.starts_with(r"\\.\pipe\skattr-"),
            "unexpected pipe name: {trimmed}"
        );
        // Suffix is exactly 24 hex chars after the prefix.
        let suffix = trimmed.trim_start_matches(r"\\.\pipe\skattr-");
        assert_eq!(suffix.len(), 24, "suffix must be 24 hex chars");
        assert!(
            suffix.chars().all(|c| c.is_ascii_hexdigit()),
            "suffix must be hex"
        );
        drop(server);
        assert!(!endpoint.exists(), "discovery file removed on drop");
    }

    #[tokio::test]
    async fn bind_unlinks_stale_discovery_file() {
        let tmp = tempfile::tempdir().unwrap();
        let endpoint = tmp.path().join("ipc.endpoint");
        std::fs::write(&endpoint, r"\\.\pipe\skattr-stale").unwrap();
        let server = Server::bind(&endpoint, current_sid()).unwrap();
        let written = std::fs::read_to_string(&endpoint).unwrap();
        assert!(!written.contains("stale"));
        drop(server);
    }

    #[tokio::test]
    async fn accept_admits_matching_sid() {
        use tokio::net::windows::named_pipe::ClientOptions;

        let tmp = tempfile::tempdir().unwrap();
        let endpoint = tmp.path().join("ipc.endpoint");
        let server = std::sync::Arc::new(Server::bind(&endpoint, current_sid()).unwrap());
        let pipe_name = server.pipe_name.clone();
        let server_for_accept = std::sync::Arc::clone(&server);
        let accept = tokio::spawn(async move { server_for_accept.accept_one().await });

        // Small spin to let the accept_one task register before connecting.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let _client = ClientOptions::new().open(&pipe_name).unwrap();

        let res = accept.await.unwrap();
        assert!(res.is_ok(), "expected Ok, got {res:?}");
    }

    #[tokio::test]
    async fn bind_with_wrong_sid_denies_client_at_os_level() {
        use tokio::net::windows::named_pipe::ClientOptions;

        // The DACL on the bound pipe grants connect rights only to
        // `allowed`. Our test process runs under `current_sid()`, so
        // when we bind with a different SID the kernel itself rejects
        // the connect at `ClientOptions::open` — the post-accept SID
        // check (defense-in-depth layer 2) never gets to run because
        // the OS-level DACL gate (layer 1) fires first.
        //
        // This test verifies layer 1; layer 2 is exercised on the
        // happy path by `accept_admits_matching_sid` and is unit-
        // testable in isolation only via `check_peer_sid_*`.
        let tmp = tempfile::tempdir().unwrap();
        let endpoint = tmp.path().join("ipc.endpoint");
        let mut wrong = current_sid();
        let last = wrong.len() - 1;
        wrong[last] = wrong[last].wrapping_add(1);
        let _server = Server::bind(&endpoint, wrong).unwrap();
        let pipe_name: String = std::fs::read_to_string(&endpoint).unwrap().trim().into();

        let result = ClientOptions::new().open(&pipe_name);
        assert!(
            result.is_err(),
            "expected ClientOptions::open to fail at the DACL gate; got Ok"
        );
        let err = result.unwrap_err();
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::PermissionDenied,
            "expected PermissionDenied; got {err:?}"
        );
    }
}
