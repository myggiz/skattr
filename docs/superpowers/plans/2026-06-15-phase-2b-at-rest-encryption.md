# Phase 2.B — At-rest Encryption Lifecycle — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make at-rest DB encryption actually happen — plaintext `skattr.sqlite` (+ `-wal`/`-shm`/sentinel) is gone after a clean shutdown and re-encrypted on the next boot after a crash; `export_backup` works.

**Architecture:** Hold the SQLite connection as `Mutex<Option<Connection>>` so both an explicit `Pool::close` and a best-effort `Drop` can `take()` it (no E0509 move-out-of-Drop). `close` checkpoints the WAL (TRUNCATE), encrypts the single file → `.age`, and removes plaintext + sidecars + sentinel. `run_with_transport` retains the owning `Arc<Pool>`, drains subsystem clones at teardown, and `Arc::try_unwrap` + `close`s it; a `Drop` safety-net (guarded on `persistent && working_path.exists()`) is the backstop. `Pool::open` re-encrypts crash residue (plaintext present at boot) and writes a sentinel marking the DB live.

**Tech Stack:** Rust 2021, `rusqlite` (bundled, WAL), `age` (scrypt passphrase), `zeroize`, `tempfile` (dev). Storage-only; wire-format neutral.

**Spec:** `docs/superpowers/specs/2026-06-15-phase-2b-at-rest-encryption-design.md`

---

## Conventions for every task

**Cargo isn't on PATH.** Prefix with `. "$HOME/.cargo/env" &&`.

**Per-task gates (run ALL before committing):**
```bash
. "$HOME/.cargo/env"
cargo fmt --all
cargo clippy --workspace --exclude skattr-ui --all-targets --all-features -- -D warnings
cargo test -p skattr-core --features test-harness
# Task 5 also: cargo test -p skattr-tests -- --test-threads=1
```

**Final gate (Task 5), single-threaded authoritative:**
```bash
. "$HOME/.cargo/env"
cargo fmt --all -- --check
cargo clippy --workspace --exclude skattr-ui --all-targets --all-features -- -D warnings
cargo test -p skattr-core --features test-harness
cargo test -p skattr-tests -- --test-threads=1
cargo build -p skattr-cli
```

**Hard rules:** GPLv3 header on new files; no `unwrap`/`expect` in non-test code (`?` + typed `CoreError`); `todo!()` not `unimplemented!()`; no pubkeys/onions/ciphertext/passphrase logged above `debug`; `Zeroizing` on the passphrase preserved; no wire/`Command`/`Event` changes. Every gate green before commit.

---

## File map

| File | Responsibility | Tasks |
|---|---|---|
| `crates/core/src/storage/pool.rs` | `Mutex<Option<Connection>>` repr, `persistent`/`sentinel_path` fields, accessor guards, WAL-safe `close`, re-encrypt-on-boot + sentinel in `open`, `Drop` safety-net | 1, 2, 3 |
| `crates/core/src/daemon/state.rs` | retain `pool` local + `Arc::try_unwrap` + `close` at teardown | 3 |
| `crates/core/src/storage/backup.rs` | end-to-end backup test through a real `Pool` (test only) | 4 |
| `crates/tests/src/at_rest_shutdown.rs` (new) | clean-shutdown integration guardrail | 5 |
| `crates/tests/src/lib.rs` | declare `mod at_rest_shutdown;` | 5 |

**Task order:** 1 (repr + WAL-safe close) → 2 (sentinel + re-encrypt-on-boot) → 3 (Drop + teardown) → 4 (backup test) → 5 (integration guardrail).

---

## Task 1: Pool repr → `Mutex<Option<Connection>>` + WAL-safe close

**Why:** `close(self)` does `self.conn.into_inner()` which blocks adding a `Drop` (E0509); it also doesn't checkpoint the WAL or remove the `-wal`/`-shm` sidecars (plaintext leak + possible data loss). This task changes the representation and makes `close` WAL-safe. (Sentinel/boot logic is Task 2; Drop is Task 3.)

**Files:**
- Modify: `crates/core/src/storage/pool.rs`
- Test: `pool.rs` `mod tests`

- [ ] **Step 1: Write the failing test** in `pool.rs` `mod tests`:

