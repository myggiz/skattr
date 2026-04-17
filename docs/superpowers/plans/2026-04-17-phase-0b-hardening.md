# Phase 0.B Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the 13 follow-ups surfaced by code review during Phase 0.B — zeroize transient leaks, vault-write durability (fsync + atomic rename for `change_passphrase`), constant-time error parity in `verify()`, a handful of missed tests, the passphrase-normalization ADR, and minor CLI ergonomics.

**Architecture:** Incremental hardening — each task touches 1-3 files, preserves existing public APIs (except one narrow `pub(crate)` signature change), and leaves the test suite strictly monotonic. No new protocol behavior. No new cryptography. Follow-ups in this plan do not block Phase 0.C (Arti), so a parallel session could pick up that work simultaneously.

**Tech Stack:** Same as Phase 0.B — Rust stable, workspace already declares everything needed (`zeroize`, `anyhow`, `serde`, etc.).

**Exit criteria:**
- Zero TaskList items #25–#36 remain open.
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --release` all green.
- `cargo +nightly fuzz build vault_parser` still builds.
- Manual: vault written pre-hardening still decrypts cleanly post-hardening (wire compatibility preserved).
- New ADR `docs/adr/0004-passphrase-normalization.md` committed.
- CLAUDE.md "known caveats" list shrinks accordingly.

---

## File structure

```
crates/core/src/identity/
├── key.rs          MODIFY: from_bytes signature; unify verify() error arms; sig-byte tamper test
├── seed.rs         MODIFY: zeroize phrase/entropy intermediates; cfg(test) Seed::from_bytes; Mnemonic::from_words normalize
└── vault.rs        MODIFY: atomic_write_vault helper with fsync; change_passphrase atomic rename; derive_aead_key sensitivity tests

crates/cli/src/
└── main.rs          MODIFY: zeroize argv seed_phrase; restore error UX; --data-dir global flag

docs/adr/
└── 0004-passphrase-normalization.md   CREATE

Cargo.toml          (unchanged)
CHANGELOG.md        MODIFY: hardening entries under [Unreleased]
CLAUDE.md           MODIFY: shrink "known Phase 0.B caveats" as fixes land
```

Nothing outside these paths changes.

---

## Pre-flight

```bash
cd /home/myggiz/development/skattr
. "$HOME/.cargo/env"

cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --release
```

All green before starting. If not: stop.

Then create a worktree for this work (per `superpowers:using-git-worktrees`):

```bash
git worktree add ../skattr-phase-0b-hardening -b phase-0b-hardening
cd ../skattr-phase-0b-hardening
cargo build --workspace  # seed target/
```

Every subsequent `cd` path in tasks assumes `/home/myggiz/development/skattr-phase-0b-hardening`.

---

## Task 1: Atomic vault write with fsync

**Goal:** Extract a private `atomic_write_vault` helper that writes to a sibling tempfile, fsyncs the tempfile, renames over the target, then fsyncs the parent directory. Makes the vault write *durably* atomic on POSIX. Refactor `Vault::create` to use it.

**Files:** Modify `crates/core/src/identity/vault.rs`.

- [ ] **Step 1: Write the failing test**

Append to the `mod tests` block in `crates/core/src/identity/vault.rs`:

```rust
#[test]
fn no_tempfile_sidecar_after_create() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("id.vault");
    let id = IdentityKey::generate().unwrap();
    Vault::create(&path, id, "pw").unwrap();
    let sidecar = path.with_extension("vault.tmp");
    assert!(!sidecar.exists(), "tempfile sidecar must be gone after create");
}
```

- [ ] **Step 2: Run test to verify baseline**

```bash
cargo test -p skattr-core --lib identity::vault::tests::no_tempfile_sidecar --release 2>&1 | tail -5
```

Expected: the existing `create` already does temp+rename, so this test passes as-is. If it panics or fails, stop and investigate.

- [ ] **Step 3: Extract the helper**

In `crates/core/src/identity/vault.rs`, replace the write section inside `Vault::create` (the current `let tmp_path = ...; std::fs::write(...); std::fs::rename(...)` block) by introducing a new private free function ABOVE `impl Vault`:

```rust
/// Durably write `vf` to `path`: serialize → tempfile → fsync tempfile →
/// rename over target → fsync parent directory.
///
/// Renames on POSIX are atomic within a single filesystem, but durability
/// against power loss additionally requires fsync on both the tempfile
/// and the parent directory (so the directory entry's inode change is
/// on platter before we report success).
fn atomic_write_vault(path: &Path, vf: &VaultFile) -> Result<()> {
    let mut buf = Vec::new();
    ciborium::ser::into_writer(vf, &mut buf)
        .map_err(|e| CoreError::CborEncode(e.to_string()))?;

    let tmp_path = path.with_extension("vault.tmp");
    {
        let mut f = std::fs::File::create(&tmp_path)?;
        use std::io::Write;
        f.write_all(&buf)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp_path, path)?;

    // Fsync parent directory so the rename itself is durable.
    if let Some(parent) = path.parent() {
        // On Linux the directory must be opened read-only; on macOS
        // File::open works too. Windows has no directory fsync — skip.
        #[cfg(unix)]
        {
            let dir = std::fs::File::open(parent)?;
            dir.sync_all()?;
        }
        #[cfg(not(unix))]
        {
            let _ = parent; // suppress unused on non-unix
        }
    }
    Ok(())
}
```

Then inside `Vault::create`, replace:

```rust
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&vf, &mut buf)
        .map_err(|e| CoreError::CborEncode(e.to_string()))?;

    // Atomic write: write to a sibling tempfile, then rename.
    let tmp_path = path.with_extension("vault.tmp");
    std::fs::write(&tmp_path, &buf)?;
    std::fs::rename(&tmp_path, path)?;
