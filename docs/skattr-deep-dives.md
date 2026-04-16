# Skattr: Deep Dives

Companion to `skattr-design.md` and `skattr-implementation-plan.md`. Concrete depth on four areas that are easy to get wrong:

1. `core` crate module structure
2. MLS group state machine
3. Mailbox wire protocol, end-to-end
4. First-run and seed phrase UX

---

## Part 1 — `core` crate module structure

The `core` crate is the library used by the CLI, the UI (via Tauri), and the integration tests. It should expose a clean, async, typed API and keep every platform-specific detail out. Phase 0 work lands in this crate; subsequent phases extend it.

### 1.1 Directory layout

```
crates/core/
├── Cargo.toml
├── build.rs                        # bind openmls crypto provider at compile time
├── src/
│   ├── lib.rs                      # re-exports, feature flags, crate docs
│   ├── error.rs                    # CoreError, Result alias
│   ├── prelude.rs                  # convenience re-exports for consumers
│   │
│   ├── identity/                   # who you are
│   │   ├── mod.rs
│   │   ├── key.rs                  # IdentityKey, PublicKey, Signature
│   │   ├── seed.rs                 # BIP39 encode/decode, identity derivation
│   │   ├── vault.rs                # passphrase-encrypted on-disk container
│   │   └── derive.rs               # domain-separated HKDF helpers
│   │
│   ├── transport/                  # how bytes move
│   │   ├── mod.rs
│   │   ├── tor.rs                  # Arti lifecycle, onion service publish/connect
│   │   ├── noise.rs                # Noise_XK initiator/responder
│   │   ├── frame.rs                # Frame enum + tokio codec
│   │   ├── connection.rs           # AuthenticatedConnection: Noise + Frame over Tor stream
│   │   └── listener.rs             # accepts onion connections, hands to session mgr
│   │
│   ├── mls/                        # group state
│   │   ├── mod.rs
│   │   ├── ciphersuite.rs          # the one chosen ciphersuite, constants
│   │   ├── keystore.rs             # OpenMlsKeyStore impl on top of storage::
│   │   ├── group.rs                # Group: thin wrapper over openmls::MlsGroup
│   │   ├── state_machine.rs        # explicit state type + transitions (see Part 2)
│   │   ├── welcome.rs              # processing Welcomes
│   │   └── commit.rs               # building and processing Commits
│   │
│   ├── envelope/                   # application payloads inside MLS
│   │   ├── mod.rs
│   │   ├── message.rs              # Envelope struct, CBOR serde
│   │   └── kinds.rs                # Text, File, Reply, Edit, Reaction, Delete, Typing
│   │
│   ├── invite/                     # out-of-band contact exchange
│   │   ├── mod.rs
│   │   ├── link.rs                 # parse, generate, sign, verify
│   │   └── qr.rs                   # feature-gated QR render
│   │
│   ├── contact/                    # who you know
│   │   ├── mod.rs
│   │   ├── contact.rs              # Contact struct
│   │   ├── card.rs                 # ContactCard (signed, versioned)
│   │   └── rotation.rs             # onion/mailbox rotation protocol
│   │
│   ├── mailbox/                    # offline delivery (client side)
│   │   ├── mod.rs
│   │   ├── protocol.rs             # wire types shared with mailbox server (see Part 3)
│   │   ├── client.rs               # MailboxClient: register, deposit, fetch, delete
│   │   └── scheduler.rs            # polling strategy, backoff, fanout
│   │
│   ├── delivery/                   # getting messages to their destination
│   │   ├── mod.rs
│   │   ├── outbox.rs               # persisted queue + retry
│   │   ├── sender.rs               # drives outbox, chooses direct vs mailbox path
│   │   └── receiver.rs             # dedup, ordering, ack generation
│   │
│   ├── storage/                    # sqlite persistence
│   │   ├── mod.rs
│   │   ├── pool.rs                 # connection pool, pragmas
│   │   ├── migrations/             # versioned .sql files, loaded at startup
│   │   │   ├── 0001_init.sql
│   │   │   └── ...
│   │   ├── contacts.rs             # ContactRepo
│   │   ├── messages.rs             # MessageRepo + FTS5
│   │   ├── groups.rs               # MlsGroupRepo
│   │   ├── outbox.rs               # OutboxRepo
│   │   └── mailboxes.rs            # MailboxRepo
│   │
│   └── daemon/                     # orchestration
│       ├── mod.rs
│       ├── config.rs               # Config struct + loader
│       ├── state.rs                # Daemon: owns all the long-lived handles
│       ├── events.rs               # Event enum emitted to UI/CLI consumers
│       └── commands.rs             # Command enum consumed from UI/CLI
│
└── tests/                          # integration tests (separate binaries)
    ├── handshake.rs
    ├── first_message.rs
    ├── offline_delivery.rs
    └── group_membership.rs
```

### 1.2 Module responsibilities and key types

**`identity`** — cryptographic identity and its at-rest protection.

```rust
pub struct IdentityKey { /* Ed25519 signing key, zeroized on drop */ }
pub struct PublicKey([u8; 32]);           // Ed25519 public
pub struct Signature([u8; 64]);

impl IdentityKey {
    pub fn generate() -> Self;
    pub fn from_seed(seed: &Seed) -> Self;
    pub fn public(&self) -> PublicKey;
    pub fn sign(&self, msg: &[u8]) -> Signature;
}

pub struct Seed([u8; 32]);
impl Seed {
    pub fn generate() -> Self;
    pub fn to_mnemonic(&self) -> Mnemonic;      // BIP39, 24 words
    pub fn from_mnemonic(m: &Mnemonic) -> Result<Self, SeedError>;
}

pub struct Vault { /* opaque */ }
impl Vault {
    pub fn create(path: &Path, passphrase: &str, identity: &IdentityKey) -> Result<Self>;
    pub fn open(path: &Path, passphrase: &str) -> Result<IdentityKey>;
    pub fn change_passphrase(path: &Path, old: &str, new: &str) -> Result<()>;
}
```

**`transport`** — everything below MLS. Tor lifecycle, Noise_XK, frame codec, composed `AuthenticatedConnection`.

