Phase 2.F (settings & history) just merged at master `d329fd7`
(`Merge branch 'phase-2f-settings-history' — Phase 2.F settings &
history`). Settings panel ships with five sidebar-nav sections
(Identity / Mailboxes / History / Notifications / Advanced),
focus-aware `notify-rust` notifications + per-contact mute,
Tauri 2 tray + close-to-tray, Cmd/Ctrl-K cross-conversation search
palette, in-memory ring-buffer logs viewer with redaction, and a
Danger Zone `WipeAllData`. `ChangePassphrase` collapsed to a thin
wrapper over the existing `Vault::change_passphrase` after a
brainstorm-vs-codebase reconciliation revealed that the SQLite age
key is derived from the BIP39 seed via HKDF (independent of the
user passphrase), so only the identity vault needs re-encrypting.
Migrations 0013 (`contacts.muted`) and 0014 (`passphrase_audit`)
land alongside seven new `Command` variants, four new
`CommandResult` variants, `Event::LogRecord` + `EventFilter::Logs`,
and two additive `ContactSummary` fields. Wire format is strictly
additive; the `wire_format_append_only` snapshot test is updated
in lockstep.

Phase 2.G (packaging & distribution) is the next workstream — the
last sub-project of Phase 2 before the umbrella exit criteria are
met. The umbrella spec at
`docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md`
§"2.G — Packaging & distribution" has the authoritative scope:
per-platform bundles (Linux `.deb` + AppImage, macOS `.dmg`,
Windows `.msi`), CI release flow at
`.github/workflows/release.yml` triggered on `v*` tags
(matrix-build → SHA-256 → minisign), per-platform smoke test via
a new `--smoke-test` daemon flag, and reproducible-build
groundwork (pinned `rust-toolchain.toml`, pinned Tauri version,
documented `SOURCE_DATE_EPOCH`). 2.G merge waits on 2.F (already
satisfied on master).

**Carry-forward limitations to address in 2.G or beyond:**

- **`persist_logs_to_disk` requires daemon restart** — the
  `tracing_subscriber::reload` plumbing across the CLI/UI layered
  subscriber proved thorny enough to defer. The flag is saved to
  config.toml immediately; the rolling-file appender is installed
  on next startup. Hot-toggle is a candidate sub-task if 2.G has
  bandwidth, but isn't a prerequisite for packaging.
- **Notification click-to-focus on macOS / Windows** — Linux gets
  this via the XDG `conversation_id` hint installed by
  `crates/ui/src/notifications.rs`; macOS / Windows route clicks
  through `notify-rust`'s response API which is a separate path
  not wired in 2.F. Document in the per-OS README; lands as a
  follow-up.
- **Search palette inline mount** — Settings → History currently
  shows a placeholder pointing at the Cmd/Ctrl-K shortcut. The
  `<SearchPalette inline />` mount inside the History page lands
  when the `searchPalette.ts` store gains a per-mount instance
  vs the singleton it currently is. Independent follow-up; not
  required for 2.G's exit criterion.
- **Search-palette deep-link "row not in loaded set"** — the
  conversation view scrolls to the focused row only if it's
  already in the rendered window; jumping to a 3-month-old
  message silently does nothing. Tracked as a follow-up; needs a
  paged "jump to row" loader.
- **Tauri save-dialog plugin** — Settings → History → Export
  currently copies the formatted blob to the clipboard because
  `@tauri-apps/plugin-dialog` isn't bundled. 2.G can pull the
  plugin (bundle-size is small) and wire a real save dialog;
  alternative is to leave clipboard for v0.1 and add the plugin
  with the Phase 3 attachments work.
- **Tauri tray + Wayland** — bare Wayland (no StatusNotifier
  protocol) tray init logs a warning and falls back to "quit on
  close". The 2.G smoke test on Linux should explicitly cover
  both X11 and a Wayland desktop; if the Wayland path consistently
  fails on a popular DE (e.g. plain Sway), document the
  installation-time expectation.

**Locked decisions from Phase 2 umbrella (do not relitigate):**

1. Three platform bundles: Linux `.deb` + AppImage, macOS `.dmg`,
   Windows `.msi`. All built via `cargo tauri build`. Flatpak
   manifest committed; Flathub publication deferred.
