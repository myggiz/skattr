# Skattr — Project Bootstrap Prompt for Claude Code

You are bootstrapping a new Rust project called **Skattr**, a desktop-first, metadata-resistant, serverless(ish) encrypted messaging application. The company behind it is Myggiz AB (Sweden). The product is licensed GPLv3 (client) and AGPLv3 (mailbox server).

## What Skattr is

A peer-to-peer encrypted messenger that routes all traffic over Tor v3 onion services, uses the MLS protocol (RFC 9420) for message encryption, and stores nothing centrally. No phone number, no email, no account server. Identity is an Ed25519 keypair backed by a BIP39 seed phrase.

## Tech stack decisions (locked)

- **Language:** Rust (2021 edition, stable toolchain)
- **Async runtime:** Tokio (required by Arti)
- **Tor:** Arti (`arti-client` + `tor-hsservice`) — embedded Rust Tor implementation
- **Noise handshake:** `snow` crate — Noise_XK_25519_ChaChaPoly_BLAKE2s
- **MLS:** `openmls` crate — ciphersuite MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519
- **Crypto:** `ed25519-dalek`, `x25519-dalek`, `chacha20poly1305` (RustCrypto ecosystem)
- **KDF:** Argon2id via `argon2` crate (params: m=64MiB, t=3, p=4)
- **Seed phrase:** BIP39 via `bip39` crate
- **Serialization:** CBOR via `ciborium` crate (wire protocol), TOML for config
- **Storage:** `rusqlite` with WAL mode, app-level encryption via `age` crate
- **Error handling:** `thiserror` for library errors, `anyhow` for binaries
- **Logging:** `tracing` + `tracing-subscriber`
- **UI (Phase 2, not now):** Tauri 2 + SvelteKit — reserve the `ui` crate but don't scaffold it yet
- **Zeroization:** `zeroize` crate for all secret material

## Task: Create the project scaffold

Create a complete Cargo workspace with the following structure. Every `.rs` file should have proper module declarations, doc comments, and placeholder implementations (types defined, methods stubbed with `todo!()` or minimal bodies). The goal is a project that compiles, passes `cargo clippy`, and has the full architecture visible in code — even though most functions aren't implemented yet.

### Workspace root

```
skattr/
├── Cargo.toml              # workspace manifest
├── rust-toolchain.toml     # pin stable toolchain
├── deny.toml               # cargo-deny config
├── LICENSE-GPL3             # GPLv3 text
├── LICENSE-AGPL3            # AGPLv3 text
├── README.md
├── CONTRIBUTING.md
├── SECURITY.md
├── CHANGELOG.md
├── ARCHITECTURE.md
├── .github/
│   └── workflows/
│       └── ci.yml          # GitHub Actions: fmt, clippy, test on Linux/macOS/Windows
├── crates/
│   ├── core/               # the main library
│   ├── mailbox/            # the mailbox server binary
│   ├── cli/                # the CLI binary
│   └── tests/              # integration test crate
└── docs/
    ├── adr/                # Architecture Decision Records
    │   ├── 0001-license.md
    │   ├── 0002-crypto-libraries.md
    │   └── 0003-storage-approach.md
    └── PROTOCOL.md
```

### `crates/core/` — the main library

This is where almost all logic lives. Structure:

```
crates/core/
├── Cargo.toml
├── src/
│   ├── lib.rs                      # re-exports, feature flags, crate docs
│   ├── error.rs                    # CoreError enum with thiserror, Result<T> alias
│   ├── prelude.rs                  # convenience re-exports
│   │
│   ├── identity/
│   │   ├── mod.rs
│   │   ├── key.rs                  # IdentityKey (Ed25519), PublicKey, Signature
│   │   ├── seed.rs                 # BIP39 encode/decode, Seed type, identity derivation via HKDF
│   │   ├── vault.rs                # Vault: passphrase-encrypted on-disk identity container
│   │   └── derive.rs               # domain-separated HKDF helpers
│   │
│   ├── transport/
│   │   ├── mod.rs
│   │   ├── tor.rs                  # TorRuntime: bootstrap, publish_onion, connect, status_stream
│   │   ├── noise.rs                # Noise_XK initiator/responder using snow
│   │   ├── frame.rs                # Frame enum + tokio_util codec
│   │   ├── connection.rs           # AuthenticatedConnection: Noise + Frame over Tor
│   │   └── listener.rs             # OnionListener: accepts connections, hands to session mgr
│   │
│   ├── mls/
│   │   ├── mod.rs
│   │   ├── ciphersuite.rs          # the one chosen ciphersuite constant
│   │   ├── keystore.rs             # OpenMlsKeyStore impl on top of storage
│   │   ├── group.rs                # Group wrapper over openmls::MlsGroup
│   │   ├── state_machine.rs        # GroupState enum: Active, PendingJoin, PendingCommit, CatchingUp, Removed, Corrupt
│   │   ├── welcome.rs              # Welcome processing
│   │   └── commit.rs               # Commit building and processing
│   │
│   ├── envelope/
│   │   ├── mod.rs
│   │   ├── message.rs              # Envelope struct with CBOR serde
│   │   └── kinds.rs                # Kind enum: Text, File, Reaction, Edit, Delete, Typing
│   │
│   ├── invite/
│   │   ├── mod.rs
│   │   ├── link.rs                 # InviteLink: parse, generate, sign, verify, to_url, from_url
│   │   └── qr.rs                   # feature-gated QR render
│   │
│   ├── contact/
│   │   ├── mod.rs
│   │   ├── contact.rs              # Contact struct
│   │   ├── card.rs                 # ContactCard (signed, versioned, published over wire)
│   │   └── rotation.rs             # onion/mailbox address rotation protocol
│   │
│   ├── mailbox/
│   │   ├── mod.rs
│   │   ├── protocol.rs             # wire types shared with mailbox server
│   │   ├── client.rs               # MailboxClient: register, deposit, fetch, delete
│   │   └── scheduler.rs            # polling strategy, backoff, fanout
│   │
│   ├── delivery/
│   │   ├── mod.rs
│   │   ├── outbox.rs               # persisted send queue + retry logic
│   │   ├── sender.rs               # drives outbox, chooses direct vs mailbox path
│   │   └── receiver.rs             # dedup, ordering, ack generation
│   │
│   ├── storage/
│   │   ├── mod.rs
│   │   ├── pool.rs                 # connection pool, WAL pragmas
│   │   ├── migrations/
│   │   │   └── 0001_init.sql       # initial schema
│   │   ├── contacts.rs             # ContactRepo
│   │   ├── messages.rs             # MessageRepo + FTS5
│   │   ├── groups.rs               # MlsGroupRepo
│   │   ├── outbox.rs               # OutboxRepo
│   │   └── mailboxes.rs            # MailboxRepo
│   │
│   └── daemon/
│       ├── mod.rs
│       ├── config.rs               # Config struct + TOML loader
│       ├── state.rs                # Daemon: owns all long-lived handles
│       ├── events.rs               # Event enum emitted to UI/CLI consumers
│       └── commands.rs             # Command enum consumed from UI/CLI
│
└── tests/
    ├── handshake.rs
    ├── first_message.rs
    ├── offline_delivery.rs
    └── group_membership.rs
```

### Key types to define (with doc comments, stub implementations)

**identity/key.rs:**
```rust
pub struct IdentityKey { /* Ed25519 signing key, Zeroizing wrapper */ }
pub struct PublicKey(pub [u8; 32]);
pub struct Signature(pub [u8; 64]);
```
With methods: `generate()`, `from_seed()`, `public()`, `sign()`, `verify()`.

