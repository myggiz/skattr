Phase 2.G (packaging & distribution) merged to master at
`5df4fe5` (`Merge pull request #2 from myggiz/phase-2g-packaging`)
on 2026-05-05. Linux + macOS bundles, CI release flow at
`.github/workflows/release.yml` (matrix-build → smoke gate →
SHA256SUMS + minisign → GitHub Release on `v*` tags), Tauri pinned
to `=2.11.0` + rustc to `1.95.0`, the `core::daemon::smoke` module
+ `skattr-ui --smoke-test` argv branch, install + build docs at
`docs/install/{README,linux,macos}.md` + `docs/build/{reproducible,
flatpak}.md`, Flatpak manifest at
`packaging/flatpak/net.myggiz.skattr.yml`. Wire-format-NEUTRAL by
design. JS toolchain refresh sweep (vite 5 → 8, vitest 2 → 4,
typescript 5.5 → 6, svelte 5.0 → 5.55, jsdom 25 → 29, etc.) +
Cargo dev-dep patch bumps + GitHub Actions to current majors all
folded into the same merge.

Phase 2.H (Windows support) is the next workstream — the
**remaining Phase 2 sub-project before the umbrella exit
criteria are fully met**. The carve-out is documented in
`docs/superpowers/specs/2026-05-04-phase-2g-packaging-design.md`
§"Carve-out: Windows (new Phase 2.H)" and re-stated in
CLAUDE.md's repository-state paragraph. 2.H lands before any
`v0.2` tag.

**Why Phase 2.H exists at all** — the daemon IPC stack is
hard-coded to AF_UNIX:

- `core::daemon::ipc::{server,client}` use
  `tokio::net::UnixListener` and `UnixStream`.
- Peer authentication uses Linux/macOS-specific `peer_cred` SCM
  credentials.
- Socket paths get `mode 0600` permission bits + are anchored
  under `XDG_RUNTIME_DIR`.
- CLI/UI hold `IpcClient<UnixStream>` as a concrete type.

`cargo build -p skattr-ui --target x86_64-pc-windows-msvc` does
not compile today. Phase 2.G's CI omits Windows from the build
matrix; this is documented in `.github/workflows/ci.yml` lines
40–44 and in the 2.G spec.

**Phase 2.H deliverables (from the carve-out):**

1. **Port `core::daemon::ipc::{server,client}` to Windows Named
   Pipes** — Tokio supports `NamedPipeServer`/`NamedPipeClient`
   natively. Replace the AF_UNIX paths with platform-conditional
   implementations behind `cfg(target_family = "unix")` /
   `cfg(target_os = "windows")`. Linux + macOS paths stay
   unchanged.
2. **Replace `peer_cred` UDS auth with DACL-based ACL on the
   named pipe.** Only the user's SID can connect. The Windows
   security descriptor is set at pipe creation time via the
   `windows-sys` `CreateNamedPipeW` flow (Tokio's
   `NamedPipeServer::Builder` exposes a security-attributes
   hook).
3. **Audit every UDS-specific code path** — socket-path
   resolution (`XDG_RUNTIME_DIR` vs Windows `%TEMP%` or
   `\\.\pipe\skattr-<user-sid>`), `peer_cred` callsites,
   `mode 0600` operations, every `IpcClient<UnixStream>`
   concrete-type usage in CLI/UI, the `serial_test`
   socket-path test harness in `crates/cli/src/main.rs`'s test
   block.
4. **Add `windows-latest` to the CI matrix** — both
   `.github/workflows/ci.yml` (test job) and
   `.github/workflows/release.yml` (build + smoke jobs). Tauri's
   default WiX template produces the `.msi`; the 2.G
   `--smoke-test` flag works unchanged once the IPC layer
   compiles. The smoke command on Windows will be
   `C:\Program Files\Skattr\skattr-ui.exe --smoke-test
   --data-dir %RUNNER_TEMP%\smoke --timeout-secs 240`.
5. **Documentation** — new `docs/install/windows.md` covering
   `.msi` install, **SmartScreen "More info → Run anyway"** for
   the unsigned bundle, and the `skattr://` URL handler test on
   Windows. Update `docs/install/README.md`'s platform list and
   the 2.G spec's "deferred" footer.

