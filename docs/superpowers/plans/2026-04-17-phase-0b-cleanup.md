# Phase 0.B Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the 3 remaining follow-ups (#37, #38, #39) from the Phase 0.B hardening pass — tempfile leak cleanup, `encrypt_identity` helper extraction, and `Vault::open` decrypting in-place to eliminate the `Vec<u8>` plaintext intermediate.

**Architecture:** All three tasks touch `crates/core/src/identity/vault.rs` only. Pure refactoring + one internal helper extraction + one crypto-API-swap. No test regressions, no wire-format changes, no new dependencies.

**Tech Stack:** Same as Phase 0.B hardening — workspace already declares everything needed.

**Exit criteria:**
- Task-list items #37, #38, #39 closed.
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --release` all green.
- No new Vec<u8> plaintext allocation in `Vault::open`'s decrypt path.
- `Vault::create` and `Vault::change_passphrase` share a single `encrypt_identity` call.
- Failed `atomic_write_vault` invocations leave no `.vault.tmp` sidecar behind.

---

## File structure

Only `crates/core/src/identity/vault.rs` is modified. Three commits total (one per task).

---

## Pre-flight

```bash
cd /home/myggiz/development/skattr
. "$HOME/.cargo/env"

git worktree add ../skattr-phase-0b-cleanup -b phase-0b-cleanup
cd ../skattr-phase-0b-cleanup
cargo build --workspace
cargo test --workspace --release 2>&1 | tail -5
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
```

All gates green before starting. All subsequent task paths assume `/home/myggiz/development/skattr-phase-0b-cleanup`.

---

## Task 1: `Vault::open` decrypts in-place into `Zeroizing<[u8; 32]>`

**Goal:** Replace `Aead::decrypt` (which allocates a `Vec<u8>` for plaintext) with `Aead::decrypt_in_place_detached` (writes plaintext into a caller-provided buffer). The new buffer IS the `Zeroizing<[u8; 32]>` we're already using — so no intermediate plaintext ever materializes as a Vec.

**Files:** Modify `crates/core/src/identity/vault.rs`.

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `crates/core/src/identity/vault.rs`:

```rust
    #[test]
    fn open_rejects_wrong_length_ciphertext() {
        // Synthesize a vault whose ciphertext is the wrong length. Must
        // be rejected BEFORE attempting AEAD decrypt (which would succeed
        // only for the specific 32-byte-plaintext case our code now
        // requires).
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bad_len.vault");
        let id = IdentityKey::generate().unwrap();
        Vault::create(&path, id, "pw").unwrap();

        // Mutate the CBOR: strip two ciphertext bytes so total length
        // is 30 + 16 (tag) = 46 instead of 32 + 16 = 48.
        let bytes = std::fs::read(&path).unwrap();
        let mut vf: VaultFile = ciborium::de::from_reader(&bytes[..]).unwrap();
        vf.ciphertext.truncate(vf.ciphertext.len() - 2);
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&vf, &mut buf).unwrap();
        std::fs::write(&path, buf).unwrap();

        let err = Vault::open(&path, "pw")
            .err()
            .expect("truncated ciphertext must fail");
        assert!(matches!(err, crate::error::CoreError::Identity(_)));
    }
```

- [ ] **Step 2: Run test to verify it passes against existing impl (baseline)**

```bash
cargo test -p skattr-core --lib identity::vault::tests::open_rejects_wrong_length --release 2>&1 | tail -5
```

Expected: the existing `Aead::decrypt` path already rejects a too-short ciphertext (the MAC fails OR the plaintext length ≠ 32 causes the `try_into` to fail). Test passes. If it doesn't, stop — we need to understand why before refactoring.

- [ ] **Step 3: Rewrite the decrypt path**

In `crates/core/src/identity/vault.rs`, `Vault::open`, replace this section:

```rust
        let plaintext = cipher
            .decrypt(
                nonce,
                Payload {
                    msg: &vf.ciphertext,
                    aad: VAULT_AAD,
                },
            )
            .map_err(|_| {
                CoreError::Identity(
                    "aead decrypt failed (wrong passphrase or tampered vault)".into(),
                )
            })?;

        // Move the plaintext into a Zeroizing-guarded fixed-size buffer before
        // handing the bytes off to IdentityKey. Any copy the optimizer leaves
        // on the stack is tied to this binding's drop.
        let secret = {
            if plaintext.len() != 32 {
                // Zero the oversized Vec before bailing so no secret-ish bytes linger.
                let mut p = plaintext;
                use zeroize::Zeroize;
                p.zeroize();
                return Err(CoreError::Identity(
                    "decrypted secret has unexpected length".into(),
                ));
            }
            let mut buf = Zeroizing::new([0u8; 32]);
            buf.copy_from_slice(&plaintext);
            // Zero the Vec now that its contents are copied out.
            let mut p = plaintext;
            use zeroize::Zeroize;
            p.zeroize();
            buf
        };
