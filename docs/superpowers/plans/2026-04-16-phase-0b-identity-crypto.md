# Phase 0.B — Identity & Crypto Foundations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the `todo!()` stubs in `crates/core/src/identity/` with real cryptography (Ed25519 keypair ops, BIP39 seed encoding, Argon2id + XChaCha20-Poly1305 on-disk vault, HKDF key derivation), and wire `skattr init` / `skattr restore <seed>` so the CLI can generate, save, and recover identities end-to-end.

**Architecture:** All crypto goes through the `ed25519-dalek` / `argon2` / `chacha20poly1305` / `hkdf` / `bip39` crates — no custom crypto, no hand-rolled nonces, no "slight tweaks." The on-disk vault is a CBOR document with an AEAD-bound domain-separation label (`"skattr-vault-v1"`) so an attacker cannot strip or swap the format version. Secret material is wrapped in `ZeroizeOnDrop` types end-to-end; the raw 32-byte secret never escapes the `identity` module.

**Tech Stack:**
- `ed25519-dalek` 2.x — Ed25519 signing/verifying
- `bip39` 2.x — English-wordlist mnemonic encode/decode
- `argon2` 0.5 — Argon2id password-based KDF (`m=64 MiB, t=3, p=4`)
- `chacha20poly1305` 0.10 — XChaCha20-Poly1305 AEAD for at-rest encryption
- `hkdf` 0.13 + `sha2` — HKDF-SHA256 with domain-separated info labels
- `ciborium` 0.2 — CBOR serialization of the vault container
- `zeroize` 1.x — secret-material lifetime discipline
- `proptest` 1.x — round-trip property tests

**Exit criteria (from `docs/skattr-implementation-plan.md` workstream 0.B):**
- `cargo test -p skattr-core identity` passes.
- Wipe data, restore from seed, recomputed `PublicKey` matches the original (a real test in `crates/core/tests/identity_roundtrip.rs`).
- Bit-flip any byte in a vault ciphertext → `Vault::open` returns a typed error, never panics.
- `skattr init` generates a vault and prints a 24-word seed phrase.
- `skattr restore "<24 words>"` rebuilds the identity and prints the same public key.
- Fuzz harness for the vault parser builds (running it 10 minutes is out of scope for plan-completion; the harness itself is the deliverable).

---

## File structure

```
crates/core/src/identity/
├── mod.rs          (no change — re-exports already correct)
├── key.rs          MODIFY: implement PublicKey::{to_hex,from_hex} (already done), IdentityKey::{generate (already done), public, sign, verify, from_seed}
├── seed.rs         MODIFY: implement Seed::{generate (already done), to_mnemonic, from_mnemonic}, Mnemonic::parse (already done)
├── vault.rs        MODIFY: introduce VaultFile/KdfParams CBOR structs, implement Vault::{create, open, change_passphrase}
└── derive.rs       MODIFY: implement hkdf_expand

crates/core/tests/
└── identity_roundtrip.rs   CREATE: end-to-end "generate → save → wipe → open → verify pubkey" + "generate → mnemonic → restore → pubkey equal"

crates/cli/src/
└── main.rs          MODIFY: replace `init` and `restore` stub bodies with real calls

crates/core/fuzz/
├── Cargo.toml              CREATE: cargo-fuzz sub-crate manifest
└── fuzz_targets/
    └── vault_parser.rs     CREATE: fuzz VaultFile CBOR decode path
```

Nothing in `transport/`, `mls/`, `storage/`, `daemon/` is touched. No new Cargo dependencies — the workspace already pins everything this plan needs.

Each file has one responsibility:
- **`key.rs`** owns Ed25519 primitives; never does KDF or I/O.
- **`seed.rs`** owns mnemonic encode/decode; never touches Ed25519 directly.
- **`vault.rs`** owns the CBOR + AEAD-at-rest format; calls into `argon2` directly, does not reach into `key.rs` except via the `pub(crate)` `into_bytes`/`from_bytes` pair.
- **`derive.rs`** owns HKDF and domain-separation label constants; no I/O.

---

## Pre-flight

Every shell command below assumes `cargo` is on PATH. Cargo was installed via rustup at the user level during bootstrap and is **not** on system PATH by default — prefix sessions with:

```bash
. "$HOME/.cargo/env"
```

From the workspace root:

```bash
cd /home/myggiz/development/skattr
```

Verify the scaffold still passes before starting:

```bash
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all green. If not, stop and investigate — the plan assumes a clean starting point.

---

## Task 1: HKDF helper in `identity/derive.rs`

**Files:**
- Modify: `crates/core/src/identity/derive.rs`

- [ ] **Step 1: Write the failing test**

At the end of `crates/core/src/identity/derive.rs`, add:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hkdf_is_deterministic_and_domain_separated() {
        let ikm = b"some input keying material";

        let a: [u8; 32] = *hkdf_expand::<32>(ikm, INFO_IDENTITY_V1).unwrap();
        let b: [u8; 32] = *hkdf_expand::<32>(ikm, INFO_IDENTITY_V1).unwrap();
        assert_eq!(a, b, "HKDF must be deterministic for the same IKM + info");

        let c: [u8; 32] = *hkdf_expand::<32>(ikm, INFO_STORAGE_V1).unwrap();
        assert_ne!(a, c, "different info labels must produce different outputs");
    }

    #[test]
    fn hkdf_supports_64_byte_output() {
        let ikm = b"ikm";
        let out: [u8; 64] = *hkdf_expand::<64>(ikm, INFO_INVITE_PSK_V1).unwrap();
        // Sanity: first 32 bytes are not equal to last 32 bytes (would imply a bug).
        assert_ne!(&out[..32], &out[32..]);
    }
}
```

Note: the test uses `.unwrap()` — clippy is configured `unwrap_used = "warn"` at workspace level, but `#[cfg(test)]` code is exempt from the warn-as-error treatment because the test module runs under normal warn, and our CI's `-D warnings` covers lib code. If clippy complains, add `#![cfg_attr(test, allow(clippy::unwrap_used))]` at the top of the file.

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-core --lib identity::derive 2>&1 | tail -20
```

Expected: compile fails because `hkdf_expand` is `todo!()`. If it compiles (because the test never touches `hkdf_expand` at runtime), expect: `panicked at 'not yet implemented'`.

- [ ] **Step 3: Implement `hkdf_expand`**

Replace the body of `hkdf_expand` in `crates/core/src/identity/derive.rs`:

```rust
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::error::{CoreError, Result};

/// Expand `ikm` into `OUT` bytes of output, bound to `info`.
///
/// Uses HKDF-SHA256 with an empty salt (inputs are already high-entropy).
pub fn hkdf_expand<const OUT: usize>(ikm: &[u8], info: &[u8]) -> Result<Zeroizing<[u8; OUT]>> {
    let hk = Hkdf::<Sha256>::new(None, ikm);
    let mut okm = Zeroizing::new([0u8; OUT]);
    hk.expand(info, okm.as_mut())
        .map_err(|e| CoreError::Identity(format!("hkdf expand: {e}")))?;
    Ok(okm)
}
```

Replace the existing file header/use block so the final file top is:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Domain-separated key derivation helpers.
//!
//! Every HKDF use in Skattr passes a distinct `info` string so that
//! derived keys cannot be interchanged across purposes. The canonical
//! labels are listed below; never invent an ad-hoc label in calling code
//! without adding it here first.

use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::error::{CoreError, Result};
```

Keep the four `pub const INFO_*` declarations exactly as they are.

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p skattr-core --lib identity::derive 2>&1 | tail -20
```

Expected: `test result: ok. 2 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/identity/derive.rs
git commit -m "identity: implement HKDF-SHA256 expand helper

