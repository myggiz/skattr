# Phase 0.D — Storage Layer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the `todo!()` stubs in `crates/core/src/storage/` with a working `Pool`, a migrations runner, seven typed repositories (contacts, onion_addresses, messages, mls_groups, outbox, mailboxes, seen_messages), a transactions wrapper, and a `skattr backup` / `skattr restore-backup` CLI pair that produces a portable tarball — all without any MLS/session-manager wiring (that's Phase 1).

**Architecture:** SQLite via `rusqlite` with app-level file encryption (`age`). On `Pool::open`, decrypt `skattr.sqlite.age` → `skattr.sqlite` (plaintext working file under `<data_dir>`), open a single writer `rusqlite::Connection` behind a `Mutex`, apply pragmas (`foreign_keys=ON`, `journal_mode=WAL`, `synchronous=NORMAL`), run migrations idempotently via a `schema_version` table. Repos are thin wrappers that call into the pool with a closure. On `Pool::close`, re-encrypt + remove the plaintext file. Backup bundles the three encrypted at-rest files (`identity.vault`, `skattr.sqlite.age`, `hs.key.age`) into a tarball that is itself `age`-encrypted under `HKDF(seed, "skattr-backup-v1")` — a third domain-separated label alongside the storage/HS keys.

**Tech Stack:**
- `rusqlite` 0.38 (bundled SQLite, pinned below arti's 0.39 ceiling).
- `age` 0.11 — at-rest wrapper around the DB file and the backup archive.
- `hkdf` 0.13 + `sha2` 0.10 — domain-separated key derivation (new `INFO_BACKUP_V1` label).
- `tar` — archive format for the backup bundle (new dep; tiny, pure-Rust).
- `flate2` — gzip compression (new dep; optional, see Task 10).
- Existing workspace primitives: `zeroize`, `tracing`, `thiserror`, `ciborium`, `clap`.

**Exit criteria:**
- `cargo fmt --all --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace --release` all green.
- `crates/core/src/storage/` has zero `todo!()`.
- Unit tests per repo pass on an in-memory SQLite.
- Integration test: open a Pool with a fresh seed, write a contact + a message, close, reopen with the same seed, read them back unchanged.
- Integration test: `Pool::open` with the wrong seed fails cleanly (typed error, no panic).
- `skattr backup <file>` produces a tarball; `skattr restore-backup <file>` on a clean `--data-dir` reconstructs a working identity.
- Phase 0 follow-up: the Phase 0.C daemon still starts without regression.

---

## File structure

```
crates/core/src/storage/
├── mod.rs              MODIFY: add `pub(crate) mod backup;`, `pub(crate) mod migrations;`, `pub(crate) mod seen_messages;`
├── pool.rs             MODIFY: real Pool — encrypted open/close, connection Mutex, transaction helper
├── migrations.rs       CREATE: include_str!'d SQL, schema_version table, apply loop
├── migrations/
│   └── 0001_init.sql   UNCHANGED (already scaffolded)
├── contacts.rs         MODIFY: ContactRepo (upsert/get/list/remove) + OnionAddressRepo on same struct
├── messages.rs         MODIFY: MessageRepo (insert/recent) — FTS5 search deferred to Phase 1
├── groups.rs           MODIFY: MlsGroupRepo (put/get/list)
├── outbox.rs           MODIFY: OutboxRepo (insert/delete/due/reschedule)
├── mailboxes.rs        MODIFY: MailboxRepo (insert/list/remove)
├── seen_messages.rs    CREATE: SeenMessagesRepo (insert/contains/sweep_older_than)
└── backup.rs           CREATE: export_backup / import_backup functions

crates/core/src/identity/
└── derive.rs           MODIFY: add INFO_BACKUP_V1 label

Cargo.toml              MODIFY: add `tar` + `flate2` workspace deps
crates/core/Cargo.toml  MODIFY: consume tar + flate2

crates/cli/src/
└── main.rs             MODIFY: add `backup <file>` + `restore-backup <file>` subcommands
```

`mls/`, `transport/`, `daemon/state.rs`, `identity/vault.rs`, `identity/key.rs` — all untouched.

---

## Pre-flight

```bash
cd /home/myggiz/development/skattr
. "$HOME/.cargo/env"

cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --release

git worktree add ../skattr-phase-0d-storage -b phase-0d-storage
cd ../skattr-phase-0d-storage
cargo build --workspace
```

All gates green. Subsequent tasks assume `/home/myggiz/development/skattr-phase-0d-storage`.

---

## Task 1: Add workspace deps + `INFO_BACKUP_V1` label

**Goal:** Bring in `tar` + `flate2` at workspace level and register a new HKDF label for the backup path. Trivial setup.

**Files:** Modify `Cargo.toml`, `crates/core/Cargo.toml`, `crates/core/src/identity/derive.rs`.

- [ ] **Step 1: Add workspace deps**

In `Cargo.toml` at the repo root, append under `[workspace.dependencies]`:

```toml
tar = "0.4"
flate2 = "1"
```

- [ ] **Step 2: Consume the deps in `core`**

In `crates/core/Cargo.toml`, under `[dependencies]`:

```toml
tar = { workspace = true }
flate2 = { workspace = true }
```

- [ ] **Step 3: Add the HKDF label**

In `crates/core/src/identity/derive.rs`, append after the existing `INFO_*` constants:

```rust
/// Backup archive at-rest encryption:
/// `HKDF(storage_seed, "skattr-backup-v1")`.
pub const INFO_BACKUP_V1: &[u8] = b"skattr-backup-v1";
```

- [ ] **Step 4: Verify**

```bash
cargo build --workspace 2>&1 | tail -3
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: both clean.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/core/Cargo.toml crates/core/src/identity/derive.rs
git commit -m "storage: add tar + flate2 deps + INFO_BACKUP_V1 label

Preparation for Phase 0.D. tar + flate2 are needed by the backup
export/import path (Task 10). INFO_BACKUP_V1 is the domain-
separation label for the backup archive's outer age encryption,
derived from the storage seed via HKDF."
```

---

## Task 2: Migrations runner

**Goal:** A `migrations::apply(conn)` helper that runs any pending SQL migrations against a `rusqlite::Connection`, keyed off a `schema_version` table. Idempotent — re-running does nothing.

**Files:** Create `crates/core/src/storage/migrations.rs`, modify `crates/core/src/storage/mod.rs`.

- [ ] **Step 1: Register the module**

In `crates/core/src/storage/mod.rs`, under the existing `pub(crate) mod ...` block, add:

```rust
pub(crate) mod migrations;
```

Keep the existing `MIGRATION_0001_INIT` constant that's already there — we'll move it into the new module.

- [ ] **Step 2: Write the migrations module**

Create `crates/core/src/storage/migrations.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Schema-migration runner.
//!
//! Each migration is a `&str` of SQL (via `include_str!`), paired with a
//! monotonic version number. On open, we consult `schema_version` (a
//! single-row bookkeeping table) and run every migration whose version
//! is greater than the current one.
//!
//! This design is simpler than `refinery` or `sqlx::migrate!` and has
//! zero extra dependencies. If we ever need rollback support or
//! transactional migrations across files, revisit at Phase 1.

use crate::error::Result;

/// A single migration: a monotonic version number and its SQL text.
struct Migration {
    version: u32,
    sql: &'static str,
}

const ALL_MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: include_str!("migrations/0001_init.sql"),
}];

/// Apply all pending migrations in order. Idempotent — re-running does
/// nothing if `schema_version` is already at the latest version.
///
/// The caller opens the Connection and sets pragmas before calling us;
/// we run the migration SQL and update `schema_version`.
pub(crate) fn apply(conn: &mut rusqlite::Connection) -> Result<()> {
    // Ensure schema_version exists. 0001_init.sql creates this table
    // too; running CREATE TABLE IF NOT EXISTS here is a no-op after the
    // first migration but handles fresh databases on first open.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY)",
        [],
    )?;

    let current: u32 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    for m in ALL_MIGRATIONS {
        if m.version <= current {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(m.sql)?;
        tx.execute(
            "INSERT OR REPLACE INTO schema_version (version) VALUES (?1)",
            [m.version],
        )?;
        tx.commit()?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_db_runs_migrations_to_v1() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        apply(&mut conn).unwrap();
        let v: u32 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 1);
    }

    #[test]
    fn re_applying_is_idempotent() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        apply(&mut conn).unwrap();
        apply(&mut conn).unwrap();
        apply(&mut conn).unwrap();
        // Row count in schema_version should still be 1.
        let rows: u32 = conn
            .query_row("SELECT COUNT(*) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn migration_creates_expected_tables() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        apply(&mut conn).unwrap();
        for table in [
            "identity",
            "contacts",
            "onion_addresses",
            "mls_groups",
            "messages",
            "outbox",
            "mailboxes",
            "seen_messages",
        ] {
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(exists, 1, "table {table} must exist after migration");
        }
    }
}
```

Also, in `crates/core/src/storage/mod.rs`, remove the `pub(crate) const MIGRATION_0001_INIT: &str = ...` line — it's now folded into the migrations module.

- [ ] **Step 3: Verify**

```bash
cargo test -p skattr-core --lib storage::migrations --release 2>&1 | tail -5
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: 3 passed, clippy clean.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/storage/mod.rs crates/core/src/storage/migrations.rs
git commit -m "storage: migrations runner with schema_version bookkeeping

Single-function `apply(&mut Connection)` that consults the
schema_version table and runs any pending migrations. Idempotent;
re-running is a no-op. Zero extra dependencies — `refinery` is fine
if we ever need it, but in-crate include_str! is simpler for Phase 0.

Three unit tests: fresh DB → v1; idempotency; all expected tables
exist after migration."
```

---

## Task 3: Pool with encrypted open/close

**Goal:** Replace the `todo!()` `Pool::open` stub with a real implementation that age-decrypts `skattr.sqlite.age` into a working `skattr.sqlite` file, opens rusqlite, applies pragmas, runs migrations, and exposes `with` / `transaction` helpers. `Pool::close` re-encrypts and removes the plaintext.

**Files:** Modify `crates/core/src/storage/pool.rs`.

- [ ] **Step 1: Rewrite `pool.rs`**

Replace the contents of `crates/core/src/storage/pool.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! SQLite connection pool with at-rest encryption via `age`.
//!
//! Lifecycle:
//!
//! 1. `Pool::open(data_dir, seed)`:
//!    - If `<data_dir>/skattr.sqlite.age` exists and
//!      `<data_dir>/skattr.sqlite` does not, decrypt .age → .sqlite.
//!    - Open a `rusqlite::Connection` on the plaintext file.
//!    - Apply pragmas: foreign_keys=ON, journal_mode=WAL, synchronous=NORMAL.
//!    - Run migrations.
//!    - Wrap the Connection in a `Mutex`.
//! 2. Queries via `pool.with(|c| { ... })` or `pool.transaction(|tx| { ... })`.
//! 3. `Pool::close(self)`: drop the Connection, encrypt plaintext → .age,
//!    remove plaintext file.
//!
//! Crash model: if the process dies without `Pool::close`, the plaintext
//! `skattr.sqlite` remains on disk. Next startup re-opens it directly
//! (skipping decrypt) and continues — no data loss, but the at-rest
//! window is wider. Phase 1 should add a sync-on-checkpoint path.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use zeroize::Zeroizing;

use crate::error::{CoreError, Result};
use crate::identity::derive::{hkdf_expand, INFO_STORAGE_V1};
use crate::identity::Seed;

/// SQLite connection pool. Single writer, WAL mode.
pub(crate) struct Pool {
    conn: Mutex<rusqlite::Connection>,
    encrypted_path: PathBuf,
    working_path: PathBuf,
    /// Age passphrase (hex of the HKDF output). Held by the pool so
    /// `close()` can re-encrypt without re-deriving.
    passphrase: Zeroizing<String>,
}

impl Pool {
    /// Open (or create) the storage DB under `data_dir`, keyed by `seed`.
    pub(crate) fn open(data_dir: &Path, seed: &Seed) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let encrypted_path = data_dir.join("skattr.sqlite.age");
        let working_path = data_dir.join("skattr.sqlite");

        let storage_key = hkdf_expand::<32>(seed.as_bytes(), INFO_STORAGE_V1)?;
        let passphrase = Zeroizing::new(hex::encode(storage_key.as_ref()));

        // Decrypt .age → .sqlite if needed.
        if encrypted_path.exists() && !working_path.exists() {
            decrypt_db(&encrypted_path, &working_path, &passphrase)?;
        }

        let mut conn = rusqlite::Connection::open(&working_path)
            .map_err(|e| CoreError::Storage(format!("open sqlite: {e}")))?;

        apply_pragmas(&conn)?;
        crate::storage::migrations::apply(&mut conn)?;

        Ok(Self {
            conn: Mutex::new(conn),
            encrypted_path,
            working_path,
            passphrase,
        })
    }

    /// Execute a read-only closure under the connection lock.
    pub(crate) fn with<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&rusqlite::Connection) -> Result<R>,
    {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CoreError::Storage("pool mutex poisoned".into()))?;
        f(&conn)
    }

    /// Execute a closure with a mutable connection under the lock. Use
    /// for INSERT/UPDATE/DELETE outside of an explicit transaction.
    pub(crate) fn with_mut<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&mut rusqlite::Connection) -> Result<R>,
    {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| CoreError::Storage("pool mutex poisoned".into()))?;
        f(&mut conn)
    }

    /// Run a closure inside a SQLite transaction. Commits on Ok, rolls
    /// back on Err.
    pub(crate) fn transaction<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<R>,
    {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| CoreError::Storage("pool mutex poisoned".into()))?;
        let tx = conn
            .transaction()
            .map_err(|e| CoreError::Storage(format!("begin tx: {e}")))?;
        let result = f(&tx)?;
        tx.commit()
            .map_err(|e| CoreError::Storage(format!("commit: {e}")))?;
        Ok(result)
    }

    /// Graceful shutdown: close the connection, encrypt plaintext →
    /// ciphertext, remove the plaintext file.
    pub(crate) fn close(self) -> Result<()> {
        let conn = self.conn.into_inner().map_err(|_| {
            CoreError::Storage("pool mutex poisoned during close".into())
        })?;
        drop(conn);

        encrypt_db(&self.working_path, &self.encrypted_path, &self.passphrase)?;
        std::fs::remove_file(&self.working_path)
            .map_err(|e| CoreError::Storage(format!("remove plaintext db: {e}")))?;
        Ok(())
    }

    /// Test-only: construct a Pool from an in-memory connection. Skips
    /// all encryption + file-path bookkeeping. Used by repo unit tests.
    #[cfg(test)]
    pub(crate) fn in_memory() -> Self {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        apply_pragmas(&conn).unwrap();
        crate::storage::migrations::apply(&mut conn).unwrap();
        Self {
            conn: Mutex::new(conn),
            encrypted_path: PathBuf::from("/dev/null"),
            working_path: PathBuf::from("/dev/null"),
            passphrase: Zeroizing::new(String::new()),
        }
    }
}

fn apply_pragmas(conn: &rusqlite::Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| CoreError::Storage(format!("pragma foreign_keys: {e}")))?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| CoreError::Storage(format!("pragma journal_mode: {e}")))?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| CoreError::Storage(format!("pragma synchronous: {e}")))?;
    Ok(())
}

fn decrypt_db(encrypted: &Path, plaintext: &Path, passphrase: &Zeroizing<String>) -> Result<()> {
    let ciphertext = std::fs::read(encrypted)?;
    let decryptor = age::Decryptor::new_buffered(&ciphertext[..])
        .map_err(|e| CoreError::Storage(format!("age decryptor: {e}")))?;
    if !decryptor.is_scrypt() {
        return Err(CoreError::Storage(
            "unexpected age recipient type on storage DB".into(),
        ));
    }
    let identity = age::scrypt::Identity::new(age::secrecy::SecretString::from(
        passphrase.as_str().to_string(),
    ));
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|e| CoreError::Storage(format!("age decrypt: {e}")))?;

    use std::io::{Read, Write};
    let mut out = std::fs::File::create(plaintext)
        .map_err(|e| CoreError::Storage(format!("create plaintext: {e}")))?;
    let mut buf = [0u8; 8192];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| CoreError::Storage(format!("age read: {e}")))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])
            .map_err(|e| CoreError::Storage(format!("write plaintext: {e}")))?;
    }
    out.sync_all()
        .map_err(|e| CoreError::Storage(format!("sync plaintext: {e}")))?;
    Ok(())
}

fn encrypt_db(plaintext: &Path, encrypted: &Path, passphrase: &Zeroizing<String>) -> Result<()> {
    let plaintext_bytes = std::fs::read(plaintext)?;
    let encryptor = age::Encryptor::with_user_passphrase(age::secrecy::SecretString::from(
        passphrase.as_str().to_string(),
    ));

    let mut ciphertext = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut ciphertext)
        .map_err(|e| CoreError::Storage(format!("age wrap: {e}")))?;
    use std::io::Write;
    writer
        .write_all(&plaintext_bytes)
        .map_err(|e| CoreError::Storage(format!("age write: {e}")))?;
    writer
        .finish()
        .map_err(|e| CoreError::Storage(format!("age finish: {e}")))?;

    // Atomic replace via tempfile + rename.
    let tmp_path = encrypted.with_extension("age.tmp");
    std::fs::write(&tmp_path, &ciphertext)
        .map_err(|e| CoreError::Storage(format!("write ciphertext tmp: {e}")))?;
    std::fs::rename(&tmp_path, encrypted)
        .map_err(|e| CoreError::Storage(format!("rename ciphertext: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_pool_has_migrated_schema() {
        let pool = Pool::in_memory();
        let count: i64 = pool
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='contacts'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| CoreError::Storage(e.to_string()))
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn open_close_roundtrip_preserves_data() {
        let tmp = tempfile::tempdir().unwrap();
        let seed = Seed::generate().unwrap();

        // Open, write a row, close.
        let pool = Pool::open(tmp.path(), &seed).unwrap();
        pool.with_mut(|c| {
            c.execute(
                "INSERT INTO identity (id, public_key, created_at) VALUES (1, ?1, ?2)",
                rusqlite::params![&[0xAA; 32][..], 12345i64],
            )
            .map_err(|e| CoreError::Storage(e.to_string()))?;
            Ok(())
        })
        .unwrap();
        pool.close().unwrap();

        // Plaintext file is gone; encrypted file exists.
        assert!(!tmp.path().join("skattr.sqlite").exists());
        assert!(tmp.path().join("skattr.sqlite.age").exists());

        // Reopen with the same seed, read the row back.
        let pool = Pool::open(tmp.path(), &seed).unwrap();
        let (pub_len, ts): (usize, i64) = pool
            .with(|c| {
                c.query_row(
                    "SELECT LENGTH(public_key), created_at FROM identity WHERE id = 1",
                    [],
                    |r| Ok((r.get::<_, Vec<u8>>(0)?.len(), r.get(1)?)),
                )
                .map_err(|e| CoreError::Storage(e.to_string()))
            })
            .unwrap();
        assert_eq!(pub_len, 32);
        assert_eq!(ts, 12345);
        pool.close().unwrap();
    }

    #[test]
    fn open_with_wrong_seed_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let seed_a = Seed::generate().unwrap();
        let seed_b = Seed::generate().unwrap();

        let pool = Pool::open(tmp.path(), &seed_a).unwrap();
        pool.close().unwrap();

        let err = Pool::open(tmp.path(), &seed_b)
            .err()
            .expect("wrong seed must fail to decrypt");
        assert!(matches!(err, CoreError::Storage(_)));
    }

    #[test]
    fn transaction_commits_on_ok() {
        let pool = Pool::in_memory();
        pool.transaction(|tx| {
            tx.execute(
                "INSERT INTO identity (id, public_key, created_at) VALUES (1, ?1, ?2)",
                rusqlite::params![&[0xBB; 32][..], 999i64],
            )
            .map_err(|e| CoreError::Storage(e.to_string()))?;
            Ok(())
        })
        .unwrap();
        let count: i64 = pool
            .with(|c| {
                c.query_row("SELECT COUNT(*) FROM identity", [], |r| r.get(0))
                    .map_err(|e| CoreError::Storage(e.to_string()))
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn transaction_rolls_back_on_err() {
        let pool = Pool::in_memory();
        let err = pool.transaction::<_, ()>(|tx| {
            tx.execute(
                "INSERT INTO identity (id, public_key, created_at) VALUES (1, ?1, ?2)",
                rusqlite::params![&[0xCC; 32][..], 100i64],
            )
            .map_err(|e| CoreError::Storage(e.to_string()))?;
            Err(CoreError::Storage("force rollback".into()))
        });
        assert!(err.is_err());
        let count: i64 = pool
            .with(|c| {
                c.query_row("SELECT COUNT(*) FROM identity", [], |r| r.get(0))
                    .map_err(|e| CoreError::Storage(e.to_string()))
            })
            .unwrap();
        assert_eq!(count, 0, "transaction closure Err must roll back");
    }
}
```

- [ ] **Step 2: Verify**

```bash
cargo test -p skattr-core --lib storage::pool --release 2>&1 | tail -10
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: 5 passed, clippy clean.

**API-drift note.** The `age` 0.11 surface we used (`Decryptor::new_buffered`, `is_scrypt()`, `scrypt::Identity::new`) was confirmed working during Phase 0.B Task 11 and Phase 0.C Task 2 (`hs_key.rs` uses the same pattern). Re-use the proven shape.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/storage/pool.rs
git commit -m "storage: Pool with age-encrypted open/close + tx wrapper

Pool::open decrypts skattr.sqlite.age under HKDF(seed,
'skattr-storage-v1') into a plaintext working file, opens rusqlite,
applies pragmas (foreign_keys ON, WAL, synchronous NORMAL), runs
migrations. Pool::close encrypts back + removes plaintext. Between
open/close, the plaintext file lives on disk — documented at-rest
window matches ADR-0003.

Three API helpers: with (read-only), with_mut (single writer),
transaction (commit/rollback on closure result).

Five unit tests cover: in-memory helper, open→write→close→reopen→
read round-trip, wrong-seed decrypt failure, tx commit, tx rollback
on Err."
```

---

## Task 4: ContactRepo + onion_addresses

**Goal:** Implement `ContactRepo` with upsert/get/list/remove on the `contacts` table, plus append-only onion-address history in `onion_addresses`. `cascade on delete` (already in the migration) handles the contact↔onion_addresses cleanup.

**Files:** Modify `crates/core/src/storage/contacts.rs`.

- [ ] **Step 1: Replace `contacts.rs`**

Rewrite `crates/core/src/storage/contacts.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Repository for `contacts` and `onion_addresses` tables.

use crate::contact::Contact;
use crate::error::{CoreError, Result};
use crate::identity::PublicKey;
use crate::storage::Pool;

/// Contact CRUD operations, plus onion-address history for each contact.
pub(crate) struct ContactRepo<'p> {
    pool: &'p Pool,
}

impl<'p> ContactRepo<'p> {
    pub(crate) fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Upsert a contact (add if new, update display name if existing).
    pub(crate) fn upsert(&self, contact: &Contact) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "INSERT INTO contacts (identity_pubkey, display_name, added_at) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(identity_pubkey) DO UPDATE SET display_name=excluded.display_name",
                rusqlite::params![
                    &contact.identity.0[..],
                    &contact.display_name,
                    contact.added_at,
                ],
            )
            .map_err(|e| CoreError::Storage(format!("upsert contact: {e}")))?;
            Ok(())
        })
    }

    /// Look up by identity pubkey. Returns `Ok(None)` if not present.
    ///
    /// Note: the `card` field is NOT loaded here — ContactCards are
    /// stored separately (Phase 1 wiring). For Phase 0.D we return
    /// contact metadata only with `card: None`.
    pub(crate) fn get(&self, identity: &PublicKey) -> Result<Option<Contact>> {
        self.pool.with(|c| {
            let result = c.query_row(
                "SELECT display_name, added_at FROM contacts WHERE identity_pubkey = ?1",
                rusqlite::params![&identity.0[..]],
                |r| {
                    Ok(Contact {
                        identity: *identity,
                        display_name: r.get(0)?,
                        added_at: r.get(1)?,
                        card: None,
                    })
                },
            );
            match result {
                Ok(contact) => Ok(Some(contact)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Storage(format!("get contact: {e}"))),
            }
        })
    }

    /// Enumerate all contacts, alphabetical by display name (nulls last).
    pub(crate) fn list(&self) -> Result<Vec<Contact>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT identity_pubkey, display_name, added_at FROM contacts \
                     ORDER BY display_name IS NULL, display_name COLLATE NOCASE",
                )
                .map_err(|e| CoreError::Storage(format!("prepare list contacts: {e}")))?;
            let rows = stmt
                .query_map([], |r| {
                    let pub_bytes: Vec<u8> = r.get(0)?;
                    let mut arr = [0u8; 32];
                    if pub_bytes.len() == 32 {
                        arr.copy_from_slice(&pub_bytes);
                    }
                    Ok(Contact {
                        identity: PublicKey(arr),
                        display_name: r.get(1)?,
                        added_at: r.get(2)?,
                        card: None,
                    })
                })
                .map_err(|e| CoreError::Storage(format!("query list contacts: {e}")))?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| CoreError::Storage(format!("collect contacts: {e}")))
        })
    }

    /// Delete by identity pubkey. `onion_addresses` rows cascade via FK.
    pub(crate) fn remove(&self, identity: &PublicKey) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "DELETE FROM contacts WHERE identity_pubkey = ?1",
                rusqlite::params![&identity.0[..]],
            )
            .map_err(|e| CoreError::Storage(format!("delete contact: {e}")))?;
            Ok(())
        })
    }

    /// Record a new onion address for a contact. Does NOT mark the old
    /// one stale — that's an explicit call to `mark_current`.
    pub(crate) fn add_onion(&self, identity: &PublicKey, address: &str, seen_at: i64) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "INSERT INTO onion_addresses (contact_id, address, seen_at, is_current) \
                 VALUES ((SELECT id FROM contacts WHERE identity_pubkey = ?1), ?2, ?3, 1)",
                rusqlite::params![&identity.0[..], address, seen_at],
            )
            .map_err(|e| CoreError::Storage(format!("add onion: {e}")))?;
            Ok(())
        })
    }

    /// Mark a specific onion address as the current one and demote
    /// others for the same contact. Use when a rotation arrives.
    pub(crate) fn mark_current(&self, identity: &PublicKey, address: &str) -> Result<()> {
        self.pool.transaction(|tx| {
            tx.execute(
                "UPDATE onion_addresses SET is_current = 0 \
                 WHERE contact_id = (SELECT id FROM contacts WHERE identity_pubkey = ?1)",
                rusqlite::params![&identity.0[..]],
            )
            .map_err(|e| CoreError::Storage(format!("demote old onions: {e}")))?;
            tx.execute(
                "UPDATE onion_addresses SET is_current = 1 \
                 WHERE contact_id = (SELECT id FROM contacts WHERE identity_pubkey = ?1) \
                 AND address = ?2",
                rusqlite::params![&identity.0[..], address],
            )
            .map_err(|e| CoreError::Storage(format!("promote new onion: {e}")))?;
            Ok(())
        })
    }

    /// Return the current onion address for a contact, if any.
    pub(crate) fn current_onion(&self, identity: &PublicKey) -> Result<Option<String>> {
        self.pool.with(|c| {
            let result = c.query_row(
                "SELECT address FROM onion_addresses \
                 WHERE contact_id = (SELECT id FROM contacts WHERE identity_pubkey = ?1) \
                 AND is_current = 1 \
                 ORDER BY seen_at DESC LIMIT 1",
                rusqlite::params![&identity.0[..]],
                |r| r.get::<_, String>(0),
            );
            match result {
                Ok(s) => Ok(Some(s)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Storage(format!("current_onion: {e}"))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_contact(seed: u8) -> Contact {
        Contact {
            identity: PublicKey([seed; 32]),
            display_name: Some(format!("Alice-{seed}")),
            added_at: 1_700_000_000 + i64::from(seed),
            card: None,
        }
    }

    #[test]
    fn upsert_get_roundtrip() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let alice = sample_contact(1);

        repo.upsert(&alice).unwrap();
        let got = repo.get(&alice.identity).unwrap().unwrap();
        assert_eq!(got.display_name, alice.display_name);
        assert_eq!(got.added_at, alice.added_at);
    }

    #[test]
    fn get_missing_returns_none() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        assert!(repo.get(&PublicKey([0x99; 32])).unwrap().is_none());
    }

    #[test]
    fn upsert_updates_display_name_on_conflict() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let mut alice = sample_contact(2);
        repo.upsert(&alice).unwrap();
        alice.display_name = Some("Alice-renamed".into());
        repo.upsert(&alice).unwrap();
        let got = repo.get(&alice.identity).unwrap().unwrap();
        assert_eq!(got.display_name, Some("Alice-renamed".into()));
    }

    #[test]
    fn list_returns_all_contacts_sorted() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        repo.upsert(&sample_contact(3)).unwrap();
        repo.upsert(&sample_contact(1)).unwrap();
        repo.upsert(&sample_contact(2)).unwrap();
        let all = repo.list().unwrap();
        assert_eq!(all.len(), 3);
        // Sorted alphabetically: Alice-1, Alice-2, Alice-3.
        assert_eq!(all[0].display_name, Some("Alice-1".into()));
        assert_eq!(all[1].display_name, Some("Alice-2".into()));
        assert_eq!(all[2].display_name, Some("Alice-3".into()));
    }

    #[test]
    fn remove_deletes_contact_and_cascades_onions() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let alice = sample_contact(4);
        repo.upsert(&alice).unwrap();
        repo.add_onion(&alice.identity, "aaaa.onion", 100).unwrap();
        repo.remove(&alice.identity).unwrap();
        assert!(repo.get(&alice.identity).unwrap().is_none());
        // Onion rows cascaded.
        let count: i64 = pool
            .with(|c| {
                c.query_row("SELECT COUNT(*) FROM onion_addresses", [], |r| r.get(0))
                    .map_err(|e| CoreError::Storage(e.to_string()))
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn onion_rotation_flow() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let alice = sample_contact(5);
        repo.upsert(&alice).unwrap();
        repo.add_onion(&alice.identity, "aaaa.onion", 100).unwrap();
        repo.add_onion(&alice.identity, "bbbb.onion", 200).unwrap();
        // Both are is_current=1 from add_onion — that's intentional;
        // mark_current demotes siblings.
        repo.mark_current(&alice.identity, "bbbb.onion").unwrap();
        assert_eq!(
            repo.current_onion(&alice.identity).unwrap(),
            Some("bbbb.onion".into())
        );
    }
}
```

- [ ] **Step 2: Verify**

```bash
cargo test -p skattr-core --lib storage::contacts --release 2>&1 | tail -10
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: 6 passed, clippy clean.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/storage/contacts.rs
git commit -m "storage: ContactRepo with CRUD + onion rotation

upsert uses ON CONFLICT to update display_name in place; get
returns Option; list is alphabetical (NULL last); remove cascades
into onion_addresses via the FK defined in 0001_init.sql.

Onion rotation: add_onion appends a new row as is_current=1;
mark_current demotes siblings in a single transaction; current_onion
returns the most recent. Six unit tests cover every CRUD path
plus the rotation flow."
```

---

## Task 5: MessageRepo (insert + recent)

**Goal:** `MessageRepo::insert` writes an incoming/outgoing message; `MessageRepo::recent` returns the N most recent messages for a group_id, newest first. FTS5 search is deferred to Phase 1.

**Files:** Modify `crates/core/src/storage/messages.rs`.

- [ ] **Step 1: Rewrite `messages.rs`**

Replace the existing stub body with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Message history repository.
//!
//! Phase 0.D covers insert + recent-by-group. FTS5 full-text search
//! lands in Phase 1 when the daemon actually holds enough messages for
//! search to matter.

use crate::envelope::Envelope;
use crate::error::{CoreError, Result};
use crate::storage::Pool;

/// A stored message row.
#[derive(Debug, Clone)]
pub(crate) struct StoredMessage {
    pub id: i64,
    pub group_id: Vec<u8>,
    pub sender: Vec<u8>,
    pub kind: String,
    pub body_blob: Option<Vec<u8>>,
    pub ts: i64,
    pub delivered_at: Option<i64>,
}

pub(crate) struct MessageRepo<'p> {
    pool: &'p Pool,
}

