# Phase 0.C Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the four follow-ups surfaced by the Phase 0.C final review and post-merge exit-criterion run — `arti_echo` tempdir permissions (#53), `is_key_already_exists_error` string-match hardening (#45), `experimental-api` feature audit comment (#46), and transport-visibility re-narrowing via a `Daemon::run` wrapper (#50).

**Architecture:** One `Daemon::run` public entry point in `crates/core/src/daemon/state.rs` owns the "unlock vault → bootstrap Tor → publish onion → await shutdown" flow; the CLI calls it. Transport returns to `pub(crate)`. Other tasks are in-place fixes. No wire-format changes, no new dependencies.

**Tech Stack:** Same as Phase 0.C — workspace already declares everything needed.

**Exit criteria:**
- Task-list items #45, #46, #50, #53 closed.
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --release` all green.
- `cargo test -p skattr-tests --release -- --ignored two_daemons_echo_bytes_over_tor` passes on a network-connected Linux machine (real Tor, 3-10 min).
- `crates/core/src/lib.rs` has `pub(crate) mod transport;` (not `pub`), `crates/core/src/transport/mod.rs` has `pub(crate) mod tor;` (not `pub`), and `pub(crate) use listener::OnionListener;` (not `pub`).
- `skattr daemon` still works end-to-end; `crates/cli/src/main.rs` does not import from `skattr_core::transport::*`.

---

## File structure

```
crates/core/src/daemon/
└── state.rs                MODIFY: add pub Daemon::run(data_dir, passphrase) → Result<()>

crates/core/src/lib.rs      MODIFY: transport back to pub(crate)
crates/core/src/transport/
├── mod.rs                  MODIFY: tor back to pub(crate); OnionListener back to pub(crate)
└── tor.rs                  MODIFY: harden is_key_already_exists_error + add experimental-api audit comment

crates/cli/src/main.rs      MODIFY: daemon handler calls Daemon::run instead of reaching into transport

crates/tests/src/arti_echo.rs   MODIFY: chmod the tempdir to 0700 after creation

Cargo.toml                  MODIFY: add comment near arti-client's experimental-api feature listing used APIs
```

Everything else untouched.

---

## Pre-flight

```bash
cd /home/myggiz/development/skattr
. "$HOME/.cargo/env"

cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --release

git worktree add ../skattr-phase-0c-cleanup -b phase-0c-cleanup
cd ../skattr-phase-0c-cleanup
cargo build --workspace
```

All gates green. Subsequent tasks assume `/home/myggiz/development/skattr-phase-0c-cleanup`.

---

## Task 1: Fix `arti_echo` tempdir permissions (closes #53)

**Goal:** Chmod the tempdirs to 0700 before handing them to `TorRuntime::bootstrap` so Arti's state-dir permission check passes.

**Files:** Modify `crates/tests/src/arti_echo.rs`.

- [ ] **Step 1: Add a tempdir helper**

Near the top of `crates/tests/src/arti_echo.rs` (after the `use` block), add:

```rust
/// Create a tempdir with 0700 permissions — Arti 0.41 refuses to open
/// a `state_dir` that's group- or world-readable.
fn arti_friendly_tempdir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(tmp.path()).unwrap().permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(tmp.path(), perms).unwrap();
    }
    tmp
}
```

- [ ] **Step 2: Use it at both call sites**

In `two_daemons_echo_bytes_over_tor`, replace:

```rust
    let tmp_a = tempfile::tempdir().unwrap();
    let tmp_b = tempfile::tempdir().unwrap();
```

with:

```rust
    let tmp_a = arti_friendly_tempdir();
    let tmp_b = arti_friendly_tempdir();
```

Everything else in the test stays identical.

- [ ] **Step 3: Verify the test at least compiles**

```bash
cargo build -p skattr-tests --release 2>&1 | tail -3
cargo test -p skattr-tests --release --no-run 2>&1 | tail -3
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: builds clean, clippy clean. Do NOT try to actually run the ignored test (takes 5+ minutes real-Tor; leave that for a final smoke pass once all four tasks land).

- [ ] **Step 4: Commit**