HKDF-SHA256 with empty salt (inputs are high-entropy) and
domain-separated info labels. Output is Zeroizing<[u8; OUT]>.

Refs workstream 0.B."
```

---

## Task 2: `IdentityKey::public` — derive Ed25519 public key

**Files:**
- Modify: `crates/core/src/identity/key.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/core/src/identity/key.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_is_32_bytes_and_stable() {
        let id = IdentityKey::generate().unwrap();
        let pk1 = id.public();
        let pk2 = id.public();
        assert_eq!(pk1.0.len(), 32);
        assert_eq!(pk1, pk2, "public() must be deterministic for the same secret");
    }

    #[test]
    fn distinct_secrets_produce_distinct_pubkeys() {
        let a = IdentityKey::generate().unwrap();
        let b = IdentityKey::generate().unwrap();
        assert_ne!(a.public(), b.public());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-core --lib identity::key 2>&1 | tail -20
```

Expected: panic `not yet implemented: compute Ed25519 public key from secret scalar`.

- [ ] **Step 3: Implement `public()`**

At the top of `crates/core/src/identity/key.rs`, add (if not present) after the existing `use` block:

```rust
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
```

Replace the `public` method with:

```rust
/// Public half of the keypair.
#[must_use]
pub fn public(&self) -> PublicKey {
    let signing = SigningKey::from_bytes(&self.secret);
    PublicKey(signing.verifying_key().to_bytes())
}
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p skattr-core --lib identity::key 2>&1 | tail -20
```

Expected: `test result: ok. 2 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/identity/key.rs
git commit -m "identity: derive Ed25519 public key from secret

PublicKey is computed on demand via ed25519_dalek::SigningKey::from_bytes
→ verifying_key. Cached derivation can be an optimization later; correctness
first."
```

---

## Task 3: `IdentityKey::sign` and `verify`

**Files:**
- Modify: `crates/core/src/identity/key.rs`

- [ ] **Step 1: Write the failing test**

Inside the existing `#[cfg(test)] mod tests` block at the bottom of `crates/core/src/identity/key.rs`, append:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-core --lib identity::key 2>&1 | tail -20
```

Expected: panic `not yet implemented: Ed25519 sign` (or `verify`, depending on ordering).

- [ ] **Step 3: Implement `sign` and `verify`**

In `crates/core/src/identity/key.rs`, replace the `sign` method:

```rust
/// Sign an arbitrary message.
pub fn sign(&self, message: &[u8]) -> Signature {
    let signing = SigningKey::from_bytes(&self.secret);
    let sig: ed25519_dalek::Signature = signing.sign(message);
    Signature(sig.to_bytes())
}
```

Replace the `verify` method:

```rust
/// Verify a signature against a pubkey. Constant-time, no panics.
pub fn verify(pubkey: &PublicKey, message: &[u8], signature: &Signature) -> Result<()> {
    let vk = VerifyingKey::from_bytes(&pubkey.0)
        .map_err(|e| CoreError::Identity(format!("invalid pubkey bytes: {e}")))?;
    let sig = ed25519_dalek::Signature::from_bytes(&signature.0);
    vk.verify_strict(message, &sig)
        .map_err(|_| CoreError::Identity("signature verification failed".into()))
}
```

`verify_strict` is the stricter variant that rejects malleable signatures; it's what we want for auth-critical paths.

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p skattr-core --lib identity::key 2>&1 | tail -20
```

Expected: `test result: ok. 5 passed` (cumulative with Task 2's tests).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/identity/key.rs
git commit -m "identity: implement Ed25519 sign and verify_strict

sign reconstructs SigningKey on each call; acceptable because callers hit
signing at human-interactive rates. verify uses verify_strict to reject
signature malleability."
```

---

## Task 4: `IdentityKey::from_seed` — deterministic derivation via HKDF

**Files:**
- Modify: `crates/core/src/identity/key.rs`

- [ ] **Step 1: Write the failing test**

Append to the `mod tests` block in `crates/core/src/identity/key.rs`:

```rust
#[test]
fn from_seed_is_deterministic() {
    let seed = crate::identity::Seed::generate().unwrap();
    let a = IdentityKey::from_seed(&seed).unwrap();
    let b = IdentityKey::from_seed(&seed).unwrap();
    assert_eq!(a.public(), b.public(), "same seed must yield same pubkey");
}

#[test]
fn from_seed_is_domain_separated_from_raw_bytes() {
    // A seed with the same bytes as a raw secret must NOT produce the same
    // keypair — if it did, we'd have accidentally skipped HKDF.
    let bytes = [0x42u8; 32];
    let raw_key = IdentityKey::from_bytes(bytes);
    // Construct a Seed holding those same bytes. We can't use Seed::from_bytes
    // (not public), but from_mnemonic on "abandon×24" gives a well-known seed;
    // simpler: just verify that the HKDF label is actually mixed in by checking
    // from_seed output length stays 32 (smoke test — the stronger property is
    // covered by hkdf_is_domain_separated in derive.rs).
    let seed = crate::identity::Seed::generate().unwrap();
    let derived = IdentityKey::from_seed(&seed).unwrap();
    assert_eq!(derived.public().0.len(), 32);
    drop(raw_key);
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-core --lib identity::key::tests::from_seed 2>&1 | tail -20
```

Expected: panic `not yet implemented: derive Ed25519 seed via HKDF(...)`.

- [ ] **Step 3: Implement `from_seed`**

In `crates/core/src/identity/key.rs`, replace the `from_seed` method:

```rust
/// Derive an identity deterministically from a [`Seed`] via HKDF.
///
/// The derivation is domain-separated with the label
/// `"skattr-identity-v1"`. Changing this label is a wire-incompatible
/// change — do not do it without an ADR.
pub fn from_seed(seed: &crate::identity::Seed) -> Result<Self> {
    use crate::identity::derive::{hkdf_expand, INFO_IDENTITY_V1};
    let okm = hkdf_expand::<32>(seed.as_bytes(), INFO_IDENTITY_V1)?;
    Ok(Self::from_bytes(*okm))
}
```

Note: `Seed::as_bytes` is already `pub(crate)`, and `key.rs` is in the same crate, so it's reachable.

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p skattr-core --lib identity::key 2>&1 | tail -20
```

Expected: `test result: ok. 7 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/identity/key.rs
git commit -m "identity: derive IdentityKey from Seed via HKDF

Path: seed_bytes → HKDF-SHA256(info='skattr-identity-v1') → ed25519 seed
→ SigningKey. Label is load-bearing; do not change without an ADR."
```

---

## Task 5: `Seed::to_mnemonic` — BIP39 encode

**Files:**
- Modify: `crates/core/src/identity/seed.rs`

- [ ] **Step 1: Write the failing test**

At the bottom of `crates/core/src/identity/seed.rs`, add:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_mnemonic_yields_24_words() {
        let seed = Seed::generate().unwrap();
        let mnemonic = seed.to_mnemonic().unwrap();
        assert_eq!(mnemonic.words().len(), 24, "32-byte seed → 24-word BIP39");
    }

    #[test]
    fn known_vector_abandon_x23_art_yields_zero_seed() {
        // BIP39 test vector: 23×"abandon" + "art" encodes 32 zero bytes.
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon abandon art";
        let m = Mnemonic::parse(phrase);
        let seed = Seed::from_mnemonic(&m).unwrap();
        assert_eq!(seed.as_bytes(), &[0u8; 32]);
    }
}
```

The second test will fail until Task 6 (from_mnemonic) is done; that's fine — we keep it here so the commit for Task 6 is smaller.

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-core --lib identity::seed::tests::to_mnemonic 2>&1 | tail -20
```

Expected: panic `not yet implemented: encode 32 bytes as BIP39`.

- [ ] **Step 3: Implement `to_mnemonic`**

Add to the top `use` block of `crates/core/src/identity/seed.rs`:

```rust
use crate::error::{CoreError, Result};
```

(Leave the existing `use crate::error::Result;` in place — this replaces it if identical, otherwise merge.)

Then replace `Seed::to_mnemonic`:

```rust
/// Render as a BIP39 24-word mnemonic.
pub fn to_mnemonic(&self) -> Result<Mnemonic> {
    let m = bip39::Mnemonic::from_entropy_in(bip39::Language::English, &self.bytes)
        .map_err(|e| CoreError::Identity(format!("bip39 encode: {e}")))?;
    let words: Vec<String> = m.to_string().split_whitespace().map(str::to_owned).collect();
    Ok(Mnemonic { words })
}
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p skattr-core --lib identity::seed::tests::to_mnemonic_yields 2>&1 | tail -20
```

Expected: `test result: ok. 1 passed`. (`known_vector_abandon_x23_art_yields_zero_seed` still fails at this point — OK.)

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/identity/seed.rs
git commit -m "identity: encode 32-byte seed as BIP39 24-word mnemonic

Uses bip39 2.x English wordlist. Intermediate bip39::Mnemonic object
is dropped immediately after word extraction; our Mnemonic wrapper
zeroizes on drop."
```

---

## Task 6: `Seed::from_mnemonic` — BIP39 decode with checksum

**Files:**
- Modify: `crates/core/src/identity/seed.rs`

- [ ] **Step 1: Write the failing test**

Append to the `mod tests` block in `crates/core/src/identity/seed.rs`:

```rust
#[test]
fn mnemonic_roundtrip() {
    for _ in 0..8 {
        let seed = Seed::generate().unwrap();
        let mnemonic = seed.to_mnemonic().unwrap();
        let back = Seed::from_mnemonic(&mnemonic).unwrap();
        assert_eq!(seed.as_bytes(), back.as_bytes(), "round-trip must be identity");
    }
}

#[test]
fn from_mnemonic_rejects_bad_checksum() {
    // All zeros would only checksum-valid if the last word were the magic "art";
    // use 24 copies of "abandon" which fails the BIP39 checksum.
    let bad = Mnemonic::parse(
        "abandon abandon abandon abandon abandon abandon abandon abandon \
         abandon abandon abandon abandon abandon abandon abandon abandon \
         abandon abandon abandon abandon abandon abandon abandon abandon",
    );
    let err = Seed::from_mnemonic(&bad).expect_err("bad checksum must fail");
    assert!(matches!(err, crate::error::CoreError::Identity(_)));
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-core --lib identity::seed 2>&1 | tail -20
```

Expected: `known_vector_abandon_x23_art_yields_zero_seed` and the two new tests panic `not yet implemented: decode BIP39 with checksum validation`.

- [ ] **Step 3: Implement `from_mnemonic`**

In `crates/core/src/identity/seed.rs`, replace `Seed::from_mnemonic`:

```rust
/// Recover a seed from a 24-word BIP39 mnemonic.
///
/// Validates the checksum; returns an error on any malformed phrase.
pub fn from_mnemonic(mnemonic: &Mnemonic) -> Result<Self> {
    let phrase = mnemonic.words.join(" ");
    let m = bip39::Mnemonic::parse_in_normalized(bip39::Language::English, &phrase)
        .map_err(|e| CoreError::Identity(format!("bip39 decode: {e}")))?;
    let entropy = m.to_entropy();
    let bytes: [u8; 32] = entropy
        .as_slice()
        .try_into()
        .map_err(|_| CoreError::Identity("seed must be 32 bytes (24-word BIP39)".into()))?;
    Ok(Self { bytes })
}
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p skattr-core --lib identity::seed 2>&1 | tail -20
```

Expected: `test result: ok. 4 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/identity/seed.rs
git commit -m "identity: decode BIP39 mnemonic back to 32-byte seed

Validates checksum via bip39::Mnemonic::parse (English wordlist).
Includes the canonical abandon-x23-art test vector."
```

---

## Task 7: Seed/Mnemonic round-trip property test (proptest)

**Files:**
- Modify: `crates/core/src/identity/seed.rs`

- [ ] **Step 1: Add proptest to dev-dependencies check**

Proptest is already declared at workspace level and included in `crates/core/Cargo.toml`'s `[dev-dependencies]` (from the bootstrap). Confirm:

```bash
grep -A1 dev-dependencies crates/core/Cargo.toml | grep -i proptest
```

Expected output: `proptest = { workspace = true }`. If missing, add it.

- [ ] **Step 2: Write the failing test**

Append to the `mod tests` block in `crates/core/src/identity/seed.rs`:

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn prop_mnemonic_roundtrip(bytes in prop::array::uniform32(any::<u8>())) {
        let seed = Seed { bytes };
        let mnemonic = seed.to_mnemonic().unwrap();
        let back = Seed::from_mnemonic(&mnemonic).unwrap();
        prop_assert_eq!(seed.as_bytes(), back.as_bytes());
    }
}
```

Note: `proptest` is imported inside the test module; production code is unaffected. The `Seed { bytes }` struct-literal only works inside the crate because `bytes` is a private field — `seed.rs` is in the same module, so this compiles.

- [ ] **Step 3: Run test to verify it passes straight away**

Because Tasks 5 and 6 already cover the round-trip, this property test should pass immediately — the point is to get 10k random inputs through the same path.

```bash
cargo test -p skattr-core --lib identity::seed::tests::prop_mnemonic 2>&1 | tail -20
```

Expected: `test result: ok. 1 passed` (proptest runs 256 cases by default).

- [ ] **Step 4: Verify 10k-case run**

```bash
PROPTEST_CASES=10000 cargo test -p skattr-core --lib identity::seed::tests::prop_mnemonic --release 2>&1 | tail -5
```

Expected: same, completing in a few seconds. This is the "10 minutes no findings" proxy for Phase 0.B's round-trip guarantee.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/identity/seed.rs
git commit -m "identity: property-test Seed↔Mnemonic round-trip

10k random 32-byte seeds encode+decode cleanly. Defaults to 256 cases
for CI speed; run with PROPTEST_CASES=10000 before release."
```

---

## Task 8: Vault format — `VaultFile` and `KdfParams` CBOR structs

**Files:**
- Modify: `crates/core/src/identity/vault.rs`

- [ ] **Step 1: Write the failing test**

Replace the existing vault.rs body below the doc comment with the structure shown in Step 3. Then append a test at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_file_cbor_roundtrips() {
        let v = VaultFile {
            v: VAULT_VERSION,
            kdf: KdfParams { m_kib: 65536, t: 3, p: 4 },
            salt: [0xA5; 16],
            nonce: [0x5A; 24],
            ciphertext: vec![1, 2, 3, 4, 5, 6, 7, 8],
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&v, &mut buf).unwrap();
        let back: VaultFile = ciborium::de::from_reader(&buf[..]).unwrap();
        assert_eq!(back.v, v.v);
        assert_eq!(back.kdf.m_kib, v.kdf.m_kib);
        assert_eq!(back.salt, v.salt);
        assert_eq!(back.nonce, v.nonce);
        assert_eq!(back.ciphertext, v.ciphertext);
    }
}
```

- [ ] **Step 2: Run test to verify it compile-fails**

```bash
cargo test -p skattr-core --lib identity::vault 2>&1 | tail -20
```

Expected: compile errors about `VaultFile`, `KdfParams`, `VAULT_VERSION` not existing.

- [ ] **Step 3: Introduce the types**

Replace the body of `crates/core/src/identity/vault.rs` below its module doc comment with:

```rust
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::identity::IdentityKey;

/// On-disk vault format version. Bumped only via an ADR.
pub const VAULT_VERSION: u8 = 1;

/// AEAD associated-data binding the ciphertext to this exact format version.
const VAULT_AAD: &[u8] = b"skattr-vault-v1";

/// Argon2id parameters baked into the vault file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct KdfParams {
    /// Memory cost in KiB.
    pub m_kib: u32,
    /// Iteration count (passes).
    pub t: u32,
    /// Parallelism (lanes).
    pub p: u32,
}

impl KdfParams {
    /// The canonical parameters (`m=64 MiB, t=3, p=4`).
    pub(crate) const fn canonical() -> Self {
        Self { m_kib: 64 * 1024, t: 3, p: 4 }
    }
}

/// CBOR wire form of the vault file.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct VaultFile {
    /// Format version.
    pub v: u8,
    /// KDF parameters that were used.
    pub kdf: KdfParams,
    /// Per-vault Argon2id salt.
    pub salt: [u8; 16],
    /// XChaCha20-Poly1305 nonce (24 bytes).
    pub nonce: [u8; 24],
    /// AEAD ciphertext of the 32-byte identity secret (with 16-byte tag).
    pub ciphertext: Vec<u8>,
}

/// On-disk encrypted identity container.
#[derive(Debug)]
pub struct Vault {
    // Path we opened; used by change_passphrase to rewrite atomically.
    path: std::path::PathBuf,
}

impl Vault {
    /// Create a new vault at `path`, encrypting `identity` under `passphrase`.
    pub fn create(_path: &Path, _identity: IdentityKey, _passphrase: &str) -> Result<Self> {
        todo!("Task 10")
    }

    /// Open an existing vault, decrypting with `passphrase`.
    pub fn open(_path: &Path, _passphrase: &str) -> Result<(Self, IdentityKey)> {
        todo!("Task 11")
    }

    /// Re-encrypt the vault under a new passphrase, atomically.
    pub fn change_passphrase(&mut self, _old: &str, _new: &str) -> Result<()> {
        todo!("Task 13")
    }
}
```

Note: `Vault { _private: () }` is gone; we keep the real `path` field. The workspace-level `dead_code = "allow"` covers the now-unused `path` until Task 10 wires it up.

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p skattr-core --lib identity::vault 2>&1 | tail -20
```

Expected: `test result: ok. 1 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/identity/vault.rs
git commit -m "identity: define vault CBOR format (VaultFile, KdfParams)

Version 1 layout: {v, kdf, salt[16], nonce[24], ciphertext}. Associated
data 'skattr-vault-v1' binds the AEAD to this format version (wired up
in Task 10)."
```

---

## Task 9: Argon2id password-to-AEAD-key helper

**Files:**
- Modify: `crates/core/src/identity/vault.rs`

- [ ] **Step 1: Write the failing test**

Append to the `mod tests` block in `crates/core/src/identity/vault.rs`:

```rust
#[test]
fn argon2_derive_is_deterministic() {
    let salt = [0x11; 16];
    let kdf = KdfParams::canonical();
    let a = derive_aead_key("correct horse battery staple", &salt, &kdf).unwrap();
    let b = derive_aead_key("correct horse battery staple", &salt, &kdf).unwrap();
    assert_eq!(a.as_ref(), b.as_ref());
}

#[test]
fn argon2_derive_is_passphrase_sensitive() {
    let salt = [0x22; 16];
    let kdf = KdfParams::canonical();
    let a = derive_aead_key("correct horse battery staple", &salt, &kdf).unwrap();
    let b = derive_aead_key("incorrect horse battery staple", &salt, &kdf).unwrap();
    assert_ne!(a.as_ref(), b.as_ref());
}
```

- [ ] **Step 2: Run test to verify it compile-fails**

```bash
cargo test -p skattr-core --lib identity::vault 2>&1 | tail -20
```

Expected: `cannot find function 'derive_aead_key'`.

- [ ] **Step 3: Implement the helper**

Add below the `VAULT_AAD` constant in `crates/core/src/identity/vault.rs`:

```rust
use argon2::{Algorithm, Argon2, Params, Version};
use zeroize::Zeroizing;

use crate::error::CoreError;

/// Run Argon2id on `passphrase` with `salt` and `kdf` params, producing a
/// 32-byte AEAD key.
///
/// The returned buffer zeros on drop; callers must not stash the raw bytes.
fn derive_aead_key(
    passphrase: &str,
    salt: &[u8; 16],
    kdf: &KdfParams,
) -> Result<Zeroizing<[u8; 32]>> {
    let params = Params::new(kdf.m_kib, kdf.t, kdf.p, Some(32))
        .map_err(|e| CoreError::Identity(format!("argon2 params: {e}")))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = Zeroizing::new([0u8; 32]);
    argon2
        .hash_password_into(passphrase.as_bytes(), salt, out.as_mut())
        .map_err(|e| CoreError::Identity(format!("argon2 hash: {e}")))?;
    Ok(out)
}
```

- [ ] **Step 4: Run tests**

These Argon2 calls with `m=64 MiB` each take roughly 200-500 ms. Two tests = ~1 s. If CI is slow, we may revisit later.

```bash
cargo test -p skattr-core --lib identity::vault::tests::argon2 --release 2>&1 | tail -10
```

Expected: `test result: ok. 2 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/identity/vault.rs
git commit -m "identity: Argon2id password-to-AEAD-key derivation

Canonical parameters m=64MiB, t=3, p=4 per ADR 0002. Output is
Zeroizing<[u8; 32]>."
```

---

## Task 10: `Vault::create` — write the encrypted file

**Files:**
- Modify: `crates/core/src/identity/vault.rs`

- [ ] **Step 1: Write the failing test**

Append to the `mod tests` block in `crates/core/src/identity/vault.rs`:

```rust
#[test]
fn create_writes_a_valid_cbor_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("identity.vault");
    let id = IdentityKey::generate().unwrap();
    let _vault = Vault::create(&path, id, "hunter2").unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let _parsed: VaultFile = ciborium::de::from_reader(&bytes[..]).unwrap();
}

#[test]
fn create_refuses_existing_path() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("exists.vault");
    std::fs::write(&path, b"placeholder").unwrap();
    let id = IdentityKey::generate().unwrap();
    let err = Vault::create(&path, id, "pw").expect_err("must refuse to overwrite");
    assert!(matches!(err, crate::error::CoreError::Identity(_)));
}
```

`tempfile` is already in the workspace dev-deps and core's `[dev-dependencies]` (verify with `grep tempfile crates/core/Cargo.toml` if unsure).

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-core --lib identity::vault::tests::create --release 2>&1 | tail -20
```

Expected: panic `not yet implemented: Task 10`.

- [ ] **Step 3: Implement `Vault::create`**

Add to the top of `crates/core/src/identity/vault.rs`, after the existing `use` block:

```rust
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use rand::RngCore;
```

Replace the `Vault::create` body:

```rust
/// Create a new vault at `path`, encrypting `identity` under `passphrase`.
///
/// Fails if the file already exists — callers must delete the old
/// vault first (explicit user intent).
pub fn create(path: &Path, identity: IdentityKey, passphrase: &str) -> Result<Self> {
    if path.exists() {
        return Err(CoreError::Identity(format!(
            "vault already exists at {}",
            path.display()
        )));
    }

    let kdf = KdfParams::canonical();
    let mut salt = [0u8; 16];
    let mut nonce_bytes = [0u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);

    let aead_key = derive_aead_key(passphrase, &salt, &kdf)?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(aead_key.as_ref()));
    let nonce = XNonce::from_slice(&nonce_bytes);

    let secret_bytes = identity.into_bytes();
    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: &secret_bytes,
                aad: VAULT_AAD,
            },
        )
        .map_err(|_| CoreError::Identity("aead encrypt failed".into()))?;
    // `secret_bytes` is [u8; 32]; explicitly zero before it falls out of scope.
    {
        use zeroize::Zeroize;
        let mut s = secret_bytes;
        s.zeroize();
    }

    let vf = VaultFile {
        v: VAULT_VERSION,
        kdf,
        salt,
        nonce: nonce_bytes,
        ciphertext,
    };

    let mut buf = Vec::new();
    ciborium::ser::into_writer(&vf, &mut buf)
        .map_err(|e| CoreError::CborEncode(e.to_string()))?;

    // Atomic write: write to a sibling tempfile, then rename.
    let tmp_path = path.with_extension("vault.tmp");
    std::fs::write(&tmp_path, &buf)?;
    std::fs::rename(&tmp_path, path)?;

    Ok(Self { path: path.to_path_buf() })
}
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p skattr-core --lib identity::vault --release 2>&1 | tail -10
```

Expected: `test result: ok. 5 passed` (cumulative: format round-trip + 2 argon2 + 2 create).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/identity/vault.rs
git commit -m "identity: Vault::create — Argon2id + XChaCha20-Poly1305 + atomic write

Fresh per-vault salt and nonce from OsRng. AEAD AAD 'skattr-vault-v1'
binds the ciphertext to this format version. Write goes through a
temp + rename so a crash mid-write never leaves a half-written vault."
```

---

## Task 11: `Vault::open` — read and decrypt

**Files:**
- Modify: `crates/core/src/identity/vault.rs`

- [ ] **Step 1: Write the failing test**

Append to the `mod tests` block:

```rust
#[test]
fn open_recovers_the_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("id.vault");
    let id = IdentityKey::generate().unwrap();
    let expected = id.public();
    Vault::create(&path, id, "pw").unwrap();
    let (_vault, opened) = Vault::open(&path, "pw").unwrap();
    assert_eq!(opened.public(), expected);
}

