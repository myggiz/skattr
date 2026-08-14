# #179 + #180 — Exit Teardown Guarantee Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every deliberate exit — tray Quit, window close, SIGTERM/SIGINT — run the daemon teardown that encrypts the database and wipes decrypted attachment plaintext.

**Architecture:** One choke point at Tauri's `RunEvent::ExitRequested` (prevent the exit, tear down, mark done, exit for real) so every quit path inherits the guarantee, including future ones. Signal handlers in both binaries simply call `app.exit(0)` and inherit the same path rather than duplicating teardown. A single bounded timeout means a stuck teardown warns and exits instead of wedging the app.

**Tech Stack:** Rust 2021, Tauri 2.11, tokio (`signal::unix`, `signal::ctrl_c`).

**Spec:** `docs/superpowers/specs/2026-08-14-issue-179-180-exit-teardown-design.md`

**Branch:** `179-exit-teardown` — already carries `863351f` (the spec).

## Global Constraints

- **No `unwrap`/`expect` in library or binary code.** Tests may use them.
- **Done-gate:** `cargo fmt --all -- --check`, `cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings`, `cargo test`, `cargo deny check`, and `cargo clippy -p skattr-ui --all-targets -- -D warnings`.
- **Bounded teardown constant is exactly 15 seconds**, used by both the quit choke point and `daemon::shutdown`'s join. Do not introduce a second timeout value.
- **Never log onions, pubkeys, or payloads at info+.**
- **Do not change** `tray.rs`'s `"quit" => { app.exit(0); }` — it is correct once the choke point exists, and leaving it proves the choke point works.
- **Do not touch** the tray / `close_to_tray` hide branch in `on_window_event` (`main.rs:382-395`).
- Every `.rs` file keeps its GPLv3 licence header. Commit with `git commit -s` (DCO enforced).
- Cargo is not on PATH: prefix with `. "$HOME/.cargo/env" && `. Run all cargo commands in the FOREGROUND.

---

### Task 1: CLI daemon tears down on SIGTERM

**Files:**
- Create: `crates/tests/src/signal_teardown.rs`
- Modify: `crates/tests/src/lib.rs` (register the module)
- Modify: `crates/cli/src/main.rs:689-691` (widen `shutdown_fut`)

**Interfaces:**
- Consumes: nothing.
- Produces: nothing consumed by later tasks. Self-contained.

**Why the CLI first:** it is headless, so it gives a real end-to-end signal test. The Tauri paths (Tasks 2-3) cannot be driven without a desktop session and are verified manually.

- [ ] **Step 1: Write the failing test**

Create `crates/tests/src/signal_teardown.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! #180: a SIGTERM'd daemon must still run its teardown — encrypt the
//! database and wipe decrypted attachment plaintext. Spawns the real
//! `skattr` binary so the signal path is exercised exactly as in the field.
//!
//! `#[ignore]`-gated: the daemon only begins polling its shutdown future
//! after Tor bootstrap completes (Step 8 of `run_with_transport`), so the
//! test must wait for readiness before signalling.
//!
//! Run with:
//!
//! ```bash
//! cargo build -p skattr-cli --release
//! cargo test -p skattr-tests --release -- --ignored signal_teardown
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

/// Tempdir under `$HOME/.cache/` — Arti's fs-mistrust rejects world-writable
/// `/tmp` on Linux. Mirrors `smoke_flag.rs`.
fn safe_tempdir() -> tempfile::TempDir {
    let cache_root = std::env::var_os("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".cache").join("skattr-signal-test"))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/skattr-signal-test-no-home"));
    std::fs::create_dir_all(&cache_root).expect("cache_root mkdir");
    tempfile::Builder::new()
        .prefix("sig-")
        .tempdir_in(&cache_root)
        .expect("tempdir_in cache")
}

fn skattr_bin() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p.join("target").join("release").join("skattr")
}

