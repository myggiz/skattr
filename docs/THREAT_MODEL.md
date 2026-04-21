# Skattr Threat Model v0

> **Status:** Draft at Phase 0 exit. Pre-audit. Will be revised before
> any public release, and again after the Phase 4 third-party audit.

## Scope

This document covers the Skattr desktop client (`crates/cli` /
Phase-2 Tauri UI) and the mailbox server (`crates/mailbox`) as they
exist at the end of Phase 0. It does NOT cover mobile clients
(post-1.0), the bridge/firewall-circumvention layer (inherited from
Tor), or operational security practices the user is expected to
maintain (e.g., backing up their seed phrase).

## Assets

1. **User identity** — the long-term Ed25519 keypair rooted in a BIP39
   seed phrase. Derived key material (HS signing key, storage-seed).
2. **Conversation contents** — all MLS application messages past and
   future, and the local message history in `skattr.sqlite`.
3. **Contact graph** — who the user talks to; what `.onion` addresses
   they've contacted.
4. **Metadata** — timing of sends/receives, mailbox poll cadence,
   online/offline patterns.
5. **Device state** — the running daemon's process memory (keys,
   cleartext DB).

## Adversaries

### A1. Passive network observer (ISP, wifi operator, state-level dragnet)
**Capabilities:** Observe encrypted flows in/out of the user's device.
No access to the device itself. Cannot break Tor.

**Defenses:** Tor hides remote peer addresses and local routing.
All application traffic is wrapped in Noise_XK, which is inside a
Tor circuit, inside TLS. A passive observer sees "this IP is using
Tor" and nothing else about the Skattr protocol specifically.

**Residual exposure:** Fact that the user is running Tor (not Skattr).
Partially mitigated by Tor bridges if the user enables them (Phase 1+).

### A2. Active network attacker (MITM, TCP reset, BGP hijack)
**Capabilities:** All of A1 plus injecting/modifying packets on the
network path.

**Defenses:** Tor onion services provide end-to-end authentication
without trusted third parties — the `.onion` address IS the public
key. Noise_XK on top adds a second layer of mutual auth using our
Ed25519 identity keys. Invite links carry an Ed25519 signature over
the contents, so MITM on the invite-sharing channel cannot substitute
a different identity or `.onion`.

**Residual exposure:** Denial of service (dropped packets, circuit
rotation attacks). Honest user experiences connectivity failures.

### A3. Malicious peer (a contact turns hostile)
**Capabilities:** A contact with whom an MLS group is established
begins trying to extract information beyond what the protocol
surfaces.

**Defenses:** MLS forward secrecy and post-compromise security limit
retrospective and prospective exposure. A removed member cannot
decrypt subsequent messages. Message tombstones are honored by
cooperating clients but not forced (see non-goals below).

**Residual exposure:** Anything the malicious peer saw while in the
group, including historical messages before removal. Screenshots and
forwarding are outside our control.

### A4. Malicious mailbox operator
**Capabilities:** Operates one of the user's registered mailbox
servers. Sees: polling cadence, identity-hash-scoped deposits, rough
message sizes, TTLs.

**Defenses:** Mailbox stores MLS ciphertext only; without the MLS
keys, the contents are random-looking bytes. The mailbox cannot forge
deposits (sender signs). The mailbox CAN withhold or drop messages,
but per-sender MLS generation numbers make withholding detectable
by the recipient.

**Residual exposure:** The fact that identity-hash X polled at time
Y, with message sizes Z. This is load-bearing metadata we don't fully
defend against in Phase 0. Mitigations: self-host, register with
multiple mailboxes, enable cover polling (Phase 4).

### A5. Physical seizure / device compromise (stolen laptop)
**Capabilities:** Physical access to the powered-off device. Full
disk read.

**Defenses:** All at-rest state is encrypted:
- `identity.vault` under a user passphrase (Argon2id → XChaCha20-Poly1305).
- `hs.key.age` under `HKDF(seed, "skattr-hs-storage-v1")`.
- `skattr.sqlite.age` under `HKDF(seed, "skattr-storage-v1")`.

Without both the passphrase AND either the seed phrase or a live
process's memory, the on-disk state is opaque.

**Residual exposure:** If the attacker recovers the user's passphrase
(via phishing, keylogger, or brute force on a weak passphrase),
everything on disk decrypts. Argon2id with `m=64 MiB, t=3, p=4`
raises brute-force cost but does not eliminate it. Users SHOULD use
strong passphrases; the CLI does not enforce strength in Phase 0.

