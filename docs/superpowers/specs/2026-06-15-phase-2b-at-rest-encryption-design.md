# Phase 2.B — At-rest Encryption Lifecycle (Design)

**Date:** 2026-06-15
**Status:** Approved (brainstorming complete); plan to follow.
**Predecessors:** Phase 2.A (merged 2026-06-14), 2.C + 2.D (merged 2026-06-15).
**Source:** `docs/superpowers/specs/2026-06-13-phase-2-decomposition.md` §2.B;
audit item T1-2.
**Dependencies:** None — fully independent, storage-only.

The last open Phase 2 sub-project. Closes the at-rest-encryption gap: the
`age`-encrypt-on-shutdown path exists but is never reached, so plaintext
`skattr.sqlite` persists after every run and `export_backup` always fails.

**Wire-format / protocol NEUTRAL.** Storage-only; no `Command` /
`CommandResult` / `Event` / `Frame` / `ErrorCode` changes. No ADR required.

---

## Ground truth (verified against code 2026-06-15)

- `Pool::close(self)` (`storage/pool.rs:119`) drops the connection, encrypts
  `skattr.sqlite → skattr.sqlite.age`, removes the plaintext. But:
  - `Pool` lives behind `Arc` (`daemon/state.rs:132,530`), cloned to ~8
    subsystems (inbound, hub, accept loop, poll scheduler, handle, mailbox
    sweeper, retention sweep, log tap). `close(self)` consumes by value, so it
    is **never reachable** — nothing reclaims the sole owner. There is no
    `Drop`. Plaintext persists after every shutdown.
  - The DB runs in **WAL mode** (`apply_pragmas`, `journal_mode=WAL`).
    `close` does NOT `wal_checkpoint` before encrypting, and removes only
    `skattr.sqlite` — the `-wal` and `-shm` sidecars are left on disk
    (plaintext leak) and committed-but-uncheckpointed rows may be absent from
    the `.age`.
- Crash model comment (`pool.rs:21-24`): on a non-clean exit the plaintext
  remains; next `open` re-opens it directly (`encrypted_path.exists() &&
  !working_path.exists()` is false, so decrypt is skipped). No data loss, but
  the `.age` is stale/absent and plaintext lingers.
- `export_backup` (`storage/backup.rs:40`) tarballs three at-rest files —
  `identity.vault`, `hs.key.age`, `skattr.sqlite.age` — and fails if any is
  missing. It is invoked OFFLINE by the CLI `skattr backup` command
  (`cli/src/main.rs:550`). Because `close` never runs, `skattr.sqlite.age`
  never exists, so export always fails. The existing `backup.rs` tests use
  synthetic byte files, never a real `Pool`-produced `.age`.
- The three `Pool` accessors used by every repo are `with` / `with_mut` /
  `transaction` (all `pub(crate)`), plus `schema_version`, `open`, `close`,
  and the `#[cfg(any(test, feature="test-harness"))] in_memory()`.

## Locked decisions (brainstorming, 2026-06-15)

1. **Reachable close = `Arc::try_unwrap` in teardown + a `Drop` safety-net.**
   The happy path reclaims sole ownership and calls `close` with error
   handling/logging; a best-effort `Drop` encrypts if the explicit close
   didn't run (abnormal exit / lingering clone / future refactor).
2. **Crash residue = sentinel file + re-encrypt-on-boot.** A
   `skattr.sqlite.open` sentinel marks "plaintext DB live"; present-at-open
   means the prior run crashed → re-encrypt the residue to `.age` on boot,
   then continue on the live plaintext (re-creating the sentinel).

---

## Architecture — six units

### B1. Pool representation: `Mutex<Option<Connection>>` + lifecycle state

Changing `conn: Mutex<rusqlite::Connection>` →
`conn: Mutex<Option<rusqlite::Connection>>`, and adding:
- `persistent: bool` — `true` for `open`, `false` for `in_memory` (the Drop
  backstop must be a no-op for in-memory test pools).
- the existing `encrypted_path` / `working_path` / `passphrase`, plus a
  `sentinel_path` (`<data_dir>/skattr.sqlite.open`).

