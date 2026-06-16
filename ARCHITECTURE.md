# Architecture

A high-level tour of the codebase. Pair this with [`docs/skattr-design.md`](docs/skattr-design.md) (protocol semantics) and [`docs/skattr-deep-dives.md`](docs/skattr-deep-dives.md) (module-level detail).

> **Phasing note.** This repo went through a v1.0 readiness audit (2026-06-12,
> `docs/V1.0-READINESS-AUDIT.md`) that re-phased remaining work. The original
> build (scaffold → identity → transport → storage → MLS → delivery → daemon →
> UI → packaging → Windows) is complete. The audit found "green tests, dead
> production path" — the messenger flows passed only because tests hand-wired
> the transport via `test_exports`; nothing wired it into `Daemon::run`. The
> current phasing is **1 (make messaging work) → 2 (critical security &
> data-integrity) → 3 (attachments) → 4 (release/docs/signing)**. Phases 1 and
> 2 are complete; see the phase table at the bottom and `CLAUDE.md`.

## Workspace layout

```
skattr/
├── crates/
│   ├── core/        # library: protocol, transport, storage, state
│   ├── mailbox/     # AGPLv3 server binary — offline delivery
│   ├── cli/         # clap-based CLI over the daemon
│   ├── ui/          # Tauri 2 + SvelteKit desktop app (GPLv3)
│   └── tests/       # cross-crate integration tests
└── docs/            # design + ADRs + protocol spec + superpowers specs/plans
```

## Crate dependency graph

```
                  ┌───────────────┐   ┌───────────────┐
                  │  crates/cli   │   │  crates/ui    │
                  └───────┬───────┘   └───────┬───────┘
                          │                   │
  ┌──────────────┐        └────────┬──────────┘
  │ crates/tests │─────────────────▼─────────────
  └──────────────┘          ┌───────────────┐
                            │  crates/core  │
                            └───────┬───────┘
                                    │
                            ┌───────▼────────┐
                            │ crates/mailbox │  (shares core::mailbox::protocol)
                            └────────────────┘
```

- `core` depends on nothing in this workspace.
- `mailbox` depends on `core` **only** for shared wire-protocol types (`core::mailbox::protocol`). It does not pull in identity, MLS, delivery, or daemon code.
- `cli` and `ui` depend on `core` to instantiate and drive a `Daemon` (the `ui` crate boots an in-process `Daemon::run` and talks to it over the same IPC socket the CLI uses).
- `tests` depends on `core` and spawns daemons in-process for end-to-end tests, including the live `run_with_transport` loopback guardrails the audit mandates.

## Inside `crates/core`

Modules, top-down (✅ all wired into the production `Daemon::run` path):