```

with:

```rust
        // Decrypt in-place directly into a Zeroizing<[u8; 32]>. The wire
        // format is `ct_body (32 bytes) || poly1305_tag (16 bytes)`; we
        // split them explicitly so AEAD output never touches a Vec<u8>.
        const POLY1305_TAG_LEN: usize = 16;
        const PLAINTEXT_LEN: usize = 32;
        if vf.ciphertext.len() != PLAINTEXT_LEN + POLY1305_TAG_LEN {
            return Err(CoreError::Identity(
                "ciphertext has unexpected length".into(),
            ));
        }
        let (ct_body, tag_bytes) = vf.ciphertext.split_at(PLAINTEXT_LEN);
        let tag = chacha20poly1305::Tag::from_slice(tag_bytes);

        let mut secret = Zeroizing::new([0u8; 32]);
        secret.copy_from_slice(ct_body);
        cipher
            .decrypt_in_place_detached(nonce, VAULT_AAD, secret.as_mut(), tag)
            .map_err(|_| CoreError::Identity("verification failed".into()))?;
```

(The rest of `Vault::open` — the `Ok((Self { ... }, IdentityKey::from_bytes(secret)))` tail — is unchanged.)

Note: the error string changes from the verbose "aead decrypt failed (wrong passphrase or tampered vault)" to the unified "verification failed" — matching the `verify()` convention established in hardening Task 9. This is a deliberate consistency improvement; existing tests assert `matches!(err, CoreError::Identity(_))` so no test assertions break.

- [ ] **Step 4: Check imports**

`chacha20poly1305::Tag` and the `AeadInPlace` trait (which provides `decrypt_in_place_detached`) may not be in the existing `use` block. Inspect the top of `vault.rs`; if missing, update the imports. The existing block has:

```rust
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
```

Change to:

```rust
use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{Key, Tag, XChaCha20Poly1305, XNonce};
```

(The `Aead` trait provides `decrypt`/`encrypt` that allocate; `AeadInPlace` provides `decrypt_in_place_detached`/`encrypt_in_place_detached` that don't. `Payload` is still used by the `encrypt` calls in `Vault::create` and `encrypt_identity` — wait, `encrypt_in_place_detached` takes AAD separately. We still use the allocating `encrypt` path for writing vaults, so `Payload` is still needed there. Keep both.)

Revised import block:

```rust
use chacha20poly1305::aead::{Aead, AeadInPlace, KeyInit, Payload};
use chacha20poly1305::{Key, Tag, XChaCha20Poly1305, XNonce};
```

Update the `Tag::from_slice(tag_bytes)` call in Step 3's snippet to use the imported name: `let tag = Tag::from_slice(tag_bytes);`.

- [ ] **Step 5: Run tests + clippy**

```bash
cargo test -p skattr-core --lib identity::vault --release 2>&1 | tail -10
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: 15 passed (prior 14 + the new `open_rejects_wrong_length_ciphertext`), clippy clean.

Pay special attention: `open_recovers_the_identity`, `any_ciphertext_bitflip_is_detected`, `aad_mismatch_is_detected`, `change_passphrase_rotates_salt_and_nonce`, and `open_rejects_wrong_passphrase` all exercise the decrypt path. They must all still pass.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/identity/vault.rs
git commit -m "identity: Vault::open decrypt_in_place_detached — no Vec plaintext

Replaces Aead::decrypt (which allocates a Vec<u8> for plaintext) with
AeadInPlace::decrypt_in_place_detached, writing the plaintext directly
into our Zeroizing<[u8; 32]> secret buffer. The intermediate Vec is
gone — plaintext no longer touches a heap allocation outside
IdentityKey's own struct.

Also tightens the length check (32 byte body + 16 byte tag; anything
else is rejected before AEAD) and unifies the error text with
verify()'s 'verification failed' for consistency.