```rust
#[test]
fn close_checkpoints_wal_and_removes_all_plaintext_files() {
    let tmp = tempfile::tempdir().unwrap();
    let seed = Seed::generate().unwrap();
    let pool = Pool::open(tmp.path(), &seed).unwrap();
    pool.with_mut(|c| {
        c.execute(
            "INSERT INTO identity (id, public_key, created_at) VALUES (1, ?1, ?2)",
            rusqlite::params![&[0xAAu8; 32][..], 7i64],
        )
        .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))?;
        Ok(())
    })
    .unwrap();
    pool.close().unwrap();

    // No plaintext db OR WAL/SHM sidecars remain; encrypted file exists.
    assert!(!tmp.path().join("skattr.sqlite").exists());
    assert!(!tmp.path().join("skattr.sqlite-wal").exists());
    assert!(!tmp.path().join("skattr.sqlite-shm").exists());
    assert!(tmp.path().join("skattr.sqlite.age").exists());

    // Reopen reads the committed row back (WAL was checkpointed before encrypt).
    let pool = Pool::open(tmp.path(), &seed).unwrap();
    let ts: i64 = pool
        .with(|c| {
            c.query_row("SELECT created_at FROM identity WHERE id = 1", [], |r| r.get(0))
                .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))
        })
        .unwrap();
    assert_eq!(ts, 7);
    pool.close().unwrap();
}

#[test]
fn accessor_after_close_returns_typed_error() {
    // A pool whose connection has been taken (closed) must error, not panic.
    let pool = Pool::in_memory();
    pool.take_conn_for_test();
    let err = pool.with(|_| Ok(())).expect_err("closed pool must error");
    assert!(matches!(err, CoreError::Storage(_)));
}
```

> NOTE: `take_conn_for_test` is a `#[cfg(any(test, feature="test-harness"))]` helper added in Step 3 that does `let _ = self.conn.lock().map(|mut g| g.take());` — it lets the test drive the closed-accessor path without a full close.