```
daemon              long-lived process. `daemon::state::run_with_transport<T>` owns the
                    Tor runtime, storage Pool, DeliveryHub, inbound accept loop, mailbox
                    PollScheduler, mailbox-outbox sweeper, and IPC server; tears down with
                    a deterministic pool-close. `Daemon::run` is a thin ArtiTransport wrapper.
  ├── ipc           length-prefixed CBOR IPC, per-platform: `unix` (AF_UNIX + SO_PEERCRED
  │                 uid check) and `windows` (Tokio Named Pipes + owner-SID DACL +
  │                 post-accept SID equality check). `ENDPOINT_FILENAME` is `ipc.sock`
  │                 (Unix) / `ipc.endpoint` (Windows, carries the random pipe name).
  ├── accept        inbound onion accept loop: `handshake_responder` per stream →
  │                 resolve `peer_x25519 → ContactCard` → `DeliveryHub::ingest`, or the
  │                 first-contact `Welcome` carve-out (ADR 0007). Bounded by a Semaphore
  │                 (concurrent handshakes) + JoinSet drain on shutdown.
  ├── dispatch      `execute_command`: send/recent, contacts (add/rename/remove/mute),
  │                 invites, mailboxes (add/remove/list, RotateOnion), config, history
  │                 (search/export/prune), passphrase change, WipeAllData.
  ├── inbound       `DaemonInbound` (`InboundDispatch`): MLS decrypt + persist + emit
  │                 `Event::MessageReceived`; `dispatch_mailbox` for mailbox-fetched
  │                 ciphertext; `dispatch_welcome_bootstrap` for first contact.
  ├── retention     hourly history sweep driven by `[history] retention_days`.
  ├── logs          in-memory ring buffer + redacting tracing layer + IPC `TailLogs`.
  ├── backup        portable encrypted backup export/import (offline CLI).
  └── {state, handle, commands, events, config, clock, smoke, hex, error_kind}
  ├── delivery      `DeliveryHub` per-peer actor hub; `dial::OutboundDial` on-demand
  │                 dialer; `outbox` + `backoff` exp-retry; `receiver` dedup + ts-window
  │                 + ACK; `MailboxFallbackShared` + `run_mailbox_fallback`; the
  │                 `mailbox_sweeper` re-deposit engine; `kill_stream` (test-harness).
  ├── mailbox       client of the mailbox server: `protocol` (frozen wire, ADR 0006),
  │                 `codec`, `auth` (challenge/sign), `client`, `poll` (`PollScheduler`,
  │                 Idle/Active/Unreachable per-mailbox with jitter).
  ├── contact       contacts + signed ContactCards (`card`, `rotation`, `self_card`),
  │                 monotonic-version persistence.
  ├── invite        signed `skattr://invite/v1#` links (embed inviter ContactCard,
  │                 ADR 0008), `qr` rendering.
  ├── mls           2-member groups, `state_machine`, `provider` keystore, `key_package`,
  │                 `ciphersuite`; two-PSK genesis (invite PSK + `h_transport` PSK).
  ├── envelope      CBOR application payloads (`message`, `kinds`) flowing inside MLS.
  ├── transport     `Transport` trait + `arti_transport::ArtiTransport` +
  │                 `loopback::LoopbackTransport`; `tor` + `hs_key` + `listener`; `frame`
  │                 codec; `noise` Noise_XK + `connection::AuthenticatedConnection`.
  ├── identity      Ed25519 keypair, BIP39, Argon2id + XChaCha20-Poly1305 vault, HKDF.
  └── storage       `Pool` (age-encrypted; `Mutex<Option<Connection>>`, WAL-safe
                    `close(&self)` + `Drop` backstop + sentinel/re-encrypt-on-boot),
                    `migrations` (runner, through 0014), typed repos (`contacts`,
                    `messages` + FTS5, `groups`, `outbox`, `mailboxes`, `key_packages`,
                    `outstanding_invites`, `seen_messages`, `read_state`,
                    `passphrase_audit`), `backup`.