**identity/seed.rs:**
```rust
pub struct Seed(Zeroizing<[u8; 32]>);
pub struct Mnemonic(Vec<String>);
```
With methods: `generate()`, `to_mnemonic()`, `from_mnemonic()`.

**identity/vault.rs:**
```rust
pub struct Vault { /* opaque */ }
```
With methods: `create()`, `open()`, `change_passphrase()`.

**daemon/commands.rs:**
```rust
pub enum Command {
    CreateInvite { nickname: Option<String> },
    AddContact { invite_url: String },
    SendMessage { contact: PublicKey, kind: Kind },
    CreateGroup { members: Vec<PublicKey>, name: String },
    Shutdown,
}
pub enum CommandResult { /* per-command response types */ }
```

**daemon/events.rs:**
```rust
pub enum Event {
    TorStatusChanged(TorStatus),
    MessageReceived { from: PublicKey, envelope: Envelope },
    ContactUpdated(PublicKey),
    DeliveryStatusChanged { message: MessageId, status: DeliveryStatus },
}
```

**daemon/state.rs:**
```rust
pub struct Daemon { /* owns TorRuntime, storage pool, active connections, outbox worker */ }
impl Daemon {
    pub async fn start(config: Config, passphrase: &str) -> Result<Self>;
    pub async fn execute(&self, cmd: Command) -> Result<CommandResult>;
    pub fn events(&self) -> broadcast::Receiver<Event>;
    pub async fn shutdown(self) -> Result<()>;
}
```

**mls/state_machine.rs:**
```rust
pub enum GroupState {
    Active { epoch: u64 },
    PendingJoin,
    PendingCommit { proposed_epoch: u64 },
    CatchingUp { target_epoch: u64 },
    Removed,
    Corrupt { reason: String },
}
```

### `crates/mailbox/` — the mailbox server

A standalone binary that accepts Tor connections and stores encrypted blobs for offline delivery. Shares `core::mailbox::protocol` types.

```
crates/mailbox/
├── Cargo.toml
└── src/
    ├── main.rs
    ├── config.rs           # MailboxConfig from TOML
    ├── server.rs           # listen loop, connection handling
    ├── store.rs            # SQLite blob storage
    └── auth.rs             # challenge-response auth via client signatures
```

License header: AGPLv3.

### `crates/cli/` — the command-line interface

A thin binary that wraps `core::Daemon`.

```
crates/cli/
├── Cargo.toml
└── src/
    └── main.rs             # clap-based CLI: init, restore, daemon, invite, send
```

Commands to define (using `clap` derive):
- `skattr init` — generate identity, require passphrase, output seed phrase
- `skattr restore <seed>` — rebuild identity from seed phrase
- `skattr daemon` — start Tor, publish onion, begin accepting connections
- `skattr invite` — generate and print an invite link
- `skattr add <link>` — add a contact from an invite link
- `skattr send <contact> <message>` — send a text message

### `crates/tests/` — integration tests

```
crates/tests/
├── Cargo.toml
└── src/
    └── lib.rs              # test helpers: spawn daemon pair, wait for Tor bootstrap
```

### Initial SQL migration (`0001_init.sql`)

```sql
CREATE TABLE IF NOT EXISTS identity (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    public_key BLOB NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS contacts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    identity_pubkey BLOB NOT NULL UNIQUE,
    display_name TEXT,
    added_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS onion_addresses (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    contact_id INTEGER NOT NULL REFERENCES contacts(id),
    address TEXT NOT NULL,
    seen_at INTEGER NOT NULL,
    is_current INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS mls_groups (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    group_id BLOB NOT NULL UNIQUE,
    state_blob BLOB NOT NULL,
    epoch INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    group_id BLOB NOT NULL,
    sender BLOB NOT NULL,
    kind TEXT NOT NULL,
    body_blob BLOB,
    ts INTEGER NOT NULL,
    delivered_at INTEGER
);

CREATE TABLE IF NOT EXISTS outbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    target BLOB NOT NULL,
    payload BLOB NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_retry_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS mailboxes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    onion TEXT NOT NULL,
    registered_at INTEGER NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('mine', 'theirs'))
);
```

