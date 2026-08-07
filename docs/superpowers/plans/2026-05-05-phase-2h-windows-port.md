# Phase 2.H Windows Port Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port `core::daemon::ipc::{server,client}` to Windows Named Pipes alongside the existing AF_UNIX implementation, ship a `.msi` bundle, and gate Windows in CI from day one — so v0.2 can drop the "Windows deferred" disclaimer.

**Architecture:** Per-platform submodules under `server/` and `client/`, sharing platform-neutral request/response loops. Windows uses Tokio Named Pipes with a custom DACL (owner-SID-only) plus a post-accept SID equality check. Daemon writes the random pipe name to `<data_dir>\ipc.endpoint`; client reads it to find the pipe. `IpcStream` type alias and `ENDPOINT_FILENAME` const make every other layer (CLI, UI, tests, wire format) platform-neutral.

**Tech Stack:** Rust 1.95.0 (pinned), Tokio (named-pipe API), `windows-sys = "0.59"` for raw SECURITY_DESCRIPTOR / SID FFI, `rand::OsRng` for pipe-name entropy, `directories::ProjectDirs` for `%APPDATA%` resolution, GitHub Actions `windows-latest` runner, `tauri-action` for the WiX `.msi`.

**Spec:** `docs/superpowers/specs/2026-05-05-phase-2h-windows-port-design.md`.

---

## File Structure

**Created:**
- `crates/core/src/daemon/ipc/server/mod.rs` — `CommandExecutor` trait, `handle_connection<S>`, `serve()`, `event_matches()`, platform-neutral unit tests, cfg-gated `pub use` re-exports of `Server`.
- `crates/core/src/daemon/ipc/server/unix.rs` — `Server`, `bind`, `accept_one`, `Drop`, `current_uid`, `check_peer_uid` (Unix). Compiled `#[cfg(unix)]`.
- `crates/core/src/daemon/ipc/server/windows.rs` — `Server`, `bind`, `accept_one`, `Drop`, `current_sid`, `peer_sid_for`, `check_peer_sid` (Windows). Compiled `#[cfg(target_os = "windows")]`.
- `crates/core/src/daemon/ipc/client/mod.rs` — `IpcClient<S>` generic body, `IpcClientError`, `from_stream`, platform-neutral tests, cfg-gated `pub use` of `connect`.
- `crates/core/src/daemon/ipc/client/unix.rs` — `IpcClient::connect(&Path)` over `UnixStream` (Unix).
- `crates/core/src/daemon/ipc/client/windows.rs` — `IpcClient::connect(&Path)` reads discovery file → `NamedPipeClient` (Windows).
- `docs/install/windows.md` — Windows install + verify + first-run + SmartScreen walkthrough.

**Deleted:**
- `crates/core/src/daemon/ipc/server.rs` — content split into `server/{mod,unix}.rs`.
- `crates/core/src/daemon/ipc/client.rs` — content split into `client/{mod,unix}.rs`.

**Modified:**
- `crates/core/src/daemon/ipc/mod.rs` — add `pub type IpcStream`, `pub type PeerId`, `pub const ENDPOINT_FILENAME`; switch `pub mod server` / `pub mod client` to directory modules.
- `crates/core/Cargo.toml` — add `[target.'cfg(windows)'.dependencies] windows-sys = "0.59"` block.
- `crates/cli/src/main.rs` — concrete `IpcClient<UnixStream>` → `IpcClient<IpcStream>`; `resolve_socket_path` becomes cfg-conditional.
- `crates/ui/src/daemon.rs` — line 70 hardcoded `"ipc.sock"` → `ENDPOINT_FILENAME` const.
- `crates/mailbox/src/health.rs` — gate `UnixListener`/`UnixStream`/`set_mode` behind `#[cfg(unix)]`; `#[cfg(windows)]` returns `MailboxError::Unsupported` from `bind` (mailbox is not shipped on Windows; the gate is for cross-compile cleanliness only).
- `crates/mailbox/src/config.rs` — `health_socket_default()` becomes `cfg(unix)`-only (mailbox doesn't run on Windows; gate prevents cross-compile failure).
- `.github/workflows/ci.yml` — add `windows-latest` to `test` matrix; promote `clippy` job to a matrix over `[ubuntu-latest, windows-latest]`; remove the Windows-omitted comment.
- `.github/workflows/release.yml` — add `windows-latest` to build matrix; new Windows-specific smoke step; ingest `.msi` artefact into `SHA256SUMS`.
- `docs/install/README.md` — Windows row in platform table; minisign verify recipe.
- `docs/build/reproducible.md` — Windows section noting non-byte-reproducibility for v0.1.
- `CLAUDE.md` — repository-state paragraph.
- `CHANGELOG.md` — Phase 2.H entry.

---

## Task ordering

Tasks 1–5 are pure-refactor prep on Unix (no behavior change); 6–7 are scaffolding so Windows compiles; 8–13 implement Windows server; 14 implements Windows client; 15–17 finish endpoint resolution and cross-compile cleanup; 18–19 wire CI; 20–22 land the docs and CHANGELOG.

**Cross-compile expectation:** From task 6 onwards, every commit must `cargo check --target x86_64-pc-windows-msvc -p skattr-core` clean (run from Linux). Run `rustup target add x86_64-pc-windows-msvc` once at the start of task 6 if not already present.

---

## Task 1: Split `server.rs` into `server/{mod,unix}.rs` (Unix-only refactor)

**Files:**
- Create: `crates/core/src/daemon/ipc/server/mod.rs`
- Create: `crates/core/src/daemon/ipc/server/unix.rs`
- Delete: `crates/core/src/daemon/ipc/server.rs`
- Modify: `crates/core/src/daemon/ipc/mod.rs:8` (`pub mod server;` line stays; just becomes a directory module)

**Goal:** Behavior-preserving move. The platform-neutral parts (`CommandExecutor`, `handle_connection`, `serve`, `event_matches`, `receive_if_some`, all tests that use `tokio::io::duplex`) live in `mod.rs`. The Unix-bound parts (`Server`, `bind`, `accept_one`, `Drop`, `current_uid`, `check_peer_uid`, the two `bind_*` tests) live in `unix.rs` behind `#[cfg(unix)]`.

- [ ] **Step 1: Verify the existing tests pass on Unix.**

```bash
cargo test -p skattr-core --features test-harness daemon::ipc::server -- --nocapture
```

Expected: all 14 tests in `daemon::ipc::server` pass (today's `server.rs` has 13 tests matching the basic `#[tokio::test]` form plus 1 with the `(flavor = "multi_thread", worker_threads = 2)` variant — `subscribe_ack_replays_cached_tor_status`).

- [ ] **Step 2: Create `crates/core/src/daemon/ipc/server/mod.rs` with the platform-neutral code.**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic))]

//! IPC server. Platform-neutral request/response loop on top of the
//! per-platform listener (Unix domain socket on Unix; Named Pipe on
//! Windows). The platform-specific `Server` type is re-exported from
//! the active child module.

use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::broadcast;

use crate::daemon::commands::{Command, CommandResult};
use crate::daemon::events::Event;
use crate::daemon::ipc::codec::{read_frame, write_frame, CodecError};
use crate::daemon::ipc::wire::{EventFilter, IpcError, IpcRequest, IpcResponse};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::Server;
#[cfg(unix)]
pub(crate) use unix::current_uid;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::Server;
#[cfg(target_os = "windows")]
pub(crate) use windows::current_sid;

/// Execute one `Command` and return its `CommandResult` or a typed
/// `IpcError`. Decouples the per-connection handler from the concrete
/// `DaemonHandle` so the unit tests can drive the handler with a mock.
#[async_trait::async_trait]
pub trait CommandExecutor: Send + Sync {
    async fn execute(&self, cmd: Command) -> std::result::Result<CommandResult, IpcError>;
    fn latest_tor_status(&self) -> Option<crate::daemon::events::TorStatus> {
        None
    }
}

/// Handle one accepted connection.
pub async fn handle_connection<S>(
    mut stream: S,
    executor: Arc<dyn CommandExecutor>,
    events_tx: broadcast::Sender<Event>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // ... PASTE the entire handle_connection body verbatim from today's
    // server.rs lines 153-266. No edits.
}

/// Accept loop. Spawns [`handle_connection`] per accepted stream.
pub async fn serve(
    server: Server,
    executor: Arc<dyn CommandExecutor>,
    events_tx: broadcast::Sender<Event>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) {
    // ... PASTE verbatim from today's server.rs lines 270-301.
}

async fn receive_if_some(rx: Option<&mut broadcast::Receiver<Event>>) -> Option<Event> {
    // ... PASTE verbatim from today's server.rs lines 303-312.
}

fn event_matches(event: &Event, filter: Option<&EventFilter>) -> bool {
    // ... PASTE verbatim from today's server.rs lines 314-341.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::ipc::wire::IpcError;
    use async_trait::async_trait;

    // PASTE the platform-neutral tests from today's server.rs:
    //   per_conn_execute_returns_ok_and_bye
    //   per_conn_subscribe_forwards_events_then_execute_still_works
    //   per_conn_unknown_command_returns_err_but_keeps_connection
    //   event_matches_filters_message_received_by_contact
    //   event_filter_mailboxes_matches_only_mailbox_status
    //   event_filter_delivery_matches_only_delivery_status
    //   event_filter_contact_matches_contact_card_received_for_same_peer
    //   event_filter_all_matches_new_events
    //   subscribe_ack_replays_cached_tor_status
    // Plus the EchoExec / StubExec test fixtures used by them.
    // The two bind_* mode tests stay in unix.rs (Step 3).
}
```

- [ ] **Step 3: Create `crates/core/src/daemon/ipc/server/unix.rs` with the Unix-bound code.**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg(unix)]
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
    pub fn bind(path: &Path, allowed_uid: u32) -> Result<Self> {
        // PASTE verbatim from today's server.rs lines 31-58.
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn accept_one(&self) -> std::result::Result<tokio::net::UnixStream, IpcError> {
        // PASTE verbatim from today's server.rs lines 69-80.
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(crate) fn current_uid() -> u32 {
    // PASTE verbatim from today's server.rs lines 96-107.
}

pub(crate) fn check_peer_uid(peer_uid: Option<u32>, expected: u32) -> io::Result<()> {
    // PASTE verbatim from today's server.rs lines 111-123.
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
        // PASTE verbatim from today's server.rs lines 363-388.
    }

    #[tokio::test]
    async fn bind_unlinks_stale_socket_file() {
        // PASTE verbatim from today's server.rs lines 391-400.
    }
}
```