```

**Public API boundary.** Only `daemon`, `error`, `envelope`, `invite`, `contact`, and the key types in `identity` are `pub`. Everything else (`transport`, `mls`, `mailbox`, `delivery`, `storage`) is `pub(crate)`. Consumers (`cli`, `ui`) talk to the daemon through `Command` / `CommandResult` / `Event` enums over the IPC socket. Integration tests reach internals via `skattr_core::test_exports` under the `test-harness` feature — but every audit-phase behavior is *also* proven through a live `run_with_transport` guardrail, not `test_exports`.

## "skattr send <contact> <text>" end-to-end trace (online, direct)

1. **CLI process** parses argv via `clap`; resolves the IPC endpoint (`ipc.sock` on Unix / `ipc.endpoint` on Windows under the data dir).
2. CLI connects; the daemon's IPC server authenticates the peer (Unix `SO_PEERCRED` uid match; Windows owner-SID DACL + post-accept SID equality), rejecting mismatches with a typed `IpcError`.
3. CLI resolves the positional contact via `Command::ListContacts` → prefix match (hex pubkey or nickname).
4. CLI sends `Command::SendMessage { contact, kind: Kind::Text { body } }` as a length-prefixed CBOR frame.
5. Daemon's `dispatch::send_message`, holding the per-group ratchet lock (`GroupLockRegistry`, T1-3): reads `contacts.group_id`, loads the MLS `Group`, builds an `Envelope`, `Group::encrypt` (ratchet advances), and persists the advanced ratchet + sender-side `messages` row + `outbox` row in **one `pool.transaction`** (`save_in_tx` / `insert_in_tx`), then hands off to `DeliveryHub::send`.
6. The `PeerConnection` actor uses its live `AuthenticatedConnection<S>` or dials the peer's onion via `OutboundDial` (Noise_XK initiator), then sends a `Frame::MlsApp` frame.
7. The remote accept loop resolves the known peer and ingests into its `DeliveryHub`; `delivery::receiver::receive_in_tx` enforces the ±1h `ts` window + `(sender, envelope_id)` dedup, captures `mls_generation` + `ts_daemon_recv`, persists the row (FTS5 triggers index `body_text`), and `DaemonInbound` broadcasts `Event::MessageReceived { contact, record }`.
8. The remote `PeerConnection` replies `Frame::Ack`; the sender's oneshot resolves and `CommandResult::MessageSent { status: Delivered }` returns to the CLI.

## "peer offline" end-to-end trace (mailbox fallback, Phase 2.C)

1–6 as above, but the direct dial/send to the offline peer fails.
7. The `PeerConnection` actor's **sustained-failure timer** (Task 20.5) is armed on the first failure and cleared on any success. After `direct_timeout_secs` of unbroken failure it calls `delivery::hub::run_mailbox_fallback`, which picks one of the peer's advertised mailboxes (deterministically by `BLAKE2s(message_id)`), flips the outbox row to `target_kind='mailbox'`, and attempts a deposit.
8. The dedicated `delivery::mailbox_sweeper` task re-deposits any due mailbox-kind outbox rows with per-mailbox failover + capped backoff (the retry engine), deleting a row once a deposit succeeds.
9. The recipient's `mailbox::poll::PollScheduler` fetches the deposit on its next poll; `DaemonInbound::dispatch_mailbox` trial-decrypts against each known group, attributes the sender, and persists + emits — exempt from the ±1h `ts` window (a store-and-forward deposit is legitimately old; replay resistance is dedup + MLS generation + server delete). The mailbox server deletes the deposit only after a successful dispatch (`poll_dispatch_once`).

## "first contact" end-to-end trace (invite → add → Welcome, Phase 1C)

1. Inviter runs `skattr invite`: mints a single-use MLS `KeyPackage`, builds an `skattr://invite/v1#` link whose body **embeds the inviter's signed `ContactCard`** (so the consumer learns the onion without a prior exchange — ADR 0008).
2. Consumer runs `skattr add <link>`: verifies the card, **dials the inviter first** (capturing `h_transport`), then in one `pool.transaction` consumes the KeyPackage (single-use re-check), creates the MLS group via an `add_member` genesis commit carrying *two* external PSKs — the invite PSK and the `h_transport` PSK (ADR 0009) — persists contact + card + group, and sends the `Welcome` over the live connection.
3. The inviter's accept loop sees a stream from an **unknown** peer and takes the first-contact `Welcome` carve-out (ADR 0007): it reads exactly one frame under a bounded timeout, and `dispatch_welcome_bootstrap` registers the same `h_transport` PSK (derived from the identical Noise transcript) before `Group::join_from_welcome`. The binding must validate or the join is rejected.
4. On success the inviter ACKs the Welcome and ingests the now-known peer; both sides are `Active` and can exchange messages bidirectionally.

> First-contact `Welcome` is **direct-only** today — there is no mailbox
> fallback for the Welcome frame (deferred; ordinary messages do fall back).

## Cross-cutting: transport↔MLS binding (ADR 0009)

The Noise handshake hash is mixed into the genesis MLS Commit as an external PSK:

```
h_transport = HKDF(noise_handshake_hash, "skattr-binding-v1")
```

As of Phase 2.A this binding is **active and mandatory** on the genesis commit (dial-first two-PSK construction). It prevents an attacker who somehow obtained MLS state from replaying it over a different Noise session. Any refactor touching either layer must preserve this invariant — it is the reason the two layers are not modeled as independent.