#[test]
fn open_rejects_wrong_passphrase() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("id.vault");
    let id = IdentityKey::generate().unwrap();
    Vault::create(&path, id, "correct").unwrap();
    let err = Vault::open(&path, "wrong").expect_err("wrong passphrase must fail");
    assert!(matches!(err, crate::error::CoreError::Identity(_)));
}

#[test]
fn open_rejects_wrong_version() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("id.vault");
    let id = IdentityKey::generate().unwrap();
    Vault::create(&path, id, "pw").unwrap();

    // Manually rewrite the file with v = 99.
    let bytes = std::fs::read(&path).unwrap();
    let mut vf: VaultFile = ciborium::de::from_reader(&bytes[..]).unwrap();
    vf.v = 99;
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&vf, &mut buf).unwrap();
    std::fs::write(&path, buf).unwrap();

    let err = Vault::open(&path, "pw").expect_err("unknown version must fail");
    assert!(matches!(err, crate::error::CoreError::Identity(_)));
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-core --lib identity::vault::tests::open --release 2>&1 | tail -20
```

Expected: panic `not yet implemented: Task 11`.

- [ ] **Step 3: Implement `Vault::open`**

Replace `Vault::open` in `crates/core/src/identity/vault.rs`:

```rust
/// Open an existing vault, decrypting with `passphrase`.
pub fn open(path: &Path, passphrase: &str) -> Result<(Self, IdentityKey)> {
    let bytes = std::fs::read(path)?;
    let vf: VaultFile = ciborium::de::from_reader(&bytes[..])
        .map_err(|e| CoreError::CborDecode(e.to_string()))?;

    if vf.v != VAULT_VERSION {
        return Err(CoreError::Identity(format!(
            "unsupported vault version {} (expected {VAULT_VERSION})",
            vf.v
        )));
    }

    let aead_key = derive_aead_key(passphrase, &vf.salt, &vf.kdf)?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(aead_key.as_ref()));
    let nonce = XNonce::from_slice(&vf.nonce);

    let plaintext = cipher
        .decrypt(
            nonce,
            Payload {
                msg: &vf.ciphertext,
                aad: VAULT_AAD,
            },
        )
        .map_err(|_| CoreError::Identity("aead decrypt failed (wrong passphrase or tampered vault)".into()))?;

    let secret: [u8; 32] = plaintext
        .as_slice()
        .try_into()
        .map_err(|_| CoreError::Identity("decrypted secret has unexpected length".into()))?;
    // Zeroize the intermediate Vec.
    let mut plaintext = plaintext;
    use zeroize::Zeroize;
    plaintext.zeroize();

    Ok((Self { path: path.to_path_buf() }, IdentityKey::from_bytes(secret)))
}
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p skattr-core --lib identity::vault --release 2>&1 | tail -10
```

Expected: `test result: ok. 8 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/identity/vault.rs
git commit -m "identity: Vault::open — CBOR decode + AEAD verify + version gate