- [ ] **Step 4: Delete the old file.**

```bash
git rm crates/core/src/daemon/ipc/server.rs
```

- [ ] **Step 5: Run all tests and clippy.**

```bash
cargo test  -p skattr-core --features test-harness
cargo clippy -p skattr-core --all-targets --all-features -- -D warnings
```

Expected: all tests pass; clippy clean. Any compile error is a paste mistake — reconcile against today's `server.rs` (commit `5df4fe5`).

- [ ] **Step 6: Commit.**

```bash
git add crates/core/src/daemon/ipc/server/ crates/core/src/daemon/ipc/server.rs
git commit -m "refactor(ipc): split server.rs into server/{mod,unix}.rs"
```

---

## Task 2: Split `client.rs` into `client/{mod,unix}.rs` (Unix-only refactor)

**Files:**
- Create: `crates/core/src/daemon/ipc/client/mod.rs`
- Create: `crates/core/src/daemon/ipc/client/unix.rs`
- Delete: `crates/core/src/daemon/ipc/client.rs`

**Goal:** Same shape as Task 1. The generic `IpcClient<S>` body, `IpcClientError`, `from_stream`, `execute`, `subscribe`, `next_event`, the duplex-driven tests live in `mod.rs`. Only the `impl IpcClient<UnixStream> { connect }` block plus the Unix-specific `connect_missing_socket_returns_daemon_not_running` test live in `unix.rs`.

- [ ] **Step 1: Create `crates/core/src/daemon/ipc/client/mod.rs`.**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic))]

//! IPC client. The platform-specific `connect` impl is in the
//! active child module; the rest is generic over the stream type.

use std::io;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};

use crate::daemon::commands::{Command, CommandResult};
use crate::daemon::events::Event;
use crate::daemon::ipc::codec::{read_frame, write_frame, CodecError};
use crate::daemon::ipc::wire::{EventFilter, IpcError, IpcRequest, IpcResponse};

#[cfg(unix)]
mod unix;

#[cfg(target_os = "windows")]
mod windows;

#[derive(Debug, Error)]
pub enum IpcClientError {
    // PASTE verbatim from today's client.rs lines 25-41.
}

impl From<CodecError> for IpcClientError {
    // PASTE verbatim from today's client.rs lines 43-50.
}

#[derive(Debug)]
pub struct IpcClient<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    pub(super) stream: BufReader<S>,
    pub(super) subscribed: bool,
}

impl<S> IpcClient<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    pub fn from_stream(stream: S) -> Self {
        Self {
            stream: BufReader::new(stream),
            subscribed: false,
        }
    }

    pub async fn execute(
        &mut self,
        cmd: Command,
    ) -> std::result::Result<CommandResult, IpcClientError> {
        // PASTE verbatim from today's client.rs lines 97-112.
    }

    pub async fn subscribe(
        &mut self,
        filter: EventFilter,
    ) -> std::result::Result<(), IpcClientError> {
        // PASTE verbatim from today's client.rs lines 116-129.
    }

    pub async fn next_event(&mut self) -> std::result::Result<Event, IpcClientError> {
        // PASTE verbatim from today's client.rs lines 134-143.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::ipc::server::{handle_connection, CommandExecutor};
    // PASTE the duplex-driven tests from today's client.rs:
    //   execute_roundtrip_over_duplex
    //   subscribe_streams_events
    // Plus OkExec.
}
```

- [ ] **Step 2: Create `crates/core/src/daemon/ipc/client/unix.rs`.**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg(unix)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic))]

//! IPC client, Unix half. `connect(&Path)` over `UnixStream`.

use std::io;
use std::path::Path;

use tokio::net::UnixStream;

use super::{IpcClient, IpcClientError};

impl IpcClient<UnixStream> {
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
```

- [ ] **Step 3: Delete the old file.**

```bash
git rm crates/core/src/daemon/ipc/client.rs
```

- [ ] **Step 4: Verify the existing `pub use` in `crates/core/src/daemon/ipc/mod.rs` still resolves.**

`mod.rs` line 11 today says `pub use client::{IpcClient, IpcClientError};`. With the directory module split, `IpcClient` and `IpcClientError` are now in `client/mod.rs` — same path, same re-export. Should compile unchanged.

- [ ] **Step 5: Run all tests and clippy.**

```bash
cargo test  -p skattr-core --features test-harness
cargo clippy -p skattr-core --all-targets --all-features -- -D warnings
```

Expected: green.

- [ ] **Step 6: Commit.**

```bash
git add crates/core/src/daemon/ipc/client/ crates/core/src/daemon/ipc/client.rs
git commit -m "refactor(ipc): split client.rs into client/{mod,unix}.rs"
```

---

## Task 3: Add `IpcStream`, `PeerId`, `ENDPOINT_FILENAME` cross-platform aliases

**Files:**
- Modify: `crates/core/src/daemon/ipc/mod.rs`

**Goal:** Introduce the three cross-platform symbols. On Unix everything resolves to today's types; the Windows arm of each cfg is a placeholder (`type Windows = ...` referring to the `tokio::net::windows::named_pipe::NamedPipeClient` which exists on Windows but is gated unreachable from here until task 6 adds the `windows-sys` dep). Even with no Windows code yet compiled, the `#[cfg(target_os = "windows")]` arm is allowed to mention `tokio::net::windows::*` — it's only compiled on Windows targets.

- [ ] **Step 1: Replace `crates/core/src/daemon/ipc/mod.rs` with the new content.**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! CLI ↔ daemon IPC transport.
//!
//! Cross-platform aliases:
//!   - `IpcStream`       — the client-side stream type for IPC connections.
//!   - `PeerId`          — opaque "this user" identity for peer auth.
//!   - `ENDPOINT_FILENAME` — relative file under `data_dir` the daemon
//!                          binds (Unix) or writes the pipe name to
//!                          (Windows).

pub mod client;
pub mod codec;
pub mod server;
pub mod wire;

pub use client::{IpcClient, IpcClientError};

/// Filename (relative to `data_dir`) of the daemon's IPC endpoint.
/// On Unix this is the AF_UNIX socket file; on Windows it is the
/// discovery file containing the named-pipe name.
#[cfg(unix)]
pub const ENDPOINT_FILENAME: &str = "ipc.sock";
#[cfg(target_os = "windows")]
pub const ENDPOINT_FILENAME: &str = "ipc.endpoint";

/// Client-side IPC stream type. Selected at compile time per platform.
#[cfg(unix)]
pub type IpcStream = tokio::net::UnixStream;
#[cfg(target_os = "windows")]
pub type IpcStream = tokio::net::windows::named_pipe::NamedPipeClient;

/// Opaque "this user" identity for the daemon's peer-auth allow-list.
/// Unix: numeric uid. Windows: raw SID bytes (variable length).
#[cfg(unix)]
pub type PeerId = u32;
#[cfg(target_os = "windows")]
pub type PeerId = Vec<u8>;
```

- [ ] **Step 2: Run tests and check.**

```bash
cargo check  -p skattr-core --all-targets
cargo test   -p skattr-core --features test-harness
```

Expected: green on Unix.

- [ ] **Step 3: Commit.**

```bash
git add crates/core/src/daemon/ipc/mod.rs
git commit -m "feat(ipc): add IpcStream, PeerId, ENDPOINT_FILENAME aliases"
```

---

## Task 4: Convert `Server::bind`'s `allowed_uid: u32` → `allowed: PeerId`

**Files:**
- Modify: `crates/core/src/daemon/ipc/server/unix.rs`
- Modify: `crates/core/src/daemon/mod.rs` (or wherever `Daemon::run` calls `Server::bind`; grep first)

**Goal:** Rename only — `PeerId` on Unix *is* `u32`. Same wire, same behavior, but the signature is now platform-neutral so the future Windows `Server::bind` matches.

- [ ] **Step 1: Locate every call to `Server::bind`.**

```bash
grep -rn "Server::bind\|server::Server::bind" crates/ --include='*.rs'
```

Expected hits: at least `crates/core/src/daemon/mod.rs` (the daemon-startup site) and the two `bind_*` tests in `server/unix.rs`. Note all hits.

- [ ] **Step 2: Edit `crates/core/src/daemon/ipc/server/unix.rs::Server::bind` signature.**

```rust
use crate::daemon::ipc::PeerId;

pub struct Server {
    listener: UnixListener,
    path: PathBuf,
    allowed: PeerId,
}

impl Server {
    pub fn bind(path: &Path, allowed: PeerId) -> Result<Self> {
        // body unchanged; `allowed_uid` field rename → `allowed`.
        // ...
        Ok(Self {
            listener,
            path: path.to_path_buf(),
            allowed,
        })
    }