```rust
pub struct TorRuntime { /* Arti handle */ }
impl TorRuntime {
    pub async fn bootstrap(config: &TorConfig) -> Result<Self>;
    pub async fn publish_onion(&self, key: &HsKey) -> Result<OnionListener>;
    pub async fn connect(&self, addr: &OnionAddress) -> Result<TorStream>;
    pub fn status_stream(&self) -> watch::Receiver<TorStatus>;
}

pub struct AuthenticatedConnection {
    pub peer: PublicKey,
    pub handshake_hash: [u8; 32],   // h_transport, bound into MLS later
    inner: /* Noise transport wrapping TorStream + frame codec */,
}

impl AuthenticatedConnection {
    pub async fn initiate(tor: &TorRuntime, me: &IdentityKey, to: &OnionAddress,
                          expected_peer: &PublicKey, psk: Option<&[u8; 32]>)
        -> Result<Self>;
    pub async fn accept(stream: TorStream, me: &IdentityKey,
                        psk_lookup: impl Fn(&PublicKey) -> Option<[u8; 32]>)
        -> Result<Self>;
    pub async fn send(&mut self, frame: Frame) -> Result<()>;
    pub async fn recv(&mut self) -> Result<Frame>;
}
```

**`mls`** — the trickiest module. Wraps OpenMLS with an explicit state machine (Part 2), a custom keystore that persists to sqlite, and a `Group` type that's easier to reason about than raw OpenMLS.

```rust
pub struct Group {
    id: GroupId,
    state: GroupState,      // see Part 2
    mls: openmls::MlsGroup,
    // plus buffered out-of-order messages, epoch metadata, etc.
}

impl Group {
    pub fn create_one_to_one(me: &IdentityKey, their_kp: &KeyPackage,
                             psk: Option<&[u8; 32]>) -> Result<(Self, Welcome)>;
    pub fn join_from_welcome(me: &IdentityKey, welcome: Welcome,
                             psk: Option<&[u8; 32]>) -> Result<Self>;
    pub fn encrypt(&mut self, payload: &[u8]) -> Result<MlsCiphertext>;
    pub fn process_incoming(&mut self, msg: &MlsCiphertext) -> Result<ProcessedMessage>;
    // ... add_member, remove_member, commit, persist
}
```

**`envelope`** — what goes inside MLS application messages. Pure data types with CBOR serde.

```rust
pub struct Envelope {
    pub v: u8,
    pub id: MessageId,             // 16 random bytes
    pub ts: u64,                   // unix ms
    pub reply_to: Option<MessageId>,
    pub kind: Kind,
}

pub enum Kind {
    Text(String),
    File(FileRef),
    Reaction { target: MessageId, emoji: String },
    Edit { edits: MessageId, body: Box<Kind> },
    Delete { deletes: MessageId },
    Typing,                        // not persisted
}
```

**`invite`** — self-contained, no I/O.

```rust
pub struct InviteLink {
    pub identity: PublicKey,
    pub onion: OnionAddress,
    pub key_package: KeyPackage,
    pub psk: [u8; 32],
    pub expires_at: u64,
}

impl InviteLink {
    pub fn sign(&self, id: &IdentityKey) -> SignedInvite;
    pub fn to_url(signed: &SignedInvite) -> String;
    pub fn from_url(url: &str) -> Result<SignedInvite>;
}
```

**`contact`** — Contact is the in-memory representation; ContactCard is what's published over the wire to update contacts when things change.

```rust
pub struct Contact {
    pub identity: PublicKey,
    pub display_name: Option<String>,
    pub current_onion: OnionAddress,
    pub mailboxes: Vec<MailboxRef>,
    pub group_id: GroupId,         // the 1:1 MLS group with this contact
}

pub struct ContactCard {
    pub identity: PublicKey,
    pub onion: OnionAddress,
    pub mailboxes: Vec<MailboxRef>,
    pub version: u64,              // monotonic
    pub issued_at: u64,
    pub signature: Signature,      // by identity
}
```

**`mailbox`** — client half of the mailbox protocol. Server half lives in a sibling `mailbox-server` crate that shares `mailbox::protocol` types with this module.

**`delivery`** — the "send until acked" logic. Decides per-message whether to try direct onion delivery, fall back to mailboxes, or batch.

**`storage`** — repos expose typed methods, not raw SQL. Migrations are `.sql` files loaded by a tiny custom runner that tracks a `schema_version` table.

**`daemon`** — the single long-lived object that owns the Tor runtime, storage pool, active connections, outbox worker, mailbox scheduler. Exposes a `Command`/`Event` API rather than letting callers touch internals directly.

```rust
pub struct Daemon { /* owns everything */ }

impl Daemon {
    pub async fn start(config: Config, passphrase: &str) -> Result<Self>;
    pub async fn execute(&self, cmd: Command) -> Result<CommandResult>;
    pub fn events(&self) -> broadcast::Receiver<Event>;
    pub async fn shutdown(self) -> Result<()>;
}

pub enum Command {
    CreateInvite { nickname: Option<String> },
    AddContact { invite_url: String },
    SendMessage { contact: PublicKey, kind: Kind },
    CreateGroup { members: Vec<PublicKey>, name: String },
    /* ... */
}

pub enum Event {
    TorStatusChanged(TorStatus),
    MessageReceived { from: PublicKey, envelope: Envelope },
    ContactUpdated(PublicKey),
    DeliveryStatusChanged { message: MessageId, status: DeliveryStatus },
    /* ... */
}
```

This split — one object, Command/Event boundary — is what makes Tauri IPC, CLI IPC, and integration tests all work through the same interface.

### 1.3 Dependency graph between modules

No module imports from a higher layer.

```
                daemon
                  │
       ┌──────────┼──────────┬────────────┐
       ▼          ▼          ▼            ▼
   delivery   contact    mailbox       invite
       │          │          │            │
       └──────────┼──────────┘            │
                  ▼                        │
                 mls  ───── envelope ◄─────┘
                  │
                  ▼
              transport
                  │
                  ▼
              identity ◄─── storage (sideways: used by many)
                              │
                              ▼
                           error
```

- `identity`, `storage`, `error` are leaf modules; no circular deps anywhere
- `mls` uses `storage` only through the `keystore` module's explicit trait
- `daemon` is the only module that wires everything together; nothing below it has a reference to it

### 1.4 Feature flags

Keep optional deps gated so the library is lean for callers that don't need everything:

```toml
[features]
default = ["qr"]
qr = ["dep:qrcode"]                # QR generation, not parsing
fuzzing = ["dep:arbitrary"]        # impls for fuzz targets
test-harness = []                  # exposes internals for integration tests
```

### 1.5 Public API surface (what `lib.rs` re-exports)

Keep this small. Anything not re-exported is implementation detail.

