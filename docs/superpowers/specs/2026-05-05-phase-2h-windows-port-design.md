# Phase 2.H — Windows port (design)

**Status:** approved, pending user review.
**Date:** 2026-05-05.
**Predecessor:** Phase 2.G (packaging & distribution) merged 2026-05-05.
**Umbrella:** `docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md`
§"2.G — Packaging & distribution".
**Carve-out source:** `docs/superpowers/specs/2026-05-04-phase-2g-packaging-design.md`
§"Carve-out: Windows (new Phase 2.H)".

## Scope

Phase 2.H ports the daemon IPC stack to Windows by adding a parallel
Named-Pipes implementation alongside the existing AF_UNIX one,
extends the `release.yml` matrix to ship a `.msi` bundle, and adds
`windows-latest` to `ci.yml` so the Windows path is build- and
test-gated from day one. Linux + macOS code paths are unchanged on
disk and at runtime.

2.H closes the umbrella's Phase 2 exit criterion that Phase 2.G
amended: "Each platform installer runs first-run wizard on a fresh
VM" returns to its original "Linux + macOS + Windows" form once 2.H
merges. v0.2 cannot be tagged before 2.H lands.

## What 2.H is NOT

- Not a wire-format change. Any addition to `Command`,
  `CommandResult`, or `Event` is a spec violation.
- Not a re-architecture of `core::daemon::ipc`. The Linux + macOS
  AF_UNIX path stays exactly as it is; Windows gets a parallel
  Named-Pipes path under `cfg(target_os = "windows")`.
- Not a code-signing or notarisation effort. The unsigned `.msi`
  ships with SmartScreen "More info → Run anyway" docs, matching
  the unsigned `.deb`/`.AppImage`/`.dmg` posture from 2.G.
  Authenticode + Developer ID land together in Phase 5.
- Not a Tauri-version bump. `tauri = "=2.11.0"` stays. If
  Windows-specific Tauri 2.11 issues surface, the fix is local to
  the IPC layer or the bundle config — not a workspace-wide bump.
- Not a Windows Service mode. 2.H ships only the user-mode binary;
  Service mode is Phase 3+ if there's demand.

## Locked decisions (this spec)

1. **Per-data-dir pipe name written to a discovery file.** On
   Windows, the daemon picks `\\.\pipe\skattr-<24-hex-chars>` at
   start (12 random bytes from `OsRng`) and writes the pipe name
   to `<data_dir>\ipc.endpoint` (one-line UTF-8 text, atomic write
   via `<data_dir>\ipc.endpoint.tmp` + `std::fs::rename`). The
   client reads the discovery file to find the pipe.
2. **Defense-in-depth peer auth.** The pipe's DACL grants connect
   rights to the daemon's user SID only — the kernel rejects
   unauthorised `connect` calls before they hit user space. After
   each accept the server *also* calls
   `GetNamedPipeClientProcessId` → `OpenProcessToken` →
   `GetTokenInformation(TokenUser)` → `EqualSid` to verify the
   connecting process belongs to the same user. Mirrors the
   Unix `0600` mode + `peer_cred()` belt-and-braces pattern.
3. **`cfg`-conditional `PeerId` alias.**
   ```rust
   #[cfg(unix)]    pub type PeerId = u32;
   #[cfg(windows)] pub type PeerId = Vec<u8>;  // raw SID bytes
   ```
   `Server::bind(path, allowed: PeerId)` replaces today's
   `allowed_uid: u32`. On Unix the change is a rename; on Windows
   `Vec<u8>` matches `windows-sys`'s native SID buffer shape.
4. **Per-platform submodules under `server/` and `client/`.**
   - `crates/core/src/daemon/ipc/server/mod.rs` — `CommandExecutor`,
     `handle_connection<S>`, `serve()`, `event_matches()`,
     all platform-neutral; re-exports `Server` from the active
     platform child.
   - `crates/core/src/daemon/ipc/server/unix.rs` — `Server`,
     `bind`, `accept_one`, `Drop`, `current_uid`. Compiled only
     under `#[cfg(unix)]`.
   - `crates/core/src/daemon/ipc/server/windows.rs` — `Server`,
     `bind`, `accept_one`, `Drop`, `current_sid`. Compiled only
     under `#[cfg(windows)]`.
   - Mirrored as `client/mod.rs` + `client/unix.rs` +
     `client/windows.rs`.
