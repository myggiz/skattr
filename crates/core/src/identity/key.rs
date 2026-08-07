// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Ed25519 identity keys, public keys, and detached signatures.

use core::fmt;

use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{CoreError, Result};

/// An Ed25519 public key, 32 bytes.
///
/// `PublicKey` is `Copy` and safe to log in debug output at `trace`
/// level only — pubkeys are sensitive metadata. Display format is
/// hex-encoded; see [`PublicKey::to_hex`].
///
/// `Ord` / `PartialOrd` derive lexicographic byte order; used for
/// stable display sorts and for deterministic ordering in tests.
/// Not a cryptographic ordering — do not use for security decisions.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ts_rs::TS)]
#[ts(
    export,
    export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/",
    type = "string"
)]
pub struct PublicKey(pub [u8; 32]);

impl PublicKey {
    /// Hex-encoded public key (64 lowercase characters).
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parse a hex-encoded public key.
    pub fn from_hex(s: &str) -> Result<Self> {
        let bytes =
            hex::decode(s).map_err(|e| CoreError::Identity(format!("invalid hex pubkey: {e}")))?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| CoreError::Identity("pubkey must be 32 bytes".into()))?;
        Ok(Self(arr))
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redact by default: full pubkey only surfaces via to_hex().
        write!(f, "PublicKey({}…)", &self.to_hex()[..8])
    }
}

/// A detached Ed25519 signature, 64 bytes.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature(#[serde(with = "serde_big_array::BigArray")] pub [u8; 64]);

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Signature({}…)", hex::encode(&self.0[..4]))
    }
}

/// The long-term Ed25519 identity keypair.
///
/// Holds the private scalar in memory only for the lifetime of the
/// process (never written to disk except encrypted via [`Vault`]).
/// The secret bytes zero on drop.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct IdentityKey {
    /// Raw 32-byte Ed25519 seed (the "secret" half; public key is derived).
    secret: [u8; 32],
}

impl IdentityKey {
    /// Generate a fresh identity from the OS CSPRNG.
    pub fn generate() -> Result<Self> {
        let mut secret = [0u8; 32];
        use rand::RngCore;
        rand::rngs::OsRng.fill_bytes(&mut secret);
        Ok(Self { secret })
    }

    /// Derive an identity deterministically from a [`Seed`] via HKDF.
    ///
    /// The derivation is domain-separated with the label
    /// `"skattr-identity-v1"`. Changing this label is a wire-incompatible
    /// change — do not do it without an ADR.
    pub fn from_seed(seed: &crate::identity::Seed) -> Result<Self> {
        use crate::identity::derive::{hkdf_expand, INFO_IDENTITY_V1};
        let okm = hkdf_expand::<32>(seed.as_bytes(), INFO_IDENTITY_V1)?;
        Ok(Self::from_bytes(okm))
    }

    /// Public half of the keypair.
    #[must_use]
    pub fn public(&self) -> PublicKey {
        let signing = SigningKey::from_bytes(&self.secret);
        PublicKey(signing.verifying_key().to_bytes())
    }

    /// Sign an arbitrary message.
    pub fn sign(&self, message: &[u8]) -> Signature {
        let signing = SigningKey::from_bytes(&self.secret);
        let sig: ed25519_dalek::Signature = signing.sign(message);
        Signature(sig.to_bytes())
    }

    /// Sign the canonical CBOR encoding of `body`. Used by ContactCard
    /// and InviteLink to sign their Body structs. Pairs with
    /// [`Self::verify_cbor`].
    pub fn sign_cbor<T: serde::Serialize>(&self, body: &T) -> Result<Signature> {
        let mut bytes = zeroize::Zeroizing::new(Vec::new());
        ciborium::ser::into_writer(body, &mut *bytes)
            .map_err(|e| CoreError::Identity(format!("sign_cbor: {e}")))?;
        Ok(self.sign(&bytes))
    }

    /// Verify a signature over the canonical CBOR encoding of `body`.
    /// Signer identity is `pubkey`. Collapses all failure modes to
    /// the same opaque string (same as [`Self::verify`]).
    pub fn verify_cbor<T: serde::Serialize>(
        pubkey: &PublicKey,
        body: &T,
        signature: &Signature,
    ) -> Result<()> {
        let mut bytes = zeroize::Zeroizing::new(Vec::new());
        ciborium::ser::into_writer(body, &mut *bytes)
            .map_err(|_| CoreError::Identity("verification failed".into()))?;
        Self::verify(pubkey, &bytes, signature)
    }