Closes follow-up #39."
```

---

## Task 2: Extract `encrypt_identity` helper

**Goal:** DRY the 18-line encrypt path shared between `Vault::create` and `Vault::change_passphrase` into a single private helper. The flow (fresh `KdfParams::canonical()` → OsRng salt + nonce → `derive_aead_key` → AEAD encrypt → build `VaultFile`) lives in exactly one place.

**Files:** Modify `crates/core/src/identity/vault.rs`.

- [ ] **Step 1: Introduce the helper**

Add a new private free function `encrypt_identity` below `derive_aead_key` in `crates/core/src/identity/vault.rs`:

```rust
/// Encrypt the given identity under `passphrase`, producing a ready-to-write
/// `VaultFile`. Fresh salt + nonce per call.
///
/// Private: called by `Vault::create` and `Vault::change_passphrase`.
fn encrypt_identity(identity: IdentityKey, passphrase: &str) -> Result<VaultFile> {
    let kdf = KdfParams::canonical();
    let mut salt = [0u8; 16];
    let mut nonce_bytes = [0u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);

    let aead_key = derive_aead_key(passphrase, &salt, &kdf)?;
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

    Ok(VaultFile {
        v: VAULT_VERSION,
        kdf,
        salt,
        nonce: nonce_bytes,
        ciphertext,
    })
}
```

- [ ] **Step 2: Refactor `Vault::create`**

Replace the body of `Vault::create` that starts at `let kdf = KdfParams::canonical();` and ends with `atomic_write_vault(path, &vf)?;` / `Ok(Self { path: path.to_path_buf() })`.

The new full body:

```rust
    pub fn create(path: &Path, identity: IdentityKey, passphrase: &str) -> Result<Self> {
        if path.exists() {
            return Err(CoreError::Identity(format!(
                "vault already exists at {}",
                path.display()
            )));
        }

        let vf = encrypt_identity(identity, passphrase)?;
        atomic_write_vault(path, &vf)?;

        Ok(Self {
            path: path.to_path_buf(),
        })
    }
```

Preserve the existing doc comment exactly as it was.

- [ ] **Step 3: Refactor `Vault::change_passphrase`**

Replace the body of `Vault::change_passphrase` from `let (_, identity) = Vault::open(&self.path, old)?;` onward.

The new full body:

```rust
    pub fn change_passphrase(&mut self, old: &str, new: &str) -> Result<()> {
        // Decrypt with the old passphrase first; if it fails, don't touch
        // the file.
        let (_, identity) = Vault::open(&self.path, old)?;
        let vf = encrypt_identity(identity, new)?;
        atomic_write_vault(&self.path, &vf)?;
        Ok(())
    }
```

Preserve the doc comment (the "Crash-safe: writes the new vault to a sidecar..." block) exactly as it was.

- [ ] **Step 4: Run tests + clippy**

```bash
cargo test -p skattr-core --lib identity::vault --release 2>&1 | tail -10
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: 15 passed (all prior including Task 1's test), clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/identity/vault.rs
git commit -m "identity: extract encrypt_identity helper — DRY vault write path

Factors the shared KDF → encrypt → VaultFile flow out of Vault::create
and Vault::change_passphrase into a single private helper. Both call
sites now read as: encrypt_identity(identity, passphrase)? →
atomic_write_vault(path, &vf)?. No behaviour change; existing 15 tests
still cover both paths.

Closes follow-up #38."
```

---

## Task 3: Cleanup `.vault.tmp` on `atomic_write_vault` error

**Goal:** When any step inside `atomic_write_vault` fails, the leftover `.vault.tmp` sidecar currently lingers until the next successful write overwrites it. Add best-effort cleanup so a failed write leaves the filesystem in a recognizable state.

**Files:** Modify `crates/core/src/identity/vault.rs`.

- [ ] **Step 1: Write the test**

Append to `mod tests` in `crates/core/src/identity/vault.rs`:

```rust
    #[test]
    fn create_overwrites_stale_sidecar() {
        // A stale .vault.tmp from a prior failed attempt must not block
        // a fresh Vault::create: the tempfile open truncates, and the
        // subsequent rename consumes the sidecar. Post-condition: no
        // .vault.tmp remains.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("id.vault");

        std::fs::write(path.with_extension("vault.tmp"), b"stale").unwrap();

        let id = IdentityKey::generate().unwrap();
        Vault::create(&path, id, "pw").unwrap();

        assert!(
            !path.with_extension("vault.tmp").exists(),
            "stale sidecar must not remain after successful create"
        );
    }
```

This is the testable half of follow-up #37: a stale sidecar from a prior failed attempt must not leak into a subsequent successful create. The stronger property — error-path cleanup of a sidecar written during a mid-flow failure — is the subject of the refactor in Step 3, but is awkward to test portably (the cleanup uses `let _ = remove_file(...)` which produces no observable difference from "rename already consumed it"). The refactor codifies the best-effort cleanup so error paths carry it automatically.

- [ ] **Step 2: Run test to verify baseline**

```bash
cargo test -p skattr-core --lib identity::vault::tests::create_overwrites_stale_sidecar --release 2>&1 | tail -5
```

Expected: PASS even before the refactor (File::create + rename already handle this case).

- [ ] **Step 3: Refactor `atomic_write_vault`**

In `crates/core/src/identity/vault.rs`, replace the body of `atomic_write_vault` with a two-function split:

```rust
/// Durably write `vf` to `path`: serialize → tempfile → fsync tempfile →
/// rename over target → fsync parent directory.
///
/// On any failure after the tempfile is created, best-effort removes
/// the `.vault.tmp` sidecar so a subsequent caller sees a clean
/// filesystem.
fn atomic_write_vault(path: &Path, vf: &VaultFile) -> Result<()> {
    let tmp_path = path.with_extension("vault.tmp");
    match atomic_write_vault_inner(path, &tmp_path, vf) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Best-effort cleanup. If the sidecar is a directory (test
            // scenario) or held by another process, this is a no-op —
            // we don't care; the original error is what matters.
            let _ = std::fs::remove_file(&tmp_path);
            Err(e)
        }
    }
}