5. **`IpcStream` alias in `crates/core/src/daemon/ipc/mod.rs`.**
   ```rust
   #[cfg(unix)]    pub type IpcStream = tokio::net::UnixStream;
   #[cfg(windows)] pub type IpcStream =
       tokio::net::windows::named_pipe::NamedPipeClient;
   ```
   The single concrete `IpcClient<tokio::net::UnixStream>` in
   `crates/cli/src/main.rs::connect_or_exit` (line 200 today)
   becomes `IpcClient<IpcStream>`. Every other call site uses
   `IpcClient::connect(&path)` and is unchanged.
6. **Hidden `ENDPOINT_FILENAME` const.**
   ```rust
   #[cfg(unix)]    pub const ENDPOINT_FILENAME: &str = "ipc.sock";
   #[cfg(windows)] pub const ENDPOINT_FILENAME: &str = "ipc.endpoint";
   ```
   Replaces the hard-coded `"ipc.sock"` literal in
   `crates/ui/src/daemon.rs:70`. `Daemon::run`'s implicit fallback
   when `config.ipc_socket = None` joins `data_dir` with this
   const. Tests that pass `ready.ipc_socket` through unchanged
   work on both platforms.
7. **Connect-time discovery in `IpcClient::connect`.**
   On Unix, `IpcClient::connect(&path)` calls `UnixStream::connect`
   as today. On Windows, it reads the file at `path`, parses one
   line as the pipe name, and calls `NamedPipeClient::connect` on
   that name. Same `IpcClientError::DaemonNotRunning` mapping for
   missing file, missing pipe, or `ERROR_FILE_NOT_FOUND` /
   `ERROR_PIPE_BUSY`. `connect` blocks for up to 5 s on
   `ERROR_PIPE_BUSY` (Windows uses `WaitNamedPipeW` for this) before
   returning `DaemonNotRunning`.
8. **Match Unix's stale-state cleanup posture.** `Server::bind` on
   Windows does `let _ = std::fs::remove_file(discovery_path);`
   before writing the new discovery file. No try-connect-first
   probe. We don't enforce daemon single-instance at the IPC
   layer on Unix today (the UI's `tauri-plugin-single-instance`
   covers user-facing double-launch); Windows matches.
   `Server::Drop` on Windows unlinks the discovery file
   best-effort.
9. **`--socket` flag and `SKATTR_SOCKET` keep their names.**
   On Windows both accept the discovery file path (not a literal
   `\\.\pipe\…` name). The clap docstring on `--socket` gets a
   one-line "On Windows, this is the path to the daemon's IPC
   discovery file" addendum. The CLI's `resolve_socket_path`
   fallback chain becomes platform-conditional:
   - Unix: `flag > $SKATTR_SOCKET > $XDG_RUNTIME_DIR/skattr/daemon.sock > $TMPDIR/skattr/daemon.sock > /tmp/skattr/daemon.sock`.
   - Windows: `flag > %SKATTR_SOCKET% > %APPDATA%\myggiz\skattr\ipc.endpoint`,
     where the `%APPDATA%` path is resolved via
     `directories::ProjectDirs::data_dir()` (matches where the UI
     daemon writes).
10. **CI matrix from day one.** `windows-latest` is added to **both**
    `ci.yml` (clippy + test) **and** `release.yml` (build + smoke).
    Per Q5: matrix runs everything everywhere — clippy stays in the
    Windows job. `cargo fmt --check` is the only step that stays
    Linux-only (platform-neutral). `crates/mailbox` must compile on
    Windows (no functional requirement; the workspace builds
    together) — its `health.sock` UDS is already `cfg(unix)`-able and
    the systemd unit ships Linux-only.
11. **No wire-format changes.** 2.H is wire-format-NEUTRAL by
    design.

## Architecture

### Module split