### A6. Running process compromise (attacker on the live machine)
**Capabilities:** Runs code as the same Unix user while the daemon
is running. Can read `/proc/<pid>/mem` on Linux, ptrace the process.

**Defenses:** Minimal at Phase 0. Secret material uses `Zeroize` on
drop to limit the decrypted-in-RAM window, but while the daemon is
alive the keys are necessarily in memory. Plaintext SQLite working
file `skattr.sqlite` exists on disk while the daemon runs.

**Residual exposure:** Everything the live daemon knows. Phase 1+
mitigations: user-selectable auto-lock that re-encrypts on idle,
seccomp/landlock sandboxing, moving the plaintext DB into a
memfd-backed in-memory file.

### A7. Compromised OS / supply chain
**Capabilities:** The operating system, Rust toolchain, or any
transitive dependency has a backdoor or a critical vulnerability.

**Defenses:** Reproducible builds (Phase 4) will let third parties
verify that released binaries match public source. `cargo-deny`
enforces a license allowlist and advisory DB. Pinned dependency
versions in `Cargo.lock` (committed). Code signing on distributed
binaries (Phase 5).

**Residual exposure:** We have ~300 transitive crates across Arti,
OpenMLS, RustCrypto. A supply-chain attack on any of them reaches us.
Audit scope and bug bounty (Phase 4-5) aim to catch issues in the
protocol-critical subset; the long tail is shared with the broader
Rust ecosystem.

## Guarantees (what Skattr promises)

- **Confidentiality.** Message contents are readable only by the
  sender and the intended recipients, past and future, under the
  current epoch's keys.
- **Authenticity.** Messages are provably signed by an identity
  keypair the recipient trusts (because they accepted the invite).
- **Forward secrecy.** Key compromise at time T does not reveal
  pre-T messages.
- **Post-compromise security.** A ratchet advances keys on every
  Commit, so recovery is automatic once a compromise ends.
- **Identity stability.** The BIP39 seed phrase is sufficient to
  fully restore the identity, the `.onion` address, and (with the
  storage-seed also recovered — via `skattr restore-backup`) the
  message history.
- **No central trust.** No server sees plaintext, the contact graph,
  or even who is talking to whom (modulo per-mailbox identity-hash
  polling).

## Non-goals (what Skattr does NOT promise)

- **Metadata against your ISP.** Tor hides a lot but not everything;
  your ISP knows you're using Tor.
- **Mailbox-operator blindness to polling cadence.** A mailbox
  operator learns the rough polling pattern for identity hashes they
  host.
- **Message-delete-for-everyone.** Tombstones are advisory; a
  non-cooperating client can retain deleted content.
- **Screenshot / recording defense.** We can't stop a recipient
  from screenshotting, recording their screen, or describing the
  message to a third party.
- **Endpoint compromise resistance.** If an attacker controls the
  device, the daemon's secrets are reachable.
- **Protection against typosquatting / social-engineering on
  invite delivery.** The signed invite link defends against MITM
  modification, but if the user accepts an invite from an attacker,
  no cryptographic defense helps.
- **Voice or video.** Out of scope for v1.
- **Anonymous routing of mailbox deposits.** Deposits flow over Tor
  (hiding IPs) but the mailbox sees the recipient's identity hash.
  We do not hide WHICH contact you're depositing for.
- **Multi-device identity.** A single identity runs on a single
  device at a time in Phase 0-1. Multi-device is a post-1.0 project.

## Open questions, tracked for Phase 1+

- How do we detect and surface a silent-withhold mailbox? MLS
  generation numbers let the receiver detect gaps, but we need a
  daemon-level alerting mechanism.
- How do we encourage users to self-host a mailbox vs use a community
  operator? The gap is real: community operators see the identity
  graph of their users.
- The `experimental-api` feature on arti-client surfaces upstream
  instability into our dependency graph. How do we defend against
  Arti breaking changes in a minor version bump?
- Passphrase strength: do we enforce a minimum? Do we support
  hardware tokens for the vault unlock?

## Revision history

| Version | Date       | Notes                                       |
|---------|------------|---------------------------------------------|
| v0      | 2026-04-17 | Initial draft, end of Phase 0. Pre-audit.   |