- [ ] **Step 2: Run, verify failures** (compile error on `take_conn_for_test`, and the WAL test fails because sidecars aren't removed):
```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness close_checkpoints_wal_and_removes_all_plaintext_files
```
Expected: FAIL.

- [ ] **Step 3: Change the representation + accessors.** In `pool.rs`:

Struct:
```rust
pub struct Pool {
    conn: Mutex<Option<rusqlite::Connection>>,
    encrypted_path: PathBuf,
    working_path: PathBuf,
    /// Marker file (`<data_dir>/skattr.sqlite.open`) present while the
    /// plaintext DB is live; removed on clean close / Drop.
    sentinel_path: PathBuf,
    /// `false` for `in_memory()` test pools — the Drop backstop is a no-op.
    persistent: bool,
    passphrase: Zeroizing<String>,
}
```

Add a typed closed-error helper:
```rust
fn pool_closed() -> CoreError {
    CoreError::Storage(StorageErrorKind::Other("pool is closed".into()))
}
```

Accessors take the connection out of the `Option`:
```rust
pub(crate) fn with<F, R>(&self, f: F) -> Result<R>
where F: FnOnce(&rusqlite::Connection) -> Result<R> {
    let guard = self.conn.lock().map_err(|_| {
        CoreError::Storage(StorageErrorKind::Other("pool mutex poisoned".into()))
    })?;
    let conn = guard.as_ref().ok_or_else(pool_closed)?;
    f(conn)
}

pub(crate) fn with_mut<F, R>(&self, f: F) -> Result<R>
where F: FnOnce(&mut rusqlite::Connection) -> Result<R> {
    let mut guard = self.conn.lock().map_err(|_| {
        CoreError::Storage(StorageErrorKind::Other("pool mutex poisoned".into()))
    })?;
    let conn = guard.as_mut().ok_or_else(pool_closed)?;
    f(conn)
}

pub(crate) fn transaction<F, R>(&self, f: F) -> Result<R>
where F: FnOnce(&rusqlite::Transaction<'_>) -> Result<R> {
    let mut guard = self.conn.lock().map_err(|_| {
        CoreError::Storage(StorageErrorKind::Other("pool mutex poisoned".into()))
    })?;
    let conn = guard.as_mut().ok_or_else(pool_closed)?;
    let tx = conn.transaction()
        .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("begin tx: {e}"))))?;
    let result = f(&tx)?;
    tx.commit()
        .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("commit: {e}"))))?;
    Ok(result)
}
```

`open` wraps the connection in `Some` and sets the new fields (sentinel write + residue handling land in Task 2 — for now just wrap + set `persistent: true` and compute `sentinel_path`):
```rust
        Ok(Self {
            conn: Mutex::new(Some(conn)),
            encrypted_path,
            working_path,
            sentinel_path: data_dir.join("skattr.sqlite.open"),
            persistent: true,
            passphrase,
        })
```

`in_memory` sets `persistent: false`, `Some(conn)`, and `sentinel_path: PathBuf::from("/dev/null")`.

Add the test helper:
```rust
    #[cfg(any(test, feature = "test-harness"))]
    pub(crate) fn take_conn_for_test(&self) {
        if let Ok(mut g) = self.conn.lock() {
            let _ = g.take();
        }
    }
```

- [ ] **Step 4: Make `close` WAL-safe.** Replace `close`:
```rust
    /// Graceful shutdown: checkpoint the WAL into the main DB, close the
    /// connection, encrypt plaintext → ciphertext, and remove the plaintext
    /// DB, its WAL/SHM sidecars, and the sentinel.
    pub fn close(self) -> Result<()> {
        // Checkpoint while the connection is still live (folds the WAL into
        // the main file and truncates -wal), then take + drop the connection
        // so SQLite releases the files.
        {
            let mut guard = self.conn.lock().map_err(|_| {
                CoreError::Storage(StorageErrorKind::Other("pool mutex poisoned during close".into()))
            })?;
            if let Some(conn) = guard.as_ref() {
                conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("wal_checkpoint: {e}")))
                })?;
            }
            let _ = guard.take(); // drop the connection
        }

        encrypt_db(&self.working_path, &self.encrypted_path, &self.passphrase)?;
        remove_plaintext_artifacts(&self.working_path, &self.sentinel_path);
        Ok(())
        // `self` drops here; the Task-3 Drop sees working_path gone → no-op.
    }
```

Add the artifact-removal helper (best-effort; NotFound is fine):
```rust
fn wal_sidecars(working: &Path) -> [PathBuf; 2] {
    let mut wal = working.as_os_str().to_owned();
    wal.push("-wal");
    let mut shm = working.as_os_str().to_owned();
    shm.push("-shm");
    [PathBuf::from(wal), PathBuf::from(shm)]
}

fn remove_plaintext_artifacts(working: &Path, sentinel: &Path) {
    let _ = std::fs::remove_file(working);
    for sidecar in wal_sidecars(working) {
        let _ = std::fs::remove_file(sidecar);
    }
    let _ = std::fs::remove_file(sentinel);
}
```

- [ ] **Step 5: Run the tests, verify PASS:**
```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness pool::
```
Expected: PASS, including the existing `open_close_roundtrip_preserves_data`, `open_with_wrong_seed_fails`, `transaction_*`, `in_memory_*`, `schema_version_*` (all still green under the new repr).

- [ ] **Step 6: Per-task gates.** Expected: green. (The whole workspace must still compile — repos use only the three accessors, unchanged signatures.)

- [ ] **Step 7: Commit**
```bash
git add crates/core/src/storage/pool.rs
git commit -m "feat(2.B): Pool conn as Option + WAL-safe close (checkpoint + remove sidecars)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 2: Sentinel + re-encrypt-on-boot

**Why:** On a crash the plaintext residue persists and the `.age` is stale/absent. Detect residue at `open` and re-encrypt immediately; write a sentinel marking the DB live (removed on clean close, Task 1).

**Files:**
- Modify: `crates/core/src/storage/pool.rs` (`open`)
- Test: `pool.rs` `mod tests`

- [ ] **Step 1: Write the failing test:**
```rust
#[test]
fn crash_residue_is_reencrypted_on_open_and_sentinel_tracks_live_state() {
    let tmp = tempfile::tempdir().unwrap();
    let seed = Seed::generate().unwrap();

    // First clean session: write a row, close (produces .age, removes plaintext+sentinel).
    let pool = Pool::open(tmp.path(), &seed).unwrap();
    // sentinel exists while live
    assert!(tmp.path().join("skattr.sqlite.open").exists());
    pool.with_mut(|c| {
        c.execute(
            "INSERT INTO identity (id, public_key, created_at) VALUES (1, ?1, ?2)",
            rusqlite::params![&[0xBBu8; 32][..], 11i64],
        )
        .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))?;
        Ok(())
    })
    .unwrap();
    pool.close().unwrap();
    assert!(!tmp.path().join("skattr.sqlite.open").exists(), "sentinel removed on clean close");

    // Simulate a crash mid-session: open (decrypts), write more, then FORGET to
    // close — drop the working files in place by mem::forget so no Drop runs.
    {
        let pool = Pool::open(tmp.path(), &seed).unwrap();
        pool.with_mut(|c| {
            c.execute(
                "INSERT INTO identity (id, public_key, created_at) VALUES (2, ?1, ?2)",
                rusqlite::params![&[0xCCu8; 32][..], 22i64],
            )
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))?;
            // checkpoint so the second row is in the main file, mimicking a crash
            // that left a consistent plaintext db on disk.
            c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))?;
            Ok(())
        })
        .unwrap();
        std::mem::forget(pool); // crash: no close, no Drop; plaintext + sentinel remain
    }
    assert!(tmp.path().join("skattr.sqlite").exists(), "crash left plaintext residue");

    // Next boot: open detects residue and re-encrypts to .age; both rows survive.
    let pool = Pool::open(tmp.path(), &seed).unwrap();
    let n: i64 = pool
        .with(|c| {
            c.query_row("SELECT COUNT(*) FROM identity", [], |r| r.get(0))
                .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))
        })
        .unwrap();
    assert_eq!(n, 2, "both rows survive the crash + re-encrypt-on-boot");
    pool.close().unwrap();

    // The reopened+closed .age now reflects both rows.
    let pool = Pool::open(tmp.path(), &seed).unwrap();
    let n: i64 = pool
        .with(|c| {
            c.query_row("SELECT COUNT(*) FROM identity", [], |r| r.get(0))
                .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))
        })
        .unwrap();
    assert_eq!(n, 2);
    pool.close().unwrap();
}
```

> NOTE: `std::mem::forget(pool)` leaks the connection + skips Drop, faithfully simulating a crash that leaves plaintext on disk. This is test-only.

- [ ] **Step 2: Run, verify it fails** (residue path not implemented; the `mem::forget` leaves plaintext but the reopen currently skips decrypt AND doesn't re-encrypt — the COUNT assertion may pass via residue-reuse, but the sentinel-on-open assertion fails first). Confirm a red:
```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness crash_residue_is_reencrypted_on_open
```
Expected: FAIL (sentinel not written on open).

- [ ] **Step 3: Implement residue + sentinel in `open`.** Rewrite the body of `Pool::open` (keep the key derivation + connection open + pragmas + migrations):
```rust
    pub fn open(data_dir: &Path, seed: &Seed) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let encrypted_path = data_dir.join("skattr.sqlite.age");
        let working_path = data_dir.join("skattr.sqlite");
        let sentinel_path = data_dir.join("skattr.sqlite.open");

        let storage_key = hkdf_expand::<32>(seed.as_bytes(), INFO_STORAGE_V1)?;
        let passphrase = Zeroizing::new(hex::encode(storage_key.as_ref()));

        // Crash residue: plaintext present before we touch anything. The clean
        // path is `.age` present + `.sqlite` absent, so a pre-existing `.sqlite`
        // means the prior run did not close cleanly.
        let residue = working_path.exists();

        if encrypted_path.exists() && !working_path.exists() {
            decrypt_db(&encrypted_path, &working_path, &passphrase)?;
        }

        let mut conn = rusqlite::Connection::open(&working_path).map_err(|e| {
            CoreError::Storage(StorageErrorKind::Other(format!("open sqlite: {e}")))
        })?;
        apply_pragmas(&conn)?;
        crate::storage::migrations::apply(&mut conn)?;

        // Re-encrypt-on-boot: a current `.age` always exists after a crash. The
        // plaintext stays live for this session (re-encrypted again at close).
        if residue {
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("boot checkpoint: {e}")))
            })?;
            encrypt_db(&working_path, &encrypted_path, &passphrase)?;
            tracing::warn!("recovered crash residue: re-encrypted storage DB on boot");
        }

        // Mark the plaintext DB live (removed by close / Drop).
        std::fs::write(&sentinel_path, b"").map_err(|e| {
            CoreError::Storage(StorageErrorKind::Other(format!("write sentinel: {e}")))
        })?;

        Ok(Self {
            conn: Mutex::new(Some(conn)),
            encrypted_path,
            working_path,
            sentinel_path,
            persistent: true,
            passphrase,
        })
    }
