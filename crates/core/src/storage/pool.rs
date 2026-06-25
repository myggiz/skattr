// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

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
//! 3. `Pool::close(&self)`: `wal_checkpoint(TRUNCATE)` to fold the WAL into
//!    the main file, drop the Connection, encrypt plaintext → .age, then
//!    remove the plaintext DB, its `-wal`/`-shm` sidecars, and the sentinel.
//!    Idempotent + guarded, so it can run through a shared `Arc` at daemon
//!    teardown and a `Drop` backstop calls it for abnormal exits.
//!
//! Crash model: a `skattr.sqlite.open` sentinel is written while the
//! plaintext DB is live and removed on clean close. If the process dies
//! without closing, the plaintext `skattr.sqlite` remains on disk; the next
//! `Pool::open` detects the residue (plaintext present before decrypt) and
//! re-encrypts it to `.age` on boot, then continues on the live plaintext —
//! no data loss, and a current encrypted copy always exists after boot.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use zeroize::Zeroizing;

use super::StorageErrorKind;
use crate::error::{CoreError, Result};
use crate::identity::derive::{hkdf_expand, INFO_STORAGE_V1};
use crate::identity::Seed;

/// Construct the typed error returned when an accessor runs against a
/// pool whose connection has already been taken (closed).
fn pool_closed() -> CoreError {
    CoreError::Storage(StorageErrorKind::Other("pool is closed".into()))
}

/// SQLite connection pool. Single writer, WAL mode.
pub struct Pool {
    conn: Mutex<Option<rusqlite::Connection>>,
    encrypted_path: PathBuf,
    working_path: PathBuf,
    /// Marker file (`<data_dir>/skattr.sqlite.open`) present while the
    /// plaintext DB is live; removed on clean close / Drop.
    sentinel_path: PathBuf,
    /// `false` for `in_memory()` test pools — the Drop backstop is a no-op.
    persistent: bool,
    /// Age passphrase (hex of the HKDF output). Held by the pool so
    /// `close()` can re-encrypt without re-deriving.
    passphrase: Zeroizing<String>,
}

