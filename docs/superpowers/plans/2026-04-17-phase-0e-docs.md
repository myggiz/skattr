# Phase 0.E — Documentation Baseline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close out Phase 0 with the documentation baseline — a committed threat model v0, an `OPERATIONS.md` that gets a new contributor to a running daemon in under an hour, a refreshed `ARCHITECTURE.md` reflecting the Phase 0.B/C/D state (including the "send one message" data-flow trace), an updated `README.md`, and a `CLAUDE.md` Phase-0-complete note.

**Architecture:** Docs only. No Rust code changes. Each task produces one or two Markdown files, cross-references the existing design/protocol/implementation-plan docs under `docs/`, and commits. Verification is "does it read well, cross-link correctly, and build under `mdBook` / `cargo doc` if we tried?" — no test suite to run.

**Tech Stack:** GitHub-flavored Markdown. No new deps.

**Exit criteria:**
- `docs/THREAT_MODEL.md` exists with an assets-adversaries-guarantees-non-goals structure.
- `docs/OPERATIONS.md` exists and includes: build, test, format, clippy, fuzz, daemon, backup flows; plus recovery from wedged tempfile / state_dir.
- `ARCHITECTURE.md` at the repo root has an updated "Inside `crates/core`" map and a "data flow: sending one message" section reflecting Phase 0.B/C/D.
- `README.md` reflects the post-Phase-0 state (status, what works, how to try it).
- `CLAUDE.md` Repository state paragraph declares Phase 0 complete (all five workstreams).
- All existing Markdown still lints clean (no broken relative links).

---

## File structure

```
docs/
├── THREAT_MODEL.md         CREATE: v0 threat model
├── OPERATIONS.md           CREATE: dev-stack guide
├── PROTOCOL.md             UNCHANGED (pointer to design doc)
├── skattr-design.md        UNCHANGED (reference)
└── adr/                    UNCHANGED (0001-0005 committed earlier)

ARCHITECTURE.md             MODIFY: refresh "Inside core" + add "send one message" data flow
README.md                   MODIFY: post-Phase-0 status + quickstart tweaks
CLAUDE.md                   MODIFY: Phase 0 complete, 0.E done
CHANGELOG.md                MODIFY: Phase 0.E bullet under [Unreleased]
```

No code files touched.

---

## Pre-flight

```bash
cd /home/myggiz/development/skattr
. "$HOME/.cargo/env"

# No-code verification: make sure master still builds clean before we
# fork a docs branch — it's cheap and catches any in-flight breakage.
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings

git worktree add ../skattr-phase-0e-docs -b phase-0e-docs
cd ../skattr-phase-0e-docs
```

All gates green. Subsequent tasks assume `/home/myggiz/development/skattr-phase-0e-docs`.

---

## Task 1: THREAT_MODEL.md v0

**Goal:** A document a privacy-curious user or external reviewer can read in 20 minutes and understand what Skattr protects against and what it doesn't.

**Files:** Create `docs/THREAT_MODEL.md`.

- [ ] **Step 1: Draft the threat model**

Create `docs/THREAT_MODEL.md` with the following content:

```markdown
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
```

- [ ] **Step 2: Verify**

```bash
# Markdown sanity: cross-links work and no dangling anchors.
# If markdownlint is not installed, skip; the doc is human-readable.
command -v markdownlint >/dev/null && markdownlint docs/THREAT_MODEL.md || echo "markdownlint not installed, skipping"
```

No hard failure. Eyeball the doc for coherence.

- [ ] **Step 3: Commit**

```bash
git add docs/THREAT_MODEL.md
git commit -m "docs: THREAT_MODEL.md v0 (Phase 0 exit)

Enumerate assets, seven adversary classes (A1-A7), guarantees,
and non-goals. Pre-audit draft; will be revised before any
public release and again after the Phase 4 third-party audit.

Closes the threat-model-v0 item from Phase 0.E workstream."
```

