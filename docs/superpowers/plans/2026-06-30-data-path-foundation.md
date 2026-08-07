# Data-Path Foundation (SK-015 / SK-016) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve all app state from one deterministic, per-user, admin-free directory shared by both the UI and CLI frontends, migrate any pre-existing identity into it, and guard the directory with a self-clearing single-daemon lock — fixing the Windows `os error 5` onboarding blocker without orphaning existing identities.

**Architecture:** Introduce two pure resolver functions in `core` (`daemon::paths::data_dir`, `daemon::paths::default_ipc_endpoint`) as the single source of truth; route `Config`, the CLI, and the UI through them. State (vault, sqlite, hs.key.age, arti/, logs, config) lives under the canonical data dir; the IPC endpoint lives in the platform **runtime** dir (never under the data dir). A pure OS advisory lock on `<data_dir>/daemon.lock` makes a second daemon refuse to start. A shared `core` migration moves the first complete legacy state set into the canonical dir before the daemon opens it.

**Tech Stack:** Rust 2021, `directories = "5"` (already a workspace dep — `BaseDirs::data_local_dir`), `libc` (unix `flock`, already in core), `windows-sys = "0.59"` (windows `LockFileEx`, already in core), Tauri 2 (UI setup hook), `clap` (CLI).

## Global Constraints

- **No new dependencies.** `directories`, `libc`, `windows-sys` are already present; use them. A new dep would need PR justification + `cargo-deny`.
- **Canonical data dir = the platform *local* data dir joined with the literal `"skattr"`** on every platform/frontend. Windows `%LOCALAPPDATA%\skattr` (non-roaming), Linux `$XDG_DATA_HOME` or `~/.local/share` → `…/skattr`, macOS `~/Library/Application Support/skattr`. Obtained via `directories::BaseDirs::data_local_dir()`. **No `net.myggiz`, no reverse-DNS, no `ProjectDirs`, no bundle-id-derived folder, no `current_exe()`, no `"."`/CWD fallback** for any data path.
- **IPC endpoint lives in the runtime dir, not the data dir.** Linux/macOS: `$XDG_RUNTIME_DIR` → `$TMPDIR` → `/tmp`, then `skattr/ipc.sock`. Windows: the named-pipe discovery file under `%TEMP%\skattr\ipc.endpoint` (the named pipe itself is a kernel object, never a data-dir file).
- **The daemon lock is a *pure* OS advisory lock on a held-open handle.** Acquire/refuse is decided **only** by the OS lock call (`flock(LOCK_EX|LOCK_NB)` / `LockFileEx(LOCKFILE_EXCLUSIVE_LOCK|LOCKFILE_FAIL_IMMEDIATELY)`). **Never** gate on the lockfile's existence or on a pid written inside it — the kernel auto-releases on process death (incl. SIGKILL / Task Manager), so a hard kill must leave a cleanly re-lockable state. Stale-lock reclaim is therefore out of scope (the OS does it).
- **Second daemon must error clearly and exit, not block.** The error must distinguish "another instance is already running" from any other lock failure (IO/permission).
- **Migration is fail-loud on partial failure.** If a legacy identity is found but the move fails midway, return an error and abort startup — never fall through to fresh onboarding (that is the silent-identity-loss trap).
- **License header on every new `.rs` file:** `// SPDX-License-Identifier: GPL-3.0-or-later` / `// Copyright (C) 2026 Myggiz B.V.`.
- **No `unwrap()`/`expect()` in library (`core`) code** outside `#[cfg(test)]`. Use `?` and typed errors.
- **Verify before claiming done:** `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test` must pass. Prefix cargo with `. "$HOME/.cargo/env" &&`.

---

## File Structure

**New files (core):**
- `crates/core/src/daemon/paths.rs` — `data_dir()`, `default_ipc_endpoint()`. The single source of truth for both paths.
- `crates/core/src/daemon/lock.rs` — `DaemonLock`, `acquire()`, `LockError`. Pure OS advisory lock.
- `crates/core/src/daemon/migrate.rs` — `migrate_legacy_into()`, `MigrateError`. Platform legacy-source scan + move.

**Modified:**
- `crates/core/src/daemon/mod.rs` — register the three new modules; re-export the public items.
- `crates/core/src/error.rs` — add `CoreError::DaemonAlreadyRunning`.
- `crates/core/src/daemon/config.rs:129` (`Config::defaults`) — use `paths::data_dir()`.
- `crates/core/src/daemon/config.rs:172-205` (`ipc_socket_or_default`, both cfg arms) — delegate to `paths::default_ipc_endpoint()`.
- `crates/core/src/daemon/state.rs:104-115` (`run_with_sink`) — acquire + hold the daemon lock **at the top, right after `create_dir_all`**, before vault/`Pool::open`/`TorRuntime::bootstrap`; also normalise `config.data_dir` to the authoritative param there. (NOT `run_with_transport` — by the time that runs, the Pool is already open and Arti is already bootstrapped.)
- `crates/core/Cargo.toml` — add `windows-sys` feature flags `Win32_Storage_FileSystem`, `Win32_System_IO` if not already enabled.
- `crates/cli/src/main.rs:195-224` (`resolve_socket_path`) and `:405-410` (`effective_data_dir`) — route through `paths`; call migration before `daemon()`.
- `crates/ui/src/main.rs:142-206` — replace `consolidated_data_dir()`/`migrate_legacy_data()` with calls into `core`; `:227,322` use `paths::data_dir()`.
- `crates/ui/src/daemon.rs:87` — stop pinning IPC to `data_dir`; use the runtime endpoint.

