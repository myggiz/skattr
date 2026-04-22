// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Crypto + storage provider for OpenMLS.
//!
//! Wraps `openmls_rust_crypto::OpenMlsRustCrypto`, which itself composes
//! `RustCrypto` (AEAD / hash / sig / HKDF / rand) with `MemoryStorage`
//! (in-process `HashMap<Vec<u8>, Vec<u8>>`). Persistence is
//! checkpoint-snapshot: after every state-advancing call on a group,
//! the caller serializes the provider via [`MlsProvider::snapshot`]
//! and writes the blob via `MlsGroupRepo`. On load, the reverse.
//!
//! The snapshot format is ciborium-encoded `HashMap<Vec<u8>, Vec<u8>>`
//! — deterministic enough across restarts, readable by anyone with the
//! schema, and fits the existing `mls_groups.state_blob` column.

use std::collections::HashMap;

use openmls_rust_crypto::OpenMlsRustCrypto;
use openmls_traits::OpenMlsProvider as _;

use crate::error::{CoreError, Result};

/// Crypto + storage provider for OpenMLS.
#[derive(Debug)]
#[cfg_attr(not(feature = "test-harness"), allow(dead_code))]
pub struct MlsProvider {
    inner: OpenMlsRustCrypto,
}

impl MlsProvider {
    /// Fresh provider with empty in-memory storage.
    pub fn new() -> Self {
        Self {
            inner: OpenMlsRustCrypto::default(),
        }
    }

    /// Borrow the underlying OpenMLS provider.
    pub(crate) fn as_openmls(&self) -> &OpenMlsRustCrypto {
        &self.inner
    }

    /// Serialize the in-memory key-value storage into a ciborium blob.
    pub(crate) fn snapshot(&self) -> Result<Vec<u8>> {
        let storage = self.inner.storage();
        let guard = storage
            .values
            .read()
            .map_err(|_| CoreError::Mls("mls: snapshot: poisoned storage lock".into()))?;
        let map: HashMap<Vec<u8>, Vec<u8>> = guard.clone();
        drop(guard);

        let mut out = Vec::new();
        ciborium::ser::into_writer(&map, &mut out)
            .map_err(|e| CoreError::Mls(format!("mls: snapshot: cbor encode: {e}")))?;
        Ok(out)
    }

    /// Rehydrate a provider from a ciborium snapshot.
    pub(crate) fn load(bytes: &[u8]) -> Result<Self> {
        let map: HashMap<Vec<u8>, Vec<u8>> = ciborium::de::from_reader(bytes)
            .map_err(|e| CoreError::Mls(format!("mls: load: cbor decode: {e}")))?;
        let provider = Self::new();
        {
            let storage = provider.inner.storage();
            let mut guard = storage
                .values
                .write()
                .map_err(|_| CoreError::Mls("mls: load: poisoned storage lock".into()))?;
            *guard = map;
        }
        Ok(provider)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use openmls_traits::OpenMlsProvider as _;

    #[test]
    fn snapshot_load_preserves_stored_bytes() {
        let provider = MlsProvider::new();

        // Inject a known key/value pair by writing directly to the
        // MemoryStorage HashMap. Using a distinctive byte pattern so
        // we can verify it survives the round-trip.
        let key = b"skattr-provider-test-key".to_vec();
        let value = b"skattr-provider-test-value".to_vec();
        {
            let storage = provider.as_openmls().storage();
            let mut guard = storage.values.write().unwrap();
            guard.insert(key.clone(), value.clone());
        }

        let snapshot = provider.snapshot().unwrap();
        assert!(!snapshot.is_empty(), "snapshot must not be empty");

        let restored = MlsProvider::load(&snapshot).unwrap();
        let storage = restored.as_openmls().storage();
        let guard = storage.values.read().unwrap();
        let got = guard
            .get(&key)
            .expect("key must survive snapshot round-trip");
        assert_eq!(got, &value);
    }

    #[test]
    fn snapshot_of_empty_provider_decodes_to_empty_provider() {
        let provider = MlsProvider::new();
        let snapshot = provider.snapshot().unwrap();
        let restored = MlsProvider::load(&snapshot).unwrap();
        let storage = restored.as_openmls().storage();
        let guard = storage.values.read().unwrap();
        assert!(guard.is_empty());
    }

    #[test]
    fn load_rejects_garbage_bytes() {
        let err = MlsProvider::load(&[0xFF, 0xFF, 0xFF, 0xFF])
            .expect_err("load must reject non-ciborium bytes");
        match err {
            CoreError::Mls(s) => assert!(s.starts_with("mls: load:")),
            other => panic!("expected CoreError::Mls, got {other:?}"),
        }
    }
}
