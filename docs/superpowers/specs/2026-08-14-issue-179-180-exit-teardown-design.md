# #179 + #180 — guarantee the daemon teardown on every deliberate exit (design)

**Issues:** #179 (`bug`, `security`, `data-path`) and #180 (`security`, `attachments`), milestone v0.1.2. Filed from the 2026-08-14 two-machine field test of v0.1.17 — #179 from Windows, #180 from Linux.
**Relates to:** #52 / #156 (the plaintext cleanup guard), 2.B / T1-2 (at-rest encryption on shutdown), #89 (tray init failure, which makes window-close the quit path on affected desktops).

**No wire-format change, no migration, no new dependency.** Changes are confined to the two binaries' exit wiring plus one shared constant.

---

## 1. Problem

Several deliberate exit paths never run the daemon's teardown, so neither of the two things that protect data at rest happens:

- `pool.close()` — `wal_checkpoint(TRUNCATE)` → age-encrypt → remove the plaintext DB and its `-wal`/`-shm` sidecars and sentinel (`state.rs:604`).
- `wipe_open_cache(data_dir)` — remove decrypted attachment plaintext from `<data_dir>/cache/open/` (`state.rs:608`).

### Field evidence

**#179 (Windows, tray → Quit).** Immediately after a normal Quit, with the process confirmed gone:

```
skattr.sqlite        1,339,392   04:27:31   <- PLAINTEXT, written at quit
skattr.sqlite-wal    4,128,272   04:27:31   <- PLAINTEXT, written at quit
skattr.sqlite.age    1,081,782   03:24:57   <- STALE (previous boot)
skattr.sqlite.open           0              <- sentinel still present
cache/open/<aid>/PSZXG3….gif                <- 27.7 MiB decrypted attachment
```

The `.age` predates the session, so **that session's messages, contact and attachment state existed only in plaintext**. This is the at-rest guarantee defeated by the only user-facing quit path.

**#180 (Linux, `kill -TERM`).** 32 MB of decrypted attachments (853 KB `.jpeg`, 2.7 MB `.png`, 29 MB `.gif`) survived termination, because `Drop` does not run on signal death and the #52 guard therefore cannot fire.

### Three distinct defects, not one

Investigation found the teardown function is well-built and simply not reliably reached.

**(a) Tray Quit never calls it** — `crates/ui/src/tray.rs:73`:

```rust
"quit" => { app.exit(0); }
```

**(b) The window-close path calls it but races** — `crates/ui/src/main.rs:397`:

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

`api.prevent_close()` is **not** called on this branch, so the default close proceeds and Tauri exits once the last window is gone. The spawned teardown — a WAL checkpoint, an encrypt, and a possibly large directory wipe — is racing a process exit it cannot reliably win. This path reads as correct, which is likely why it survived review.