    pub async fn accept_one(&self) -> std::result::Result<tokio::net::UnixStream, IpcError> {
        let (stream, _) = self.listener.accept().await
            .map_err(|e| IpcError::Internal(format!("accept: {e}")))?;
        let cred = stream.peer_cred()
            .map_err(|e| IpcError::Internal(format!("peer_cred: {e}")))?;
        check_peer_uid(Some(cred.uid()), self.allowed).map_err(|_| IpcError::AuthDenied)?;
        Ok(stream)
    }
}
```

- [ ] **Step 3: Update `current_uid` to return `PeerId`.**

```rust
pub(crate) fn current_uid() -> PeerId {
    // body unchanged; return type changed from u32 to PeerId (alias).
    // ...
}
```

- [ ] **Step 4: Update every call site found in Step 1.**

In each file that called `Server::bind(path, my_uid)` or `current_uid()`, the argument/return is now `PeerId`. Since `PeerId = u32` on Unix this is a type-name change only. Existing `let allowed_uid: u32 = current_uid();` becomes `let allowed: PeerId = current_uid();` (variable rename optional but recommended for grep-ability).

- [ ] **Step 5: Run tests and clippy.**

```bash
cargo test  -p skattr-core --features test-harness
cargo clippy -p skattr-core --all-targets --all-features -- -D warnings
```

Expected: green.

- [ ] **Step 6: Commit.**

```bash
git add -u crates/core/src/daemon/
git commit -m "refactor(ipc): Server::bind takes PeerId instead of u32"
```

---

## Task 5: Drop the concrete `IpcClient<UnixStream>` from CLI; use `ENDPOINT_FILENAME` in UI

**Files:**
- Modify: `crates/cli/src/main.rs:200` (`connect_or_exit` return type)
- Modify: `crates/ui/src/daemon.rs:70` (hard-coded `"ipc.sock"`)

**Goal:** Two one-line edits that purge the last cross-platform-hostile literals.

- [ ] **Step 1: Edit `crates/cli/src/main.rs` `connect_or_exit`.**

Change:

```rust
async fn connect_or_exit(
    sock_flag: Option<&std::path::Path>,
) -> Result<skattr_core::daemon::IpcClient<tokio::net::UnixStream>> {
```

to:

```rust
async fn connect_or_exit(
    sock_flag: Option<&std::path::Path>,
) -> Result<skattr_core::daemon::IpcClient<skattr_core::daemon::ipc::IpcStream>> {
```

- [ ] **Step 2: Edit `crates/ui/src/daemon.rs:70`.**

Change:

```rust
config.ipc_socket = Some(data_dir.join("ipc.sock"));
```

to:

```rust
config.ipc_socket = Some(data_dir.join(skattr_core::daemon::ipc::ENDPOINT_FILENAME));
```

- [ ] **Step 3: Run all tests across the workspace.**

```bash
cargo test --workspace --exclude skattr-ui --features test-harness
cargo clippy --workspace --exclude skattr-ui --all-targets --all-features -- -D warnings
```

Expected: green.

- [ ] **Step 4: Commit.**

```bash
git add crates/cli/src/main.rs crates/ui/src/daemon.rs
git commit -m "refactor(ipc): purge concrete UnixStream from CLI; use ENDPOINT_FILENAME in UI"
```

---

## Task 6: Add `windows-sys` Cargo dep + cross-compile-check baseline

**Files:**
- Modify: `crates/core/Cargo.toml`

**Goal:** From this commit onward, `cargo check --target x86_64-pc-windows-msvc -p skattr-core` must succeed (run from Linux). This task adds the dep and verifies the baseline.

- [ ] **Step 1: Install the Windows target if not already present.**

```bash
rustup target add x86_64-pc-windows-msvc
```

Expected: "info: component 'rust-std' for target 'x86_64-pc-windows-msvc' is up to date" or installed.

- [ ] **Step 2: Append the Windows-only dep block to `crates/core/Cargo.toml`.**

After the existing `[features]` block (line 100ish), append:

```toml
[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.59", features = [
    "Win32_Foundation",
    "Win32_Security",
    "Win32_Security_Authorization",
    "Win32_System_Threading",
    "Win32_System_Pipes",
    "Win32_Storage_FileSystem",
] }
```

- [ ] **Step 3: Cross-compile check.**

```bash
cargo check --target x86_64-pc-windows-msvc -p skattr-core
```

Expected: success. The check must also be clean for the `skattr_core::daemon::ipc::IpcStream = tokio::net::windows::named_pipe::NamedPipeClient` alias added in Task 3 — Tokio's named_pipe types ship with the `net` feature on Windows (already enabled).

If the check fails because Tokio's `net` feature doesn't expose `windows::named_pipe`, add `tokio = { workspace = true, features = ["net"] }` to a `[target.'cfg(windows)'.dependencies]` block (the workspace Tokio already enables `net`; explicit pin is belt-and-braces).

- [ ] **Step 4: Linux test pass (sanity, dep should not affect Linux).**

```bash
cargo test -p skattr-core --features test-harness
```

Expected: green.

- [ ] **Step 5: Commit.**

```bash
git add crates/core/Cargo.toml Cargo.lock
git commit -m "build(core): add windows-sys dep for Win32 SID + DACL FFI"
```

---

## Task 7: Scaffold `server/windows.rs` and `client/windows.rs` (compile, not yet functional)

**Files:**
- Create: `crates/core/src/daemon/ipc/server/windows.rs`
- Create: `crates/core/src/daemon/ipc/client/windows.rs`

**Goal:** Provide the `Server` struct and `IpcClient::connect` impl needed for cross-compile, with `todo!()` bodies. After this commit, `cargo check --target x86_64-pc-windows-msvc -p skattr-core` builds; no Windows test runs yet.

- [ ] **Step 1: Create `crates/core/src/daemon/ipc/server/windows.rs`.**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

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
```

- [ ] **Step 2: Create `crates/core/src/daemon/ipc/client/windows.rs`.**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg(target_os = "windows")]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic))]

//! IPC client, Windows half. Reads the discovery file and connects
//! to the named pipe.

use std::io;
use std::path::Path;

use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};

use super::{IpcClient, IpcClientError};

impl IpcClient<NamedPipeClient> {
    pub async fn connect(_path: &Path) -> std::result::Result<Self, IpcClientError> {
        todo!("Phase 2.H Task 14: Windows discovery-file + NamedPipeClient::connect")
    }
}
```

- [ ] **Step 3: Cross-compile check.**

```bash
cargo check --target x86_64-pc-windows-msvc -p skattr-core
```

Expected: success.

- [ ] **Step 4: Linux check (the `cfg(target_os = "windows")` blocks must not break Unix).**

```bash
cargo test -p skattr-core --features test-harness
```

Expected: green.

- [ ] **Step 5: Commit.**

```bash
git add crates/core/src/daemon/ipc/server/windows.rs crates/core/src/daemon/ipc/client/windows.rs
git commit -m "feat(ipc): scaffold Windows server + client (todo!() bodies)"
```

---

## Task 8: Implement `current_sid()` for Windows

**Files:**
- Modify: `crates/core/src/daemon/ipc/server/windows.rs`

**Goal:** Replace the `current_sid()` `todo!()` with a working implementation that returns the daemon's user SID as `Vec<u8>`. This is the most contained `unsafe` block in the plan — get it right first; later tasks reuse the pattern.

- [ ] **Step 1: Add the test stub in `server/windows.rs::tests`** (gated `#[cfg(test)]`).

```rust
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
```

- [ ] **Step 2: Implement `current_sid()`.**

```rust
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::{
    CopySid, GetLengthSid, GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

pub(crate) fn current_sid() -> PeerId {
    // SAFETY: All FFI is to documented Win32 APIs. We close the
    // process-token handle on every exit path. The TOKEN_USER buffer is
    // sized via the standard two-call pattern. CopySid into a fresh Vec
    // gives us a stable, owned SID that outlives the process token.
    unsafe {
        let mut token: HANDLE = 0;
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            // Must not happen for our own process; fall back to an empty
            // SID which `check_peer_sid` will reject.
            tracing::error!("OpenProcessToken on self failed: {}", io::Error::last_os_error());
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
```

- [ ] **Step 3: Cross-compile check from Linux.**

```bash
cargo check --target x86_64-pc-windows-msvc -p skattr-core
```

Expected: success. The unit test compiles but won't run from Linux.

- [ ] **Step 4: (Skip Windows test execution at this step.)** The `current_sid_returns_non_empty_well_formed_sid` test will be exercised on the `windows-latest` CI runner once Task 18 lands. Document this in the commit message.

- [ ] **Step 5: Commit.**

```bash
git add crates/core/src/daemon/ipc/server/windows.rs
git commit -m "feat(ipc): implement current_sid() for Windows (CI-tested)"
```

---

## Task 9: Implement `check_peer_sid()` and `peer_sid_for(handle)` for Windows

**Files:**
- Modify: `crates/core/src/daemon/ipc/server/windows.rs`

**Goal:** Two helpers needed by `accept_one`. `peer_sid_for(handle: HANDLE)` extracts the connecting client's SID via `GetNamedPipeClientProcessId` → `OpenProcessToken` → `GetTokenInformation(TokenUser)`. `check_peer_sid(peer, expected)` wraps `EqualSid`.

- [ ] **Step 1: Add tests** to the existing `tests` mod.

```rust
#[test]
fn check_peer_sid_accepts_matching_sid() {
    let sid = current_sid();
    assert!(!sid.is_empty());
    assert!(check_peer_sid(&sid, &sid).is_ok());
}

#[test]
fn check_peer_sid_rejects_mismatched_sid() {
    let mut a = current_sid();
    let mut b = a.clone();
    // Flip the last sub-authority byte to invalidate the SID.
    let last = b.len() - 1;
    b[last] = b[last].wrapping_add(1);
    assert!(check_peer_sid(&a, &b).is_err());
    let _ = a.pop(); // touch a to dodge "unused mut"
}

#[test]
fn check_peer_sid_rejects_empty_peer() {
    let me = current_sid();
    assert!(check_peer_sid(&[], &me).is_err());
}
```

- [ ] **Step 2: Implement the two helpers.**

