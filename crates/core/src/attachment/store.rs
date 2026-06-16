// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! On-disk ciphertext chunk store at `<data_dir>/attachments/<hex id>/<index>`.
//! Blobs are AEAD ciphertext (keys live in the MLS-protected manifest), so this
//! is not an at-rest plaintext gap.

use std::path::{Path, PathBuf};

use crate::attachment::reassembler::ChunkSource;
use crate::error::Result;

pub(crate) struct ChunkStore {
    root: PathBuf, // <data_dir>/attachments
}

impl ChunkStore {
    pub(crate) fn new(data_dir: &Path) -> Self {
        Self {
            root: data_dir.join("attachments"),
        }
    }
    fn dir(&self, attachment_id: &[u8; 16]) -> PathBuf {
        self.root.join(hex::encode(attachment_id))
    }
    pub(crate) fn put(
        &self,
        attachment_id: &[u8; 16],
        index: u32,
        ciphertext: &[u8],
    ) -> Result<()> {
        let d = self.dir(attachment_id);
        std::fs::create_dir_all(&d)?;
        let tmp = d.join(format!("{index}.part"));
        std::fs::write(&tmp, ciphertext)?;
        std::fs::rename(&tmp, d.join(index.to_string()))?;
        Ok(())
    }
    pub(crate) fn get_chunk(&self, attachment_id: &[u8; 16], index: u32) -> Result<Vec<u8>> {
        Ok(std::fs::read(
            self.dir(attachment_id).join(index.to_string()),
        )?)
    }
    pub(crate) fn remove(&self, attachment_id: &[u8; 16]) -> Result<()> {
        let d = self.dir(attachment_id);
        if d.exists() {
            std::fs::remove_dir_all(&d)?;
        }
        Ok(())
    }
}

/// Adapter so a `ChunkStore` (bound to one attachment) is a `ChunkSource`.
pub(crate) struct StoreSource<'s> {
    store: &'s ChunkStore,
    attachment_id: [u8; 16],
}
impl<'s> StoreSource<'s> {
    pub(crate) fn new(store: &'s ChunkStore, attachment_id: [u8; 16]) -> Self {
        Self {
            store,
            attachment_id,
        }
    }
}
impl ChunkSource for StoreSource<'_> {
    fn get(&self, index: u32) -> Result<Vec<u8>> {
        self.store.get_chunk(&self.attachment_id, index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_remove_and_chunk_source_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ChunkStore::new(tmp.path());
        let id = [0x55u8; 16];
        store.put(&id, 0, b"chunk-zero").unwrap();
        store.put(&id, 1, b"chunk-one").unwrap();
        assert_eq!(store.get_chunk(&id, 0).unwrap(), b"chunk-zero");
        // ChunkSource impl reads back by index
        let src = StoreSource::new(&store, id);
        assert_eq!(src.get(1).unwrap(), b"chunk-one");
        store.remove(&id).unwrap();
        assert!(store.get_chunk(&id, 0).is_err()); // dir gone
        store.remove(&id).unwrap(); // idempotent (missing dir is ok)
    }
}