This branch is reached whenever there is no tray, which on HiDPI Wayland/KDE is *every* launch (#89: `tray icon error: wrong data size, expected 1024 got 2048`). So closing the window is the quit path for those users.

**(c) No signal handling in the UI at all** — #180. Separately, the CLI daemon handles only SIGINT (`crates/cli/src/main.rs:689`):

```rust
let shutdown_fut = async {
    let _ = tokio::signal::ctrl_c().await;
};
```

so `skattr daemon` tears down on Ctrl-C but **not** on `systemctl stop` / `kill`, which send SIGTERM.

### What already works (and stays)

`daemon::shutdown` (`crates/ui/src/daemon.rs:151`) drains the daemon over its oneshot and joins the task. `state.rs:329` wipes `cache/open` at boot and `Pool::open` re-encrypts crash residue, so these leaks self-heal on next successful unlock — verified during the field test (3 plaintext files before relaunch, 0 after). **The exposure is the window between exit and the next time the passphrase is entered**, which is unbounded: launching the app and walking away, or simply not unlocking, leaves the plaintext database on disk indefinitely. This is because `Vault::open` (which requires the passphrase) runs at `state.rs:138`, before `Pool::open` at `state.rs:149`, so the boot-time backstop cannot run until after successful unlock. Field observation (2026-08-15): plaintext `skattr.sqlite`, `skattr.sqlite-wal`, and the sentinel remained on disk 18 hours after exit and were still untouched four minutes after app launch; they cleared only when the passphrase was entered.

---

## 2. Design

### 2.1 One choke point: `RunEvent::ExitRequested`

All quit paths funnel through a single handler rather than each re-establishing the guarantee.

The handler is a tri-state machine — `Idle -> TearingDown -> Done` — not a two-state flag:

- **`Idle`**: this request claims teardown (`compare_exchange` into `TearingDown`), calls `api.prevent_exit()`, runs the teardown, transitions to `Done`, then `app.exit(0)`.
- **`TearingDown`**: a request arriving *while teardown is still running* calls `api.prevent_exit()` and is **held** — it is not let through.
- **`Done`**: teardown already finished; return without preventing, so this exit proceeds.

The middle state is the point: a plain bool flipped at claim time can only say "teardown started," not "teardown finished," so a second `ExitRequested` arriving mid-teardown would pass straight through and let Tauri kill the process while `pool.close()` is mid-encrypt — the same class of bug this branch fixes, reintroduced by the handler itself. Holding that request instead keeps the process alive until the first request's teardown reaches `Done` and calls `app.exit(0)` itself, so the handler is idempotent and cannot recurse.

Consequences:

- **Tray Quit** — `tray.rs:73` is left exactly as it is. `app.exit(0)` now gets teardown for free.
- **Window close (no tray)** — the racy `spawn { shutdown; exit }` block at `main.rs:397-402` is **deleted**; the close is allowed to proceed. The race disappears because teardown no longer competes with process exit — it *is* the exit.
- **Future quit paths** — covered by construction.

The tray / `close_to_tray` hide branch (`main.rs:382-395`) is untouched.

### 2.2 Signals

A handler awaiting SIGTERM or SIGINT (unix) / `ctrl_c` (Windows) that calls `app.exit(0)`, inheriting the choke point rather than duplicating teardown.

The CLI's `shutdown_fut` widens from `ctrl_c()` alone to *either* SIGTERM or SIGINT, closing the headless half of #180.

**Windows note:** `tokio::signal::ctrl_c` covers Ctrl-C, not logoff/shutdown. Windows session-end (`WM_QUERYENDSESSION`) is deliberately out of scope (§4), so a Windows *reboot* still will not tear down cleanly. Recorded as a known limitation rather than implied as covered.

### 2.3 Bounded teardown

One named constant caps the quit teardown. Exceeding it logs a `warn!` and exits anyway; the boot-time backstop then cleans up on next successful unlock (when `Vault::open` runs at `state.rs:138` before `Pool::open` at `state.rs:149`).

**Value: 15 seconds.** Generous for a ~1.3 MB DB encrypted with scrypt at `N = 2^12` (chosen in `hs_key.rs` precisely to be fast) plus a directory wipe, and short enough that quitting never feels hung. Never leaving the user with an app that will not close matters: the workaround for a hung quit is `kill -9`, which produces exactly the outcome being prevented.

`daemon::shutdown`'s existing 30 s join is aligned to the same constant so there are not two competing numbers. It has exactly one caller today (the block being deleted in §2.1), so nothing else changes behaviour. **This is a deliberate behaviour change beyond the strict bug fix**, accepted by the maintainer.

---

## 3. Testing

### 3.1 The real test: end-to-end signal teardown (CLI)

The Tauri wiring is awkward to exercise headlessly; the core guarantee is not. The CLI daemon is headless and runs the same `run_with_transport` teardown.

Spawn the actual `skattr daemon` binary against a temp data dir, wait for ready, send **SIGTERM**, wait for exit, then assert:

- no `skattr.sqlite`, `skattr.sqlite-wal`, `skattr.sqlite-shm`
- a `skattr.sqlite.age` **newer than the process start** (proving this session was encrypted, not left stale)
- no `skattr.sqlite.open` sentinel
- `cache/open` empty or absent

This fails today and directly pins #180. The stale-`.age` assertion matters: a test that only checked "no plaintext" would pass against a daemon that never wrote anything.

### 3.2 Unit: the choke point is idempotent

Teardown runs once; a second exit request passes straight through. Pins the flag logic without a display.

### 3.3 Manual, and honestly labelled

Tray → Quit and window-close-without-tray need a desktop session. Verified by hand on both machines and recorded as **manual** in the spec — not dressed up as automated coverage.

**Gate:** `cargo fmt --all -- --check`, `cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings`, `cargo test`, `cargo deny check`, plus `cargo clippy -p skattr-ui --all-targets -- -D warnings`.

---

## 4. Out of scope

- **SIGKILL, OOM, power loss** — cannot be intercepted. The boot-time wipe and crash-residue re-encryption remain the backstop, and that is the right design.
- **Windows session-end** (`WM_QUERYENDSESSION`, console control events) — a Windows logoff/reboot still exits without teardown. Known limitation; own issue if wanted.
- **No shutdown-progress UI.** A slow quit is silent beyond the log.
- **#89 itself** (tray init failure) is not fixed here. This work makes its consequence harmless, since the close path now tears down properly.
- **Signals during startup — two distinct instances, both out of scope.**
  (1) `run_with_transport` opens the pool at Step 2 but only begins polling
  its shutdown future at Step 8, after Tor bootstrap — a window of up to
  180 s in which a SIGTERM still takes the default action and leaves the
  plaintext DB. (2) The UI has its own, separate instance of the same gap:
  `daemon::shutdown` reads `AppState.shutdown_tx`, which
  `start_in_process_cmd` only populates *after* its own 180 s readiness await
  (`crates/ui/src/daemon.rs:110-146`). A tray Quit (or a termination signal,
  via the choke point) arriving during that window finds `shutdown_tx` still
  `None`, so `daemon::shutdown` returns instantly and `ExitRequested`'s
  teardown reaches `Done` having done nothing — `app.exit(0)` then kills the
  process with the database still open. Closing either gap means restructuring
  startup so the shutdown future (or the tx that feeds it) is live from the
  moment the pool opens. Out of scope here; the boot-time backstop covers both
  on next launch. This is also why the Task 1 test must wait for readiness
  before signalling, and it is why the CHANGELOG entry for this branch does
  not claim quitting mid-startup is covered.

---

## 5. Acceptance

| # | Criterion | Where |
|---|---|---|
| 1 | Tray → Quit encrypts the DB and wipes `cache/open` | §2.1, manual |
| 2 | Window close with no tray does the same, with no race | §2.1, manual |
| 3 | `SIGTERM` to `skattr daemon` leaves no plaintext and a fresh `.age` | §3.1, manual-invocation, automated assertions |
| 4 | `SIGTERM`/`SIGINT` to the UI trigger the same teardown | §2.2 |
| 5 | Teardown cannot wedge the app: bounded, warns, exits | §2.3 |
| 6 | Teardown runs exactly once per exit | §3.2 |
| 7 | Boot-time backstop still works for uncoverable exits | unchanged, verified in field |