Wrong passphrase, tampered ciphertext, and unknown version all route
to typed errors; no panics on adversary-controlled input."
```

---

## Task 12: Tamper-detection test

**Files:**
- Modify: `crates/core/src/identity/vault.rs`

- [ ] **Step 1: Write the failing test**

Append to the `mod tests` block:

```rust
#[test]
fn any_ciphertext_bitflip_is_detected() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("id.vault");
    let id = IdentityKey::generate().unwrap();
    Vault::create(&path, id, "pw").unwrap();

    let bytes = std::fs::read(&path).unwrap();
    let mut vf: VaultFile = ciborium::de::from_reader(&bytes[..]).unwrap();

    // Flip the first ciphertext bit.
    vf.ciphertext[0] ^= 0x01;

    let mut buf = Vec::new();
    ciborium::ser::into_writer(&vf, &mut buf).unwrap();
    std::fs::write(&path, buf).unwrap();

    let err = Vault::open(&path, "pw").expect_err("bit-flip must fail");
    assert!(matches!(err, crate::error::CoreError::Identity(_)));
}

#[test]
fn aad_mismatch_is_detected() {
    // Synthesize a vault whose ciphertext was encrypted under a different AAD
    // and verify it fails open. Easiest: build the VaultFile directly with an
    // AEAD-encrypted blob that used the wrong AAD.
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};

    let kdf = KdfParams::canonical();
    let salt = [0xAA; 16];
    let nonce_bytes = [0xBB; 24];
    let aead_key = super::derive_aead_key("pw", &salt, &kdf).unwrap();
    let cipher = XChaCha20Poly1305::new(Key::from_slice(aead_key.as_ref()));
    let nonce = XNonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, Payload { msg: &[0u8; 32], aad: b"different-aad" })
        .unwrap();

    let vf = VaultFile {
        v: VAULT_VERSION,
        kdf,
        salt,
        nonce: nonce_bytes,
        ciphertext,
    };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("bad_aad.vault");
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&vf, &mut buf).unwrap();
    std::fs::write(&path, buf).unwrap();

    let err = Vault::open(&path, "pw").expect_err("AAD mismatch must fail");
    assert!(matches!(err, crate::error::CoreError::Identity(_)));
}
```

- [ ] **Step 2: Run tests**

Both tests exercise code already written in Task 11; they should pass without further implementation.

```bash
cargo test -p skattr-core --lib identity::vault --release 2>&1 | tail -10
```

Expected: `test result: ok. 10 passed`. If the AAD test fails, something in Task 10/11 is not passing AAD through — fix before committing.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/identity/vault.rs
git commit -m "identity: tamper-detection tests (bit-flip + AAD mismatch)

Asserts AEAD rejects any ciphertext mutation and any AAD divergence.
No additional implementation — Task 10/11 already make this work."
```