---

## Task 2: OPERATIONS.md

**Goal:** A new contributor with Rust and git installed can go from zero to running two daemons that echo bytes over Tor in under an hour.

**Files:** Create `docs/OPERATIONS.md`.

- [ ] **Step 1: Draft the operations guide**

Create `docs/OPERATIONS.md`:

```markdown
# Skattr Operations

> Target audience: contributors + operators running the dev stack.
> End-users should read the README instead.

## Prerequisites

- Rust stable (pinned in `rust-toolchain.toml`). Install via
  [rustup](https://rustup.rs). If `cargo` is not on your PATH, source
  `~/.cargo/env`.
- A C compiler and `pkg-config` — required transitively by Arti's
  dependencies on some Linux distributions.
- `git`.
- For the real-Tor integration test: internet access on the first run
  (~30 s consensus download per daemon), Tor directory authorities
  reachable on port 9030/443.
- For the fuzz harness: nightly Rust (`rustup toolchain install
  nightly --profile minimal`) and `cargo-fuzz` (`cargo install
  cargo-fuzz`).

## One-time setup

```bash
git clone https://github.com/myggiz/skattr
cd skattr
cargo build --workspace
```

First build is slow (~10 min on a fast laptop; Arti pulls ~100
crates). Subsequent incremental builds are fast.

## Running the test suite

```bash
# Fast unit + integration tests. Runs in ~30 s.
cargo test --workspace --release

# Slow tests that hit the Tor network (two daemons echo bytes).
# Requires real Tor connectivity. Takes 3-10 min.
cargo test -p skattr-tests --release -- --ignored

# Format check + clippy (what CI runs).
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Running the daemon locally

```bash
# Step 1: generate an identity + a passphrase-locked vault.
# This writes identity.vault under ~/.local/share/skattr/.
# Record the 24-word seed phrase printed on stdout — it's your
# only recovery path.
cargo run -p skattr-cli -- init

# Step 2: start the daemon.
cargo run -p skattr-cli -- daemon
```

The first daemon start bootstraps Arti (~30-90 s on a fresh
`state_dir`) and publishes a v3 onion service. You'll see:

```
Bootstrapping Tor…
Tor ready. Publishing onion service…

Listening on: abcdef...onion:1
Ctrl-C to shut down.
```