**Topics worth pinning down in the 2.H brainstorm:**

- **Named-pipe path convention.** `\\.\pipe\skattr-<sid>` puts
  the user's SID in the path; `\\.\pipe\skattr.<random>` plus
  the path written to a per-user discovery file is closer to
  the XDG_RUNTIME_DIR pattern. Pick one; commit it as a locked
  decision.
- **DACL construction.** Tokio's
  `NamedPipeServer::Builder::pipe_mode + reject_remote_clients`
  gets us localhost-only; the security descriptor for "owner
  only" needs `CreateNamedPipeW` with a custom
  `SECURITY_ATTRIBUTES`. Using the `windows-sys` raw API vs.
  pulling in a higher-level crate (`windows`,
  `winapi-util`)? Recommendation: `windows-sys` for the
  primitive, no new high-level crate.
- **`IpcClient<UnixStream>` concrete-type purge.** Today many
  callers hold `IpcClient<UnixStream>` directly. The cleanest
  path is to introduce a platform-conditional `type IpcStream
  = ...` alias in `core::daemon::ipc` and rewrite every
  `IpcClient<UnixStream>` to `IpcClient<IpcStream>`. Verify
  with a grep before/after.
- **Test harness.** `crates/tests/src/cli_ipc_roundtrip.rs`
  and friends spawn paired daemons over UDS sockets. The same
  tests need to work on Windows; the helper abstraction lives
  in `crates/tests/src/lib.rs`. Decide: cfg-guarded paths in
  every test, or one common helper that returns an
  abstract pipe handle?
- **`current_uid()` equivalent on Windows.** The IPC server's
  `allowed_uid` check uses Linux/macOS uid. On Windows the
  equivalent is the user SID. Define a platform-conditional
  type (`type PeerId = u32` on Unix, `Vec<u8>` for SID on
  Windows; or just `String` if we want one type).
- **`socket-path` semantics in CLI.** The `--socket` flag and
  `SKATTR_SOCKET` env var accept a filesystem path today. On
  Windows: a pipe name (string), not a path. UX:
  `--socket=\\.\pipe\skattr` works literally? Or rename to
  `--ipc-endpoint`? Recommendation: keep the existing flag
  name; the docstring documents the platform-conditional
  meaning.
- **AppDirs / data directory.** `directories::ProjectDirs`
  already returns the right per-platform path
  (`%APPDATA%\myggiz\skattr` on Windows). Verify by inspection.
- **Task 12 minisign on Windows.** The signing path runs on
  ubuntu-latest in 2.G; no change needed for 2.H. The
  `minisign -V` verification command in
  `docs/install/README.md` works on Windows via the official
  `minisign-win32` binary.
- **Tauri WiX template defaults.** Phase 2.G locked decision 1
  said "Tauri's default WiX template suffices for v0.1; revisit
  if installer UX is awful." 2.H is the moment to validate.
- **Smoke test on Windows.** The CI smoke step on Windows
  installs the `.msi` via `msiexec /i ...`. Verify:
  - That `msiexec /i ...` adds `skattr-ui.exe` to PATH (or that
    the smoke step references the `Program Files` path
    directly).
  - That `--smoke-test` argv parsing on Windows handles the
    same flag layout (CMD.exe vs PowerShell quoting).
- **Service mode (deferred).** Long-term Windows users may
  want the daemon to run as a Windows Service. That's a Phase
  3+ feature; 2.H ships only the user-mode binary.

**Locked decisions from Phase 2 umbrella + 2.G that 2.H
inherits (do not relitigate):**

1. Tauri 2 + SvelteKit, transport-agnostic JS adapter.
2. Daemon as single source of truth (UI never opens SQLite
   directly).
3. JIT IPC evolution with wire-format contracts (additive only).
4. `tauri = "=2.11.0"`, rustc pinned to `1.95.0` in
   `rust-toolchain.toml`.
5. Bundle metadata at `net.myggiz.skattr` (matches `ProjectDirs`).
6. CI release flow on `v*` tags via `.github/workflows/release.yml`.
7. Wire-format-NEUTRAL — no `Command`/`CommandResult`/`Event`
   variant additions in 2.H.
