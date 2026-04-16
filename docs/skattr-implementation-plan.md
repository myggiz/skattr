# Skattr: Detailed Implementation Plan

Companion to `skattr-design.md`. Breaks each phase into workstreams with concrete tasks, dependencies, and validation criteria.

## How to read this doc

- **Workstreams** within a phase can mostly run in parallel — dependencies between them are flagged explicitly.
- **Tasks** within a workstream are roughly sequential.
- Each phase opens with **decisions to lock** — these need a committed answer before serious coding starts, because changing them later is expensive.
- Time estimates assume 1–2 senior Rust engineers. Multiply for part-time, halve for a bigger team up to the point where coordination overhead bites (usually ~4 devs on a codebase this size).
- Estimates are ranges, not commitments. Crypto projects have unknown-unknowns.

---

## Phase 0 — Foundations (weeks 1–4)

**Goal:** two processes on two machines can open a Tor-backed bidirectional byte stream between them. No protocol yet, just plumbing.

### Decisions to lock before starting

| Decision | Recommendation | Reversibility |
|----------|---------------|---------------|
| License | GPLv3 for clients, AGPLv3 for the mailbox server | Hard to change later |
| Project name | Pick something boring and searchable | Medium |
| Crypto library | `ed25519-dalek` + `x25519-dalek` + `chacha20poly1305` (RustCrypto) | Medium — can swap per-primitive |
| Storage | `rusqlite` with app-level file encryption via `age` | Medium — migrations cover schema, file-level changes are worse |
| Async runtime | `tokio` (Arti requires it anyway) | Hard |
| Error handling | `thiserror` for library errors, `anyhow` for binaries | Easy |
| Logging | `tracing` + `tracing-subscriber` | Easy |
| Seed phrase format | BIP39 (widely understood, existing wallets as user mental model) | Medium |

### Workstream 0.A — Project infrastructure

1. Create repo, add `LICENSE`, `README.md`, `CONTRIBUTING.md`, `SECURITY.md`, `CHANGELOG.md` skeletons
2. Cargo workspace with crates: `core`, `mailbox`, `cli`, `tests` (integration); reserve `ui` for Phase 2
3. `rust-toolchain.toml` pinning the MSRV (use stable, pin for reproducibility)
4. CI on GitHub Actions / Codeberg: `fmt`, `clippy -D warnings`, `test`, matrix across Linux/macOS/Windows
5. `cargo-deny` config: license allowlist, advisory check, source allowlist (no git deps), banned crates
6. `cargo-audit` in CI, fail on high/critical advisories
7. Pre-commit hook config (`rustfmt`, `clippy`, trailing whitespace, commit-msg format)
8. Issue and PR templates (security issue template routes to private disclosure)
9. Decide commit signing policy — require signed commits from maintainers

**Validation:** a no-op PR runs the full CI matrix in under 10 minutes.

### Workstream 0.B — Identity & crypto foundations

*Depends on 0.A project setup.*

1. `IdentityKey` type: Ed25519 private + public, with `generate()`, `public()`, `sign()`, `verify()`
2. Secret wrapping: wrap `IdentityKey` in a type that zeroes on drop (`zeroize::Zeroizing`)
3. At-rest encryption: Argon2id (params: m=64MiB, t=3, p=4 — benchmark on target hardware) → 32-byte key → XChaCha20-Poly1305
4. On-disk identity format v1: `{ version, kdf_params, salt, nonce, ciphertext, mac }` serialized as CBOR
5. Load/save with correct/incorrect passphrase handling (constant-time comparison of derived key material implicit via AEAD)
6. BIP39 seed phrase generation and recovery — derive identity key from seed via HKDF (domain-separated)
7. Property tests: round-trip, tamper detection (any bit flip → auth error), version migration stub
8. Fuzz target on the on-disk format parser
9. Document the key derivation path: `seed → HKDF("skattr-identity-v1") → ed25519 seed → keypair`

**Validation:** `cargo test -p core identity` passes; fuzz target runs for 10 minutes with no findings; manual test of "wipe data, restore from seed, get same public key."

### Workstream 0.C — Arti integration

*Depends on 0.A project setup. Independent of 0.B.*

