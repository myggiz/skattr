# Skattr Threat Model (v1.0)

> **Status:** v1.0 release model. Reflects the shipped 1:1, attachment-capable,
> Tor-only client and the mailbox server. A third-party security audit has **not**
> been performed (disclosed below); this document will be revisited after one.
> Disclosure scope follows the
> [v1.0 pull-forward-vs-disclose decision record](superpowers/specs/2026-06-23-v1.0-pull-forward-vs-disclose-decisions.md).

## Scope

This document covers the Skattr desktop client (`crates/cli` /
Tauri UI) and the mailbox server (`crates/mailbox`) as shipped for v1.0.
It does NOT cover mobile clients
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
Partially mitigated by Tor bridges (if supported by your Tor configuration).

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
deposits (sender signs). The mailbox CAN withhold or drop messages: per-sender MLS generation
numbers make a withhold **detectable in principle**, but the v1.0 client does
**not** yet surface a withhold alert — treat this as a latent property, not an
active defense.

**Residual exposure:** The recipient address a mailbox sees is a **stable,
non-rotating** hash of the recipient's public key. It does not rotate across
time, so a mailbox can correlate all of a recipient's polls/deposits over its
whole lifetime, and **two colluding mailboxes can confirm they host the same
recipient**. Combined with polling cadence, message sizes, and TTLs, this is
load-bearing metadata v1.0 does not defend against. Mitigations available to the
user: self-host, and register with multiple mailboxes. (Cover polling / traffic
padding are **not** implemented — see v1.1 limitations.)

### A5. Physical seizure / device compromise (stolen laptop)
**Capabilities:** Physical access to the powered-off device. Full
disk read.

**Defenses:** All at-rest state is encrypted:
- `identity.vault` under a user passphrase (Argon2id → XChaCha20-Poly1305).
- `hs.key.age` under `HKDF(seed, "skattr-hs-storage-v1")`.
- `skattr.sqlite.age` under `HKDF(seed, "skattr-storage-v1")`.
- Received attachment chunks under `<data_dir>/attachments/<hex id>/<index>` are
  stored as AEAD ciphertext (keys live inside the MLS-protected manifest).
  Plaintext is produced only on an explicit *Open* or *Save* command: *Open*
  decrypts into `<data_dir>/cache/open/`, which the app makes a *best-effort*
  attempt to wipe on start and clean shutdown; *Save* decrypts to a user-chosen
  path. The wipe is `remove_dir_all` with failures logged (not fatal): a delete
  failure — or a crash / kill that skips the clean-shutdown path — can leave
  decrypted attachment plaintext in the open-cache until the next successful
  wipe.

Without both the passphrase AND either the seed phrase or a live
process's memory, the on-disk state is opaque.

**Residual exposure:** If the attacker recovers the user's passphrase
(via phishing, keylogger, or brute force on a weak passphrase),
everything on disk decrypts. Argon2id with `m=64 MiB, t=3, p=4`
raises brute-force cost but does not eliminate it. Users SHOULD use
strong passphrases; the CLI does not enforce passphrase strength.

### A6. Running process compromise (attacker on the live machine)
**Capabilities:** Runs code as the same Unix user while the daemon
is running. Can read `/proc/<pid>/mem` on Linux, ptrace the process.

**Defenses:** Secret material uses `Zeroize` on drop to limit the
decrypted-in-RAM window, but while the daemon is alive the keys are
necessarily in memory. Plaintext SQLite working file `skattr.sqlite`
exists on disk while the daemon runs.

**Residual exposure:** Everything the live daemon knows. The database is
encrypted on clean shutdown, but while the daemon is running a
plaintext SQLite working file `skattr.sqlite` exists on disk. Future hardening
(not in v1.0): user-selectable auto-lock that re-encrypts on idle,
seccomp/landlock sandboxing, moving the plaintext DB into a
memfd-backed in-memory file.

### A7. Compromised OS / supply chain
**Capabilities:** The operating system, Rust toolchain, or any
transitive dependency has a backdoor or a critical vulnerability.

**Defenses:** Reproducible builds (planned, not in v1.0) will let third parties
verify that released binaries match public source. `cargo-deny`
enforces a license allowlist and advisory DB. Pinned dependency
versions in `Cargo.lock` (committed). Code signing on distributed
binaries (planned, not in v1.0).

**Residual exposure:** We have ~300 transitive crates across Arti,
OpenMLS, RustCrypto. A supply-chain attack on any of them reaches us.
A future audit and bug bounty (planned, not in v1.0) aim to catch issues
in the protocol-critical subset; the long tail is shared with the broader
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
- **Identity stability.** The BIP39 seed phrase fully restores the
  Ed25519 identity keypair, and — because the message database is encrypted
  under a seed-derived key — your message history too, **as long as the
  encrypted database file is intact** (the same seed re-derives its key).
  The `.onion` address, however, is **not** seed-derived: a clean restore
  from seed alone generates a **new** HS key → a **new** onion address. To
  preserve the onion address (and to recover history on a clean machine or
  after the database file is lost), use the encrypted backup
  (`skattr restore-backup`, which carries `hs.key.age` and the seed-encrypted DB).
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
  device at a time. Multi-device is a post-1.0 project.

## v1.0 known limitations (deferred to a later release)

These are absent in v1.0 by decision (see the
[disclosure decision record](superpowers/specs/2026-06-23-v1.0-pull-forward-vs-disclose-decisions.md)):

- **First-contact requires both peers online.** The first-contact Welcome is
  delivered directly (no mailbox fallback); if the inviter is offline when the
  joiner sends the Welcome, first contact stalls until both are online. Ordinary
  messages and ContactCard updates *do* have mailbox fallback. (D1)
- **Onion-address rotation is degenerate.** `Command::RotateOnion` bumps the
  self-card version and republishes the *current* onion; it does not generate a
  new address. True rotation is future work. (D2)
- **Offline attachments are best-effort.** Deposited chunks are held by a mailbox
  for ~7 days and dropped if never fetched within the window; files larger than
  10 MiB transfer only while both peers are online. Text messages are not subject
  to these limits. (D3)
- **Received attachment chunks are retained indefinitely.** Completed ciphertext
  chunks accumulate under `<data_dir>/attachments/`; there is no automatic GC or
  per-file expiry. Disk usage grows with received files. A delete/retention policy
  is a v1.1 candidate.
- **No metadata-minimization.** No message-size padding, send-timing jitter, or
  cover traffic / cover polling.
- **The recipient-hash mailbox-correlation leak** (A4) is unmitigated.
- **No multi-member groups (> 2).**
- **Reactions / edit / delete-for-everyone / typing / read receipts** are inert
  placeholders.
- **No multi-device.**
- **No third-party security audit** has been performed for v1.0.

## Open questions (v1.1+)

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
| v1.0    | 2026-06-25 | v1.0 release model: withhold-detection downgraded, identity-hash correlation disclosed, D1/D2/D3 + v1.1 list added. |