```

- [ ] **Step 4: Run the test, verify PASS:**
```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness crash_residue_is_reencrypted_on_open
```
Expected: PASS. Existing `open_*` / `schema_version_*` tests still pass (they go through clean open→close; the sentinel is created + removed transparently).

- [ ] **Step 5: Per-task gates.** Expected: green.

- [ ] **Step 6: Commit**
```bash
git add crates/core/src/storage/pool.rs
git commit -m "feat(2.B): sentinel + re-encrypt crash residue on Pool::open

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 3: Drop safety-net + reachable teardown close

**Why:** The explicit `close` is only reachable if teardown reclaims sole ownership. A `Drop` backstop guarantees encryption even if `close` doesn't run (abnormal exit / a lingering Arc clone).

**Files:**
- Modify: `crates/core/src/storage/pool.rs` (`impl Drop`)
- Modify: `crates/core/src/daemon/state.rs` (`run_with_transport` teardown)
- Test: `pool.rs` `mod tests`

- [ ] **Step 1: Write the failing Drop tests:**
```rust
#[test]
fn drop_without_close_encrypts_persistent_pool() {
    let tmp = tempfile::tempdir().unwrap();
    let seed = Seed::generate().unwrap();
    {
        let pool = Pool::open(tmp.path(), &seed).unwrap();
        pool.with_mut(|c| {
            c.execute(
                "INSERT INTO identity (id, public_key, created_at) VALUES (1, ?1, ?2)",
                rusqlite::params![&[0xDDu8; 32][..], 33i64],
            )
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))?;
            Ok(())
        })
        .unwrap();
        // No close(): pool drops at end of scope → Drop safety-net runs.
    }
    assert!(!tmp.path().join("skattr.sqlite").exists(), "Drop removed plaintext");
    assert!(!tmp.path().join("skattr.sqlite.open").exists(), "Drop removed sentinel");
    assert!(tmp.path().join("skattr.sqlite.age").exists(), "Drop encrypted to .age");
    // .age is decryptable + has the row.
    let pool = Pool::open(tmp.path(), &seed).unwrap();
    let ts: i64 = pool
        .with(|c| {
            c.query_row("SELECT created_at FROM identity WHERE id = 1", [], |r| r.get(0))
                .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))
        })
        .unwrap();
    assert_eq!(ts, 33);
    pool.close().unwrap();
}

#[test]
fn drop_after_close_is_noop_no_double_encrypt() {
    let tmp = tempfile::tempdir().unwrap();
    let seed = Seed::generate().unwrap();
    let pool = Pool::open(tmp.path(), &seed).unwrap();
    pool.close().unwrap(); // removes working_path; self drops → Drop sees it gone.
    assert!(tmp.path().join("skattr.sqlite.age").exists());
    // Reopen still works (the .age wasn't corrupted by a double-encrypt).
    Pool::open(tmp.path(), &seed).unwrap().close().unwrap();
}

#[test]
fn in_memory_drop_is_noop() {
    // Must not attempt to encrypt /dev/null on drop.
    let pool = Pool::in_memory();
    drop(pool); // no panic, no file ops
}
```