Today: two flat files, `crates/core/src/daemon/ipc/server.rs` and
`crates/core/src/daemon/ipc/client.rs`. Both are Unix-bound.

After 2.H:

```
crates/core/src/daemon/ipc/
├── mod.rs           # re-exports + IpcStream + ENDPOINT_FILENAME + PeerId
├── codec.rs         # unchanged (platform-neutral)
├── wire.rs          # unchanged (platform-neutral)
├── server/
│   ├── mod.rs       # CommandExecutor, handle_connection<S>, serve, event_matches
│   ├── unix.rs      # #[cfg(unix)]    — Server, bind, accept_one, Drop, current_uid, check_peer_uid
│   └── windows.rs   # #[cfg(windows)] — Server, bind, accept_one, Drop, current_sid, check_peer_sid
└── client/
    ├── mod.rs       # IpcClient<S> generic body, IpcClientError, from_stream
    ├── unix.rs      # #[cfg(unix)]    — IpcClient::connect(&Path) using UnixStream
    └── windows.rs   # #[cfg(windows)] — IpcClient::connect(&Path) reads discovery file → NamedPipeClient
```

`server/mod.rs` and `client/mod.rs` re-export the platform `Server`
and `IpcClient::connect` impl behind `cfg`-gated `pub use` lines, so
external users (`crate::daemon::ipc::Server`,
`crate::daemon::ipc::IpcClient`) don't see the split.

The platform-neutral parts that already exist in
`server.rs`/`client.rs` move into the respective `mod.rs` files
verbatim. The `#[cfg(test)]` mod inside today's `server.rs` is
mostly platform-neutral (`tokio::io::duplex`-driven); it moves to
`server/mod.rs`. The two `bind` tests that touch `0600`/`0700`
modes move to `server/unix.rs`. A new parallel test in
`server/windows.rs` checks DACL+SID semantics against
`tokio::net::windows::named_pipe::ClientOptions::open(...)` from a
test client.

### Discovery file protocol

**Format.** UTF-8 text, single line, no trailing whitespace except
an optional `\n`. Content is the pipe name, e.g.
`\\.\pipe\skattr-9f3c1ab8e427d6052f0a8c91`.

**Daemon write.** `Server::bind` (Windows) computes the pipe name
from `OsRng` (12 bytes → 24 hex chars), opens the pipe via
`NamedPipeServer::Builder` with the locked-down DACL, then writes
the pipe name atomically to the discovery file:

```rust
let tmp = discovery_path.with_extension("endpoint.tmp");
std::fs::write(&tmp, format!("{pipe_name}\n"))?;
std::fs::rename(&tmp, discovery_path)?;
```

`std::fs::rename` on Windows is atomic for same-volume same-name
replace.

**Daemon cleanup.** `Server::Drop` (Windows) does
`let _ = std::fs::remove_file(&self.discovery_path);`. Best-effort,
matches Unix's `Drop`. The kernel reaps the pipe object when all
handles close; a stale discovery file at next start is overwritten
unconditionally per locked decision 8.

**Client read.** `IpcClient::connect(&path)` (Windows):

```rust
let pipe_name = std::fs::read_to_string(path)
    .map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => IpcClientError::DaemonNotRunning,
        _ => IpcClientError::Io(e),
    })?
    .trim()
    .to_string();
match ClientOptions::new().open(&pipe_name) {
    Ok(stream) => Ok(Self::from_stream(stream)),
    Err(e) if e.kind() == io::ErrorKind::NotFound => {
        Err(IpcClientError::DaemonNotRunning)
    }
    Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
        // 5s wait via WaitNamedPipeW; if it times out, treat as DNR.
        match wait_named_pipe(&pipe_name, Duration::from_secs(5)) {
            Ok(()) => /* retry open once */ ...,
            Err(_) => Err(IpcClientError::DaemonNotRunning),
        }
    }
    Err(e) => Err(IpcClientError::Io(e)),
}
```

`wait_named_pipe` wraps `WaitNamedPipeW` from `windows-sys`.

### Peer auth on Windows