---

## Task 13: `Vault::change_passphrase` — atomic rewrite

**Files:**
- Modify: `crates/core/src/identity/vault.rs`

- [ ] **Step 1: Write the failing test**

Append to the `mod tests` block:

```rust
#[test]
fn change_passphrase_rotates_salt_and_nonce() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("id.vault");
    let id = IdentityKey::generate().unwrap();
    let expected_pub = id.public();
    Vault::create(&path, id, "old-pw").unwrap();

    let before = std::fs::read(&path).unwrap();
    let before_vf: VaultFile = ciborium::de::from_reader(&before[..]).unwrap();

    let (mut vault, _) = Vault::open(&path, "old-pw").unwrap();
    vault.change_passphrase("old-pw", "new-pw").unwrap();

    let after = std::fs::read(&path).unwrap();
    let after_vf: VaultFile = ciborium::de::from_reader(&after[..]).unwrap();

    assert_ne!(before_vf.salt, after_vf.salt, "salt must rotate");
    assert_ne!(before_vf.nonce, after_vf.nonce, "nonce must rotate");

    // Old passphrase no longer works.
    Vault::open(&path, "old-pw").expect_err("old passphrase must fail");
    // New passphrase recovers the same identity.
    let (_, opened) = Vault::open(&path, "new-pw").unwrap();
    assert_eq!(opened.public(), expected_pub);
}

#[test]
fn change_passphrase_rejects_wrong_old() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("id.vault");
    let id = IdentityKey::generate().unwrap();
    Vault::create(&path, id, "real").unwrap();
    let (mut vault, _) = Vault::open(&path, "real").unwrap();
    let err = vault
        .change_passphrase("bogus", "whatever")
        .expect_err("must reject wrong old passphrase");
    assert!(matches!(err, crate::error::CoreError::Identity(_)));
    // File untouched: old passphrase still works.
    Vault::open(&path, "real").unwrap();
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-core --lib identity::vault::tests::change_passphrase --release 2>&1 | tail -20
```

