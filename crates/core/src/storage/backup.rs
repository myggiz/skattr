// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::err_expect)
)]

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

use super::StorageErrorKind;
use crate::error::{CoreError, Result};
use crate::identity::derive::{hkdf_expand, INFO_BACKUP_V1};
use crate::identity::Seed;

const BACKUP_FILES: &[&str] = &["identity.vault", "hs.key.age", "skattr.sqlite.age"];

/// Build a gzip-compressed tar of `members` (each is `(source_path,
/// archive_name)`), age-encrypt the tarball under `passphrase`, and
/// atomically write the result to `out_path`.
fn write_encrypted_archive(
    members: &[(&Path, &str)],
    out_path: &Path,
    passphrase: &Zeroizing<String>,
) -> Result<()> {
    // Build gzipped tar in memory.
    let mut tar_gz = Vec::new();
    {
        let gz = flate2::write::GzEncoder::new(&mut tar_gz, flate2::Compression::default());
        let mut builder = tar::Builder::new(gz);
        for (src, name) in members {
            builder.append_path_with_name(src, name).map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("tar append {name}: {e}")))
            })?;
        }
        builder
            .into_inner()
            .and_then(|gz| gz.finish())
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("tar/gz finish: {e}")))
            })?;
    }

    // Age-encrypt the gzipped tarball.
    let encryptor = age::Encryptor::with_user_passphrase(age::secrecy::SecretString::from(
        passphrase.as_str().to_string(),
    ));

    let mut ciphertext = Vec::new();
    let mut writer = encryptor.wrap_output(&mut ciphertext).map_err(|e| {
        CoreError::Storage(StorageErrorKind::Other(format!("age wrap backup: {e}")))
    })?;
    writer.write_all(&tar_gz).map_err(|e| {
        CoreError::Storage(StorageErrorKind::Other(format!("age write backup: {e}")))
    })?;
    writer.finish().map_err(|e| {
        CoreError::Storage(StorageErrorKind::Other(format!("age finish backup: {e}")))
    })?;

    // Atomic write.
    let tmp_path = out_path.with_extension("tmp");
    std::fs::write(&tmp_path, &ciphertext)?;
    std::fs::rename(&tmp_path, out_path)?;
    Ok(())
}

/// Write a backup archive to `out_path`. Fails if any of the three
/// source files are missing from `data_dir`.
pub(crate) fn export_backup(data_dir: &Path, out_path: &Path, seed: &Seed) -> Result<()> {
    for name in BACKUP_FILES {
        let p = data_dir.join(name);
        if !p.exists() {
            return Err(CoreError::Storage(StorageErrorKind::Other(format!(
                "missing backup input: {}",
                p.display()
            ))));
        }
    }

    let vault = data_dir.join("identity.vault");
    let hs_key = data_dir.join("hs.key.age");
    let db_age = data_dir.join("skattr.sqlite.age");
    let members: [(&Path, &str); 3] = [
        (&vault, "identity.vault"),
        (&hs_key, "hs.key.age"),
        (&db_age, "skattr.sqlite.age"),
    ];

    let key = hkdf_expand::<32>(seed.as_bytes(), INFO_BACKUP_V1)?;
    let passphrase = Zeroizing::new(hex::encode(key.as_ref()));
    write_encrypted_archive(&members, out_path, &passphrase)
}

/// Bundle `identity.vault` + `hs.key.age` (from `data_dir`) and the DB snapshot
/// `.age` at `db_age` (stored as `skattr.sqlite.age`) into an age-encrypted
/// archive at `out_path`, encrypted under `backup_key`. Used by live export.
pub(crate) fn export_backup_from_parts(
    data_dir: &Path,
    db_age: &Path,
    out_path: &Path,
    backup_key: &[u8; 32],
) -> Result<()> {
    let vault = data_dir.join("identity.vault");
    let hs_key = data_dir.join("hs.key.age");
    let members: [(&Path, &str); 3] = [
        (&vault, "identity.vault"),
        (&hs_key, "hs.key.age"),
        (db_age, "skattr.sqlite.age"),
    ];
    let passphrase = Zeroizing::new(hex::encode(backup_key));
    write_encrypted_archive(&members, out_path, &passphrase)
}

/// Read a backup archive from `archive_path` and extract the three
/// files into `data_dir`. Refuses to overwrite any existing file in
/// the target directory — the caller is expected to restore into a
/// clean data_dir.
pub(crate) fn import_backup(archive_path: &Path, data_dir: &Path, seed: &Seed) -> Result<()> {
    for name in BACKUP_FILES {
        if data_dir.join(name).exists() {
            return Err(CoreError::Storage(StorageErrorKind::Other(format!(
                "refusing to overwrite existing {}; restore into a clean data_dir",
                name
            ))));
        }
    }

    let ciphertext = std::fs::read(archive_path)?;
    let key = hkdf_expand::<32>(seed.as_bytes(), INFO_BACKUP_V1)?;
    let passphrase = Zeroizing::new(hex::encode(key.as_ref()));

    let decryptor = age::Decryptor::new_buffered(&ciphertext[..]).map_err(|e| {
        CoreError::Storage(StorageErrorKind::Other(format!(
            "age decryptor backup: {e}"
        )))
    })?;
    if !decryptor.is_scrypt() {
        return Err(CoreError::Storage(StorageErrorKind::Other(
            "unexpected age recipient type on backup".into(),
        )));
    }
    let identity = age::scrypt::Identity::new(age::secrecy::SecretString::from(
        passphrase.as_str().to_string(),
    ));
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|e| {
            CoreError::Storage(StorageErrorKind::Other(format!("age decrypt backup: {e}")))
        })?;

    let mut tar_gz = Vec::new();
    reader.read_to_end(&mut tar_gz).map_err(|e| {
        CoreError::Storage(StorageErrorKind::Other(format!(
            "read backup plaintext: {e}"
        )))
    })?;

    // Extract gzipped tarball.
    std::fs::create_dir_all(data_dir)?;
    let gz = flate2::read::GzDecoder::new(&tar_gz[..]);
    let mut archive = tar::Archive::new(gz);
    for entry in archive
        .entries()
        .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("tar entries: {e}"))))?
    {
        let mut entry = entry
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("tar entry: {e}"))))?;
        let name = entry
            .path()
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("tar entry path: {e}")))
            })?
            .to_string_lossy()
            .into_owned();
        if !BACKUP_FILES.contains(&name.as_str()) {
            return Err(CoreError::Storage(StorageErrorKind::Other(format!(
                "unexpected backup entry: {name}"
            ))));
        }
        let dest = data_dir.join(&name);
        entry.unpack(&dest).map_err(|e| {
            CoreError::Storage(StorageErrorKind::Other(format!("extract {name}: {e}")))
        })?;
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
    fn export_import_roundtrip_through_real_pool() {
        use crate::storage::Pool;

        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let seed = Seed::generate().unwrap();

        // Real Pool: open, write, close → produces a real skattr.sqlite.age.
        let pool = Pool::open(src.path(), &seed).unwrap();
        pool.with_mut(|c| {
            c.execute(
                "INSERT INTO seen_messages (sender, message_id, seen_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![&[0xEEu8; 32][..], &[0x01u8; 16][..], 55i64],
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