impl<'p> MessageRepo<'p> {
    pub(crate) fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Insert a message and return its rowid.
    pub(crate) fn insert(
        &self,
        group_id: &[u8],
        sender: &[u8],
        envelope: &Envelope,
    ) -> Result<i64> {
        let body = envelope.encode()?;
        let kind = match &envelope.kind {
            crate::envelope::Kind::Text { .. } => "text",
            crate::envelope::Kind::File { .. } => "file",
            crate::envelope::Kind::Reaction { .. } => "reaction",
            crate::envelope::Kind::Edit { .. } => "edit",
            crate::envelope::Kind::Delete { .. } => "delete",
            crate::envelope::Kind::Typing => "typing",
        };
        self.pool.with_mut(|c| {
            c.execute(
                "INSERT INTO messages (group_id, sender, kind, body_blob, ts) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![group_id, sender, kind, body, envelope.ts],
            )
            .map_err(|e| CoreError::Storage(format!("insert message: {e}")))?;
            Ok(c.last_insert_rowid())
        })
    }

    /// Most-recent-first list of messages in a group.
    pub(crate) fn recent(&self, group_id: &[u8], limit: usize) -> Result<Vec<StoredMessage>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, group_id, sender, kind, body_blob, ts, delivered_at \
                     FROM messages \
                     WHERE group_id = ?1 \
                     ORDER BY ts DESC LIMIT ?2",
                )
                .map_err(|e| CoreError::Storage(format!("prepare recent: {e}")))?;
            let rows = stmt
                .query_map(
                    rusqlite::params![group_id, i64::try_from(limit).unwrap_or(i64::MAX)],
                    |r| {
                        Ok(StoredMessage {
                            id: r.get(0)?,
                            group_id: r.get(1)?,
                            sender: r.get(2)?,
                            kind: r.get(3)?,
                            body_blob: r.get(4)?,
                            ts: r.get(5)?,
                            delivered_at: r.get(6)?,
                        })
                    },
                )
                .map_err(|e| CoreError::Storage(format!("query recent: {e}")))?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| CoreError::Storage(format!("collect recent: {e}")))
        })
    }

    /// Mark a message delivered. Used by the ACK path.
    pub(crate) fn mark_delivered(&self, id: i64, delivered_at: i64) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "UPDATE messages SET delivered_at = ?1 WHERE id = ?2",
                rusqlite::params![delivered_at, id],
            )
            .map_err(|e| CoreError::Storage(format!("mark delivered: {e}")))?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Kind, MessageId};

    fn sample_envelope(text: &str) -> Envelope {
        Envelope {
            v: 1,
            id: MessageId::generate(),
            ts: 1_700_000_000,
            reply_to: None,
            kind: Kind::Text {
                body: text.to_string(),
            },
        }
    }

    #[test]
    fn insert_returns_rowid_and_round_trips() {
        let pool = Pool::in_memory();
        let repo = MessageRepo::new(&pool);
        let gid = [0xAA; 32];
        let sender = [0x42; 32];
        let env = sample_envelope("hello");

        let id = repo.insert(&gid, &sender, &env).unwrap();
        assert!(id > 0);

        let all = repo.recent(&gid, 10).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, id);
        assert_eq!(all[0].kind, "text");
        assert_eq!(all[0].ts, env.ts);
        // Decode the body_blob back into an Envelope.
        let decoded = Envelope::decode(all[0].body_blob.as_ref().unwrap()).unwrap();
        assert!(matches!(decoded.kind, Kind::Text { body } if body == "hello"));
    }

    #[test]
    fn recent_orders_newest_first_and_limits() {
        let pool = Pool::in_memory();
        let repo = MessageRepo::new(&pool);
        let gid = [0xBB; 32];
        for i in 0..5 {
            let mut env = sample_envelope(&format!("msg-{i}"));
            env.ts = 100 + i as i64;
            repo.insert(&gid, &[0u8; 32], &env).unwrap();
        }
        let three = repo.recent(&gid, 3).unwrap();
        assert_eq!(three.len(), 3);
        assert_eq!(three[0].ts, 104);
        assert_eq!(three[1].ts, 103);
        assert_eq!(three[2].ts, 102);
    }

    #[test]
    fn recent_scoped_to_group_id() {
        let pool = Pool::in_memory();
        let repo = MessageRepo::new(&pool);
        let g1 = [0x11; 32];
        let g2 = [0x22; 32];
        repo.insert(&g1, &[0u8; 32], &sample_envelope("g1")).unwrap();
        repo.insert(&g2, &[0u8; 32], &sample_envelope("g2")).unwrap();
        assert_eq!(repo.recent(&g1, 10).unwrap().len(), 1);
        assert_eq!(repo.recent(&g2, 10).unwrap().len(), 1);
    }

    #[test]
    fn mark_delivered_sets_timestamp() {
        let pool = Pool::in_memory();
        let repo = MessageRepo::new(&pool);
        let id = repo
            .insert(&[0x33; 32], &[0u8; 32], &sample_envelope("x"))
            .unwrap();
        repo.mark_delivered(id, 9999).unwrap();
        let rows = repo.recent(&[0x33; 32], 10).unwrap();
        assert_eq!(rows[0].delivered_at, Some(9999));
    }
}
```

- [ ] **Step 2: Verify**

```bash
cargo test -p skattr-core --lib storage::messages --release 2>&1 | tail -5
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: 4 passed, clippy clean.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/storage/messages.rs
git commit -m "storage: MessageRepo insert + recent