**Why `Option`:** a type that implements `Drop` cannot have its fields moved
out (`E0509`), but today `close` does `self.conn.into_inner()`. Holding the
connection as `Option` lets BOTH `close` and `Drop` `take()` the connection
out from behind the lock without moving the `Mutex` field — the standard
pattern. Connection close (drop) must happen before encryption so SQLite
releases the file + sidecars.

The three accessors change minimally:
```rust
pub(crate) fn with<F, R>(&self, f: F) -> Result<R>
where F: FnOnce(&rusqlite::Connection) -> Result<R> {
    let guard = self.conn.lock().map_err(|_| pool_poisoned())?;
    let conn = guard.as_ref().ok_or_else(|| pool_closed())?;
    f(conn)
}
```
(`with_mut` uses `as_mut`; `transaction` takes `as_mut` then `conn.transaction()`.)
`pool_closed()` is a typed `CoreError::Storage(StorageErrorKind::Other("pool
is closed"))`. Repos are UNTOUCHED — they only call the three accessors.

*Files:* `storage/pool.rs`.

### B2. WAL-safe close

A private helper `fn checkpoint_and_take(&self) -> Result<()>` (or inline in
`close`):
1. `conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")` (folds WAL into the
   main DB, truncates `-wal`) — under the lock, while the connection is still
   `Some`.
2. `self.conn.lock()?.take()` and drop the connection (releases the file +
   `-shm`).
3. `encrypt_db(working_path → encrypted_path)` (existing helper, atomic
   tmp+rename — unchanged).
4. Remove `skattr.sqlite`, `skattr.sqlite-wal`, `skattr.sqlite-shm` (each
   best-effort: ignore `NotFound`), and the sentinel.

`close(self)` returns `Result<()>` and sets internal state so the `Drop`
backstop (B4) knows the close already happened.

*Files:* `storage/pool.rs`.

### B3. Sentinel + re-encrypt-on-boot

`Pool::open`:
- Compute `sentinel_path = data_dir.join("skattr.sqlite.open")`.
- **Crash residue:** if `working_path.exists()` at entry (before any decrypt) —
  the clean path is `.age` present + `.sqlite` absent — the prior run did not
  close cleanly. Open the residue, `wal_checkpoint(TRUNCATE)`, and
  `encrypt_db(working_path → encrypted_path)` immediately so a current `.age`
  exists; leave the plaintext live for the session. (If `.age` is also absent —
  e.g. first run ever crashed before any close — encrypt produces the first
  `.age`.)
- **Clean path:** `encrypted_path.exists() && !working_path.exists()` → decrypt
  `.age → .sqlite` as today.
- After the connection is open + migrated, **create the sentinel**
  (`std::fs::write(&sentinel_path, b"")`), marking the plaintext live.
- A re-encrypt-on-boot failure is a fatal `CoreError` from `open` — do not
  proceed with an un-encryptable DB.

The sentinel is removed by `close` (B2) and by the `Drop` backstop (B4) after
a successful encrypt.

*Files:* `storage/pool.rs`.

### B4. Reachable teardown + Drop safety-net

**Teardown (`daemon/state.rs::run_with_transport`):** after `shutdown.await`
and after the existing task aborts (`accept_task`, `mailbox_sweeper_task`,
scheduler/sweep handles), drop every remaining `Arc<Pool>` clone in
deterministic order — the `DaemonHandle` (and the IPC executor holding it),
`hub`, `inbound`, `scheduler`, and any `*_pool` clones — so the only live
strong ref is the function-local `pool`. Then:
```rust
match std::sync::Arc::try_unwrap(pool) {
    Ok(p) => { if let Err(e) = p.close() { tracing::warn!(error = %e, "pool close failed"); } }
    Err(_still_shared) => {
        tracing::warn!("pool still shared at teardown; relying on Drop safety-net");
        // _still_shared drops here → Drop backstop encrypts when the last clone goes.
    }
}
```

**Drop safety-net (`impl Drop for Pool`):** best-effort, guarded:
```rust
impl Drop for Pool {
    fn drop(&mut self) {
        if !self.persistent { return; }                 // in-memory test pool
        if !self.working_path.exists() { return; }       // already closed/removed
        // checkpoint (if conn still Some), take+drop conn, encrypt, remove
        // plaintext + sidecars + sentinel — swallow + log errors (no Result/await).
    }
}
```
Because `close` removes `working_path`, a Pool that was cleanly closed has
nothing for Drop to do (the `working_path.exists()` guard short-circuits) — no
double-encrypt. The backstop only fires for a Pool dropped without `close`.