```rust
// lib.rs
pub use crate::daemon::{Daemon, Command, CommandResult, Event};
pub use crate::identity::{IdentityKey, PublicKey, Seed, Mnemonic};
pub use crate::contact::{Contact, ContactCard};
pub use crate::envelope::{Envelope, Kind, MessageId};
pub use crate::invite::{InviteLink, SignedInvite};
pub use crate::error::{CoreError, Result};

pub mod prelude {
    pub use crate::{Daemon, Command, Event, PublicKey};
}
```

Everything else (`transport`, `mls`, `storage`, etc.) is `pub(crate)` or only exposed via the `Daemon`.

### 1.6 Starting order in Phase 0

Concrete order of implementation to minimize blocked work:

1. `error` + `storage::pool` + `storage::migrations` runner (a day)
2. `identity::key` + `identity::seed` + `identity::vault` (2–3 days)
3. `transport::tor` standalone with echo test (3–5 days, includes Arti fighting)
4. `transport::frame` + fuzz target (2 days)
5. `daemon::state` skeleton that owns Tor + storage, exposes `start`/`shutdown` (2 days)
6. First integration test: two daemons on localhost (loopback Tor) exchange raw frames

Phase 1 then picks up with Noise and MLS on top of this foundation.

---

## Part 2 — MLS group state machine

### 2.1 Why this deserves its own state machine

OpenMLS is stateful and strict. The protocol has hard ordering requirements (every message belongs to a specific `epoch`, and epochs advance via `Commit` messages). In practice, three realities collide:

1. **Messages arrive out of order.** A Commit and the Application messages after it can race in transit.
2. **Users go offline for long stretches.** A week later they come back to a group that has advanced five epochs.
3. **State persistence can fail halfway.** Process killed during a Commit write; next start, the on-disk state and in-memory expectations disagree.

Naively calling OpenMLS methods as messages arrive gives you a brittle system that bricks groups on first network hiccup. The fix is an explicit state machine that wraps OpenMLS, buffers out-of-order input, persists transitions atomically, and knows when it's stuck enough to need recovery.

### 2.2 States

```rust
pub enum GroupState {
    /// Group is fully functional. Can send and receive.
    Active {
        epoch: u64,
    },

    /// We've received a Welcome but haven't processed it yet (or processing is async).
    PendingJoin {
        welcome_cached: bool,
    },

    /// We've built a Commit and are waiting for it to be acknowledged/processed.
    /// We cannot produce new proposals in this state.
    PendingCommit {
        epoch_before: u64,
        staged_commit_id: CommitId,
    },

    /// We received messages for an epoch we haven't reached yet. Buffering until
    /// the Commits that get us there arrive.
    CatchingUp {
        local_epoch: u64,
        seen_epoch: u64,
        buffered: Vec<BufferedMessage>,
    },

    /// We were removed from the group. Terminal for this member's view.
    Removed {
        at_epoch: u64,
    },

    /// State is unrecoverable locally; needs user-level recovery (re-invite).
    /// This is a last-resort state, not a first-try one.
    Corrupt {
        reason: CorruptReason,
    },
}
```

The design goals:

- **Every transition is triggered by exactly one input** (command, incoming message, timer tick). No implicit transitions. This makes the code greppable and the behavior testable.
- **`Corrupt` is reachable but rare.** Most "bad" situations are `CatchingUp` with a retry.
- **Only `Active` can send.** Every other state silently queues outbound messages in the outbox until transition.

### 2.3 Transition diagram

```
                          ┌───────────────────────┐
                          │                       │
    [create/invite]       │                       ▼
  ──────────────►  Active ────send App────►  Active
                     │ ▲      (same epoch)
                     │ │
                     │ └──recv Commit we applied────┐
                     │                              │
                     │                              │
          build Commit│                              │ recv expected
                     ▼                              │ Application
              PendingCommit                         │
                     │                              │
         ┌───────────┼────────────┐                 │
         │           │            │                 │
     our Commit  our Commit   timeout waiting        │
      applied     rejected   for confirmation        │
         │           │            │                 │
         ▼           ▼            ▼                 │
      Active     Active(same   Active(retry)─────────┘
              epoch, rolled
                 back)

    [recv message for
    future epoch]
  ──────────────────► CatchingUp
                         │
                         │ recv missing Commit(s)
                         ▼
                      Active (buffer drained and applied in order)


    [recv Welcome]
  ──────────────────► PendingJoin ──process──► Active


    [recv Remove targeting me]
  ──────────────────► Removed  (terminal)


    [persistence failure detected on load]
  ──────────────────► Corrupt  (terminal for this group id)
```

### 2.4 Transition table

Columns: current state, event, next state, side effects.

| From | Event | To | Side effects |
|------|-------|-----|--------------|
| `Active(e)` | Send application payload | `Active(e)` | encrypt, push to outbox |
| `Active(e)` | Build Add/Remove/Update Commit | `PendingCommit(e, id)` | persist staged commit, broadcast to peers |
| `PendingCommit(e, id)` | All (or quorum) peers acked commit | `Active(e+1)` | apply commit to MLS, persist new state, drain buffered messages for e+1 |
| `PendingCommit(e, id)` | Conflicting Commit arrived first | `Active(e+1)` | drop our staged commit, apply theirs, retry our change later |
| `PendingCommit(e, id)` | Timeout (configurable, default 30s) | `Active(e)` | drop staged commit, retry policy decides whether to rebuild |
| `Active(e)` | Recv Application for epoch e | `Active(e)` | decrypt, deliver to app, ack |
| `Active(e)` | Recv Commit for epoch e | `Active(e+1)` | apply, persist, drain buffer |
| `Active(e)` | Recv message for epoch > e | `CatchingUp(e, seen, [msg])` | buffer, request missing commits from a peer |
| `CatchingUp(e, seen, buf)` | Recv Commit for epoch e | `CatchingUp(e+1, seen, buf)` or `Active(e+1)` if caught up | apply, try to drain buffer |
| `CatchingUp` | Catchup timeout (e.g. 5 min) | `Corrupt(CatchupTimeout)` | user-facing error, suggest rejoin |
| `Active/Pending/Catching` | Recv Welcome for this group | ignore (`log_warn`) | — |
| `*` | Recv Remove where removed = self | `Removed(e)` | wipe key material, mark read-only |
| `Active` | `reset_requested` (user action) | `Corrupt(UserReset)` | require re-invite |

