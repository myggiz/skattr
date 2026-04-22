// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! MLS KeyPackage newtype.
//!
//! A KeyPackage binds an identity's signature key to an HPKE init key
//! and a set of capabilities + extensions. The inviter encrypts a
//! Welcome against the invitee's KeyPackage `init_key`. KPs are
//! single-use: 1.C persists them via `KeyPackageRepo`, 1.D enforces.

use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_traits::OpenMlsProvider as _;
use sha2::{Digest, Sha256};
use tls_codec::{Deserialize as _, Serialize as _};

use crate::error::{CoreError, Result};
use crate::identity::IdentityKey;
use crate::mls::ciphersuite::CIPHERSUITE;
use crate::mls::provider::MlsProvider;
use crate::storage::KeyPackageRepo;

/// Ciphersuite code-point as an `openmls::prelude::Ciphersuite`.
pub(crate) const MLS_CIPHERSUITE: Ciphersuite =
    Ciphersuite::MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519;

// Sanity: keep the module-level u16 constant and the openmls enum in sync.
const _: () = {
    assert!(CIPHERSUITE == MLS_CIPHERSUITE as u16);
};

/// A freshly-generated MLS KeyPackage, ready to be shared with a peer.
#[derive(Debug)]
pub struct KeyPackage {
    inner: openmls::key_packages::KeyPackage,
}

impl KeyPackage {
    /// Generate a fresh KeyPackage bound to `identity` and register it
    /// with `provider`. Persists the package + its hash via `kp_repo`
    /// with `direction = "ours"` and `consumed = false`.
    pub(crate) fn generate(
        identity: &IdentityKey,
        provider: &MlsProvider,
        kp_repo: &KeyPackageRepo<'_>,
    ) -> Result<Self> {
        let signer = signer_from_identity(identity, provider)?;
        let cwk = credential_with_key(identity, &signer);

        let kp_bundle = openmls::key_packages::KeyPackage::builder()
            .build(MLS_CIPHERSUITE, provider.as_openmls(), &signer, cwk)
            .map_err(|e| CoreError::Mls(format!("mls: key_package builder: {e:?}")))?;
        let kp = kp_bundle.key_package().clone();

        let bytes = kp
            .tls_serialize_detached()
            .map_err(|e| CoreError::Mls(format!("mls: key_package serialize: {e}")))?;
        let hash = sha256(&bytes);
        kp_repo.insert(&hash, &bytes, "ours")?;

        Ok(Self { inner: kp })
    }

    /// Serialize to TLS-codec wire bytes for transmission.
    pub(crate) fn to_bytes(&self) -> Result<Vec<u8>> {
        self.inner
            .tls_serialize_detached()
            .map_err(|e| CoreError::Mls(format!("mls: key_package serialize: {e}")))
    }

    /// Deserialize from TLS-codec wire bytes. Validates the KeyPackage
    /// via OpenMLS's verification step (signature + ciphersuite).
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let kp_in = openmls::key_packages::KeyPackageIn::tls_deserialize_exact(bytes)
            .map_err(|e| CoreError::Mls(format!("mls: key_package deserialize: {e}")))?;
        let crypto = openmls_rust_crypto::OpenMlsRustCrypto::default();
        let kp = kp_in
            .validate(crypto.crypto(), ProtocolVersion::Mls10)
            .map_err(|e| CoreError::Mls(format!("mls: key_package validate: {e:?}")))?;
        Ok(Self { inner: kp })
    }

    /// 32-byte SHA-256 of the TLS-codec serialization.
    pub(crate) fn hash(&self) -> Result<[u8; 32]> {
        let bytes = self
            .inner
            .tls_serialize_detached()
            .map_err(|e| CoreError::Mls(format!("mls: key_package hash serialize: {e}")))?;
        Ok(sha256(&bytes))
    }

    /// Borrow the underlying OpenMLS type. Used by `Group::add_member`.
    pub(crate) fn as_openmls(&self) -> &openmls::key_packages::KeyPackage {
        &self.inner
    }
}