---

### Task 1: Core path resolvers (`daemon::paths`)

**Files:**
- Create: `crates/core/src/daemon/paths.rs`
- Modify: `crates/core/src/daemon/mod.rs`

**Interfaces:**
- Produces:
  - `pub fn data_dir() -> crate::error::Result<std::path::PathBuf>` — canonical per-user data dir (`<local-data>/skattr`).
  - `pub fn default_ipc_endpoint() -> crate::error::Result<std::path::PathBuf>` — runtime IPC endpoint path.

- [ ] **Step 1: Register the module**

In `crates/core/src/daemon/mod.rs`, add alongside the other `mod`/`pub mod` declarations:

```rust
pub mod paths;
```

- [ ] **Step 2: Write the failing tests**

Create `crates/core/src/daemon/paths.rs`:

```rust
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
/// - macOS: `~/Library/Application Support/skattr`.
///
/// Writable without admin rights and deterministic across launches. Errors
/// only when no home directory can be determined.
pub fn data_dir() -> Result<PathBuf> {
    let base = directories::BaseDirs::new()
        .ok_or_else(|| CoreError::Config("cannot determine home directory".into()))?;
    Ok(base.data_local_dir().join("skattr"))
}

/// The default IPC endpoint path, in the platform **runtime** dir.
///
/// - Unix (Linux/macOS): `$XDG_RUNTIME_DIR/skattr/ipc.sock`, falling back to
///   `$TMPDIR/skattr/ipc.sock`, then `/tmp/skattr/ipc.sock`.
/// - Windows: `%TEMP%\skattr\ipc.endpoint` — the named-pipe *discovery* file
///   (the pipe itself is a kernel object, not a file).
#[cfg(unix)]
pub fn default_ipc_endpoint() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("TMPDIR").map(PathBuf::from))
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    Ok(base.join("skattr").join(crate::daemon::ipc::ENDPOINT_FILENAME))
}

#[cfg(windows)]
pub fn default_ipc_endpoint() -> Result<PathBuf> {
    Ok(std::env::temp_dir()
        .join("skattr")
        .join(crate::daemon::ipc::ENDPOINT_FILENAME))
}

#[cfg(test)]
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
    fn ipc_endpoint_is_under_runtime_dir_not_data_dir() {
        // With XDG_RUNTIME_DIR set, the endpoint must live under it.
        // SAFETY: single-threaded test; we set then read one env var.
        std::env::set_var("XDG_RUNTIME_DIR", "/run/user/4242");
        let ep = default_ipc_endpoint().unwrap();
        assert_eq!(ep, PathBuf::from("/run/user/4242/skattr/ipc.sock"));
        std::env::remove_var("XDG_RUNTIME_DIR");
    }

    #[cfg(unix)]
    #[test]
    fn ipc_endpoint_filename_is_the_shared_constant() {
        let ep = default_ipc_endpoint().unwrap();
        assert_eq!(ep.file_name().unwrap(), crate::daemon::ipc::ENDPOINT_FILENAME);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core daemon::paths`