```bash
git add crates/tests/src/arti_echo.rs
git commit -m "tests: chmod arti_echo tempdirs to 0700 (closes #53)

Arti 0.41 refuses to open a state_dir with group- or world-readable
permissions. tempfile::tempdir() creates directories with umask-default
perms (typically 0755), so the two_daemons_echo_bytes_over_tor test
failed at bootstrap with 'problem with filesystem permissions'.

Fix: arti_friendly_tempdir() helper creates a tempdir and chmods it
to 0700 before returning. Both daemon tempdirs use it. Unix-gated —
Windows doesn't have POSIX permissions and Arti's check is Unix-only
in the first place."
```

---

## Task 2: Harden `is_key_already_exists_error` detection (closes #45)

**Goal:** Replace the string-match detection of "key already exists" in `inject_hs_secret`'s error path with something more robust — probe the keymgr BEFORE attempting insertion, so we never rely on upstream's Display message wording.

**Files:** Modify `crates/core/src/transport/tor.rs`.

- [ ] **Step 1: Inspect current implementation**

Open `crates/core/src/transport/tor.rs` and locate the current `is_key_already_exists_error` helper (it's private, near `inject_hs_secret`). It currently lowercases the `error.source()` chain and searches for `"already exists"`. That's the surface to replace.

- [ ] **Step 2: Replace with a pre-insert existence probe**

Find `inject_hs_secret` (it's the helper called from `publish_onion`). Its current shape is approximately:

```rust
fn inject_hs_secret(
    client: &TorClient<TokioRustlsRuntime>,
    nickname: &tor_hsservice::HsNickname,
    secret: &HsSecretBytes,
) -> Result<()> {
    let kp = hs_id_keypair_from_secret(secret);
    let spec = tor_hsservice::HsIdKeypairSpecifier::new(nickname.clone());
    match client.keymgr().insert(kp, &spec, KeystoreSelector::Primary, false) {
        Ok(_) => Ok(()),
        Err(e) if is_key_already_exists_error(&e) => Ok(()),
        Err(e) => Err(CoreError::Transport(format!("keymgr insert: {e}"))),
    }
}
```

Replace it with:

```rust
fn inject_hs_secret(
    client: &TorClient<TokioRustlsRuntime>,
    nickname: &tor_hsservice::HsNickname,
    secret: &HsSecretBytes,
) -> Result<()> {
    let spec = tor_hsservice::HsIdKeypairSpecifier::new(nickname.clone());
    // Probe the keymgr BEFORE inserting: if an HS identity keypair for
    // this nickname already exists, Arti's launch_onion_service call will
    // pick it up from the keystore and our seed-derived key is ignored.
    // Deliberate: this is the "subsequent run, same state_dir" path.
    match client.keymgr().get::<tor_hsservice::HsIdKeypair>(&spec, KeystoreSelector::Primary) {
        Ok(Some(_)) => return Ok(()),
        Ok(None) => { /* slot empty — proceed to insert */ }
        Err(e) => {
            return Err(CoreError::Transport(format!("keymgr get: {e}")));
        }
    }
    let kp = hs_id_keypair_from_secret(secret);
    client
        .keymgr()
        .insert(kp, &spec, KeystoreSelector::Primary, false)
        .map_err(|e| CoreError::Transport(format!("keymgr insert: {e}")))?;
    Ok(())
}
```

Key change: `keymgr.get(&spec, ...)` checks whether the slot is occupied. If it is, we skip the insert entirely; if it isn't, we insert fresh. No more reliance on error-message text.

- [ ] **Step 3: Delete the now-unused `is_key_already_exists_error` helper**

The function is no longer called. Remove it.

- [ ] **Step 4: Verify**

```bash
cargo build -p skattr-core 2>&1 | tail -3
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
cargo test -p skattr-core --lib transport --release 2>&1 | tail -5
```

Expected: build clean (no dead `is_key_already_exists_error`), clippy clean, all non-ignored transport tests pass.

**API-drift caveat.** The exact method name on `keymgr()` may differ from `get::<HsIdKeypair>(&spec, selector)` depending on Arti 0.41's public keymgr surface. Read `tor_keymgr::KeyMgr` rustdoc to find the correct "does this slot exist?" API. If the method is `contains_key` / `get_entry` / something else, adapt. Keep the semantic intent: probe-before-insert, not error-string-match.

If the keymgr surface does NOT expose a lookup method at all (only insert/remove), escalate — we'd need to keep the error-match approach and improve the match (e.g., downcast to a typed error variant if one is exposed, rather than string-match).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/transport/tor.rs
git commit -m "transport: probe keymgr before HS key insert (closes #45)

Replace the string-match error detection (which was fragile against
upstream Display rewrites) with a deliberate keymgr.get() probe before
insert. If the HS identity keypair for this nickname already exists,
the slot is reused; otherwise we insert our seed-derived key fresh.

Deletes the now-unused is_key_already_exists_error helper. Net: no
more reliance on Arti error-message wording."
```

---

## Task 3: Add `experimental-api` audit comment (closes #46)

**Goal:** Drop a comment near `arti-client`'s `experimental-api` feature listing which exact APIs we use from it, so a future maintainer (or future Claude session) bumping Arti sees the audit checklist inline.

**Files:** Modify `Cargo.toml` (workspace root).

- [ ] **Step 1: Update the workspace dep**

In `Cargo.toml` at the repo root, find the `arti-client` workspace-dependency line (currently something like):

```toml
arti-client = { version = "0.41", default-features = false, features = ["tokio", "rustls", "onion-service-client", "onion-service-service", "experimental-api"] }
```

Replace with:

```toml
# `experimental-api` is unstable and may break between minor Arti
# releases. Used ONLY for:
#   - `TorClient::launch_onion_service_with_hsid` (transport/tor.rs)
#   - `TorClient::keymgr` (transport/tor.rs)
# On every Arti version bump, re-audit these usages and the feature's
# semver contract. Grep: `rg 'experimental-api|keymgr|_with_hsid'`.
arti-client = { version = "0.41", default-features = false, features = ["tokio", "rustls", "onion-service-client", "onion-service-service", "experimental-api"] }
```

Adjust the API-list inside the comment if `keymgr` is not actually behind `experimental-api` (it may be a different feature flag). Match what your `Cargo.lock` resolved — the comment's job is to be accurate, not guess.

- [ ] **Step 2: Verify (no code change, just docs)**

```bash
cargo build --workspace 2>&1 | tail -3
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: both clean.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "chore: document experimental-api usage (closes #46)

Add a grep-able comment near the arti-client workspace dep listing
the exact APIs we consume from experimental-api (launch_onion_service_with_hsid,
keymgr). On every Arti version bump, re-audit these usages and the
feature's semver contract."
```

---

## Task 4: `Daemon::run` wrapper + re-narrow transport visibility (closes #50)

**Goal:** Add a public `Daemon::run(data_dir, passphrase) -> Result<()>` that owns the full daemon startup flow. The CLI calls this instead of reaching into `transport::tor::*`. `transport` and its submodules return to `pub(crate)`.

**Files:** Modify `crates/core/src/daemon/state.rs`, `crates/core/src/lib.rs`, `crates/core/src/transport/mod.rs`, `crates/cli/src/main.rs`.

- [ ] **Step 1: Implement `Daemon::run` in `crates/core/src/daemon/state.rs`**

Open `crates/core/src/daemon/state.rs`. The current `Daemon` struct / `impl Daemon` is stub-level (Phase 1 will flesh out the full session manager). For Phase 0.C cleanup, add a minimal static `run` method owning the daemon startup flow.

Add (or merge into the existing `impl Daemon` block):

```rust
use std::path::Path;

use crate::error::Result;
use crate::identity::derive::derive_storage_seed;
use crate::identity::vault::Vault;
use crate::transport::tor::{TorConfig, TorRuntime};

impl Daemon {
    /// Run the Phase 0.C daemon: unlock the vault, derive the storage
    /// seed, bootstrap Tor, publish the onion service, signal readiness
    /// with the `.onion` address, then await a caller-supplied shutdown
    /// future. Returns `Ok(())` after a graceful shutdown.
    ///
    /// `ready` fires as soon as the onion is published — the caller can
    /// print the banner while this future continues to hold the runtime.
    ///
    /// This is the public entry point the CLI calls; subsequent phases
    /// extend it with the MLS session manager, outbox, mailbox poller,
    /// etc.
    pub async fn run(
        data_dir: &Path,
        passphrase: &zeroize::Zeroizing<String>,
        ready: tokio::sync::oneshot::Sender<String>,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let vault_path = data_dir.join("identity.vault");
        let (_vault, identity) = Vault::open(&vault_path, passphrase.as_str())?;
        let seed = derive_storage_seed(identity)?;

        let cfg = TorConfig {
            state_dir: data_dir.join("arti"),
            socks_port: None,
        };
        let mut rt = TorRuntime::bootstrap(cfg).await?;

        let hs_key_path = data_dir.join("hs.key.age");
        let onion = rt
            .publish_onion(&hs_key_path, &seed, "skattr-daemon")
            .await?;

        // If the receiver was dropped, there's no reader — that's fine,
        // proceed to listen until shutdown.
        let _ = ready.send(onion);

        shutdown.await;
        rt.shutdown().await?;
        Ok(())
    }
}
```

The `passphrase: &zeroize::Zeroizing<String>` signature keeps zeroize discipline across the Daemon boundary — callers pass their own `Zeroizing<String>` by reference.

**Visibility note.** `Daemon::run` uses `Vault::open` (public via `identity::Vault`), `derive_storage_seed` (public in `identity::derive`), and `TorConfig` / `TorRuntime` (currently `pub` but reverting to `pub(crate)` in Step 3). Since `daemon::state` is inside the `core` crate, `pub(crate)` access is fine.

- [ ] **Step 2: Rewrite the CLI daemon handler**

In `crates/cli/src/main.rs`, the existing `async fn daemon(detach: bool, data_dir_override: Option<&std::path::Path>)` imports `TorConfig` and `TorRuntime` directly. Rewrite to use `Daemon::run`:

```rust
async fn daemon(detach: bool, data_dir_override: Option<&std::path::Path>) -> Result<()> {
    use skattr_core::daemon::Daemon;

    if detach {
        anyhow::bail!("--detach is not yet supported; run in foreground for Phase 0.C");
    }

    let data_dir = effective_data_dir(data_dir_override)?;
    std::fs::create_dir_all(&data_dir)?;
    let vault_path = data_dir.join("identity.vault");

    if !vault_path.exists() {
        anyhow::bail!(
            "no identity vault at {}; run `skattr init` first",
            vault_path.display()
        );
    }

    let pw = read_passphrase("Vault passphrase: ")?;

    println!("Bootstrapping Tor…");
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let shutdown_fut = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    // Move the Zeroizing<String> passphrase by value into the spawned
    // task — it drops (and wipes) when Daemon::run returns.
    let data_dir_owned = data_dir.clone();
    let daemon_fut = tokio::spawn(async move {
        Daemon::run(&data_dir_owned, &pw, ready_tx, shutdown_fut).await
    });

    // Wait for the daemon to signal readiness.
    let onion = ready_rx
        .await
        .map_err(|_| anyhow::anyhow!("daemon exited before becoming ready"))?;
    println!();
    println!("Listening on: {onion}:1");
    println!("Ctrl-C to shut down.");

    // Block until the daemon future returns (SIGINT + graceful shutdown).
    daemon_fut
        .await
        .map_err(|e| anyhow::anyhow!("daemon join: {e}"))??;

    println!();
    println!("Shutdown complete.");
    Ok(())
}
```

Key differences from the pre-cleanup version:
- No direct `use skattr_core::transport::...` imports — only `daemon::Daemon`.
- `Daemon::run` is spawned on a separate task so we can await the ready-channel in the foreground, print the `.onion` banner, then block on the daemon future's completion.
- The `Zeroizing<String>` passphrase is moved into the spawned task and wipes on drop there.

- [ ] **Step 3: Re-narrow `transport` to `pub(crate)`**

In `crates/core/src/lib.rs`, change:

```rust
pub mod transport;
```

back to:

```rust
pub(crate) mod transport;
```

In `crates/core/src/transport/mod.rs`, change:

```rust
pub mod tor;
...
pub use listener::OnionListener;
```

back to:

```rust
pub(crate) mod tor;
...
pub(crate) use listener::OnionListener;
```

- [ ] **Step 4: Also re-narrow `Seed::from_storage_bytes`** if it's still `pub`

The Phase 0.C cleanup already re-narrowed this, but double-check. In `crates/core/src/identity/seed.rs`, confirm:

```rust
pub(crate) fn from_storage_bytes(bytes: [u8; 32]) -> Self { ... }
```

If it's still `pub`, change to `pub(crate)`.

- [ ] **Step 5: Fix the integration test**

`crates/tests/src/arti_echo.rs` currently imports from `skattr_core::transport::*`. After the re-narrowing in Step 3, those imports break. Choose one:

(a) **Extend `Daemon::run`'s API** to also expose enough primitives for a two-daemon integration test. But that widens the daemon public API just for tests — bad.

(b) **Move the integration test inside `skattr-core`** at `crates/core/tests/arti_echo.rs`. Integration tests at that path are inside the `skattr_core` crate for linking purposes and CAN reach `pub(crate)` items. This is the idiomatic Rust path.

Take option (b). Move the test file:

```bash
mv crates/tests/src/arti_echo.rs crates/core/tests/arti_echo.rs
```

Then fix imports in the new location:

```rust
use skattr_core::identity::Seed;
use skattr_core::transport::tor::{TorConfig, TorRuntime};
use skattr_core::transport::OnionListener;
```

Wait — `skattr_core::transport::tor::` requires `transport` to be `pub`. We just re-narrowed it.

**The Rust integration-test at `crates/core/tests/arti_echo.rs` runs as a SEPARATE crate** (each `tests/*.rs` file is its own test binary). It can only access `pub` API. So even at the `tests/` path, we can't reach `pub(crate)` items.

Ugh. Options:

(c) Keep `transport` as `pub` but mark it with `#[doc(hidden)]` — discourages external use but allows the integration test.

(d) Expose a test-only re-export module: `#[cfg(feature = "test-harness")] pub mod test_exports { pub use crate::transport::tor::*; pub use crate::transport::OnionListener; }` guarded by the `test-harness` feature (already declared in `Cargo.toml`).

(e) Rewrite the integration test to use ONLY `Daemon::run` — the test spawns two `Daemon::run`s, one acts as the server, the other dials. But `Daemon::run` doesn't expose `connect` or `OnionListener`... so this doesn't work either.

Option (d) is cleanest — it keeps the public API narrow for real users while letting our own tests reach the crate internals. Add to `crates/core/src/lib.rs`:

```rust
/// Re-exports for integration tests. Gated on the `test-harness`
/// feature, which CI enables for `cargo test --all-features`. Not
/// stable, not part of the public API.
#[cfg(feature = "test-harness")]
pub mod test_exports {
    pub use crate::transport::tor::{TorConfig, TorRuntime, TorStatus};
    pub use crate::transport::OnionListener;
}
```

And in `crates/tests/Cargo.toml`, change the `skattr-core` dep line:

```toml
skattr-core = { path = "../core", features = ["test-harness"] }
```

Then `crates/tests/src/arti_echo.rs` imports:

```rust
use skattr_core::identity::Seed;
use skattr_core::test_exports::{OnionListener, TorConfig, TorRuntime};
```

The test stays in `crates/tests/` (where it belongs as a cross-crate integration test), and the core crate's public API stays narrow.

- [ ] **Step 6: Verify**

```bash
cargo build --workspace 2>&1 | tail -5
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
cargo test --workspace --release 2>&1 | tail -5
```

Also spot-check that `transport` is not reachable from a generic dependent:

```bash
cargo doc -p skattr-core --no-deps 2>&1 | tail -5
# Open the generated docs; /transport/ should not appear at the crate root.
```

Expected: build clean, clippy clean, tests green. If the ignored two-daemon test doesn't compile after the imports change, adapt the test file — the `test_exports` module is the only path.

- [ ] **Step 7: Commit**

```bash
git add \
  crates/core/src/daemon/state.rs \
  crates/core/src/lib.rs \
  crates/core/src/transport/mod.rs \
  crates/core/src/identity/seed.rs \
  crates/cli/src/main.rs \
  crates/tests/Cargo.toml \
  crates/tests/src/arti_echo.rs
git commit -m "daemon: Daemon::run wrapper, re-narrow transport (closes #50)

Adds pub async Daemon::run(data_dir, &Zeroizing<String>, ready_tx,
shutdown_fut) -> Result<()> in daemon::state that owns the full
daemon startup flow: unlock vault → derive storage seed → Tor
bootstrap → publish onion → await shutdown. CLI's daemon handler
now calls it; no more direct reaches into transport::tor from the
CLI crate.

Re-narrows transport + its submodules + OnionListener re-export +
Seed::from_storage_bytes back to pub(crate), restoring the module
visibility discipline from CLAUDE.md.

Test exports for the cross-crate integration test are gated behind
the test-harness feature via pub mod test_exports in lib.rs; the
two-daemon echo test opts in via crates/tests/Cargo.toml's feature
selection."
```

---

## Post-plan wrap-up

- [ ] **Step 1: Verification gate**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --release
cd crates/core && cargo +nightly fuzz build vault_parser && cd ../..
```

All four must pass. Apply fmt + fmt-commit if any drift.

- [ ] **Step 2: Run the ignored integration test (network required)**

```bash
cargo test -p skattr-tests --release --features 'skattr-core/test-harness' -- --ignored two_daemons_echo_bytes_over_tor --nocapture
```

Expected: `test result: ok. 1 passed` after 3-10 min. If it fails on anything except the known "exit code 0 from tail" ambiguity, stop and fix.

- [ ] **Step 3: CHANGELOG + CLAUDE.md updates**

Append under `[Unreleased]` in `CHANGELOG.md`:

```markdown
- **Phase 0.C cleanup:** `Daemon::run` public wrapper owns the daemon startup flow; CLI no longer reaches into `transport::*`. Transport module + submodules + `OnionListener` re-export + `Seed::from_storage_bytes` re-narrowed to `pub(crate)`. `inject_hs_secret` probes the Arti keymgr before inserting (no more error-string matching). `arti_echo` tempdirs chmod'd to 0700 so the Phase 0 exit-criterion test actually runs. `experimental-api` audit comment added at the workspace dep site.
```

Commit:

```bash
git add CHANGELOG.md
git commit -m "changelog: Phase 0.C cleanup pass"
```

In `CLAUDE.md`, find the Repository-state paragraph and remove the sentence mentioning "transport was widened to pub"; replace with a sentence noting that transport is `pub(crate)` and the daemon-startup flow is owned by `Daemon::run`.

Commit:

```bash
git add CLAUDE.md
git commit -m "docs: CLAUDE.md — Phase 0.C cleanup complete"
```

- [ ] **Step 4: Mark follow-up tasks complete**

After merge, mark TaskList items #45, #46, #50, #53 as completed.

---

## Notes for the executing engineer

- **Task 1 is trivial and independent** — can be done first or last. Put it first so the exit-criterion test works when we run it at the end.
- **Task 2's keymgr probe API shape is the main uncertainty.** Arti's keymgr public surface may not expose a direct "slot exists?" lookup. If it doesn't, the fallback is to inspect the returned Err type structurally (downcast via source chain to a typed enum), still better than string-matching. Escalate if neither works.
- **Task 4 is the biggest and has the trickiest visibility dance** (`test_exports` module). Verify that `cargo doc` doesn't leak it into the advertised public API — `#[doc(hidden)]` on the `pub mod test_exports` is a belt-and-suspenders choice if the feature-gate alone isn't enough.
- **Release-mode `cargo test` is mandatory** for any run that involves Argon2 (transitively, anything that opens a Vault).
- **`cargo fmt --check` drift** has been the norm in every Phase 0.x plan — expect to apply fmt in a final commit. Budget one extra commit for it.