- [ ] **Step 2: Run, verify the Drop test fails** (no Drop impl → plaintext remains):
```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness drop_without_close_encrypts_persistent_pool
```
Expected: FAIL.

- [ ] **Step 3: Add the Drop safety-net** in `pool.rs`:
```rust
impl Drop for Pool {
    fn drop(&mut self) {
        // In-memory test pools: never touch the filesystem.
        if !self.persistent {
            return;
        }
        // A clean close() already removed the plaintext → nothing to do
        // (prevents a double-encrypt after the explicit close).
        if !self.working_path.exists() {
            return;
        }
        // Best-effort: checkpoint (if the conn is still live), drop it, encrypt,
        // remove plaintext + sidecars + sentinel. Errors are logged, not returned.
        if let Ok(mut guard) = self.conn.lock() {
            if let Some(conn) = guard.as_ref() {
                let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
            }
            let _ = guard.take();
        }
        match encrypt_db(&self.working_path, &self.encrypted_path, &self.passphrase) {
            Ok(()) => remove_plaintext_artifacts(&self.working_path, &self.sentinel_path),
            Err(e) => tracing::warn!(error = %e, "Pool::drop encrypt failed; plaintext retained"),
        }
    }
}
```

> NOTE on `close` vs `Drop`: `close(self)` already removed `working_path`, so when `self` drops at the end of `close`, the `!self.working_path.exists()` guard returns early — exactly one encrypt. `close` takes `self` by value and never moves a field out (it `take()`s from inside the `Mutex`), so it coexists with `Drop` (no E0509).