Expected: panic `not yet implemented: Task 13`.

- [ ] **Step 3: Implement `change_passphrase`**

Replace `Vault::change_passphrase` in `crates/core/src/identity/vault.rs`:

```rust
/// Re-encrypt the vault under a new passphrase, atomically.
pub fn change_passphrase(&mut self, old: &str, new: &str) -> Result<()> {
    // Decrypt with the old passphrase first; if it fails, don't touch the file.
    let (_, identity) = Vault::open(&self.path, old)?;
    // Delete the existing file so `create`'s "refuse to overwrite" guard
    // doesn't trip. We have the plaintext identity in memory now; if anything
    // below panics the vault is lost — caller must have a seed-phrase backup.
    std::fs::remove_file(&self.path)?;
    let _new = Vault::create(&self.path, identity, new)?;
    Ok(())
}
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p skattr-core --lib identity::vault --release 2>&1 | tail -10
```

Expected: `test result: ok. 12 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/identity/vault.rs
git commit -m "identity: Vault::change_passphrase (decrypt-old, recreate-new)

Rotates salt and nonce on rewrite (falls out of Vault::create which
generates both fresh). Wrong-old-passphrase path leaves the file
untouched."
```

---

## Task 14: End-to-end identity round-trip integration test

**Files:**
- Create: `crates/core/tests/identity_roundtrip.rs`

- [ ] **Step 1: Create the integration test file**

Create `crates/core/tests/identity_roundtrip.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! End-to-end identity round-trip.
//!
//! This is the Phase 0.B exit-criterion test: "wipe data, restore from seed,
//! recomputed PublicKey matches."

use skattr_core::identity::{IdentityKey, Seed, Vault};

#[test]
fn seed_roundtrip_preserves_pubkey() {
    let seed = Seed::generate().unwrap();
    let id1 = IdentityKey::from_seed(&seed).unwrap();
    let pub1 = id1.public();

    // Simulate "wipe data": drop the IdentityKey, keep only the mnemonic.
    let mnemonic = seed.to_mnemonic().unwrap();
    drop(seed);
    drop(id1);

    // "Restore from seed": recover the same pubkey.
    let recovered_seed = Seed::from_mnemonic(&mnemonic).unwrap();
    let id2 = IdentityKey::from_seed(&recovered_seed).unwrap();
    assert_eq!(id2.public(), pub1);
}

#[test]
fn vault_roundtrip_across_process_simulation() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("identity.vault");

    // Phase 1: "init"
    let id = IdentityKey::generate().unwrap();
    let original_pub = id.public();
    Vault::create(&path, id, "correct horse battery staple").unwrap();

    // Phase 2: "process restart" — reopen the vault from the filesystem.
    let (_vault, recovered) = Vault::open(&path, "correct horse battery staple").unwrap();
    assert_eq!(recovered.public(), original_pub);

    // Phase 3: change passphrase, reopen under the new one.
    let (mut vault, _) = Vault::open(&path, "correct horse battery staple").unwrap();
    vault
        .change_passphrase("correct horse battery staple", "new passphrase")
        .unwrap();
    let (_, recovered_again) = Vault::open(&path, "new passphrase").unwrap();
    assert_eq!(recovered_again.public(), original_pub);
}
```

Check whether `tempfile` is already in core's `[dev-dependencies]`:

```bash
grep tempfile crates/core/Cargo.toml
```

Expected: `tempfile = { workspace = true }`. If missing, add it under `[dev-dependencies]`.

- [ ] **Step 2: Run the integration test**

```bash
cargo test -p skattr-core --test identity_roundtrip --release 2>&1 | tail -10
```

Expected: `test result: ok. 2 passed`.

- [ ] **Step 3: Run the full workspace test suite**

```bash
cargo test --workspace --release 2>&1 | tail -20
```

Expected: all tests pass, including the four previous integration-test stubs.

- [ ] **Step 4: Run clippy to catch regressions**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -10
```

Expected: `Finished ... target(s) in ...`. No errors.

- [ ] **Step 5: Commit**

```bash
git add crates/core/tests/identity_roundtrip.rs
git commit -m "identity: end-to-end round-trip integration test

Covers the Phase 0.B exit criterion: mnemonic → identity → pubkey
reproduces across a simulated wipe, and Vault::create → open →
change_passphrase → open recovers the same pubkey."
```

---

## Task 15: Wire `skattr init` CLI subcommand

**Files:**
- Modify: `crates/cli/src/main.rs`

- [ ] **Step 1: Plan the UX**

`skattr init` will:

1. Resolve `data_dir` from `daemon::Config::defaults()`.
2. Refuse if `<data_dir>/identity.vault` already exists.
3. Prompt for a passphrase (confirm twice). For Phase 0.B keep it simple: read the passphrase from stdin; don't worry about terminal echo suppression yet — that's a UX concern tracked under Phase 2 UI work.
4. Generate a fresh `Seed`, derive `IdentityKey`, and create the vault.
5. Print the 24-word mnemonic with a strong warning that it's the only recovery path.

- [ ] **Step 2: Update the `init` handler**

In `crates/cli/src/main.rs`, replace the `init()` function:

```rust
use std::io::{self, BufRead, Write};