### Cargo.toml dependencies (core crate)

```toml
[dependencies]
# Crypto
ed25519-dalek = { version = "2", features = ["rand_core"] }
x25519-dalek = { version = "2", features = ["static_secrets"] }
chacha20poly1305 = "0.10"
argon2 = "0.5"
hkdf = "0.12"
sha2 = "0.10"
rand = "0.8"
zeroize = { version = "1", features = ["derive"] }

# BIP39
bip39 = "2"

# MLS
openmls = "*"
openmls_traits = "*"
openmls_rust_crypto = "*"

# Tor
arti-client = { version = "*", features = ["onion-service-service"] }
tor-hsservice = "*"

# Noise
snow = "0.9"

# Transport
tokio = { version = "1", features = ["full"] }
tokio-util = { version = "0.7", features = ["codec"] }

# Serialization
ciborium = "0.2"
serde = { version = "1", features = ["derive"] }
toml = "0.8"

# Storage
rusqlite = { version = "0.31", features = ["bundled"] }

# Error handling & logging
thiserror = "1"
tracing = "0.1"

# Optional
qrcode = { version = "0.14", optional = true }
arbitrary = { version = "1", optional = true }

[features]
default = ["qr"]
qr = ["dep:qrcode"]
fuzzing = ["dep:arbitrary"]
test-harness = []
```

Note: Pin exact versions of `openmls`, `arti-client`, and `tor-hsservice` to whatever is latest stable at the time. These APIs shift. Use `cargo search` to find current versions.

### CI workflow (`.github/workflows/ci.yml`)

Standard Rust CI: check formatting, run clippy with `-D warnings`, run tests, across ubuntu-latest, macos-latest, and windows-latest. Use `dtolnay/rust-toolchain` action.

### Documentation skeletons

**README.md** — Project name, one-line description ("Skattr — messages, scattered"), what it is, what it isn't, status (Phase 0), how to build, license.

**ARCHITECTURE.md** — Workspace layout, crate dependency diagram (daemon → delivery/contact/mailbox/invite → mls/envelope → transport → identity/storage → error), data flow for "send one message."

**SECURITY.md** — Responsible disclosure instructions, PGP key placeholder, scope.

**CONTRIBUTING.md** — How to build, how to run tests, commit message format, PR process.

**docs/adr/0001-license.md** — GPLv3 for clients, AGPLv3 for mailbox, rationale.

**docs/adr/0002-crypto-libraries.md** — RustCrypto ecosystem, ed25519-dalek, rationale.

**docs/adr/0003-storage-approach.md** — rusqlite + app-level encryption, rationale.

## Important constraints

1. **Every file must have a license header comment** — GPLv3 for core/cli/tests, AGPLv3 for mailbox.
2. **All secret types must derive or implement `Zeroize`** — no raw secret bytes without zeroization.
3. **No `unwrap()` or `expect()` in library code** — use `?` and proper error types. Binaries may use `anyhow`.
4. **All public types and functions must have doc comments.**
5. **The project must compile and pass `cargo clippy -D warnings`** after scaffolding.
6. **Do not implement actual crypto, Tor, or MLS logic yet** — stub it. The goal is the architecture, not the implementation. Use `todo!()` for method bodies that need real work.
7. **Module visibility:** only `daemon`, `identity` (key types), `envelope`, `invite`, `contact`, `error` are public API. Everything else is `pub(crate)`.

## What success looks like

After running your output:
- `cargo build` succeeds
- `cargo clippy -D warnings` passes
- `cargo test` runs (tests may be empty but the harness works)
- The full module tree is visible and navigable
- A new developer can read ARCHITECTURE.md and understand where everything goes
- The CLI binary exists and `skattr --help` prints subcommands (even if they all say "not yet implemented")