- [ ] **Step 4: Run the Drop tests, verify PASS:**
```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness drop_
```
Expected: all three PASS.

- [ ] **Step 5: Wire reachable teardown in `state.rs`.** In `run_with_transport`:

(a) Keep the owning `pool` local — change the handle construction (`state.rs:383-390`) so it gets a CLONE, not the moved original:
```rust
    let mut handle = DaemonHandle::<T::Stream>::new_with_mailbox(
        pool.clone(),
        hub,
        identity,
        events_tx.clone(),
        mailbox_factory,
        poller_ctrl,
    );
```

(b) At the END of teardown (after `transport.shutdown().await?`, replacing the bare `Ok(())` at `state.rs:489`), drain the remaining local Pool-holders and reclaim:
```rust
    transport.shutdown().await?;
    // Server::drop removes the socket file automatically.

    // At-rest encryption (2.B): drop the remaining strong refs to the Pool so
    // the owning `pool` is sole, then reclaim it and close (checkpoint WAL,
    // encrypt → .age, remove plaintext + sidecars + sentinel). The Drop
    // safety-net on Pool is the backstop if a clone still lingers.
    drop(handle);
    drop(inbound);
    match std::sync::Arc::try_unwrap(pool) {
        Ok(p) => {
            if let Err(e) = p.close() {
                tracing::warn!(error = %e, "pool close at shutdown failed");
            }
        }
        Err(_still_shared) => {
            tracing::warn!("pool still shared at shutdown; relying on Drop safety-net");
            // `_still_shared` drops here; Pool::drop encrypts when the last clone goes.
        }
    }
    Ok(())
```

> NOTE TO IMPLEMENTER: confirm by reading `state.rs:300-490` which local bindings still hold an `Arc<Pool>` (or an `Arc` that transitively holds one) at this point. After the existing aborts/awaits (`ipc_task`, `tor_tap_task`, `sweep_handle`, `drop(scheduler)`, `accept_task`, `mailbox_sweeper_task`), the locals holding a Pool clone are `handle` (now via `pool.clone()`) and `inbound` (`Arc<dyn InboundDispatch>` wrapping a `DaemonInbound` that holds `pool.clone()`). `hub` was moved into `handle`. If you find any OTHER live local holding a clone, `drop` it before `try_unwrap`. Even if one is missed, the `Drop` safety-net still encrypts when the function returns (all Pool clones are function-local) — `try_unwrap` is the clean error-surfacing path, `Drop` is the guarantee. Do NOT add the teardown to `run_loopback` separately if it delegates to `run_with_transport` (confirm; it does).