insert stores a CBOR-encoded Envelope in body_blob, maps Kind to
the text column for indexing, returns rowid. recent is
group-scoped, newest-first, limit-bounded. mark_delivered is for
the ACK path (Phase 1 wiring).

FTS5 search is deferred to Phase 1 — the scaffold's messages_fts
virtual table is created by 0001_init.sql but has no syncing
triggers yet."
```

---

## Task 6: MlsGroupRepo

**Goal:** `put/get/list` for MLS group state blobs.

**Files:** Modify `crates/core/src/storage/groups.rs`.

- [ ] **Step 1: Rewrite `groups.rs`**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Repository for MLS group state blobs.

use crate::error::{CoreError, Result};
use crate::storage::Pool;

pub(crate) struct MlsGroupRepo<'p> {
    pool: &'p Pool,
}

impl<'p> MlsGroupRepo<'p> {
    pub(crate) fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Save or update the serialized MLS state for a group.
    pub(crate) fn put(&self, group_id: &[u8], state_blob: &[u8], epoch: u64) -> Result<()> {
        let epoch_i = i64::try_from(epoch)
            .map_err(|_| CoreError::Storage("epoch overflows i64".into()))?;
        self.pool.with_mut(|c| {
            c.execute(
                "INSERT INTO mls_groups (group_id, state_blob, epoch) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(group_id) DO UPDATE SET state_blob=excluded.state_blob, \
                                                     epoch=excluded.epoch",
                rusqlite::params![group_id, state_blob, epoch_i],
            )
            .map_err(|e| CoreError::Storage(format!("put mls group: {e}")))?;
            Ok(())
        })
    }

    /// Load the serialized state for a group. None if unknown.
    pub(crate) fn get(&self, group_id: &[u8]) -> Result<Option<Vec<u8>>> {
        self.pool.with(|c| {
            let result = c.query_row(
                "SELECT state_blob FROM mls_groups WHERE group_id = ?1",
                rusqlite::params![group_id],
                |r| r.get::<_, Vec<u8>>(0),
            );
            match result {
                Ok(b) => Ok(Some(b)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Storage(format!("get mls group: {e}"))),
            }
        })
    }

    /// Enumerate all known groups as `(group_id, epoch)` pairs.
    pub(crate) fn list(&self) -> Result<Vec<(Vec<u8>, u64)>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare("SELECT group_id, epoch FROM mls_groups ORDER BY id")
                .map_err(|e| CoreError::Storage(format!("prepare list mls: {e}")))?;
            let rows = stmt
                .query_map([], |r| {
                    let gid: Vec<u8> = r.get(0)?;
                    let epoch: i64 = r.get(1)?;
                    Ok((gid, u64::try_from(epoch).unwrap_or(0)))
                })
                .map_err(|e| CoreError::Storage(format!("query list mls: {e}")))?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| CoreError::Storage(format!("collect mls list: {e}")))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_roundtrip() {
        let pool = Pool::in_memory();
        let repo = MlsGroupRepo::new(&pool);
        let gid = [0xAA; 32];
        let state = b"serialized-state-bytes";
        repo.put(&gid, state, 7).unwrap();
        let got = repo.get(&gid).unwrap().unwrap();
        assert_eq!(got, state);
    }

    #[test]
    fn put_updates_existing_row() {
        let pool = Pool::in_memory();
        let repo = MlsGroupRepo::new(&pool);
        let gid = [0xBB; 32];
        repo.put(&gid, b"state-v1", 1).unwrap();
        repo.put(&gid, b"state-v2", 2).unwrap();
        assert_eq!(repo.get(&gid).unwrap().unwrap(), b"state-v2");
        assert_eq!(repo.list().unwrap().len(), 1);
    }

    #[test]
    fn get_missing_returns_none() {
        let pool = Pool::in_memory();
        let repo = MlsGroupRepo::new(&pool);
        assert!(repo.get(&[0x99; 32]).unwrap().is_none());
    }

    #[test]
    fn list_returns_all_groups() {
        let pool = Pool::in_memory();
        let repo = MlsGroupRepo::new(&pool);
        repo.put(&[0x01; 32], b"a", 1).unwrap();
        repo.put(&[0x02; 32], b"b", 5).unwrap();
        repo.put(&[0x03; 32], b"c", 10).unwrap();
        let all = repo.list().unwrap();
        assert_eq!(all.len(), 3);
        // Check epochs survived the round-trip.
        assert!(all.iter().any(|(_, e)| *e == 1));
        assert!(all.iter().any(|(_, e)| *e == 5));
        assert!(all.iter().any(|(_, e)| *e == 10));
    }
}
```