Expected: FAIL — `paths` module / functions not found (compile error) until Step 1+2 land together, then PASS once the file compiles. (If Step 1 and 2 are committed together the first run should already pass; the point is the test exists and exercises the behavior.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core daemon::paths`
Expected: PASS (3 tests on unix).

- [ ] **Step 5: Lint + commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all && cargo clippy -p skattr-core --all-targets -- -D warnings
git add crates/core/src/daemon/paths.rs crates/core/src/daemon/mod.rs
git commit -m "feat(core): single source of truth for data dir + IPC endpoint paths"
```

---

### Task 2: Route Config + CLI + UI through the resolvers

**Files:**
- Modify: `crates/core/src/daemon/config.rs:129-142` (`Config::defaults`)
- Modify: `crates/core/src/daemon/config.rs:172-205` (`ipc_socket_or_default`, both cfg arms)
- Modify: `crates/cli/src/main.rs:195-224` (`resolve_socket_path`), `:405-410` (`effective_data_dir`)
- Modify: `crates/ui/src/main.rs:142-151` (`consolidated_data_dir`), `:227`, `:322`
- Modify: `crates/ui/src/daemon.rs:80-87`

**Interfaces:**
- Consumes: `daemon::paths::data_dir`, `daemon::paths::default_ipc_endpoint` (Task 1).

- [ ] **Step 1: Point `Config::defaults` at the resolver**

In `crates/core/src/daemon/config.rs`, replace the body of `defaults()` (currently uses `directories::ProjectDirs::from("net","myggiz","skattr")`):

```rust
pub fn defaults() -> Result<Self> {
    Ok(Self {
        data_dir: crate::daemon::paths::data_dir()?,
        ipc_socket: None,
        log_filter: default_log_filter(),
        history: HistoryConfig::default(),
        delivery: DeliveryConfig::default(),
        notifications: NotificationsConfig::default(),
        ui: UiConfig::default(),
        download_dir: None,
    })
}
```

- [ ] **Step 2: Delegate `ipc_socket_or_default` to the resolver**

In `crates/core/src/daemon/config.rs`, the platform split now lives inside `paths::default_ipc_endpoint`, so the previously-`cfg`-split `ipc_socket_or_default` collapses to **one** non-`cfg` method. Delete both `#[cfg(unix)]` / `#[cfg(windows)]` arms (`:172-205`) and replace with a single:

```rust
/// Return the configured `ipc_socket` or the platform default runtime
/// endpoint (resolved by `paths::default_ipc_endpoint`).
pub fn ipc_socket_or_default(&self) -> Result<std::path::PathBuf> {
    if let Some(p) = &self.ipc_socket {
        return Ok(p.clone());
    }
    crate::daemon::paths::default_ipc_endpoint()
}
```

- [ ] **Step 3: Point the CLI at the resolvers**

In `crates/cli/src/main.rs`, the two `cfg` arms of `resolve_socket_path` (`:194-224`) now have identical bodies (the platform split moved into `default_ipc_endpoint`), so collapse them to **one** non-`cfg` function — flag > env > shared default:

```rust
/// Resolve the IPC endpoint path with precedence flag > env > shared default.
fn resolve_socket_path(flag: Option<&std::path::Path>) -> PathBuf {
    if let Some(p) = flag {
        return p.to_path_buf();
    }
    if let Some(env) = std::env::var_os("SKATTR_SOCKET") {
        return PathBuf::from(env);
    }
    skattr_core::daemon::paths::default_ipc_endpoint()
        .unwrap_or_else(|_| PathBuf::from(skattr_core::daemon::ipc::ENDPOINT_FILENAME))
}
```

And `effective_data_dir` (CLI `:405-410`) — replace the `Config::defaults()?.data_dir` fallback with the resolver directly:

```rust
fn effective_data_dir(override_dir: Option<&std::path::Path>) -> Result<PathBuf> {
    if let Some(d) = override_dir {
        return Ok(d.to_path_buf());
    }
    Ok(skattr_core::daemon::paths::data_dir()?)
}
```

- [ ] **Step 4: Point the UI at the resolver and stop pinning IPC to the data dir**

In `crates/ui/src/main.rs`, **delete** the `consolidated_data_dir()` function (`:142-151`) and replace its two call sites (`:227` and `:322`) with:

```rust
let data_dir = skattr_core::daemon::paths::data_dir()
    .map_err(|e| format!("resolve data dir: {e}"))?;
```

> Note: `:227` is in `main()` which returns `()` for the tracing setup — there it must not use `?`. Use:
> ```rust
> let data_dir = match skattr_core::daemon::paths::data_dir() {
>     Ok(d) => d,
>     Err(e) => {
>         eprintln!("fatal: cannot resolve data dir: {e}");
>         std::process::exit(1);
>     }
> };
> ```
> The setup-hook site (`:322`) returns `Result<_, Box<dyn Error>>`, so the `.map_err(...)?` form is correct there.

In `crates/ui/src/daemon.rs`, change `:87` so the daemon binds the **runtime** endpoint (UI binds, CLI connects to the same path):

```rust
// IPC endpoint lives in the runtime dir (shared resolver), not the data dir.
// Leaving ipc_socket = None makes run_with_transport call
// Config::ipc_socket_or_default() -> paths::default_ipc_endpoint().
config.ipc_socket = None;
```

(Delete the old `config.ipc_socket = Some(data_dir.join(... ENDPOINT_FILENAME))` line and its comment.)

- [ ] **Step 5: Build all three crates**

Run: `. "$HOME/.cargo/env" && cargo build -p skattr-core -p skattr-cli -p skattr-ui`
Expected: clean build. (If `skattr-ui` requires system webkit/pnpm and is unavailable in the agent env, at minimum `cargo build -p skattr-core -p skattr-cli` must pass; note the UI build status explicitly.)

- [ ] **Step 6: Lint + commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all && cargo clippy -p skattr-core -p skattr-cli --all-targets -- -D warnings
git add crates/core/src/daemon/config.rs crates/cli/src/main.rs crates/ui/src/main.rs crates/ui/src/daemon.rs
git commit -m "refactor: route Config, CLI, and UI through the shared path resolvers"
```

---

### Task 3: Pure OS advisory daemon lock (`daemon::lock`)

**Files:**
- Create: `crates/core/src/daemon/lock.rs`
- Modify: `crates/core/src/daemon/mod.rs`
- Modify: `crates/core/Cargo.toml` (windows-sys feature flags)

**Interfaces:**
- Produces:
  - `pub(crate) struct DaemonLock` — RAII guard; holds the lockfile handle open. Dropping it (or process death) releases the OS lock.
  - `pub(crate) fn acquire(data_dir: &std::path::Path) -> std::result::Result<DaemonLock, LockError>`
  - `pub(crate) enum LockError { AlreadyRunning, Io(std::io::Error) }`

- [ ] **Step 1: Register the module + windows feature flags**

In `crates/core/src/daemon/mod.rs`, add:

```rust
pub(crate) mod lock;
```

In `crates/core/Cargo.toml`, ensure the `[target.'cfg(windows)'.dependencies] windows-sys` `features` list includes (add any missing):

```toml
"Win32_Storage_FileSystem",
"Win32_System_IO",
"Win32_Foundation",
```

- [ ] **Step 2: Write the failing test (unix, in-process double-acquire)**

Create `crates/core/src/daemon/lock.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! A pure OS advisory lock guaranteeing one daemon per data directory.
//!
//! Acquisition is decided **only** by the OS lock call (`flock` on unix,
//! `LockFileEx` on Windows) against a held-open handle to
//! `<data_dir>/daemon.lock`. We never gate on the lockfile's existence or on
//! a pid inside it: the kernel auto-releases the lock when the holding
//! process dies (including SIGKILL / Task Manager), so a hard kill always
//! leaves a cleanly re-lockable state and there is no stale lock to reclaim.

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
                write!(f, "another skattr daemon is already using this data directory")
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
        .open(&path)
        .map_err(LockError::Io)?;
    lock_exclusive_nonblocking(&file)?;
    Ok(DaemonLock { _file: file })
}