### 2.5 Persistence rules

The state machine can crash at any transition. Rules for durability:

1. **Persist before you tell anyone.** Apply the transition to sqlite (including new MLS state bytes) in a transaction, *then* emit events to the UI or send frames on the wire. If the process dies between DB write and network, retry on next start. If it dies before DB write, nothing happened.
2. **Stage, then commit.** For `PendingCommit`, the staged commit is persisted separately from the epoch bump. On restart, the recovery code checks: did our commit get applied by the group? If yes, advance; if no, roll back the staged commit.
3. **Buffered messages are persisted too.** `CatchingUp.buffered` isn't in memory — it's in a `mls_buffer` table keyed by `(group_id, epoch)`. Restarts lose nothing.
4. **Atomic swap of MLS state blob.** OpenMLS state is serialized as a blob; overwrite via a temp row + rename pattern, never partial updates.

### 2.6 Recovery: how we detect and leave `Corrupt`

Corrupt is user-visible: "This group's state can't be recovered on this device. Ask a member to re-add you." The detection paths:

- **Load-time integrity check.** On startup, for each group: can we deserialize the MLS state? Does the persisted epoch match OpenMLS's view? If not, `Corrupt(CheckFailed)`.
- **Unresolvable catchup.** `CatchingUp` with timeout → we've been asking peers for missing commits and getting nothing. Either we're isolated, or the commits are lost. Transition to `Corrupt(CatchupTimeout)`.
- **Bad commit application.** OpenMLS rejects a commit we thought was valid. This shouldn't happen but if it does, `Corrupt(MlsRejected)`.

Recovery options offered:

- **Re-invite by another member.** Most common case. UI shows a banner: "Ask [member] to re-add you to [group]." The other member does an `Add` + `Commit` with a fresh KeyPackage from us.
- **Export and import.** If it's a 1:1, user can export their half, reset, and re-establish from an invite link.
- **Last-resort: drop the group locally.** Works only for groups where nothing personally important was; in most cases re-invite is better because history stays.

### 2.7 Ordering and concurrency

All MLS state transitions for a given group go through a single tokio task per group. Send channels feed it commands; it owns the `Group` struct, no locks. This gives you:

- Impossible to have two concurrent transitions on the same group
- Straightforward reasoning about ordering
- Natural backpressure (channel bounded)

Cross-group operations go through the `daemon::state` layer which fans out.

### 2.8 The out-of-order buffer, concretely

When in `CatchingUp`, buffered messages are indexed by `(group_id, epoch, kind)`:

```rust
struct BufferedMessage {
    group_id: GroupId,
    epoch: u64,
    kind: BufferedKind,   // Commit | Application | Proposal
    payload: Vec<u8>,     // raw MLS message bytes
    received_at: u64,
}
```

Buffer rules:

- Bounded: max 1000 messages per group, oldest dropped with warning log
- Oldest-first drain: when we advance to epoch e, process all buffered messages for epoch e in `received_at` order
- Applications buffered for an epoch we've already left are discarded with a warning (likely a delayed retransmit)
- Commits for epochs behind us are ignored

### 2.9 Testing the state machine

This is the part everyone skimps on. Commit to these test categories:

**Property tests** over a `Scenario` DSL:

```rust
let scenario = Scenario::builder()
    .actors(["alice", "bob", "carol"])
    .with_group_created_by("alice")
    .add_member("bob")
    .add_member("carol")
    .send("bob", "hello")
    .partition("alice")          // alice goes offline
    .add_member("dave")           // bob commits while alice is out
    .send("dave", "hi all")
    .heal("alice")                // alice comes back
    .assert_everyone_sees_same_history();
```

The harness runs each actor's state machine in-process with a controllable delivery layer.

**Fuzz the delivery order.** Given a set of produced MLS messages, shuffle and drop with a seed, drive each actor's state machine, assert `Active` and identical view at the end (barring dropped messages).

**Targeted state transitions.** For each row in the transition table, a test that constructs the precondition and asserts the postcondition.

**Crash-restart tests.** Using a fake panic injection point, crash mid-transition, restart, verify recovery.

### 2.10 The dangerous temptations

A partial list of "clever" things that look helpful and create subtle bugs. Don't:

- **Don't eagerly apply commits from unknown senders.** Validate membership first; OpenMLS does this but double-check at your boundary too.
- **Don't let the UI force-transition states.** Every transition is event-driven.
- **Don't share `openmls::MlsGroup` across threads.** Use the per-group actor model.
- **Don't store MLS state unencrypted on disk.** Even "just for debugging."
- **Don't treat `Corrupt` as normal.** It should be rare enough that hitting it during development is a bug to investigate.

---

## Part 3 — Mailbox wire protocol, end-to-end

### 3.1 Goals

A minimal, versioned, Tor-only protocol that lets:

- Anyone deposit ciphertext addressed to a recipient hash
- The legitimate recipient (and only them) retrieve deposits
- Operators enforce size, rate, and TTL policies without seeing content
- Clients subscribe for near-real-time delivery when they're online

No user identifier is ever sent in plaintext. The mailbox sees recipient *hashes*, not public keys.

### 3.2 Transport

- Tor onion service, v3
- Inside the stream, reuse the same frame codec as peer-to-peer (`length_u32 || type_u8 || payload`)
- Frame types for the mailbox protocol are a disjoint range from peer-to-peer frames

### 3.3 Version negotiation

First frame after stream open is always `PROTOCOL_HELLO`.

```
PROTOCOL_HELLO (type = 0x80)
  payload: {
    "app": "skattr-mailbox",
    "version": 1,
    "capabilities": ["deposit", "fetch", "delete", "subscribe", "register"],
    "server_nonce": <32 random bytes>,     # used for challenge auth
  }
```

Client responds:

```
PROTOCOL_HELLO_ACK (type = 0x81)
  payload: {
    "version": 1,                           # exact match or error
    "capabilities": [...],                  # intersection of wanted and offered
  }
```

Mismatch → `ERROR { code: UnsupportedVersion, server_version }` and close.

### 3.4 Frame types