```

with a single call:

```rust
    atomic_write_vault(path, &vf)?;
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-core --lib identity::vault --release 2>&1 | tail -10
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -5
```

Expected: 13 passed (12 prior + the new `no_tempfile_sidecar_after_create`), clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/identity/vault.rs
git commit -m "identity: atomic_write_vault helper with fsync durability

Vault::create now writes via a private helper that fsyncs the
tempfile and the parent directory after rename. Unix-gated because
Windows has no directory-fsync primitive. Closes follow-up #32."
```

---

## Task 2: `Vault::change_passphrase` atomic rename (no unlink)

**Goal:** Eliminate the crash window in `change_passphrase` by writing the re-encrypted vault to a sidecar and renaming over the old path — no `remove_file` step. Uses the helper from Task 1.

**Files:** Modify `crates/core/src/identity/vault.rs`.

- [ ] **Step 1: Write the failing test**

Append to `mod tests`:

```rust
#[test]
fn change_passphrase_survives_simulated_new_create_failure() {
    // Simulate the "disk full during new-vault write" failure by pre-creating
    // a busy/unwritable sidecar path before change_passphrase. The rename
    // should fail cleanly and the OLD vault must still be openable.
    //
    // We approximate "unwritable sidecar" by pre-creating `.vault.tmp` as
    // a directory — File::create will fail with IsADirectory on Unix.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("id.vault");
    let id = IdentityKey::generate().unwrap();
    let expected_pub = id.public();
    Vault::create(&path, id, "old").unwrap();

    // Block the sidecar path.
    std::fs::create_dir(path.with_extension("vault.tmp")).unwrap();

    let (mut vault, _) = Vault::open(&path, "old").unwrap();
    let err = vault.change_passphrase("old", "new");
    assert!(err.is_err(), "sidecar conflict must return Err");

    // Unblock and verify the old vault is still intact.
    std::fs::remove_dir(path.with_extension("vault.tmp")).unwrap();
    let (_, opened) = Vault::open(&path, "old").unwrap();
    assert_eq!(opened.public(), expected_pub);
}
```

- [ ] **Step 2: Run test to verify failure**

```bash
cargo test -p skattr-core --lib identity::vault::tests::change_passphrase_survives --release 2>&1 | tail -15
```

Expected: compile OK; test FAILs — with the current `remove_file → create` sequence, the old vault is deleted before the sidecar conflict hits, so `Vault::open(&path, "old")` at the end returns `Err(io::NotFound)` and the assertion fails.

- [ ] **Step 3: Refactor `change_passphrase`**

Replace the body of `Vault::change_passphrase` in `crates/core/src/identity/vault.rs`:

```rust
    /// Re-encrypt the vault under a new passphrase.
    ///
    /// Crash-safe: writes the new vault to a sidecar, fsyncs, then
    /// renames over the existing path atomically. A crash at any point
    /// either leaves the old vault intact (rename hasn't landed) or the
    /// new one (rename has landed) — never neither.
    ///
    /// Takes `&mut self` to serialize concurrent rewrites at the API
    /// boundary — no `self` field is actually mutated; the mutation is
    /// on-disk.
    pub fn change_passphrase(&mut self, old: &str, new: &str) -> Result<()> {
        // Decrypt with the old passphrase first; if it fails, don't touch
        // the file.
        let (_, identity) = Vault::open(&self.path, old)?;

        // Rebuild a fresh VaultFile under the new passphrase, then write
        // atomically over the existing path. Fresh salt + nonce per
        // rewrite fall out of this flow.
        let kdf = KdfParams::canonical();
        let mut salt = [0u8; 16];
        let mut nonce_bytes = [0u8; 24];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);

        let aead_key = derive_aead_key(new, &salt, &kdf)?;
        let cipher = XChaCha20Poly1305::new(Key::from_slice(aead_key.as_ref()));
        let nonce = XNonce::from_slice(&nonce_bytes);

        let secret_bytes = Zeroizing::new(identity.into_bytes());
        let ciphertext = cipher
            .encrypt(
                nonce,
                Payload {
                    msg: secret_bytes.as_ref(),
                    aad: VAULT_AAD,
                },
            )
            .map_err(|_| CoreError::Identity("aead encrypt failed".into()))?;

        let vf = VaultFile {
            v: VAULT_VERSION,
            kdf,
            salt,
            nonce: nonce_bytes,
            ciphertext,
        };

        atomic_write_vault(&self.path, &vf)?;
        Ok(())
    }
```