#[cfg(unix)]
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
```

> If `tempfile` is not already a `dev-dependency` of `skattr-core`, check `crates/core/Cargo.toml` `[dev-dependencies]`; it is used widely in the IPC tests (`ipc/server/unix.rs` uses `tempfile::tempdir`), so it is present — reuse it.

- [ ] **Step 3: Run the test to verify it fails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core daemon::lock`
Expected: compile error / FAIL until the module compiles, then both tests run.

- [ ] **Step 4: Run the test to verify it passes**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core daemon::lock`
Expected: PASS — `second_acquire_on_same_dir_reports_already_running`, `lock_is_released_on_drop_and_reacquirable`.

- [ ] **Step 5: Lint + commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all && cargo clippy -p skattr-core --all-targets -- -D warnings
git add crates/core/src/daemon/lock.rs crates/core/src/daemon/mod.rs crates/core/Cargo.toml
git commit -m "feat(core): pure OS advisory single-daemon lock on <data_dir>/daemon.lock"
```

---

### Task 4: Hold the lock for the daemon's lifetime (in `run_with_sink`, before Pool + Arti)

**Critical ordering:** `run_with_transport` receives an **already-opened** `Pool` and an **already-constructed** Arti transport — both built inside `run_with_sink` (`Pool::open` at `state.rs:136`, `TorRuntime::bootstrap` with `state_dir: data_dir.join("arti")` at `:187`, `ArtiTransport::new` at `:192`) *before* `run_with_transport` is called (`:205`). A lock inside `run_with_transport` would therefore be **too late to prevent either the SQLite or the Arti/`hss` collision** — the whole point of the lock. The lock MUST be acquired at the **top of `run_with_sink`**, immediately after `std::fs::create_dir_all(data_dir)?` (`:115`) and before the first `Vault::open`. Required ordering (frontends do resolve+migrate; core does the rest):

```
resolve data_dir → migrate_legacy_into (frontend) → [run_with_sink:] create_dir_all → ACQUIRE LOCK → vault → Pool::open → TorRuntime::bootstrap → run_with_transport
```

Both production frontends route through `run_with_sink` (UI `daemon.rs:99`, CLI `main.rs:690`), so this one site guards both. The loopback guardrails call `run_with_transport` directly with separate temp dirs and are unaffected.

**Files:**
- Modify: `crates/core/src/error.rs` (add a variant)
- Modify: `crates/core/src/daemon/state.rs:104-136` (`run_with_sink`)
- Test: co-locate the lock-seam test in `state.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `daemon::lock::{acquire, DaemonLock, LockError}` (Task 3).
- Produces: `CoreError::DaemonAlreadyRunning` returned by `run_with_sink` when the data dir is already locked.

- [ ] **Step 1: Add the error variant**

In `crates/core/src/error.rs`, inside `enum CoreError`, add:

```rust
    #[error("another skattr daemon is already using this data directory")]
    DaemonAlreadyRunning,