impl Pool {
    /// Open (or create) the storage DB under `data_dir`, keyed by `seed`.
    pub fn open(data_dir: &Path, seed: &Seed) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Private to the owning user: the dir holds the encrypted DB +
            // sentinels. Enforce even if a prior umask created it 0755.
            std::fs::set_permissions(data_dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let encrypted_path = data_dir.join("skattr.sqlite.age");
        let working_path = data_dir.join("skattr.sqlite");
        let sentinel_path = data_dir.join("skattr.sqlite.open");

        let storage_key = hkdf_expand::<32>(seed.as_bytes(), INFO_STORAGE_V1)?;
        let passphrase = Zeroizing::new(hex::encode(storage_key.as_ref()));

        // Crash residue: plaintext present before we touch anything. The clean
        // path is `.age` present + `.sqlite` absent, so a pre-existing `.sqlite`
        // means the prior run did not close cleanly. Computed BEFORE decrypt so
        // the clean path (which creates `.sqlite`) is never misclassified.
        let residue = working_path.exists();

        // Decrypt .age → .sqlite if needed.
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
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .map_err(|e| {
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

    /// Execute a read-only closure under the connection lock.
    pub(crate) fn with<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&rusqlite::Connection) -> Result<R>,
    {
        let guard = self.conn.lock().map_err(|_| {
            CoreError::Storage(StorageErrorKind::Other("pool mutex poisoned".into()))
        })?;
        let conn = guard.as_ref().ok_or_else(pool_closed)?;
        f(conn)
    }

    /// Execute a closure with a mutable connection under the lock. Use
    /// for INSERT/UPDATE/DELETE outside of an explicit transaction.
    pub(crate) fn with_mut<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&mut rusqlite::Connection) -> Result<R>,
    {
        let mut guard = self.conn.lock().map_err(|_| {
            CoreError::Storage(StorageErrorKind::Other("pool mutex poisoned".into()))
        })?;
        let conn = guard.as_mut().ok_or_else(pool_closed)?;
        f(conn)
    }

    /// Run a closure inside a SQLite transaction. Commits on Ok, rolls
    /// back on Err.
    pub(crate) fn transaction<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<R>,
    {
        let mut guard = self.conn.lock().map_err(|_| {
            CoreError::Storage(StorageErrorKind::Other("pool mutex poisoned".into()))
        })?;
        let conn = guard.as_mut().ok_or_else(pool_closed)?;
        let tx = conn
            .transaction()
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("begin tx: {e}"))))?;
        let result = f(&tx)?;
        tx.commit()
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("commit: {e}"))))?;
        Ok(result)
    }

    /// Graceful shutdown: checkpoint the WAL into the main DB, close the
    /// connection, encrypt plaintext → ciphertext, and remove the plaintext
    /// DB, its WAL/SHM sidecars, and the sentinel.
    ///
    /// Takes `&self` and is idempotent: it can be called through a shared
    /// `Arc<Pool>` at daemon teardown (sole ownership is NOT required), and a
    /// second call (or the `Drop` backstop) is a no-op once the plaintext is
    /// gone. In-memory pools (`persistent == false`) are a no-op.
    pub fn close(&self) -> Result<()> {
        if !self.persistent {
            return Ok(());
        }
        // Already closed (plaintext removed by a prior close) → nothing to do.
        // This guard is checked without holding a lock; it is safe because the
        // daemon teardown closes the pool from a single thread, and every other
        // `Arc<Pool>` clone is dropped sequentially after that (so a clone's
        // `Drop`-driven close only ever runs once the plaintext is already gone).
        if !self.working_path.exists() {
            return Ok(());
        }
        {
            let mut guard = self.conn.lock().map_err(|_| {
                CoreError::Storage(StorageErrorKind::Other(
                    "pool mutex poisoned during close".into(),
                ))
            })?;
            if let Some(conn) = guard.as_ref() {
                conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                    .map_err(|e| {
                        CoreError::Storage(StorageErrorKind::Other(format!("wal_checkpoint: {e}")))
                    })?;
            }
            let _ = guard.take(); // drop the connection so SQLite releases the files
        }
        encrypt_db(&self.working_path, &self.encrypted_path, &self.passphrase)?;
        remove_plaintext_artifacts(&self.working_path, &self.sentinel_path);
        Ok(())
    }

    /// Return the highest applied migration version. Reads from the
    /// `schema_version` table that the migrations runner maintains.
    pub fn schema_version(&self) -> Result<u32> {
        self.with(|c| {
            let v: u32 = c
                .query_row(
                    "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("schema_version: {e}")))
                })?;
            Ok(v)
        })
    }

    /// Write a consistent, encrypted snapshot of the live DB to `out_age`
    /// without closing the pool. Checkpoints the WAL, `VACUUM INTO` a temp
    /// plaintext copy, encrypts it under the storage passphrase, and removes
    /// the temp. Used by `Command::ExportBackup`.
    pub(crate) fn snapshot_encrypted(&self, out_age: &Path) -> Result<()> {
        let snap = self.working_path.with_extension("snapshot");
        {
            let guard = self.conn.lock().map_err(|_| {
                CoreError::Storage(StorageErrorKind::Other("pool mutex poisoned".into()))
            })?;
            let conn = guard
                .as_ref()
                .ok_or_else(|| CoreError::Storage(StorageErrorKind::Other("pool closed".into())))?;
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("checkpoint: {e}")))
                })?;
            // VACUUM INTO writes a consistent snapshot even with an active WAL.
            conn.execute("VACUUM INTO ?1", [snap.to_string_lossy().as_ref()])
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("vacuum into: {e}")))
                })?;
        }
        let res = encrypt_db(&snap, out_age, &self.passphrase);
        let _ = std::fs::remove_file(&snap); // always clean up the plaintext temp
        res
    }

    /// Test-only: construct a Pool from an in-memory connection. Skips
    /// all encryption + file-path bookkeeping. Used by repo unit tests
    /// and integration tests under the `test-harness` feature.
    #[cfg(any(test, feature = "test-harness"))]
    #[allow(clippy::expect_used)]
    pub fn in_memory() -> Self {
        let mut conn = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
        apply_pragmas(&conn).expect("pragmas");
        crate::storage::migrations::apply(&mut conn).expect("migrations");
        Self {
            conn: Mutex::new(Some(conn)),
            encrypted_path: PathBuf::from("/dev/null"),
            working_path: PathBuf::from("/dev/null"),
            sentinel_path: PathBuf::from("/dev/null"),
            persistent: false,
            passphrase: Zeroizing::new(String::new()),
        }
    }

    /// Test-only: take the connection out of the pool to simulate a
    /// post-close state, so accessor error paths can be exercised.
    #[cfg(any(test, feature = "test-harness"))]
    pub(crate) fn take_conn_for_test(&self) {
        if let Ok(mut g) = self.conn.lock() {
            let _ = g.take();
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        // Best-effort backstop for an abnormal exit / a clone that outlived an
        // explicit close. `close` is guarded (in-memory + already-closed → no-op)
        // and idempotent; errors can't be returned from Drop, so log + swallow.
        if let Err(e) = self.close() {
            tracing::warn!(error = %e, "Pool::drop close failed; plaintext may remain");
        }
    }
}