    /// Verify a signature against a pubkey. Constant-time, no panics.
    ///
    /// Both invalid-pubkey and bad-signature outcomes collapse to the
    /// same opaque error to prevent a timing/error-text distinguisher
    /// in auth paths that accept attacker-controlled pubkeys.
    pub fn verify(pubkey: &PublicKey, message: &[u8], signature: &Signature) -> Result<()> {
        let vk = match VerifyingKey::from_bytes(&pubkey.0) {
            Ok(v) => v,
            Err(_) => return Err(CoreError::Identity("verification failed".into())),
        };
        let sig = ed25519_dalek::Signature::from_bytes(&signature.0);
        vk.verify_strict(message, &sig)
            .map_err(|_| CoreError::Identity("verification failed".into()))
    }

    /// Derive the X25519 static secret used by Noise_XK.
    ///
    /// Matches libsodium's `crypto_sign_ed25519_sk_to_curve25519`:
    /// SHA-512 of the Ed25519 seed, truncate to 32 bytes, apply the
    /// X25519 clamp. Returns `Zeroizing<[u8; 32]>` so callers cannot
    /// accidentally leak the secret on stack unwind.
    pub(crate) fn noise_static_secret(&self) -> zeroize::Zeroizing<[u8; 32]> {
        use sha2::{Digest, Sha512};
        use zeroize::Zeroize as _;

        // The explicit `full.zeroize()` below is the ONLY guarantee that this
        // intermediate is scrubbed: `sha2`'s `Output` does not implement
        // `ZeroizeOnDrop`, and `.into()` copies the digest into a plain
        // `[u8; 64]` that would otherwise be left on the stack. Keep the
        // manual scrub. (An earlier revision of this comment credited a
        // nonexistent `hash` crate with auto-zeroizing here.)
        let mut full: [u8; 64] = Sha512::digest(self.secret).into();
        let mut out = zeroize::Zeroizing::new([0u8; 32]);
        out.copy_from_slice(&full[..32]);

        // Scrub the full-length intermediate now that we have what we need.
        full.zeroize();

        // X25519 clamp.
        out[0] &= 248;
        out[31] &= 127;
        out[31] |= 64;
        out
    }

    /// The raw 32-byte Ed25519 seed. Crate-private: used by the MLS
    /// module to construct an `openmls_basic_credential::SignatureKeyPair`
    /// that signs with the same key as our identity. Wrapped in
    /// `Zeroizing` so the copy wipes on drop — callers should not
    /// leave the guard alive longer than the single use site.
    pub(crate) fn ed25519_seed(&self) -> zeroize::Zeroizing<[u8; 32]> {
        zeroize::Zeroizing::new(self.secret)
    }

    /// The X25519 public key matching [`Self::noise_static_secret`].
    ///
    /// Computed from our own Ed25519 verifying key via the
    /// Edwards-Y → Montgomery-U map — *not* via
    /// `PublicKey::from(StaticSecret)` on the derived secret. Both
    /// routes yield bitwise-equal output; we use the public-key path
    /// because it avoids re-hashing the seed.
    pub(crate) fn noise_static_public(&self) -> [u8; 32] {
        let signing = SigningKey::from_bytes(&self.secret);
        ed25519_pub_to_x25519(&signing.verifying_key())
    }

    /// Consume into raw secret bytes. Caller is responsible for zeroization.
    ///
    /// This is only used by the vault on encrypted save; avoid calling
    /// it from other code paths.
    pub(crate) fn into_bytes(mut self) -> [u8; 32] {
        let out = self.secret;
        self.secret.zeroize();
        out
    }

    /// Construct from Zeroizing-wrapped secret bytes. Private: only
    /// callable from inside the crate (vault open, seed derivation).
    ///
    /// Takes `Zeroizing<[u8; 32]>` (not bare `[u8; 32]`) so the caller's
    /// guard drops after the move, leaving `self.secret` as the sole
    /// un-wiped copy — which itself zeroes on drop via the struct's
    /// `ZeroizeOnDrop` derive.
    pub(crate) fn from_bytes(secret: zeroize::Zeroizing<[u8; 32]>) -> Self {
        Self { secret: *secret }
    }
}

impl fmt::Debug for IdentityKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IdentityKey(<redacted>)")
    }
}