```

- [ ] **Step 2: Write the failing guardrail test**

The single best proof is "second `run_with_transport` against a locked dir fails fast." Because spinning two full transports is heavy, prove it at the lock seam instead — add to the existing `#[cfg(test)] mod tests` in `crates/core/src/daemon/state.rs` (or the loopback harness):

```rust
#[tokio::test]
async fn second_daemon_on_locked_data_dir_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    // Hold the lock as if a daemon were already running.
    let _held = crate::daemon::lock::acquire(dir.path()).expect("first lock");
    // The startup guard run_with_sink uses must surface AlreadyRunning.
    let err = crate::daemon::lock::acquire(dir.path()).unwrap_err();
    assert!(matches!(err, crate::daemon::lock::LockError::AlreadyRunning));
}
```

(The end-to-end behavior — that `run_with_sink` maps this to `CoreError::DaemonAlreadyRunning` and aborts before `Pool::open`/Arti bootstrap — is verified by Step 3's wiring + the existing loopback guardrails continuing to pass, since each uses a fresh temp data dir.)

- [ ] **Step 3: Acquire the lock at the top of `run_with_sink`, before vault/Pool/Arti**

In `crates/core/src/daemon/state.rs`, inside `run_with_sink`, immediately **after** `std::fs::create_dir_all(data_dir)?;` (`:115`) and **before** the first `let vault_path = ...` / `Vault::open` (`:124`), insert:

```rust
        // Single-daemon guard: hold an OS advisory lock on <data_dir>/daemon.lock
        // for the whole run. A second daemon on the same data dir fails fast
        // HERE — before the Pool is opened (`:136`) or Arti touches `arti/`/`hss`
        // (`:187`) — rather than corrupting shared SQLite / Tor state or
        // double-publishing the onion. The handle lives in `_daemon_lock` until
        // this function returns; the OS also releases it on process death
        // (incl. SIGKILL), so a hard kill leaves a cleanly re-lockable state
        // with no stale-lock reclaim needed.
        let _daemon_lock = match crate::daemon::lock::acquire(data_dir) {
            Ok(g) => g,
            Err(crate::daemon::lock::LockError::AlreadyRunning) => {
                return Err(CoreError::DaemonAlreadyRunning);
            }
            Err(crate::daemon::lock::LockError::Io(e)) => return Err(CoreError::Io(e)),
        };

        // Belt-and-suspenders for point 3: the authoritative on-disk paths
        // (vault/Pool/arti/hs) already use the `data_dir` parameter, but a
        // migrated config.toml could carry a stale absolute `data_dir`. Force
        // the in-memory config to agree so config-derived paths (downloads,
        // <data_dir>/skattr.log) can't be re-pointed at the old location.
        let mut config = config;
        config.data_dir = data_dir.to_path_buf();
```

> `config` is the by-value parameter of `run_with_sink`; shadowing it as `mut` is fine — the rest of the function (the `config.clone()` into `config_arc` at `:160` and the move into `run_with_transport` at `:215`) then sees the normalised value. Confirm no earlier use of `config` between `:112` and the insert point (there is none — `create_dir_all` and vault opens come first).
> `data_dir` already exists here (`create_dir_all` ran at `:115`), so `acquire`'s `OpenOptions::create(true)` on `<data_dir>/daemon.lock` succeeds.
> Confirm `CoreError::Io(std::io::Error)` exists. If the variant is named differently, map `LockError::Io` to the closest IO-bearing variant; do not invent a new one for this.
> `Daemon::run` → `run_with_sink`, and both frontends call `run_with_sink`, so this is the only production site needed. Leave the test-only `run_loopback*` entrypoints unguarded.

- [ ] **Step 4: Run tests**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core daemon::state && cargo test -p skattr-tests`
Expected: the new `second_daemon_on_locked_data_dir_is_rejected` passes; existing loopback guardrails (e.g. `two_daemons_exchange_messages_both_directions_over_loopback`) still pass (each uses its own temp data dir, so no lock contention).

- [ ] **Step 5: Lint + commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add crates/core/src/error.rs crates/core/src/daemon/state.rs
git commit -m "feat(core): hold the single-daemon lock for the run_with_transport lifetime"
```

---

### Task 5: Shared legacy migration (`daemon::migrate`)

**Files:**
- Create: `crates/core/src/daemon/migrate.rs`
- Modify: `crates/core/src/daemon/mod.rs`

**Interfaces:**
- Produces:
  - `pub fn migrate_legacy_into(canonical: &std::path::Path) -> std::result::Result<(), MigrateError>`
  - `pub enum MigrateError { Move { from: PathBuf, name: String, source: std::io::Error }, ReadDir { dir: PathBuf, source: std::io::Error } }`

**Behavior contract:**
1. If `canonical/identity.vault` exists → `Ok(())` (idempotent; never re-migrates).
2. Else scan platform legacy sources in priority order; the **first** whose `identity.vault` exists is the source.
3. Move every entry from that source into `canonical` (whole set: vault, `skattr.sqlite*`, `hs.key.age`, `arti/`, `config.toml`, …). Cross-filesystem rename failures fall back to copy-then-remove.
4. Fail loud: any move error → `Err` (caller aborts startup; no fresh onboarding).
5. Set `canonical` to user-only perms (0700 on unix; Windows inherits per-user ACL under `%LOCALAPPDATA%`).

- [ ] **Step 1: Register the module**

In `crates/core/src/daemon/mod.rs`:

```rust
pub mod migrate;
```

- [ ] **Step 2: Write the failing tests**

Create `crates/core/src/daemon/migrate.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

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

#[derive(Debug)]
pub enum MigrateError {
    ReadDir { dir: PathBuf, source: std::io::Error },
    Move { from: PathBuf, name: String, source: std::io::Error },
}

impl std::fmt::Display for MigrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrateError::ReadDir { dir, source } => {
                write!(f, "migrate: read legacy dir {}: {source}", dir.display())
            }
            MigrateError::Move { from, name, source } => write!(
                f,
                "migrate: move {name} from {}: {source}",
                from.display()
            ),
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
            .or_else(|| {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share"))
            });
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
    for entry in entries.flatten() {
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

/// Idempotent, fail-loud migration of the first complete legacy state set
/// into `canonical`. No-op once `canonical` holds an identity vault.
pub fn migrate_legacy_into(canonical: &Path) -> Result<(), MigrateError> {
    if canonical.join(VAULT).exists() {
        return Ok(());
    }
    for cand in legacy_candidates() {
        if cand == *canonical {
            continue;
        }
        if cand.join(VAULT).exists() {
            std::fs::create_dir_all(canonical).map_err(|e| MigrateError::ReadDir {
                dir: canonical.to_path_buf(),
                source: e,
            })?;
            move_dir_contents(&cand, canonical)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_when_canonical_already_has_vault() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(VAULT), b"x").unwrap();
        migrate_legacy_into(dir.path()).unwrap();
        assert!(dir.path().join(VAULT).exists());
    }

    #[test]
    fn moves_complete_set_from_legacy_and_leaves_nothing_behind() {
        // Simulate a legacy dir and a canonical dir under one root, and point
        // the unix candidate scan at it via XDG_DATA_HOME.
        let root = tempfile::tempdir().unwrap();
        let xdg = root.path().join("xdg");
        let legacy = xdg.join("net.myggiz.skattr");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join(VAULT), b"vault").unwrap();
        std::fs::write(legacy.join("skattr.sqlite.age"), b"db").unwrap();
        std::fs::create_dir_all(legacy.join("arti")).unwrap();
        std::fs::write(legacy.join("arti").join("state"), b"tor").unwrap();

        let canonical = xdg.join("skattr");

        // SAFETY: single-threaded test.
        std::env::set_var("XDG_DATA_HOME", &xdg);
        let r = migrate_legacy_into(&canonical);
        std::env::remove_var("XDG_DATA_HOME");
        r.unwrap();

        assert!(canonical.join(VAULT).exists(), "vault moved");
        assert!(canonical.join("skattr.sqlite.age").exists(), "db moved");
        assert!(canonical.join("arti").join("state").exists(), "arti moved");
        assert!(!legacy.join(VAULT).exists(), "legacy vault gone");
    }

    #[test]
    fn no_legacy_means_clean_first_run() {
        let root = tempfile::tempdir().unwrap();
        let xdg = root.path().join("xdg-empty");
        std::fs::create_dir_all(&xdg).unwrap();
        let canonical = xdg.join("skattr");
        std::env::set_var("XDG_DATA_HOME", &xdg);
        let r = migrate_legacy_into(&canonical);
        std::env::remove_var("XDG_DATA_HOME");
        r.unwrap();
        assert!(!canonical.join(VAULT).exists(), "no vault appears from nowhere");
    }
}
```

> The env-var tests share process state; if the suite runs them in parallel and they flake, gate them behind a `serial_test`-style mutex **only if** that crate is already a dev-dep — otherwise keep them as written (each uses a distinct `XDG_DATA_HOME` value, and the candidate scan reads it fresh each call).

- [ ] **Step 3: Run the tests to verify they fail, then pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core daemon::migrate`
Expected: PASS — `noop_when_canonical_already_has_vault`, `moves_complete_set_from_legacy_and_leaves_nothing_behind`, `no_legacy_means_clean_first_run`.

- [ ] **Step 4: Lint + commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all && cargo clippy -p skattr-core --all-targets -- -D warnings
git add crates/core/src/daemon/migrate.rs crates/core/src/daemon/mod.rs
git commit -m "feat(core): shared fail-loud legacy-identity migration into canonical data dir"
```

---

### Task 6: Wire migration into both frontends; confirm the onboarding guard

**Files:**
- Modify: `crates/ui/src/main.rs:316-340` (setup hook) + delete `migrate_legacy_data` (`:153-206`)
- Modify: `crates/cli/src/main.rs` (`daemon()` at `:644`, before `Daemon::run_with_sink`)
- Read-only verify: `crates/ui/src/bootstrap.rs:16` (`vault_exists`) and the SvelteKit onboarding gate

**Interfaces:**
- Consumes: `daemon::migrate::migrate_legacy_into`, `daemon::paths::data_dir` (Tasks 1, 5).

- [ ] **Step 1: UI — call the shared migration, delete the local one**

In `crates/ui/src/main.rs`, in the setup hook (around `:340`), replace the call to the local `migrate_legacy_data(&data_dir)` with:

```rust
            skattr_core::daemon::migrate::migrate_legacy_into(&data_dir)
                .map_err(|e| format!("data migration failed: {e}"))?;
```

Then **delete** the now-unused local `migrate_legacy_data` function (`:153-206`). The `0700` perms set in the setup hook (`:325-334`) stays (it covers a fresh dir; migration also sets perms on the migrated dir).

- [ ] **Step 2: CLI — migrate before starting the daemon**

In `crates/cli/src/main.rs`, inside `daemon(...)` (`:644`), **before** the `Daemon::run_with_sink(...)` call (`:690`) and after the data dir is resolved, insert:

```rust
    // Carry any pre-existing identity from a legacy location into the
    // canonical data dir before the daemon opens it (fail-loud: abort rather
    // than silently onboarding anew).
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| anyhow::anyhow!("create data dir {}: {e}", data_dir.display()))?;
    skattr_core::daemon::migrate::migrate_legacy_into(&data_dir)
        .map_err(|e| anyhow::anyhow!("data migration failed: {e}"))?;
```

> Confirm the local variable holding the resolved data dir in `daemon()` (it comes from `effective_data_dir`/config). Use that variable's name; if the daemon receives `data_dir` via `Config`, migrate `config.data_dir` before `run_with_sink`.

- [ ] **Step 3: Verify the onboarding guard routes off the canonical dir**

Read `crates/ui/src/bootstrap.rs:16-23` (`vault_exists`) — it checks `data_dir.join("identity.vault")` against `AppState.data_dir`, which the setup hook sets to `paths::data_dir()` **after** migration. So an existing identity (post-migration) makes `vault_exists` return `true` → the frontend routes to unlock, not onboarding. Confirm the SvelteKit side gates the welcome/create flow on `vault_exists` (grep the frontend):

Run: `grep -rniE "vault_exists|vaultExists" crates/ui/src crates/ui/*/src 2>/dev/null`
Expected: the onboarding/welcome route checks `vault_exists` before showing "create passphrase". If it does, no code change — record the file:line as evidence. If it does **not**, that is a real gap → add a guard (a follow-up sub-step: invoke the welcome screen only when `vault_exists` is false).

- [ ] **Step 4: Build + test**

Run: `. "$HOME/.cargo/env" && cargo build -p skattr-core -p skattr-cli && cargo test -p skattr-core && cargo test -p skattr-tests`
Expected: clean build; all tests pass. (UI build per Task 2 Step 5 caveat.)

- [ ] **Step 5: Lint + commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add crates/ui/src/main.rs crates/cli/src/main.rs
git commit -m "feat: run shared legacy migration on both frontends before daemon start"
```

---

### Task 7: Grep guardrail, docs, and final gate

**Files:**
- Read-only verify: whole workspace
- Modify (if present): `CLAUDE.md` data-dir note; `docs/` install/threat-model paths if they cite the old path

- [ ] **Step 1: Prove no forbidden derivations remain**

Run each; all must return **no matches** in non-test data-path code:

```bash
cd /home/myggiz/development/skattr
# No exe-relative derivation anywhere:
grep -rniE "current_exe" crates/core/src crates/cli/src crates/ui/src
# No reverse-DNS / ProjectDirs for data or IPC paths (UI/CLI/config):
grep -rniE "ProjectDirs|net\.myggiz|\"net\", *\"myggiz\"" crates/ui/src crates/cli/src crates/core/src/daemon/config.rs
# No "." / CWD data-dir fallback in the UI resolver (it's deleted):
grep -nE "consolidated_data_dir|PathBuf::from\(\"\.\"\)" crates/ui/src/main.rs
```

Expected: empty output for the first two. (Migration's `legacy_candidates` legitimately references `net.myggiz` as a *legacy source* in `migrate.rs` — that is expected and correct; the greps above deliberately exclude `migrate.rs`.) The third must show `consolidated_data_dir` is gone.

- [ ] **Step 2: Confirm one resolver feeds all sub-paths**

Sanity-trace (read-only): `data_dir` → vault (`bootstrap.rs:49`), sqlite (`storage/pool.rs:73`), arti (`state.rs:178`), hs.key.age (`state.rs:312`) all `.join()` off the dir set by `paths::data_dir()`. IPC endpoint resolves via `paths::default_ipc_endpoint()` only. Record evidence in the task notes.

- [ ] **Step 3: Update docs if they cite the old path**

```bash
grep -rniE "net\.myggiz\.skattr/|~/\.local/share/net\.myggiz|%APPDATA%\\\\myggiz" docs CLAUDE.md README.md 2>/dev/null
```

For each hit that documents the *runtime* data location (not historical/migration notes), update to the canonical `~/.local/share/skattr` / `%LOCALAPPDATA%\skattr`. Leave threat-model/changelog history intact.

- [ ] **Step 4: Full workspace gate**

Run:
```bash
. "$HOME/.cargo/env" && cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test
```
Expected: fmt clean, clippy clean, all tests pass. Capture the test summary line as evidence.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "docs+chore: lock data-path foundation; grep guardrail; canonical-path docs"
```

---

## Notes / known risks (disclose, do not silently absorb)

- **macOS AF_UNIX path length:** on macOS `$TMPDIR` can be long; `…/skattr/ipc.sock` may approach the ~104-byte `sun_path` limit. This is pre-existing behavior (the old `ipc_socket_or_default` already used the TMPDIR fallback) and macOS is untested (no hardware). If a future macOS bring-up hits `bind` failures, shorten the runtime subpath. Not a regression for Linux/Windows.
- **Windows lock code is untested on-device** (no Windows in the agent env). It compiles under `cfg(windows)` and uses the same `windows-sys 0.59` crate as the IPC server. Flag for the Windows field tester as the primary acceptance check (AC: non-admin onboarding, no `os error 5`).
- **Migration `Program Files` sources are read-only for a non-admin user** — but we only *read*/move *out* of them; the destination is the writable canonical dir, so the move's writes never touch Program Files. If the source itself is unreadable, `read_dir` fails loud (correct — better than silent fresh onboarding).
- **Runtime socket parent dir — already handled, no change.** Both `Server::bind` arms `create_dir_all` the endpoint's parent before binding: unix `ipc/server/unix.rs:33-34` (+chmod 0700), windows `ipc/server/windows.rs:158-159`. So moving the endpoint to `$XDG_RUNTIME_DIR/skattr/` (or `%TEMP%\skattr\`) binds cleanly even when the `skattr/` subdir doesn't exist yet.
- **Stale `config.toml` `data_dir` — closed in Task 4.** `Config` serializes `data_dir` (no `#[serde(skip)]`; `save_to_disk` at `config.rs:333` writes it), but the authoritative paths use the `data_dir` *parameter* of `run_with_sink`, not `config.data_dir`. The UI overwrites `config.data_dir` post-load (`main.rs:352`, `daemon.rs:85`) and the CLI daemon uses `Config::defaults()` (resolver) without loading the migrated file. Task 4 additionally normalises `config.data_dir = data_dir` at the top of `run_with_sink` so config-derived paths (downloads, log) also can't be re-pointed by a migrated file.
- **Headless workflow is `skattr daemon` first, then commands — intended.** Plain CLI commands (`send`/`list`/…) connect-or-error (exit code 3 with "Start it with: skattr daemon"); they do **not** auto-spawn a daemon. Correct for a systemd-managed box and avoids a connect-or-spawn race. Not a gap.
- **M-1 — migration runs in the frontends *before* the core daemon lock (accepted tradeoff, not a defect).** CLI/UI call `migrate_legacy_into` before `run_with_sink` acquires `<data_dir>/daemon.lock`, so two simultaneously-started processes could both enter migration in the narrow pre-lock window. We **deliberately decline** a lock-around-migration because the worst case is already bounded to a non-corrupting outcome by the migration's design:
  - **Copy-before-remove ordering** — migration copies the legacy set into the canonical dir and verifies, then removes the legacy source; it never moves/renames or removes-before-verify, so state never exists in neither location.
  - **Idempotent** — if `canonical/identity.vault` exists, migration is a no-op; a second process arriving after the first completed finds nothing to do.
  - **Worst case under concurrency** is therefore a *one-time spurious abort*: the losing process hits a transient mid-copy conflict and exits, then retries cleanly because the now-completed migration is idempotent. No partial state, no corruption, no data loss — "one process exits and is restarted," not "the data dir is damaged."
  - The daemon lock already serializes daemons past startup; this race is only the pre-lock window, and copy-before-remove + idempotency make it non-destructive. A lock around migration would add heavyweight serialization to prevent a self-healing, non-corrupting abort — cost without matching risk. (CodeRabbit PR #25 raised this; declined with this reasoning.)
- **Out of scope (do not add):** tray menu, default-quit, relocation pointer file, panic-delete, tmpfs/encrypted-volume support, stale-lock auto-reclaim (the OS lock makes the last one unnecessary by construction), CLI auto-spawn of a daemon, lock-around-migration (see M-1).