fn apply_pragmas(conn: &rusqlite::Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| {
            CoreError::Storage(StorageErrorKind::Other(format!("pragma foreign_keys: {e}")))
        })?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| {
            CoreError::Storage(StorageErrorKind::Other(format!("pragma journal_mode: {e}")))
        })?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| {
            CoreError::Storage(StorageErrorKind::Other(format!("pragma synchronous: {e}")))
        })?;
    Ok(())
}

fn decrypt_db(encrypted: &Path, plaintext: &Path, passphrase: &Zeroizing<String>) -> Result<()> {
    let ciphertext = std::fs::read(encrypted)?;
    let decryptor = age::Decryptor::new_buffered(&ciphertext[..])
        .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("age decryptor: {e}"))))?;
    if !decryptor.is_scrypt() {
        return Err(CoreError::Storage(StorageErrorKind::Other(
            "unexpected age recipient type on storage DB".into(),
        )));
    }
    let identity = age::scrypt::Identity::new(age::secrecy::SecretString::from(
        passphrase.as_str().to_string(),
    ));
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("age decrypt: {e}"))))?;

    use std::io::{Read, Write};
    let mut out = std::fs::File::create(plaintext).map_err(|e| {
        CoreError::Storage(StorageErrorKind::Other(format!("create plaintext: {e}")))
    })?;
    let mut buf = [0u8; 8192];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("age read: {e}"))))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| {
            CoreError::Storage(StorageErrorKind::Other(format!("write plaintext: {e}")))
        })?;
    }
    out.sync_all()
        .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("sync plaintext: {e}"))))?;
    Ok(())
}

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