The `TODO(phase-1)` comment that was there can be removed.

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-core --lib identity::vault --release 2>&1 | tail -10
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: 14 passed (prior 13 + the new sidecar-conflict test), clippy clean. In particular, `change_passphrase_rotates_salt_and_nonce` and `change_passphrase_rejects_wrong_old` still pass.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/identity/vault.rs
git commit -m "identity: Vault::change_passphrase atomic rename (no unlink)

Writes the re-encrypted vault to a sidecar and renames over the
existing path via atomic_write_vault — no remove_file step. Closes
follow-up #34 and resolves the crash window documented in the
prior Phase 0.B commit."
```

---

## Task 3: `IdentityKey::from_bytes` takes `Zeroizing<[u8; 32]>`

**Goal:** Tighten the zeroize contract on the final leg of seed derivation. Change the `pub(crate)` signature of `from_bytes` so callers can't accidentally pass a raw stack `[u8; 32]` without Zeroize guard.

**Files:** Modify `crates/core/src/identity/key.rs`, `crates/core/src/identity/vault.rs`.

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `crates/core/src/identity/key.rs`:

```rust
#[test]
fn from_bytes_accepts_zeroizing() {
    let mut buf = zeroize::Zeroizing::new([0u8; 32]);
    buf[0] = 1;
    let id = IdentityKey::from_bytes(buf);
    assert_eq!(id.public().0.len(), 32);
}
```

- [ ] **Step 2: Run test to verify failure**

```bash
cargo test -p skattr-core --lib identity::key::tests::from_bytes_accepts --release 2>&1 | tail -10
```

Expected: compile error — `from_bytes` currently takes `[u8; 32]`, not `Zeroizing<[u8; 32]>`.

- [ ] **Step 3: Change `from_bytes` signature + update callers**

In `crates/core/src/identity/key.rs`, replace:

```rust
    /// Construct from raw secret bytes. Private: only callable from inside
    /// the crate (vault open, seed derivation).
    pub(crate) fn from_bytes(secret: [u8; 32]) -> Self {
        Self { secret }
    }
```

with:

```rust
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
```

Now update the two call sites.

In `crates/core/src/identity/key.rs`, `from_seed`:

```rust
    pub fn from_seed(seed: &crate::identity::Seed) -> Result<Self> {
        use crate::identity::derive::{hkdf_expand, INFO_IDENTITY_V1};
        let okm = hkdf_expand::<32>(seed.as_bytes(), INFO_IDENTITY_V1)?;
        Ok(Self::from_bytes(okm))
    }
```

(dropped the `*okm` deref; `okm` is already `Zeroizing<[u8; 32]>`.)

In `crates/core/src/identity/vault.rs`, `Vault::open`:

```rust
        Ok((
            Self {
                path: path.to_path_buf(),
            },
            IdentityKey::from_bytes(secret),
        ))
```

(dropped the `*secret` deref; `secret` is already `Zeroizing<[u8; 32]>`.)

Also update the existing test `from_seed_is_domain_separated_from_raw_bytes` in `key.rs` which uses `IdentityKey::from_bytes(bytes)` with a bare array:

```rust
    #[test]
    fn from_seed_is_domain_separated_from_raw_bytes() {
        let bytes = zeroize::Zeroizing::new([0x42u8; 32]);
        let raw_key = IdentityKey::from_bytes(bytes);
        let seed = crate::identity::Seed::generate().unwrap();
        let derived = IdentityKey::from_seed(&seed).unwrap();
        assert_eq!(derived.public().0.len(), 32);
        drop(raw_key);
    }
```

(This test is strengthened in Task 6; for now, just adapt it to compile.)

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-core --lib identity --release 2>&1 | tail -10
cargo test -p skattr-core --test identity_roundtrip --release 2>&1 | tail -5
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: 27 lib tests passed (prior 26 + `from_bytes_accepts_zeroizing`), 2 integration tests passed, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/identity/key.rs crates/core/src/identity/vault.rs
git commit -m "identity: IdentityKey::from_bytes takes Zeroizing<[u8;32]>

The caller's Zeroizing guard now drops immediately after the move,
leaving the struct's ZeroizeOnDrop as the sole wipe path. Closes
follow-up #27."
```

---

## Task 4: Zeroize intermediate mnemonic strings

**Goal:** Wipe the heap allocations that transiently hold the BIP39 phrase and the 32-byte entropy. Closes the Task 5/6 review concerns about `m.to_string()` and `m.to_entropy()` leaking secret material through dropped-but-unwiped heap pages.

**Files:** Modify `crates/core/src/identity/seed.rs`.

- [ ] **Step 1: Update `Seed::to_mnemonic` and `Seed::from_mnemonic`**

In `crates/core/src/identity/seed.rs`, replace `Seed::to_mnemonic`:

```rust
    /// Render as a BIP39 24-word mnemonic.
    pub fn to_mnemonic(&self) -> Result<Mnemonic> {
        let m = bip39::Mnemonic::from_entropy_in(bip39::Language::English, &self.bytes)
            .map_err(|e| CoreError::Identity(format!("bip39 encode: {e}")))?;
        // The joined phrase lives on the heap; wipe it before drop.
        let phrase = zeroize::Zeroizing::new(m.to_string());
        let words: Vec<String> = phrase
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        Ok(Mnemonic { words })
    }
```

