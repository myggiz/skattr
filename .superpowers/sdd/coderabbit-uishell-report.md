# CodeRabbit UI-shell findings — resolution report

Branch: `attachments-encrypted-at-rest` (PR #19)
Date: 2026-06-28

## Per-finding disposition

### Fix #15/#18 — Reverse-DNS identifier + stable data dir

**FIXED.**

- `crates/ui/tauri.conf.json`: `"identifier": "skattr"` → `"identifier": "net.myggiz.skattr"`.
- `crates/ui/src/main.rs` setup closure: replaced `app.path().app_data_dir()` (which would have returned `~/.local/share/net.myggiz.skattr` with the new identifier) with `consolidated_data_dir()`.  `consolidated_data_dir()` is XDG_DATA_HOME / HOME -based and always resolves to `~/.local/share/skattr`, so the user's real data is not stranded.
- Updated the doc-comment on `consolidated_data_dir()` to explain the deliberate decoupling from the Tauri identifier.
- Updated the stale inline comment in `main()` that previously claimed the path "matches app_data_dir() for identifier skattr".

**Confirmed:** daemon data dir is `~/.local/share/skattr` via `consolidated_data_dir()` — independent of the Tauri identifier.

### Fix #1 — Remove asset protocol

**FIXED.**

Verification: `grep -rn "convertFileSrc\|asset:" src-svelte/src` — only in
`src/lib/test/tauri-mock.ts` (mock stub) and a comment in `FileAttachmentBubble.test.ts`.
No production Svelte/TS component uses `convertFileSrc` or `asset:` URLs.

Changes:
- `crates/ui/tauri.conf.json`: removed `"assetProtocol"` block; stripped `asset: http://asset.localhost` from `img-src` CSP.
- `crates/ui/Cargo.toml`: removed `"protocol-asset"` from tauri features.
- `crates/ui/src/main.rs`: removed `app.asset_protocol_scope().allow_directory(...)` call and the `download_dir` resolution that existed solely for it (`Config::resolved_download_dir()` is still called in `start_in_process_cmd` for the daemon config).
- `Cargo.lock`: `http-range` (pulled in only by `protocol-asset`) dropped automatically.

### Fix #17 — data_dir 0700 permissions

**FIXED.**

In the `setup` closure in `crates/ui/src/main.rs`: after `create_dir_all(&data_dir)`, added a `#[cfg(unix)]` block that calls `std::fs::set_permissions(..., Permissions::from_mode(0o700))`.  Failure is best-effort — logged at `warn!`, does not abort startup.

### Fix #16 — Migration errors propagated

**FIXED.**

`migrate_legacy_data` changed signature from `fn(...) -> ()` (silently swallowing rename errors) to `fn(...) -> Result<(), String>`.  Rename failures for REAL data files (the `identity.vault`-containing old dir) now propagate as `Err`; the setup closure `.map_err(|e| format!("data migration failed: {e}"))?` surfaces these as a Tauri setup error, aborting startup rather than continuing with a potentially incomplete identity.

Config migration (`config.toml`) remains best-effort (missing config → defaults) and only logs a `warn!`.

### Fix #13 — reset_local_data daemon-live guard

**FIXED.**

In `crates/ui/src/bootstrap.rs`, `reset_local_data` now checks `state.ready.read().is_some()` at the top and returns `Err(...)` if the in-process daemon is running.  This prevents accidental data destruction if the command is somehow invoked while the daemon holds open files.

### Fix #14 — Daemon leak on startup timeout

**FIXED.**

In `crates/ui/src/daemon.rs`, the `Err(_)` (timeout) arm of the `ready_rx` match now:
1. Sends the graceful shutdown signal: `let _ = shutdown_tx.send(());`
2. Calls `task.abort()` to cancel the Tokio task.

Previously the timeout returned immediately, leaving an orphaned daemon task consuming resources indefinitely.

## Gate output

```
cargo fmt --all --check   → clean (no diff)
cargo clippy --all-targets -- -D warnings → Finished (0 warnings)
cargo test -p skattr-ui   → 18 passed; 0 failed
cargo build -p skattr-ui  → Finished dev profile
pnpm check (svelte-check) → 0 errors and 0 warnings
```

## Data-dir confirmation

`consolidated_data_dir()` (lines 134–151 of `crates/ui/src/main.rs` after changes):
```rust
fn consolidated_data_dir() -> std::path::PathBuf {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        ...
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    data_home.join("skattr")
}
```
Result: `$XDG_DATA_HOME/skattr` (default `~/.local/share/skattr`).  The setup closure now calls this function instead of `app.path().app_data_dir()`, so the identifier change from `skattr` → `net.myggiz.skattr` does NOT change where the daemon looks for data.