The `.onion` address is derived from your identity seed, so restoring
from the BIP39 mnemonic reproduces the same address (see
[recovery](#recovery)).

### Running two daemons on the same machine

For testing, use `--data-dir` to point each daemon at a different
directory:

```bash
TMP1=$(mktemp -d)
TMP2=$(mktemp -d)
cargo run -p skattr-cli -- --data-dir "$TMP1" init  # daemon A
cargo run -p skattr-cli -- --data-dir "$TMP2" init  # daemon B
cargo run -p skattr-cli -- --data-dir "$TMP1" daemon  # in one terminal
cargo run -p skattr-cli -- --data-dir "$TMP2" daemon  # in another
```

Arti requires `$TMP1` and `$TMP2` to be mode `0700` — if you created
them with a permissive umask, run `chmod 700 $TMP1 $TMP2` first.

## Backup and recovery

### Backup

```bash
cargo run -p skattr-cli -- backup /path/to/backup.age
```

You'll be prompted for the vault passphrase. The archive is a
tar.gz of:

- `identity.vault` (passphrase-encrypted)
- `hs.key.age` (seed-encrypted)
- `skattr.sqlite.age` (seed-encrypted)

with an outer age layer keyed by `HKDF(seed, "skattr-backup-v1")`.
The archive is safe to store on untrusted media; the inner layers
mean even the BIP39 seed alone is not enough to open it without the
vault passphrase.

### Recovery from the BIP39 seed alone (identity only)

```bash
cargo run -p skattr-cli -- restore "word1 word2 ... word24"
```

This rebuilds `identity.vault` under a fresh passphrase you choose.
The seed phrase is sufficient to recover the identity AND the
`.onion` address AND the HS signing key. It does NOT recover the
message history — you'll have a fresh SQLite database.

### Recovery from a backup archive (full state)

```bash
cargo run -p skattr-cli -- restore-backup "word1 word2 ... word24" /path/to/backup.age
```

This extracts all three inner files into your `--data-dir`.
Subsequent `skattr daemon` picks up the restored identity, the same
`.onion`, and the same message history.

## Known operational issues

### Daemon killed ungracefully → plaintext `skattr.sqlite` on disk

If the daemon process dies without a clean `Ctrl-C` (e.g., `SIGKILL`,
crash), the plaintext working file `<data_dir>/skattr.sqlite` remains
on disk. This is by design: next startup re-opens it directly and
continues. Drawback: at-rest encryption is effectively disabled
until the next clean shutdown. Phase 1 will add a sync-on-checkpoint
path.

Manual recovery: if you want to re-encrypt immediately, the simplest
route is to run `skattr daemon` once more and let it exit cleanly.

### Arti bootstrap fails with "filesystem permissions"

Arti 0.41 refuses to open a `state_dir` that's group- or
world-readable. If you see:

```
arti client: tor: problem with filesystem permissions
```

Fix it with:

```bash
chmod 700 /path/to/data_dir
```

`skattr daemon` will auto-create its subdirectories with the right
mode going forward.

### Forgot the vault passphrase

Without the passphrase there is no way to decrypt `identity.vault`.
Use `skattr restore <24-word seed phrase>` to rebuild the vault
under a new passphrase. You keep the identity and the `.onion`
address; you lose only the passphrase itself.

### Forgot the seed phrase AND the vault passphrase

The identity is unrecoverable. By design. There is no key-recovery
server.

## Reaching dev infrastructure

- **Issue tracker:** GitHub issues on this repo (once public).
  Security issues route privately per `SECURITY.md`.
- **Fuzz corpus:** local under `crates/core/fuzz/corpus/` when you
  run `cargo +nightly fuzz run`.
- **ADRs:** `docs/adr/` — append a new numbered file for any
  protocol-layer, crypto, or storage decision.

## Phase-0 completion checklist

If you're reading this at the end of Phase 0, the following should
all be true. Run through them as a sanity sweep after any significant
change to the transport or storage layers:

- [ ] `cargo test --workspace --release` passes with 77+ tests.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean.
- [ ] `cargo fmt --all --check` clean.
- [ ] `cd crates/core && cargo +nightly fuzz build vault_parser` succeeds.
- [ ] `cargo test -p skattr-tests --release -- --ignored` passes on a
  network-connected machine (3-10 min).
- [ ] `skattr --help` prints all subcommands: init, restore, daemon,
  invite, add, contacts, send, tail, backup, restore-backup.
- [ ] `skattr init` → record phrase → `skattr daemon` → see `.onion` →
  Ctrl-C shuts down cleanly.
- [ ] `skattr backup` and `skattr restore-backup` round-trip against a
  clean data_dir.
```

- [ ] **Step 2: Commit**

```bash
git add docs/OPERATIONS.md
git commit -m "docs: OPERATIONS.md — dev-stack guide

How to build, test, run the daemon, back up, restore. Known
operational issues (ungraceful shutdown leaving plaintext DB,
Arti's 0700 state_dir requirement, forgotten passphrases).
Closes the OPERATIONS.md item from Phase 0.E workstream."
```

---

## Task 3: ARCHITECTURE.md refresh

**Goal:** The scaffold-era `ARCHITECTURE.md` has the right bones (crate layout, "public API boundary" note, per-phase roadmap) but predates the real Phase 0.B/C/D implementations. Refresh it: update the "Inside `crates/core`" map, expand the "send one message" data flow, and update the phase table to mark 0.B/C/D done.

**Files:** Modify `ARCHITECTURE.md`.

- [ ] **Step 1: Read the current file**

```bash
cat ARCHITECTURE.md
```

The structure is:
1. Workspace layout
2. Crate dependency graph
3. Inside `crates/core` (module map)
4. Public API boundary
5. Data flow: sending one message (sketchy placeholder)
6. Cross-cutting: transport↔MLS binding
7. State that survives restart
8. Where work lands by phase

Preserve sections 1-4 mostly as-is. Rewrite section 5. Update section 7 (new files from Phase 0.C/D). Update section 8 to mark 0.B/C/D complete.

- [ ] **Step 2: Rewrite the "data flow: sending one message" section**

Find the existing `## Data flow: sending one message` section and replace it with:

```markdown
## Data flow: sending one message

End-to-end trace of `Command::SendMessage { contact, kind: Text { ... } }`
once Phase 1 wires the message-send path in:

```
1. UI/CLI submits Command::SendMessage via the Daemon's public API.
2. Daemon looks up the contact's MLS group state (storage::groups::MlsGroupRepo).
3. mls::group wraps the Envelope as an MLS application message (AEAD).
4. storage::outbox::OutboxRepo enqueues the ciphertext with next_retry_at = now.
5. delivery::sender picks up the due entry:
     a. Peer currently online? → transport::connection::send (MLS_APP over
        Noise_XK over arti_client::DataStream, dialled via TorRuntime::connect).
     b. Peer unreachable? → mailbox::client deposits to each of the contact's
        registered mailboxes in parallel.
6. storage::messages::MessageRepo.insert records a local copy for history.
7. On ACK (direct path) or successful DEPOSIT (mailbox path):
     - outbox entry removed
     - messages.delivered_at updated
     - Daemon emits Event::DeliveryStatusChanged to subscribers.
```

The inverse (receiving):

```
1. transport::listener accepts an onion connection; Noise_XK auths the peer.
2. An MLS_APP frame arrives. mls::group decrypts → Envelope.
3. storage::seen_messages dedupes on (sender, message_id).
4. storage::messages::MessageRepo.insert persists the envelope.
5. Daemon emits Event::MessageReceived.
6. Receiver sends an ACK frame back.
```

**Current phase state.** Everything above "Phase 1 wires" is stubbed;
Phase 0 delivered the building blocks. Concretely:

- `Daemon::run` exists as the daemon entry point but does not yet
  accept `Command::SendMessage` — it just bootstraps Tor, publishes,
  and blocks on Ctrl-C.
- `mls::*` is all `todo!()` — Phase 1 work.
- `delivery::sender` is `todo!()` — Phase 1 work.
- `transport::{connection, noise, frame}` are `todo!()` — Phase 1 work.
- `storage::*` is fully implemented and unit-tested; repos are just
  not called yet.
- `transport::{tor, listener, hs_key}` are fully implemented.
```

- [ ] **Step 3: Update the "Inside `crates/core`" module map**

Find the `## Inside `crates/core`` section. The existing box-diagram is
roughly right; update the annotations to reflect Phase 0.B/C/D state.
Specifically:

Replace any line that says "transport — framed Noise_XK over Tor streams"
with the more accurate current state:

```
transport     tor + HS key + accept loop implemented; Noise/frame/connection
              stubbed for Phase 1
```

Replace any line on `storage` with:

```
storage       Pool (age-encrypted), migrations runner, 7 repos, backup — all
              done Phase 0.D
```

Replace any line on `identity` with:

```
identity      Ed25519 keypair, BIP39, Argon2id + XChaCha20-Poly1305 vault,
              HKDF derivations — all done Phase 0.B
```

Mark the rest (mls, delivery, envelope, invite, contact, mailbox, daemon)
as "Phase 1".

- [ ] **Step 4: Update the "State that survives restart" section**

Find the bullet list that enumerates on-disk files. Ensure it reads:

```
- `~/.local/share/skattr/identity.vault` — passphrase-encrypted
  Ed25519 identity seed (Argon2id + XChaCha20-Poly1305).
- `~/.local/share/skattr/hs.key.age` — age-encrypted v3 HS signing
  key. Key: `HKDF(storage_seed, "skattr-hs-storage-v1")`.
- `~/.local/share/skattr/skattr.sqlite.age` — age-encrypted SQLite
  database (contacts, messages, groups, outbox, mailboxes,
  seen_messages, schema_version). Key: `HKDF(storage_seed,
  "skattr-storage-v1")`. While the daemon is running, a plaintext
  `skattr.sqlite` sidecar exists; it's re-encrypted and removed on
  clean shutdown.
- `~/.local/share/skattr/arti/` — Arti's state directory
  (circuits, guards, HS keystore). Mode 0700.
```

Remove any stale reference to a separate `storage-seed` file (the
Phase 0.C cleanup removed it; the storage seed is now HKDF-derived
from the identity secret at daemon start).

- [ ] **Step 5: Update the "Where work lands by phase" table**

Find the table and rewrite it:

```markdown
| Phase | Modules that change | Exit criterion |
|-------|--------------------|----------------|
| 0.A ✅ | Workspace scaffold | Scaffold compiles, clippy clean, tests empty |
| 0.B ✅ | identity | skattr init/restore work, vault round-trips |
| 0.C ✅ | transport::{tor, hs_key, listener} | Two daemons echo bytes over Tor (integration test) |
| 0.D ✅ | storage, daemon::backup | Pool + 7 repos + backup/restore-backup CLI |
| 0.E ✅ | docs | THREAT_MODEL.md, OPERATIONS.md, ARCHITECTURE.md refresh, README update |
| 1 | mls, delivery, transport::{noise, frame, connection}, daemon::state session manager | Two CLI users exchange E2EE messages over real Tor |
| 2 | mailbox (client + server), contact::{card, rotation}, ui | Offline user receives queued messages; Tauri UI shell |
| 3 | mls (multi-member), delivery::fanout, envelope::kinds (attachments, reactions, edits) | 50-member group passes week-long soak |
| 4 | hardening — fuzz harnesses, padding, duress mode, reproducible builds | External audit clean |
| 5 | distribution — signing, updates, docs | Public release |
```

- [ ] **Step 6: Commit**

```bash
git add ARCHITECTURE.md
git commit -m "docs: ARCHITECTURE.md refresh (Phase 0 complete)

- Updated 'Inside crates/core' module map with Phase 0.B/C/D
  implementation state annotations.
- Rewrote 'Data flow: sending one message' with the concrete
  module sequence + a 'current phase state' note flagging the
  Phase 1 stubs.
- Updated on-disk-state section (hs.key.age, skattr.sqlite.age,
  identity.vault, arti/; removed stale storage-seed reference).
- Phase table marks 0.A-0.E complete."
```

---

## Task 4: README.md refresh

**Goal:** The scaffold-era README says "Status: Phase 0 (foundations). The project scaffold is in place; most functionality is stubbed (`todo!()`)." That's no longer true. Update the status section and the build/quickstart paragraphs.

**Files:** Modify `README.md`.

- [ ] **Step 1: Read the current README**

```bash
cat README.md
```

Expected sections: project name, what Skattr is, what it isn't, status, build-from-source, layout, security, license.

- [ ] **Step 2: Update the Status paragraph**

Find the "Status:" line (currently `**Status: Phase 0 (foundations).**`
or similar) and its explanation paragraph. Replace with:

```markdown
**Status: Phase 0 complete.** Identity, at-rest encryption, Arti
integration, and storage all land. `skattr daemon` bootstraps Tor,
publishes a v3 onion service, and accepts inbound streams. Phase 1
(MLS message exchange, outbox delivery, invite links) is next. See
[ARCHITECTURE.md](ARCHITECTURE.md) and [`docs/`](docs/) for the full
design.
```

- [ ] **Step 3: Update the quickstart**

Find the "Building from source" or "Quickstart" section. Ensure it
includes (add what's missing; keep what's already correct):

```markdown
## Quickstart (desktop, Linux/macOS)

Requirements: Rust stable (see `rust-toolchain.toml`), a C toolchain,
and internet access on first run for Arti's consensus download.

```bash
git clone https://github.com/myggiz/skattr
cd skattr
cargo build --workspace --release

# Generate an identity. Record the 24-word seed phrase on screen.
cargo run -p skattr-cli --release -- init

# Start the daemon. Prints the .onion address once Tor is ready.
# Ctrl-C to stop.
cargo run -p skattr-cli --release -- daemon

# Back up everything (identity + HS key + DB) to a single file.
cargo run -p skattr-cli --release -- backup ~/skattr-backup.age

# Full recovery from seed phrase + backup on a clean machine:
cargo run -p skattr-cli --release -- restore-backup "word1 ... word24" ~/skattr-backup.age
```

See [`docs/OPERATIONS.md`](docs/OPERATIONS.md) for the full
developer guide.
```

- [ ] **Step 4: Update the "What's included" / layout section if present**

The scaffold-era README may claim things like "most methods are
stubbed" — remove any such claim. The crate layout list is still
accurate (`crates/core`, `crates/mailbox`, `crates/cli`,
`crates/tests`); keep it.

- [ ] **Step 5: Add a "Current capabilities" / "What works" snippet**

Near the top, after "what Skattr is/isn't", insert:

```markdown
## What works now (end of Phase 0)

- Create and restore a BIP39-backed identity (`skattr init` /
  `skattr restore`).
- Encrypted at-rest storage for identity, HS key, message database.
- Bootstrap Tor via embedded Arti, publish a v3 onion service with
  a seed-derived address.
- Byte-level inbound accept loop (`OnionListener`).
- Backup / restore of the full state as a portable archive.

## What doesn't work yet

- Sending actual messages (Phase 1 — MLS + delivery layer).
- Invite links, contact management beyond storage plumbing
  (Phase 1).
- Offline delivery via mailbox server (Phase 2).
- Desktop UI (Phase 2 — Tauri).
- Group chat (Phase 3).
```

- [ ] **Step 6: Commit**

```bash
git add README.md
git commit -m "docs: README refresh — Phase 0 complete, quickstart

Update status from 'scaffolding, mostly stubbed' to 'Phase 0
complete'. Add 'What works / What doesn't' sections so a visitor
can see exactly where we are. Rewrite quickstart to include
init/daemon/backup/restore-backup with --release flags. Point at
OPERATIONS.md for the full developer guide."
```

---

## Task 5: CLAUDE.md Phase-0-complete note

**Goal:** The Repository-state paragraph in `CLAUDE.md` currently ends with "Only remaining Phase 0 workstream is 0.E". After this plan, that's no longer true.

**Files:** Modify `CLAUDE.md`.

- [ ] **Step 1: Read the current paragraph**

```bash
grep -A2 "^## Repository state" CLAUDE.md | head -5
```

- [ ] **Step 2: Update**

Open `CLAUDE.md` in your editor. Find the Repository-state paragraph.
Replace the current opening (whatever it says now) with:

```markdown
**Phase 0 is complete** — all five workstreams (0.A scaffold, 0.B
identity & crypto, 0.C Arti integration, 0.D storage layer, 0.E
documentation baseline) have shipped and are merged to master.

`crates/core/src/identity/` is fully implemented (Ed25519, BIP39,
Argon2id + XChaCha20-Poly1305 vault, HKDF). `crates/core/src/transport/{tor,
hs_key, listener}.rs` wire `arti-client` 0.41 + `tor-hsservice` 0.41
end-to-end. `crates/core/src/storage/` has a real `rusqlite` + `age`
`Pool`, a migrations runner, seven typed repos, transactions wrapper,
and portable backup export/import. `docs/` has a v0 threat model,
an operations guide, and refreshed ARCHITECTURE.md with a
"send one message" data-flow trace.

The daemon is driven by `Daemon::run(data_dir, &Zeroizing<String>,
ready_tx, shutdown_fut)` — the CLI is a thin wrapper. `transport`
and `storage` are both `pub(crate)`; integration tests reach
internals via `skattr_core::test_exports` gated on the `test-harness`
feature. All four crates compile, `cargo clippy -D warnings` / `cargo
test` / `cargo fmt --check` are green — 77+ unit + integration tests
passing. Phase 0 exit criterion (two daemons echo bytes over Tor)
is exercised by `crates/tests/src/arti_echo.rs`, `#[ignore]`-gated
(run with `cargo test -p skattr-tests --release -- --ignored`).

Phase 1 is next: MLS message exchange, outbox delivery, invite links,
and the session manager wiring that ties transport + mls + storage
together. The bootstrap prompt remains authoritative for file
layout, module boundaries, type signatures, and visibility rules —
match it exactly.
```

Remove the "Remaining Phase 0 workstreams..." sentence. Replace with
the "Phase 1 is next" framing above.

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: CLAUDE.md — Phase 0 complete

Update Repository state paragraph to declare all five Phase 0
workstreams done. Replaces the stale 'Only remaining Phase 0
workstream is 0.E' line with a Phase 1 forward pointer.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Post-plan wrap-up

- [ ] **Step 1: Full gate run**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --release
```

All three must pass. No code changed in this plan, so any regression
would be a fluke from the earlier branches — stop and investigate if
so.

- [ ] **Step 2: CHANGELOG bullet**

Append under `[Unreleased]`:

```markdown
- **Phase 0.E Documentation baseline:** `docs/THREAT_MODEL.md` v0
  (7 adversary classes, guarantees, non-goals). `docs/OPERATIONS.md`
  dev-stack guide (build, test, daemon, backup/restore flows,
  known operational issues). `ARCHITECTURE.md` refreshed with
  Phase 0.B/C/D implementation state and a concrete "send one
  message" data-flow trace. `README.md` updated to "Phase 0
  complete" with What-works / What-doesn't sections and a quickstart
  that covers init/daemon/backup/restore-backup.
- **Phase 0 complete.** All five workstreams shipped (0.A scaffold,
  0.B identity, 0.C Arti, 0.D storage, 0.E docs). Phase 1 (MLS +
  delivery) up next.
```

Commit:

```bash
git add CHANGELOG.md
git commit -m "changelog: Phase 0.E docs + Phase 0 complete marker"
```

---

## Notes for the executing engineer

- **No TDD here.** Each task is a single doc edit — write, read back,
  commit. Verification is "does it read well, do the cross-links
  work, does `cargo build` still pass." No unit tests to write.
- **Cross-link discipline.** Every relative link (`[X](../y.md)`) must
  point at a file that exists in the worktree. `grep -r '\](\./' docs
  ARCHITECTURE.md README.md CLAUDE.md` finds all relative links; eyeball
  them before committing.
- **Tone.** Honest, technical, no marketing copy. The project is
  pre-release infrastructure software; readers are contributors,
  researchers, or privacy-curious technical users. Don't hedge
  ("should be secure, probably"); state what's true and what isn't.
- **Threat model tone.** Be specific about guarantees and non-goals.
  Better to admit a gap explicitly than to gloss it — the Phase 4
  audit will find anything we're hand-waving, and it's cheaper to
  surface concerns now.
- **CLAUDE.md has been edited often;** make sure the Repository-state
  section you're updating is the first `##` section after the top-
  level doc comment, not something inside a later section with a
  similar name.
- **Markdown linters are optional.** If you run `markdownlint`, treat
  its output as advisory — we're not committing to a specific
  lint profile. Consistency with existing docs matters more than
  conforming to any lint's default ruleset.