And `Seed::from_mnemonic`:

```rust
    /// Recover a seed from a 24-word BIP39 mnemonic.
    ///
    /// Validates the checksum; returns an error on any malformed phrase.
    pub fn from_mnemonic(mnemonic: &Mnemonic) -> Result<Self> {
        let phrase = zeroize::Zeroizing::new(mnemonic.words.join(" "));
        let m = bip39::Mnemonic::parse_in_normalized(bip39::Language::English, &phrase)
            .map_err(|e| CoreError::Identity(format!("bip39 decode: {e}")))?;
        let entropy = zeroize::Zeroizing::new(m.to_entropy());
        let bytes: [u8; 32] = entropy
            .as_slice()
            .try_into()
            .map_err(|_| CoreError::Identity("seed must be 32 bytes (24-word BIP39)".into()))?;
        Ok(Self { bytes })
    }
```

Note: `bip39::Mnemonic` itself (the upstream `m` binding) still doesn't zeroize internally — that's an unfixable upstream gap documented as a known limitation in the security write-up. We wrap our own heap allocations only.

- [ ] **Step 2: Run tests + clippy**

```bash
cargo test -p skattr-core --lib identity::seed --release 2>&1 | tail -10
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: 5 passed (all prior), clippy clean.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/identity/seed.rs
git commit -m "identity: zeroize intermediate mnemonic/phrase/entropy allocations

Wraps the heap String from m.to_string()/words.join() and the
Vec<u8> from m.to_entropy() in Zeroizing so the plaintext does
not linger in dropped heap pages. The upstream bip39::Mnemonic
still holds the phrase without zeroization — a known gap documented
for the threat model. Closes follow-up #29."
```

---

## Task 5: Zeroize argv seed phrase + restore error UX

**Goal:** The `skattr restore <seed>` command receives the 24-word phrase via `clap` (so it lives in `argv`, already visible in `/proc/<pid>/cmdline`). We can't remove that exposure, but we CAN wipe our in-process copy after parsing, and we can improve the error UX when a bad phrase is given.

**Files:** Modify `crates/cli/src/main.rs`.

- [ ] **Step 1: Update the `restore` handler**

In `crates/cli/src/main.rs`, replace the body of `async fn restore(seed_phrase: &str) -> Result<()>`:

```rust
async fn restore(seed_phrase: &str) -> Result<()> {
    use anyhow::Context;

    let config = Config::defaults()?;
    std::fs::create_dir_all(&config.data_dir)?;
    let vault_path = config.data_dir.join("identity.vault");

    if vault_path.exists() {
        anyhow::bail!(
            "identity vault already exists at {}; refusing to overwrite",
            vault_path.display()
        );
    }

    // Parse the phrase through a Zeroizing copy so our local String
    // does not linger. (The clap-owned argv slice is still exposed via
    // /proc/<pid>/cmdline — users should avoid passing secrets on the
    // command line; this is documented in the README.)
    let mnemonic = {
        let owned = zeroize::Zeroizing::new(seed_phrase.to_string());
        Mnemonic::parse(&*owned)
    };
    let seed = Seed::from_mnemonic(&mnemonic)
        .context("invalid seed phrase (check word list and checksum)")?;
    let identity = IdentityKey::from_seed(&seed)?;
    let pubkey_hex = identity.public().to_hex();

    let pw1 = read_passphrase("Choose a new vault passphrase: ")?;
    let pw2 = read_passphrase("Confirm passphrase: ")?;
    if *pw1 != *pw2 {
        anyhow::bail!("passphrases do not match");
    }

    Vault::create(&vault_path, identity, pw1.as_str())?;

    println!();
    println!("Identity restored.");
    println!("  public key: {pubkey_hex}");
    println!("  vault:      {}", vault_path.display());
    Ok(())
}
```

- [ ] **Step 2: Smoke test**

```bash
cd /home/myggiz/development/skattr-phase-0b-hardening
TMP=$(mktemp -d)
OUT=$(XDG_DATA_HOME="$TMP" printf 'pw\npw\n' | cargo run --quiet -p skattr-cli -- init 2>&1)
PHRASE=$(echo "$OUT" | grep -E '^  [a-z]+( [a-z]+){23}$' | head -1 | sed 's/^  //')
TMP2=$(mktemp -d)
OUT2=$(XDG_DATA_HOME="$TMP2" printf 'newpw\nnewpw\n' | cargo run --quiet -p skattr-cli -- restore "$PHRASE" 2>&1)
echo "$OUT2" | grep "Identity restored"

# Bad-phrase path:
TMP3=$(mktemp -d)
XDG_DATA_HOME="$TMP3" cargo run --quiet -p skattr-cli -- restore "not a real phrase at all" 2>&1 | tail -5
```

Expected: restore succeeds with a valid phrase; bad-phrase run prints "invalid seed phrase (check word list and checksum)" as the top-level error (anyhow context chain).

- [ ] **Step 3: Run clippy**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/cli/src/main.rs
git commit -m "cli: zeroize restore argv copy + improve bad-phrase error UX