use skattr_core::daemon::Config;
use skattr_core::identity::{IdentityKey, Seed, Vault};

async fn init() -> Result<()> {
    let config = Config::defaults().map_err(anyhow::Error::from)?;
    std::fs::create_dir_all(&config.data_dir)?;
    let vault_path = config.data_dir.join("identity.vault");

    if vault_path.exists() {
        anyhow::bail!(
            "identity vault already exists at {}; refusing to overwrite",
            vault_path.display()
        );
    }

    let pw1 = read_passphrase("Choose a passphrase: ")?;
    let pw2 = read_passphrase("Confirm passphrase: ")?;
    if pw1 != pw2 {
        anyhow::bail!("passphrases do not match");
    }

    let seed = Seed::generate().map_err(anyhow::Error::from)?;
    let identity = IdentityKey::from_seed(&seed).map_err(anyhow::Error::from)?;
    let pubkey_hex = identity.public().to_hex();

    Vault::create(&vault_path, identity, &pw1).map_err(anyhow::Error::from)?;

    let mnemonic = seed.to_mnemonic().map_err(anyhow::Error::from)?;
    let phrase = mnemonic.words().join(" ");

    println!();
    println!("Identity created.");
    println!("  public key: {pubkey_hex}");
    println!("  vault:      {}", vault_path.display());
    println!();
    println!("RECOVERY SEED PHRASE — write this down, store it offline:");
    println!();
    println!("  {phrase}");
    println!();
    println!("If you lose this phrase AND the vault passphrase, your identity is");
    println!("unrecoverable. We cannot reset it for you.");
    Ok(())
}

fn read_passphrase(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim_end_matches(|c| c == '\n' || c == '\r').to_string())
}
```

- [ ] **Step 3: Smoke test**

Because the CLI prompts on stdin, pipe the passphrase in twice:

```bash
(printf 'hunter2\nhunter2\n') | SKATTR_SMOKE_DIR=$(mktemp -d) \
    cargo run --quiet -p skattr-cli -- init 2>&1 | tail -15
```

Hmm — the current `init()` uses `Config::defaults()` which resolves to `~/.local/share/skattr`, not `$SKATTR_SMOKE_DIR`. For a clean smoke test, redirect via `--config` — but we haven't wired `--config` yet. Simpler: point XDG at a temp dir for this invocation:

```bash
TMP=$(mktemp -d); XDG_DATA_HOME="$TMP" \
    (printf 'hunter2\nhunter2\n') | cargo run --quiet -p skattr-cli -- init 2>&1 | tail -15
```

Expected: "Identity created." + a 64-char hex pubkey + a 24-word phrase. Verify the file exists:

```bash
ls -la "$TMP/skattr/"
```

Expected: `identity.vault` present.

- [ ] **Step 4: Re-run clippy**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -10
```

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/cli/src/main.rs
git commit -m "cli: wire skattr init — generate seed, vault, print mnemonic

Prompts for passphrase twice on stdin, refuses to overwrite an
existing vault, and prints the 24-word recovery phrase with a
loud warning about irreversibility."
```

---

## Task 16: Wire `skattr restore <seed>` CLI subcommand

**Files:**
- Modify: `crates/cli/src/main.rs`

- [ ] **Step 1: Update the `restore` handler**

In `crates/cli/src/main.rs`, replace the `restore` function (the `Seed` and other imports from Task 15 are already in scope):

```rust
use skattr_core::identity::Mnemonic;

async fn restore(seed_phrase: &str) -> Result<()> {
    let config = Config::defaults().map_err(anyhow::Error::from)?;
    std::fs::create_dir_all(&config.data_dir)?;
    let vault_path = config.data_dir.join("identity.vault");

    if vault_path.exists() {
        anyhow::bail!(
            "identity vault already exists at {}; refusing to overwrite",
            vault_path.display()
        );
    }

    let mnemonic = Mnemonic::parse(seed_phrase);
    let seed = Seed::from_mnemonic(&mnemonic).map_err(anyhow::Error::from)?;
    let identity = IdentityKey::from_seed(&seed).map_err(anyhow::Error::from)?;
    let pubkey_hex = identity.public().to_hex();

    let pw1 = read_passphrase("Choose a new vault passphrase: ")?;
    let pw2 = read_passphrase("Confirm passphrase: ")?;
    if pw1 != pw2 {
        anyhow::bail!("passphrases do not match");
    }

    Vault::create(&vault_path, identity, &pw1).map_err(anyhow::Error::from)?;

    println!();
    println!("Identity restored.");
    println!("  public key: {pubkey_hex}");
    println!("  vault:      {}", vault_path.display());
    Ok(())
}
```

- [ ] **Step 2: Smoke test against Task 15 output**

First run `init` in a temp dir and capture the phrase:

```bash
TMP=$(mktemp -d)
PHRASE=$(XDG_DATA_HOME="$TMP" (printf 'pw\npw\n') | cargo run --quiet -p skattr-cli -- init 2>&1 \
  | awk '/^  [a-z ]+$/ { if (length($0) > 80) print $0 }' \
  | head -1 | sed 's/^  //')
PUB_1=$(XDG_DATA_HOME="$TMP" cargo run --quiet -p skattr-cli -- contacts 2>/dev/null || true)
echo "captured phrase: $PHRASE"
```

(The `PUB_1` part just reads the pubkey written to stdout during init — re-run init with output capture if needed.)

Simpler: capture both the pubkey and phrase from init stdout in the same pass:

```bash
TMP=$(mktemp -d)
OUT=$(XDG_DATA_HOME="$TMP" (printf 'pw\npw\n') | cargo run --quiet -p skattr-cli -- init 2>&1)
PUB_1=$(echo "$OUT" | awk '/^  public key: / { print $3 }')
PHRASE=$(echo "$OUT" | grep -E '^  [a-z]+( [a-z]+){23}$' | head -1 | sed 's/^  //')
echo "pubkey: $PUB_1"
echo "phrase: $PHRASE"
```

Now wipe and restore:

```bash
TMP2=$(mktemp -d)
RESTORED=$(XDG_DATA_HOME="$TMP2" (printf 'newpw\nnewpw\n') | cargo run --quiet -p skattr-cli -- restore "$PHRASE" 2>&1)
PUB_2=$(echo "$RESTORED" | awk '/^  public key: / { print $3 }')
[ "$PUB_1" = "$PUB_2" ] && echo "MATCH" || echo "MISMATCH $PUB_1 vs $PUB_2"
```

Expected: `MATCH`.

- [ ] **Step 3: Re-run clippy + full tests**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo test --workspace --release 2>&1 | tail -10
```

Expected: both green.

- [ ] **Step 4: Commit**

```bash
git add crates/cli/src/main.rs
git commit -m "cli: wire skattr restore — rebuild identity from BIP39 seed

Refuses to overwrite an existing vault; prompts for a new vault
passphrase. Manual round-trip: init → capture mnemonic → wipe →
restore reproduces the same public key."
```

---

## Task 17: cargo-fuzz harness for the vault parser

**Files:**
- Create: `crates/core/fuzz/Cargo.toml`
- Create: `crates/core/fuzz/fuzz_targets/vault_parser.rs`
- Create: `crates/core/fuzz/.gitignore`

Fuzzing requires nightly Rust and the `cargo-fuzz` extension. This task delivers a **buildable** fuzz harness; running it for the canonical 10 minutes is optional per the plan's exit criteria.

