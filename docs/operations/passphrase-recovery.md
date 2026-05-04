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

If you've forgotten your passphrase, recovery is **not possible**
through the daemon — by design. The BIP39 seed phrase you wrote
down during first-run is your only recovery path:

1. Back up `${data_dir}` to a safe location (so you can recover
   any in-flight messages later if you choose to dig manually).
2. Delete `${data_dir}/identity.vault` and `${data_dir}/skattr.sqlite.age`.
3. Run `skattr restore <seed words>` and choose a new passphrase.
4. Your contacts will see your old onion as unreachable; you'll
   need to send them new invites (or wait for them to send you
   one).

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