- [ ] **Step 6: Run core tests, verify nothing regressed:**
```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness
```
Expected: all green (the teardown change is exercised end-to-end by Task 5's guardrail; here just confirm compilation + no unit regressions).

- [ ] **Step 7: Per-task gates.** Expected: green.

- [ ] **Step 8: Commit**
```bash
git add crates/core/src/storage/pool.rs crates/core/src/daemon/state.rs
git commit -m "feat(2.B): Pool Drop safety-net + reachable try_unwrap close at daemon teardown

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 4: export_backup works end-to-end through a real Pool

**Why:** `export_backup` needs a real `skattr.sqlite.age`. The existing tests use synthetic byte files; prove the real path now works.

**Files:**
- Modify: `crates/core/src/storage/backup.rs` (`mod tests`)
- Test: `backup.rs` `mod tests`

- [ ] **Step 1: Write the test** (in `backup.rs` `mod tests`):
```rust
#[test]
fn export_import_roundtrip_through_real_pool() {
    use crate::storage::Pool;

    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let seed = Seed::generate().unwrap();

    // Real Pool: open, write, close → produces a real skattr.sqlite.age.
    let pool = Pool::open(src.path(), &seed).unwrap();
    pool.with_mut(|c| {
        c.execute(
            "INSERT INTO identity (id, public_key, created_at) VALUES (1, ?1, ?2)",
            rusqlite::params![&[0xEEu8; 32][..], 55i64],
        )
        .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))?;
        Ok(())
    })
    .unwrap();
    pool.close().unwrap();

    // The other two backup inputs must exist for export to succeed.
    std::fs::write(src.path().join("identity.vault"), b"vault").unwrap();
    std::fs::write(src.path().join("hs.key.age"), b"hskey").unwrap();

    let archive = src.path().join("backup.age");
    export_backup(src.path(), &archive, &seed).unwrap();
    import_backup(&archive, dst.path(), &seed).unwrap();

    // The restored DB reopens with the seed and reads the row back.
    let pool = Pool::open(dst.path(), &seed).unwrap();
    let ts: i64 = pool
        .with(|c| {
            c.query_row("SELECT created_at FROM identity WHERE id = 1", [], |r| r.get(0))
                .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))
        })
        .unwrap();
    assert_eq!(ts, 55);
    pool.close().unwrap();
}
```

> NOTE: `import_backup` extracts `skattr.sqlite.age` into `dst`; `Pool::open(dst, seed)` then decrypts it (clean path: `.age` present, `.sqlite` absent). This proves the real `.age` produced by `close` is a valid backup input AND restores to a working DB. `Seed`/`Pool` imports: `Seed` is already used in this module's tests; add `use crate::storage::Pool;` inside the test.

- [ ] **Step 2: Run, verify PASS:**
```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness export_import_roundtrip_through_real_pool
```
Expected: PASS (it was impossible before — `close` never produced a `.age`).

- [ ] **Step 3: Per-task gates.** Expected: green.

- [ ] **Step 4: Commit**
```bash
git add crates/core/src/storage/backup.rs
git commit -m "test(2.B): export/import backup round-trips through a real Pool-produced .age

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 5: Clean-shutdown integration guardrail + final verification

**Why:** The exit criterion — prove that a real `run_with_transport` daemon, cleanly shut down, leaves no plaintext and a decryptable `.age`.

**Files:**
- Create: `crates/tests/src/at_rest_shutdown.rs`
- Modify: `crates/tests/src/lib.rs` (`mod at_rest_shutdown;`)

- [ ] **Step 1: Read first** — `crates/tests/src/daemon_run_direct.rs` and `crates/tests/src/loopback_harness.rs` for how a daemon is booted via the `test_exports` loopback entrypoint and how a clean shutdown is driven (the shutdown future + awaiting the run future). Identify the exact `test_exports` function (e.g. `run_loopback`) and how to signal + await a clean shutdown. Also check `crates/core/src/lib.rs` `test_exports` for what's exported.

- [ ] **Step 2: Write the guardrail test** in `crates/tests/src/at_rest_shutdown.rs`:
```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 2.B exit criterion: a cleanly shut-down daemon leaves no plaintext
//! storage DB (or WAL/SHM/sentinel) on disk, only a decryptable .age.

#[tokio::test]
async fn clean_shutdown_leaves_only_encrypted_db() {
    // 1. Boot a daemon over LoopbackTransport via the test_exports entrypoint,
    //    in a tempdir data_dir, with a shutdown oneshot.
    // 2. Wait for readiness, optionally drive a trivial op.
    // 3. Fire the shutdown future; await the run future to return Ok.
    // 4. Assert the data_dir has NO skattr.sqlite / -wal / -shm / skattr.sqlite.open,
    //    and that skattr.sqlite.age EXISTS.
    // 5. Re-open a Pool on the data_dir with the same seed and read back —
    //    proving the .age is valid (clean decrypt path).
}
```