```rust
use windows_sys::Win32::Foundation::{ERROR_INVALID_SID, FALSE};
use windows_sys::Win32::Security::EqualSid;
use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

pub(crate) fn check_peer_sid(peer: &[u8], expected: &[u8]) -> io::Result<()> {
    if peer.is_empty() || expected.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "empty SID",
        ));
    }
    // SAFETY: EqualSid takes two PSIDs (raw byte pointers). Both inputs
    // are non-empty Vec<u8> slices owned by the caller; their pointers
    // are valid for the call.
    let eq = unsafe {
        EqualSid(
            peer.as_ptr() as *mut _,
            expected.as_ptr() as *mut _,
        )
    };
    if eq != FALSE {
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
    if process == 0 {
        return Err(io::Error::last_os_error());
    }

    let mut token: HANDLE = 0;
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
        return Err(io::Error::new(io::ErrorKind::Other, "TOKEN_USER probe failed"));
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
        return Err(io::Error::new(io::ErrorKind::Other, "TOKEN_USER.Sid is null"));
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
```

- [ ] **Step 3: Cross-compile check.**

```bash
cargo check --target x86_64-pc-windows-msvc -p skattr-core
```

Expected: success.

- [ ] **Step 4: Linux test pass.**

```bash
cargo test -p skattr-core --features test-harness
```

Expected: green.

- [ ] **Step 5: Commit.**

```bash
git add crates/core/src/daemon/ipc/server/windows.rs
git commit -m "feat(ipc): implement check_peer_sid + peer_sid_for"
```

---

## Task 10: Implement `Server::bind` on Windows

**Files:**
- Modify: `crates/core/src/daemon/ipc/server/windows.rs`

**Goal:** Generate a random pipe name; build a SECURITY_DESCRIPTOR granting `FILE_GENERIC_READ | FILE_GENERIC_WRITE` to `allowed` and nothing else; create the named pipe with that SD; atomic-write the pipe name to `discovery_path`.

- [ ] **Step 1: Add a unit test.**

```rust
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
    std::fs::write(&endpoint, "\\\\.\\pipe\\skattr-stale").unwrap();
    let server = Server::bind(&endpoint, current_sid()).unwrap();
    let written = std::fs::read_to_string(&endpoint).unwrap();
    assert!(!written.contains("stale"));
    drop(server);
}
```

- [ ] **Step 2: Add a SECURITY_DESCRIPTOR builder helper.** Inside `server/windows.rs`:

```rust
use std::ptr;

use windows_sys::Win32::Foundation::{LocalFree, GENERIC_READ, GENERIC_WRITE};
use windows_sys::Win32::Security::{
    AddAccessAllowedAce, InitializeAcl, InitializeSecurityDescriptor, IsValidSecurityDescriptor,
    SetSecurityDescriptorDacl, ACL, ACL_REVISION, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR,
    SECURITY_DESCRIPTOR_REVISION,
};

/// Holds a SECURITY_DESCRIPTOR + DACL + SECURITY_ATTRIBUTES alive together.
/// The descriptor's DACL points into `dacl_buf`; the SECURITY_ATTRIBUTES
/// points at `sd`. Lifetime invariants: this struct must outlive every
/// raw pointer it produces.
struct OwnerOnlySa {
    sd: Box<SECURITY_DESCRIPTOR>,
    dacl_buf: Vec<u8>,
    sa: SECURITY_ATTRIBUTES,
}

impl OwnerOnlySa {
    /// Build a SECURITY_ATTRIBUTES that grants
    /// FILE_GENERIC_READ | FILE_GENERIC_WRITE to `allowed_sid` and
    /// nothing else.
    fn new(allowed_sid: &[u8]) -> io::Result<Self> {
        // ACL with one allow-ACE: header (sizeof::<ACL_HEADER>=8) +
        // ACE header (4) + access mask (4) + sid_len.
        let dacl_size = 8 + 4 + 4 + allowed_sid.len();
        // Round up to DWORD boundary.
        let dacl_size = (dacl_size + 3) & !3usize;
        let mut dacl_buf = vec![0u8; dacl_size];

        // SAFETY: dacl_buf is sized per the formula above; we never write
        // past `dacl_size`. SD pointer comes from a Box we own.
        unsafe {
            let dacl = dacl_buf.as_mut_ptr() as *mut ACL;
            if InitializeAcl(dacl, dacl_size as u32, ACL_REVISION) == 0 {
                return Err(io::Error::last_os_error());
            }
            if AddAccessAllowedAce(
                dacl,
                ACL_REVISION,
                GENERIC_READ | GENERIC_WRITE,
                allowed_sid.as_ptr() as *mut _,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }

            let mut sd: Box<SECURITY_DESCRIPTOR> = Box::new(std::mem::zeroed());
            if InitializeSecurityDescriptor(
                &mut *sd as *mut _ as *mut _,
                SECURITY_DESCRIPTOR_REVISION,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            // Attach the DACL: present=true, dacl=dacl, defaulted=false.
            if SetSecurityDescriptorDacl(&mut *sd as *mut _ as *mut _, 1, dacl, 0) == 0 {
                return Err(io::Error::last_os_error());
            }
            // Sanity: the descriptor must validate.
            if IsValidSecurityDescriptor(&mut *sd as *mut _ as *mut _) == 0 {
                return Err(io::Error::last_os_error());
            }

            let sa = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: &mut *sd as *mut _ as *mut _,
                bInheritHandle: 0,
            };

            Ok(Self { sd, dacl_buf, sa })
        }
    }

    fn as_raw(&mut self) -> *mut SECURITY_ATTRIBUTES {
        // Re-point the SA to our SD on every call (the SD's address may
        // have shifted if `self` moved).
        self.sa.lpSecurityDescriptor = &mut *self.sd as *mut _ as *mut _;
        &mut self.sa
    }
}
```

- [ ] **Step 3: Implement `Server::bind`.**

```rust
use rand::RngCore;

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

impl Server {
    pub fn bind(discovery_path: &Path, allowed: PeerId) -> Result<Self> {
        if let Some(parent) = discovery_path.parent() {
            std::fs::create_dir_all(parent).map_err(crate::error::CoreError::Io)?;
        }
        // Best-effort cleanup of any stale discovery file (matches
        // Unix's `let _ = remove_file(path)` pattern).
        let _ = std::fs::remove_file(discovery_path);

        // 12 random bytes → 24 hex char suffix.
        let mut entropy = [0u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut entropy);
        let hex: String = entropy.iter().map(|b| format!("{b:02x}")).collect();
        let pipe_name = format!(r"\\.\pipe\skattr-{hex}");

        // Build the SECURITY_ATTRIBUTES owner-only DACL.
        let mut sa = OwnerOnlySa::new(&allowed)
            .map_err(|e| crate::error::CoreError::Io(io::Error::new(
                io::ErrorKind::Other,
                format!("OwnerOnlySa::new: {e}"),
            )))?;

        // Open the pipe. Tokio's `create_with_security_attributes_raw`
        // is the only stable hook for a custom SD.
        let listener = unsafe {
            ServerOptions::new()
                .first_pipe_instance(true)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(
                    &pipe_name,
                    sa.as_raw() as *mut _,
                )
        }
        .map_err(crate::error::CoreError::Io)?;

        // Atomic write of the pipe name to the discovery file.
        let tmp = discovery_path.with_extension("endpoint.tmp");
        std::fs::write(&tmp, format!("{pipe_name}\n"))
            .map_err(crate::error::CoreError::Io)?;
        std::fs::rename(&tmp, discovery_path)
            .map_err(crate::error::CoreError::Io)?;

        Ok(Self {
            listener,
            discovery_path: discovery_path.to_path_buf(),
            pipe_name,
            allowed,
        })
    }
}
```

A note on `OsStrExt::encode_wide`: the `create_with_security_attributes_raw` call takes the pipe name as `impl AsRef<OsStr>`. Tokio internally wides it. We pass `&pipe_name` (a `&String`) directly — `String: AsRef<OsStr>`. No wide-encoding gymnastics needed at our call site.

- [ ] **Step 4: Cross-compile check.**

```bash
cargo check --target x86_64-pc-windows-msvc -p skattr-core
```

Expected: success.

- [ ] **Step 5: Commit.** (Tests run on `windows-latest` after Task 18.)

```bash
git add crates/core/src/daemon/ipc/server/windows.rs
git commit -m "feat(ipc): implement Server::bind on Windows (DACL + discovery file)"
```

---

## Task 11: Implement `Server::accept_one` on Windows + post-accept SID check

**Files:**
- Modify: `crates/core/src/daemon/ipc/server/windows.rs`

**Goal:** `accept_one` waits for a client to connect, calls `peer_sid_for` to extract the connecting process's SID, validates against `self.allowed`, and returns the (now-connected) `NamedPipeServer`. Caller (`serve`) is responsible for spawning a follow-up `NamedPipeServer` instance to listen for the next connection — Windows pipe servers are single-shot.

- [ ] **Step 1: Add a sketch test** (will run on Windows CI after Task 18).

```rust
#[tokio::test]
async fn accept_rejects_mismatched_sid() {
    let tmp = tempfile::tempdir().unwrap();
    let endpoint = tmp.path().join("ipc.endpoint");
    // Pre-build a "wrong" expected SID: zeroed, same length as ours.
    let mut wrong = current_sid();
    wrong.iter_mut().for_each(|b| *b = 0);
    let server = Server::bind(&endpoint, wrong).unwrap();

    // Spawn the accept_one in a task; meanwhile try to connect from
    // this process (whose SID = current_sid != wrong).
    let pipe_name = server.pipe_name.clone();
    let accept = tokio::spawn(async move { server.accept_one().await });
    let _client = ClientOptions::new().open(&pipe_name).unwrap();
    let res = accept.await.unwrap();
    assert!(matches!(res, Err(IpcError::AuthDenied)), "got {res:?}");
}

#[tokio::test]
async fn accept_admits_matching_sid() {
    let tmp = tempfile::tempdir().unwrap();
    let endpoint = tmp.path().join("ipc.endpoint");
    let server = Server::bind(&endpoint, current_sid()).unwrap();
    let pipe_name = server.pipe_name.clone();
    let accept = tokio::spawn(async move { server.accept_one().await });
    let _client = ClientOptions::new().open(&pipe_name).unwrap();
    let res = accept.await.unwrap();
    assert!(res.is_ok(), "got {res:?}");
}
```