The clap-owned argv slice is out of our reach, but our owned
String copy is now Zeroizing so the local copy doesn't linger
in the heap. Bad phrases get 'invalid seed phrase (check word
list and checksum)' via anyhow::Context instead of the raw
'bip39 decode: ...' surface. Closes follow-up #33."
```

---

## Task 6: `#[cfg(test)] Seed::from_bytes` + strengthen from_seed domain-sep test

**Goal:** Replace the vacuous `from_seed_is_domain_separated_from_raw_bytes` test with a real assertion: given raw bytes `X` wrapped as a `Seed`, `IdentityKey::from_seed(seed)` must produce a DIFFERENT pubkey from `IdentityKey::from_bytes(X)` — proving the HKDF label is actually mixed in.

**Files:** Modify `crates/core/src/identity/seed.rs`, `crates/core/src/identity/key.rs`.

- [ ] **Step 1: Add `cfg(test) Seed::from_bytes`**

In `crates/core/src/identity/seed.rs`, inside `impl Seed { ... }` block (anywhere near `as_bytes`):

```rust
    /// Construct from raw bytes — test-only. Production code must go
    /// through `Seed::generate` or `Seed::from_mnemonic` so the entropy
    /// source stays auditable.
    #[cfg(test)]
    pub(crate) fn from_bytes(bytes: [u8; 32]) -> Self {
        Self { bytes }
    }
```

- [ ] **Step 2: Strengthen the test**

In `crates/core/src/identity/key.rs`, inside `mod tests`, replace the existing `from_seed_is_domain_separated_from_raw_bytes`:

```rust
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
```

- [ ] **Step 3: Run tests + clippy**

```bash
cargo test -p skattr-core --lib identity --release 2>&1 | tail -10
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: 27+ lib tests passed, clippy clean. The new assertion PASSES because HKDF is correctly applied in Task 4's original `from_seed`.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/identity/seed.rs crates/core/src/identity/key.rs
git commit -m "identity: strengthen from_seed domain-separation test

Adds cfg(test) Seed::from_bytes so the test can construct a Seed
with the same raw bytes as a raw IdentityKey, then asserts the
two pubkeys diverge — a real guard against a future refactor that
bypasses HKDF. Closes follow-up #28."
```

---

## Task 7: Signature-byte tamper test

**Goal:** Add a test that flips a byte in a valid `Signature` and asserts `verify_strict` rejects — exercises the malleability guard that motivated `verify_strict` over plain `verify`.

**Files:** Modify `crates/core/src/identity/key.rs`.

- [ ] **Step 1: Add the test**

Append to `mod tests` in `crates/core/src/identity/key.rs`:

```rust
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
```

- [ ] **Step 2: Run**

```bash
cargo test -p skattr-core --lib identity::key::tests::verify_rejects_tampered --release 2>&1 | tail -5
```

Expected: `test result: ok. 1 passed`.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/identity/key.rs
git commit -m "identity: test that verify_strict rejects tampered signatures

Complements verify_rejects_tampered_message by flipping a byte in
the signature itself — the malleability case that motivated the
'verify_strict' (vs 'verify') choice. Closes follow-up #25."
```

---

## Task 8: Salt + param-sensitivity tests for `derive_aead_key`

**Goal:** Lock in the two remaining input channels to `derive_aead_key` beyond passphrase (covered in Task 9): salt and KDF params.

**Files:** Modify `crates/core/src/identity/vault.rs`.

- [ ] **Step 1: Add the tests**

Append to `mod tests` in `crates/core/src/identity/vault.rs`:

```rust
    #[test]
    fn argon2_derive_is_salt_sensitive() {
        let kdf = KdfParams::canonical();
        let a = derive_aead_key("pw", &[0x11; 16], &kdf).unwrap();
        let b = derive_aead_key("pw", &[0x22; 16], &kdf).unwrap();
        assert_ne!(a.as_ref(), b.as_ref());
    }

    #[test]
    fn argon2_derive_is_params_sensitive() {
        let salt = [0x33; 16];
        let a = derive_aead_key("pw", &salt, &KdfParams::canonical()).unwrap();
        let b = derive_aead_key(
            "pw",
            &salt,
            &KdfParams {
                m_kib: 64 * 1024,
                t: 4, // vs canonical 3
                p: 4,
            },
        )
        .unwrap();
        assert_ne!(a.as_ref(), b.as_ref());
    }
```

- [ ] **Step 2: Run (release mode required — Argon2 is slow in debug)**

```bash
cargo test -p skattr-core --lib identity::vault::tests::argon2 --release 2>&1 | tail -5
```

Expected: 4 passed (prior 2 + 2 new).

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/identity/vault.rs
git commit -m "identity: test derive_aead_key salt and param sensitivity

Asserts that (a) same passphrase + different salts → different keys,
and (b) same passphrase + salt + different Argon2 params → different
keys. Closes follow-up #31."
```

---

## Task 9: Unify `verify()` error arms for constant-time parity

**Goal:** Collapse the two failure modes in `IdentityKey::verify` (invalid pubkey encoding vs. signature-verification failure) into a single opaque error, eliminating the timing and error-text distinguisher that would otherwise leak information in the rare case an attacker controls the pubkey.