## State that survives restart

- `~/.local/share/skattr/identity.vault` — passphrase-encrypted Ed25519 identity seed (Argon2id + XChaCha20-Poly1305).
- `~/.local/share/skattr/hs.key.age` — age-encrypted v3 HS signing key. Key: `HKDF(storage_seed, "skattr-hs-storage-v1")`.
- `~/.local/share/skattr/skattr.sqlite.age` — age-encrypted SQLite database (contacts, messages, groups, outbox, mailboxes, key_packages, outstanding_invites, seen_messages, read_state, passphrase_audit, schema_version). Key: `HKDF(storage_seed, "skattr-storage-v1")`. While the daemon runs, a plaintext `skattr.sqlite` (+ `-wal`/`-shm`) sidecar exists and a `skattr.sqlite.open` sentinel marks it live; a clean shutdown checkpoints the WAL, re-encrypts, and removes all plaintext + the sentinel (Phase 2.B). A crash leaves the plaintext; the next `Pool::open` detects the residue and re-encrypts on boot.
- `~/.local/share/skattr/arti/` — Arti's state directory (circuits, guards, HS keystore). Mode 0700.

The design deliberately uses two separate keypairs (identity vs. onion service — see design §1.1). Losing the onion key means changing address; losing the identity key means losing the identity.

## Where work lands by phase

The **original build** (old phases 0–2.H) is complete: scaffold, identity/crypto, Arti transport, storage, MLS, invite/contact, delivery, daemon IPC + CLI, FTS5 message storage + retention, hardening, mailbox server, mailbox client, Tauri/SvelteKit UI, packaging, and the Windows port. The table below is the **audit-driven phasing** (2026-06-12 onward); each phase is proven through the real `Daemon::run` assembly, not `test_exports`.

| Phase | Modules that change | Exit criterion |
|-------|--------------------|----------------|
| **1 — make messaging work (T0)** ✅ | daemon::{state, accept, dispatch, inbound}, delivery::dial, transport::{loopback, Transport}, invite (embed card), mls (Welcome carve-out) | Two daemons exchange messages both directions through `run_with_transport` over loopback (`two_daemons_exchange_messages_both_directions_over_loopback`); first contact works (`first_contact_invite_add_then_bidirectional_over_loopback`) |
| **2.A — MLS ratchet & binding integrity** ✅ | mls::group, daemon::{dispatch, inbound}, delivery::dial (ADR 0009) | `h_transport` binding active+mandatory; per-group ratchet race closed; inbound Commit merges; per-invite PSKs unique; invite single-use atomic |
| **2.C — offline delivery: fallback + drain** ✅ | delivery::{hub, peer, mailbox_sweeper, outbox}, daemon::{dispatch, inbound}, storage::outbox | Offline peer receives via mailbox fallback (`offline_peer_receives_via_mailbox_fallback`); RemoveMailbox preserves held messages; no ts-replay poison |
| **2.D — resource hardening (anti-flood)** ✅ | mailbox crate (store/policy/server), daemon::accept | Mailbox survives flood + victim-fill (bounded disk, no lockout); idle/connection caps; bounded Delete; daemon accept-loop concurrency bounded (`mailbox_flood`) |
| **2.B — at-rest encryption lifecycle** ✅ | storage::pool, daemon::state, storage::backup | No plaintext DB/sidecars/sentinel after clean shutdown; re-encrypt crash residue on boot; `export_backup` works (`clean_shutdown_leaves_only_encrypted_db`) |
| **3 — attachments** ⬜ | envelope::kinds, delivery (chunked transfer), mailbox path | File send/receive/preview with metadata stripping |
| **4 — release integrity, docs, signing** ⬜ | docs, release CI, signing keys | Honest docs; real minisign + PGP keys; working download-verification chain |

**v1.1+ deferrals** (disclosed as absent in the v1.0 threat model): third-party audit; metadata-minimization (size padding, timing jitter, cover traffic); multi-member groups (>2); real onion-key rotation (Task 23.5); reactions/edit/delete-for-everyone/typing/read-receipts; multi-device.
