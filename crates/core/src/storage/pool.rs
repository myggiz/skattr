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
pub struct Pool {
    conn: Mutex<rusqlite::Connection>,
    encrypted_path: PathBuf,
    working_path: PathBuf,
    /// Age passphrase (hex of the HKDF output). Held by the pool so
    /// `close()` can re-encrypt without re-deriving.
    passphrase: Zeroizing<String>,
}

impl Pool {
    /// Open (or create) the storage DB under `data_dir`, keyed by `seed`.
    pub fn open(data_dir: &Path, seed: &Seed) -> Result<Self> {
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
    pub fn close(self) -> Result<()> {
        let conn = self
            .conn
            .into_inner()
            .map_err(|_| CoreError::Storage("pool mutex poisoned during close".into()))?;
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
                rusqlite::params![&[0xAAu8; 32][..], 12345i64],
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
                    |r| Ok((r.get::<_, i64>(0)? as usize, r.get(1)?)),
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
                rusqlite::params![&[0xBBu8; 32][..], 999i64],
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
                rusqlite::params![&[0xCCu8; 32][..], 100i64],
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