**Files:** Modify `crates/core/src/identity/key.rs`.

- [ ] **Step 1: Update `verify` + adapt test assertions**

In `crates/core/src/identity/key.rs`, replace `IdentityKey::verify`:

```rust
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
```

No test assertions change (they all just `matches!(err, CoreError::Identity(_))`).

- [ ] **Step 2: Run tests + clippy**

```bash
cargo test -p skattr-core --lib identity::key --release 2>&1 | tail -10
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: 8 passed (existing 5 from Phase 0.B + `from_seed_*` (2) + Task 7's tampered-sig test), clippy clean.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/identity/key.rs
git commit -m "identity: unify verify() error arms to opaque 'verification failed'

Both invalid-pubkey and bad-signature outcomes now return the same
error text. Prevents an attacker with pubkey control from using
timing or error-message distinguishers. Closes follow-up #26."
```

---

## Task 10: `Mnemonic::from_words` normalization

**Goal:** Fix the normalization asymmetry — `Mnemonic::parse` lowercases + trims, but `Mnemonic::from_words` trusts its input verbatim. Make `from_words` normalize identically, so either entry point produces the same Mnemonic for the same visible words.

**Files:** Modify `crates/core/src/identity/seed.rs`.

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `crates/core/src/identity/seed.rs`:

```rust
    #[test]
    fn from_words_normalizes_whitespace_and_case() {
        let raw = vec![
            "ABANDON".to_string(),
            " abandon".to_string(),
            "abandon\t".to_string(),
        ];
        let m = Mnemonic::from_words(raw);
        for w in m.words() {
            assert_eq!(w, "abandon", "from_words must normalize");
        }
    }
```

- [ ] **Step 2: Run test to verify failure**

```bash
cargo test -p skattr-core --lib identity::seed::tests::from_words_normalizes --release 2>&1 | tail -5
```

Expected: FAIL.

- [ ] **Step 3: Update `from_words`**

In `crates/core/src/identity/seed.rs`, replace:

```rust
    /// Build from an explicit list of words.
    #[must_use]
    pub fn from_words(words: Vec<String>) -> Self {
        Self { words }
    }
```

with:

```rust
    /// Build from an explicit list of words. Normalizes each entry to
    /// lower-case and trims surrounding whitespace, matching the
    /// behavior of `parse`.
    #[must_use]
    pub fn from_words(words: Vec<String>) -> Self {
        let words = words
            .into_iter()
            .map(|w| w.trim().to_ascii_lowercase())
            .collect();
        Self { words }
    }
```

- [ ] **Step 4: Run tests + clippy**

```bash
cargo test -p skattr-core --lib identity::seed --release 2>&1 | tail -5
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: 7+ passed, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/identity/seed.rs
git commit -m "identity: Mnemonic::from_words normalizes like parse

Both entry points now lowercase + trim each word, eliminating
the asymmetry flagged in the Phase 0.B final review. Closes
follow-up #36."
```

---

## Task 11: ADR-0004 — passphrase Unicode normalization

**Goal:** Commit the decision to treat the passphrase as opaque bytes (`&str.as_bytes()`) without Unicode normalization, and document the constraint in the `derive_aead_key` docstring so future Claude sessions see it.

**Files:** Create `docs/adr/0004-passphrase-normalization.md`; modify `crates/core/src/identity/vault.rs` (docstring only).

- [ ] **Step 1: Write the ADR**

Create `docs/adr/0004-passphrase-normalization.md`:

```markdown
# ADR 0004: Passphrase Unicode normalization

- **Status:** Accepted
- **Date:** 2026-04-17

## Context

`derive_aead_key` in `crates/core/src/identity/vault.rs` feeds the
user's passphrase into Argon2id as `passphrase.as_bytes()`. Non-ASCII
passphrases can be represented in multiple equivalent Unicode forms
(NFC, NFD, NFKC, NFKD); the same visible string may yield different
byte sequences depending on the input method, OS, and copy-paste
source. Without a normalization step, a user who creates a vault on
macOS (which historically preferred NFD for HFS+ filenames) and
re-opens it on Linux (NFC) would fail authentication even when typing
the "same" passphrase.

## Decision

**Passphrase bytes are used verbatim with no normalization.** Callers
are responsible for feeding stable byte sequences. The CLI surface
documents this constraint prominently (README and on-screen prompt
guidance in Phase 2).

Rationale:

- Unicode normalization is subtle and adds attack surface — every
  additional step is a potential source of inconsistency.
- In the Phase 0.B/Phase 1 timeline, the CLI is the only surface;
  users typing at a terminal generally get consistent output from
  their input method.
- If we ever add mobile or web clients, those surfaces ship their
  own NFC-normalizing input and the caller can do the conversion.
- Fixing this later is a breaking change only for users who
  actually rely on non-ASCII passphrases — a small cohort. We
  accept the risk and document the contract.

## Consequences

- **Good:** No crypto-adjacent normalization code to audit.
- **Bad:** Non-ASCII passphrases can lock users out if their input
  method changes. Mitigation: the Phase 2 UI will ship an NFC
  normalization pass on the passphrase-entry textbox.
- Docstring on `derive_aead_key` and the restore/init CLI surfaces
  will state: "ASCII passphrases recommended; non-ASCII entries are
  used verbatim and will not round-trip across OSes with different
  default Unicode forms."

## Alternatives considered

- **NFC normalization in `derive_aead_key`:** rejected — adds an
  `unicode-normalization` dependency and shifts the contract. Revisit
  if user reports make this painful.
- **NFKC (compatibility normalization):** rejected — stronger than NFC,
  would silently collapse visually-distinct characters (e.g. ligatures)
  which is surprising.
- **Stronger input validation (ASCII-only enforcement):** rejected —
  excludes users with non-Latin keyboards from ever setting an
  ergonomic passphrase.
```

