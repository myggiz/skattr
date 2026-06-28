# Installing Skattr

Skattr is distributed as unsigned bundles whose checksums are
signed with [minisign](https://jedisct1.github.io/minisign/).
Verify both before running the binary on any machine where you
care about provenance.

## Verification flow

Every Skattr release attaches three "supply-chain" files alongside
the platform bundles:

- `SHA256SUMS` — one line per bundle: `<hash>  <filename>`.
- `SHA256SUMS.minisig` — minisign signature over `SHA256SUMS`.
- (the bundles themselves)

The skattr maintainer's minisign public key is committed in this
repository at `docs/install/minisign.pub`. The same key is also
displayed below for offline reference.

### Step 1 — download

From the [Releases page](https://github.com/myggiz/skattr/releases),
pull the bundle for your platform plus `SHA256SUMS` and
`SHA256SUMS.minisig`.

### Step 2 — verify the bundle hash

```bash
sha256sum -c SHA256SUMS --ignore-missing
```

Expected output (one matching line per file you actually
downloaded):

```
skattr_0.0.1_amd64.deb: OK
```

If you see `FAILED`, **do not run the binary**. Re-download or
file an issue.

The committed `minisign.pub` is the real maintainer key (key ID
`EEDBFDA4BF232D38`); signature verification is live for signed releases as of
v0.1.0.

### Step 3 — verify the minisign signature

Point minisign at the public-key file from `docs/install/minisign.pub`
(the `-p` flag takes the key file directly, including its
`untrusted comment:` line):

```bash
minisign -Vm SHA256SUMS -p path/to/minisign.pub
```

(Use lowercase `-p <file>` for the key *file*. Uppercase `-P` takes the
bare key *string* only — `-P "$(cat minisign.pub)"` fails because the
file's comment line is not part of the key.)

Expected: `Signature and comment signature verified`.

If the signature does not verify, the `SHA256SUMS` file was
tampered with after the maintainer signed it. **Do not run the
binary.** Report the discrepancy.

### Step 4 — install + first run

See the per-platform docs:

| Platform | Bundle filename | Guide |
|----------|-----------------|-------|
| Linux (x64) | `skattr_<version>_amd64.deb` or `Skattr_<version>_amd64.AppImage` | [linux.md](linux.md) |
| macOS (ARM64) | `Skattr_<version>_arm64.dmg` | [macos.md](macos.md) |
| Windows (x64) | `Skattr_<version>_x64_en-US.msi` | [windows.md](windows.md) |

## Why minisign and not GPG?

Minisign signatures are 116 bytes; the public key is 56 bytes.
Verification is a single Ed25519 check with no Web-of-Trust /
keyserver coordination. This is enough for "the same person who
controls this GitHub repo signed this release" without the
moving parts of OpenPGP.

GPG support is on the roadmap but not required for v0.1.

## Key rotation

If the maintainer's minisign key is compromised or rotated, the
new public key will be:

1. Committed at `docs/install/minisign.pub` (same path).
2. Signed by the *old* key in a `SHA256SUMS.minisig` of a
   transition release, alongside a `KEYROTATION.md` document
   that explains what changed and when.

Until that document is published, treat the in-repo public key
as the authoritative one.