/// Construct an MLS `SignatureKeyPair` whose private/public halves match
/// `identity`. The returned signer is stored with `provider` via
/// `signer.store` so OpenMLS can look it up later.
pub(crate) fn signer_from_identity(
    identity: &IdentityKey,
    provider: &MlsProvider,
) -> Result<SignatureKeyPair> {
    let secret_bytes = identity.ed25519_seed().to_vec();
    let public_bytes = identity.public().0.to_vec();
    let signer = SignatureKeyPair::from_raw(SignatureScheme::ED25519, secret_bytes, public_bytes);
    signer
        .store(provider.as_openmls().storage())
        .map_err(|e| CoreError::Mls(format!("mls: signer store: {e:?}")))?;
    Ok(signer)
}

/// Build a `CredentialWithKey` wrapping the identity's Ed25519 public
/// key inside a `BasicCredential` whose identity payload is the raw
/// public-key bytes (same payload on both sides, makes the ACL check
/// trivial — we ignore BasicCredential contents for auth, the
/// X25519-bound Noise handshake already did identity verification).
pub(crate) fn credential_with_key(
    identity: &IdentityKey,
    signer: &SignatureKeyPair,
) -> CredentialWithKey {
    let credential = BasicCredential::new(identity.public().0.to_vec());
    CredentialWithKey {
        credential: credential.into(),
        signature_key: signer.public().into(),
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::storage::Pool;

    fn setup() -> (IdentityKey, MlsProvider, Pool) {
        let id = IdentityKey::generate().unwrap();
        let provider = MlsProvider::new();
        let pool = Pool::in_memory();
        (id, provider, pool)
    }

    #[test]
    fn generate_persists_row_in_key_packages_repo() {
        let (id, provider, pool) = setup();
        let repo = KeyPackageRepo::new(&pool);
        let kp = KeyPackage::generate(&id, &provider, &repo).unwrap();

        let hash = kp.hash().unwrap();
        let (bytes, consumed) = repo.get(&hash).unwrap().unwrap();
        assert!(!bytes.is_empty());
        assert!(!consumed);
    }

    #[test]
    fn to_bytes_from_bytes_round_trips() {
        let (id, provider, pool) = setup();
        let repo = KeyPackageRepo::new(&pool);
        let kp = KeyPackage::generate(&id, &provider, &repo).unwrap();

        let bytes = kp.to_bytes().unwrap();
        let restored = KeyPackage::from_bytes(&bytes).unwrap();
        assert_eq!(kp.hash().unwrap(), restored.hash().unwrap());
    }

    #[test]
    fn hash_is_stable_across_calls() {
        let (id, provider, pool) = setup();
        let repo = KeyPackageRepo::new(&pool);
        let kp = KeyPackage::generate(&id, &provider, &repo).unwrap();
        assert_eq!(kp.hash().unwrap(), kp.hash().unwrap());
    }

    #[test]
    fn distinct_identities_yield_distinct_hashes() {
        let id1 = IdentityKey::generate().unwrap();
        let id2 = IdentityKey::generate().unwrap();
        let provider1 = MlsProvider::new();
        let provider2 = MlsProvider::new();
        let pool = Pool::in_memory();
        let repo = KeyPackageRepo::new(&pool);

        let kp1 = KeyPackage::generate(&id1, &provider1, &repo).unwrap();
        let kp2 = KeyPackage::generate(&id2, &provider2, &repo).unwrap();
        assert_ne!(kp1.hash().unwrap(), kp2.hash().unwrap());
    }

    #[test]
    fn from_bytes_rejects_garbage() {
        let err = KeyPackage::from_bytes(&[0u8, 1, 2, 3]).expect_err("must reject garbage");
        match err {
            CoreError::Mls(s) => assert!(s.starts_with("mls: key_package")),
            other => panic!("expected CoreError::Mls, got {other:?}"),
        }
    }
}