**At pipe creation.** `Server::bind` (Windows) builds a
`SECURITY_DESCRIPTOR` containing a single ACE that grants
`FILE_GENERIC_READ | FILE_GENERIC_WRITE` to the daemon's own user
SID, then passes the wrapping `SECURITY_ATTRIBUTES` to
`CreateNamedPipeW` via Tokio's
`NamedPipeServer::Builder::create_with_security_attributes_raw`
(unsafe but the only stable hook). `reject_remote_clients(true)`
keeps the pipe local-only (Tokio's default; verify in Cargo.lock at
implementation time).

**At each accept.** After `NamedPipeServer::connect` resolves, the
server obtains the connecting process's user SID via:

1. `GetNamedPipeClientProcessId(handle, &mut pid)`
2. `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)`
3. `OpenProcessToken(process, TOKEN_QUERY, &mut token)`
4. `GetTokenInformation(token, TokenUser, ...)` → `TOKEN_USER` →
   `Sid` pointer
5. `EqualSid(client_sid, allowed_sid)` against the `PeerId` stored
   on `Server`

Failures map to `IpcError::AuthDenied`. The connection is closed
and the accept loop continues.

**`current_sid()`.** `Server::bind`'s caller (today
`Daemon::run` via `current_uid()`) needs the daemon's own SID. The
Windows equivalent calls
`GetCurrentProcessToken()` → `GetTokenInformation(TokenUser, ...)`
to extract the SID, copies the bytes into a `Vec<u8>`, returns it
as `PeerId`.

### Endpoint resolution flow

**Daemon side.** `Daemon::Config.ipc_socket: Option<PathBuf>` is
unchanged. Resolution order in `Daemon::run`:

1. Caller-supplied `config.ipc_socket` (UI / tests).
2. Fallback: `data_dir.join(skattr_core::daemon::ipc::ENDPOINT_FILENAME)`.

The UI's `crates/ui/src/daemon.rs:70` line changes from
`config.ipc_socket = Some(data_dir.join("ipc.sock"));` to
`config.ipc_socket = Some(data_dir.join(skattr_core::daemon::ipc::ENDPOINT_FILENAME));`.
On Unix that's still `data_dir/ipc.sock`; on Windows it becomes
`data_dir/ipc.endpoint`.

**CLI side.** `resolve_socket_path` in `crates/cli/src/main.rs`
becomes `cfg`-conditional. Unix branch is exactly today's logic.
Windows branch:

```rust
#[cfg(windows)]
fn resolve_socket_path(flag: Option<&Path>) -> PathBuf {
    if let Some(p) = flag { return p.to_path_buf(); }
    if let Some(env) = std::env::var_os("SKATTR_SOCKET") {
        return PathBuf::from(env);
    }
    directories::ProjectDirs::from("net", "myggiz", "skattr")
        .map(|p| p.data_dir().join(skattr_core::daemon::ipc::ENDPOINT_FILENAME))
        .unwrap_or_else(|| PathBuf::from("ipc.endpoint"))
}
```

The `ProjectDirs` qualifier triple `("net", "myggiz", "skattr")`
matches Tauri's bundle identifier (`net.myggiz.skattr`) and
therefore matches where the UI's `app_data_dir()` resolves to on
Windows — so `skattr` (CLI) and `skattr-ui` running against a
fresh data_dir on the same machine find each other without explicit
`--socket`.

## New Cargo dependencies