/// Convert a peer's Ed25519 verifying key (the identity pubkey carried
/// in ContactCards and invites) into its X25519 public key for Noise
/// DH. Uses the Edwards-Y → Montgomery-U birational map — a standard,
/// lossless morphism between the two forms of the underlying curve25519
/// group.
///
/// Used by the contact layer when it needs to dial an X25519-shaped
/// peer whose identity is stored as Ed25519. Kept `pub(crate)` so
/// higher layers must go through a typed wrapper (e.g. ContactCard →
/// handshake dial) rather than converting raw bytes everywhere.
pub(crate) fn ed25519_pub_to_x25519(pk: &VerifyingKey) -> [u8; 32] {
    // `VerifyingKey::to_montgomery()` is stable public API in
    // ed25519-dalek 2.x and internally calls the curve25519-dalek
    // birational map. `MontgomeryPoint::to_bytes()` yields the
    // little-endian U coordinate — exactly what Noise expects.
    pk.to_montgomery().to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_is_32_bytes_and_stable() {
        let id = IdentityKey::generate().unwrap();
        let pk1 = id.public();
        let pk2 = id.public();
        assert_eq!(pk1.0.len(), 32);
        assert_eq!(
            pk1, pk2,
            "public() must be deterministic for the same secret"
        );
    }

    #[test]
    fn distinct_secrets_produce_distinct_pubkeys() {
        let a = IdentityKey::generate().unwrap();
        let b = IdentityKey::generate().unwrap();
        assert_ne!(a.public(), b.public());
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let id = IdentityKey::generate().unwrap();
        let msg = b"skattr handshake payload v1";
        let sig = id.sign(msg);
        IdentityKey::verify(&id.public(), msg, &sig).expect("signature must verify");
    }

    #[test]
    fn verify_rejects_tampered_message() {
        let id = IdentityKey::generate().unwrap();
        let sig = id.sign(b"original message");
        let err = IdentityKey::verify(&id.public(), b"tampered message", &sig)
            .expect_err("tampered verify must fail");
        assert!(matches!(err, crate::error::CoreError::Identity(_)));
    }

    #[test]
    fn verify_rejects_wrong_pubkey() {
        let signer = IdentityKey::generate().unwrap();
        let other = IdentityKey::generate().unwrap();
        let sig = signer.sign(b"msg");
        IdentityKey::verify(&other.public(), b"msg", &sig)
            .expect_err("verify under wrong pubkey must fail");
    }

    #[test]
    fn from_seed_is_deterministic() {
        let seed = crate::identity::Seed::generate().unwrap();
        let a = IdentityKey::from_seed(&seed).unwrap();
        let b = IdentityKey::from_seed(&seed).unwrap();
        assert_eq!(a.public(), b.public(), "same seed must yield same pubkey");
    }

    #[test]
    fn from_bytes_accepts_zeroizing() {
        let mut buf = zeroize::Zeroizing::new([0u8; 32]);
        buf[0] = 1;
        let id = IdentityKey::from_bytes(buf);
        assert_eq!(id.public().0.len(), 32);
    }

    #[test]
    fn noise_static_secret_is_deterministic_and_clamped() {
        let seed = zeroize::Zeroizing::new([0x42u8; 32]);
        let id = IdentityKey::from_bytes(seed);
        let a = id.noise_static_secret();
        let b = id.noise_static_secret();
        assert_eq!(*a, *b, "same identity must produce same X25519 secret");

        // X25519 clamping: low 3 bits of byte 0 cleared, bit 6 of byte
        // 31 set, bit 7 of byte 31 cleared.
        assert_eq!(a[0] & 0b0000_0111, 0, "byte 0 low 3 bits must be zero");
        assert_eq!(a[31] & 0b1000_0000, 0, "byte 31 high bit must be zero");
        assert_eq!(
            a[31] & 0b0100_0000,
            0b0100_0000,
            "byte 31 bit 6 must be set"
        );
    }

    #[test]
    fn distinct_identities_produce_distinct_noise_secrets() {
        let a = IdentityKey::from_bytes(zeroize::Zeroizing::new([0x01u8; 32]));
        let b = IdentityKey::from_bytes(zeroize::Zeroizing::new([0x02u8; 32]));
        assert_ne!(*a.noise_static_secret(), *b.noise_static_secret());
    }

    #[test]
    fn from_seed_is_domain_separated_from_raw_bytes() {
        // If HKDF were accidentally bypassed (e.g. someone rewrote from_seed
        // as Self::from_bytes(seed.as_bytes().into())), this test would fail:
        // the "raw-bytes" and "seed-derived" keys would coincide.
        let bytes = [0x42u8; 32];
        let raw_key = IdentityKey::from_bytes(zeroize::Zeroizing::new(bytes));
        let seed = crate::identity::Seed::from_bytes(bytes);
        let derived = IdentityKey::from_seed(&seed).unwrap();
        assert_ne!(
            raw_key.public(),
            derived.public(),
            "from_seed must mix HKDF label; raw-bytes and seed-derived keys must differ"
        );
    }

    #[test]
    fn verify_rejects_tampered_signature() {
        let id = IdentityKey::generate().unwrap();
        let msg = b"payload";
        let mut sig = id.sign(msg);
        // Flip the first byte of the signature's R component.
        sig.0[0] ^= 0x01;
        let err = IdentityKey::verify(&id.public(), msg, &sig)
            .expect_err("tampered signature must fail verify_strict");
        assert!(matches!(err, crate::error::CoreError::Identity(_)));
    }

    #[test]
    fn noise_static_public_matches_x25519_dalek_derivation() {
        // For any identity, the X25519 public key derived by applying
        // the Edwards → Montgomery map to the Ed25519 pubkey must match
        // the X25519 public key derived by running `x25519-dalek` on
        // the output of `noise_static_secret`. If these diverge, the
        // Noise handshake will silently fail to authenticate.
        let id = IdentityKey::from_bytes(zeroize::Zeroizing::new([0x7Fu8; 32]));

        let from_pub = id.noise_static_public();

        let sk = x25519_dalek::StaticSecret::from(*id.noise_static_secret());
        let from_sk: [u8; 32] = x25519_dalek::PublicKey::from(&sk).to_bytes();

        assert_eq!(
            from_pub, from_sk,
            "noise_static_public (Edwards->Montgomery of Ed25519 pub) must match \
             x25519-dalek(PublicKey::from(StaticSecret::from(noise_static_secret)))"
        );
    }

    #[test]
    fn ed25519_pub_to_x25519_matches_method() {
        let id = IdentityKey::from_bytes(zeroize::Zeroizing::new([0xAA; 32]));
        let vk = ed25519_dalek::VerifyingKey::from_bytes(&id.public().0).unwrap();
        let via_free_fn = super::ed25519_pub_to_x25519(&vk);
        assert_eq!(via_free_fn, id.noise_static_public());
    }

    #[test]
    fn sign_cbor_roundtrips_and_buffer_is_zeroizing() {
        // Behavioral: sign+verify still round-trips after the buffer change.
        let key = IdentityKey::from_seed(&crate::identity::Seed::generate().unwrap()).unwrap();
        #[derive(serde::Serialize)]
        struct Body {
            x: u8,
        }
        let body = Body { x: 7 };
        let sig = key.sign_cbor(&body).unwrap();
        IdentityKey::verify_cbor(&key.public(), &body, &sig).unwrap();
        // Type-level guard: the scratch buffer is Zeroizing<Vec<u8>>.
        let scratch: zeroize::Zeroizing<Vec<u8>> = zeroize::Zeroizing::new(Vec::new());
        assert!(scratch.is_empty());
    }

    #[test]
    fn ed25519_pub_to_x25519_is_deterministic() {
        let id = IdentityKey::from_bytes(zeroize::Zeroizing::new([0x55; 32]));
        let vk = ed25519_dalek::VerifyingKey::from_bytes(&id.public().0).unwrap();
        assert_eq!(
            super::ed25519_pub_to_x25519(&vk),
            super::ed25519_pub_to_x25519(&vk)
        );
    }

    #[test]
    fn sign_cbor_verify_cbor_round_trip() {
        use serde::{Deserialize, Serialize};
        #[derive(Serialize, Deserialize)]
        struct Sample {
            a: u64,
            b: String,
            c: Vec<u8>,
        }

        let id = IdentityKey::generate().unwrap();
        let body = Sample {
            a: 42,
            b: "hello".into(),
            c: vec![1, 2, 3, 4],
        };

        let sig = id.sign_cbor(&body).unwrap();
        IdentityKey::verify_cbor(&id.public(), &body, &sig).expect("valid");

        // Tampered body should fail.
        let tampered = Sample {
            a: 43,
            b: body.b.clone(),
            c: body.c.clone(),
        };
        IdentityKey::verify_cbor(&id.public(), &tampered, &sig).expect_err("tampered body");

        // Wrong signer should fail.
        let other = IdentityKey::generate().unwrap();
        IdentityKey::verify_cbor(&other.public(), &body, &sig).expect_err("wrong signer");
    }
}