fn encrypt_db(plaintext: &Path, encrypted: &Path, passphrase: &Zeroizing<String>) -> Result<()> {
    let plaintext_bytes = std::fs::read(plaintext)?;
    let encryptor = age::Encryptor::with_user_passphrase(age::secrecy::SecretString::from(
        passphrase.as_str().to_string(),
    ));

    let mut ciphertext = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut ciphertext)
        .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("age wrap: {e}"))))?;
    use std::io::Write;
    writer
        .write_all(&plaintext_bytes)
        .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("age write: {e}"))))?;
    writer
        .finish()
        .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("age finish: {e}"))))?;

    // Atomic replace via tempfile + rename.
    let tmp_path = encrypted.with_extension("age.tmp");
    std::fs::write(&tmp_path, &ciphertext).map_err(|e| {
        CoreError::Storage(StorageErrorKind::Other(format!(
            "write ciphertext tmp: {e}"
        )))
    })?;
    std::fs::rename(&tmp_path, encrypted).map_err(|e| {
        CoreError::Storage(StorageErrorKind::Other(format!("rename ciphertext: {e}")))
    })?;
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
                .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))
            })
            .unwrap();
        assert_eq!(count, 1);
    }

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

        assert!(!tmp.path().join("skattr.sqlite").exists());
        assert!(!tmp.path().join("skattr.sqlite-wal").exists());
        assert!(!tmp.path().join("skattr.sqlite-shm").exists());
        assert!(tmp.path().join("skattr.sqlite.age").exists());

        let pool = Pool::open(tmp.path(), &seed).unwrap();
        let ts: i64 = pool
            .with(|c| {
                c.query_row("SELECT created_at FROM identity WHERE id = 1", [], |r| {
                    r.get(0)
                })
                .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))
            })
            .unwrap();
        assert_eq!(ts, 7);
        pool.close().unwrap();
    }

    #[test]
    fn accessor_after_close_returns_typed_error() {
        let pool = Pool::in_memory();
        pool.take_conn_for_test();
        let err = pool.with(|_| Ok(())).expect_err("closed pool must error");
        assert!(matches!(err, CoreError::Storage(_)));
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
                rusqlite::params![&[0xAAu8; 32][..], 12345i64],
            )
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))?;
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
                    |r| Ok((r.get::<_, i64>(0)? as usize, r.get(1)?)),
                )
                .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))
            })
            .unwrap();
        assert_eq!(pub_len, 32);
        assert_eq!(ts, 12345);
        pool.close().unwrap();
    }

    #[test]
    fn crash_residue_is_reencrypted_on_open_and_sentinel_tracks_live_state() {
        let tmp = tempfile::tempdir().unwrap();
        let seed = Seed::generate().unwrap();

        // First clean session: sentinel exists while live; close removes it.
        // `seen_messages` is an unconstrained multi-row table (`identity` is a
        // singleton via `CHECK (id = 1)`, so it cannot hold two rows).
        let pool = Pool::open(tmp.path(), &seed).unwrap();
        assert!(tmp.path().join("skattr.sqlite.open").exists());
        pool.with_mut(|c| {
            c.execute(
                "INSERT INTO seen_messages (sender, message_id, seen_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![&[0xBBu8; 32][..], &[0x01u8; 16][..], 11i64],
            )
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))?;
            Ok(())
        })
        .unwrap();
        pool.close().unwrap();
        assert!(
            !tmp.path().join("skattr.sqlite.open").exists(),
            "sentinel removed on clean close"
        );

        // Simulate a crash mid-session: open, write more, then mem::forget so no
        // close + no Drop runs — plaintext + sentinel are left on disk.
        {
            let pool = Pool::open(tmp.path(), &seed).unwrap();
            pool.with_mut(|c| {
                c.execute(
                    "INSERT INTO seen_messages (sender, message_id, seen_at) VALUES (?1, ?2, ?3)",
                    rusqlite::params![&[0xCCu8; 32][..], &[0x02u8; 16][..], 22i64],
                )
                .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))?;
                c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                    .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))?;
                Ok(())
            })
            .unwrap();
            std::mem::forget(pool);
        }
        assert!(
            tmp.path().join("skattr.sqlite").exists(),
            "crash left plaintext residue"
        );

        // Next boot: open detects residue and re-encrypts to .age; both rows survive.
        let pool = Pool::open(tmp.path(), &seed).unwrap();
        let n: i64 = pool
            .with(|c| {
                c.query_row("SELECT COUNT(*) FROM seen_messages", [], |r| r.get(0))
                    .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))
            })
            .unwrap();
        assert_eq!(n, 2, "both rows survive crash + re-encrypt-on-boot");
        pool.close().unwrap();

        // The reopened+closed .age reflects both rows.
        let pool = Pool::open(tmp.path(), &seed).unwrap();
        let n: i64 = pool
            .with(|c| {
                c.query_row("SELECT COUNT(*) FROM seen_messages", [], |r| r.get(0))
                    .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))
            })
            .unwrap();
        assert_eq!(n, 2);
        pool.close().unwrap();
    }

    #[test]
    fn drop_without_close_encrypts_persistent_pool() {
        let tmp = tempfile::tempdir().unwrap();
        let seed = Seed::generate().unwrap();
        {
            let pool = Pool::open(tmp.path(), &seed).unwrap();
            pool.with_mut(|c| {
                c.execute(
                    "INSERT INTO seen_messages (sender, message_id, seen_at) VALUES (?1, ?2, ?3)",
                    rusqlite::params![&[0xDDu8; 32][..], &[0x01u8; 16][..], 33i64],
                )
                .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))?;
                Ok(())
            })
            .unwrap();
            // No close(): pool drops at end of scope → Drop safety-net runs.
        }
        assert!(
            !tmp.path().join("skattr.sqlite").exists(),
            "Drop removed plaintext"
        );
        assert!(
            !tmp.path().join("skattr.sqlite.open").exists(),
            "Drop removed sentinel"
        );
        assert!(
            tmp.path().join("skattr.sqlite.age").exists(),
            "Drop encrypted to .age"
        );
        let pool = Pool::open(tmp.path(), &seed).unwrap();
        let n: i64 = pool
            .with(|c| {
                c.query_row("SELECT COUNT(*) FROM seen_messages", [], |r| r.get(0))
                    .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))
            })
            .unwrap();
        assert_eq!(n, 1);
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
        let pool = Pool::in_memory();
        drop(pool); // must not panic / must not touch /dev/null
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
                rusqlite::params![&[0xBBu8; 32][..], 999i64],
            )
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))?;
            Ok(())
        })
        .unwrap();
        let count: i64 = pool
            .with(|c| {
                c.query_row("SELECT COUNT(*) FROM identity", [], |r| r.get(0))
                    .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))
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
                rusqlite::params![&[0xCCu8; 32][..], 100i64],
            )
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))?;
            Err(CoreError::Storage(StorageErrorKind::Other(
                "force rollback".into(),
            )))
        });
        assert!(err.is_err());
        let count: i64 = pool
            .with(|c| {
                c.query_row("SELECT COUNT(*) FROM identity", [], |r| r.get(0))
                    .map_err(|e| CoreError::Storage(StorageErrorKind::Other(e.to_string())))
            })
            .unwrap();
        assert_eq!(count, 0, "transaction closure Err must roll back");
    }

    #[test]
    fn snapshot_encrypted_produces_decryptable_db() {
        let dir = tempfile::tempdir().unwrap();
        let seed = crate::identity::Seed::generate().unwrap();
        let pool = Pool::open(dir.path(), &seed).unwrap();
        // write a row so the snapshot has content
        pool.with_mut(|c| {
            c.execute("CREATE TABLE t(x)", []).unwrap();
            c.execute("INSERT INTO t VALUES (42)", []).unwrap();
            Ok(())
        })
        .unwrap();
        let out = dir.path().join("snap.age");
        pool.snapshot_encrypted(&out).unwrap();
        assert!(out.exists(), "snapshot .age written");
        // temp plaintext snapshot must be gone
        assert!(!dir.path().join("skattr.sqlite.snapshot").exists());
        // pool is still usable after snapshot
        pool.close().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn open_sets_data_dir_0700() {
        use std::os::unix::fs::PermissionsExt;
        let parent = tempfile::tempdir().unwrap();
        let data_dir = parent.path().join("skattr-data");
        let seed = crate::identity::Seed::generate().unwrap();
        let _pool = Pool::open(&data_dir, &seed).unwrap();
        let mode = std::fs::metadata(&data_dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "data_dir must be private (0700)");
    }
}

#[cfg(test)]
mod schema_version_tests {
    use super::*;

    #[test]
    fn schema_version_returns_latest_after_open() {
        let tmp = tempfile::tempdir().unwrap();
        let seed = Seed::generate().unwrap();
        let pool = Pool::open(tmp.path(), &seed).unwrap();
        let v = pool.schema_version().unwrap();
        assert!(v >= 9, "expected schema_version >= 9, got {v}");
    }
}