2. Unsigned in Phase 2. macOS gets a documented Gatekeeper
   warning + workaround instructions; Windows gets the equivalent
   for SmartScreen. Code signing + notarisation lands in Phase 5.
3. Release artefacts attached to the GitHub Release: per-platform
   bundles, `SHA256SUMS` listing all bundle hashes, and
   `SHA256SUMS.minisig` (minisign signature using a key kept in
   repo secrets).
4. Per-platform smoke test runs as the final gate on every
   release. A new `--smoke-test` daemon flag completes the first-
   run wizard via injected fixtures and exits 0. Failures fail
   the release.
5. `Cargo.lock` is CI-enforced (already in place from earlier
   phases). `rust-toolchain.toml` pins the toolchain. Tauri
   version is pinned in both `Cargo.toml` and `package.json`.
   Phase 2 does NOT claim byte-identical reproducibility — only
   "inputs are pinned and the recipe is documented."
6. CI release flow lives at `.github/workflows/release.yml` and
   is triggered on `v*` tags. Matrix-build over
   `ubuntu-latest` / `macos-latest` / `windows-latest`.
7. No platform-specific code branches inside `crates/ui/` beyond
   what Tauri's `tauri.conf.json` per-platform overrides allow.
   If a new feature genuinely needs `cfg(target_os)` in Rust,
   that's an ADR.

**Topics worth pinning down in the 2.G brainstorm** (the umbrella
deferred these):

- **Bundle metadata.** App name, identifier
  (`net.myggiz.skattr` matches the existing XDG / Project.dirs
  pattern in `core::daemon::config`), category, icon set
  (need 16/32/64/128/256/512 PNG + ICO + ICNS), short
  description, long description, screenshots (if any). Decide
  what goes in the bundle metadata vs the GitHub Release
  description.
- **Minisign key management.** Generate the keypair, document
  the public key in `docs/install/`, store the secret key in
  `actions/secrets`. UX: how do users verify a downloaded
  bundle? Document a `minisign -V -P <pubkey> -m
  SHA256SUMS.minisig` invocation in the install docs.
- **`--smoke-test` flag implementation.** A new CLI flag on the
  `daemon` subcommand (or a new top-level subcommand) that
  injects fixtures into the first-run wizard and exits 0 after
  the daemon successfully bootstraps Tor + opens an onion +
  shuts down. Decide: is this a UI command (Tauri-driven) or a
  CLI-only path? The CLI path is simpler and sufficient for
  smoke; the UI path would also exercise the SvelteKit shell.
  Recommendation: CLI for the release smoke; UI smoke is a
  separate Playwright job that already exists in the
  `crates/ui/src-svelte/tests/e2e/` directory.
- **Linux `.deb` vs AppImage trade-off.** `.deb` integrates
  with apt + auto-update; AppImage is single-file portable
  no-install. Both have constituencies — ship both per the
  spec. Document when to use which in the install docs.
- **Linux Flatpak manifest.** The umbrella says committed but
  not Flathub-published. The manifest can sit at
  `packaging/flatpak/net.myggiz.skattr.yml`. UX: does the
  manifest reference the local source, or a tag? For the in-
  repo build it's local-source; for Flathub publication it'd
  be a tag. Decide which to commit.
- **Windows .msi via WiX.** WiX 4 is the current stable. Does
  the build need a custom `Wix.toml` / `Wix.config`, or does
  Tauri's default WiX template suffice? Probably the default
  works for the first cut; revisit if the installer UX is
  awful.
- **macOS `.dmg` Gatekeeper UX.** Without notarisation, users
  see "App can't be opened because it is from an unidentified
  developer". Document the right-click → Open workaround in
  `docs/install/macos.md`. Optionally explore `xattr -d
  com.apple.quarantine /Applications/Skattr.app` as the
  power-user option.
- **Auto-update (deferred).** Phase 5 ships an auto-update
  mechanism. 2.G should NOT wire one up — but if Tauri's
  built-in updater requires a config file even when disabled,
  set it to disabled explicitly so Phase 5 has a clean handoff.
- **Reproducible-build documentation.** A short
  `docs/build/reproducible.md` describing the `SOURCE_DATE_EPOCH`
  recipe + the pinned versions. Phase 4's exit criterion claims
  byte-identical reproducibility; Phase 2.G's job is "make sure
  someone could later prove or disprove the claim".