- [ ] **Step 2: Verify**

```bash
cargo test -p skattr-core --lib storage::groups --release 2>&1 | tail -5
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: 4 passed, clippy clean.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/storage/groups.rs
git commit -m "storage: MlsGroupRepo — put/get/list for opaque state blobs

put uses ON CONFLICT(group_id) to upsert. get returns Option.
list is id-ordered for determinism. epoch stored as INTEGER
(i64 column), converted to/from u64 at the boundary."
```

---

## Task 7: OutboxRepo

**Goal:** Insert/delete/due/reschedule for the outbound-message retry queue.

**Files:** Modify `crates/core/src/storage/outbox.rs`.

- [ ] **Step 1: Rewrite `outbox.rs`**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! SQL repository for the outbox table.

use crate::error::{CoreError, Result};
use crate::storage::Pool;

/// A row read back from the `outbox` table: `(id, target, payload, attempts)`.
pub type OutboxRow = (i64, Vec<u8>, Vec<u8>, u32);

pub(crate) struct OutboxRepo<'p> {
    pool: &'p Pool,
}

impl<'p> OutboxRepo<'p> {
    pub(crate) fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Insert a new outbound entry. Returns the rowid.
    pub(crate) fn insert(&self, target: &[u8], payload: &[u8], next_retry_at: i64) -> Result<i64> {
        self.pool.with_mut(|c| {
            c.execute(
                "INSERT INTO outbox (target, payload, attempts, next_retry_at) \
                 VALUES (?1, ?2, 0, ?3)",
                rusqlite::params![target, payload, next_retry_at],
            )
            .map_err(|e| CoreError::Storage(format!("insert outbox: {e}")))?;
            Ok(c.last_insert_rowid())
        })
    }