`crates/core/Cargo.toml`:

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
tokio = { workspace = true, features = ["net"] }   # already enabled; named_pipe ships with the net feature on Windows
```

No new high-level Windows wrapper crates (`windows`,
`winapi-util`). `windows-sys` is the raw FFI binding and the
project standard for Tokio's own Windows bits.

No changes to other crates' Cargo.toml. Tauri 2.11 already pulls
its own Windows dependencies.

## Data flow walkthroughs

### Daemon startup (Windows)

1. `Daemon::run(data_dir, passphrase, ready_tx, shutdown_fut)`
   resolves `ipc_socket` to `<data_dir>\ipc.endpoint` per locked
   decision 6.
2. Resolves `current_sid()` → `PeerId` (Vec<u8>).
3. Calls `Server::bind(&endpoint_path, allowed_sid)`:
   a. `OsRng` → 12 bytes → 24 hex → pipe name
      `\\.\pipe\skattr-<hex>`.
   b. Builds `SECURITY_DESCRIPTOR` with one ACE for
      `allowed_sid`.
   c. Calls
      `NamedPipeServer::Builder::create_with_security_attributes_raw`
      to bind the pipe.
   d. Atomic-writes `endpoint_path` with the pipe name.
4. Sends `DaemonReady { ipc_socket: endpoint_path, ... }` on
   `ready_tx`.
5. `serve(server, executor, events_tx, shutdown_fut)` loop accepts
   per locked decision 2.

### Client connect (Windows)

1. Caller resolves `path` (CLI: `resolve_socket_path`; UI:
   `ready.ipc_socket`; tests: `ready.ipc_socket`).
2. `IpcClient::connect(&path)`:
   a. Reads the discovery file → pipe name.
   b. `NamedPipeClient::connect(&pipe_name)` with the
      `WaitNamedPipeW` retry on `ERROR_PIPE_BUSY`.
   c. Returns `IpcClient<NamedPipeClient>` (which is
      `IpcClient<IpcStream>` per locked decision 5).

### Per-accept on Windows

1. `Server::accept_one` awaits `NamedPipeServer::connect`.
2. After connect, calls the SID extraction chain (locked decision 2).
3. `EqualSid` check: pass → return stream; fail → close + log +
   `IpcError::AuthDenied`.
4. Caller (`serve`) spawns `handle_connection(stream, executor,
   events_tx)`. `handle_connection` is platform-neutral; it
   already takes `S: AsyncRead + AsyncWrite + Unpin + Send +
   'static`.
5. Server immediately rebinds a *new* `NamedPipeServer` instance
   to accept the next client (Windows pipe servers consume their
   listener on connect; standard Tokio pattern).

## Error handling

**`IpcError`** (wire-level, in `wire.rs`) gains no new variants.
Existing `IpcError::AuthDenied`, `IpcError::Internal`, and
`IpcError::Codec` cover the new failure modes:

- DACL violation at OS layer never reaches user space → no IPC
  error.
- Post-accept SID mismatch → `IpcError::AuthDenied` (same as Unix
  uid mismatch).
- Failure to read discovery file or open pipe →
  `IpcClientError::DaemonNotRunning` (same as Unix missing socket).
- All other Win32 errors → `IpcClientError::Io` (existing
  variant; wraps `io::Error` with the OS error code).

`Server::bind` errors map through the existing `CoreError::Io`
variant. No new `CoreError` subkind.

## Testing strategy

### Unit tests

**Move to `server/mod.rs`** (platform-neutral, currently in
`server.rs`):
- `per_conn_execute_returns_ok_and_bye`
- `per_conn_subscribe_forwards_events_then_execute_still_works`
- `per_conn_unknown_command_returns_err_but_keeps_connection`
- `event_matches_*` (5 tests)
- `subscribe_ack_replays_cached_tor_status`

**Move to `server/unix.rs`** (Unix-specific, `#[cfg(unix)]`):
- `check_peer_uid_*` (3 tests)
- `bind_sets_socket_mode_0600_and_parent_0700`
- `bind_unlinks_stale_socket_file`

**New in `server/windows.rs`** (`#[cfg(windows)]`):
- `bind_writes_discovery_file_with_pipe_name` — bind, read
  endpoint file, assert it starts with `\\.\pipe\skattr-` and
  has 24 hex chars.
- `bind_unlinks_stale_discovery_file` — pre-create stale file;
  bind succeeds; new file content differs.
- `drop_unlinks_discovery_file` — bind then drop; assert file
  gone.
- `accept_rejects_mismatched_sid` — bind with a fake "allowed"
  SID (zeroes), connect from the test process, expect
  `IpcError::AuthDenied`.
- `check_peer_sid_*` (parallel to Unix's three uid tests but
  against the SID equality helper).

**New in `client/windows.rs`** (`#[cfg(windows)]`):
- `connect_missing_discovery_returns_daemon_not_running`
- `connect_invalid_pipe_name_in_discovery_returns_io`
- `connect_pipe_busy_then_available_succeeds` (uses
  `tokio::time::pause` to simulate the wait).