| Type | Name | Direction | Purpose |
|------|------|-----------|---------|
| 0x80 | PROTOCOL_HELLO | S→C | Server announces version and challenge nonce |
| 0x81 | PROTOCOL_HELLO_ACK | C→S | Client confirms version |
| 0x82 | DEPOSIT | C→S | Store a ciphertext for a recipient |
| 0x83 | DEPOSIT_OK | S→C | Deposit accepted, returns id |
| 0x84 | FETCH | C→S | Retrieve pending deposits (authenticated) |
| 0x85 | FETCH_RESULT | S→C | Zero or more deposits |
| 0x86 | DELETE | C→S | Remove deposits (authenticated) |
| 0x87 | DELETE_OK | S→C | Deletion confirmed |
| 0x88 | SUBSCRIBE | C→S | Long-poll for new deposits |
| 0x89 | SUBSCRIBE_PUSH | S→C | New deposit (during active subscribe) |
| 0x8A | REGISTER | C→S | Optional: register recipient (operator policy) |
| 0x8B | REGISTER_OK | S→C | Registration confirmed |
| 0x8F | ERROR | S→C | Typed error |
| 0x8E | BYE | Either | Graceful close |

### 3.5 Operations in detail

Fields below are CBOR-encoded (canonical CBOR, sorted keys, definite lengths).

**DEPOSIT — anyone to anyone**

Request:
```
{
  "recipient_hash": <32-byte sha256 of recipient pubkey>,
  "ciphertext": <bytes, MLS application message>,
  "expires_at": <unix seconds, capped by operator policy>,
  "sender_cover": <optional 16-byte random, for size padding>
}
```

Constraints enforced by server:
- `ciphertext` length ≤ `max_deposit_size` (default 1 MiB)
- `expires_at` ≤ `now + max_ttl` (default 30 days)
- Per-circuit rate limit (default: 60 deposits/hour)
- Per-recipient storage cap (default: 100 deposits, 100 MiB)

Response (success):
```
DEPOSIT_OK {
  "deposit_id": <16 random bytes>,
  "expires_at": <actual TTL assigned>,
}
```

Response (failure):
```
ERROR { "code": "...", "message": "..." }
```

Codes: `TooLarge`, `RateLimited`, `RecipientFull`, `TtlTooLong`, `RegistrationRequired`.

**CHALLENGE (implicit via HELLO) — no separate frame**

The `server_nonce` from `PROTOCOL_HELLO` is what FETCH and DELETE sign. Signing input:

```
"skattr-mailbox-auth-v1" || server_nonce || operation_byte || operation_hash
```

where `operation_hash = sha256(cbor(operation_payload))`.

This binds the signature to:
- The specific server (onion address identifies server, nonce is per-connection)
- This connection (nonce is fresh per `PROTOCOL_HELLO`)
- This operation (operation_byte + hash)
- This request's payload (hash over payload)

Replay resistance: server tracks (nonce, operation) used within the session; each nonce is valid for a single session.

**FETCH — recipient retrieves deposits**

Request:
```
{
  "recipient_hash": <32 bytes>,
  "recipient_pubkey": <32 bytes>,           # proves hash preimage
  "after": <optional deposit_id, for resumption>,
  "max": <optional max count, default 100>,
  "signature": <Ed25519 over auth string>,
}
```

Server validates:
- `sha256(recipient_pubkey) == recipient_hash`
- `signature` verifies under `recipient_pubkey` over the auth string (see CHALLENGE above)
- `after`, if provided, exists and belongs to this recipient

Response:
```
FETCH_RESULT {
  "deposits": [
    {
      "deposit_id": <16 bytes>,
      "ciphertext": <bytes>,
      "deposited_at": <unix seconds>,
    },
    ...
  ],
  "more_available": <bool>,                 # true if paginated
}
```

**DELETE — recipient removes retrieved deposits**

Same auth pattern. Payload:
```
{
  "recipient_hash": <32 bytes>,
  "recipient_pubkey": <32 bytes>,
  "deposit_ids": [<16 bytes>, <16 bytes>, ...],
  "signature": <Ed25519 over auth string>,
}
```

Response:
```
DELETE_OK {
  "deleted": <count>,
  "not_found": <count>,   # deposits that expired or didn't exist
}
```

**SUBSCRIBE — online recipient, push delivery**

After authenticating with FETCH, client can issue SUBSCRIBE to keep the stream open for push:

```
SUBSCRIBE {
  "recipient_hash": <32 bytes>,
  "recipient_pubkey": <32 bytes>,
  "signature": <Ed25519 over auth string>,
  "keepalive_interval": <seconds, default 60>,
}
```

Server holds the stream; on new deposit matching the hash, sends:

```
SUBSCRIBE_PUSH {
  "deposit_id": <16 bytes>,
  "ciphertext": <bytes>,
  "deposited_at": <unix seconds>,
}
```

Client is expected to `DELETE` after processing. If the client doesn't DELETE, normal polling picks up the message later.

Server sends a keepalive PING every `keepalive_interval`; subscribe ends cleanly with BYE.

Subscription limits:
- One active subscription per `recipient_hash` per server (second one displaces the first)
- Server may drop subscriptions after an idle period (default: 30 minutes; client reconnects)

**REGISTER — optional policy gate**

Some mailbox operators require registration to prevent unknown-recipient spam. Registration binds a recipient_hash to a first-seen auth signature; subsequent DEPOSIT is allowed for that hash. Operators who run open mailboxes skip this entirely.

```
REGISTER {
  "recipient_hash": <32 bytes>,
  "recipient_pubkey": <32 bytes>,
  "signature": <Ed25519 over auth string>,
  "invite_code": <optional bytes>,          # if operator uses invite codes
}
```

Response: `REGISTER_OK` or `ERROR { "code": "InviteRequired" | "RecipientAlreadyRegistered" }`.

### 3.6 Error codes (full list)

| Code | Meaning |
|------|---------|
| `UnsupportedVersion` | Version negotiation failed |
| `MalformedRequest` | CBOR invalid or fields missing |
| `TooLarge` | Payload exceeds limit |
| `RateLimited` | Per-circuit rate limit hit |
| `RecipientFull` | Recipient's storage cap hit |
| `TtlTooLong` | expires_at exceeds operator max |
| `TtlTooShort` | expires_at in the past |
| `RegistrationRequired` | Operator requires registration |
| `InviteRequired` | Operator requires invite code |
| `InvalidSignature` | Auth signature doesn't verify |
| `HashMismatch` | sha256(pubkey) != recipient_hash |
| `NonceExpired` | server_nonce stale or reused |
| `NotFound` | deposit_id unknown or expired |
| `Internal` | Server error, try again later |

### 3.7 Size padding

To reduce size-based traffic analysis:

- Clients pad DEPOSIT ciphertext to the next bucket: `{256B, 1KiB, 4KiB, 16KiB, 64KiB, 256KiB, 1MiB}`
- Padding is part of the ciphertext, invisible to the server
- FETCH_RESULT is not padded (already varies; correlating with deposits is not useful to an attacker)

### 3.8 Storage schema (server side)

```sql
CREATE TABLE deposits (
  deposit_id BLOB PRIMARY KEY,              -- 16 random bytes
  recipient_hash BLOB NOT NULL,             -- 32 bytes
  ciphertext BLOB NOT NULL,
  deposited_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL
);

CREATE INDEX idx_deposits_recipient ON deposits(recipient_hash, deposited_at);
CREATE INDEX idx_deposits_expiry ON deposits(expires_at);

CREATE TABLE registrations (
  recipient_hash BLOB PRIMARY KEY,          -- 32 bytes
  registered_at INTEGER NOT NULL,
  invite_code BLOB                          -- optional, consumed
);
```

Notes:

- `deposit_id` is a random UUID-like identifier, not a sequence number (leaks less metadata about volume)
- The `ciphertext` BLOB is what the client already padded; server stores as-is
- Expiry sweep is a single `DELETE FROM deposits WHERE expires_at < ?` on a timer

### 3.9 Operator configuration

```toml
# mailbox.toml
[server]
onion_key_path = "/var/lib/skattr-mailbox/hs_key"
data_dir = "/var/lib/skattr-mailbox"
listen_timeout_seconds = 300

[policy]
max_deposit_size = 1048576                  # 1 MiB
max_ttl_seconds = 2592000                   # 30 days
rate_limit_per_circuit_per_hour = 60
recipient_cap_count = 100
recipient_cap_bytes = 104857600             # 100 MiB
require_registration = false
invite_codes_enabled = false

[capacity]
total_storage_bytes = 10737418240           # 10 GiB
reject_threshold_percent = 90               # stop accepting new recipients at 90%
```

### 3.10 Privacy properties the server provides

Reiterated concretely, per-operation:

| Operation | Server learns | Server does not learn |
|-----------|--------------|----------------------|
| DEPOSIT | Recipient hash; ciphertext size (padded); deposit time; sender's Tor circuit | Sender identity; recipient identity (just the hash); contents |
| FETCH | Recipient identity (via pubkey); online time of recipient | Contents; who sent deposits |
| DELETE | Same as FETCH plus which deposits are being deleted | Contents |
| SUBSCRIBE | Recipient identity; online session duration | Contents |

The key trade: to FETCH, the recipient reveals their identity to the mailbox operator. This is why self-hosted or trusted-party mailboxes are the recommended model.

### 3.11 What the server never does

Worth being explicit:

- No logging of recipient_hash or pubkey values, even to local logs
- No retention beyond TTL; no "archive"
- No cross-recipient analytics
- No federation or forwarding (each mailbox is standalone)
- No modification of ciphertext
- No "deleted" flag; delete means gone from disk

An operator who wants to violate any of this can, because they run the server. The architecture minimizes what they could see even if they did.

---

## Part 4 — First-run and seed phrase UX

### 4.1 Why this section exists

Most privacy-focused apps lose the majority of potential users in the first 90 seconds. The first-run experience has to do three things simultaneously:

1. **Generate and protect a keypair** the user can't lose without consequences
2. **Bootstrap Tor** (slow, variable, sometimes fails)
3. **Explain enough** that the user understands what they've set up

Getting any one of these wrong breaks the rest. This section is a concrete, screen-by-screen flow with copy that's been sanity-checked for clarity rather than for marketing punch.

### 4.2 Design principles

- **No dark patterns, no skip-buttons on the things that matter.** Seed phrase confirmation is required; no "I'll do it later."
- **Explain once, concisely, in plain language.** Assume the user has heard "encrypted" before but not "key derivation function."
- **Show progress for anything over 2 seconds.** Tor bootstrap in particular.
- **Fail visibly.** A silent Tor failure is worse than a loud one.
- **Every screen has an "exit" that doesn't destroy state.** Close the app mid-setup → next launch resumes where you left off.

### 4.3 The full flow

```
[Welcome] → [Passphrase] → [Generating…] → [Seed Display]
   → [Seed Confirm] → [Tor Bootstrap] → [Ready]
```

**Existing user path** (diverges at Welcome):

```
[Welcome] → [Restore Passphrase or Seed] → [Tor Bootstrap] → [Ready]
```

### 4.4 Screen 1: Welcome

Single screen, single decision.

```
╭─────────────────────────────────────────────╮
│                                             │
│               [app logo]                    │
│                                             │
│          Welcome to Skattr                 │
│                                             │
│     Private messaging without servers.      │
│                                             │
│                                             │
│     ┌─────────────────────────────────┐    │
│     │      Set up a new account       │    │
│     └─────────────────────────────────┘    │
│                                             │
│     ┌─────────────────────────────────┐    │
│     │      Restore existing account   │    │
│     └─────────────────────────────────┘    │
│                                             │
│                                             │
│       What does Skattr protect?  →         │
│                                             │
╰─────────────────────────────────────────────╯
```

Copy notes:

- "Private messaging without servers" is the one-line pitch. Don't say "anonymous" (legally and technically hard to defend).
- The "What does Skattr protect?" link goes to a clear page: what the app does protect (message contents, participation in conversations from outsiders), what it doesn't (endpoint compromise, mailbox operator, social graph if mailbox is shared). Honest from the start.
- No "Sign up" / "Log in" verbs — this is a local account, there's no signup.

### 4.5 Screen 2: Passphrase

```
╭─────────────────────────────────────────────╮
│  ← Back                                      │
│                                             │
│       Protect your account                  │
│                                             │
│  Choose a passphrase. It encrypts your      │
│  account on this device.                    │
│                                             │
│  You'll need it every time you open the     │
│  app on this device.                        │
│                                             │
│  Passphrase                                 │
│  ┌─────────────────────────────────────┐   │
│  │                                     │   │
│  └─────────────────────────────────────┘   │
│                                             │
│  Strength: ████░░░░░░  Fair                 │
│                                             │
│  Confirm                                    │
│  ┌─────────────────────────────────────┐   │
│  │                                     │   │
│  └─────────────────────────────────────┘   │
│                                             │
│  ⚠  There is no password reset.             │
│     If you forget this, you lose access.    │
│     We'll give you a recovery phrase next.  │
│                                             │
│                                             │
│        ┌─────────────────────┐              │
│        │       Continue      │              │
│        └─────────────────────┘              │
│                                             │
╰─────────────────────────────────────────────╯
```