    /// Delete by rowid (e.g. after successful ACK).
    pub(crate) fn delete(&self, id: i64) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute("DELETE FROM outbox WHERE id = ?1", rusqlite::params![id])
                .map_err(|e| CoreError::Storage(format!("delete outbox: {e}")))?;
            Ok(())
        })
    }

    /// Fetch entries whose `next_retry_at` has passed.
    pub(crate) fn due(&self, now: i64, limit: usize) -> Result<Vec<OutboxRow>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, target, payload, attempts FROM outbox \
                     WHERE next_retry_at <= ?1 \
                     ORDER BY next_retry_at LIMIT ?2",
                )
                .map_err(|e| CoreError::Storage(format!("prepare due: {e}")))?;
            let rows = stmt
                .query_map(
                    rusqlite::params![now, i64::try_from(limit).unwrap_or(i64::MAX)],
                    |r| {
                        let attempts: i64 = r.get(3)?;
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            u32::try_from(attempts).unwrap_or(u32::MAX),
                        ))
                    },
                )
                .map_err(|e| CoreError::Storage(format!("query due: {e}")))?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| CoreError::Storage(format!("collect due: {e}")))
        })
    }

    /// Increment attempts and set a new next_retry_at for a failed send.
    pub(crate) fn reschedule(&self, id: i64, next_retry_at: i64) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "UPDATE outbox SET attempts = attempts + 1, next_retry_at = ?1 WHERE id = ?2",
                rusqlite::params![next_retry_at, id],
            )
            .map_err(|e| CoreError::Storage(format!("reschedule outbox: {e}")))?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_returns_rowid() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        let id = repo.insert(&[0x01; 32], b"payload", 1000).unwrap();
        assert!(id > 0);
    }

    #[test]
    fn due_returns_past_and_skips_future() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        let id_past = repo.insert(&[0xAA; 32], b"past", 100).unwrap();
        let _id_future = repo.insert(&[0xBB; 32], b"future", 9999).unwrap();
        let due = repo.due(500, 10).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].0, id_past);
    }

    #[test]
    fn reschedule_increments_attempts() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        let id = repo.insert(&[0xCC; 32], b"retry", 100).unwrap();
        repo.reschedule(id, 200).unwrap();
        repo.reschedule(id, 300).unwrap();
        let due = repo.due(999, 10).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].3, 2, "attempts must be 2 after two reschedules");
    }

    #[test]
    fn delete_removes_row() {
        let pool = Pool::in_memory();
        let repo = OutboxRepo::new(&pool);
        let id = repo.insert(&[0xDD; 32], b"x", 100).unwrap();
        repo.delete(id).unwrap();
        assert_eq!(repo.due(999, 10).unwrap().len(), 0);
    }
}
```

- [ ] **Step 2: Verify**

```bash
cargo test -p skattr-core --lib storage::outbox --release 2>&1 | tail -5
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: 4 passed, clippy clean.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/storage/outbox.rs
git commit -m "storage: OutboxRepo — insert/delete/due/reschedule

Persisted send queue keyed by rowid. due returns rows with
next_retry_at <= now, oldest first, bounded by limit. reschedule
increments attempts in a single UPDATE (the caller computes the
new next_retry_at from the exponential-backoff helper in
delivery::outbox)."
```

---

## Task 8: MailboxRepo

**Goal:** `insert/list/remove` for the registered-mailboxes table. Two roles: `mine` and `theirs`.

**Files:** Modify `crates/core/src/storage/mailboxes.rs`.

- [ ] **Step 1: Rewrite `mailboxes.rs`**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Repository for the `mailboxes` table.

use crate::error::{CoreError, Result};
use crate::storage::Pool;

/// Role of a stored mailbox record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MailboxRole {
    /// We've registered with this mailbox and poll it for inbound messages.
    Mine,
    /// Belongs to a contact; we deposit here when they're offline.
    Theirs,
}

impl MailboxRole {
    fn as_sql(self) -> &'static str {
        match self {
            MailboxRole::Mine => "mine",
            MailboxRole::Theirs => "theirs",
        }
    }

    fn from_sql(s: &str) -> Result<Self> {
        match s {
            "mine" => Ok(MailboxRole::Mine),
            "theirs" => Ok(MailboxRole::Theirs),
            other => Err(CoreError::Storage(format!(
                "unknown mailbox role: {other}"
            ))),
        }
    }
}

pub(crate) struct MailboxRepo<'p> {
    pool: &'p Pool,
}

impl<'p> MailboxRepo<'p> {
    pub(crate) fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    pub(crate) fn insert(&self, onion: &str, role: MailboxRole, registered_at: i64) -> Result<i64> {
        self.pool.with_mut(|c| {
            c.execute(
                "INSERT OR IGNORE INTO mailboxes (onion, registered_at, role) VALUES (?1, ?2, ?3)",
                rusqlite::params![onion, registered_at, role.as_sql()],
            )
            .map_err(|e| CoreError::Storage(format!("insert mailbox: {e}")))?;
            Ok(c.last_insert_rowid())
        })
    }

    pub(crate) fn list(&self, role: MailboxRole) -> Result<Vec<String>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare("SELECT onion FROM mailboxes WHERE role = ?1 ORDER BY registered_at")
                .map_err(|e| CoreError::Storage(format!("prepare list mailboxes: {e}")))?;
            let rows = stmt
                .query_map(rusqlite::params![role.as_sql()], |r| r.get::<_, String>(0))
                .map_err(|e| CoreError::Storage(format!("query list mailboxes: {e}")))?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| CoreError::Storage(format!("collect mailboxes: {e}")))
        })
    }

    pub(crate) fn remove(&self, onion: &str, role: MailboxRole) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "DELETE FROM mailboxes WHERE onion = ?1 AND role = ?2",
                rusqlite::params![onion, role.as_sql()],
            )
            .map_err(|e| CoreError::Storage(format!("delete mailbox: {e}")))?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_list_by_role() {
        let pool = Pool::in_memory();
        let repo = MailboxRepo::new(&pool);
        repo.insert("mine-a.onion", MailboxRole::Mine, 100).unwrap();
        repo.insert("theirs-a.onion", MailboxRole::Theirs, 200).unwrap();
        repo.insert("mine-b.onion", MailboxRole::Mine, 300).unwrap();

        let mine = repo.list(MailboxRole::Mine).unwrap();
        assert_eq!(mine, vec!["mine-a.onion", "mine-b.onion"]);

        let theirs = repo.list(MailboxRole::Theirs).unwrap();
        assert_eq!(theirs, vec!["theirs-a.onion"]);
    }

    #[test]
    fn insert_or_ignore_dedups_same_role() {
        let pool = Pool::in_memory();
        let repo = MailboxRepo::new(&pool);
        repo.insert("dup.onion", MailboxRole::Mine, 100).unwrap();
        repo.insert("dup.onion", MailboxRole::Mine, 200).unwrap();
        assert_eq!(repo.list(MailboxRole::Mine).unwrap().len(), 1);
    }

    #[test]
    fn remove_scoped_to_role() {
        let pool = Pool::in_memory();
        let repo = MailboxRepo::new(&pool);
        repo.insert("multi.onion", MailboxRole::Mine, 100).unwrap();
        repo.insert("multi.onion", MailboxRole::Theirs, 200).unwrap();
        repo.remove("multi.onion", MailboxRole::Mine).unwrap();
        assert_eq!(repo.list(MailboxRole::Mine).unwrap().len(), 0);
        assert_eq!(repo.list(MailboxRole::Theirs).unwrap().len(), 1);
    }

    #[test]
    fn sql_role_parse_rejects_garbage() {
        assert!(MailboxRole::from_sql("bogus").is_err());
    }
}
```

