# Maintainer prerequisite: minisign keypair (Phase 2.G Task 12)

**Status:** the real maintainer public key (key ID `EEDBFDA4BF232D38`) is
committed to `docs/install/minisign.pub`. The remaining maintainer action
before tagging `v0.1.0` is wiring the GitHub Actions secrets
(`MINISIGN_SECRET_KEY` / `MINISIGN_PASSWORD`) so `release.yml` can sign — see
step 4 below; steps 1–3 (key generation) are already done. Delete this doc
once the secrets are set.

## Why this is a maintainer-only step

Generating the keypair requires:

- Running `minisign -G` on a trusted machine (not in CI).
- Choosing + safeguarding a passphrase.
- Setting two GitHub Actions secrets (`MINISIGN_SECRET_KEY`,
  `MINISIGN_PASSWORD`) on the `myggiz/skattr` repo.

None of these can be safely automated by Claude Code (or any
agent). The secret key never enters the repo or CI logs.

## Procedure (run once, before the first `v*` tag)

1. **Install minisign locally:**
   ```bash
   sudo pacman -S minisign        # Arch / Manjaro
   sudo apt-get install minisign  # Debian / Ubuntu
   brew install minisign          # macOS
   ```

2. **Generate the keypair:**
   ```bash
   mkdir -p ~/.private
   minisign -G \
       -p $(git rev-parse --show-toplevel)/docs/install/minisign.pub \
       -s ~/.private/skattr-minisign-secret.key
   ```
   Use a strong passphrase. Save it in your password manager.

3. **Verify the public key replaced the placeholder:**
   ```bash
   cat $(git rev-parse --show-toplevel)/docs/install/minisign.pub
   ```
   The `untrusted comment:` line should NOT say "PLACEHOLDER".

4. **Set GitHub Actions secrets:**
   ```bash
   # MINISIGN_SECRET_KEY: base64-encoded encrypted secret-key file content
   base64 -w0 ~/.private/skattr-minisign-secret.key | \
       gh secret set MINISIGN_SECRET_KEY --repo myggiz/skattr

   # MINISIGN_PASSWORD: the passphrase from step 2
   gh secret set MINISIGN_PASSWORD --repo myggiz/skattr
   # gh prompts for the value
   ```
   Verify:
   ```bash
   gh secret list --repo myggiz/skattr | grep MINISIGN
   ```

5. **Commit the real pubkey:**
   ```bash
   git add docs/install/minisign.pub
   git commit -m "release(minisign): commit production public key

   Replaces the Phase 2.G placeholder. Secret key held offline by
   the maintainer; encrypted form + passphrase live in GitHub
   Actions secrets MINISIGN_SECRET_KEY + MINISIGN_PASSWORD.
   "
   git push
   ```

6. **Delete this maintainer-only doc:**
   ```bash
   git rm docs/install/README-MAINTAINER-MINISIGN.md
   git commit -m "release(minisign): remove maintainer setup placeholder"
   git push
   ```

## Key rotation

If the secret key is compromised or rotated:

1. Generate the new keypair on a clean machine.
2. Sign the new pubkey with the old (transition release):
   ```bash
   minisign -Sm new.pub -s ~/.private/skattr-minisign-secret-OLD.key
   ```
3. Publish a `KEYROTATION.md` document explaining the change.
4. Update `docs/install/minisign.pub` and the GH secrets.
5. Bump the version + announce.

Don't proceed with key rotation in Phase 2.G — track as a Phase 5
follow-up.
