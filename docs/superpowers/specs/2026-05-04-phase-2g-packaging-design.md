# Phase 2.G — Packaging & distribution (design)

**Status:** approved, pending user review.
**Date:** 2026-05-04.
**Predecessor:** Phase 2.F (settings & history) merged 2026-05-04.
**Umbrella:** `docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md`
§"2.G — Packaging & distribution".

## Scope

Phase 2.G ships installers for Linux and macOS, a CI release flow that
produces signed-checksum bundles on every `v*` tag, and the
groundwork (pinned toolchains, smoke test, reproducible-build recipe)
that Phase 4's byte-identical reproducibility claim will need.

**Windows is carved out to a new sub-phase 2.H** (see "Carve-out:
Windows" below). The umbrella's locked decision 6 lists Windows in the
release matrix; Phase 2.G amends that to Linux + macOS for the
v0.1.0 tag because the daemon IPC stack is hard-coded to AF_UNIX and
porting it to Named Pipes is a sub-phase of its own — not 2.G polish.

## Carve-out: Windows (new Phase 2.H)

`core::daemon::ipc::{server,client}` uses `tokio::net::UnixListener`,
`peer_cred`, and `mode 0600` bits; CLI/UI hold
`IpcClient<UnixStream>` concretely. `cargo build -p skattr-ui
--target x86_64-pc-windows-msvc` does not compile today. CI's
`ci.yml` already documents the omission (lines 40–44).

**Phase 2.H will deliver:**

1. Named Pipes + DACL-based peer auth on Windows; AF_UNIX path
   unchanged on Linux/macOS. Platform-conditional behind
   `cfg(target_family = "unix")` / `cfg(target_os = "windows")`.
2. Audit of every UDS-specific code path: socket resolution
   (`XDG_RUNTIME_DIR` vs Windows equivalent), `peer_cred` callsites,
   mode-bit operations, `IpcClient<UnixStream>` concrete types,
   `serial_test` socket-path test.
3. `windows-latest` added to `ci.yml` test job and `release.yml`
   build + smoke jobs. Tauri's WiX template produces the `.msi`;
   the 2.G `--smoke-test` flag works unchanged.
4. SmartScreen UX documentation at `docs/install/windows.md`.

**Phase 2 exit-criterion amendment:** the umbrella's "Each platform
installer runs first-run wizard on a fresh VM" becomes "Linux +
macOS installers run first-run wizard; Windows deferred to 2.H".
2.H lands before any "v0.2" tag.

## Locked decisions (this spec)

1. **Smoke flag lives on `skattr-ui --smoke-test`.** It's the bundled
   binary; the CLI is not in the `.deb`/`.dmg`. Branch happens before
   `tauri::Builder::default()`, so no webview is opened.
2. **Smoke logic lives in `skattr_core::daemon::smoke`.** Reusable
   from a `skattr daemon --smoke-test` developer escape hatch and
   unit-testable from `cargo test -p skattr-core`.
3. **Tauri pinned to an exact patch version.** `tauri = "=2.11.0"`
   in `crates/ui/Cargo.toml`; `@tauri-apps/api` matched in
   `package.json`. The current `version = "2"` is too loose for a
   recipe-based reproducibility claim.
4. **Toolchain pinned to an exact stable.** `rust-toolchain.toml`
   gains an explicit `version = "x.y.z"` line (currently only
   `channel = "stable"`). The exact version is resolved at 2.G start
   from the maintainer's `rustc --version`; the implementation plan's
   first task commits it.
5. **Linux ships `.deb` + AppImage + Flatpak (build-from-source).**
   Flathub publication deferred per umbrella scope.
6. **macOS ships ARM64-only for v0.1.** `macos-latest` is Apple
   Silicon; an x86_64 macOS bundle would need an extra `macos-13`
   matrix entry. Track as Phase 2.H or Phase 5 follow-up.
7. **`skattr://` URL scheme is registered as a packaging deliverable.**
   Linux `.desktop` `MimeType=x-scheme-handler/skattr;`; macOS
   `Info.plist` `CFBundleURLSchemes`. Turns invite paste into invite
   click. The handler dispatches to the existing `AddContact` flow
   in the SvelteKit shell — no new wire surface.
8. **No wire-format changes.** 2.G is wire-format-NEUTRAL by design.
9. **Tauri updater explicitly disabled** in `tauri.conf.json` so
   Phase 5's enable is a one-line diff with a clean handoff.

## Architecture

### Smoke test (`skattr_core::daemon::smoke`)

New module exporting:

```rust
pub struct SmokeConfig {
    pub data_dir: PathBuf,
    pub tor_ready_timeout: Duration,   // default 240s
    pub seed_bytes: [u8; 32],          // default = OsRng
}

pub enum SmokeError {
    DataDirNotEmpty,                   // refuse to clobber real state
    VaultCreate(VaultError),
    DaemonStart(CoreError),
    TorTimeout { waited: Duration },
    Other(CoreError),
}

pub async fn run_smoke(cfg: SmokeConfig) -> Result<SmokeReport, SmokeError>;
```

Flow:

1. Require `data_dir` to be non-existent or empty — refuse to run
   smoke over real user state. Returns `SmokeError::DataDirNotEmpty`
   if either an `identity.vault` or any non-hidden file is present.
2. Generate throwaway passphrase (random 32 bytes hex), throwaway
   seed (from `cfg.seed_bytes` or `OsRng`).
3. Initialise vault at `data_dir/identity.vault` using existing
   `Vault::create`.
4. Run `Daemon::run_with_sink` with a shutdown trigger that fires on
   the first `TorStatusChanged(Ready)` event. Time out at
   `tor_ready_timeout`.
5. On success: emit `SmokeReport { onion: String, duration: Duration,
   schema_version: u32 }` for the CI step summary.

Argv branch in `crates/ui/src/main.rs::main()`:

```rust
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--smoke-test") {
        return run_smoke_and_exit(args);
    }
    // existing Tauri::Builder code...
}
```

`run_smoke_and_exit` parses `--data-dir <path>` and
`--timeout-secs <N>` from argv, builds a `SmokeConfig`, runs
`tokio::runtime::Runtime::new()?.block_on(run_smoke(cfg))`, prints
the report (or error), exits 0/1.

CLI escape hatch: `crates/cli/src/main.rs` adds a `--smoke-test`
flag on the existing `Daemon` subcommand, sharing the same
`run_smoke` path.

### CI release workflow (`.github/workflows/release.yml`)

Triggered on `push.tags: ['v*']`. Three job groups:

**`build` (matrix `os: [ubuntu-latest, macos-latest]`):**
- Checkout, install Rust + Node 20 + pnpm via corepack.
- Linux only: install Tauri 2 system deps (same set as `ci.yml`'s
  `ui` job).
- `cargo test -p skattr-core --features test-harness` to refresh
  ts-rs bindings.
- `pnpm --dir crates/ui/src-svelte install --frozen-lockfile`.
- `cargo tauri build` (the Tauri CLI is invoked via the workspace).
- Upload bundles to a workflow-scoped artefact named
  `bundles-${{ matrix.os }}`.

**`smoke` (matrix mirrors `build`, `needs: build`):**
- Download the matching `bundles-${{ matrix.os }}` artefact.
- Linux: `dpkg -i skattr_*.deb` (in a clean Ubuntu container layer
  via `docker run` if dpkg is denied; otherwise the runner's apt
  permissions cover it). For AppImage: `chmod +x` and run directly.
- macOS: `hdiutil attach Skattr_*.dmg`, copy `.app` to
  `/tmp/Skattr.app`, treat that as the install root.
- Run `<binary> --smoke-test --data-dir $RUNNER_TEMP/smoke
  --timeout-secs 240`. Exit code 0 = pass.
- On failure: upload `$RUNNER_TEMP/smoke/**` as artefact
  `smoke-failure-${{ matrix.os }}` for triage.
- On success: GitHub job summary line "smoke OK on
  ${{ matrix.os }} in ${duration}".

**`release` (`needs: smoke`):**
- Download all `bundles-*` artefacts to a single directory.
- `sha256sum *.deb *.AppImage *.dmg > SHA256SUMS`.
- `minisign -Sm SHA256SUMS -s <(echo $MINISIGN_SECRET_KEY | base64
  -d)` (with `MINISIGN_PASSWORD` piped to the prompt). Resulting
  `SHA256SUMS.minisig`.
- `gh release create $TAG --notes-file CHANGELOG-extract.md
  bundles/* SHA256SUMS SHA256SUMS.minisig`.
- Release notes: extracted from `CHANGELOG.md` between the current
  tag and the previous tag (a small `awk` script).

**Why three job groups, not one:** smoke must run on a clean
artefact, not on whatever a `cargo tauri build` left in the runner's
working directory. Decoupling guarantees the smoke tests the
*bundle*, not the build tree.

### Bundle metadata (`crates/ui/tauri.conf.json` diffs)

Add:

```json
{
  "bundle": {
    "publisher": "Myggiz AB",
    "copyright": "© 2026 Myggiz AB",
    "license": "GPL-3.0-or-later",
    "licenseFile": "../../COPYING",
    "icon": [
      "icons/16x16.png",
      "icons/32x32.png",
      "icons/64x64.png",
      "icons/128x128.png",
      "icons/256x256.png",
      "icons/512x512.png",
      "icons/icon.ico",
      "icons/icon.icns"
    ],
    "linux": {
      "deb": {
        "depends": ["libwebkit2gtk-4.1-0", "libayatana-appindicator3-1"],
        "section": "net",
        "priority": "optional"
      },
      "appimage": { "bundleMediaFramework": false }
    },
    "macOS": {
      "minimumSystemVersion": "12.0",
      "category": "public.app-category.social-networking"
    }
  },
  "plugins": {
    "updater": { "active": false }
  }
}
```

Icons: ship pre-generated PNGs at all six sizes. Source SVG at
`crates/ui/icons/icon.svg` for future regeneration; not used by the
build, only stored for traceability.

`skattr://` URL scheme registration (NEW for 2.G):

- Linux: `crates/ui/src-tauri/.desktop` template gains
  `MimeType=x-scheme-handler/skattr;`. Tauri 2 `tauri.conf.json`
  exposes this via the `linux.deb.deepLinking` field — verify
  syntax against Tauri 2.11 docs at implementation time.
- macOS: `tauri.conf.json` `bundle.macOS.urlSchemes = ["skattr"]`.
  Tauri injects the `CFBundleURLTypes` entry into `Info.plist`.
- Runtime handling: `crates/ui/src/main.rs` adds
  `tauri-plugin-deep-link` (canonical Tauri 2 deep-link plugin) and
  `tauri-plugin-single-instance` (so a second `skattr://` click
  forwards into the existing process rather than starting a second
  daemon). Plugin docs at https://github.com/tauri-apps/plugins-workspace.
  On open-with-`skattr://` URL, dispatches a Tauri event the
  SvelteKit shell wires for "open add-contact dialog with this URL".
  Implementation note: this is a small additive surface in
  `crates/ui/src/main.rs` (a few `.plugin(...)` calls + an event
  forwarder), not a new module.

### Minisign key management

- Keypair generated locally on a maintainer machine (not in CI):
  `minisign -G -p docs/install/minisign.pub -s
  ~/.private/skattr-minisign-secret.key` (passphrase-protected).
- Public key committed at `docs/install/minisign.pub`.
- Two GitHub Actions secrets:
  - `MINISIGN_SECRET_KEY` — base64-encoded contents of the encrypted
    secret key file (newline-safe transport in the env var).
  - `MINISIGN_PASSWORD` — the passphrase to decrypt at signing time.
- CI signing step:
  ```bash
  KEY_FILE=$(mktemp -p $RUNNER_TEMP)
  echo "$MINISIGN_SECRET_KEY" | base64 -d > $KEY_FILE
  echo "$MINISIGN_PASSWORD" | minisign -Sm SHA256SUMS -s $KEY_FILE
  shred -u $KEY_FILE
  ```
- Verification one-liner in `docs/install/README.md`:
  ```bash
  minisign -Vm SHA256SUMS -P "$(cat docs/install/minisign.pub)"
  ```
- Key rotation procedure documented for completeness; not exercised
  in 2.G.

### Install docs (`docs/install/`)

- `README.md` — top-level verification flow:
  1. Download bundle from GitHub Release.
  2. Download `SHA256SUMS` + `SHA256SUMS.minisig`.
  3. `sha256sum -c SHA256SUMS --ignore-missing` (verify the bundle
     hash).
  4. `minisign -Vm SHA256SUMS -P <pubkey>` (verify minisign
     signature). Pubkey reproduced inline.
- `linux.md` — `.deb` (`sudo dpkg -i …` or `sudo apt install
  ./skattr_*.deb`); `.AppImage` (`chmod +x` + run, optional
  `appimaged` for desktop integration); Flatpak (build-from-source,
  Flathub-listing deferred). **Wayland tray caveat documented:** on
  bare Wayland desktops without StatusNotifier, the tray icon is not
  displayed; close-to-tray falls back to quit-on-close (logged at
  WARN). Recommend installing a StatusNotifier host (`waybar`,
  `swaybar`) on Sway/Hyprland.
- `macos.md` — `.dmg` mount, drag to Applications, **right-click →
  Open** to bypass the unsigned-developer Gatekeeper warning;
  power-user `xattr -d com.apple.quarantine
  /Applications/Skattr.app` documented as well.

### Flatpak manifest

`packaging/flatpak/net.myggiz.skattr.yml`:

- Source type: `dir`, path `../..` (in-repo build from a checkout).
- Runtime: `org.freedesktop.Platform//23.08`, sdk
  `org.freedesktop.Sdk//23.08` + Rust extension.
- Build commands wrap `cargo tauri build` inside the Flatpak sandbox.
- Permissions: `--share=network`, `--socket=fallback-x11`,
  `--socket=wayland`, `--socket=session-bus` (for tray /
  notifications), `--filesystem=xdg-data/skattr:create`.

`docs/build/flatpak.md` documents the diff for a Flathub-publication
manifest (tag-based source, sandbox tightening).
`packaging/flatpak/net.myggiz.skattr.metainfo.xml` ships AppStream
metadata (name, summary, description, categories, OARS rating, no
screenshots in v0.1).

### Reproducible-build doc (`docs/build/reproducible.md`)

Recipe:

```bash
export SOURCE_DATE_EPOCH=$(git log -1 --format=%ct)
export RUSTFLAGS="-C link-arg=-Wl,--build-id=none"   # Linux
pnpm --dir crates/ui/src-svelte install --frozen-lockfile
cargo tauri build --no-bundle    # build first
cargo tauri build                  # bundle from cached build
```

Pinned versions table (snapshot at 2.G start, updated when bumps
land):

| Component         | Version pin                        | Lock location                       |
|-------------------|------------------------------------|-------------------------------------|
| rustc             | exact stable resolved at 2.G start | `rust-toolchain.toml` `version=`    |
| Tauri (Rust)      | `=2.11.0`                          | `crates/ui/Cargo.toml`              |
| Tauri (JS)        | match Rust patch (e.g. `=2.11.x`)  | `crates/ui/src-svelte/package.json` |
| Node              | `20.x` (LTS)                       | `.github/workflows/release.yml`     |
| pnpm              | `10`                               | `package.json` `packageManager`     |
| Rust deps         | per `Cargo.lock`                   | committed                           |
| JS deps           | per `pnpm-lock.yaml`               | committed                           |

The exact rustc version is set during the implementation plan's
first task by reading `rustc --version` from the maintainer's
machine and committing it into `rust-toolchain.toml`. No "TBD" lands
in the committed file — the placeholder lives only in this design
doc.

Caveats: WebKit2GTK / WKWebView versions are platform-supplied and
NOT pinned by this recipe. Phase 2 explicitly does not claim
byte-identical reproducibility — only "inputs are pinned and the
recipe is documented". Phase 4 closes the byte-equivalence claim.

## Wire-format contract

**None.** No new `Command`, `CommandResult`, or `Event` variants. No
field additions on existing types. The `wire_format_append_only`
snapshot test is unchanged. If the implementation surfaces a need to
touch `crates/core/src/daemon/commands.rs` or `events.rs`, that's a
spec violation and a halt-and-discuss signal.

## Test plan

**Unit:**
- `core::daemon::smoke::run_smoke` — happy path with a fixture-
  bootstrapped Tor (use the existing `test-harness` feature's
  `test_exports`); error paths for `DataDirNotEmpty`, vault
  creation failure, Tor timeout (forced by zero-timeout config).

**Integration:**
- New `crates/tests/src/smoke_flag.rs`, `#[ignore]`-gated for real
  Tor: spawn `skattr-ui --smoke-test --data-dir <tmp>`, assert
  exit 0 within 240s, assert `<tmp>/identity.vault` exists, assert
  smoke leaves no orphan processes.
- Existing `crates/tests/src/ui_first_run.rs` (`#[ignore]`-gated)
  validates the non-smoke path still works after the argv branch
  lands.

**CI release dry-run:**
- A `release-dry-run` workflow (manual `workflow_dispatch`) that
  runs the release.yml steps end-to-end against a synthetic tag
  but skips the `gh release create` step. Used to debug the
  pipeline without burning real tags.

**Manual smoke matrix (per release tag, before public announce):**
- Linux: install `.deb` on Ubuntu 22.04 + Ubuntu 24.04 fresh VMs,
  install AppImage on a Fedora 39 fresh VM, install Flatpak from
  source on the same Fedora VM. Open app, complete first-run,
  confirm Tor reaches Ready, send a test message between two
  paired daemons.
- macOS: install `.dmg` on a fresh macOS 14 + macOS 15 Apple
  Silicon VM, repeat the above. Confirm the right-click → Open
  Gatekeeper bypass docs are accurate.

## Risks and mitigations

| Risk | Mitigation |
|------|-----------|
| Tor bootstrap exceeds 240s on a slow CI runner | Make timeout configurable; bump if observed; add a "retry once" wrapper around the smoke step for transient `TorStatus::Bootstrapping` plateaus |
| `cargo tauri build` produces non-deterministic output even with `SOURCE_DATE_EPOCH` set | Documented limitation in `reproducible.md`; Phase 4 closes the gap, not Phase 2.G |
| Minisign secret leaks via CI logs | `set +x` discipline; signing step pipes through `mktemp` files that are `shred`'d immediately; secret never echoed to stdout |
| Tauri 2.11 `bundle.linux.deb.depends` syntax differs from Tauri 1.x docs | Verify against Tauri 2.11 reference at implementation time; fall back to manual `.desktop` + `.deb` post-processing if needed |
| `skattr://` URL scheme conflicts with another app | Unlikely (the scheme is reserved-by-claim, not by registry); first-run dialog warns the user about handler conflicts on Linux |
| Flatpak build fails inside the sandbox due to network deps | Pre-fetch Cargo + pnpm deps via `flatpak-cargo-generator.py` + `flatpak-node-generator.py` ahead of the build; commit the generated lockfile JSON |
| AppImage missing `libwebkit2gtk` on user's host | AppImage built with `--bundle-media-framework=false` but webkit must be host-supplied. Document minimum WebKitGTK version in `linux.md` |
| Apple Silicon-only macOS bundle excludes Intel users | Documented limitation; Phase 2.H or Phase 5 adds Intel matrix entry |

## Out of scope for 2.G

- **Windows** — moved to Phase 2.H (carve-out above).
- **Code signing + notarisation** — Phase 5 (umbrella locked
  decision 2).
- **Auto-update mechanism** — Phase 5 (Tauri updater explicitly
  disabled here).
- **Flathub publication** — manifest committed; submission deferred.
- **Snap / RPM packaging** — `.deb` + AppImage + Flatpak cover
  major distros; Snap is a Phase 5 ask if there's demand.
- **Mac App Store / Microsoft Store distribution** — sandboxed
  network restrictions don't fit Tor.
- **macOS x86_64 bundle** — ARM64-only via `macos-latest`; Intel
  bundle deferred (additional matrix runner).
- **Phase 2.F follow-ups** — `persist_logs_to_disk` hot-toggle,
  click-to-focus on macOS, search palette inline mount, deep-link
  paged loader, Tauri save-dialog plugin. None pulled into 2.G.
- **Wire-format breaking changes** — out of scope by design.
- **Phase 3+ items** — avatars, reactions, replies, attachments,
  multi-member groups.
- **Phase 4+ items** — cover traffic, panic-wipe, duress mode,
  byte-identical reproducible builds.

## Exit criteria

- [ ] `cargo tauri build` produces a working `.deb`, `.AppImage`,
  and `.dmg` on `ubuntu-latest` and `macos-latest`.
- [ ] `skattr-ui --smoke-test --data-dir <tmp>` exits 0 within 240s
  on each platform.
- [ ] `.github/workflows/release.yml` runs end-to-end on a `v*`
  tag and produces a GitHub Release with bundles + `SHA256SUMS`
  + `SHA256SUMS.minisig`.
- [ ] `docs/install/README.md` + `linux.md` + `macos.md` cover
  download → verify → install → first-run for all three
  Linux variants and macOS.
- [ ] `docs/build/reproducible.md` documents the recipe with the
  pinned version table.
- [ ] `wire_format_append_only` snapshot test unchanged
  (zero-byte diff).
- [ ] CHANGELOG entry; CLAUDE.md status update marking 2.G complete
  and Phase 2 closed (modulo 2.H Windows port).
- [ ] Two non-technical testers complete download → verify →
  install → first-run on Linux + macOS without operator hand-
  holding.