- [ ] **Step 2: Implement `accept_one`.**

```rust
use std::os::windows::io::AsRawHandle;

impl Server {
    pub async fn accept_one(&self) -> std::result::Result<NamedPipeServer, IpcError> {
        // Wait for a client to connect to this listener instance.
        self.listener
            .connect()
            .await
            .map_err(|e| IpcError::Internal(format!("connect: {e}")))?;

        // SAFETY: as_raw_handle() returns a valid pipe handle for the
        // lifetime of `self.listener`. peer_sid_for does not close it.
        let raw = self.listener.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
        let peer = unsafe { peer_sid_for(raw) }
            .map_err(|e| IpcError::Internal(format!("peer_sid_for: {e}")))?;

        check_peer_sid(&peer, &self.allowed).map_err(|_| IpcError::AuthDenied)?;

        // The listener's internal state has now transitioned to
        // "connected"; we hand it back as the per-conn stream. The
        // caller (`serve`) must spawn a fresh NamedPipeServer instance
        // for the next accept — see Step 3.
        //
        // tokio::net::windows::named_pipe::NamedPipeServer is consumed
        // here via std::mem::replace in serve(). We can't return
        // `&self.listener`; we must extract by value. The simplest
        // pattern is to return the `&Server` and let `serve` orchestrate.
        // BUT our trait signature returns the stream. So instead, we
        // have `serve` rebuild the listener after each accept (Step 3
        // changes `serve` to call a new `Server::next_listener()`).

        // For now, this method clones the handle isn't possible; we
        // wrap the listener in an Option<NamedPipeServer> field on
        // Server and `take()` it here, then `next_listener()` refills it.
        // See Step 4 for the field change.
        unreachable!("see Step 4 — listener field becomes Option<NamedPipeServer>")
    }
}
```

- [ ] **Step 3: Restructure: `Server.listener` → `Option<NamedPipeServer>`; add `Server::next_listener()`.**

The Windows named-pipe pattern: each `NamedPipeServer` instance accepts exactly one client. After `connect()` resolves, you must build a *new* `NamedPipeServer` for the same pipe name (the kernel routes the *next* `ClientOptions::open(name)` to the new instance). To preserve the `Server::accept_one() -> NamedPipeServer` signature, store the listener as `Option<NamedPipeServer>` and refill it after each accept.

```rust
pub struct Server {
    listener: std::sync::Mutex<Option<NamedPipeServer>>,
    discovery_path: PathBuf,
    pipe_name: String,
    allowed: PeerId,
    sa: std::sync::Mutex<OwnerOnlySa>,  // Persisted so next_listener can reuse the SD.
}

impl Server {
    fn next_listener(&self) -> io::Result<NamedPipeServer> {
        let mut sa_guard = self.sa.lock().unwrap();
        // SAFETY: pipe_name is stable; sa.as_raw points into self.
        let listener = unsafe {
            ServerOptions::new()
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(
                    &self.pipe_name,
                    sa_guard.as_raw() as *mut _,
                )
        }?;
        Ok(listener)
    }

    pub async fn accept_one(&self) -> std::result::Result<NamedPipeServer, IpcError> {
        let listener = {
            let mut guard = self.listener.lock().unwrap();
            guard.take().ok_or_else(|| IpcError::Internal(
                "accept_one called concurrently".into(),
            ))?
        };
        listener
            .connect()
            .await
            .map_err(|e| IpcError::Internal(format!("connect: {e}")))?;

        let raw = std::os::windows::io::AsRawHandle::as_raw_handle(&listener)
            as windows_sys::Win32::Foundation::HANDLE;
        let peer = unsafe { peer_sid_for(raw) }
            .map_err(|e| IpcError::Internal(format!("peer_sid_for: {e}")))?;
        if let Err(_) = check_peer_sid(&peer, &self.allowed) {
            // Refill before returning; otherwise the next accept_one
            // would see an empty Option.
            let new = self.next_listener()
                .map_err(|e| IpcError::Internal(format!("next_listener: {e}")))?;
            *self.listener.lock().unwrap() = Some(new);
            return Err(IpcError::AuthDenied);
        }

        // Refill the listener for the next accept.
        let new = self.next_listener()
            .map_err(|e| IpcError::Internal(format!("next_listener: {e}")))?;
        *self.listener.lock().unwrap() = Some(new);
        Ok(listener)
    }
}
```

Update `Server::bind` to populate `listener: std::sync::Mutex::new(Some(initial))` and `sa: std::sync::Mutex::new(sa)`. The pre-existing `Server::bind` body builds `sa` and `listener`; just stash both in their `Mutex` wrappers at the end.

- [ ] **Step 4: Cross-compile check.**

```bash
cargo check --target x86_64-pc-windows-msvc -p skattr-core
```

Expected: success.

- [ ] **Step 5: Commit.**

```bash
git add crates/core/src/daemon/ipc/server/windows.rs
git commit -m "feat(ipc): implement Server::accept_one on Windows with post-accept SID check"
```

---

## Task 12: Verify `serve()` works unchanged for Windows

**Files:**
- Modify (if needed): `crates/core/src/daemon/ipc/server/mod.rs`

**Goal:** `serve()` is platform-neutral but it calls `server.accept_one()` and gets back a stream type. On Unix that's `tokio::net::UnixStream`; on Windows that's `tokio::net::windows::named_pipe::NamedPipeServer` (post-connect; `NamedPipeServer` itself implements `AsyncRead + AsyncWrite`, so it satisfies the trait bounds on `handle_connection<S>`).

- [ ] **Step 1: Cross-compile check.**

```bash
cargo check --target x86_64-pc-windows-msvc -p skattr-core
```

Expected: success. If a type error surfaces in `serve()` (e.g. `accept_one`'s return type is now platform-specific and the `tokio::spawn` body doesn't accept it), the fix is in `serve()` — likely adding `S: AsyncRead + AsyncWrite + Unpin + Send + 'static` bounds on a generic helper. But `handle_connection<S>` already takes that bound, so the spawn closure should infer cleanly.

- [ ] **Step 2: Linux test pass.**

```bash
cargo test -p skattr-core --features test-harness
```

Expected: green.

- [ ] **Step 3: If no edits were needed, skip the commit.** Otherwise:

```bash
git add -u crates/core/src/daemon/ipc/server/
git commit -m "fix(ipc): adjust serve() type bounds for Windows accept type"
```

---

## Task 13: Implement `IpcClient::connect` on Windows

**Files:**
- Modify: `crates/core/src/daemon/ipc/client/windows.rs`

**Goal:** Read the discovery file at `path`; parse one line as the pipe name; call `ClientOptions::open(&pipe_name)`; on `ERROR_PIPE_BUSY`, call `WaitNamedPipeW` for up to 5 s and retry once; otherwise map errors to `DaemonNotRunning` / `Io`.

- [ ] **Step 1: Add tests** to `client/windows.rs`.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_missing_discovery_returns_daemon_not_running() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("no-such-endpoint");
        let err = IpcClient::connect(&missing).await.unwrap_err();
        assert!(matches!(err, IpcClientError::DaemonNotRunning), "got {err:?}");
    }

    #[tokio::test]
    async fn connect_pipe_named_in_discovery_but_no_server_returns_daemon_not_running() {
        let tmp = tempfile::tempdir().unwrap();
        let endpoint = tmp.path().join("ipc.endpoint");
        std::fs::write(&endpoint, "\\\\.\\pipe\\skattr-not-actually-bound\n").unwrap();
        let err = IpcClient::connect(&endpoint).await.unwrap_err();
        assert!(matches!(err, IpcClientError::DaemonNotRunning), "got {err:?}");
    }
}
```

- [ ] **Step 2: Implement `connect`.**

```rust
use std::time::Duration;

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY};
use windows_sys::Win32::System::Pipes::WaitNamedPipeW;

impl IpcClient<NamedPipeClient> {
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