*Files:* `daemon/state.rs`, `storage/pool.rs`.

### B5. export_backup works

No change to `backup.rs` logic is anticipated — once B2/B4 produce a real
`skattr.sqlite.age` on clean shutdown, the offline `skattr backup` path
succeeds. The deliverable is a TEST proving it end-to-end through a real
`Pool` (the existing tests use synthetic bytes).

*Files:* `storage/backup.rs` (test only); `crates/core/src/storage/` test
module.

### B6. Integration guardrail (exit criterion)

A `run_with_transport`-driven test (standalone, over `LoopbackTransport`, NOT
folded into the bidirectional loopback harness — it must drive a CLEAN
shutdown and then inspect the data_dir): boot a daemon, write something, fire
the shutdown future, await `run_with_transport` to return, then assert the
data_dir has NO `skattr.sqlite` / `-wal` / `-shm` / `skattr.sqlite.open`, and
that `skattr.sqlite.age` exists and is decryptable (re-`Pool::open` succeeds
and reads the row back).

*Files:* `crates/tests/src/` (new), `crates/core/src/lib.rs` test_exports if a
clean-shutdown hook isn't already exported.

---

## Error handling

- Clean `close` returns `Result`; the teardown logs on failure (shutdown must
  not hang/panic on an encrypt error).
- The `Drop` backstop cannot return or await — it swallows and `tracing::warn!`s
  (static/error-string only, no secrets).
- Accessors on a closed pool return a typed `CoreError::Storage`, never panic.
- Re-encrypt-on-boot failure is fatal to `open` (returns `CoreError`).
- No `unwrap`/`expect` in non-test code; `Zeroizing` on the passphrase
  preserved; the seed-derived key + `HKDF(seed,"skattr-storage-v1")` unchanged.

## Security posture

- No plaintext DB or WAL/SHM sidecars on disk after a clean shutdown (T1-2
  closed). After a crash, the next boot re-encrypts within `open`, bounding how
  long any plaintext-only state goes without a current encrypted copy.
- Plaintext is necessarily live while the daemon runs — the at-rest window
  closes at shutdown, not during operation. The sentinel makes a crash
  detectable rather than silently re-using stale state.
- Storage-only: no wire/protocol surface; no metadata exposure change.

## Out of scope (unchanged deferrals)

- Docs-truthfulness follow-through (`passphrase-recovery.md`, `OPERATIONS.md`)
  — Phase 4.
- Re-keying the storage key on passphrase change — the age key derives from the
  BIP39 seed (not the passphrase), unchanged here (see 2.F's
  `change_passphrase` note).
- Live (running-daemon) on-demand backup snapshot — `export_backup` is offline;
  not needed for this exit criterion.

## Exit criteria

1. After a clean daemon shutdown, the data_dir contains NO `skattr.sqlite`,
   `skattr.sqlite-wal`, `skattr.sqlite-shm`, or `skattr.sqlite.open`, and a
   decryptable `skattr.sqlite.age` (re-open reads prior data back).
2. After a crash (plaintext residue + sentinel present), the next `Pool::open`
   re-encrypts to `.age` and continues without data loss; wrong-seed still
   fails to decrypt.
3. WAL is checkpointed before encryption (committed rows survive in the `.age`);
   sidecars are removed.
4. `export_backup` succeeds end-to-end through a real `Pool` (open → write →
   close → export → import → reopen → read).
5. The `Drop` safety-net encrypts a persistent pool dropped without `close`;
   it is a no-op for an in-memory pool and for an already-closed pool (no
   double-encrypt).
6. `cargo fmt --check`, `cargo clippy --workspace --exclude skattr-ui
   --all-targets --all-features -- -D warnings`, and the full core + tests
   suites (single-threaded) are green.

## Delivery model

`spec (this doc) → writing-plans → subagent-driven execution → two-stage
review per task → verification → finish branch`.