**Existing in `client.rs`** (move to `client/mod.rs`,
platform-neutral, `tokio::io::duplex`-driven):
- `execute_roundtrip_over_duplex`
- `connect_missing_socket_returns_daemon_not_running` (renamed to
  `connect_missing_endpoint_returns_daemon_not_running`; on Unix
  it tests a missing UDS file, on Windows a missing discovery
  file — same `DaemonNotRunning` mapping)
- `subscribe_streams_events`

### Integration tests

The following existing integration tests under `crates/tests/`
must pass on `windows-latest`:

- `cli_ipc_roundtrip` — single-daemon round trip.
- `cli_two_daemons` — paired daemons in separate data_dirs.
- `welcome_propagation` — paired daemons + MLS Welcome.
- `ui_first_run` — UI in-process daemon boot.

These tests pass `ready.ipc_socket: PathBuf` through unchanged. The
`PathBuf` carries the discovery file path on Windows; the test code
needs no platform conditional.

The `#[ignore]`-gated real-Tor tests (`cli_real_tor`,
`mailbox_real_tor`, `delivery_real_tor`, `ui_send_roundtrip` paths
that talk to real Tor) **are not gated to run on Windows CI** —
same posture as today on Linux/macOS where they're also
`#[ignore]`-gated. Manual `--ignored` runs on Windows are out of
scope for 2.H but should work.

### Smoke test on Windows

`release.yml`'s Windows smoke step:

```yaml
- name: Smoke (Windows)
  shell: pwsh
  run: |
    Start-Process msiexec.exe -Wait -ArgumentList @(
      '/i', "$env:GITHUB_WORKSPACE\target\release\bundle\msi\Skattr_*_x64_en-US.msi",
      '/qn'
    )
    & "C:\Program Files\Skattr\skattr-ui.exe" --smoke-test `
      --data-dir "$env:RUNNER_TEMP\smoke" --timeout-secs 240
```

The `Start-Process ... -Wait` form is required because
`msiexec.exe` returns immediately by default (it dispatches to a
service). 240s Tor-bootstrap budget matches Linux/macOS.

## CI changes

### `.github/workflows/ci.yml`

**`test` job:** add `windows-latest` to the matrix; remove the
"Windows omitted" comment block (lines 40–44 today). Body
(`cargo build` + `cargo test`) runs unchanged on Windows.

**`clippy` job:** promoted to a matrix over
`[ubuntu-latest, windows-latest]` per locked decision 10. Body
(`cargo clippy --workspace --exclude skattr-ui --all-targets
--all-features -- -D warnings`) runs unchanged on both. The Windows
entry exercises the `#[cfg(windows)]` modules' lints from day one.
The `ui` job's clippy stays Linux-only (Tauri Windows clippy runs
in `release.yml`'s build matrix).

**`fmt` job:** stays Linux-only.

**`ui` job:** stays Linux-only. Tauri's Windows build runs in
`release.yml` only (matches macOS today).

### `.github/workflows/release.yml`

Add `windows-latest` to the build matrix. New steps for the
Windows entry:

1. `tauri-apps/tauri-action` produces the `.msi` from the existing
   `tauri.conf.json` WiX defaults.
2. The smoke-install step above runs against the produced bundle.
3. The signing step (minisign over `SHA256SUMS`) is unchanged —
   it runs on `ubuntu-latest` and ingests the Windows artifact
   like any other.

Tauri's WiX template installs to `C:\Program Files\Skattr\` by
default and registers the `skattr://` URL handler via
`tauri-plugin-deep-link`'s Windows schema. No `tauri.conf.json`
changes needed for v0.1.

## Documentation deliverables

### New: `docs/install/windows.md`

Outline:

1. **Download.** Link to the GitHub Release; show the artefacts
   table (`Skattr_<ver>_x64_en-US.msi`, `SHA256SUMS`,
   `SHA256SUMS.minisig`).