        // Step 2: try to open. Retry once on ERROR_PIPE_BUSY (5s wait).
        for attempt in 0..2 {
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
                    // its pointer is valid for the call.
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
```

- [ ] **Step 3: Cross-compile check.**

```bash
cargo check --target x86_64-pc-windows-msvc -p skattr-core
```

Expected: success.

- [ ] **Step 4: Commit.**

```bash
git add crates/core/src/daemon/ipc/client/windows.rs
git commit -m "feat(ipc): implement IpcClient::connect on Windows (discovery + pipe open)"
```

---

## Task 14: Make CLI `resolve_socket_path` cfg-conditional

**Files:**
- Modify: `crates/cli/src/main.rs`

**Goal:** Replace today's single `resolve_socket_path` (Unix `XDG_RUNTIME_DIR`/`TMPDIR`/`/tmp` fallback) with a cfg-conditional version. Unix branch is unchanged; Windows branch uses `directories::ProjectDirs`.

- [ ] **Step 1: Locate today's `resolve_socket_path` (around line 182 of `crates/cli/src/main.rs`).**

```bash
grep -n "fn resolve_socket_path" crates/cli/src/main.rs
```

- [ ] **Step 2: Replace the function with the cfg-conditional pair.**

```rust
/// Resolve the IPC endpoint path with precedence flag > env > default.
///
/// On Unix the path is the AF_UNIX socket file; on Windows it is the
/// daemon's discovery file (containing the named-pipe name).
#[cfg(unix)]
fn resolve_socket_path(flag: Option<&std::path::Path>) -> PathBuf {
    if let Some(p) = flag {
        return p.to_path_buf();
    }
    if let Some(env) = std::env::var_os("SKATTR_SOCKET") {
        return PathBuf::from(env);
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("TMPDIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("skattr").join(skattr_core::daemon::ipc::ENDPOINT_FILENAME)
}

#[cfg(windows)]
fn resolve_socket_path(flag: Option<&std::path::Path>) -> PathBuf {
    if let Some(p) = flag {
        return p.to_path_buf();
    }
    if let Some(env) = std::env::var_os("SKATTR_SOCKET") {
        return PathBuf::from(env);
    }
    directories::ProjectDirs::from("net", "myggiz", "skattr")
        .map(|p| p.data_dir().join(skattr_core::daemon::ipc::ENDPOINT_FILENAME))
        .unwrap_or_else(|| PathBuf::from(skattr_core::daemon::ipc::ENDPOINT_FILENAME))
}
```

Note the Unix branch picks up `ENDPOINT_FILENAME` instead of the literal `"daemon.sock"` — backward-incompatible only for users who relied on the implicit `/run/user/UID/skattr/daemon.sock` path under the previous default. The CLI command-line `--socket` and `$SKATTR_SOCKET` overrides are unchanged. Document the rename in the CHANGELOG entry (Task 22).

- [ ] **Step 3: Update the `--socket` flag's clap docstring.**

Change today's:

```rust
    /// Path to the daemon's IPC socket. Overrides $SKATTR_SOCKET and
    /// the XDG default.
```

to:

```rust
    /// Path to the daemon's IPC endpoint. On Unix this is the AF_UNIX
    /// socket file; on Windows this is the daemon's discovery file
    /// (which contains the named-pipe name). Overrides $SKATTR_SOCKET
    /// and the platform default.
```

- [ ] **Step 4: Linux test pass.**

```bash
cargo test -p skattr-cli
```

Expected: green. (The cli's `resolve_socket_path` test, if any, may need updating to expect `ipc.sock` instead of `daemon.sock`. Check existing tests in `crates/cli/src/main.rs`.)

- [ ] **Step 5: Cross-compile check.**

```bash
cargo check --target x86_64-pc-windows-msvc -p skattr-cli
```

Expected: success.

- [ ] **Step 6: Commit.**

```bash
git add crates/cli/src/main.rs
git commit -m "feat(cli): cfg-conditional resolve_socket_path; ProjectDirs on Windows"
```

---

## Task 15: Gate `crates/mailbox` Unix-only paths for cross-compile

**Files:**
- Modify: `crates/mailbox/src/health.rs`
- Modify: `crates/mailbox/src/config.rs`

**Goal:** The mailbox crate's `health.rs` uses `UnixListener`/`UnixStream`/`set_mode` directly; `config.rs` defaults `health_socket` to `data_dir.join("health.sock")` and to `/var/lib/mailbox/health.sock`. The mailbox is **not shipped** on Windows (it's a Linux server), but the workspace must cross-compile cleanly so `cargo build --workspace` on the Windows CI runner doesn't fail.

Strategy: gate the health-socket module entirely behind `#[cfg(unix)]`. On Windows, the mailbox crate exposes a stub `health::bind` that returns `MailboxError::Unsupported`. Operators don't run the mailbox on Windows; the gate is purely for cross-compile cleanliness.

- [ ] **Step 1: Inspect `crates/mailbox/src/lib.rs` for the `pub mod health;` line.**

```bash
grep -n "pub mod health\|mod health" crates/mailbox/src/lib.rs
```

- [ ] **Step 2: Wrap the `health` module in `cfg(unix)`** (in `crates/mailbox/src/lib.rs`):

```rust
#[cfg(unix)]
pub mod health;

#[cfg(target_os = "windows")]
pub mod health {
    //! Health-check Unix socket is Linux/macOS-only. On Windows the
    //! mailbox server is not supported; this stub exists for
    //! cross-compile cleanliness.
    use crate::error::MailboxError;
    use std::path::Path;

    pub async fn bind(_path: &Path, _store: std::sync::Arc<crate::store::Store>) -> Result<(), MailboxError> {
        Err(MailboxError::Unsupported("health socket requires Unix".into()))
    }
}
```

If `MailboxError::Unsupported` doesn't exist, add it to `crates/mailbox/src/error.rs`:

```rust
#[error("not supported on this platform: {0}")]
Unsupported(String),
```

- [ ] **Step 3: Gate the `health_socket_default()` helper in `crates/mailbox/src/config.rs`** behind `#[cfg(unix)]` (or have the Windows branch return `PathBuf::from("health.sock")` in the data_dir — never used, but compiles).

Concrete edit: locate `pub fn health_socket(&self) -> PathBuf` (around line 77). If the current implementation calls `self.data_dir.join("health.sock")` regardless of platform, that's already cross-compile-clean — `PathBuf::join` works on Windows. The only Unix-specific bits are inside `health.rs`.

For the `unwrap_or_else(|| PathBuf::from("/var/lib/mailbox/health.sock"))` default at config.rs:229, that path is just a string; it works as a `PathBuf` on Windows even though no Windows code reads it. Leave as-is; only `health.rs` itself needed gating.

- [ ] **Step 4: Cross-compile check the whole workspace.**

```bash
cargo check --target x86_64-pc-windows-msvc --workspace --exclude skattr-ui
```

Expected: success.

If `crates/mailbox/src/arti.rs:91`'s `perms.set_mode(0o700)` causes a Windows compile error (it calls `std::os::unix::fs::PermissionsExt::set_mode`), gate it: wrap the `arti.rs` module — or just that helper — behind `#[cfg(unix)]`. The mailbox `bin` target's main loop already only runs on Linux (the systemd unit ships only there); on Windows the binary's entrypoint can early-return `Err(MailboxError::Unsupported)` if the cross-compile path takes execution there.

- [ ] **Step 5: Linux test pass.**

```bash
cargo test -p skattr-mailbox
```

Expected: green (Unix path unchanged).

- [ ] **Step 6: Commit.**

```bash
git add crates/mailbox/
git commit -m "build(mailbox): cfg(unix)-gate health socket for Windows cross-compile"
```

---

## Task 16: Move tests around per the spec's test partition

**Files:**
- Modify: `crates/core/src/daemon/ipc/server/mod.rs` (test mod consolidation)
- Modify: `crates/core/src/daemon/ipc/server/unix.rs` (Unix-only tests)
- Modify: `crates/core/src/daemon/ipc/client/mod.rs` (platform-neutral tests)

**Goal:** Tasks 1+2 already moved tests roughly into the right files; this task validates the partition matches the spec exactly:

- Platform-neutral tests in `server/mod.rs::tests`: 8 tests (`per_conn_*` × 3, `event_matches_filters_message_received_by_contact`, `event_filter_*` × 4 — `mailboxes`, `delivery`, `contact_matches_contact_card_received_for_same_peer`, `all_matches_new_events`).
- Plus 1 platform-neutral test driven by a stub executor: `subscribe_ack_replays_cached_tor_status`. Total 9 in `server/mod.rs::tests`.
- Unix-only tests in `server/unix.rs::tests`: 4 tests (`check_peer_uid_*` × 3, `bind_sets_socket_mode_0600_and_parent_0700`, `bind_unlinks_stale_socket_file`). Total 5.
- Platform-neutral tests in `client/mod.rs::tests`: 2 tests (`execute_roundtrip_over_duplex`, `subscribe_streams_events`).
- Unix-only tests in `client/unix.rs::tests`: 1 test (`connect_missing_socket_returns_daemon_not_running`).
- Windows-only tests in `server/windows.rs::tests` and `client/windows.rs::tests`: covered by Tasks 8, 9, 10, 11, 13.

Sanity check: 9 + 5 = 14 in `server/{mod,unix}.rs` (matching today's 14 in `server.rs`); 2 + 1 = 3 in `client/{mod,unix}.rs` (matching today's 3 in `client.rs`). Total 17 Unix-side tests preserved across the split.

- [ ] **Step 1: Audit.**

```bash
grep -cE "^[[:space:]]*#\[(tokio::)?test" \
  crates/core/src/daemon/ipc/server/mod.rs \
  crates/core/src/daemon/ipc/server/unix.rs \
  crates/core/src/daemon/ipc/client/mod.rs \
  crates/core/src/daemon/ipc/client/unix.rs
```

Expected counts: `server/mod.rs: 9`, `server/unix.rs: 5`, `client/mod.rs: 2`, `client/unix.rs: 1`. Adjust if mismatched.

- [ ] **Step 2: Rename the cross-platform `connect_missing_socket_returns_daemon_not_running` test.**

In `client/unix.rs::tests`, leave the test as `connect_missing_socket_returns_daemon_not_running` (Unix-specific naming is appropriate since it tests UDS file absence). In `client/windows.rs::tests` (added in Task 13) the equivalent is `connect_missing_discovery_returns_daemon_not_running`. Both must exist.

- [ ] **Step 3: Run all tests.**

```bash
cargo test -p skattr-core --features test-harness
```

Expected: all 17 IPC-related tests pass on Unix (9 + 5 + 2 + 1).

- [ ] **Step 4: Commit if any edits.**

```bash
git add -u crates/core/src/daemon/ipc/
git commit -m "test(ipc): finalize per-platform test partition"
```

---

## Task 17: Update `Daemon::run` callsite to use `current_uid()` / `current_sid()` correctly

**Files:**
- Modify: `crates/core/src/daemon/mod.rs` (or wherever `Server::bind` is called from)

**Goal:** The daemon-startup code must call the right `current_*` function per platform. Cleanest: re-export both under a single name in `ipc/mod.rs` and let `cfg` pick.

- [ ] **Step 1: Add `current_peer_id()` to `crates/core/src/daemon/ipc/mod.rs`.**

```rust
/// Return the daemon's own `PeerId`. Platform-conditional.
#[cfg(unix)]
pub fn current_peer_id() -> PeerId {
    server::current_uid()
}
#[cfg(target_os = "windows")]
pub fn current_peer_id() -> PeerId {
    server::current_sid()
}
```

- [ ] **Step 2: Update the daemon-startup call site.**

Locate (typically `crates/core/src/daemon/mod.rs::Daemon::run`):

```bash
grep -rn "current_uid\|Server::bind" crates/core/src/daemon/ --include='*.rs'
```

Change every:

```rust
let allowed = ipc::server::current_uid();
let server = ipc::server::Server::bind(&socket_path, allowed)?;
```

to:

```rust
let allowed = ipc::current_peer_id();
let server = ipc::server::Server::bind(&socket_path, allowed)?;
```

- [ ] **Step 3: Run tests on Linux + cross-compile to Windows.**

```bash
cargo test  -p skattr-core --features test-harness
cargo check --target x86_64-pc-windows-msvc -p skattr-core
```

Expected: green on both.

- [ ] **Step 4: Commit.**

```bash
git add crates/core/src/daemon/
git commit -m "feat(ipc): expose current_peer_id; daemon uses it at bind"
```

---

## Task 18: Add `windows-latest` to `ci.yml` (test + clippy matrices)

**Files:**
- Modify: `.github/workflows/ci.yml`

**Goal:** Windows CI gates the IPC port from day one. After this commit, every push must pass `cargo build` + `cargo test` on `windows-latest` and clippy on both Linux and Windows.

- [ ] **Step 1: Promote the `clippy` job to a matrix.**

Replace today's:

```yaml
  clippy:
    name: clippy
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy
      - uses: Swatinem/rust-cache@v2
      - run: cargo clippy --workspace --exclude skattr-ui --all-targets --all-features -- -D warnings
```

with:

```yaml
  clippy:
    name: clippy (${{ matrix.os }})
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, windows-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy
      - uses: Swatinem/rust-cache@v2
      - run: cargo clippy --workspace --exclude skattr-ui --all-targets --all-features -- -D warnings
```

- [ ] **Step 2: Add `windows-latest` to the `test` matrix.**

Replace today's:

```yaml
        # Windows omitted: the daemon IPC stack ...
        os: [ubuntu-latest, macos-latest]
```

with:

```yaml
        os: [ubuntu-latest, macos-latest, windows-latest]
```

(Remove the four-line omission comment.)

- [ ] **Step 3: Push the branch and watch CI.**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add windows-latest to test and clippy matrices"
git push -u origin phase-2h-windows-port
```

Watch the GitHub Actions run. Expected first-iteration failures on Windows:

1. **`cargo build` fails** — likely a remaining `unsafe` call site or `cfg` gate gap. Triage from the build log; iterate.
2. **`cargo test` fails on a specific Windows test** — likely a `Server::bind` issue (e.g., DACL formation, pipe-name encoding, discovery-file path). Triage from the test output.
3. **`cargo clippy` fails with new lints** — `cfg(windows)` code may produce warnings that `-D warnings` rejects. Fix by `#[allow(...)]` in narrow scopes or refactor.

Iterate via push-fix-push until Windows CI is green. Do **not** mark this task complete until both jobs pass on `windows-latest`.

- [ ] **Step 4: Commit any post-iteration fixes.** Each iteration is its own commit.

---

## Task 19: Add `windows-latest` to `release.yml` + Windows smoke step

**Files:**
- Modify: `.github/workflows/release.yml`

**Goal:** The release matrix produces a `.msi` artifact on `windows-latest` and runs the smoke gate against it.

- [ ] **Step 1: Add `windows-latest` to the build matrix.**

In the `build` job's `strategy.matrix.include` (or `os`) list, append a Windows entry. Mirror the Linux/macOS entry's structure. The Tauri action handles the `.msi` artifact production from the existing `tauri.conf.json`.

- [ ] **Step 2: Add a Windows smoke step.**

Conditional on `runs-on == 'windows-latest'`:

```yaml
- name: Smoke (Windows)
  if: runner.os == 'Windows'
  shell: pwsh
  run: |
    $msi = Get-ChildItem -Path "$env:GITHUB_WORKSPACE\target\release\bundle\msi\Skattr_*_x64_en-US.msi" | Select-Object -First 1
    if (-not $msi) { throw "MSI not found" }
    Start-Process msiexec.exe -Wait -ArgumentList @('/i', "$($msi.FullName)", '/qn')
    if ($LASTEXITCODE -ne 0) { throw "msiexec failed: $LASTEXITCODE" }
    & "C:\Program Files\Skattr\skattr-ui.exe" --smoke-test --data-dir "$env:RUNNER_TEMP\smoke" --timeout-secs 240
    if ($LASTEXITCODE -ne 0) { throw "smoke failed: $LASTEXITCODE" }
```

- [ ] **Step 3: Verify the `.msi` is included in `SHA256SUMS`.**

Today's `SHA256SUMS` step likely globs `bundle/{deb,appimage,dmg}/*` artifacts. Extend the glob to include `bundle/msi/*.msi`.

- [ ] **Step 4: Push and run a release dry-run.**

```bash
git add .github/workflows/release.yml
git commit -m "ci: add windows-latest to release matrix + msi smoke step"
git push
```

Trigger the existing `release-dry-run` workflow (per 2.G's "CI release dry-run" hook) on the branch to validate end-to-end without a real tag. Iterate until the dry-run produces a `.msi` and the smoke step exits 0.

---

## Task 20: Write `docs/install/windows.md`

**Files:**
- Create: `docs/install/windows.md`

**Goal:** Single-page guide covering download → verify → install → first-run + SmartScreen + URL handler test.

- [ ] **Step 1: Write the doc.**

```markdown
# Installing Skattr on Windows

> Windows 11 (x64) is the supported runtime target. Windows 10 may
> work but is not CI-tested.

## 1. Download

Grab the latest release artifacts from
<https://github.com/myggiz/skattr/releases>:

- `Skattr_<version>_x64_en-US.msi`
- `SHA256SUMS`
- `SHA256SUMS.minisig`
- `minisign.pub` (the maintainer's public verification key — pinned
  in the repo at `docs/install/minisign.pub`)

## 2. Verify

Two complementary checks. Either is sufficient; together they're
defence-in-depth.

**SHA256:**

```powershell
Get-FileHash -Algorithm SHA256 .\Skattr_*.msi
```

The output line's hash must match the corresponding line in
`SHA256SUMS`.

**Minisign signature** (recommended):

Download `minisign-win32` from
<https://jedisct1.github.io/minisign/> and put `minisign.exe`
on PATH. Then:

```powershell
minisign.exe -V -m SHA256SUMS -p minisign.pub
```

Expected: `Signature and comment signature verified` (or similar).

## 3. Install

Double-click the `.msi`. Microsoft Defender SmartScreen will gate
the unsigned installer with "Windows protected your PC":

1. Click **More info**.
2. Click **Run anyway**.
3. Walk the WiX installer prompts. Default install path is
   `C:\Program Files\Skattr\`.

The `.msi` registers the `skattr://` URL handler and adds a Start
menu entry. It does **not** add `skattr-ui.exe` to `PATH`.

> Code signing (Authenticode) is planned for v0.2 — Phase 5 in the
> roadmap. SmartScreen will reappear on every download until the
> bundle accumulates "reputation" on Microsoft's reputation system,
> which only signed bundles do.

## 4. First-run

Launch Skattr from the Start menu. The first-run wizard walks four
steps:

1. Welcome.
2. Set a passphrase (zxcvbn ≥ 3 required).
3. Type back your 24-word BIP39 seed.
4. Wait for Tor bootstrap (up to 240 s on first run; subsequent
   launches are faster as the consensus is cached).

Once Tor reaches Ready, you're at the contact list — empty until
you add your first contact via an invite link.

You can verify the install end-to-end without going through the
GUI:

```powershell
& "C:\Program Files\Skattr\skattr-ui.exe" --smoke-test `
  --data-dir "$env:TEMP\skattr-smoke" --timeout-secs 240
```

Expected exit code: 0. The smoke test creates a throwaway identity
in `%TEMP%\skattr-smoke`, boots the daemon, waits for Tor Ready,
then exits.

## 5. `skattr://` URL handler

Paste this URL into the Edge address bar to test:
`skattr://invite/v1#test`. Edge will prompt "Open Skattr?" — click
**Open**. The app should focus and open the "Add contact" dialog
(it will reject the test invite as malformed; that's expected).

If the prompt never appears, the URL handler registration didn't
take. Re-running the `.msi` install (Repair) typically fixes it.

## 6. Uninstall

Settings → Apps → Skattr → Uninstall.

User data under `%APPDATA%\myggiz\skattr` is **not** removed by
uninstall by design — re-installing preserves your identity. To
fully wipe, delete `%APPDATA%\myggiz\skattr` manually after
uninstall.

## Troubleshooting

- **SmartScreen reappears every download.** Expected for unsigned
  bundles; Phase 5 Authenticode signing will silence it.
- **Daemon won't start; UI hangs at "starting".** Check
  `%APPDATA%\myggiz\skattr\ipc.endpoint` exists. If yes, delete it
  and relaunch — a stale entry from a prior crash can't be reused
  cross-process.
- **"This app can't run on your PC".** You downloaded the x64
  `.msi` on an ARM64 Windows machine. ARM64 Windows isn't supported
  for v0.1.
- **First-run hangs at Tor bootstrap.** Some networks throttle Tor
  guards. Increase `--timeout-secs` or use a `tor` bridge (UI
  setting under "Network").
```

- [ ] **Step 2: Verify it renders cleanly in GitHub-flavoured Markdown** (paste into the GH preview).

- [ ] **Step 3: Commit.**

```bash
git add docs/install/windows.md
git commit -m "docs: Windows install guide (smartscreen, verify, first-run)"
```

---

## Task 21: Update `docs/install/README.md` + `docs/build/reproducible.md`

**Files:**
- Modify: `docs/install/README.md`
- Modify: `docs/build/reproducible.md`

- [ ] **Step 1: Add a Windows row to the platform table in `docs/install/README.md`.**

Locate the existing platform table; add:

```markdown
| Windows 11 (x64) | `Skattr_<v>_x64_en-US.msi` | [windows.md](windows.md) |
```

- [ ] **Step 2: Add a Windows section to `docs/build/reproducible.md`.**

Append:

```markdown
## Windows (`.msi`)

The WiX `.msi` bundle is built on `windows-latest` via the
`tauri-action` workflow. The toolchain is pinned via
`rust-toolchain.toml` (`version = "1.95.0"`), and Tauri at
`=2.11.0`. The bundle is **not byte-reproducible across runs** for
v0.1 — Tauri 2.11's WiX template embeds timestamps and a build
GUID. Phase 4 (byte-identical reproducible builds) addresses this;
v0.1 ships `.msi` for verifiable distribution via SHA256 + minisign
only.

To rebuild locally on a Windows host:

```powershell
git clone https://github.com/myggiz/skattr
cd skattr
cargo tauri build
```

Output: `target\release\bundle\msi\Skattr_<v>_x64_en-US.msi`. Hash
it with `Get-FileHash` to compare against the released
`SHA256SUMS`. The hashes will **not** match across hosts — only
within a single CI run on the same `windows-latest` runner image
will reproducibility hold.
```

- [ ] **Step 3: Commit.**

```bash
git add docs/install/README.md docs/build/reproducible.md
git commit -m "docs: Windows entries for install + reproducible-build guides"
```

---

## Task 22: Update `CLAUDE.md` and `CHANGELOG.md`

**Files:**
- Modify: `CLAUDE.md`
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Update the repository-state paragraph in `CLAUDE.md`.**

Locate the "## Repository state" header (around line 7). Append a paragraph after the Phase 2.G section:

```markdown
Phase 2.H (Windows port) merged at the head of `phase-2h-windows-port`.
`crates/core/src/daemon/ipc/{server,client}` now have per-platform
submodules: `unix.rs` (AF_UNIX, unchanged) and `windows.rs` (Tokio
Named Pipes + owner-SID DACL + post-accept SID equality check).
New cross-platform aliases in `core::daemon::ipc::mod.rs`:
`IpcStream` (UnixStream / NamedPipeClient), `PeerId` (u32 / Vec<u8>
SID), `ENDPOINT_FILENAME` (`ipc.sock` / `ipc.endpoint`). Discovery
file at `<data_dir>\ipc.endpoint` carries the random per-daemon
pipe name `\\.\pipe\skattr-<24-hex>`. CI's `windows-latest` matrix
entry is now non-optional: `cargo test --workspace --exclude
skattr-ui` and `cargo clippy --workspace --exclude skattr-ui
--all-targets --all-features` both run on `windows-latest`. The
`release.yml` matrix produces a `.msi` artifact via Tauri's WiX
template; the smoke step installs via `msiexec /qn` and runs
`skattr-ui --smoke-test`. New install doc at
`docs/install/windows.md`. Wire-format-NEUTRAL — no `Command` /
`CommandResult` / `Event` variant additions. Phase 2 is now fully
closed; v0.2 can drop the "Windows deferred" disclaimer.
```

Also update the **opening paragraph** of "Repository state": change
"Phase 2.H ... is the remaining Phase 2 sub-project" to "Phase 2.H
(Windows port) is complete (merged YYYY-MM-DD); Phase 2 is fully
closed."

Also remove the Windows-omitted disclaimer from the "## Commands"
section (the "CI runs fmt + clippy + test on `ubuntu-latest` and
`macos-latest` ..." paragraph). Replace with:

```markdown
CI runs fmt + clippy + test on `ubuntu-latest`, `macos-latest`, and
`windows-latest`, plus a dedicated `ui` job on `ubuntu-latest` for
the Tauri 2 + SvelteKit crate. macOS x86_64 is still deferred —
`macos-latest` is Apple Silicon only.
```

- [ ] **Step 2: Add a `CHANGELOG.md` entry.**

If `CHANGELOG.md` doesn't exist yet, create it with a Keep-a-Changelog header. Otherwise append a new section above the most recent entry:

```markdown
## [Unreleased] - Phase 2.H (Windows port)

### Added
- Windows support for the daemon IPC layer via Tokio Named Pipes.
- Per-platform submodules under `core::daemon::ipc::server` and
  `core::daemon::ipc::client`.
- `IpcStream`, `PeerId`, and `ENDPOINT_FILENAME` cross-platform
  aliases in `core::daemon::ipc::mod.rs`.
- Owner-SID-only DACL on the Windows Named Pipe + post-accept SID
  equality check (mirrors Unix's 0600 mode + peer_cred pattern).
- `windows-sys = "0.59"` dependency for raw Win32 SID and
  SECURITY_DESCRIPTOR FFI.
- `windows-latest` runner in both `ci.yml` (test + clippy) and
  `release.yml` (build + smoke).
- `.msi` bundle production via Tauri WiX template; `msiexec /qn`
  smoke install in CI.
- `docs/install/windows.md` with download → verify → install →
  first-run + SmartScreen walkthrough.

### Changed
- `Server::bind`'s `allowed_uid: u32` → `allowed: PeerId`.
- CLI `--socket` flag and `$SKATTR_SOCKET` env var docs note that
  on Windows the path points to the daemon's discovery file.
- CLI's default IPC endpoint filename changes from `daemon.sock`
  to the platform-specific `ENDPOINT_FILENAME` (`ipc.sock` on Unix,
  `ipc.endpoint` on Windows). Users with `--socket=/run/user/1000/skattr/daemon.sock`
  hard-coded must update.
- `crates/mailbox/src/health.rs` is `cfg(unix)`-gated; the mailbox
  is not shipped on Windows.

### Deferred to Phase 5
- Authenticode code-signing (and macOS Developer ID + notarisation).
- Tauri auto-updater enablement.
- macOS Intel matrix entry.
- Per-platform `cargo fmt` matrix.
```

- [ ] **Step 3: Commit.**

```bash
git add CLAUDE.md CHANGELOG.md
git commit -m "docs: Phase 2.H CHANGELOG + CLAUDE.md status update"
```

---

## Final verification before merge

- [ ] **Step 1: Run the full local check suite.**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --exclude skattr-ui --all-targets --all-features -- -D warnings
cargo test  --workspace --exclude skattr-ui --features test-harness
cargo check --target x86_64-pc-windows-msvc --workspace --exclude skattr-ui
```

Expected: all four green.

- [ ] **Step 2: Confirm the `wire_format_append_only` snapshot test passes with zero diff.**

```bash
cargo test -p skattr-core wire_format_append_only
```

Expected: pass; no `EXPECTED_*.cbor` snapshot updates required (locked decision 11).

- [ ] **Step 3: Confirm CI on `windows-latest` is fully green** for both `ci.yml` (test + clippy) and `release.yml` (build + smoke).

- [ ] **Step 4: Open the PR.**

```bash
gh pr create --title "Phase 2.H — Windows port (Named Pipes + .msi)" --body "$(cat <<'EOF'
## Summary

Ports `core::daemon::ipc::{server,client}` to Windows Named Pipes
alongside the existing AF_UNIX implementation. Windows-latest is now
in `ci.yml` (test + clippy) and `release.yml` (build + smoke + .msi).
Wire-format-NEUTRAL.

Spec: `docs/superpowers/specs/2026-05-05-phase-2h-windows-port-design.md`.
Plan: `docs/superpowers/plans/2026-05-05-phase-2h-windows-port.md`.

## Test plan

- [ ] CI green on `ubuntu-latest`
- [ ] CI green on `macos-latest`
- [ ] CI green on `windows-latest` (both test and clippy jobs)
- [ ] release.yml dry-run produces `.msi` and smoke exits 0
- [ ] Wire-format snapshot test unchanged (zero diff)
- [ ] One non-technical tester completes download → verify → install
      → first-run on a fresh Windows 11 VM

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Self-review checklist (run after writing the plan; not part of execution)

- [x] **Spec coverage:** every locked decision in the spec maps to a task or step.
  - LD 1 (per-data-dir pipe + discovery file) → Tasks 7, 10
  - LD 2 (defense-in-depth peer auth) → Tasks 8, 9, 10, 11
  - LD 3 (PeerId alias) → Tasks 3, 4
  - LD 4 (per-platform submodules) → Tasks 1, 2
  - LD 5 (IpcStream alias) → Tasks 3, 5
  - LD 6 (ENDPOINT_FILENAME const) → Tasks 3, 5
  - LD 7 (connect-time discovery on Windows) → Task 13
  - LD 8 (Unix-style cleanup posture) → Task 10 (Drop already in scaffold)
  - LD 9 (--socket flag + SKATTR_SOCKET) → Task 14
  - LD 10 (CI matrix everywhere) → Tasks 18, 19, 15
  - LD 11 (wire-format-NEUTRAL) → final-verification step 2
- [x] **Placeholder scan:** every "PASTE verbatim" reference cites the exact line range in the source file. No bare TBDs. The `todo!()` bodies in Task 7 are intentional scaffolds replaced by Tasks 8–13.
- [x] **Type consistency:** `PeerId` is the alias name everywhere; `IpcStream` likewise. The `Server::bind` signature `(path: &Path, allowed: PeerId) -> Result<Self>` is identical across `unix.rs` and `windows.rs`. The new `current_peer_id()` helper shows up in Task 17 and the daemon-startup site is the only call site.
- [x] **Scope:** 22 tasks; all bundled into one PR. No spec requirement is left to a follow-up.