#[test]
#[ignore = "spawns a real daemon (Tor bootstrap); build skattr-cli --release first"]
#[cfg(unix)]
fn sigterm_leaves_no_plaintext_and_a_fresh_age() {
    let dir = safe_tempdir();
    let data_dir = dir.path();
    let pass_path = data_dir.join("pass.txt");
    std::fs::write(&pass_path, "correct horse battery staple").unwrap();

    let bin = skattr_bin();
    assert!(bin.exists(), "build first: cargo build -p skattr-cli --release");

    // Identity must exist before the daemon can unlock a vault.
    let init = Command::new(&bin)
        .args(["init", "--data-dir"])
        .arg(data_dir)
        .arg("--passphrase-file")
        .arg(&pass_path)
        .output()
        .expect("spawn init");
    assert!(init.status.success(), "init failed: {}", String::from_utf8_lossy(&init.stderr));

    let started = SystemTime::now();
    let mut child = Command::new(&bin)
        .args(["daemon", "--data-dir"])
        .arg(data_dir)
        .arg("--passphrase-file")
        .arg(&pass_path)
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn daemon");

    // Wait for readiness. The daemon only polls its shutdown future after
    // Step 8, so signalling earlier would test the default signal action.
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.contains("Ctrl-C to shut down") {
                let _ = tx.send(());
                break;
            }
        }
    });
    rx.recv_timeout(Duration::from_secs(240))
        .expect("daemon never became ready within 240s");

    // The plaintext DB must exist right now — otherwise the assertions below
    // would pass vacuously against a daemon that never wrote anything.
    assert!(
        data_dir.join("skattr.sqlite").exists(),
        "precondition: plaintext DB should exist while running"
    );

    let pid = child.id();
    let kill = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .expect("send SIGTERM");
    assert!(kill.success(), "kill -TERM failed");

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("daemon did not exit within 60s of SIGTERM");
            }
            None => std::thread::sleep(Duration::from_millis(200)),
        }
    }

    for leftover in ["skattr.sqlite", "skattr.sqlite-wal", "skattr.sqlite-shm", "skattr.sqlite.open"] {
        assert!(
            !data_dir.join(leftover).exists(),
            "{leftover} must not survive a SIGTERM'd shutdown"
        );
    }

    let age = data_dir.join("skattr.sqlite.age");
    assert!(age.exists(), "encrypted DB must exist after shutdown");
    let mtime = std::fs::metadata(&age).unwrap().modified().unwrap();
    assert!(
        mtime >= started,
        "skattr.sqlite.age is stale — this session was never encrypted"
    );

    let cache_open = data_dir.join("cache").join("open");
    let remaining = std::fs::read_dir(&cache_open)
        .map(|rd| rd.flatten().count())
        .unwrap_or(0);
    assert_eq!(remaining, 0, "decrypted plaintext left in cache/open");
}
```

Register it in `crates/tests/src/lib.rs`, keeping the list alphabetical:

```rust
#[cfg(test)]
mod signal_teardown;
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo build -p skattr-cli --release
cargo test -p skattr-tests --release -- --ignored sigterm_leaves_no_plaintext
```

Expected: FAIL. The daemon dies on the default SIGTERM action, so `skattr.sqlite` survives and the first leftover assertion trips.

- [ ] **Step 3: Widen the CLI's shutdown future**

In `crates/cli/src/main.rs`, replace the `shutdown_fut` at line 689:

```rust
    // #180: SIGTERM is what logout, reboot, systemd and `kill` send — not
    // SIGINT. Handling only Ctrl-C meant those paths skipped the teardown
    // that encrypts the DB and wipes decrypted attachment plaintext.
    let shutdown_fut = async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            match (signal(SignalKind::terminate()), signal(SignalKind::interrupt())) {
                (Ok(mut term), Ok(mut intr)) => {
                    tokio::select! {
                        _ = term.recv() => {}
                        _ = intr.recv() => {}
                    }
                }
                // Registration failed (unusual): fall back to Ctrl-C so the
                // daemon is still stoppable rather than unkillable-cleanly.
                _ => {
                    let _ = tokio::signal::ctrl_c().await;
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    };
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo build -p skattr-cli --release
cargo test -p skattr-tests --release -- --ignored sigterm_leaves_no_plaintext
```

Expected: PASS. Also run the normal suite to confirm nothing regressed:
`cargo test -p skattr-tests`

- [ ] **Step 5: Commit**

```bash
git add crates/cli/src/main.rs crates/tests/src/signal_teardown.rs crates/tests/src/lib.rs
git commit -s -m "fix(cli): tear down on SIGTERM, not just SIGINT (#180)"
```

---

### Task 2: One choke point at `RunEvent::ExitRequested`

**Files:**
- Modify: `crates/ui/src/main.rs` — add the constant + flag, convert `.run(...)` to `.build(...)?.run(|app, event| …)`, delete the racy spawn at `:397-402`
- Modify: `crates/ui/src/daemon.rs:151-162` — align the join timeout to the shared constant

**Interfaces:**
- Produces: `pub const QUIT_TEARDOWN_TIMEOUT: std::time::Duration` in `crates/ui/src/daemon.rs`, consumed by `main.rs` and by `daemon::shutdown` itself. Task 3 relies on the choke point existing but adds no new interface.

- [ ] **Step 1: Write the failing test**

The handler itself needs a Tauri run loop, but its *decision* does not. Extract that decision so there is something real to test — a test asserting `AtomicBool::swap` semantics directly would pass with the handler deleted and is worthless.

Add to `crates/ui/src/main.rs` in the existing `mod smoke_argv_tests` (the file's only test module):

```rust
    #[test]
    fn first_exit_tears_down_then_later_ones_pass_through() {
        // #179: the ExitRequested handler calls app.exit() after tearing down,
        // which raises ExitRequested again. Without this claim the second pass
        // would tear down a second time and never actually exit.
        use std::sync::atomic::AtomicBool;
        let flag = AtomicBool::new(false);

        assert!(super::claim_teardown(&flag), "first exit must tear down");
        assert!(!super::claim_teardown(&flag), "second exit must pass through");
        assert!(!super::claim_teardown(&flag), "and stay passed through");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p skattr-ui --bins first_exit_tears_down
```

Expected: FAIL — `cannot find function 'claim_teardown' in module 'super'`.

- [ ] **Step 2b: Add the function under test**

Add to `crates/ui/src/main.rs`:

```rust
/// Claim the one-shot right to run the quit teardown.
///
/// Returns `true` exactly once per process: the caller that gets `true` owns
/// the teardown, every later caller must let the exit proceed. Extracted from
/// the `RunEvent::ExitRequested` handler so the decision is testable without
/// a Tauri run loop.
fn claim_teardown(flag: &AtomicBool) -> bool {
    !flag.swap(true, Ordering::SeqCst)
}
```

Re-run: expected PASS. The handler wiring itself is verified manually (spec §3.3) because it needs a desktop session.

- [ ] **Step 3: Add the shared constant and align the join**

In `crates/ui/src/daemon.rs`, above `pub async fn shutdown`:

```rust
/// Hard cap on the quit teardown (#179/#180).
///
/// Generous for a small DB encrypted with scrypt at `N = 2^12` plus a
/// directory wipe, short enough that quitting never feels hung. Exceeding it
/// warns and exits anyway: an app that will not close gets `kill -9`, which
/// produces exactly the plaintext-on-disk outcome this is preventing. The
/// boot-time wipe and `Pool::open` crash-residue re-encryption clean up then.
pub const QUIT_TEARDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
```

and in `shutdown`, replace the hardcoded 30s join:

```rust
    let handle = state.task.lock().await.take();
    if let Some(handle) = handle {
        let _ = tokio::time::timeout(QUIT_TEARDOWN_TIMEOUT, handle).await;
    }
```

- [ ] **Step 4: Delete the racy spawn**

In `crates/ui/src/main.rs`, the `else` branch of the `CloseRequested` handler currently reads:

```rust
                } else {
                    // No tray or close_to_tray disabled: normal quit path.
                    let app = window.app_handle().clone();
                    tauri::async_runtime::spawn(async move {
                        daemon::shutdown(&app).await;
                        app.exit(0);
                    });
                }
```

Replace the whole `else` block with:

```rust
                }
                // No tray, or close_to_tray disabled: let the close proceed.
                // Teardown is NOT spawned here — it would race the process
                // exit it cannot win. `RunEvent::ExitRequested` below owns it.