1. Add `arti-client` and `tor-hsservice` (check latest versions — the API shifts, use the most recent stable release)
2. Arti config: data directory under `~/.local/share/<app>` (XDG) / `Application Support` (mac) / `AppData` (win)
3. Bootstrap function with progress callback (later surfaced in UI)
4. Generate v3 hidden service key, persist it encrypted with identity-derived key
5. Start hidden service, register a connection handler that pushes streams onto an mpsc channel
6. Outbound: resolve `.onion` address, open stream, return `AsyncRead + AsyncWrite`
7. Graceful shutdown: drain connections, flush, stop Arti
8. Expose Tor status (bootstrapped / bootstrapping / error) via a `watch` channel for UI
9. Integration test: spin up two Arti instances in one process, one publishes onion service, other connects, byte echo round-trips

**Validation:** the integration test above runs in CI (may need `tor-testnet` or Arti's built-in chutney equivalent — the real Tor network is too slow for CI).

**Risks:** Arti's HS server API is the youngest part of Arti. If you hit blockers, the fallback is to shell out to system `tor` with a controller socket. Don't architect around this fallback unless you have to.

### Workstream 0.D — Storage layer

*Depends on 0.A, 0.B (for passphrase-derived key).*

1. Pick migration tool: `refinery` is fine, or roll your own `schema_version` table with `include_str!`'d migrations — fewer deps
2. Connection wrapper: open DB with key derived from identity passphrase, run pragmas (`foreign_keys=ON`, `journal_mode=WAL`, `synchronous=NORMAL`)
3. Schema v1 migration (tables are placeholders; most filled in later phases):
   - `identity(id, public_key, created_at)` — single row
   - `contacts(id, identity_pubkey, display_name, added_at)`
   - `onion_addresses(contact_id, address, seen_at, is_current)`
   - `mls_groups(id, group_id, state_blob, epoch)`
   - `messages(id, group_id, sender, kind, body_blob, ts, delivered_at)`
   - `outbox(id, target, payload, attempts, next_retry_at)`
   - `mailboxes(id, onion, registered_at, role)` — "mine" / "theirs"
4. Typed repository layer: `ContactRepo`, `MessageRepo`, etc., each with 5–10 methods
5. Transactions wrapper with explicit rollback on panic
6. Backup export: tar-gz of DB + identity file, encrypted with seed-derived key
7. Schema forward-compatibility tests (write v1, migrate to v2-stub, round-trip)

**Validation:** unit tests per repo; fuzz test on backup import parser.

### Workstream 0.E — Documentation baseline

*Runs in parallel with everything else.*

1. Draft threat model v0: assets, adversaries (passive network, active network, malicious peer, malicious mailbox, endpoint compromise, physical seizure), guarantees, non-goals
2. `ARCHITECTURE.md`: component diagram, crate layout, data flow for "send one message"
3. `PROTOCOL.md`: start as a pointer to the design doc, evolve with implementation
4. `OPERATIONS.md`: how to run the dev stack locally
5. ADR (Architecture Decision Record) directory with first few ADRs: license choice, crypto library, storage approach

**Validation:** a new contributor can read the docs and get a local dev env running in under an hour.

### Phase 0 exit checklist

- [ ] `cargo init`, `cargo test`, `cargo clippy` all clean across OSes
- [ ] `skattr init` generates identity, requires passphrase, produces seed phrase
- [ ] `skattr restore <seed>` rebuilds identity
- [ ] `skattr daemon` starts Tor, publishes onion, prints address
- [ ] Two daemons can echo bytes between each other over Tor
- [ ] Threat model v0 is reviewed and committed
- [ ] All ADRs for Phase 0 decisions committed

---

## Phase 1 — 1:1 messaging, both online (weeks 5–10)

**Goal:** two online users on different networks exchange end-to-end encrypted messages via CLI.

### Decisions to lock

| Decision | Recommendation | Notes |
|----------|---------------|-------|
| Noise pattern | `Noise_XK_25519_ChaChaPoly_BLAKE2s` | Matches Signal-style transport; `snow` crate supports it |
| MLS ciphersuite | `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519` | Widely supported in OpenMLS, matches Noise primitives |
| CBOR library | `ciborium` | Mature, no unsafe code |
| Invite URI scheme | Finalize the exact scheme (e.g. `skattr://`) | Registered later but pin now for stability |
| Max message size | 64 KiB at protocol layer; attachments out-of-band in Phase 3 | |

### Workstream 1.A — Frame codec

1. `Frame` enum with one variant per frame type; `FrameType` as `#[repr(u8)]`
2. Serialize/deserialize: length prefix + type byte + CBOR-encoded body for typed frames; raw bytes for MLS frames
3. `tokio_util::codec::Decoder` + `Encoder` implementation
4. Strict max length enforcement (16 MiB); oversized = error, connection close
5. Version byte in connection preamble (single byte before first frame)
6. Unit tests: partial reads, truncation, oversized, unknown frame type
7. Fuzz target with `cargo-fuzz`; commit seed corpus
8. Property test: `decode(encode(frame)) == frame` over `proptest`-generated frames

**Validation:** fuzz target runs 1 hour clean; property test with 10k cases passes.

### Workstream 1.B — Noise handshake

*Depends on 1.A.*

1. `snow` integration, builder pattern for Noise_XK
2. Initiator side: wraps an outbound `AsyncRead + AsyncWrite`, performs handshake, returns `(Transport, PeerIdentity)`
3. Responder side: symmetric
4. Extract handshake hash; derive `h_transport = HKDF(hh, "skattr-binding-v1", 32 bytes)`
5. Post-handshake: wrap the stream so reads/writes go through the Noise transport cipher
6. Error taxonomy: MalformedHandshake, AuthenticationFailed, UnknownPeer, Replay
7. Integration test over `tokio::io::duplex`
8. Integration test over Tor (depends on 0.C)
9. Timing: reject handshake messages that take longer than N seconds to arrive (slowloris defense)

**Validation:** handshake succeeds with correct keys, fails deterministically with every category of wrong input.

### Workstream 1.C — MLS for 2-member groups

*Depends on 0.D (for state persistence), not on 1.B (parallel).*

1. OpenMLS crypto provider setup; wire it to RustCrypto primitives you already use
2. KeyPackage generation: fresh one for each invite, stored locally until consumed
3. Create group (initiator) with self as only member, then generate Add proposal + Commit + Welcome for the invitee
4. Process Welcome (invitee) → now a 2-member group
5. Send/receive MLS Application messages with the chosen ciphersuite
6. MLS state persistence: implement `OpenMlsKeyStore` on top of your sqlite repo
7. External PSK injection: mix `h_transport` and `invite_psk` into the first Commit
8. Epoch advancement: periodic self-Commit (every 24h or 100 messages, whichever first) for PCS
9. Integration test: full 2-party session, 1000 messages, restart both sides, verify state recovers

**Validation:** round-trip messages across restarts; deliberate corruption of one side's state triggers clean error, not silent failure.

### Workstream 1.D — Invite & contact flow

*Depends on 1.C (needs KeyPackage).*

1. Invite link struct + CBOR serialization + base64url
2. Parser with strict validation (all fields present, signature valid, not expired)
3. Generator: takes your identity, current onion, fresh KeyPackage, fresh PSK, expiry → signed link
4. QR code rendering (`qrcode` crate) → PNG or SVG
5. Single-use enforcement: mark KeyPackage as consumed on first successful use
6. `skattr invite` and `skattr add <link>` commands
7. Contact persisted with: identity pubkey, nickname, current onion, MLS group id
8. Edge case: duplicate add → update onion, keep existing MLS session

**Validation:** generate link on one machine, consume on another, end up with a usable MLS session.

### Workstream 1.E — Delivery semantics

*Depends on 1.A, 1.B, 1.C.*

1. Outbox: persisted queue of outbound messages with `next_retry_at` and attempt count
2. Sender loop: pick pending messages, attempt delivery, update status
3. ACK frame handling: on ACK, mark delivered and remove from outbox
4. Exponential backoff with jitter: 1s → 2s → 4s → ... → 5min cap
5. Receiver dedup: bloom filter or sqlite index over `(sender, message_id)` with 24h sliding window
6. Connection pooling: one connection per online peer, reused across messages
7. Idle timeout: close connections after N minutes of inactivity
8. Online presence: peer is "online" while connection is open and responsive to PING

**Validation:** kill network mid-send, restore, message arrives exactly once; send burst of 100 messages, all ordered.

### Workstream 1.F — CLI

*Depends on everything above.*

1. `clap` derive-based command tree
2. Commands: `init`, `restore`, `daemon`, `invite [--qr]`, `add <link>`, `contacts`, `send <contact> <text>`, `tail [<contact>]`
3. Daemon runs in foreground or via `--detach`; outputs structured logs
4. Interactive mode: `chat <contact>` — read line, send, print incoming in-band
5. IPC between CLI and daemon: Unix domain socket (macOS/Linux), named pipe (Windows)
6. Config file: `~/.config/skattr/config.toml` for defaults (default contact, mailbox preferences later)
7. `--json` output mode for scripting

**Validation:** scripted end-to-end test that drives two CLIs through the full flow.

### Workstream 1.G — Message storage & search

*Depends on 0.D.*

1. `messages` schema fleshed out: `(id, group_id, sender_id, kind, body, ts_sender, ts_received, acked)`
2. FTS5 virtual table on message bodies (text kind only), updated via triggers
3. Query API: recent per-conversation, search across all, unread count
4. History pruning policy (configurable retention, default infinite)
5. Export conversation to JSON / plaintext (user-controlled)

**Validation:** store 100k messages, search p95 latency < 50ms.

### Phase 1 exit checklist

- [ ] Two users on different networks complete the full flow: generate invite, add contact, exchange messages
- [ ] History survives restart on both sides
- [ ] MLS epoch advances observably (verify via debug command)
- [ ] At least one external developer reviews the Noise handshake and MLS integration code
- [ ] All fuzz targets run nightly with no findings for a week
- [ ] Performance sanity: message latency over real Tor under 5s, memory steady-state under 200MB

---

## Phase 2 — Offline delivery + UI (weeks 11–16)

**Goal:** users message each other even when not both online. Non-technical users can install and use the app.

### Decisions to lock

| Decision | Recommendation |
|----------|---------------|
| UI framework | Tauri 2 + SvelteKit (small bundle, good DX) |
| Mailbox polling strategy | Adaptive: poll more when recently active, back off when idle; always have a floor |
| Default mailbox offering | None. User must choose. Offer a list of volunteer-run mailboxes with warnings. |
| Package formats | `.deb`, AppImage, `.dmg`, `.msi` for v1; Flatpak later |
| UI state model | Rust owns all state, UI is a thin view layer |

### Workstream 2.A — Mailbox server

*Shares `core` crate. Can start in parallel with 2.B.*

1. `mailbox` crate structure, binary target
2. Embedded Arti with a pinned v3 onion service
3. SQLite storage: `deposits(id, recipient_hash, ciphertext, deposited_at, expires_at, challenge_state)`
4. `DEPOSIT` handler: size check (e.g. 1 MiB cap), TTL cap (e.g. 30 days), per-circuit rate limit, write deposit
5. `CHALLENGE` handler: generate 32-byte nonce, store with timestamp, return
6. `FETCH` handler: verify Ed25519 signature over nonce, return all deposits for `SHA-256(pubkey)`
7. `DELETE` handler: signature-authenticated delete by deposit id
8. Background task: expire deposits past TTL
9. Operator config: `mailbox.toml` — onion key location, TTL policy, size limits, storage path
10. systemd unit file, Docker image, healthcheck endpoint (unauthenticated, reports "ok" only)
11. Metrics (local, not exposed externally): deposits/hour, storage used, unique recipients
12. Logging policy: never log identity hashes, never log deposit contents, log only aggregate counts

**Validation:** run mailbox against a fuzz client sending all known-bad inputs; 24h soak test under synthetic load.

### Workstream 2.B — Client mailbox integration

*Depends on 2.A protocol being frozen.*

1. Mailbox client module: `register`, `deposit`, `fetch`, `delete`
2. Mailbox registration flow: user provides onion address of chosen mailbox, client registers, persists
3. Multiple mailboxes per user supported; user picks at add-time
4. Polling scheduler: exponential backoff up to max interval (e.g. 5 min) when idle, 10–30s when active
5. Poll all registered mailboxes in parallel
6. Deliver-online-first logic: try direct onion connection first, fall back to depositing at recipient's mailboxes after N seconds
7. ACKing deposits: receiver acks via direct connection or new deposit; sender deletes from mailbox on ack
8. Cover-polling stub (implementation in Phase 4)

**Validation:** contact A goes offline, B sends 10 messages, A comes online, receives all 10 in order; deposits are deleted from the mailbox after ack.

### Workstream 2.C — ContactCard & address rotation

*Depends on 2.B.*

1. `ContactCard` struct: identity pubkey, current onion, list of mailboxes, version, expiry, signature
2. Publish ContactCard via deposits to contacts' mailboxes on change
3. Receive + verify ContactCard updates (version monotonic, signature valid)
4. Onion address rotation command: generate new HS key, publish ContactCard, old onion listens for a grace period (24h default)
5. Mailbox change: same mechanism
6. Contact picker displays current status (online, last-seen per policy)

**Validation:** rotate onion address, contacts pick up the new one within one poll cycle; grace period prevents connection loss.

### Workstream 2.D — UI foundation

*Can start at week 11 in parallel; depends on 2.B/2.C only for last-mile wiring.*

1. Tauri 2 project setup inside workspace
2. IPC layer: typed commands from JS to Rust (one per user action), events from Rust to JS (incoming messages, state changes)
3. First-run wizard:
   1. Welcome screen
   2. Create passphrase (with strength meter, warnings about loss)
   3. Show seed phrase, require user to type it back to confirm
   4. Bootstrap Tor with progress bar
5. Main layout: contact list (left), conversation view (right)
6. Contact add dialog: paste invite or scan QR (webcam on desktop via WebRTC)
7. Invite generate dialog with QR display
8. Conversation view: message bubbles, timestamps, delivery state indicators
9. Text input with paste and basic markdown preview (no HTML rendering anywhere)
10. Settings panel: identity info, mailbox management, notification preferences, export data
11. Accessibility pass: keyboard navigation, screen reader labels, high-contrast mode

**Validation:** usability test with 3 non-technical users; all complete install → add contact → message flow without help.

### Workstream 2.E — Packaging & distribution

*Depends on 2.D producing a Tauri app.*

1. Tauri bundler config for each target
2. Linux: `.deb`, AppImage, Flatpak manifest (Flathub can come later)
3. macOS: `.dmg` (unsigned; Phase 5 adds signing)
4. Windows: `.msi` via WiX
5. CI release workflow on tag: build all, upload to Releases, generate SHA-256 sums
6. Signed checksums via minisign (Phase 5 adds proper code signing)
7. Install and run smoke test per platform in CI

**Validation:** clean VM on each OS can install the package, run the app, complete first-run wizard.

### Phase 2 exit checklist

- [ ] Offline user receives messages next time they come online
- [ ] Address rotation works without breaking conversations
- [ ] UI is usable by non-technical testers in unmoderated sessions
- [ ] Installers work on clean VMs for all 3 OSes
- [ ] Mailbox operator can run a mailbox from the documented setup in under 30 minutes
- [ ] A mailbox-server soak test runs for 72 hours with no leaks or crashes

---

## Phase 3 — Groups and rich messaging (weeks 17–24)

**Goal:** small (≤50 member) groups work end-to-end. Attachments, reactions, replies, edits.

### Decisions to lock

| Decision | Recommendation |
|----------|---------------|
| Max group size for v1 | 50 |
| Attachment storage | Chunked, per-attachment key, uploaded to sender's + recipients' mailboxes; direct when both online |
| Delete semantics | Tombstone honored by cooperating clients; clearly communicated in UI |
| Read receipts | Off by default, opt-in per-conversation |
| Edits | Full history preserved and shown on demand |

### Workstream 3.A — MLS groups (multi-member)

*Core MLS work; no new deps.*

1. Group creation with multiple initial members (bulk Add proposals + Commit)
2. Add member flow: consume their KeyPackage, produce Welcome, distribute Commit to existing members
3. Remove member: Remove proposal + Commit, distribute; removed member cannot decrypt future messages
4. Leave group (self-remove)
5. Join via external commit (for invite-link-to-group flow)
6. Handle out-of-order / stale Commits (store as buffered proposals, apply when possible)
7. Epoch rotation policy: on membership change always; time-based for PCS (e.g. daily if active)
8. Recovery from desynced state: detect, re-fetch, rejoin via external commit if unrecoverable

**Validation:** 20-member group, members join/leave/rejoin, messages across a week, state intact on all sides.

### Workstream 3.B — Group UX

*Depends on 3.A.*

1. Create group dialog: name, initial members picker
2. Group settings: name, icon, member list
3. Add member action: pick contact → sends invite to join group
4. Remove member action: admin-only (first implementation: everyone is admin)
5. Leave group action
6. Group info panel: member online status, joined-at
7. Group icon storage: local only initially; syncing via encrypted app message in Phase 3 stretch or Phase 4

**Validation:** UI tests for each flow; visual regression snapshots.

### Workstream 3.C — Fanout & delivery

*Depends on 3.A, 2.B.*

1. Group message send: produce MLS ciphertext once, deliver to each member independently
2. Parallel delivery to online members (direct onion), queued delivery to offline members (via their mailboxes)
3. Per-recipient state tracking in outbox
4. ACK collection: group message considered "delivered to N of M"
5. UI shows partial delivery states
6. Retry policy per recipient

**Validation:** send in a 20-member group, randomly take members offline, verify all eventually receive in order.

### Workstream 3.D — Attachments

*Depends on 2.B, 3.A.*

1. File picker + drag-drop in UI
2. Chunker: split file into 256 KiB chunks
3. Per-attachment symmetric key; encrypt each chunk with derived subkey
4. Attachment manifest (chunk hashes, size, filename, mime) carried in MLS app message
5. Chunks uploaded to mailboxes (sender's and recipients') or transferred directly
6. Recipient: fetch chunks, verify hashes, reassemble, decrypt, write
7. Progress reporting in UI (send and receive)
8. Resume broken transfers (chunks are idempotent by hash)
9. Thumbnail generation on sender side for images (local-only, not transmitted thumbnails)
10. Max size policy: default 100 MiB, configurable by user

**Validation:** send 100 MiB file, kill connection mid-transfer, resume, verify hash matches original.

### Workstream 3.E — Message kinds

*Depends on 1.G, 3.A.*

1. Envelope `kind` enum expanded: text, file, reaction, reply, edit, delete, typing
2. Reaction: small message `{ target_id, emoji }`, aggregated per target in UI
3. Reply: `reply_to` field renders as quoted preview
4. Edit: new message with `edits` field; UI shows "edited", tap to see history
5. Delete (for-everyone): tombstone with `deletes` field; cooperating clients hide content, show "message deleted"
6. Typing indicator: ephemeral frame type (not stored, not MLS — separate transient channel, or send-and-forget MLS app msg with short TTL)

**Validation:** all kinds round-trip correctly; old clients ignore unknown kinds gracefully (forward compatibility test).

### Workstream 3.F — Notifications

*Depends on 2.D.*

1. `notify-rust` for cross-platform native notifications
2. Content modes: full (sender + content), minimal (sender only), generic ("New message")
3. Per-conversation mute with expiry options (1h, 8h, forever)
4. Unread counts per conversation, aggregated in tray icon
5. Focus-aware: no notification for conversation currently in foreground
6. Notification on delivery failure after N retries (opt-in)

**Validation:** manual test matrix across OSes; focus, minimize, lock screen, DND mode.

### Phase 3 exit checklist

- [ ] 50-member group stress test passes
- [ ] Attachments up to 100 MiB work reliably
- [ ] All message kinds implemented and documented
- [ ] Notifications respect mute, focus, and content-level settings
- [ ] Performance: UI remains responsive during 10k-message conversation scroll
- [ ] All new surfaces have fuzz coverage

---

## Phase 4 — Hardening (weeks 25–32)

**Goal:** ready for small public beta. Externally audited. Known properties and limitations documented.

### Decisions to lock

| Decision | Recommendation |
|----------|---------------|
| Audit firm | Trail of Bits, NCC Group, Cure53, or Radically Open Security — get quotes |
| Audit budget | $40–80k realistic for a protocol + implementation audit |
| Multi-device in v1? | Defer. Ship single-device v1, add in v1.x. |
| Cover traffic | Implement as opt-in; not default (battery/bandwidth cost) |
| Panic wipe | Yes, via duress passphrase |

### Workstream 4.A — Audit preparation & engagement

1. Freeze protocol and major interfaces at start of Phase 4 (emergency fixes only)
2. Write formal protocol specification (grown out of the design doc), publish
3. Internal crypto code review with a checklist (constant-time ops, secret lifetime, error paths don't leak)
4. Produce a "here's how the pieces fit" brief for auditors (2–4 pages)
5. Send RFP to 3 audit firms, pick one, schedule (usually 2–4 month lead time — start early)
6. During audit: triage findings within 48h, fix criticals immediately, plan highs for Phase 4 exit
7. Post-audit: public audit report with findings + remediations
8. Budget contingency for fixes surfacing major redesign — this happens

**Validation:** audit report published with zero outstanding critical or high findings at Phase 5 start.

### Workstream 4.B — Fuzzing

1. Fuzz harnesses for: frame codec, invite link parser, ContactCard parser, mailbox wire protocol, message envelope (CBOR)
2. OpenMLS message fuzz harness (use OpenMLS upstream's if available, otherwise write a wrapper)
3. Seed corpus curated per target (real messages, edge cases)
4. CI nightly run of each fuzzer, 4 hours per target, crashes fail the build
5. Consider OSS-Fuzz integration if the project is accepted
6. ASan + MSan runs in CI on the fuzz targets

**Validation:** all fuzzers run 100 hours cumulatively with no new findings.

### Workstream 4.C — Metadata minimization

1. Audit every place a timestamp is transmitted; coarsen where possible
2. Message size padding: pad to next bucket (256B, 1KiB, 4KiB, 16KiB, 64KiB); remove fine-grained size signal
3. Send jitter: random 0–500ms delay on outbound messages (configurable, default on)
4. Poll jitter: ±25% random jitter on mailbox poll intervals
5. Cover traffic (opt-in): constant-rate dummy messages in conversations; dummies are MLS app messages with a `cover: true` flag, discarded on receive
6. Log audit: grep for all log lines that include identity/onion/mailbox strings, scrub or gate behind debug-only
7. Crash reporting: if added, ensure it's opt-in and strips all PII

**Validation:** adversarial review — can someone watching your mailbox learn your sleep schedule? Work until the answer is "not well."

### Workstream 4.D — Backup, recovery, panic

1. Seed phrase restore: rebuild identity, reconnect to contacts (requires contacts to re-invite — document this clearly)
2. Full encrypted backup export: DB + MLS state + identity + ContactCards, encrypted with seed-derived key
3. Import flow with migration validation
4. Duress mode: secondary passphrase that looks valid, wipes identity key + MLS state + messages on entry
5. Panic button: UI affordance for "wipe now"
6. Auto-lock: timed lock after idle, requires passphrase to unlock
7. Secure delete: overwrite before unlink where filesystem supports it

**Validation:** restore-from-seed recovery test; duress passphrase destroys everything in under 2 seconds on spinning rust.

### Workstream 4.E — Reproducible builds

1. Pin all deps, including transitive, via `Cargo.lock` committed and CI-enforced
2. Use Nix (flakes) or a pinned Docker image for build environment
3. Deterministic build flags: `-C link-arg=-Wl,--build-id=none`, strip metadata, fixed timestamps
4. Publish build instructions so a third party can reproduce and compare hashes
5. Automate hash comparison in CI: third-party builder reproduces each release and submits signatures

**Validation:** two independent reproducers produce byte-identical binaries for a release tag.

### Workstream 4.F — Public documentation

1. Threat model: final, public, linked from app
2. "Who this is for / not for" plain-language page
3. Comparison matrix (Signal, Session, Briar, Cwtch, SimpleX) — honest, including where others are better
4. Known limitations document
5. FAQ with the hard questions (metadata visible to mailbox, identity loss, multi-device)
6. User documentation: first-run, adding contacts, groups, attachments, backup/restore, panic
7. Mailbox operator documentation: install, config, monitoring, decommissioning

**Validation:** external reviewer (security writer, not a developer) reads docs and flags misleading claims — fix all of them.

### Workstream 4.G — Beta program

1. Private beta: 10–30 testers, NDA optional, direct feedback channel
2. Issue triage cadence (daily during beta)
3. Crash collection mechanism (opt-in, local review before submission by user)
4. Expand beta to 100–500 after 2 weeks of stability
5. Decide on public launch criteria (crash-free rate, open critical bug count)

**Validation:** beta phase runs with MTTR on critical bugs under 72 hours.

### Phase 4 exit checklist

- [ ] Audit complete, critical/high findings closed, report ready to publish
- [ ] All fuzz targets clean for 2 weeks
- [ ] Reproducible builds verified by at least one third party
- [ ] Public threat model and limitations documented
- [ ] Beta feedback incorporated, stability bar met
- [ ] Backup/restore/panic tested by QA on all platforms

---

## Phase 5 — Release (weeks 33+)

**Goal:** sustainable public release.

### Workstream 5.A — Signing & notarization

1. Obtain Apple Developer ID, notarization setup in CI
2. Windows EV code signing certificate
3. GPG-signed Linux packages, APT/RPM repo setup
4. Minisign signatures for AppImages and cross-checks
5. CI release workflow with signing keys in secure secrets storage

### Workstream 5.B — Update mechanism

1. Signed update manifest (JSON + minisign signature)
2. Static hosting for manifest and binaries
3. Client: check for updates on a schedule, verify signature, notify user, user-initiated install
4. Rollback support (keep previous binary, restore on startup failure)
5. No auto-install without explicit user consent — privacy app users specifically care

### Workstream 5.C — Web presence & docs

1. Marketing site (static, no trackers)
2. Download page with checksums and signature verification instructions
3. User documentation site
4. Transparency page (funding, team, governance)

### Workstream 5.D — Community & security

1. Issue templates, including private security disclosure workflow
2. Bug bounty: decide self-hosted vs platform (HackerOne, YesWeHack). Budget accordingly.
3. Security.txt on the site
4. Community chat — Matrix room is conventional, or dogfood your own app
5. Mailbox operator directory (signed, community-maintained)

### Workstream 5.E — Launch

1. Soft launch: closed list → broader beta → public
2. Pre-launch: press brief to privacy-focused outlets (The Register, Ars Technica, specialized security press)
3. Post-launch monitoring: crash reports, issue intake, security@ mailbox
4. 30-day post-launch retrospective and roadmap for v1.1

### Post-1.0 roadmap candidates (not scheduled)

- Mobile port (iOS + Android) — large project; revisit with fresh constraints
- Post-quantum ciphersuite migration when MLS PQ variants standardize
- Voice/video calling — very large scope, likely separate product
- Federated discovery / introduction protocol (Briar-style)
- Multi-device

---

## Cross-cutting concerns

Things that apply across all phases and need someone holding the thread.

### Security review cadence

- Every PR touching crypto, protocol, or auth requires a second reviewer
- Weekly internal security review of merged changes during Phases 1–4
- External review checkpoints: end of Phase 1 (handshake), end of Phase 3 (groups), Phase 4 full audit

### Testing pyramid

- **Unit tests** — every module, target 80%+ line coverage on `core`
- **Property tests** — for serialization, state machines, crypto wrappers
- **Fuzz tests** — for all parsers (required before each phase exit from Phase 1 onward)
- **Integration tests** — spin up real Arti + mailbox + two clients, run end-to-end scenarios
- **Adversarial tests** — simulate malicious peer, malicious mailbox, corrupted state

### Observability (careful!)

- Structured logging with redaction by default (never log pubkeys, onions, or message contents at info level)
- Metrics are local-only; no telemetry to any server
- Debug logs gated behind explicit opt-in in settings
- If crash reporting is added, user reviews crash content before submission

### Dependency discipline

- Lockfile committed and CI-enforced
- `cargo-deny` in CI for licenses, advisories, sources
- `cargo-audit` in CI
- New dependencies require justification in PR description
- Periodic review to drop unused deps

### Governance

- Decide early: single maintainer / maintainer group / foundation
- Commit signing required for protected branches
- Release approvals require two maintainer signoffs
- Public roadmap, quarterly review

---

## Risk register (top risks by phase)

| Phase | Risk | Mitigation |
|-------|------|-----------|
| 0 | Arti HS API unstable | Pin version, smoke-test on upgrades, have `tor` CLI fallback documented |
| 1 | MLS integration more complex than estimated | Timebox to 3 weeks, simplify (1:1 only), punt groups to Phase 3 |
| 2 | Tauri 2 breaking changes | Pin Tauri version, upgrade deliberately between phases |
| 2 | Mailbox protocol needs revision after real use | Version the protocol from day one, plan for v2 in Phase 4 |
| 3 | Group fanout performance | Profile early, consider message batching per recipient |
| 4 | Audit findings require redesign | Budget 4 weeks of Phase 4 purely for findings; if worse, slip Phase 5 |
| 4 | Reproducible builds harder than expected | Start in Phase 3, not Phase 4 |
| 5 | Windows EV cert takes months | Start certificate procurement in Phase 3 |

---

## Appendix: working agreements that prevent pain later

- **No crypto decisions get merged without a written rationale.** ADR format is fine.
- **No "TODO security" comments make it to a release branch.** Track as issues with security label.
- **Every phase produces a recorded demo.** 5-minute video of the exit criteria being met. Becomes regression reference.
- **Beta testers sign nothing, but they get a clear statement: "this is pre-release, do not rely on it for high-stakes communication."**
- **The word "anonymous" never appears in product copy without qualification.** "Metadata-minimizing" or "pseudonymous" is honest; "anonymous" is a claim you can't defend in court.