- **Smoke test reporting.** Each platform's smoke test produces
  pass/fail and a log file. Decide where the logs go — usually
  the GitHub Actions run summary is sufficient; persisting them
  as release artefacts is overkill.

## Context

- `docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md` §
  "2.G — Packaging & distribution" — sketch + locked decisions.
  Read first.
- `docs/superpowers/specs/2026-05-04-phase-2f-settings-history-design.md`
  — what 2.F shipped; 2.G inherits the live config + Settings UI
  + tray + notifications.
- `docs/skattr-implementation-plan.md` Phase 2 §Workstream 2.G —
  the original detailed task list.
- `docs/skattr-design.md` — distribution & verification posture.
- `crates/ui/tauri.conf.json` — existing Tauri config; bundle
  identifiers / icons / per-platform options live here.
- `crates/ui/Cargo.toml` — Tauri version is pinned via the
  workspace; bundle features (`tray-icon`) already enabled.
- `crates/ui/src/main.rs` — the smoke-test injection point if
  the UI path is chosen for the release smoke.
- `crates/cli/src/main.rs` — clap subcommand structure if the CLI
  path is chosen for the release smoke.
- `rust-toolchain.toml` — toolchain pin; verify it pins the right
  channel (stable) and minor version.
- `.github/workflows/` — existing CI matrix (Ubuntu + macOS) for
  test/clippy/fmt; the release flow extends with Windows.
- CLAUDE.md locked decisions remain binding. `crates/ui/` ships
  GPLv3.

## Locked from the 2.F merge (do not relitigate)

- Wire-format append-only rule — every protocol change must
  extend existing variants with `#[serde(default)]` fields or
  add new variants. The `wire_format_append_only` snapshot test
  enforces this; update the static lists alongside any addition.
  2.G should be wire-format-NEUTRAL; if you find yourself
  touching `commands.rs` for packaging, that's a smell.
- ts-rs emits the existing types as bare interfaces with
  snake_case fields (per the Phase 2.F findings). Continue
  that convention.
- `Vault::change_passphrase` is the canonical re-key surface;
  the storage age key is seed-derived (HKDF) and independent
  of the user passphrase. 2.G's installer / first-run wiring
  must respect this — DO NOT introduce a parallel "wrap age
  key under user passphrase" code path.
- The Tauri app identifier is `net.myggiz.skattr` (matches
  `core::daemon::config`'s `directories::ProjectDirs::from("net",
  "myggiz", "skattr")` invocation). Use this consistently across
  bundle metadata and any platform-specific install paths.
- The 2.F `--smoke-test` flag does NOT exist yet — Plan Task 44
  noted it as the major net-new piece for 2.G beyond bundling +
  CI. Design it before bundling.

## After brainstorming

- `superpowers:writing-plans` to author the implementation plan.
- `superpowers:using-git-worktrees` to branch off master onto a
  `phase-2g-packaging` branch.
- `superpowers:test-driven-development` +
  `superpowers:subagent-driven-development` to execute.
- `superpowers:verification-before-completion` before the merge.

## Out of scope for 2.G

- Code signing + notarisation (Phase 5).
- Auto-update mechanism (Phase 5).
- Flathub publication (deferred — manifest committed only).
- macOS Mac App Store distribution (out of skattr's threat model
  — sandboxed network restrictions don't fit Tor).
- Windows Store distribution (same).
- Mobile shell builds — Phase 2 leaves the door open via the IPC
  adapter; no implementation in Phase 2.
- Snap / RPM Linux packages — `.deb` + AppImage + Flatpak cover
  the major distros; Snap is a Phase 5 ask if there's demand.
- Phase 2.F follow-ups: `persist_logs_to_disk` hot-toggle,
  notification click-to-focus on macOS/Windows, search-palette
  inline mount, conversation deep-link paged loader, Tauri
  save-dialog plugin. Independent follow-ups; tracked at the
  top of this kickoff.
- Wire-format BREAKING changes — any rename or removal of
  Command / CommandResult / Event variants requires a separate
  spec.
- Phase 3+ items (avatars, reactions, replies, attachments,
  multi-member groups).
- Phase 4+ items (cover traffic, panic-wipe, duress mode,
  byte-identical reproducible builds).