8. Tauri updater plugin disabled (Phase 5 enables).
9. macOS Intel + Flatpak Flathub deferred (separate workstreams).
10. Phase 2.G's three platforms (Linux .deb + AppImage,
    macOS .dmg) ship unsigned with minisign on `SHA256SUMS`;
    Windows .msi gets the same treatment in 2.H. Code signing +
    notarisation = Phase 5.

**What 2.H is NOT:**

- It is **not** a wire-format change. Any IPC variant addition
  is a spec violation.
- It is **not** a re-architecture of `core::daemon::ipc`.
  Linux + macOS keep their AF_UNIX path; Windows gets a
  parallel Named-Pipes path under `cfg(target_os = "windows")`.
- It is **not** code-signing or notarisation work. SmartScreen
  + Gatekeeper bypass docs are the v0.1 posture.
- It is **not** the `rand_chacha` 0.3 → 0.10 dev-dep migration
  on the mailbox fuzz harness — that's a separate focused PR
  tracked as a Phase 2.G follow-up.
- It is **not** a Tauri-version bump. `=2.11.0` stays; if
  Windows-specific Tauri 2.11 issues surface, the fix is local
  to the IPC layer, not a workspace-wide bump.

## Context

- `docs/superpowers/specs/2026-05-04-phase-2g-packaging-design.md` §
  "Carve-out: Windows (new Phase 2.H)" — the authoritative scope
  sketch. Read first.
- `docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md`
  — Phase 2 umbrella; locked decisions 1–10 carry forward.
- `crates/core/src/daemon/ipc/server.rs` and
  `crates/core/src/daemon/ipc/client.rs` — the AF_UNIX
  implementations to port.
- `crates/core/src/daemon/ipc/mod.rs` — module re-exports;
  the `cfg`-conditional `type IpcStream` should land here.
- `crates/cli/src/main.rs` — `IpcClient<UnixStream>` concrete
  type usages + the `socket` flag/env handling.
- `crates/ui/src/main.rs` and `crates/ui/src/ipc_bridge.rs`
  (or wherever IPC is held) — Tauri-side IPC client usage.
- `crates/tests/src/lib.rs` + every `cli_*` and `mailbox_*`
  integration test — UDS-specific paths to audit.
- `.github/workflows/{ci,release}.yml` — matrices to extend
  with `windows-latest`.
- `docs/install/README.md` — platform list to update.
- CLAUDE.md locked decisions remain binding. `crates/core` is
  GPLv3.

## After brainstorming

- `superpowers:writing-plans` to author the implementation plan.
- `superpowers:using-git-worktrees` to branch off master onto
  `phase-2h-windows-port`.
- `superpowers:test-driven-development` +
  `superpowers:subagent-driven-development` to execute. CI
  iteration on `windows-latest` will be the slow loop —
  expect 2–3 round trips on the first compile.
- `superpowers:verification-before-completion` before merge.

## Out of scope for 2.H

- **Code signing + notarisation** (Phase 5; both Authenticode
  for Windows and Developer ID for macOS land together).
- **Auto-update mechanism** (Phase 5).
- **Microsoft Store distribution** — sandboxed network
  restrictions don't fit Tor.
- **Windows Service mode** (Phase 3+).
- **`rand_chacha` 0.3 → 0.10** mailbox fuzz dev-dep migration
  (separate PR; not blocking v0.1.0).
- **Wire-format BREAKING changes** — any rename or removal of
  `Command`/`CommandResult`/`Event` variants requires a
  separate spec.
- **Phase 3+ items** (avatars, reactions, replies,
  attachments, multi-member groups).
- **Phase 4+ items** (cover traffic, panic-wipe, duress mode,
  byte-identical reproducible builds).

## Maintainer reminder (carries over from 2.G)

Before tagging `v0.1.0`, the maintainer must complete the
minisign keypair generation per
`docs/install/README-MAINTAINER-MINISIGN.md`:

1. Generate keypair offline.
2. Set GitHub Actions secrets `MINISIGN_SECRET_KEY` +
   `MINISIGN_PASSWORD`.
3. Replace the placeholder `docs/install/minisign.pub`.
4. Delete `docs/install/README-MAINTAINER-MINISIGN.md`.

Phase 2.H can land before or after this — but `v0.1.0` cannot
ship without it.