> NOTE TO IMPLEMENTER: model the boot/shutdown scaffolding on `daemon_run_direct.rs` exactly (it already boots `run_loopback` daemons with a shutdown signal and awaits them). You need only ONE daemon here. After the run future returns, assert on the data_dir files, then `skattr_core` `Pool::open(data_dir, seed).close()` round-trips (the seed is the one the harness derives — reuse the harness's seed/vault setup). If reconstructing the seed in the test is awkward, assert the file-level invariants (no plaintext/sidecars/sentinel; `.age` present and non-empty) which already prove the exit criterion; the decrypt-round-trip is covered by Task 1/4. Keep it deterministic (await the run future; no fixed sleeps). If the boot/shutdown plumbing isn't cleanly reachable from `crates/tests`, ESCALATE rather than forcing it.

Declare `mod at_rest_shutdown;` in `crates/tests/src/lib.rs` (match the existing style/placement).

- [ ] **Step 3: Run it, verify PASS:**
```bash
. "$HOME/.cargo/env" && cargo test -p skattr-tests clean_shutdown_leaves_only_encrypted_db -- --test-threads=1 --nocapture
```
Expected: PASS.

- [ ] **Step 4: FULL final gate** (CI-parity, single-threaded authoritative):
```bash
. "$HOME/.cargo/env"
cargo fmt --all -- --check
cargo clippy --workspace --exclude skattr-ui --all-targets --all-features -- -D warnings
cargo test -p skattr-core --features test-harness
cargo test -p skattr-tests -- --test-threads=1
cargo build -p skattr-cli
```
Expected: fmt clean; clippy clean; core green (incl. all new pool/backup tests); `skattr-tests` all non-ignored green incl. `clean_shutdown_leaves_only_encrypted_db` AND the pre-existing loopback guardrails (`two_daemons_exchange_messages_both_directions_over_loopback`, `first_contact_invite_add_then_bidirectional_over_loopback`) + 2.C/2.D guardrails (regression check — the teardown change must not break the existing loopback shutdowns). CLI builds.

- [ ] **Step 5: Commit**
```bash
git add crates/tests/src/at_rest_shutdown.rs crates/tests/src/lib.rs
git commit -m "test(2.B): clean-shutdown guardrail — no plaintext DB, decryptable .age

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Self-review (against the spec)

**Spec coverage:**
- B1 repr (`Mutex<Option<Connection>>` + persistent + sentinel_path + accessor guards) → Task 1 ✓
- B2 WAL-safe close (checkpoint TRUNCATE + remove .sqlite/-wal/-shm/sentinel) → Task 1 ✓
- B3 sentinel + re-encrypt-on-boot → Task 2 ✓
- B4 reachable teardown (`try_unwrap` + close) + Drop safety-net → Task 3 ✓
- B5 export_backup works (real-Pool test) → Task 4 ✓
- B6 clean-shutdown integration guardrail → Task 5 ✓
- Exit criteria 1 (no plaintext/sidecars/sentinel + decryptable .age) → Tasks 1+5; 2 (crash re-encrypt) → Task 2; 3 (WAL checkpoint) → Task 1; 4 (export) → Task 4; 5 (Drop no-double-encrypt + in-memory no-op) → Task 3; 6 (gates) → Task 5 ✓

**Type/signature consistency:** `Pool` fields (`conn: Mutex<Option<Connection>>`, `persistent`, `sentinel_path`) are defined in Task 1 and used consistently in Tasks 2 (open sets them) and 3 (Drop reads `persistent`/`working_path`/`sentinel_path`). Helpers `pool_closed`, `wal_sidecars`, `remove_plaintext_artifacts`, `take_conn_for_test` are each defined in Task 1 and reused. `close(self)` signature unchanged (callers: tests + Task 3 teardown). `Pool::open`/`in_memory` signatures unchanged (repos + all existing callers unaffected). The teardown uses `pool.clone()` into the handle + `Arc::try_unwrap(pool)` — consistent with keeping `pool` local.

**Placeholder scan:** no TBD/TODO; every code step shows real code. The two IMPLEMENTER notes (Task 3 teardown clone-enumeration; Task 5 harness reuse + escalation) point at concrete files to read and a fallback assertion path — deliberate, since they depend on re-confirming the live-binding set / harness surface rather than inventing API.

**Security invariants:** no non-`close`/`Drop` path encrypts; `close` removes working_path so Drop never double-encrypts; in-memory pools never touch the FS (persistent guard); `Zeroizing` passphrase preserved; the `tracing::warn!` lines carry only static/error-string text (no secrets); storage-only, wire-format neutral.
