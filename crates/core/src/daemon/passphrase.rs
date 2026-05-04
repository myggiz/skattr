// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! ChangePassphrase: stage-then-rename atomic re-key spanning the
//! identity vault and the storage age key. Recovery is deterministic
//! from on-disk file fingerprints — never needs the OLD passphrase.
//!
//! See docs/superpowers/specs/2026-05-04-phase-2f-settings-history-design.md
//! § "ChangePassphrase — journaled atomic re-key" for the full spec.

use std::path::Path;

use serde::{Deserialize, Serialize};
use zeroize::ZeroizeOnDrop;

use crate::error::{CoreError, Result};

/// Filenames are siblings of `data_dir/identity.vault` and `age-key`.
pub(crate) const JOURNAL_FILE: &str = "passphrase-rekey.journal";
pub(crate) const VAULT_STAGED: &str = "identity.vault.staged";
pub(crate) const AGE_KEY_STAGED: &str = "age-key.staged";

#[derive(Debug, Serialize, Deserialize, ZeroizeOnDrop)]
pub(crate) struct RekeyJournal {
    /// = 1 in 2.F. Increment on any incompatible journal-format change.
    pub version: u8,
    /// Argon2id salt for the NEW passphrase.
    pub new_salt: [u8; 16],
    pub started_unix: u64,
}

impl RekeyJournal {
    pub fn write(&self, data_dir: &Path) -> Result<()> {
        let path = data_dir.join(JOURNAL_FILE);
        let tmp = data_dir.join(format!("{JOURNAL_FILE}.tmp"));
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(self, &mut bytes)
            .map_err(|e| CoreError::Config(format!("journal encode: {e}")))?;
        write_atomic_0600(&tmp, &path, &bytes)
    }

    pub fn read(data_dir: &Path) -> Result<Option<Self>> {
        let path = data_dir.join(JOURNAL_FILE);
        match std::fs::read(&path) {
            Ok(bytes) => {
                let j: Self = ciborium::de::from_reader(bytes.as_slice())
                    .map_err(|e| CoreError::Config(format!("journal decode: {e}")))?;
                Ok(Some(j))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(CoreError::Config(format!("journal read: {e}"))),
        }
    }

    pub fn delete(data_dir: &Path) -> Result<()> {
        let path = data_dir.join(JOURNAL_FILE);
        match std::fs::remove_file(&path) {
            Ok(()) => fsync_dir(data_dir),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(CoreError::Config(format!("journal delete: {e}"))),
        }
    }
}

pub(crate) fn write_atomic_0600(tmp: &Path, final_path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(tmp)
            .map_err(|e| CoreError::Config(format!("create {}: {e}", tmp.display())))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = f
                .metadata()
                .map_err(|e| CoreError::Config(format!("stat: {e}")))?
                .permissions();
            perm.set_mode(0o600);
            f.set_permissions(perm)
                .map_err(|e| CoreError::Config(format!("chmod: {e}")))?;
        }
        f.write_all(bytes)
            .map_err(|e| CoreError::Config(format!("write: {e}")))?;
        f.sync_all()
            .map_err(|e| CoreError::Config(format!("fsync file: {e}")))?;
    }
    std::fs::rename(tmp, final_path).map_err(|e| CoreError::Config(format!("rename: {e}")))?;
    if let Some(parent) = final_path.parent() {
        fsync_dir(parent)?;
    }
    Ok(())
}

pub(crate) fn fsync_dir(dir: &Path) -> Result<()> {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

pub(crate) fn cleanup_staged(data_dir: &Path) {
    for name in [VAULT_STAGED, AGE_KEY_STAGED] {
        let path = data_dir.join(name);
        let _ = std::fs::remove_file(&path);
    }
    let _ = fsync_dir(data_dir);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn journal_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let j = RekeyJournal {
            version: 1,
            new_salt: [0xAB; 16],
            started_unix: 1_700_000_000,
        };
        j.write(tmp.path()).unwrap();
        let back = RekeyJournal::read(tmp.path()).unwrap().unwrap();
        assert_eq!(back.version, 1);
        assert_eq!(back.new_salt, [0xAB; 16]);
        assert_eq!(back.started_unix, 1_700_000_000);
        RekeyJournal::delete(tmp.path()).unwrap();
        assert!(RekeyJournal::read(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn cleanup_staged_removes_present_files_and_ignores_missing() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(VAULT_STAGED), b"x").unwrap();
        cleanup_staged(tmp.path());
        assert!(!tmp.path().join(VAULT_STAGED).exists());
        assert!(!tmp.path().join(AGE_KEY_STAGED).exists());
        // Idempotent
        cleanup_staged(tmp.path());
    }

    #[test]
    fn write_atomic_0600_creates_file_with_correct_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let final_path = tmp.path().join("test.dat");
        let tmp_path = tmp.path().join("test.dat.tmp");
        write_atomic_0600(&tmp_path, &final_path, b"hello").unwrap();
        let bytes = std::fs::read(&final_path).unwrap();
        assert_eq!(&bytes[..], b"hello");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&final_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        // .tmp file was renamed away
        assert!(!tmp_path.exists());
    }
}
