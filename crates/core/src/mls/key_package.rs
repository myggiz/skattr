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
use crate::mls::error_kind::MlsErrorKind;
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
    pub fn generate(
        identity: &IdentityKey,
        provider: &MlsProvider,
        kp_repo: &KeyPackageRepo<'_>,
    ) -> Result<Self> {
        let signer = signer_from_identity(identity, provider)?;
        let cwk = credential_with_key(identity, &signer);

        let kp_bundle = openmls::key_packages::KeyPackage::builder()
            .build(MLS_CIPHERSUITE, provider.as_openmls(), &signer, cwk)
            .map_err(|e| {
                CoreError::from(MlsErrorKind::Other(format!(
                    "mls: key_package builder: {e:?}"
                )))
            })?;
        let kp = kp_bundle.key_package().clone();

        let bytes = kp.tls_serialize_detached().map_err(|e| {
            CoreError::from(MlsErrorKind::Other(format!(
                "mls: key_package serialize: {e}"
            )))
        })?;
        let hash = sha256(&bytes);
        kp_repo.insert(&hash, &bytes, "ours")?;

        Ok(Self { inner: kp })
    }

    /// Serialize to TLS-codec wire bytes for transmission.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        self.inner.tls_serialize_detached().map_err(|e| {
            CoreError::from(MlsErrorKind::Other(format!(
                "mls: key_package serialize: {e}"
            )))
        })
    }

    /// Deserialize from TLS-codec wire bytes. Validates the KeyPackage
    /// via OpenMLS's verification step (signature + ciphersuite).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let kp_in =
            openmls::key_packages::KeyPackageIn::tls_deserialize_exact(bytes).map_err(|e| {
                CoreError::from(MlsErrorKind::Other(format!(
                    "mls: key_package deserialize: {e}"
                )))
            })?;
        let crypto = openmls_rust_crypto::OpenMlsRustCrypto::default();
        let kp = kp_in
            .validate(crypto.crypto(), ProtocolVersion::Mls10)
            .map_err(|e| {
                CoreError::from(MlsErrorKind::Other(format!(
                    "mls: key_package validate: {e:?}"
                )))
            })?;
        Ok(Self { inner: kp })
    }

    /// 32-byte SHA-256 of the TLS-codec serialization.
    pub fn hash(&self) -> Result<[u8; 32]> {
        let bytes = self.inner.tls_serialize_detached().map_err(|e| {
            CoreError::from(MlsErrorKind::Other(format!(
                "mls: key_package hash serialize: {e}"
            )))
        })?;
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
    let seed_guard = identity.ed25519_seed();
    let secret_bytes = zeroize::Zeroizing::new(seed_guard.to_vec());
    let public_bytes = identity.public().0.to_vec();
    // `from_raw` takes `Vec<u8>` by value; the `secret_bytes` Zeroizing
    // guard covers the lifetime up to the move into SignatureKeyPair.
    let signer = SignatureKeyPair::from_raw(
        SignatureScheme::ED25519,
        secret_bytes.to_vec(),
        public_bytes,
    );
    signer
        .store(provider.as_openmls().storage())
        .map_err(|e| CoreError::from(MlsErrorKind::Other(format!("mls: signer store: {e:?}"))))?;
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

/// Extract the new-member KeyPackage hash from a TLS-serialized
/// Welcome blob. Used by the inviter to look up the matching
/// `outstanding_invites` row.
///
/// Phase 2 scope: 2-member groups only — Welcomes carry exactly one
/// `EncryptedGroupSecrets`. Returns the first entry's
/// `KeyPackageRef` (32 bytes).
///
/// On parse failure (corrupt bytes, wrong message type, multiple
/// secrets, mis-sized hash) returns `Err(MlsErrorKind::Other(_))`.
pub(crate) fn parse_welcome_kp_hash(welcome: &[u8]) -> crate::error::Result<[u8; 32]> {
    use openmls::framing::{MlsMessageBodyIn, MlsMessageIn};
    use openmls::prelude::tls_codec::Deserialize as _;

    use crate::error::CoreError;
    use crate::mls::error_kind::MlsErrorKind;

    let msg = MlsMessageIn::tls_deserialize_exact(welcome).map_err(|e| {
        CoreError::from(MlsErrorKind::Other(format!(
            "welcome parse: deserialize: {e}"
        )))
    })?;

    let inner = match msg.extract() {
        MlsMessageBodyIn::Welcome(w) => w,
        _ => {
            return Err(CoreError::from(MlsErrorKind::Other(
                "welcome parse: not a Welcome".into(),
            )))
        }
    };

    let secrets = inner.secrets();
    if secrets.is_empty() {
        return Err(CoreError::from(MlsErrorKind::Other(
            "welcome parse: empty secrets".into(),
        )));
    }
    if secrets.len() > 1 {
        return Err(CoreError::from(MlsErrorKind::Other(format!(
            "welcome parse: {} secrets, only 1 supported in Phase 2",
            secrets.len()
        ))));
    }
    let kp_ref = secrets[0].new_member();
    let bytes = kp_ref.as_slice();
    if bytes.len() != 32 {
        return Err(CoreError::from(MlsErrorKind::Other(format!(
            "welcome parse: kp_ref wrong length {}",
            bytes.len()
        ))));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(bytes);
    Ok(arr)
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
mod welcome_hash_tests {
    use super::*;
    use crate::identity::IdentityKey;
    use crate::mls::group::Group;
    use crate::mls::provider::MlsProvider;
    use crate::storage::key_packages::KeyPackageRepo;
    use crate::storage::Pool;

    /// Compute the canonical MLS KeyPackageRef for a KeyPackage.
    ///
    /// This is RefHash("MLS 1.0 KeyPackage Reference", tls_kp_bytes),
    /// NOT a plain SHA-256 of the bytes. The Welcome message routes
    /// using this ref, not bob_kp.hash(). See openmls hash_ref.rs.
    fn kp_ref_from_kp(kp: &KeyPackage) -> [u8; 32] {
        use openmls::ciphersuite::hash_ref::make_key_package_ref;
        use openmls_rust_crypto::OpenMlsRustCrypto;
        use tls_codec::Serialize as _;

        let crypto = OpenMlsRustCrypto::default();
        let tls_bytes = kp.as_openmls().tls_serialize_detached().unwrap();
        let kp_ref = make_key_package_ref(&tls_bytes, MLS_CIPHERSUITE, crypto.crypto()).unwrap();
        let bytes = kp_ref.as_slice();
        assert_eq!(bytes.len(), 32, "SHA-256 ref must be 32 bytes");
        let mut arr = [0u8; 32];
        arr.copy_from_slice(bytes);
        arr
    }

    #[allow(clippy::unwrap_used)]
    #[test]
    fn parse_welcome_kp_hash_returns_invitee_kp_ref() {
        let pool = Pool::in_memory();
        let alice = IdentityKey::from_seed(&crate::identity::Seed::generate().unwrap()).unwrap();
        let bob = IdentityKey::from_seed(&crate::identity::Seed::generate().unwrap()).unwrap();

        let bob_provider = MlsProvider::new();
        let kp_repo = KeyPackageRepo::new(&pool);
        let bob_kp = KeyPackage::generate(&bob, &bob_provider, &kp_repo).unwrap();

        // The canonical expected value is the MLS KeyPackageRef (RefHash),
        // not the plain SHA-256 from bob_kp.hash(). The Welcome message
        // routes by KeyPackageRef, so parse_welcome_kp_hash must return
        // that value.
        let expected_kp_ref = kp_ref_from_kp(&bob_kp);

        let mut alice_group =
            Group::create_solo(&alice, None, MlsProvider::new()).unwrap();
        let (welcome, _commit) = alice_group.add_member(&bob_kp, None).unwrap();

        let parsed = parse_welcome_kp_hash(&welcome).unwrap();
        assert_eq!(parsed, expected_kp_ref);
        assert_eq!(parsed.len(), 32);
        assert!(parsed.iter().any(|&b| b != 0), "must be non-zero");
    }
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
            CoreError::Mls(MlsErrorKind::Other(s)) => assert!(s.starts_with("mls: key_package")),
            other => panic!("expected CoreError::Mls(Other(_)), got {other:?}"),
        }
    }
}