2. **Verify.** Two recipes:
   a. `Get-FileHash -Algorithm SHA256 .\Skattr_*.msi` and grep
      against `SHA256SUMS`.
   b. `minisign-win32.exe -V -m SHA256SUMS -p minisign.pub` from
      the maintainer-shipped public key (link to
      `docs/install/minisign.pub`).
3. **Install.** Double-click the `.msi`. Microsoft Defender
   SmartScreen will gate the unsigned bundle. Walkthrough:
   "Windows protected your PC" → "More info" → "Run anyway". One
   screenshot per step.
4. **First-run.** Same wizard as Linux/macOS. Confirm Tor reaches
   Ready (240s timeout). The smoke test command from CI is
   reproducible locally:
   `& "C:\Program Files\Skattr\skattr-ui.exe" --smoke-test
   --data-dir "$env:TEMP\skattr-smoke" --timeout-secs 240`.
5. **`skattr://` URL handler.** Manual test: paste
   `skattr://invite/v1#...` into the Edge address bar; confirm
   the OS dialog "Open Skattr?" appears.
6. **Uninstall.** Settings → Apps → Skattr → Uninstall.
   Note that user data under
   `%APPDATA%\myggiz\skattr` is not removed by uninstall by
   design (matches macOS `.app` uninstall, where `~/Library/Application Support/skattr/`
   stays).
7. **Troubleshooting.** Two known issues:
   - SmartScreen reappears on every download until the bundle
     accumulates "reputation" — Phase 5 Authenticode signing fixes
     this.
   - If the daemon crashes mid-conversation, the discovery file at
     `%APPDATA%\myggiz\skattr\ipc.endpoint` may be stale; relaunch
     the UI to overwrite.

### Update: `docs/install/README.md`

Add a Windows row to the platform table; mirror the Linux/macOS
verify recipe.

### Update: `docs/build/reproducible.md`

Add a Windows section noting that the WiX `.msi` build is **not**
byte-reproducible across runs without further work (Phase 4 scope).
Document the toolchain pin (`rust-toolchain.toml` already locks
the rustc version; Tauri at `=2.11.0`).

### Update: `CLAUDE.md`

Phase 2.H gets its paragraph in the repository-state header; the
"Windows is not in the matrix today" disclaimer is removed.

## Risks and mitigations

| Risk | Mitigation |
|------|-----------|
| `tokio::net::windows::named_pipe` API shape changes between Tokio minor versions | Pin Tokio in `Cargo.toml` to the exact minor in use at the start of 2.H; verify the `create_with_security_attributes_raw` signature; pin in the implementation plan's first task |
| `windows-sys` SID extraction has too many `unsafe` blocks for a security-sensitive code path | Encapsulate the SID extraction in a single `pub(crate) fn current_sid() -> Result<Vec<u8>, io::Error>` and a single `pub(crate) fn peer_sid_for(handle: HANDLE) -> Result<Vec<u8>, io::Error>` with extensive comments and a unit test that round-trips against `current_sid()` |
| Tauri 2.11 `.msi` produced on `windows-latest` runner doesn't include `skattr-ui.exe` at the documented `C:\Program Files\Skattr\` path | Validate via the smoke step itself; if the install path differs (e.g., per-user install at `%LOCALAPPDATA%\Programs\Skattr`), update the smoke command and `windows.md` together |
| `WaitNamedPipeW` doesn't handle very-fast-open daemon (race on first connect attempt) | The 5s wait is generous; if the pipe doesn't exist yet, `NamedPipeClient::connect` returns `NotFound` (not `BUSY`), which maps to `DaemonNotRunning`. UI re-attempts as today |
| Concurrent test daemons step on each other's pipe names | Pipe name includes 12 random bytes from `OsRng` per daemon; collision probability is negligible. Plus tests use unique data_dirs so the discovery files are isolated |
| Windows `cargo test` on the runner takes >30 min | Use `Swatinem/rust-cache@v2` (already in ci.yml); run only `--workspace --exclude skattr-ui` matching today's posture; `skattr-ui` has its own `ui` job |
| `directories::ProjectDirs` returns `None` on a CI runner without a real user profile | Fall back to `PathBuf::from("ipc.endpoint")` (a relative path) per the resolve_socket_path code; the test harness always passes an explicit `--data-dir` so the fallback is never hit in CI |
| `crates/mailbox` fails to compile on Windows because of an unguarded UDS path | Audit `crates/mailbox/src/` for `UnixListener` / `UnixStream` / `0o600` / `peer_cred`; confirm the existing `cfg(unix)` gates around `health.sock` cover everything; if not, add gates as part of the implementation plan |
| `serial_test` socket-path locking interaction with Windows pipe naming | `serial_test` is process-wide test serialisation; Windows pipe names are randomised per daemon so no name collision — the existing tests should work without changing the lock pattern |
| SmartScreen blocks the smoke step at install time | `msiexec /qn` runs in quiet mode and bypasses SmartScreen for MSI install (SmartScreen gates the *download* and *interactive launch*, not silent MSI install). Verify on first CI run; if blocked, add `Set-MpPreference -DisableRealtimeMonitoring $false` to the runner setup |