```

i.e. keep the `if tray_present && close_to_tray { … }` branch exactly as-is and drop the `else` entirely.

- [ ] **Step 5: Add the choke point**

Add near the top of `crates/ui/src/main.rs` (beside the other statics):

```rust
/// Set once the quit teardown has run, so the `app.exit()` we issue after
/// tearing down does not re-enter the handler (#179).
static TORN_DOWN: AtomicBool = AtomicBool::new(false);
```

Then convert the builder tail. Replace:

```rust
        .run(tauri::generate_context!());

    if let Err(e) = result {
        tracing::error!(error = %e, "Tauri runtime exited with error");
        std::process::exit(1);
    }
```

with:

```rust
        .build(tauri::generate_context!());

    let app = match result {
        Ok(app) => app,
        Err(e) => {
            tracing::error!(error = %e, "Tauri runtime failed to build");
            std::process::exit(1);
        }
    };

    // #179/#180: the single place every deliberate exit funnels through —
    // tray Quit, window close, a termination signal, or anything added later.
    // Without it, `app.exit(0)` ends the process with the database still in
    // plaintext and decrypted attachments still on disk.
    app.run(|app_handle, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            if !claim_teardown(&TORN_DOWN) {
                // Already torn down — this is the exit we asked for. Let it go.
                return;
            }
            api.prevent_exit();
            let app_handle = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                if tokio::time::timeout(
                    daemon::QUIT_TEARDOWN_TIMEOUT,
                    daemon::shutdown(&app_handle),
                )
                .await
                .is_err()
                {
                    tracing::warn!(
                        "quit teardown exceeded its timeout; exiting anyway \
                         (boot-time wipe will clean up)"
                    );
                }
                app_handle.exit(0);
            });
        }
    });