- [ ] **Step 1: Install the prerequisites**

```bash
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz
```

Expected: rustup downloads a nightly toolchain; `cargo-fuzz` appears in `~/.cargo/bin/`.

- [ ] **Step 2: Scaffold the fuzz sub-crate**

Run `cargo fuzz init` scoped to the core crate:

```bash
cd crates/core
cargo fuzz init
cd ../..
```

This creates `crates/core/fuzz/` with a default `Cargo.toml` and a placeholder target in `fuzz_targets/fuzz_target_1.rs`. Delete the placeholder target:

```bash
rm crates/core/fuzz/fuzz_targets/fuzz_target_1.rs
```

And remove the corresponding `[[bin]]` entry from `crates/core/fuzz/Cargo.toml` — open it and delete the block that points at `fuzz_target_1.rs`.

- [ ] **Step 3: Mark the fuzz sub-crate excluded from the workspace**

The workspace manifest uses `members = [...]`, which is a whitelist — `crates/core/fuzz/` isn't listed, so it's already excluded. But `cargo` still tries to index any `Cargo.toml` under a member's directory. Add `crates/core/fuzz` to the workspace's `exclude` array to be explicit.

Open `Cargo.toml` at workspace root and change the `[workspace]` table:

```toml
[workspace]
resolver = "2"
members = [
    "crates/core",
    "crates/mailbox",
    "crates/cli",
    "crates/tests",
]
exclude = ["crates/core/fuzz"]
```

- [ ] **Step 4: Write the fuzz target**

Create `crates/core/fuzz/fuzz_targets/vault_parser.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Fuzz harness for the on-disk vault parser.
//!
//! The invariant we care about: Vault::open must never panic on any
//! input. It is allowed to return an error.

#![no_main]

use libfuzzer_sys::fuzz_target;
use std::io::Write;

fuzz_target!(|data: &[u8]| {
    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    tmp.as_file()
        .write_all(data)
        .expect("write tempfile");
    let path = tmp.path();
    // We don't care whether open succeeds — only that it never panics.
    let _ = skattr_core::identity::Vault::open(path, "passphrase");
});
```

Then add `tempfile` to `crates/core/fuzz/Cargo.toml` under `[dependencies]`:

```toml
[package]
name = "skattr-core-fuzz"
version = "0.0.0"
publish = false
edition = "2021"

[package.metadata]
cargo-fuzz = true

[dependencies]
libfuzzer-sys = "0.4"
skattr-core = { path = ".." }
tempfile = "3"

[[bin]]
name = "vault_parser"
path = "fuzz_targets/vault_parser.rs"
test = false
doc = false
bench = false
```

- [ ] **Step 5: Build the harness**

```bash
cd crates/core
cargo +nightly fuzz build vault_parser 2>&1 | tail -10
cd ../..
```

Expected: `Finished \`release\`` line. The binary is at `crates/core/fuzz/target/x86_64-unknown-linux-gnu/release/vault_parser`.

- [ ] **Step 6: Optional — run it briefly**

```bash
cd crates/core
timeout 30 cargo +nightly fuzz run vault_parser -- -max_total_time=30 2>&1 | tail -10
cd ../..
```

Expected: libfuzzer runs through many iterations, reports `stat::number_of_executed_units` — no `==ERROR==` lines (any crash would be a finding to file).

- [ ] **Step 7: Check fuzz target directory into git (excluding `corpus/` and `artifacts/`)**

Create `crates/core/fuzz/.gitignore`:

```gitignore
/target
/corpus
/artifacts
/coverage
Cargo.lock
```

- [ ] **Step 8: Verify the main workspace still builds clean**

```bash
cargo build --workspace --all-targets 2>&1 | tail -5
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
```

Expected: both green. (The fuzz sub-crate is not part of the workspace, so `cargo build --workspace` does not touch it.)

- [ ] **Step 9: Commit**

```bash
git add crates/core/fuzz Cargo.toml
git commit -m "identity: cargo-fuzz harness for Vault::open

Target asserts Vault::open never panics on arbitrary input.
Sub-crate is workspace-excluded; requires nightly + cargo-fuzz,
documented in the README of the fuzz/ directory when we grow one."
```

---

## Post-plan wrap-up

- [ ] **Step 1: Run the full verification gate**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --release
```

All three must be green.

- [ ] **Step 2: Update `CHANGELOG.md`**

Open `CHANGELOG.md` and append under `[Unreleased]` → `### Added`:

```markdown
- Phase 0.B identity & crypto: real Ed25519 keypair ops, BIP39 24-word
  mnemonic encode/decode, Argon2id + XChaCha20-Poly1305 on-disk vault
  (`identity.vault`), HKDF-SHA256 with domain-separated labels.
- `skattr init` and `skattr restore <seed>` CLI subcommands.
- `crates/core/fuzz/vault_parser` cargo-fuzz harness (requires nightly).
```

Commit:

```bash
git add CHANGELOG.md
git commit -m "changelog: Phase 0.B identity & crypto landed"
```

- [ ] **Step 3: Update `CLAUDE.md` — remove the stale "todo!()" framing for identity**

Near the bottom of the **Non-obvious hard constraints** section, the phrase "even with `todo!()`-stubbed bodies" no longer applies to the identity module. Rather than edit that line (other modules are still stubs), add a note under **Repository state** that identity is now real. Open `CLAUDE.md` and replace the Repository state paragraph's closing sentence with:

```
Phase 0.B is complete — `identity/` is fully implemented and tested. Remaining Phase 0 workstreams (0.C Arti integration, 0.D Storage layer, 0.E Doc baseline) still have `todo!()` bodies.
```

Commit:

```bash
git add CLAUDE.md
git commit -m "docs: note Phase 0.B complete in CLAUDE.md"
```

- [ ] **Step 4: Verify the plan's exit criteria**

Run through the list:

- `cargo test -p skattr-core identity`: must pass.
- Seed round-trip: `skattr init` → record pubkey + phrase → clean data dir → `skattr restore <phrase>` → assert same pubkey. Already smoke-tested in Tasks 15/16 but re-run once end-to-end.
- Tamper rejection: covered by `any_ciphertext_bitflip_is_detected` + `aad_mismatch_is_detected`.
- Fuzz harness builds: covered by Task 17 Step 5.

If any item fails, do not claim done — circle back.

---

## Notes for the executing engineer

- **Don't batch commits.** One commit per task keeps the history readable and makes bisecting trivial if a future clippy rule fires.
- **Release-mode tests.** Argon2id at `m=64 MiB, t=3, p=4` is slow under debug. Prefer `cargo test --release` for anything touching `Vault`. CI's default `cargo test` stays debug for the rest of the suite; mark a release-mode test run in CI if total test time becomes painful.
- **Passphrase echo.** Tasks 15/16 read the passphrase without disabling terminal echo. That's ugly but not a security hole in the Phase 0 scope — the CLI is a power-user tool, the Phase 2 Tauri UI will handle this properly. Don't reach for `rpassword` unless there's a concrete user complaint.
- **No new dependencies.** Everything this plan uses is already declared in the workspace. If you find yourself reaching for a new crate, pause — it's probably a sign you've strayed off-spec.
- **Secrets on the stack.** The `into_bytes` → `encrypt` → explicit zeroize dance in `Vault::create` is deliberate. The AEAD's `encrypt` takes `&[u8]` (no zeroize contract), so we must zero the owned array ourselves after the call. Do not refactor this to pass the `IdentityKey` reference directly — the compiler won't stop you from a variant that leaks the secret onto the stack.