Requirements:

- Minimum 10 characters. No maximum. No character class requirements (those are cargo cult security).
- Strength meter uses zxcvbn. Do not block submission based on "strength" alone; block only on length < 10.
- Explicitly mention there's no reset. Users are used to "forgot password" links; they must know this is different.
- Note the recovery phrase up front — reduces the "wait what is this?" reaction later.

### 4.6 Screen 3: Generating

```
╭─────────────────────────────────────────────╮
│                                             │
│       Setting up your account…              │
│                                             │
│            [ animated spinner ]             │
│                                             │
│       Generating encryption keys            │
│                                             │
│       This takes a few seconds.             │
│                                             │
╰─────────────────────────────────────────────╯
```

What's actually happening: seed generation, identity key derivation, Argon2id run at configured params (might take 1–3 seconds on slow hardware), vault file creation. No user interaction; screen stays for at least 1.5 seconds even if done faster, so the transition doesn't jar.

### 4.7 Screen 4: Seed phrase display

The single most important screen. Default is hidden, user reveals deliberately.

```
╭─────────────────────────────────────────────╮
│  ← Back                                      │
│                                             │
│       Write down your recovery phrase       │
│                                             │
│  These 24 words are the only way to         │
│  restore your account if you lose this      │
│  device or forget your passphrase.          │
│                                             │
│                                             │
│  ┌─────────────────────────────────────┐   │
│  │                                     │   │
│  │      ···  Tap to reveal  ···        │   │
│  │                                     │   │
│  └─────────────────────────────────────┘   │
│                                             │
│                                             │
│  How to store it safely:                    │
│                                             │
│   • Write it on paper and keep it somewhere │
│     private                                 │
│   • Don't take a photo                      │
│   • Don't type it into a password manager   │
│     on another device                       │
│   • Don't share it with anyone, ever        │
│                                             │
│                                             │
│        ┌─────────────────────┐              │
│        │      Continue       │              │
│        └─────────────────────┘              │
│                                             │
╰─────────────────────────────────────────────╯
```

Revealed state:

```
│  ┌─────────────────────────────────────┐   │
│  │                                     │   │
│  │   1. advice       13. mammal        │   │
│  │   2. brown        14. never         │   │
│  │   3. cherry       15. ocean         │   │
│  │   4. doctor       16. protect       │   │
│  │   5. equal        17. quantum       │   │
│  │   6. fabric       18. ribbon        │   │
│  │   7. giant        19. secret        │   │
│  │   8. harbor       20. talent        │   │
│  │   9. ignite       21. uniform       │   │
│  │  10. jungle       22. village       │   │
│  │  11. kettle       23. whisper       │   │
│  │  12. legend       24. yonder        │   │
│  │                                     │   │
│  │            [Hide phrase]            │   │
│  └─────────────────────────────────────┘   │
```

UX details that matter:

- Blurred/hidden by default; explicit tap to reveal. Protects against shoulder surfers and screen-recording attacks.
- Numbered words. Order matters, and numbering eliminates one common transcription error.
- Fixed-width font for the words, consistent column widths. Easier to proofread.
- "Hide phrase" button puts it back to hidden. Useful if someone walks in.
- No "Copy to clipboard" button. This is deliberate. Clipboard is cross-app, often cross-device (handoff, synced clipboards). A copy button trains the wrong behavior.
- No "Skip" or "I'll do this later." The user must at least *see* the phrase once.
- On mobile, disable screenshot on this screen where the platform allows (`FLAG_SECURE` on Android, blurred thumbnail on iOS). Desktop can't really do this but we can still not encourage it.

### 4.8 Screen 5: Seed phrase confirmation

Prove the user actually wrote it down. Don't just ask "did you write it down?" — everyone lies.

```
╭─────────────────────────────────────────────╮
│  ← Back                                      │
│                                             │
│       Confirm your recovery phrase          │
│                                             │
│  Enter the following words to make sure     │
│  you've stored your phrase correctly.       │
│                                             │
│                                             │
│   Word #3       Word #11      Word #17      │
│  ┌────────┐   ┌────────┐   ┌────────┐      │
│  │        │   │        │   │        │      │
│  └────────┘   └────────┘   └────────┘      │
│                                             │
│   Word #22                                  │
│  ┌────────┐                                 │
│  │        │                                 │
│  └────────┘                                 │
│                                             │
│                                             │
│  [ ] I understand I cannot recover my       │
│      account without this phrase            │
│                                             │
│                                             │
│        ┌─────────────────────┐              │
│        │       Confirm       │              │
│        └─────────────────────┘              │
│                                             │
╰─────────────────────────────────────────────╯
```

Mechanics:

- Pick 4 random word positions (not 1st, not 24th — those are easier to remember without writing)
- Words are typed, not picked from a dropdown. Dropdowns train "I recognize the word" rather than "I know the word."
- Autocomplete from BIP39 wordlist after 3 characters (prevents typo rage, doesn't remove the security)
- Wrong answer: subtle shake animation, "That's not the word we're looking for. Check your list." No three-strikes lockout; this is local.
- The checkbox is required and separate from the words. Signing a mental contract.
- "Back" returns to seed display, not all the way to passphrase. User hasn't lost work.

### 4.9 Screen 6: Tor bootstrap

This takes anywhere from 5 seconds to 2 minutes depending on network. Visible progress is non-negotiable.

```
╭─────────────────────────────────────────────╮
│                                             │
│       Connecting to the Tor network…        │
│                                             │
│  ████████████░░░░░░░░░░░░░░░  42%           │
│                                             │
│  Building circuits                          │
│                                             │
│                                             │
│  Your messages travel through Tor so that   │
│  no one can see who you're talking to, or   │
│  where you are.                             │
│                                             │
│  This takes longer the first time.          │
│                                             │
│                                             │
│                                             │
│  Having trouble connecting?  →              │
│                                             │
╰─────────────────────────────────────────────╯
```

States to surface (mapped from Arti's bootstrap phases):

| Percent | Label |
|---------|-------|
| 0–15% | "Starting Tor" |
| 15–50% | "Fetching network directory" |
| 50–85% | "Building circuits" |
| 85–99% | "Publishing your address" |
| 100% | "Connected" |

If stuck for >30 seconds at any phase, show a subtle "Still working…" note. If stuck >2 minutes, surface the trouble-connecting link prominently.

The trouble-connecting page covers:

- Am I behind a firewall? (Try bridges, future feature)
- Is my clock correct? (Tor needs accurate clocks; this is a real failure mode)
- Am I on a network that blocks Tor? (Some corporate / regional)
- Try again button

### 4.10 Screen 7: Ready

```
╭─────────────────────────────────────────────╮
│                                             │
│              ✓ You're set up                │
│                                             │
│                                             │
│   Your address:                             │
│   ┌─────────────────────────────────────┐  │
│   │  skattr:7f3a...b2                  │  │
│   └─────────────────────────────────────┘  │
│                                             │
│                                             │
│   Add your first contact by:                │
│                                             │
│   ┌─────────────────────────────────────┐  │
│   │   Sharing an invite with someone    │  │
│   └─────────────────────────────────────┘  │
│                                             │
│   ┌─────────────────────────────────────┐  │
│   │   Opening an invite you received    │  │
│   └─────────────────────────────────────┘  │
│                                             │
│                                             │
│              Explore the app  →             │
│                                             │
╰─────────────────────────────────────────────╯
```

Notes:

- Truncated address is display-only; full address behind tap. Prevents "it's too long I'll never remember it" reactions.
- Two contact-adding paths equally weighted; don't assume the user's role.
- "Explore the app" is the escape hatch to the empty main view.

### 4.11 Restore flow (existing user path)

```
[Welcome: Restore] → [Enter passphrase or seed] → [Deriving…]
   → [Tor Bootstrap] → [Ready, restore from backup?]
```

**Restore screen:**

```
╭─────────────────────────────────────────────╮
│  ← Back                                      │
│                                             │
│       Restore your account                  │
│                                             │
│  Choose how you want to restore:            │
│                                             │
│  ( • ) Passphrase + local vault file        │
│        (Fastest. Requires the vault file    │
│         from your previous device.)         │
│                                             │
│  (   ) Recovery phrase (24 words)           │
│        (Rebuilds your identity. Contacts    │
│         will need to re-add you.)           │
│                                             │
│                                             │
│        ┌─────────────────────┐              │
│        │      Continue       │              │
│        └─────────────────────┘              │
│                                             │
╰─────────────────────────────────────────────╯
```

Be honest about the second option. Restoring from seed gets the identity back but not the contact graph, the MLS state, or the message history. Contacts need to re-invite. This is the biggest UX debt of the "identity = private key" model, and users need to know about it before choosing.

### 4.12 Edge cases and error paths

**User closes the app mid-setup.** On next launch:

- If vault was created but seed not confirmed: re-display seed phrase + confirmation screen
- If vault created and seed confirmed but Tor not bootstrapped: go straight to Tor bootstrap
- If everything done: normal login (passphrase prompt)

**Wrong passphrase at login.** Standard: error message, no lockout (local only, lockout adds no security), no limit on attempts. Rate-limit the KDF: Argon2id params already make brute force expensive.

**User enters the wrong word during confirmation, three times.** Offer a "Show phrase again" button. Some users genuinely can't read their own handwriting. Don't treat this as a security signal.

**Tor fails to bootstrap repeatedly.** Offer:
- Retry
- Continue in offline mode (lets user explore UI; warn prominently that nothing will work)
- Report problem (local diagnostics, never auto-submitted)

**User tries to paste into the seed confirmation fields.** Silently strip. Don't allow clipboard paste into any seed-phrase entry field. The seed should never be in clipboard.

**User is on a system clock that's wrong by >1 hour.** Tor bootstrap will fail in weird ways. Detect early (NTP query if allowed, or heuristic from Tor's error messages), surface clearly: "Your system clock appears to be off. Tor needs an accurate time."

### 4.13 Accessibility

- All screens keyboard-navigable (tab order matches visual order)
- Strength meter and error states are not color-only (text labels required)
- Seed phrase readable by screen readers with "word one, advice, word two, brown…" synthesis
- Font scales with OS accessibility settings
- Minimum 4.5:1 contrast everywhere
- Animations respect `prefers-reduced-motion`

### 4.14 Telemetry (or rather, the lack of it)

Don't instrument any of these flows. It's tempting to want to know "where do users drop off in first-run?" — resist. Any metric you collect here is a metric an adversary could also collect. The way you learn this is through small, voluntary usability studies with informed testers, not analytics.

If you absolutely must know drop-off rates, the answer is a local counter that users can opt to share as part of a one-time feedback form, not continuous telemetry.

### 4.15 The "why is this different from Signal" explainer

Somewhere — in the "What does Skattr protect?" link on Welcome, probably — a short page addressing the comparison users will inevitably make:

> **Signal is excellent.** Skattr isn't trying to replace it. Signal is the right choice if your threat model is "I want strong end-to-end encryption with a mainstream app." Signal requires a phone number, its servers see who's talking to whom (by phone number hashes), and you need to trust Signal as an organization.
>
> **Skattr is for people who need more than that.** No phone number. No account server. All traffic goes over Tor, so network observers can't see who you're connecting to. The tradeoff is that offline delivery depends on someone running a mailbox — either you, a friend, or a volunteer — and that mailbox sees a bit of metadata (your address, when you poll) but never contents.
>
> Skattr is also much newer and less tested. Use Signal for the things that really matter, and give Skattr a try for the things that matter *and* require stronger anonymity guarantees.

Being this honest up front does two things: it builds trust with users who are sophisticated enough to notice a comparison was avoided, and it filters out users whose actual need is "easy E2EE" (send them to Signal, they'll be happier) from those whose need is "strong metadata resistance" (welcome, and here's what you're trading).

---

## Cross-cutting: what these four pieces have in common

Looking across module structure, state machine, wire protocol, and first-run UX, a few themes:

- **Explicit states over implicit.** Every place where reality is complex (group state, connection state, first-run state), name the states and enumerate the transitions. Don't encode them as tangled if/else.
- **Persist the transition, then announce it.** Whether it's an MLS epoch bump, a deposit accepted, or a seed phrase confirmed — write to disk before you tell anyone.
- **Error paths are first-class.** The `Corrupt` MLS state, the `ERROR` mailbox frame, the "wrong word" confirmation flow — each has explicit design, not "TODO: handle this."
- **Honesty about tradeoffs.** The mailbox trades recipient pseudonymity to the operator; the seed flow trades convenience for irreversibility; the Signal comparison trades marketing for trust. Users pick up on the honest version faster than you'd think.