- [ ] **Step 2: Verify and commit**

```bash
cargo test -p skattr-core --lib storage::mailboxes --release 2>&1 | tail -5
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
git add crates/core/src/storage/mailboxes.rs
git commit -m "storage: MailboxRepo — insert/list/remove, keyed by (onion, role)

The CHECK constraint on role is enforced by the 0001 migration;
MailboxRole ↔ 'mine'/'theirs' conversion is handled in one place.
insert uses OR IGNORE so re-registration is a no-op. list orders
by registered_at. remove is scoped to (onion, role) so the same
.onion string can coexist under both roles."
```

---

## Task 9: SeenMessagesRepo (dedup)

**Goal:** The dedup table for received messages. `(sender, message_id)` uniqueness with a sliding 24-hour TTL.

**Files:** Create `crates/core/src/storage/seen_messages.rs`, modify `crates/core/src/storage/mod.rs`.

- [ ] **Step 1: Register the module**

In `crates/core/src/storage/mod.rs`, add:

```rust
pub(crate) mod seen_messages;
```

- [ ] **Step 2: Create `seen_messages.rs`**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Dedup table for received messages: `(sender, message_id)` with TTL sweep.
//!
//! Receiver-side dedup uses a sliding 24-hour window. We insert
//! `(sender, message_id, now)` on every successful receive and query
//! "contains(sender, message_id)" on each incoming envelope before
//! surfacing it to the UI. `sweep_older_than(cutoff)` is called
//! periodically to garbage-collect rows outside the window.

use crate::error::{CoreError, Result};
use crate::storage::Pool;

pub(crate) struct SeenMessagesRepo<'p> {
    pool: &'p Pool,
}

impl<'p> SeenMessagesRepo<'p> {
    pub(crate) fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Mark a message as seen. Returns `true` if this is new (insert
    /// succeeded) or `false` if we've already seen it (PRIMARY KEY
    /// conflict).
    pub(crate) fn insert(&self, sender: &[u8], message_id: &[u8], seen_at: i64) -> Result<bool> {
        self.pool.with_mut(|c| {
            let changed = c
                .execute(
                    "INSERT OR IGNORE INTO seen_messages (sender, message_id, seen_at) \
                     VALUES (?1, ?2, ?3)",
                    rusqlite::params![sender, message_id, seen_at],
                )
                .map_err(|e| CoreError::Storage(format!("insert seen: {e}")))?;
            Ok(changed > 0)
        })
    }

    /// Has this (sender, message_id) been seen?
    pub(crate) fn contains(&self, sender: &[u8], message_id: &[u8]) -> Result<bool> {
        self.pool.with(|c| {
            let count: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM seen_messages WHERE sender = ?1 AND message_id = ?2",
                    rusqlite::params![sender, message_id],
                    |r| r.get(0),
                )
                .map_err(|e| CoreError::Storage(format!("contains seen: {e}")))?;
            Ok(count > 0)
        })
    }

