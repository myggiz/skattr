// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! A pure OS advisory lock guaranteeing one daemon per data directory.
//!
//! Acquisition is decided **only** by the OS lock call (`flock` on unix,
//! `LockFileEx` on Windows) against a held-open handle to
//! `<data_dir>/daemon.lock`. We never gate on the lockfile's existence or on
//! a pid inside it: the kernel auto-releases the lock when the holding
//! process dies (including SIGKILL / Task Manager), so a hard kill always
//! leaves a cleanly re-lockable state and there is no stale lock to reclaim.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::fs::{File, OpenOptions};
use std::path::Path;

const LOCK_FILENAME: &str = "daemon.lock";

/// Why a lock acquisition failed.
#[derive(Debug)]
pub(crate) enum LockError {
    /// Another daemon already holds the lock for this data dir.
    AlreadyRunning,
    /// The lockfile could not be opened/locked for some other reason.
    Io(std::io::Error),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::AlreadyRunning => {
                write!(
                    f,
                    "another skattr daemon is already using this data directory"
                )
            }
            LockError::Io(e) => write!(f, "data-dir lock: {e}"),
        }
    }
}

impl std::error::Error for LockError {}

/// RAII guard: holds the lockfile handle open for the daemon's lifetime.
/// Dropping it releases the OS lock; so does process death.
#[derive(Debug)]
pub(crate) struct DaemonLock {
    // The lock is bound to this open handle. Never closed early.
    _file: File,
}

/// Acquire the single-daemon lock for `data_dir`, non-blocking.
///
/// Returns `LockError::AlreadyRunning` if another process holds it (the
/// caller should print a clear message and exit), or `LockError::Io` for any
/// other failure.
pub(crate) fn acquire(data_dir: &Path) -> std::result::Result<DaemonLock, LockError> {
    let path = data_dir.join(LOCK_FILENAME);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        // Never truncate: the lockfile is a pure lock target, its content is
        // irrelevant, and truncation would needlessly touch a file a peer may
        // hold open.
        .truncate(false)
        .open(&path)
        .map_err(LockError::Io)?;
    lock_exclusive_nonblocking(&file)?;
    Ok(DaemonLock { _file: file })
}

#[cfg(unix)]
#[allow(unsafe_code)] // FFI `flock` syscall; justified by the SAFETY comment below.
fn lock_exclusive_nonblocking(file: &File) -> std::result::Result<(), LockError> {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is a valid open file descriptor for the lifetime of the call.
    let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        // EWOULDBLOCK (== EAGAIN on Linux/macOS): the lock is held elsewhere.
        Some(c) if c == libc::EWOULDBLOCK => Err(LockError::AlreadyRunning),
        _ => Err(LockError::Io(err)),
    }
}

#[cfg(windows)]
#[allow(unsafe_code)] // FFI `LockFileEx` + zeroed `OVERLAPPED`; justified by the SAFETY comments below.
fn lock_exclusive_nonblocking(file: &File) -> std::result::Result<(), LockError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{ERROR_LOCK_VIOLATION, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::{
        LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    let handle = file.as_raw_handle() as HANDLE;
    // SAFETY: zeroed OVERLAPPED is valid for a whole-file lock; `handle` is a
    // valid open handle for the duration of the call.
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        LockFileEx(
            handle,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    if rc != 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
        Err(LockError::AlreadyRunning)
    } else {
        Err(LockError::Io(err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_on_same_dir_reports_already_running() {
        let dir = tempfile::tempdir().unwrap();
        let _first = acquire(dir.path()).expect("first acquire succeeds");
        match acquire(dir.path()) {
            Err(LockError::AlreadyRunning) => {} // expected
            other => panic!("expected AlreadyRunning, got {other:?}"),
        }
    }

    #[test]
    fn lock_is_released_on_drop_and_reacquirable() {
        let dir = tempfile::tempdir().unwrap();
        {
            let _g = acquire(dir.path()).expect("acquire");
        } // dropped here -> OS releases
          // A fresh acquire must now succeed (no stale-lock brick).
        let _again = acquire(dir.path()).expect("re-acquire after drop");
    }
}
