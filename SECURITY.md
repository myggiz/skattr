# Security policy

## Reporting a vulnerability

**Do not open a public issue for security problems.** Email `security@myggiz.net`, ideally encrypted to the project PGP key below. Expect acknowledgement within 72 hours and a status update within 7 days.

### PGP key

> **⚠️ v1.0 status:** The PGP key below and the minisign public key
> (`docs/install/minisign.pub`) are **placeholders**. Encrypted vulnerability
> reports and download-signature verification are **not usable until the real
> keys are published** (a maintainer action tracked for the v0.1.0 tag, separate
> from this documentation pass). Until then, the verification *procedure* below
> is correct but will not validate against the committed placeholder key.

```
-----BEGIN PGP PUBLIC KEY BLOCK-----
(placeholder — replaced before first public release)
-----END PGP PUBLIC KEY BLOCK-----
```

Fingerprint: `TBD`

> **Placeholder, not yet real.** Both this PGP key and the release-signing
> minisign key (`docs/install/minisign.pub`, marked "PLACEHOLDER — REPLACE
> BEFORE TAGGING v0.1.0") are placeholders. Generating the real keypairs and
> wiring the GitHub Actions secrets (`MINISIGN_SECRET_KEY` /
> `MINISIGN_PASSWORD`) is a hard prerequisite of the v0.1.0 release and must be
> done before any public `v0.1.0` tag. Until then, downloads cannot be
> cryptographically verified.

## Scope

In scope:

- The `core`, `mailbox`, `cli`, and `ui` crates in this repository.
- The wire protocol as described in [`docs/skattr-design.md`](docs/skattr-design.md) — frame framing, Noise_XK handshake, the MLS binding (`h_transport`, ADR 0009), the invite-link format (ADR 0008), and the frozen mailbox protocol (ADR 0006).
- The local IPC surface between the CLI/UI and the daemon, and its peer authentication: **Unix** (`AF_UNIX` socket at `<data_dir>/ipc.sock`, `SO_PEERCRED` uid match) and **Windows** (Tokio Named Pipes with a per-daemon random pipe name `\\.\pipe\skattr-<24-hex>` discovered via `<data_dir>\ipc.endpoint`, an owner-SID DACL on the pipe, and a post-accept SID equality check). Both reject a non-matching local peer before dispatch.
- Signed release binaries published by Myggiz AB (see *Release artifacts* below).

Out of scope (report to upstream):

- Vulnerabilities in Arti, OpenMLS, `snow`, `ed25519-dalek`, or other third-party dependencies. We will fast-track coordinated updates once upstream has a fix.
- Tor network-level attacks (covered by the Tor Project threat model).
- Attacks that require physical access to an unlocked device running Skattr.

## Release artifacts and verification

Releases are built by `.github/workflows/release.yml` on `v*` tags across `ubuntu-latest`, `macos-latest`, and `windows-latest`, each smoke-tested (`skattr-ui --smoke-test`) after install:

- **Linux:** `.deb` and AppImage (signed release artifacts). A Flatpak manifest (`packaging/flatpak/`) is provided for **build-from-source** sandboxing; it is not a published signed binary.
- **macOS:** `.dmg` (Apple Silicon / ARM64 only; x86_64 is deferred).
- **Windows:** `.msi` (Tauri WiX bundle).
- **Verification chain:** a `SHA256SUMS` manifest over all artifacts, signed with **minisign** (`SHA256SUMS.minisig`). Verify the signature against the maintainer's public key (`docs/install/minisign.pub`) before trusting any hash, then verify your download's hash against `SHA256SUMS`. **The committed minisign key is a placeholder today** (see the note above) — verification is not yet meaningful until the v0.1.0 release ships the real key. The Tauri auto-updater is explicitly disabled; updates are manual, verified downloads.

## What we ask

- Give us reasonable time to patch before public disclosure. "Reasonable" is usually 90 days; we will move faster for simple fixes and negotiate for complex ones.
- Do not exfiltrate data beyond what is needed to demonstrate the vulnerability.
- Do not run disruptive tests against third-party mailbox operators.

## What you can expect

- Acknowledgement of your report within 72 hours.
- Credit in the release notes for the fix, if you want it.
- A public write-up of the issue after a fix ships, with a timeline of events.
- No bounty program yet — this is planned for a later phase.

## Known limitations

These are documented in the design, the threat model, and the v1.0 readiness audit. Reports about them will be closed with a pointer here. They fall into three groups.

### By design (permanent)

- A mailbox operator learns which identity hashes are polling them (metadata leak — mitigated by self-hosting and operator choice).
- Message "delete for everyone" is advisory — recipient clients cooperate, but we cannot force them to.
- Losing your seed phrase means losing the identity. We explicitly chose not to have a key-recovery server.
- Skattr does not protect against endpoint compromise. If your device is compromised, the attacker can read your messages.
- While the daemon is running, a plaintext SQLite database (`skattr.sqlite` + `-wal`/`-shm`) necessarily exists on disk. Phase 2.B made a clean shutdown checkpoint, re-encrypt, and remove it (and re-encrypt crash residue on the next boot), but at-rest encryption protects data **between** runs, not during operation.

### Out of v1.0 scope (must be disclosed; do not over-trust)

- **No metadata-minimization defenses yet:** no message-size padding, no send-timing jitter, no cover traffic / cover polling. Traffic-analysis resistance beyond what Tor provides is future work.
- **No third-party security audit yet.**
- **No multi-member groups (>2):** v1.0 is strictly 1:1.
- **Real onion-key rotation is not implemented (Task 23.5).** `Command::RotateOnion` bumps the ContactCard version and republishes the *current* onion address; the address itself does not change, so it is not a true rotation. Contacts see a new `ContactCardReceived` version but route to the same onion.
- **First-contact `Welcome` is direct-only.** There is no mailbox fallback for the first-contact `Welcome` frame (old Task 2.E.5); if the inviter is offline when the joiner sends the Welcome, first contact stalls until the inviter is reachable. Ordinary messages and ContactCard updates *do* fall back to a mailbox.
- Reactions, message edit, typing indicators, and read receipts are inert placeholders.

### Resolved by the Phase 2 security workstream

These were open deferrals at the time of the audit and have since been fixed — listed so older notes/issues referencing them can be closed:

- **Task 20.5** — the per-peer sustained-direct-failure trigger into the mailbox fallback is now wired (Phase 2.C); offline delivery happens automatically.
- **Task 22.5** — `RemoveMailbox` now drains held deposits into local storage before finalizing removal (Phase 2.C); removing a mailbox no longer destroys queued offline messages.
- **At-rest encryption (T1-2)** — the `age`-encrypt-on-shutdown path is now actually reached and WAL-safe (Phase 2.B); previously plaintext persisted after every shutdown.