## Out of scope for 2.H

- **Code signing + notarisation** (Phase 5). Includes Authenticode
  for the `.msi` and Developer ID for the `.dmg`.
- **Auto-update mechanism** (Phase 5; Tauri updater stays
  disabled).
- **Microsoft Store distribution.** Sandboxed network restrictions
  don't fit Tor.
- **Windows Service mode** (Phase 3+).
- **`rand_chacha` 0.3 → 0.10** mailbox fuzz dev-dep migration
  (separate PR; not blocking v0.1.0).
- **Wire-format BREAKING changes** — any rename or removal of
  `Command` / `CommandResult` / `Event` variants requires a
  separate spec.
- **macOS Intel matrix entry** (deferred from 2.G; Phase 5 follow-up).
- **`#[ignore]`-gated real-Tor tests gated to run on Windows CI**
  — same posture as Linux/macOS today.
- **Phase 3+ items** (avatars, reactions, replies, attachments,
  multi-member groups).
- **Phase 4+ items** (cover traffic, panic-wipe, duress mode,
  byte-identical reproducible builds).

## Exit criteria

- [ ] `cargo build --workspace --target x86_64-pc-windows-msvc`
  compiles clean from `ubuntu-latest` (cross-compile sanity check).
- [ ] `cargo test --workspace --exclude skattr-ui --features
  test-harness` passes on `windows-latest` for all non-`#[ignore]`
  tests.
- [ ] `cargo clippy --workspace --exclude skattr-ui --all-targets
  --all-features -- -D warnings` is green on **both**
  `ubuntu-latest` **and** `windows-latest`.
- [ ] `cargo tauri build` produces a working `.msi` on
  `windows-latest`.
- [ ] `skattr-ui --smoke-test --data-dir <tmp>` exits 0 within
  240s on `windows-latest`.
- [ ] `.github/workflows/ci.yml` test matrix includes
  `windows-latest` with no `continue-on-error`.
- [ ] `.github/workflows/release.yml` build matrix includes
  `windows-latest` and produces a `.msi` artefact in the GitHub
  Release.
- [ ] `docs/install/windows.md` covers download → verify → install
  → first-run with the SmartScreen walkthrough and screenshots.
- [ ] `docs/install/README.md` platform table includes Windows.
- [ ] `wire_format_append_only` snapshot test unchanged (zero-byte
  diff).
- [ ] CHANGELOG entry; CLAUDE.md status update marking 2.H
  complete and Phase 2 fully closed.
- [ ] One non-technical tester completes download → verify →
  install → first-run on a fresh Windows 11 VM without operator
  hand-holding.

## Maintainer reminder (carries over from 2.G)

Before tagging `v0.1.0`, the maintainer must complete the minisign
keypair generation per
`docs/install/README-MAINTAINER-MINISIGN.md`:

1. Generate keypair offline.
2. Set GitHub Actions secrets `MINISIGN_SECRET_KEY` +
   `MINISIGN_PASSWORD`.
3. Replace the placeholder `docs/install/minisign.pub`.
4. Delete `docs/install/README-MAINTAINER-MINISIGN.md`.

Phase 2.H can land before or after this — but `v0.1.0` cannot
ship without it.