- [ ] **Step 2: Update the `derive_aead_key` docstring**

In `crates/core/src/identity/vault.rs`, replace the docstring on `derive_aead_key`:

```rust
/// Run Argon2id on `passphrase` with `salt` and `kdf` params, producing a
/// 32-byte AEAD key.
///
/// The returned buffer zeros on drop; callers must not stash the raw bytes.
///
/// **Passphrase bytes are used verbatim** — no Unicode normalization is
/// applied. ASCII passphrases are recommended; non-ASCII entries will not
/// round-trip across OSes with different default Unicode forms. See
/// `docs/adr/0004-passphrase-normalization.md`.
fn derive_aead_key(
```

- [ ] **Step 3: Run tests + clippy (sanity — no behavior change)**

```bash
cargo test -p skattr-core --lib identity::vault --release 2>&1 | tail -5
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: all pass, clippy clean.

- [ ] **Step 4: Commit**

```bash
git add docs/adr/0004-passphrase-normalization.md crates/core/src/identity/vault.rs
git commit -m "docs: ADR-0004 passphrase Unicode normalization

Decides: passphrase bytes used verbatim, no Unicode normalization.
Docstring on derive_aead_key now states the contract so future
sessions do not silently add normalization. Closes follow-up #30."
```

---

## Task 12: CLI `--data-dir` override

**Goal:** Add a global `--data-dir <PATH>` flag to `skattr` so users (and test scripts) can redirect the vault/data location without resorting to `XDG_DATA_HOME` gymnastics.

**Files:** Modify `crates/cli/src/main.rs`.

- [ ] **Step 1: Extend the CLI struct + wire into init/restore**

In `crates/cli/src/main.rs`, update the `Cli` struct to add a `data_dir` global option:

```rust
#[derive(Debug, Parser)]
#[command(
    name = "skattr",
    version,
    about = "Skattr: metadata-resistant encrypted messaging over Tor.",
    long_about = None,
)]
struct Cli {
    /// Path to config file. Defaults to `~/.config/skattr/config.toml`.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Override the data directory (vault + daemon state). Defaults to
    /// the XDG data dir.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    /// JSON output (for scripting).
    #[arg(long, global = true)]
    json: bool,

    /// Subcommand.
    #[command(subcommand)]
    cmd: Command,
}
```

Thread `data_dir` through `main()` → `init()` / `restore()`. Replace the dispatch block in `main`:

```rust
    match cli.cmd {
        Command::Init => init(cli.data_dir.as_deref()).await,
        Command::Restore { seed } => restore(&seed, cli.data_dir.as_deref()).await,
        Command::Daemon { detach } => daemon(detach).await,
        Command::Invite { qr } => invite(qr).await,
        Command::Add { link } => add(&link).await,
        Command::Contacts => contacts().await,
        Command::Send { contact, text } => send(&contact, &text).await,
        Command::Tail { contact } => tail(contact.as_deref()).await,
    }