    /// Delete rows with `seen_at < cutoff`. Returns the number removed.
    pub(crate) fn sweep_older_than(&self, cutoff: i64) -> Result<u64> {
        self.pool.with_mut(|c| {
            let n = c
                .execute(
                    "DELETE FROM seen_messages WHERE seen_at < ?1",
                    rusqlite::params![cutoff],
                )
                .map_err(|e| CoreError::Storage(format!("sweep seen: {e}")))?;
            Ok(n as u64)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_is_idempotent() {
        let pool = Pool::in_memory();
        let repo = SeenMessagesRepo::new(&pool);
        let sender = [0xAA; 32];
        let mid = [0x01; 16];
        assert!(repo.insert(&sender, &mid, 100).unwrap(), "first insert is new");
        assert!(!repo.insert(&sender, &mid, 200).unwrap(), "second insert is dup");
    }

    #[test]
    fn contains_after_insert() {
        let pool = Pool::in_memory();
        let repo = SeenMessagesRepo::new(&pool);
        let sender = [0xBB; 32];
        let mid = [0x02; 16];
        assert!(!repo.contains(&sender, &mid).unwrap());
        repo.insert(&sender, &mid, 100).unwrap();
        assert!(repo.contains(&sender, &mid).unwrap());
    }

    #[test]
    fn sweep_removes_old_rows_only() {
        let pool = Pool::in_memory();
        let repo = SeenMessagesRepo::new(&pool);
        repo.insert(&[0x01; 32], &[0xAA; 16], 100).unwrap();
        repo.insert(&[0x02; 32], &[0xBB; 16], 500).unwrap();
        repo.insert(&[0x03; 32], &[0xCC; 16], 900).unwrap();

        let removed = repo.sweep_older_than(600).unwrap();
        assert_eq!(removed, 2);
        assert!(!repo.contains(&[0x01; 32], &[0xAA; 16]).unwrap());
        assert!(!repo.contains(&[0x02; 32], &[0xBB; 16]).unwrap());
        assert!(repo.contains(&[0x03; 32], &[0xCC; 16]).unwrap());
    }
}
```

- [ ] **Step 3: Verify and commit**

```bash
cargo test -p skattr-core --lib storage::seen_messages --release 2>&1 | tail -5
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
git add crates/core/src/storage/mod.rs crates/core/src/storage/seen_messages.rs
git commit -m "storage: SeenMessagesRepo — dedup table with sliding window sweep

Receiver-side dedup. insert returns true/false for new/duplicate
(INSERT OR IGNORE on the composite PRIMARY KEY). contains is the
hot-path query the receiver calls on each incoming envelope.
sweep_older_than drops rows outside the 24-hour window; called
from a periodic task."
```

---

## Task 10: Backup export + import

**Goal:** `export_backup(data_dir, out_path, seed)` bundles the three at-rest files into a gzipped tar archive and age-encrypts the result with a seed-derived key. `import_backup(archive_path, data_dir, seed)` reverses it. One commit covers both directions plus tests.

**Files:** Create `crates/core/src/storage/backup.rs`, modify `crates/core/src/storage/mod.rs`.

- [ ] **Step 1: Register the module**

In `crates/core/src/storage/mod.rs`, add:

```rust
pub(crate) mod backup;
```

- [ ] **Step 2: Write `backup.rs`**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Portable backup of the three at-rest encrypted files.
//!
//! A backup is a tarball containing:
//!
//! ```text
//! identity.vault
//! hs.key.age
//! skattr.sqlite.age
//! ```
//!
//! The tarball is gzip-compressed and then age-encrypted under
//! `HKDF(seed, "skattr-backup-v1")` — a THIRD layer of domain-
//! separated encryption on top of the already-encrypted inner files.
//! The rationale is defense-in-depth: even if an attacker obtains
//! the archive AND the user's vault passphrase somehow, they still
//! need the seed to open the outer layer.

use std::io::{Read, Write};
use std::path::Path;

use zeroize::Zeroizing;

use crate::error::{CoreError, Result};
use crate::identity::derive::{hkdf_expand, INFO_BACKUP_V1};
use crate::identity::Seed;

const BACKUP_FILES: &[&str] = &["identity.vault", "hs.key.age", "skattr.sqlite.age"];

/// Write a backup archive to `out_path`. Fails if any of the three
/// source files are missing from `data_dir`.
pub(crate) fn export_backup(data_dir: &Path, out_path: &Path, seed: &Seed) -> Result<()> {
    for name in BACKUP_FILES {
        let p = data_dir.join(name);
        if !p.exists() {
            return Err(CoreError::Storage(format!(
                "missing backup input: {}",
                p.display()
            )));
        }
    }

    // Build gzipped tar in memory.
    let mut tar_gz = Vec::new();
    {
        let gz = flate2::write::GzEncoder::new(&mut tar_gz, flate2::Compression::default());
        let mut builder = tar::Builder::new(gz);
        for name in BACKUP_FILES {
            let src = data_dir.join(name);
            builder
                .append_path_with_name(&src, name)
                .map_err(|e| CoreError::Storage(format!("tar append {name}: {e}")))?;
        }
        builder
            .into_inner()
            .and_then(|gz| gz.finish())
            .map_err(|e| CoreError::Storage(format!("tar/gz finish: {e}")))?;
    }

    // Age-encrypt the gzipped tarball.
    let key = hkdf_expand::<32>(seed.as_bytes(), INFO_BACKUP_V1)?;
    let passphrase = Zeroizing::new(hex::encode(key.as_ref()));
    let encryptor = age::Encryptor::with_user_passphrase(age::secrecy::SecretString::from(
        passphrase.as_str().to_string(),
    ));

    let mut ciphertext = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut ciphertext)
        .map_err(|e| CoreError::Storage(format!("age wrap backup: {e}")))?;
    writer
        .write_all(&tar_gz)
        .map_err(|e| CoreError::Storage(format!("age write backup: {e}")))?;
    writer
        .finish()
        .map_err(|e| CoreError::Storage(format!("age finish backup: {e}")))?;

    // Atomic write.
    let tmp_path = out_path.with_extension("tmp");
    std::fs::write(&tmp_path, &ciphertext)?;
    std::fs::rename(&tmp_path, out_path)?;
    Ok(())
}

/// Read a backup archive from `archive_path` and extract the three
/// files into `data_dir`. Refuses to overwrite any existing file in
/// the target directory — the caller is expected to restore into a
/// clean data_dir.
pub(crate) fn import_backup(archive_path: &Path, data_dir: &Path, seed: &Seed) -> Result<()> {
    for name in BACKUP_FILES {
        if data_dir.join(name).exists() {
            return Err(CoreError::Storage(format!(
                "refusing to overwrite existing {}; restore into a clean data_dir",
                name
            )));
        }
    }

    let ciphertext = std::fs::read(archive_path)?;
    let key = hkdf_expand::<32>(seed.as_bytes(), INFO_BACKUP_V1)?;
    let passphrase = Zeroizing::new(hex::encode(key.as_ref()));

    let decryptor = age::Decryptor::new_buffered(&ciphertext[..])
        .map_err(|e| CoreError::Storage(format!("age decryptor backup: {e}")))?;
    if !decryptor.is_scrypt() {
        return Err(CoreError::Storage(
            "unexpected age recipient type on backup".into(),
        ));
    }
    let identity = age::scrypt::Identity::new(age::secrecy::SecretString::from(
        passphrase.as_str().to_string(),
    ));
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|e| CoreError::Storage(format!("age decrypt backup: {e}")))?;

    let mut tar_gz = Vec::new();
    reader
        .read_to_end(&mut tar_gz)
        .map_err(|e| CoreError::Storage(format!("read backup plaintext: {e}")))?;

    // Extract gzipped tarball.
    std::fs::create_dir_all(data_dir)?;
    let gz = flate2::read::GzDecoder::new(&tar_gz[..]);
    let mut archive = tar::Archive::new(gz);
    for entry in archive
        .entries()
        .map_err(|e| CoreError::Storage(format!("tar entries: {e}")))?
    {
        let mut entry = entry.map_err(|e| CoreError::Storage(format!("tar entry: {e}")))?;
        let name = entry
            .path()
            .map_err(|e| CoreError::Storage(format!("tar entry path: {e}")))?
            .to_string_lossy()
            .into_owned();
        if !BACKUP_FILES.contains(&name.as_str()) {
            return Err(CoreError::Storage(format!(
                "unexpected backup entry: {name}"
            )));
        }
        let dest = data_dir.join(&name);
        entry
            .unpack(&dest)
            .map_err(|e| CoreError::Storage(format!("extract {name}: {e}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn populate_data_dir(data_dir: &Path) {
        std::fs::create_dir_all(data_dir).unwrap();
        std::fs::write(data_dir.join("identity.vault"), b"vault-bytes").unwrap();
        std::fs::write(data_dir.join("hs.key.age"), b"hs-key-bytes").unwrap();
        std::fs::write(data_dir.join("skattr.sqlite.age"), b"db-bytes").unwrap();
    }

    #[test]
    fn roundtrip_preserves_all_three_files() {
        let tmp_src = tempfile::tempdir().unwrap();
        let tmp_dst = tempfile::tempdir().unwrap();
        populate_data_dir(tmp_src.path());
        let seed = Seed::generate().unwrap();
        let archive = tmp_src.path().join("backup.age");

        export_backup(tmp_src.path(), &archive, &seed).unwrap();
        import_backup(&archive, tmp_dst.path(), &seed).unwrap();

        for name in BACKUP_FILES {
            let src = std::fs::read(tmp_src.path().join(name)).unwrap();
            let dst = std::fs::read(tmp_dst.path().join(name)).unwrap();
            assert_eq!(src, dst, "{name} must round-trip");
        }
    }

    #[test]
    fn export_fails_with_missing_input() {
        let tmp = tempfile::tempdir().unwrap();
        let seed = Seed::generate().unwrap();
        // Only create two of the three.
        std::fs::write(tmp.path().join("identity.vault"), b"v").unwrap();
        std::fs::write(tmp.path().join("hs.key.age"), b"h").unwrap();
        let err = export_backup(tmp.path(), &tmp.path().join("backup.age"), &seed)
            .err()
            .expect("missing skattr.sqlite.age must error");
        assert!(matches!(err, CoreError::Storage(_)));
    }

    #[test]
    fn import_refuses_to_overwrite() {
        let tmp_src = tempfile::tempdir().unwrap();
        let tmp_dst = tempfile::tempdir().unwrap();
        populate_data_dir(tmp_src.path());
        let seed = Seed::generate().unwrap();
        let archive = tmp_src.path().join("backup.age");
        export_backup(tmp_src.path(), &archive, &seed).unwrap();

        // Plant a conflicting file in the destination.
        std::fs::write(tmp_dst.path().join("identity.vault"), b"existing").unwrap();
        let err = import_backup(&archive, tmp_dst.path(), &seed)
            .err()
            .expect("overwrite must fail");
        assert!(matches!(err, CoreError::Storage(_)));
    }

    #[test]
    fn import_fails_with_wrong_seed() {
        let tmp_src = tempfile::tempdir().unwrap();
        let tmp_dst = tempfile::tempdir().unwrap();
        populate_data_dir(tmp_src.path());
        let seed_a = Seed::generate().unwrap();
        let seed_b = Seed::generate().unwrap();
        let archive = tmp_src.path().join("backup.age");
        export_backup(tmp_src.path(), &archive, &seed_a).unwrap();

        let err = import_backup(&archive, tmp_dst.path(), &seed_b)
            .err()
            .expect("wrong seed must fail to decrypt");
        assert!(matches!(err, CoreError::Storage(_)));
    }
}
```

- [ ] **Step 3: Verify**

```bash
cargo test -p skattr-core --lib storage::backup --release 2>&1 | tail -5
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: 4 passed, clippy clean.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/storage/mod.rs crates/core/src/storage/backup.rs
git commit -m "storage: backup export/import with age outer encryption

export_backup tars+gzips the three at-rest files (identity.vault,
hs.key.age, skattr.sqlite.age) and age-encrypts under HKDF(seed,
'skattr-backup-v1'). import_backup reverses it, refusing to
overwrite existing files in the target data_dir.

Defense-in-depth: outer encryption uses a seed-derived key that
is DIFFERENT from the inner files' keys — even if an attacker
compromises the vault passphrase, they still need the seed to
open the archive."
```

---

## Task 11: Wire `skattr backup <file>` + `skattr restore-backup <file>`

**Goal:** Expose `export_backup` / `import_backup` through the CLI. The `backup` subcommand requires an open vault (to derive the storage seed); `restore-backup` requires the BIP39 mnemonic (so we can derive the seed without the vault being present).

**Files:** Modify `crates/cli/src/main.rs`.

- [ ] **Step 1: Extend the clap enum**

In `crates/cli/src/main.rs`, find the `enum Command { ... }` and add two new variants alongside `Init`, `Restore`, etc.:

```rust
    /// Export a portable backup of identity + storage + HS key to FILE.
    Backup {
        /// Destination archive path.
        file: PathBuf,
    },
    /// Restore identity + storage + HS key from a backup archive.
    RestoreBackup {
        /// BIP39 mnemonic (quoted space-separated words).
        seed: String,
        /// Source archive path.
        file: PathBuf,
    },
```

(Place them before the `Contacts` variant for rough grouping with `Restore`.)

In `main`'s dispatch:

```rust
        Command::Backup { file } => backup(&file, cli.data_dir.as_deref()).await,
        Command::RestoreBackup { seed, file } => {
            restore_backup(&seed, &file, cli.data_dir.as_deref()).await
        }
```

- [ ] **Step 2: Implement `backup`**

After the existing `restore` function, add:

```rust
async fn backup(out_file: &std::path::Path, data_dir_override: Option<&std::path::Path>) -> Result<()> {
    use skattr_core::identity::derive::derive_storage_seed;
    use skattr_core::storage::backup::export_backup;

    let data_dir = effective_data_dir(data_dir_override)?;
    let vault_path = data_dir.join("identity.vault");
    if !vault_path.exists() {
        anyhow::bail!(
            "no identity vault at {}; nothing to back up",
            vault_path.display()
        );
    }

    let pw = read_passphrase("Vault passphrase: ")?;
    let (_vault, identity) = Vault::open(&vault_path, pw.as_str())?;
    let seed = derive_storage_seed(identity)?;

    export_backup(&data_dir, out_file, &seed)?;
    println!("Backup written to {}", out_file.display());
    Ok(())
}
```

**Visibility.** `storage::backup` is `pub(crate)` and `storage` itself is `pub(crate)`, so the CLI can't reach the helpers directly. We surface them through the already-public `daemon` module — semantically appropriate since backup IS a daemon-lifecycle operation.

Add to `crates/core/src/daemon/mod.rs`:

```rust
pub mod backup;
```

Create `crates/core/src/daemon/backup.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Public re-exports for backup export / import. The implementations
//! live in `storage::backup` (crate-private); this module surfaces the
//! two helpers through the public `daemon` API so the CLI can call them
//! without widening `storage`'s visibility.

pub use crate::storage::backup::{export_backup, import_backup};
```

Update the CLI `use` line to match:

```rust
use skattr_core::daemon::backup::export_backup;
```

- [ ] **Step 3: Implement `restore_backup`**

```rust
async fn restore_backup(
    seed_phrase: &str,
    archive_file: &std::path::Path,
    data_dir_override: Option<&std::path::Path>,
) -> Result<()> {
    use anyhow::Context;
    use skattr_core::daemon::backup::import_backup;
    use skattr_core::identity::derive::derive_storage_seed;

    let data_dir = effective_data_dir(data_dir_override)?;

    // Parse the mnemonic through a Zeroizing copy.
    let mnemonic = {
        let owned = zeroize::Zeroizing::new(seed_phrase.to_string());
        Mnemonic::parse(&owned)
    };
    let identity_seed = Seed::from_mnemonic(&mnemonic)
        .context("invalid seed phrase (check word list and checksum)")?;
    let identity = IdentityKey::from_seed(&identity_seed)?;
    let storage_seed = derive_storage_seed(identity)?;

    import_backup(archive_file, &data_dir, &storage_seed)?;
    println!("Restored from {}", archive_file.display());
    println!("Data at: {}", data_dir.display());
    println!("Run `skattr daemon` to bring the identity online.");
    Ok(())
}
```

- [ ] **Step 4: Verify**

```bash
cargo build --workspace 2>&1 | tail -5
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
cargo test --workspace --release 2>&1 | tail -5
```

Expected: build clean, clippy clean, no regressions.

**Smoke test (optional — network-free):**

```bash
TMP=$(mktemp -d)
BACKUP=$TMP/backup.age

# Step 1: init + produce the three at-rest files.
printf 'pw\npw\n' | cargo run --quiet -p skattr-cli -- --data-dir "$TMP" init 2>&1 | tail -10
# At this point only identity.vault exists — hs.key.age and skattr.sqlite.age
# are created by `skattr daemon` on first start. For a smoke test without
# launching Tor, manually create placeholder files so backup can run:
touch "$TMP/hs.key.age"
touch "$TMP/skattr.sqlite.age"

# Step 2: backup.
printf 'pw\n' | cargo run --quiet -p skattr-cli -- --data-dir "$TMP" backup "$BACKUP" 2>&1 | tail -5

# Step 3: restore into a clean dir. Capture the mnemonic from step 1's output
# (not shown here for brevity; in a real test you'd grep/save it).
TMP2=$(mktemp -d)
PHRASE="..."  # from step 1
cargo run --quiet -p skattr-cli -- --data-dir "$TMP2" restore-backup "$PHRASE" "$BACKUP" 2>&1 | tail -5

# Step 4: confirm the three files are present in TMP2.
ls -la "$TMP2"
```

Note: this smoke test doesn't exercise the real (non-zero-length) inner files because creating them requires running `skattr daemon`. For Phase 0.D the unit tests in Task 10 cover the round-trip with synthetic content; a true end-to-end test would require starting Tor, which is out of scope.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/mod.rs \
        crates/core/src/daemon/backup.rs \
        crates/cli/src/main.rs
git commit -m "cli: wire skattr backup + restore-backup subcommands

daemon::backup re-exports storage::backup::{export_backup,
import_backup} through the public daemon API, keeping storage
pub(crate).

backup prompts for vault passphrase → derives storage seed →
exports. restore-backup takes the BIP39 mnemonic directly
(argv-Zeroizing same pattern as skattr restore) and extracts
into a clean data_dir.

Does NOT start the daemon after restore — the user explicitly
runs `skattr daemon` next, which boots Tor and publishes the
restored onion."
```

---

## Task 12: Integration test — open → write → close → reopen cycle

**Goal:** Exercise Pool + multiple repos end-to-end in one test, proving they compose correctly across the encrypt/decrypt boundary.

**Files:** Create `crates/core/tests/storage_roundtrip.rs`.

- [ ] **Step 1: Create the integration test**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Phase 0.D integration test: Pool + repos survive a close/reopen cycle.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use skattr_core::contact::Contact;
use skattr_core::envelope::{Envelope, Kind, MessageId};
use skattr_core::identity::{PublicKey, Seed};

// Pool + repos are pub(crate); the test-harness feature exposes the
// handful we need through test_exports. Extend test_exports in core
// if the set grows.
use skattr_core::test_exports::{ContactRepo, MessageRepo, Pool};

#[test]
fn pool_close_reopen_preserves_contacts_and_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let seed = Seed::generate().unwrap();

    // First open: write a contact + a message.
    let pool = Pool::open(tmp.path(), &seed).unwrap();
    let alice = Contact {
        identity: PublicKey([0x77; 32]),
        display_name: Some("alice".into()),
        added_at: 1700000000,
        card: None,
    };
    ContactRepo::new(&pool).upsert(&alice).unwrap();

    let env = Envelope {
        v: 1,
        id: MessageId::generate(),
        ts: 1700000100,
        reply_to: None,
        kind: Kind::Text {
            body: "hello alice".into(),
        },
    };
    let gid = [0xAA; 32];
    let sender = [0x77; 32];
    MessageRepo::new(&pool).insert(&gid, &sender, &env).unwrap();

    pool.close().unwrap();

    // Second open: same seed, read both back.
    let pool = Pool::open(tmp.path(), &seed).unwrap();
    let got = ContactRepo::new(&pool)
        .get(&alice.identity)
        .unwrap()
        .expect("alice survives close/reopen");
    assert_eq!(got.display_name, Some("alice".into()));

    let messages = MessageRepo::new(&pool).recent(&gid, 10).unwrap();
    assert_eq!(messages.len(), 1);
    let decoded = Envelope::decode(messages[0].body_blob.as_ref().unwrap()).unwrap();
    assert!(matches!(decoded.kind, Kind::Text { body } if body == "hello alice"));

    pool.close().unwrap();
}
```

- [ ] **Step 2: Extend `test_exports` in `core`**

In `crates/core/src/lib.rs`, the existing `#[cfg(feature = "test-harness")] pub mod test_exports` (from the Phase 0.C cleanup) needs to add the storage items:

```rust
#[cfg(feature = "test-harness")]
pub mod test_exports {
    pub use crate::transport::tor::{TorConfig, TorRuntime, TorStatus};
    pub use crate::transport::OnionListener;
    // Phase 0.D additions:
    pub use crate::storage::Pool;
    pub use crate::storage::contacts::ContactRepo;
    pub use crate::storage::messages::MessageRepo;
}
```

Since `storage::contacts::ContactRepo` is `pub(crate)`, the same trick as in transport applies: add `#[cfg(feature = "test-harness")] pub use` in `storage/mod.rs`:

```rust
#[cfg(feature = "test-harness")]
pub use pool::Pool;
#[cfg(feature = "test-harness")]
pub use contacts::ContactRepo;
#[cfg(feature = "test-harness")]
pub use messages::MessageRepo;

#[cfg(not(feature = "test-harness"))]
pub(crate) use pool::Pool;
```

And similar twin-arms for `ContactRepo`/`MessageRepo`. Match the exact pattern used by `transport/mod.rs` from the Phase 0.C cleanup.

(If `crates/tests/Cargo.toml`'s `skattr-core = { path = "../core", features = ["test-harness"] }` is already set, the integration test opts in automatically.)

- [ ] **Step 3: Verify**

```bash
cargo test -p skattr-core --test storage_roundtrip --release 2>&1 | tail -5
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: 1 passed, clippy clean.

- [ ] **Step 4: Commit**

```bash
git add crates/core/tests/storage_roundtrip.rs \
        crates/core/src/lib.rs \
        crates/core/src/storage/mod.rs
git commit -m "storage: integration test for close/reopen round-trip

Writes a contact + a message, closes the pool (encrypts on disk,
removes plaintext), reopens with the same seed, reads both back.
Proves Pool + repos survive the encrypt/decrypt boundary
end-to-end.

Extends test_exports with Pool, ContactRepo, MessageRepo behind
the test-harness feature; storage/mod.rs mirrors the twin-arm
pub/pub(crate) pattern established by transport/mod.rs in the
Phase 0.C cleanup."
```

---

## Post-plan wrap-up

- [ ] **Step 1: Full gate run**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --release
cd crates/core && cargo +nightly fuzz build vault_parser && cd ../..
```

All four must pass. Apply fmt + commit if any drift.

- [ ] **Step 2: CHANGELOG update**

Append under `[Unreleased]`:

```markdown
- **Phase 0.D Storage layer:** `rusqlite` + `age`-encrypted `Pool` (HKDF `skattr-storage-v1` key, decrypt-to-plaintext-working-file at open, encrypt-back on close). Migrations runner keyed by a `schema_version` table (one migration so far: `0001_init.sql`). Seven repos — `ContactRepo`/`MessageRepo`/`MlsGroupRepo`/`OutboxRepo`/`MailboxRepo`/`SeenMessagesRepo` plus onion-address helpers on `ContactRepo`. Transactions wrapper with commit-on-Ok / rollback-on-Err. Backup archive (`tar.gz` of the three at-rest files, outer-age-encrypted under `HKDF(seed, "skattr-backup-v1")`). `skattr backup <file>` + `skattr restore-backup <seed> <file>` CLI commands. Storage primitives exposed via `skattr_core::daemon::backup` (public) and `skattr_core::test_exports` (test-harness feature) so internals stay `pub(crate)`.
```

Commit:

```bash
git add CHANGELOG.md
git commit -m "changelog: Phase 0.D storage layer"
```

- [ ] **Step 3: CLAUDE.md update**

In `CLAUDE.md`, find the Repository-state paragraph and expand the Phase-complete list to include 0.D. Keep the existing 0.B/0.C text. Remove 0.D from "Remaining" list.

Commit:

```bash
git add CLAUDE.md
git commit -m "docs: CLAUDE.md — Phase 0.D complete"
```

- [ ] **Step 4: Final check**

```bash
grep -rn 'todo!' crates/core/src/storage/
```

Expected: zero matches. Phase 0.D deliverable is the storage plumbing without `todo!()`.

---

## Notes for the executing engineer

- **`pub(crate)` discipline.** The entire `storage::*` layer is `pub(crate)` except for the two re-exports via `daemon::backup` and the test-harness `test_exports`. Follow the twin-arm `pub`/`pub(crate)` pattern from `transport/mod.rs`; don't widen the storage module itself.
- **In-memory tests are cheap.** Every repo test uses `Pool::in_memory()` and completes in milliseconds. Only the file-based `open_close_roundtrip_preserves_data` test actually exercises age encryption, and it runs in ~100 ms thanks to SQLite writing the small fixture fast.
- **Release mode for Argon2 tests.** You'll notice no Argon2 in this plan — the Vault tests are already covered under Phase 0.B. Storage's encryption is age+scrypt which is cheap in debug mode. Default `cargo test` is fine.
- **Foreign-key cascade.** `0001_init.sql` already has `ON DELETE CASCADE` on `onion_addresses.contact_id`. With `PRAGMA foreign_keys = ON` applied by `apply_pragmas`, the cascade actually fires. Task 4's "remove_deletes_contact_and_cascades_onions" test depends on this.
- **FTS5 search deferred to Phase 1.** The `messages_fts` virtual table exists (created by 0001_init.sql) but has no sync triggers yet. Phase 1 adds the triggers and a search method on `MessageRepo`. Don't add them in Phase 0.D — the daemon doesn't query messages yet.
- **Backup is ADR-adjacent.** If you want to be pedantic, an ADR-0006 documenting the "tar+gz+age outer encryption" design could land here. The plan doesn't mandate it, but a 20-line ADR alongside Task 10 would be a clean addition. Optional.