```

Note `result` is now the `.build(...)` result; the variable it was bound to keeps its name.

- [ ] **Step 6: Verify it compiles and the suite passes**

```bash
cargo clippy -p skattr-ui --all-targets -- -D warnings
cargo test -p skattr-ui --bins
```

Expected: zero warnings, all tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/ui/src/main.rs crates/ui/src/daemon.rs
git commit -s -m "fix(ui): guarantee daemon teardown via a single ExitRequested choke point (#179)"
```

---

### Task 3: UI signal handler

**Files:**
- Modify: `crates/ui/src/main.rs` — add `spawn_signal_handler`, call it from `setup`

**Interfaces:**
- Consumes: the Task 2 choke point (this handler only calls `app.exit(0)`; the teardown is the choke point's job).
- Produces: `fn spawn_signal_handler(app: tauri::AppHandle)`.

- [ ] **Step 1: Add the handler**

Add to `crates/ui/src/main.rs`:

```rust
/// #180: route termination signals into the normal quit path.
///
/// Calls `app.exit(0)` rather than tearing down here, so there is exactly one
/// teardown implementation (`RunEvent::ExitRequested`). `Drop` never runs on
/// signal death, so without this a logout, reboot or `kill` leaves the
/// database in plaintext and decrypted attachments on disk.
#[cfg(unix)]
fn spawn_signal_handler(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        let (mut term, mut intr) =
            match (signal(SignalKind::terminate()), signal(SignalKind::interrupt())) {
                (Ok(t), Ok(i)) => (t, i),
                _ => {
                    tracing::warn!("could not register termination handlers");
                    return;
                }
            };
        tokio::select! {
            _ = term.recv() => {}
            _ = intr.recv() => {}
        }
        tracing::info!("termination signal received; shutting down");
        app.exit(0);
    });
}

/// Windows counterpart. Covers Ctrl-C only — session end (logoff/shutdown)
/// is not handled; see the spec's out-of-scope section.
#[cfg(not(unix))]
fn spawn_signal_handler(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("Ctrl-C received; shutting down");
            app.exit(0);
        }
    });
}
```

- [ ] **Step 2: Call it from setup**

Inside the existing `.setup(|app| { … })` closure in `crates/ui/src/main.rs`, add before the closure returns `Ok(())`:

```rust
            spawn_signal_handler(app.handle().clone());
```

- [ ] **Step 3: Verify**

```bash
cargo clippy -p skattr-ui --all-targets -- -D warnings
cargo test -p skattr-ui --bins
```

Expected: zero warnings, tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ui/src/main.rs
git commit -s -m "fix(ui): route SIGTERM/SIGINT into the quit teardown (#180)"
```

---

### Task 4: Gate, CHANGELOG, and the bootstrap-window limitation

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `docs/superpowers/specs/2026-08-14-issue-179-180-exit-teardown-design.md` (§4 out-of-scope)

**Interfaces:** none.

- [ ] **Step 1: Record the bootstrap-window limitation in the spec**

Investigation during planning found a gap worth disclosing. `run_with_transport` runs its startup steps sequentially and only begins polling the shutdown future at Step 8, after Tor bootstrap. The pool is opened at Step 2. So a signal arriving **during bootstrap** — a window that can last up to 180 s — still takes the default action and leaves the plaintext DB behind.

Add to §4 (Out of scope) of the spec:

```markdown
- **Signals during startup.** `run_with_transport` opens the pool at Step 2 but
  only begins polling its shutdown future at Step 8, after Tor bootstrap — a
  window of up to 180 s in which a SIGTERM still takes the default action and
  leaves the plaintext DB. Closing it means restructuring startup so the
  shutdown future is selected against from the moment the pool opens. Out of
  scope here; the boot-time backstop covers it on next launch. This is also
  why the Task 1 test must wait for readiness before signalling.
```

- [ ] **Step 2: Add the CHANGELOG entry**

Under `### Fixed` in the `## [Unreleased] — targeting v0.1.17` section of `CHANGELOG.md`, in the same user-facing voice as its neighbours:

```markdown
- **Quitting could leave your messages unencrypted on disk** (#179, #180):
  closing the app from the tray — or closing the window on systems without a
  tray, or shutting your computer down — could skip the step that locks your
  data away again. The database, and any attachment you had opened, could be
  left readable until the next time the app started. Every way of quitting now
  performs that step, and if it cannot finish quickly the app says so in the
  log and still closes rather than hanging.
```

- [ ] **Step 3: Run the full gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings
cargo clippy -p skattr-ui --all-targets -- -D warnings
cargo test
cargo deny check
```

Expected: fmt clean, zero clippy warnings, all suites 0 failed, deny ok. Run in the FOREGROUND and paste the real output — no success claims without it.

Note the Task 1 signal test is `#[ignore]`-gated and will **not** run here; it is run explicitly per Task 1 Step 4.

- [ ] **Step 4: Commit**

```bash
git add CHANGELOG.md docs/superpowers/specs/2026-08-14-issue-179-180-exit-teardown-design.md
git commit -s -m "docs: record the exit-teardown fix and the startup-window limitation (#179, #180)"
```

---

## Verification checklist

Maps to the spec's acceptance table (§5):

- [ ] Tray → Quit encrypts the DB and wipes `cache/open` — Task 2 choke point; **manual**
- [ ] Window close with no tray does the same, with no race — Task 2 (racy spawn deleted); **manual**
- [ ] `SIGTERM` to `skattr daemon` leaves no plaintext and a fresh `.age` — Task 1, automated
- [ ] `SIGTERM`/`SIGINT` to the UI trigger the same teardown — Task 3
- [ ] Teardown cannot wedge the app: bounded, warns, exits — Task 2 Step 5
- [ ] Teardown runs exactly once per exit — Task 2 Step 1
- [ ] Boot-time backstop unchanged — no task touches `state.rs:329`