```

Add a helper that resolves the effective data dir:

```rust
fn effective_data_dir(override_dir: Option<&std::path::Path>) -> Result<PathBuf> {
    if let Some(d) = override_dir {
        return Ok(d.to_path_buf());
    }
    Ok(Config::defaults()?.data_dir)
}
```

Update `init()`:

```rust
async fn init(data_dir_override: Option<&std::path::Path>) -> Result<()> {
    let data_dir = effective_data_dir(data_dir_override)?;
    std::fs::create_dir_all(&data_dir)?;
    let vault_path = data_dir.join("identity.vault");

    if vault_path.exists() {
        anyhow::bail!(
            "identity vault already exists at {}; refusing to overwrite",
            vault_path.display()
        );
    }

    let pw1 = read_passphrase("Choose a passphrase: ")?;
    let pw2 = read_passphrase("Confirm passphrase: ")?;
    if *pw1 != *pw2 {
        anyhow::bail!("passphrases do not match");
    }

    let seed = Seed::generate()?;
    let identity = IdentityKey::from_seed(&seed)?;
    let pubkey_hex = identity.public().to_hex();

    Vault::create(&vault_path, identity, pw1.as_str())?;

    let mnemonic = seed.to_mnemonic()?;
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
```

Update `restore()`:

```rust
async fn restore(seed_phrase: &str, data_dir_override: Option<&std::path::Path>) -> Result<()> {
    use anyhow::Context;

    let data_dir = effective_data_dir(data_dir_override)?;
    std::fs::create_dir_all(&data_dir)?;
    let vault_path = data_dir.join("identity.vault");

    if vault_path.exists() {
        anyhow::bail!(
            "identity vault already exists at {}; refusing to overwrite",
            vault_path.display()
        );
    }

    let mnemonic = {
        let owned = zeroize::Zeroizing::new(seed_phrase.to_string());
        Mnemonic::parse(&*owned)
    };
    let seed = Seed::from_mnemonic(&mnemonic)
        .context("invalid seed phrase (check word list and checksum)")?;
    let identity = IdentityKey::from_seed(&seed)?;
    let pubkey_hex = identity.public().to_hex();

    let pw1 = read_passphrase("Choose a new vault passphrase: ")?;
    let pw2 = read_passphrase("Confirm passphrase: ")?;
    if *pw1 != *pw2 {
        anyhow::bail!("passphrases do not match");
    }

    Vault::create(&vault_path, identity, pw1.as_str())?;

    println!();
    println!("Identity restored.");
    println!("  public key: {pubkey_hex}");
    println!("  vault:      {}", vault_path.display());
    Ok(())
}
```

(Config::defaults() is no longer called directly in `init`/`restore` — it's inside `effective_data_dir`.)

- [ ] **Step 2: Smoke test**

```bash
cd /home/myggiz/development/skattr-phase-0b-hardening
TMP=$(mktemp -d)
printf 'pw\npw\n' | cargo run --quiet -p skattr-cli -- --data-dir "$TMP" init 2>&1 | tail -5
ls -la "$TMP/identity.vault"
```

Expected: "Identity created." banner with `vault: $TMP/identity.vault`; file exists.

- [ ] **Step 3: Run clippy + tests**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
cargo test --workspace --release 2>&1 | tail -5
```

Expected: clean; no test regressions.

- [ ] **Step 4: Commit**

```bash
git add crates/cli/src/main.rs
git commit -m "cli: --data-dir global override

Lets users and test scripts point skattr at an arbitrary data
directory without XDG_DATA_HOME gymnastics. Closes follow-up #35."
```

---

## Post-plan wrap-up

- [ ] **Step 1: Verification gate**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --release
cd crates/core && cargo +nightly fuzz build vault_parser && cd ../..
```

All four must pass.

- [ ] **Step 2: Update CHANGELOG.md**

Append under `[Unreleased]` → `### Added` (or `### Changed` where appropriate) — a single bullet block:

```markdown
- **Phase 0.B hardening:** atomic + fsync'd vault writes (`atomic_write_vault`); `Vault::change_passphrase` now crash-safe via tempfile → rename; `IdentityKey::from_bytes` takes `Zeroizing<[u8; 32]>`; mnemonic phrase/entropy intermediates zeroized; `verify()` returns a single opaque "verification failed" error for constant-time parity; `Mnemonic::from_words` normalizes like `parse`; CLI gains `--data-dir` override; ADR-0004 pins passphrase byte contract.
```

Commit:

```bash
git add CHANGELOG.md
git commit -m "changelog: Phase 0.B hardening pass"
```

- [ ] **Step 3: Update CLAUDE.md**

Shrink the "Known Phase 0.B caveats" text. Open `CLAUDE.md`, locate the Repository-state paragraph, and replace the sentence that starts "Known Phase 0.B caveats: ..." with:

```
Phase 0.B hardening is complete — `change_passphrase` is now crash-safe, `IdentityKey::from_bytes` enforces Zeroizing, mnemonic intermediates are wiped, `verify()` collapses its error arms for constant-time parity, and an ADR pins the passphrase byte contract.
```

Commit:

```bash
git add CLAUDE.md
git commit -m "docs: CLAUDE.md — Phase 0.B hardening caveats resolved"
```

- [ ] **Step 4: Mark follow-up tasks complete**

After everything is merged, mark TaskList items #25, #26, #27, #28, #29, #30, #31, #32, #33, #34, #35, #36 as completed.

---

## Notes for the executing engineer

- **No protocol or wire-format changes.** Every task preserves the on-disk vault format, BIP39 phrase encoding, and Ed25519 signature shape. Existing vaults written during Phase 0.B must decrypt cleanly after all these changes.
- **`cargo test --release` remains mandatory** for any task touching Argon2 (that's `vault.rs` tests and anything below). Debug-mode Argon2 at `m=64 MiB` takes 10+ seconds per derivation.
- **Task 3 is the only API change** (`IdentityKey::from_bytes` signature). It's `pub(crate)` so no external breakage, but internal call sites must all update in the same commit — don't split.
- **Tasks 1, 2, 3 form a short dependency chain**; all other tasks are independent. A parallel-agent run could fan out 7–8 of them after the chain lands.
- **Tasks are CBOR-backwards-compatible.** Atomic write doesn't change the serialized bytes; `change_passphrase` atomic rename doesn't change the format; all tests exercise real files end-to-end.
- **When tests need release mode, CI will feel it.** These hardening tasks roughly double the `cargo test --release` wall-clock due to the extra Argon2 runs. Acceptable for Phase 0; if CI time becomes painful, consider a `skattr-slow-tests` gate.
