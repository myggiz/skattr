# Passphrase Recovery

Skattr's `Command::ChangePassphrase` re-encrypts the on-disk
**identity vault** — the file at `${data_dir}/identity.vault`,
which holds your 32-byte Ed25519 secret wrapped under an
Argon2id-derived key from your passphrase.

The SQLite database is **not** re-encrypted by this operation. Its
encryption is via `age`, keyed by a passphrase derived from the
BIP39 seed via HKDF. The seed is unchanged when the user passphrase
changes, so the database key is unchanged too.

## Crash safety

`Vault::change_passphrase` is atomic on its own:
1. Decrypts `identity.vault` under the OLD passphrase.
2. Builds a new ciphertext under the NEW passphrase.
3. Writes the new ciphertext to `identity.vault.tmp`.
4. fsyncs the temp file.
5. Atomically renames `identity.vault.tmp` → `identity.vault`.
6. fsyncs the parent directory.

If the daemon is killed at any point before step 5, the original
`identity.vault` remains intact and unlocks under the OLD
passphrase. After step 5, the new file unlocks under the NEW
passphrase. There is no in-between state.

You can verify this by examining `${data_dir}/`:
- If `identity.vault.tmp` exists, the rename didn't land — the
  current `identity.vault` is still the OLD one. Delete the `.tmp`
  file safely.
- If `identity.vault.tmp` is absent and `identity.vault` is
  present, you have a consistent state — try unlocking with one
  of your passphrases.

## Manual recovery

If the daemon is in a state where neither the OLD nor the NEW
passphrase works, the most likely cause is data corruption (disk
failure, manual tampering, or a panic during the AEAD operation —
which would be a bug worth filing).

1. Stop the daemon.
2. Inspect `${data_dir}/`:
   ```
   ls -la ${data_dir}/identity.vault*
   ```
3. If `identity.vault.tmp` exists, delete it:
   ```
   rm ${data_dir}/identity.vault.tmp
   ```
4. Try unlocking with each of your passphrases.

If the file is genuinely corrupt (rare), the only path forward is
to restore from backup OR re-derive your identity from the BIP39
seed phrase you wrote down at first-run (see "Lost passphrase"
below).

## Audit log

Each successful passphrase change appends a row to the
`passphrase_audit` table. Settings → Identity surfaces "Last
changed: <timestamp>" from this log. Rows are append-only and
never deleted by the retention sweep.

## Lost passphrase

Your passphrase protects the local **identity vault**
(`identity.vault`, Argon2id → XChaCha20-Poly1305). It is **not**
the key to your message history: the message database
(`skattr.sqlite.age`) is encrypted under a key derived from your
**seed phrase** (`HKDF(seed, "skattr-storage-v1")`). This means
that if you still have your 24-word seed phrase, you can recover
**fully** — identity and message history — without knowing the old
passphrase.

**Recommended recovery (preserves history):**

**If your files are intact on this machine** — you simply forgot the
passphrase (the common case) — you do **not** need a backup. The message
database key is derived from your seed phrase, not your passphrase, so a fresh
vault with a new passphrase still opens your existing database.

1. Stop the daemon.
2. Move or rename the inaccessible `identity.vault` out of the way (do **not**
   delete `skattr.sqlite.age`). `skattr restore` will not overwrite an existing
   vault.
3. Recreate the vault from your seed phrase (you will be prompted to set a new
   passphrase):
   ```
   skattr restore "<24 seed words>"
   ```
4. Start the daemon. It re-derives the seed-based storage key and decrypts your
   existing `skattr.sqlite.age` — **your message history is preserved; no backup
   archive is required.**

**On a clean machine, or if `skattr.sqlite.age` is lost or corrupt:** restore
from a backup archive you made earlier with `skattr backup <archive.age>`:

1. Stop the daemon (or start fresh on the new machine).
2. Restore identity + message history from the backup:
   ```
   skattr restore-backup "<24 seed words>" <archive.age>
   ```
   This re-derives the database key from the seed phrase, decrypts the
   backed-up history, and prompts for a new passphrase.

**⚠️ Last resort only — permanently destroys message history:**
Deleting `skattr.sqlite.age` (and its `-wal`/`-shm` sidecars,
if present) discards **all local message history permanently**.
There is no way to recover deleted history, even with the seed
phrase. Only do this if you have no backup and accept the
permanent loss of history. After deletion, you can restore your
identity with `skattr restore "<24 seed words>"`, but your
contacts will see your old onion as unreachable and you will need
to send them new invites.

The seed phrase is the only thing that survives passphrase loss.
Store it offline. Do not commit it to git. Do not share it with
anyone.

## What this doc does NOT cover

- Recovering a lost BIP39 seed: there is no recovery path. The
  seed IS the identity.
- Migrating an identity to a new device: not in 2.F. Phase 5+
  will add an explicit "export + restore" flow.
- Multi-device shared identity: out of scope for skattr's
  threat model — each device runs its own identity.