/// The actual write machinery, wrapped by `atomic_write_vault` for
/// error-path cleanup.
fn atomic_write_vault_inner(path: &Path, tmp_path: &Path, vf: &VaultFile) -> Result<()> {
    let mut buf = Vec::new();
    ciborium::ser::into_writer(vf, &mut buf)
        .map_err(|e| CoreError::CborEncode(e.to_string()))?;

    {
        let mut f = std::fs::File::create(tmp_path)?;
        use std::io::Write;
        f.write_all(&buf)?;
        f.sync_all()?;
    }
    std::fs::rename(tmp_path, path)?;

    // Fsync parent directory so the rename itself is durable.
    if let Some(parent) = path.parent() {
        #[cfg(unix)]
        {
            let dir = std::fs::File::open(parent)?;
            dir.sync_all()?;
        }
        #[cfg(not(unix))]
        {
            let _ = parent;
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests + clippy**

```bash
cargo test -p skattr-core --lib identity::vault --release 2>&1 | tail -10
cargo clippy -p skattr-core --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: 16 passed (prior 15 + the new stale-sidecar test), clippy clean. Existing tests, including `change_passphrase_survives_simulated_new_create_failure` (hardening Task 2) and `no_tempfile_sidecar_after_create` (hardening Task 1), must all still pass.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/identity/vault.rs
git commit -m "identity: atomic_write_vault cleans up .vault.tmp on error

Wraps the write/fsync/rename flow in atomic_write_vault_inner and
best-effort-removes the sidecar on any error return. If the sidecar
is a directory (test blocker) or held by another process, the
cleanup is a no-op; the original error is returned unchanged.

Also adds a test asserting a stale sidecar from a prior failed
attempt is not left behind after a successful Vault::create.

Closes follow-up #37."
```

---

## Post-plan wrap-up

- [ ] **Step 1: Full verification gate**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --release
cd crates/core && cargo +nightly fuzz build vault_parser && cd ../..
```

All four must pass. If `fmt --check` fails (probable given past plans' line-width drift), apply `cargo fmt --all` and commit as a separate `style:` commit, same pattern as prior phases.

- [ ] **Step 2: Update CHANGELOG.md**

Append under `[Unreleased]` (the existing Phase 0.B hardening bullet can grow or get a new bullet — choose whichever fits):

```markdown
- **Phase 0.B cleanup:** `Vault::open` decrypts in-place into `Zeroizing<[u8; 32]>` — no Vec<u8> plaintext intermediate; `encrypt_identity` helper DRYs the vault-write path; `atomic_write_vault` best-effort cleans up the `.vault.tmp` sidecar on error.
```

Commit:

```bash
git add CHANGELOG.md
git commit -m "changelog: Phase 0.B cleanup pass"
```

- [ ] **Step 3: Verify no follow-ups reopen**

After this plan, the TaskList should show #37, #38, #39 as completed. No new follow-ups expected from this pass (it's cleanup, not new surface area) — but if the reviewer surfaces something, file it as a Phase 1 item.

---

## Notes for the executing engineer

- **All three tasks touch `crates/core/src/identity/vault.rs` only.** No imports into other modules change; no new dependencies.
- **Task 1 is the only one with a non-trivial behavior change.** The `decrypt_in_place_detached` API splits AAD from the message — the wire format stays `body || tag` but our code now treats them as separate values. Existing tests that decode the CBOR `VaultFile` and mutate `ciphertext[0]` still work because `ct_body` and `tag_bytes` are both slices of `vf.ciphertext`.
- **Task 2 is pure refactoring.** If it changes behaviour, you've done it wrong. Bisect against the pre-task commit if any test starts failing.
- **Task 3's test is mild.** The real assertion (cleanup on error) is awkward to test portably; the stale-sidecar round-trip is the closest non-platform-specific proof.
- **Release-mode `cargo test` is mandatory** for any task run that exercises Argon2 (that's all three tasks' tests, indirectly via `Vault::create`/`change_passphrase`/`open`).
