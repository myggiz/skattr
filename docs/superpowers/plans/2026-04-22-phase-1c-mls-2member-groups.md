# Phase 1.C MLS 2-Member Groups Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two peers form a 2-member MLS group, exchange application messages in both directions, and have their group state survive daemon restart. The 1.B `h_transport` is injected as an external PSK into the first MLS Commit.

**Architecture:** `crates/core/src/mls/` holds a thin `Group` wrapper over `openmls::group::MlsGroup`, backed by `openmls_rust_crypto::OpenMlsRustCrypto` as the provider. Persistence is checkpoint-snapshot: after every state-advancing call, we ciborium-serialize `provider.storage().values` (a public `RwLock<HashMap<Vec<u8>, Vec<u8>>>`) into the existing `mls_groups.state_blob` column. `GroupState` shrinks from six variants to `{Active, PendingJoin, Corrupt}`. External PSKs are registered on both sides before proposing the Add; shared identifier byte string is `b"skattr-binding-v1"` (matches the HKDF label from 1.B).

**Tech Stack:** Rust 2021, `openmls = "0.8"`, `openmls_traits = "0.5"`, `openmls_rust_crypto = "0.5"`, `ciborium` (snapshot + envelope), `sha2` (KeyPackage hash), `rusqlite` (existing pool, one new table in migration 0002), `tls_codec` (OpenMLS wire serialisation; transitive dep, already in Cargo.lock).

**Design spec:** `docs/superpowers/specs/2026-04-22-phase-1c-mls-2member-groups-design.md` — read this first.

---

## Pre-flight

```bash
cd /home/myggiz/development/skattr-phase-1c-mls-groups
. "$HOME/.cargo/env"

cargo build --workspace
cargo test --workspace --release
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

All four must pass before starting Task 1. The worktree is branched from `master` at `5b3fccb` (Phase 1.B merge); Phase 0 through 1.B state is fully in place, 133 tests passing.

**Cargo isn't on system PATH.** Every shell block assumes you've run `. "$HOME/.cargo/env"` once at the top of your shell session — the literal command to use is `. "$HOME/.cargo/env" && <command>`.

---

## File structure

```
crates/core/src/mls/ciphersuite.rs            NO CHANGE
crates/core/src/mls/mod.rs                    MODIFY:  drop keystore/welcome/commit from mod tree;
                                                       add provider + key_package; shrink re-exports
crates/core/src/mls/state_machine.rs          SHRINK:  GroupState::{Active{epoch}, PendingJoin,
                                                       Corrupt{reason}}
crates/core/src/mls/keystore.rs               DELETE:  superseded
crates/core/src/mls/welcome.rs                DELETE:  absorbed into group::join_from_welcome
crates/core/src/mls/commit.rs                 DELETE:  absorbed into group::{add_member,
                                                       advance_epoch, process_incoming_commit}
crates/core/src/mls/provider.rs               CREATE:  MlsProvider wrapping OpenMlsRustCrypto
crates/core/src/mls/key_package.rs            CREATE:  KeyPackage newtype
crates/core/src/mls/group.rs                  REWRITE: full Group impl (create_solo, add_member,
                                                       join_from_welcome, encrypt, decrypt,
                                                       process_incoming_commit, advance_epoch,
                                                       save, load) + inline tests

crates/core/src/storage/migrations/0002_key_packages.sql   CREATE
crates/core/src/storage/key_packages.rs       CREATE:  KeyPackageRepo
crates/core/src/storage/mod.rs                MODIFY:  declare + re-export key_packages

crates/core/src/lib.rs                        MODIFY:  test_exports += Group, KeyPackage,
                                                       KeyPackageRepo, mls_public_test_helpers
crates/tests/src/mls_pair.rs                  CREATE:  integration test
crates/tests/src/lib.rs                       MODIFY:  pub mod mls_pair (feature-gated)

CHANGELOG.md                                  MODIFY:  bullet under [Unreleased]
CLAUDE.md                                     MODIFY:  Repository-state paragraph one-liner
```

No workspace Cargo.toml edits — all deps (`openmls`, `openmls_rust_crypto`, `openmls_traits`, `ciborium`, `sha2`) are already in place.

---

## Task 1: Pre-flight + scaffolding cleanup

**Goal:** Confirm the worktree is green, shrink `GroupState` to three variants, delete the three obsolete stub files (`keystore.rs`, `welcome.rs`, `commit.rs`), and stub-replace `group.rs` and `mod.rs` so the crate still compiles. No logic yet — this establishes the new file layout.

**Files:**
- Modify: `crates/core/src/mls/state_machine.rs`
- Modify: `crates/core/src/mls/mod.rs`
- Modify: `crates/core/src/mls/group.rs` (stub rewrite)
- Delete: `crates/core/src/mls/keystore.rs`
- Delete: `crates/core/src/mls/welcome.rs`
- Delete: `crates/core/src/mls/commit.rs`

- [ ] **Step 1: Pre-flight check**

```bash
cargo build --workspace
cargo test --workspace --release
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

Expected: all four green. If any fails, STOP and report BLOCKED.

- [ ] **Step 2: Shrink `GroupState`**

Replace the entire contents of `crates/core/src/mls/state_machine.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Explicit group lifecycle states.
//!
//! MLS is stateful and unforgiving. Modeling the phases explicitly lets
//! us reject operations on a group that isn't in a valid state and gives
//! the UI something meaningful to display instead of raw OpenMLS errors.
//!
//! Phase 1.C restricts the enum to three variants. `PendingCommit`,
//! `CatchingUp`, and `Removed` land in 1.E (delivery) / Phase 2
//! (multi-member) when they're actually reachable.

/// Lifecycle of a single MLS group from our perspective.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupState {
    /// Steady-state: the group has an accepted epoch and we can send.
    Active {
        /// Current MLS epoch number.
        epoch: u64,
    },
    /// We have a Welcome but haven't processed it yet.
    PendingJoin,
    /// State is irrecoverably corrupt; only option is recreate.
    Corrupt {
        /// Human-readable, non-sensitive reason.
        reason: String,
    },
}

impl GroupState {
    /// Whether sends are permitted from this state.
    #[must_use]
    pub fn can_send(&self) -> bool {
        matches!(self, GroupState::Active { .. })
    }

    /// Whether the group is in any recoverable state.
    #[must_use]
    pub fn is_recoverable(&self) -> bool {
        !matches!(self, GroupState::Corrupt { .. })
    }
}
```

- [ ] **Step 3: Delete the three obsolete files**

```bash
rm crates/core/src/mls/keystore.rs
rm crates/core/src/mls/welcome.rs
rm crates/core/src/mls/commit.rs
```

- [ ] **Step 4: Replace `mls/mod.rs`**

Replace the entire contents of `crates/core/src/mls/mod.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! MLS (RFC 9420) integration.
//!
//! We use OpenMLS with a single locked ciphersuite (see [`ciphersuite`]).
//! Internally the module wraps `openmls::group::MlsGroup` and persists
//! its opaque state blobs through [`crate::storage::groups::MlsGroupRepo`].
//! Group lifecycle is driven by an explicit [`state_machine::GroupState`]
//! so that inconsistent states become unrepresentable rather than silently
//! corrupting MLS internals.

pub(crate) mod ciphersuite;
pub(crate) mod group;
pub(crate) mod key_package;
pub(crate) mod provider;
pub(crate) mod state_machine;

#[cfg(not(feature = "test-harness"))]
pub(crate) use ciphersuite::CIPHERSUITE;
#[cfg(not(feature = "test-harness"))]
pub(crate) use group::{CommitBytes, Group, GroupId, WelcomeBytes};
#[cfg(not(feature = "test-harness"))]
pub(crate) use key_package::KeyPackage;
#[cfg(not(feature = "test-harness"))]
pub(crate) use state_machine::GroupState;

#[cfg(feature = "test-harness")]
pub use ciphersuite::CIPHERSUITE;
#[cfg(feature = "test-harness")]
pub use group::{CommitBytes, Group, GroupId, WelcomeBytes};
#[cfg(feature = "test-harness")]
pub use key_package::KeyPackage;
#[cfg(feature = "test-harness")]
pub use state_machine::GroupState;
```

- [ ] **Step 5: Stub-replace `group.rs`**

Replace the entire contents of `crates/core/src/mls/group.rs` with a stub that compiles (real impl lands in Task 5+):

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Thin wrapper over `openmls::group::MlsGroup`.
//!
//! The only legitimate construction paths are [`Group::create_solo`] and
//! [`Group::join_from_welcome`]. All state-advancing operations update
//! [`GroupState`] so the UI / delivery layer can reject send attempts on
//! a group that isn't `Active`.

use crate::envelope::Envelope;
use crate::error::Result;
use crate::identity::IdentityKey;
use crate::mls::key_package::KeyPackage;
use crate::mls::state_machine::GroupState;

/// Opaque MLS group id (generated by OpenMLS).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GroupId(pub Vec<u8>);

/// Opaque Welcome blob (MLS wire-format bytes).
pub type WelcomeBytes = Vec<u8>;

/// Opaque Commit blob (MLS wire-format bytes).
pub type CommitBytes = Vec<u8>;

/// A single MLS group (1:1 conversations are 2-member groups).
pub struct Group {
    _private: (),
}

impl Group {
    /// Create a fresh single-member group.
    pub(crate) fn create_solo(
        _identity: &IdentityKey,
        _psk: Option<&[u8; 32]>,
    ) -> Result<Self> {
        todo!("Task 5")
    }

    /// Add an invitee via their KeyPackage. Produces (Welcome, Commit).
    pub(crate) fn add_member(
        &mut self,
        _invitee_kp: &KeyPackage,
        _psk: Option<&[u8; 32]>,
    ) -> Result<(WelcomeBytes, CommitBytes)> {
        todo!("Task 6")
    }

    /// Join from a received Welcome.
    pub(crate) fn join_from_welcome(
        _identity: &IdentityKey,
        _welcome: &[u8],
        _psk: Option<&[u8; 32]>,
    ) -> Result<Self> {
        todo!("Task 7")
    }

    /// Encrypt an [`Envelope`] as an MLS application message.
    pub(crate) fn encrypt(&mut self, _envelope: &Envelope) -> Result<Vec<u8>> {
        todo!("Task 8")
    }

    /// Decrypt an incoming MLS application message.
    pub(crate) fn decrypt(&mut self, _ciphertext: &[u8]) -> Result<Envelope> {
        todo!("Task 8")
    }

    /// Apply an incoming peer Commit.
    pub(crate) fn process_incoming_commit(&mut self, _commit: &[u8]) -> Result<()> {
        todo!("Task 10")
    }

    /// Build an empty self-Commit that ratchets our epoch forward.
    pub(crate) fn advance_epoch(&mut self) -> Result<Vec<u8>> {
        todo!("Task 10")
    }

    /// Persist current state via the MLS group repo.
    pub(crate) fn save(&self, _repo: &crate::storage::MlsGroupRepo) -> Result<()> {
        todo!("Task 5")
    }

    /// Restore from persisted state.
    pub(crate) fn load(
        _group_id: &GroupId,
        _repo: &crate::storage::MlsGroupRepo,
    ) -> Result<Option<Self>> {
        todo!("Task 5")
    }

    /// Group identifier.
    pub fn id(&self) -> &GroupId {
        todo!("Task 5")
    }

    /// Current epoch number.
    pub fn epoch(&self) -> u64 {
        todo!("Task 5")
    }

    /// Current lifecycle state.
    pub fn state(&self) -> &GroupState {
        todo!("Task 5")
    }
}
```

- [ ] **Step 6: Stub `provider.rs` and `key_package.rs`**

Create `crates/core/src/mls/provider.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! `OpenMlsRustCrypto` wrapper with snapshot / load for persistence.

use crate::error::Result;

/// Crypto + storage provider for OpenMLS. Uses `OpenMlsRustCrypto`
/// under the hood (in-memory `MemoryStorage`) plus a custom snapshot
/// path for persistence.
pub(crate) struct MlsProvider {
    _private: (),
}

impl MlsProvider {
    pub(crate) fn new() -> Self {
        todo!("Task 3")
    }

    pub(crate) fn snapshot(&self) -> Result<Vec<u8>> {
        todo!("Task 3")
    }

    pub(crate) fn load(_bytes: &[u8]) -> Result<Self> {
        todo!("Task 3")
    }
}
```

Create `crates/core/src/mls/key_package.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! MLS KeyPackage newtype.

use crate::error::Result;
use crate::identity::IdentityKey;
use crate::mls::provider::MlsProvider;

/// A fresh MLS KeyPackage. Generated by the invitee ahead of time,
/// consumed at most once by the inviter's [`Group::add_member`].
pub struct KeyPackage {
    _private: (),
}

impl KeyPackage {
    pub(crate) fn generate(
        _identity: &IdentityKey,
        _provider: &MlsProvider,
        _kp_repo: &crate::storage::KeyPackageRepo,
    ) -> Result<Self> {
        todo!("Task 4")
    }

    pub(crate) fn to_bytes(&self) -> Result<Vec<u8>> {
        todo!("Task 4")
    }

    pub(crate) fn from_bytes(_bytes: &[u8]) -> Result<Self> {
        todo!("Task 4")
    }

    pub(crate) fn hash(&self) -> [u8; 32] {
        todo!("Task 4")
    }
}
```

- [ ] **Step 7: Stub `KeyPackageRepo` in `storage/`**

Create `crates/core/src/storage/key_packages.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Repository for MLS KeyPackages (ours to publish + theirs to consume).

use crate::error::Result;
use crate::storage::Pool;

pub(crate) struct KeyPackageRepo<'p> {
    _pool: &'p Pool,
}

impl<'p> KeyPackageRepo<'p> {
    pub(crate) fn new(_pool: &'p Pool) -> Self {
        todo!("Task 2")
    }

    pub(crate) fn insert(
        &self,
        _hash: &[u8; 32],
        _bytes: &[u8],
        _direction: &str,
    ) -> Result<()> {
        todo!("Task 2")
    }

    pub(crate) fn get(&self, _hash: &[u8; 32]) -> Result<Option<(Vec<u8>, bool)>> {
        todo!("Task 2")
    }

    pub(crate) fn mark_consumed(&self, _hash: &[u8; 32]) -> Result<()> {
        todo!("Task 2")
    }
}
```

Modify `crates/core/src/storage/mod.rs`: add `pub(crate) mod key_packages;` in the module list, and in the re-export block (matching the existing `ContactRepo`/`MessageRepo`/`Pool` pattern), add:

```rust
pub(crate) use key_packages::KeyPackageRepo;
```

(Check the existing file for the exact indentation / grouping; follow the established order: `contacts`, `groups`, `key_packages`, `mailboxes`, `messages`, `outbox`, `pool`, `seen_messages`. Alphabetical.)

- [ ] **Step 8: Verify the crate builds**

```bash
cargo build --workspace
```

Expected: clean. Every stub compiles via `todo!()`; no tests exercise them yet.

- [ ] **Step 9: Verify clippy stays clean**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: clean. `todo!()` is allowed by workspace lints (see `CLAUDE.md`: "Use `todo!()`, never `unimplemented!()`").

- [ ] **Step 10: Commit**

```bash
git add crates/core/src/mls/ crates/core/src/storage/
git commit -m "$(cat <<'EOF'
mls: scaffolding cleanup — shrink GroupState; stub provider + key_package

Delete keystore.rs / welcome.rs / commit.rs (their logic moves into
group.rs in later tasks). Shrink GroupState to {Active, PendingJoin,
Corrupt} per the Phase 1.C design spec. Introduce provider.rs and
key_package.rs as stubs, plus storage/key_packages.rs stub for the
upcoming KeyPackageRepo. GroupId moves to group.rs and widens from
[u8; 32] to Vec<u8> to match OpenMLS's variable-length id bytes.
EOF
)"
```

---

## Task 2: Migration 0002 + `KeyPackageRepo`

**Goal:** Add the `key_packages` table via `0002_key_packages.sql` and implement `KeyPackageRepo::insert` / `get` / `mark_consumed` with unit tests. Migration runner is already multi-file-aware (Phase 0.D design) — adding a second numbered file just works.

**Files:**
- Create: `crates/core/src/storage/migrations/0002_key_packages.sql`
- Modify: `crates/core/src/storage/key_packages.rs`

- [ ] **Step 1: Write the migration SQL**

Create `crates/core/src/storage/migrations/0002_key_packages.sql`:

```sql
-- skattr schema migration 0002: MLS KeyPackages
--
-- Tracks KeyPackages we've generated for peers (direction = 'ours') and
-- KeyPackages we've received from peers (direction = 'theirs'; Phase 2
-- only). 1.C always inserts 'ours'. Single-use enforcement is 1.D's job.

UPDATE schema_version SET version = 2 WHERE version = 1;

CREATE TABLE IF NOT EXISTS key_packages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kp_hash BLOB NOT NULL UNIQUE,
    kp_bytes BLOB NOT NULL,
    direction TEXT NOT NULL CHECK (direction IN ('ours', 'theirs')),
    consumed INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_key_packages_hash ON key_packages(kp_hash);
CREATE INDEX IF NOT EXISTS idx_key_packages_direction ON key_packages(direction);
```

- [ ] **Step 2: Write the failing insert/get round-trip test**

Replace the stub in `crates/core/src/storage/key_packages.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Repository for MLS KeyPackages.
//!
//! For 1.C every row is `direction = 'ours'` (we generated the KP for a
//! peer to consume). `direction = 'theirs'` lands in Phase 2 when we
//! cache received KPs for out-of-order use. `consumed` is flipped by
//! 1.D's invite flow on successful single-use join; 1.C only persists.

use crate::error::{CoreError, Result};
use crate::storage::Pool;

pub(crate) struct KeyPackageRepo<'p> {
    pool: &'p Pool,
}

impl<'p> KeyPackageRepo<'p> {
    pub(crate) fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Insert a KeyPackage row. `direction` must be `"ours"` or
    /// `"theirs"` (CHECK constraint at the SQL layer).
    pub(crate) fn insert(
        &self,
        hash: &[u8; 32],
        bytes: &[u8],
        direction: &str,
    ) -> Result<()> {
        self.pool.with_mut(|c| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            c.execute(
                "INSERT INTO key_packages (kp_hash, kp_bytes, direction, consumed, created_at) \
                 VALUES (?1, ?2, ?3, 0, ?4)",
                rusqlite::params![hash, bytes, direction, now],
            )
            .map_err(|e| CoreError::Storage(format!("insert key_package: {e}")))?;
            Ok(())
        })
    }

    /// Return `(kp_bytes, consumed)` if the hash is known. `None` otherwise.
    pub(crate) fn get(&self, hash: &[u8; 32]) -> Result<Option<(Vec<u8>, bool)>> {
        self.pool.with(|c| {
            let result = c.query_row(
                "SELECT kp_bytes, consumed FROM key_packages WHERE kp_hash = ?1",
                rusqlite::params![hash],
                |r| {
                    let bytes: Vec<u8> = r.get(0)?;
                    let consumed: i64 = r.get(1)?;
                    Ok((bytes, consumed != 0))
                },
            );
            match result {
                Ok(row) => Ok(Some(row)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Storage(format!("get key_package: {e}"))),
            }
        })
    }

    /// Mark a KeyPackage consumed. Idempotent.
    pub(crate) fn mark_consumed(&self, hash: &[u8; 32]) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "UPDATE key_packages SET consumed = 1 WHERE kp_hash = ?1",
                rusqlite::params![hash],
            )
            .map_err(|e| CoreError::Storage(format!("mark_consumed: {e}")))?;
            Ok(())
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn insert_get_round_trip() {
        let pool = Pool::in_memory();
        let repo = KeyPackageRepo::new(&pool);
        let hash = [0xAAu8; 32];
        repo.insert(&hash, b"kp-bytes", "ours").unwrap();
        let got = repo.get(&hash).unwrap().unwrap();
        assert_eq!(got.0, b"kp-bytes");
        assert!(!got.1, "freshly inserted KP must not be consumed");
    }

    #[test]
    fn get_missing_returns_none() {
        let pool = Pool::in_memory();
        let repo = KeyPackageRepo::new(&pool);
        assert!(repo.get(&[0x99u8; 32]).unwrap().is_none());
    }

    #[test]
    fn mark_consumed_flips_flag() {
        let pool = Pool::in_memory();
        let repo = KeyPackageRepo::new(&pool);
        let hash = [0xBBu8; 32];
        repo.insert(&hash, b"kp", "ours").unwrap();
        repo.mark_consumed(&hash).unwrap();
        let (_bytes, consumed) = repo.get(&hash).unwrap().unwrap();
        assert!(consumed, "mark_consumed must flip the flag");
    }

    #[test]
    fn mark_consumed_is_idempotent() {
        let pool = Pool::in_memory();
        let repo = KeyPackageRepo::new(&pool);
        let hash = [0xCCu8; 32];
        repo.insert(&hash, b"kp", "ours").unwrap();
        repo.mark_consumed(&hash).unwrap();
        repo.mark_consumed(&hash).unwrap();
        let (_bytes, consumed) = repo.get(&hash).unwrap().unwrap();
        assert!(consumed);
    }

    #[test]
    fn direction_check_constraint_rejects_bogus_value() {
        let pool = Pool::in_memory();
        let repo = KeyPackageRepo::new(&pool);
        let hash = [0xDDu8; 32];
        let err = repo.insert(&hash, b"kp", "sideways").expect_err(
            "CHECK (direction IN ('ours', 'theirs')) must reject 'sideways'",
        );
        match err {
            CoreError::Storage(s) => assert!(s.contains("CHECK")),
            other => panic!("expected Storage, got {other:?}"),
        }
    }
}
```

- [ ] **Step 3: Run the tests — they must fail with "no such table: key_packages"**

```bash
cargo test -p skattr-core --lib storage::key_packages::tests
```

Expected: 5 tests fail, all with `no such table: key_packages` (the migration hasn't been picked up by the compiler yet because `include_str!` resolves at compile time, but the migrations runner only sees files that exist at build time — so `cargo build` should already be re-running. The actual failure cause may be different; investigate if you see something else).

- [ ] **Step 4: Verify the migrations runner picks up `0002_key_packages.sql`**

Open `crates/core/src/storage/migrations.rs` and confirm it iterates every `*.sql` file numerically in the `migrations/` directory. The existing code uses `include_str!` for the Phase 0.D migration:

```bash
grep -n "include_str\|migrations/" crates/core/src/storage/migrations.rs
```

If the runner uses `include_str!("migrations/0001_init.sql")` explicitly (not a directory scan), add a second `include_str!("migrations/0002_key_packages.sql")` line and extend the MIGRATIONS array. The exact code shape depends on what 0.D shipped — follow the pattern precisely. If unclear, read the full file and extend.

- [ ] **Step 5: Run the tests again — they must pass**

```bash
cargo test -p skattr-core --lib storage::key_packages::tests
```

Expected: 5 tests PASS.

- [ ] **Step 6: Verify clippy stays clean**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

Both must be clean. Run `cargo fmt --all` if fmt complains.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/storage/migrations/0002_key_packages.sql \
        crates/core/src/storage/key_packages.rs \
        crates/core/src/storage/migrations.rs
git commit -m "$(cat <<'EOF'
storage: migration 0002 + KeyPackageRepo

Add key_packages table (kp_hash unique, kp_bytes, direction CHECK,
consumed bool, created_at). Schema version bumps 1→2. KeyPackageRepo
provides insert / get / mark_consumed for the upcoming MLS group
flow. Five unit tests cover the happy path, missing keys, idempotent
mark_consumed, and the CHECK constraint rejecting bogus direction.
EOF
)"
```

---

## Task 3: `MlsProvider` — snapshot / load over `MemoryStorage.values`

**Goal:** Implement the `MlsProvider` wrapper. `new()` instantiates a fresh `OpenMlsRustCrypto`. `snapshot()` ciborium-serializes the provider's `MemoryStorage.values: RwLock<HashMap<Vec<u8>, Vec<u8>>>`. `load(bytes)` deserializes and installs the HashMap into a fresh provider. One round-trip test.

**Files:**
- Modify: `crates/core/src/mls/provider.rs`

**Key fact verified during plan research:** `openmls_rust_crypto::MemoryStorage` has a `pub values: RwLock<HashMap<Vec<u8>, Vec<u8>>>` field (source in `/home/myggiz/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/openmls_memory_storage-0.5.0/src/lib.rs:15`). We read/write the HashMap directly and serialize it with ciborium. No feature flags, no custom `StorageProvider` impl required.

- [ ] **Step 1: Write the failing round-trip test**

Replace the stub in `crates/core/src/mls/provider.rs` with the inline-test scaffold (implementation still `todo!()` for now):

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Crypto + storage provider for OpenMLS.
//!
//! Wraps `openmls_rust_crypto::OpenMlsRustCrypto`, which itself composes
//! `RustCrypto` (AEAD / hash / sig / HKDF / rand) with `MemoryStorage`
//! (in-process `HashMap<Vec<u8>, Vec<u8>>`). Persistence is
//! checkpoint-snapshot: after every state-advancing call on a group,
//! the caller serializes the provider via [`MlsProvider::snapshot`]
//! and writes the blob via `MlsGroupRepo`. On load, the reverse.
//!
//! The snapshot format is ciborium-encoded `HashMap<Vec<u8>, Vec<u8>>`
//! — deterministic enough across restarts, readable by anyone with the
//! schema, and fits the existing `mls_groups.state_blob` column.

use std::collections::HashMap;

use openmls_rust_crypto::OpenMlsRustCrypto;

use crate::error::{CoreError, Result};

/// Crypto + storage provider for OpenMLS.
pub(crate) struct MlsProvider {
    inner: OpenMlsRustCrypto,
}

impl MlsProvider {
    /// Fresh provider with empty in-memory storage.
    pub(crate) fn new() -> Self {
        Self {
            inner: OpenMlsRustCrypto::default(),
        }
    }

    /// Borrow the underlying OpenMLS provider.
    pub(crate) fn as_openmls(&self) -> &OpenMlsRustCrypto {
        &self.inner
    }

    /// Serialize the in-memory key-value storage into a ciborium blob.
    pub(crate) fn snapshot(&self) -> Result<Vec<u8>> {
        todo!("read MemoryStorage.values, ciborium-encode HashMap")
    }

    /// Rehydrate a provider from a ciborium snapshot.
    pub(crate) fn load(bytes: &[u8]) -> Result<Self> {
        todo!("ciborium-decode HashMap, install into MemoryStorage.values")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use openmls_traits::OpenMlsProvider as _;

    #[test]
    fn snapshot_load_preserves_stored_bytes() {
        let provider = MlsProvider::new();

        // Inject a known key/value pair by writing directly to the
        // MemoryStorage HashMap. Using a distinctive byte pattern so
        // we can verify it survives the round-trip.
        let key = b"skattr-provider-test-key".to_vec();
        let value = b"skattr-provider-test-value".to_vec();
        {
            let storage = provider.as_openmls().storage();
            let mut guard = storage.values.write().unwrap();
            guard.insert(key.clone(), value.clone());
        }

        let snapshot = provider.snapshot().unwrap();
        assert!(!snapshot.is_empty(), "snapshot must not be empty");

        let restored = MlsProvider::load(&snapshot).unwrap();
        let storage = restored.as_openmls().storage();
        let guard = storage.values.read().unwrap();
        let got = guard.get(&key).expect("key must survive snapshot round-trip");
        assert_eq!(got, &value);
    }

    #[test]
    fn snapshot_of_empty_provider_decodes_to_empty_provider() {
        let provider = MlsProvider::new();
        let snapshot = provider.snapshot().unwrap();
        let restored = MlsProvider::load(&snapshot).unwrap();
        let storage = restored.as_openmls().storage();
        let guard = storage.values.read().unwrap();
        assert!(guard.is_empty());
    }

    #[test]
    fn load_rejects_garbage_bytes() {
        let err = MlsProvider::load(&[0xFF, 0xFF, 0xFF, 0xFF]).expect_err(
            "load must reject non-ciborium bytes",
        );
        match err {
            CoreError::Mls(s) => assert!(s.starts_with("mls: load:")),
            other => panic!("expected CoreError::Mls, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run the tests — they must fail on `todo!()` or unresolved import**

```bash
cargo test -p skattr-core --lib mls::provider::tests
```

Expected: panic inside `todo!("read MemoryStorage.values, ciborium-encode HashMap")`.

- [ ] **Step 3: Implement `snapshot` and `load`**

Replace the two `todo!()` bodies in `crates/core/src/mls/provider.rs`:

```rust
    pub(crate) fn snapshot(&self) -> Result<Vec<u8>> {
        let storage = self.inner.storage();
        let guard = storage
            .values
            .read()
            .map_err(|_| CoreError::Mls("mls: snapshot: poisoned storage lock".into()))?;
        // Clone into an owned HashMap so we can drop the lock before
        // serializing. HashMap<Vec<u8>, Vec<u8>> is ciborium-friendly.
        let map: HashMap<Vec<u8>, Vec<u8>> = guard.clone();
        drop(guard);

        let mut out = Vec::new();
        ciborium::ser::into_writer(&map, &mut out)
            .map_err(|e| CoreError::Mls(format!("mls: snapshot: cbor encode: {e}")))?;
        Ok(out)
    }

    pub(crate) fn load(bytes: &[u8]) -> Result<Self> {
        let map: HashMap<Vec<u8>, Vec<u8>> = ciborium::de::from_reader(bytes)
            .map_err(|e| CoreError::Mls(format!("mls: load: cbor decode: {e}")))?;
        let provider = Self::new();
        {
            let storage = provider.inner.storage();
            let mut guard = storage
                .values
                .write()
                .map_err(|_| CoreError::Mls("mls: load: poisoned storage lock".into()))?;
            *guard = map;
        }
        Ok(provider)
    }
```

Also remove the unused `use openmls_traits::OpenMlsProvider as _;` import from the tests if clippy complains (or leave it — the tests use `.storage()` on the provider which is the `OpenMlsProvider` trait method).

- [ ] **Step 4: Run the tests — they must pass**

```bash
cargo test -p skattr-core --lib mls::provider::tests
```

Expected: 3 PASS.

- [ ] **Step 5: Verify clippy + fmt**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

Run `cargo fmt --all` if fmt complains; include the fix in the commit.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/mls/provider.rs
git commit -m "$(cat <<'EOF'
mls: MlsProvider snapshot/load via ciborium over MemoryStorage.values

openmls_rust_crypto's MemoryStorage exposes its inner HashMap as
`pub values: RwLock<HashMap<Vec<u8>, Vec<u8>>>`. snapshot() clones
+ ciborium-encodes the HashMap; load() reverses. Round-trip test
verifies a known (key, value) pair survives snapshot → load → read.
Empty-provider and garbage-bytes tests cover the edge cases.
EOF
)"
```

---

## Task 4: `KeyPackage` — generate, to_bytes, from_bytes, hash

**Goal:** Implement the `KeyPackage` newtype. `generate` builds a fresh `openmls::key_packages::KeyPackage` for the given identity, serializes it, inserts the row via `KeyPackageRepo`, and returns the newtype. `to_bytes` / `from_bytes` are TLS-codec serialization (OpenMLS's wire format). `hash` is SHA-256 of the serialized bytes.

**Files:**
- Modify: `crates/core/src/mls/key_package.rs`

- [ ] **Step 1: Write the failing tests**

Replace the stub in `crates/core/src/mls/key_package.rs` with the full implementation shape (body still `todo!()`) plus tests:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! MLS KeyPackage newtype.
//!
//! A KeyPackage binds an identity's signature key to an HPKE init key
//! and a set of capabilities + extensions. The inviter encrypts a
//! Welcome against the invitee's KeyPackage `init_key`. KPs are
//! single-use: 1.C persists them via `KeyPackageRepo`, 1.D enforces.

use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_traits::{signatures::SignerError, OpenMlsProvider};
use sha2::{Digest, Sha256};
use tls_codec::{Deserialize as _, Serialize as _};

use crate::error::{CoreError, Result};
use crate::identity::IdentityKey;
use crate::mls::ciphersuite::CIPHERSUITE;
use crate::mls::provider::MlsProvider;
use crate::storage::KeyPackageRepo;

/// Ciphersuite code-point as an `openmls::prelude::Ciphersuite`.
pub(crate) const MLS_CIPHERSUITE: Ciphersuite =
    Ciphersuite::MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519;

// Sanity: keep the module-level u16 constant and the openmls enum in sync.
const _: () = {
    assert!(CIPHERSUITE == MLS_CIPHERSUITE as u16);
};

/// A freshly-generated MLS KeyPackage, ready to be shared with a peer.
pub struct KeyPackage {
    inner: openmls::key_packages::KeyPackage,
}

impl KeyPackage {
    /// Generate a fresh KeyPackage bound to `identity` and register it
    /// with `provider`. Persists the package + its hash via `kp_repo`
    /// with `direction = "ours"` and `consumed = false`.
    pub(crate) fn generate(
        identity: &IdentityKey,
        provider: &MlsProvider,
        kp_repo: &KeyPackageRepo,
    ) -> Result<Self> {
        let signer = signer_from_identity(identity, provider)?;
        let credential_with_key = credential_with_key(identity, &signer);

        let kp_bundle = openmls::key_packages::KeyPackage::builder()
            .build(
                MLS_CIPHERSUITE,
                provider.as_openmls(),
                &signer,
                credential_with_key,
            )
            .map_err(|e| CoreError::Mls(format!("mls: key_package builder: {e}")))?;
        let kp = kp_bundle.key_package().clone();

        let bytes = kp
            .tls_serialize_detached()
            .map_err(|e| CoreError::Mls(format!("mls: key_package serialize: {e}")))?;
        let hash = sha256(&bytes);
        kp_repo.insert(&hash, &bytes, "ours")?;

        Ok(Self { inner: kp })
    }

    /// Serialize to TLS-codec wire bytes for transmission.
    pub(crate) fn to_bytes(&self) -> Result<Vec<u8>> {
        self.inner
            .tls_serialize_detached()
            .map_err(|e| CoreError::Mls(format!("mls: key_package serialize: {e}")))
    }

    /// Deserialize from TLS-codec wire bytes. Does NOT validate the
    /// signature or insert into any repo — callers do that on receipt.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let kp = openmls::key_packages::KeyPackageIn::tls_deserialize_exact(bytes)
            .map_err(|e| CoreError::Mls(format!("mls: key_package deserialize: {e}")))?;
        // KeyPackageIn → KeyPackage via `validate`; 1.C skips signature
        // validation (caller is trusted in tests; 1.D adds the check).
        let crypto = openmls_rust_crypto::OpenMlsRustCrypto::default();
        let kp = kp
            .validate(crypto.crypto(), ProtocolVersion::Mls10)
            .map_err(|e| CoreError::Mls(format!("mls: key_package validate: {e}")))?;
        Ok(Self { inner: kp })
    }

    /// 32-byte SHA-256 of the TLS-codec serialization.
    pub(crate) fn hash(&self) -> [u8; 32] {
        let bytes = self
            .inner
            .tls_serialize_detached()
            .expect("serialize of a validly-constructed KeyPackage never fails");
        sha256(&bytes)
    }

    /// Borrow the underlying OpenMLS type. Used by `Group::add_member`.
    pub(crate) fn as_openmls(&self) -> &openmls::key_packages::KeyPackage {
        &self.inner
    }
}

/// Construct an MLS SignatureKeyPair whose private/public halves match
/// `identity`. The returned signer is registered with `provider` via
/// `signer.store` so that OpenMLS can look it up later.
pub(crate) fn signer_from_identity(
    identity: &IdentityKey,
    provider: &MlsProvider,
) -> Result<SignatureKeyPair> {
    // We reuse the Ed25519 seed directly: the same byte string that
    // IdentityKey signs messages with.
    let secret_bytes = ed25519_private_bytes(identity);
    let public_bytes = identity.public().0.to_vec();
    let signer = SignatureKeyPair::from_raw(
        SignatureScheme::ED25519,
        secret_bytes.clone(),
        public_bytes,
    );
    signer
        .store(provider.as_openmls().storage())
        .map_err(|e: SignerError| CoreError::Mls(format!("mls: signer store: {e:?}")))?;
    Ok(signer)
}

/// Extract the raw Ed25519 private scalar bytes from an IdentityKey.
/// Implemented via a `pub(crate)` accessor added in Task 5 — for now,
/// a helper that goes through a throw-away `SigningKey` round-trip.
fn ed25519_private_bytes(identity: &IdentityKey) -> Vec<u8> {
    // IdentityKey stores the 32-byte seed. ed25519_dalek::SigningKey
    // expands that into the 64-byte expanded private key if asked,
    // but MLS's Ed25519 signer also uses the seed directly. Use the
    // new `pub(crate) fn ed25519_seed(&self) -> Vec<u8>` added in
    // identity/key.rs in Step 3 below.
    identity.ed25519_seed().to_vec()
}

/// Build a `CredentialWithKey` wrapping the identity's Ed25519 public
/// key inside a `BasicCredential` whose identity payload is the raw
/// public-key bytes (same payload on both sides, makes the ACL check
/// trivial — we ignore BasicCredential contents for auth, the
/// X25519-bound Noise handshake already did identity verification).
pub(crate) fn credential_with_key(
    identity: &IdentityKey,
    signer: &SignatureKeyPair,
) -> CredentialWithKey {
    let credential = BasicCredential::new(identity.public().0.to_vec());
    CredentialWithKey {
        credential: credential.into(),
        signature_key: signer.public().into(),
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::storage::Pool;

    fn setup() -> (IdentityKey, MlsProvider, Pool) {
        let id = IdentityKey::generate().unwrap();
        let provider = MlsProvider::new();
        let pool = Pool::in_memory();
        (id, provider, pool)
    }

    #[test]
    fn generate_persists_row_in_key_packages_repo() {
        let (id, provider, pool) = setup();
        let repo = KeyPackageRepo::new(&pool);
        let kp = KeyPackage::generate(&id, &provider, &repo).unwrap();

        let hash = kp.hash();
        let (bytes, consumed) = repo.get(&hash).unwrap().unwrap();
        assert!(!bytes.is_empty());
        assert!(!consumed);
    }

    #[test]
    fn to_bytes_from_bytes_round_trips() {
        let (id, provider, pool) = setup();
        let repo = KeyPackageRepo::new(&pool);
        let kp = KeyPackage::generate(&id, &provider, &repo).unwrap();

        let bytes = kp.to_bytes().unwrap();
        let restored = KeyPackage::from_bytes(&bytes).unwrap();
        assert_eq!(kp.hash(), restored.hash());
    }

    #[test]
    fn hash_is_stable_across_calls() {
        let (id, provider, pool) = setup();
        let repo = KeyPackageRepo::new(&pool);
        let kp = KeyPackage::generate(&id, &provider, &repo).unwrap();
        assert_eq!(kp.hash(), kp.hash());
    }

    #[test]
    fn distinct_identities_yield_distinct_hashes() {
        let (_, provider1, pool) = setup();
        let id2 = IdentityKey::generate().unwrap();
        let provider2 = MlsProvider::new();
        let repo = KeyPackageRepo::new(&pool);

        let id1 = IdentityKey::generate().unwrap();
        let kp1 = KeyPackage::generate(&id1, &provider1, &repo).unwrap();
        let kp2 = KeyPackage::generate(&id2, &provider2, &repo).unwrap();
        assert_ne!(kp1.hash(), kp2.hash());
    }

    #[test]
    fn from_bytes_rejects_garbage() {
        let err = KeyPackage::from_bytes(&[0u8, 1, 2, 3]).expect_err("must reject garbage");
        match err {
            CoreError::Mls(s) => assert!(s.starts_with("mls: key_package")),
            other => panic!("expected CoreError::Mls, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Add the `ed25519_seed` accessor to `IdentityKey`**

Open `crates/core/src/identity/key.rs`. Inside `impl IdentityKey`, just after `noise_static_public` (or wherever the crate-private accessors live), add:

```rust
    /// The raw 32-byte Ed25519 seed. Crate-private: used by the MLS
    /// module to construct an `openmls_basic_credential::SignatureKeyPair`
    /// that signs with the same key as our identity. Do NOT widen the
    /// visibility — wrapping code must go through typed identity APIs.
    pub(crate) fn ed25519_seed(&self) -> [u8; 32] {
        self.secret
    }
```

- [ ] **Step 3: Verify the `CIPHERSUITE` const_assert compiles**

The `const _: () = { assert!(CIPHERSUITE == MLS_CIPHERSUITE as u16); };` block in `key_package.rs` requires `Ciphersuite as u16` in const context. OpenMLS 0.8's `Ciphersuite` enum is `#[repr(u16)]`, so `as u16` is a free cast.

```bash
cargo build --workspace
```

Expected: clean build. If the const assert fails (`CIPHERSUITE != MLS_CIPHERSUITE as u16`), the two constants have drifted — stop and fix `ciphersuite.rs` (should be `0x0001`) or `MLS_CIPHERSUITE` (should be `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519`). Both map to IANA code-point 0x0001.

- [ ] **Step 4: Run the tests**

```bash
cargo test -p skattr-core --lib mls::key_package::tests
```

Expected: 5 PASS.

- [ ] **Step 5: Verify clippy + fmt**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

Both clean.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/mls/key_package.rs crates/core/src/identity/key.rs
git commit -m "$(cat <<'EOF'
mls: KeyPackage newtype + signer_from_identity bridge

KeyPackage wraps openmls::key_packages::KeyPackage and exposes
generate / to_bytes / from_bytes / hash. generate() builds a fresh
KP bound to the caller's IdentityKey, stores the signer in the
provider, and inserts a (hash, bytes, direction='ours', consumed=0)
row via KeyPackageRepo.

signer_from_identity constructs an openmls_basic_credential::
SignatureKeyPair from the IdentityKey's Ed25519 seed — same private
key signs MLS messages that signs Skattr messages. IdentityKey
grows a pub(crate) ed25519_seed() accessor for this bridge.
EOF
)"
```

---

## Task 5: `Group::create_solo` + `save` + `load` — no PSK

**Goal:** Alice creates a solo MLS group. Save + load round-trip through `MlsGroupRepo` works. No PSK wiring yet — Task 9 adds it.

**Files:**
- Modify: `crates/core/src/mls/group.rs`
- Modify: `crates/core/src/storage/groups.rs` (if `MlsGroupRepo` needs a signature change — see below)

- [ ] **Step 1: Write the failing test**

Replace the stub in `crates/core/src/mls/group.rs` with the full structure (methods still `todo!()` except where filled in below), plus an inline test module:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Thin wrapper over `openmls::group::MlsGroup`.

use openmls::prelude::*;
use tls_codec::{Deserialize as _, Serialize as _};

use crate::envelope::Envelope;
use crate::error::{CoreError, Result};
use crate::identity::IdentityKey;
use crate::mls::key_package::{
    credential_with_key, signer_from_identity, KeyPackage, MLS_CIPHERSUITE,
};
use crate::mls::provider::MlsProvider;
use crate::mls::state_machine::GroupState;
use crate::storage::MlsGroupRepo;

/// Opaque MLS group id (variable-length bytes generated by OpenMLS).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GroupId(pub Vec<u8>);

/// Opaque Welcome blob.
pub type WelcomeBytes = Vec<u8>;

/// Opaque Commit blob.
pub type CommitBytes = Vec<u8>;

/// A single MLS group from our perspective.
pub struct Group {
    id: GroupId,
    state: GroupState,
    provider: MlsProvider,
    inner: openmls::group::MlsGroup,
}

impl Group {
    /// Create a fresh single-member group.
    pub(crate) fn create_solo(
        identity: &IdentityKey,
        _psk: Option<&[u8; 32]>,
    ) -> Result<Self> {
        let provider = MlsProvider::new();
        let signer = signer_from_identity(identity, &provider)?;
        let cwk = credential_with_key(identity, &signer);

        let group_create_config = MlsGroupCreateConfig::builder()
            .ciphersuite(MLS_CIPHERSUITE)
            .use_ratchet_tree_extension(true)
            .build();

        let inner = openmls::group::MlsGroup::new(
            provider.as_openmls(),
            &signer,
            &group_create_config,
            cwk,
        )
        .map_err(|e| CoreError::Mls(format!("mls: builder: {e:?}")))?;

        let gid = GroupId(inner.group_id().to_vec());
        // PSK wiring lands in Task 9; ignored in Task 5.

        Ok(Self {
            id: gid,
            state: GroupState::Active { epoch: 0 },
            provider,
            inner,
        })
    }

    pub(crate) fn add_member(
        &mut self,
        _invitee_kp: &KeyPackage,
        _psk: Option<&[u8; 32]>,
    ) -> Result<(WelcomeBytes, CommitBytes)> {
        todo!("Task 6")
    }

    pub(crate) fn join_from_welcome(
        _identity: &IdentityKey,
        _welcome: &[u8],
        _psk: Option<&[u8; 32]>,
    ) -> Result<Self> {
        todo!("Task 7")
    }

    pub(crate) fn encrypt(&mut self, _envelope: &Envelope) -> Result<Vec<u8>> {
        todo!("Task 8")
    }

    pub(crate) fn decrypt(&mut self, _ciphertext: &[u8]) -> Result<Envelope> {
        todo!("Task 8")
    }

    pub(crate) fn process_incoming_commit(&mut self, _commit: &[u8]) -> Result<()> {
        todo!("Task 10")
    }

    pub(crate) fn advance_epoch(&mut self) -> Result<Vec<u8>> {
        todo!("Task 10")
    }

    /// Persist current state. Writes the `(group_id, state_blob, epoch)`
    /// row via `MlsGroupRepo::put`. `state_blob` is the ciborium-encoded
    /// provider snapshot (HashMap of all OpenMLS internal state).
    pub(crate) fn save(&self, repo: &MlsGroupRepo) -> Result<()> {
        let blob = self.provider.snapshot()?;
        repo.put(&self.id.0, &blob, self.inner.epoch().as_u64())
    }

    /// Restore from persisted state. Returns `None` if `group_id` is unknown.
    pub(crate) fn load(
        group_id: &GroupId,
        repo: &MlsGroupRepo,
    ) -> Result<Option<Self>> {
        let Some(blob) = repo.get(&group_id.0)? else {
            return Ok(None);
        };
        let provider = MlsProvider::load(&blob)?;
        let openmls_gid = openmls::prelude::GroupId::from_slice(&group_id.0);
        let inner = openmls::group::MlsGroup::load(
            provider.as_openmls().storage(),
            &openmls_gid,
        )
        .map_err(|e| CoreError::Mls(format!("mls: load: {e:?}")))?
        .ok_or_else(|| CoreError::Mls("mls: load: corrupt state blob".into()))?;

        Ok(Some(Self {
            id: group_id.clone(),
            state: GroupState::Active {
                epoch: inner.epoch().as_u64(),
            },
            provider,
            inner,
        }))
    }

    #[must_use]
    pub fn id(&self) -> &GroupId {
        &self.id
    }

    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.inner.epoch().as_u64()
    }

    #[must_use]
    pub fn state(&self) -> &GroupState {
        &self.state
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::storage::Pool;

    fn alice() -> IdentityKey {
        IdentityKey::generate().unwrap()
    }

    #[test]
    fn create_solo_is_active_at_epoch_0() {
        let id = alice();
        let g = Group::create_solo(&id, None).unwrap();
        assert_eq!(g.epoch(), 0);
        assert!(matches!(g.state(), GroupState::Active { epoch: 0 }));
        assert!(!g.id().0.is_empty(), "group id must be set");
    }

    #[test]
    fn save_load_round_trip_preserves_epoch_and_id() {
        let pool = Pool::in_memory();
        let repo = MlsGroupRepo::new(&pool);
        let id = alice();

        let g = Group::create_solo(&id, None).unwrap();
        let gid = g.id().clone();
        g.save(&repo).unwrap();

        drop(g);

        let restored = Group::load(&gid, &repo).unwrap().expect("must load");
        assert_eq!(restored.epoch(), 0);
        assert_eq!(restored.id(), &gid);
    }

    #[test]
    fn load_missing_group_returns_none() {
        let pool = Pool::in_memory();
        let repo = MlsGroupRepo::new(&pool);
        let gid = GroupId(vec![0x99; 32]);
        assert!(Group::load(&gid, &repo).unwrap().is_none());
    }

    #[test]
    fn load_rejects_corrupt_blob() {
        let pool = Pool::in_memory();
        let repo = MlsGroupRepo::new(&pool);
        let gid = GroupId(vec![0xAA; 32]);
        // Write a garbage blob directly.
        repo.put(&gid.0, &[0xFFu8; 32], 7).unwrap();
        let err = Group::load(&gid, &repo).expect_err("garbage must fail");
        match err {
            CoreError::Mls(s) => assert!(s.starts_with("mls: load") || s.starts_with("mls:")),
            other => panic!("expected CoreError::Mls, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Confirm `MlsGroupRepo::put` accepts `&[u8]` for `group_id`**

The existing `MlsGroupRepo::put(&self, group_id: &[u8], state_blob: &[u8], epoch: u64)` signature already takes `&[u8]` — no change needed. Verify by reading `crates/core/src/storage/groups.rs`.

- [ ] **Step 3: Run the tests — at least the first must pass; others may fail depending on Task 2's migration state**

```bash
cargo test -p skattr-core --lib mls::group::tests
```

Expected: `create_solo_is_active_at_epoch_0` PASS. Other tests pass if Tasks 2 + 3 are in place.

- [ ] **Step 4: Verify clippy + fmt**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

Both clean.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/mls/group.rs
git commit -m "$(cat <<'EOF'
mls: Group::create_solo + save + load (no PSK)

Group owns a GroupId, GroupState, MlsProvider, and openmls::MlsGroup.
create_solo uses MlsGroupCreateConfig with the locked ciphersuite +
ratchet_tree_extension, and returns the group in Active{epoch: 0}.

save snapshots the provider via ciborium and writes via MlsGroupRepo.
load reads the blob, reconstructs the provider, and rehydrates
MlsGroup via its own load() API. Four tests cover the happy path,
round-trip, missing-group, and corrupt-blob cases.
EOF
)"
```

---

## Task 6: `Group::add_member` (no PSK)

**Goal:** Alice's `add_member(bob_kp, None)` produces (Welcome, Commit) byte blobs and bumps Alice's epoch from 0 to 1. `merge_pending_commit` is called immediately — 2-member has no race.

**Files:**
- Modify: `crates/core/src/mls/group.rs`

- [ ] **Step 1: Write the failing test**

Append inside the `mod tests` block in `crates/core/src/mls/group.rs`:

```rust
    #[test]
    fn add_member_emits_welcome_and_commit_and_bumps_epoch_to_1() {
        let pool = Pool::in_memory();
        let kp_repo = crate::storage::KeyPackageRepo::new(&pool);

        let alice_id = alice();
        let bob_id = IdentityKey::generate().unwrap();
        let bob_provider = MlsProvider::new();
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo).unwrap();

        let mut alice = Group::create_solo(&alice_id, None).unwrap();
        assert_eq!(alice.epoch(), 0);

        let (welcome, commit) = alice.add_member(&bob_kp, None).unwrap();
        assert!(!welcome.is_empty());
        assert!(!commit.is_empty());
        assert_eq!(alice.epoch(), 1);
        assert!(matches!(alice.state(), GroupState::Active { epoch: 1 }));
    }
```

- [ ] **Step 2: Run — expect `todo!()` panic**

```bash
cargo test -p skattr-core --lib mls::group::tests::add_member_emits_welcome_and_commit_and_bumps_epoch_to_1
```

Expected: panic inside `todo!("Task 6")`.

- [ ] **Step 3: Implement `add_member`**

Replace the `todo!("Task 6")` body in `Group::add_member` with:

```rust
    pub(crate) fn add_member(
        &mut self,
        invitee_kp: &KeyPackage,
        _psk: Option<&[u8; 32]>,
    ) -> Result<(WelcomeBytes, CommitBytes)> {
        // Guard against 3rd member (2-member only for 1.C). The test
        // for this lands in Task 11; checking here means Task 11 is
        // assertion-only.
        if self.inner.members().count() >= 2 {
            return Err(CoreError::Mls("mls: add_member: already 2-member".into()));
        }

        // PSK proposal (lands in Task 9). In Task 6 we just skip it.

        // Re-derive the signer for this identity. OpenMLS requires the
        // signer to be passed in on every state-advancing call; we
        // don't keep it inside `Group` because a signer is logically
        // tied to an identity, not a group.
        //
        // For Task 6 we get the signer from the provider's storage.
        // `SignatureKeyPair::read(storage, public_key_bytes)` rebuilds
        // the signer from its stored private half.
        let signer = load_signer(&self.provider, &own_public_key(&self.inner)?)?;

        let (commit_out, welcome_out, _group_info) = self
            .inner
            .add_members(
                self.provider.as_openmls(),
                &signer,
                &[invitee_kp.as_openmls().clone()],
            )
            .map_err(|e| CoreError::Mls(format!("mls: add_members: {e:?}")))?;

        // Merge the staged Commit *before* shipping — 2-member has no
        // conflicting proposal source. Task 11 adds the guard that
        // rejects a 3rd member; Phase 2's PendingCommit state removes
        // this eager-merge.
        self.inner
            .merge_pending_commit(self.provider.as_openmls())
            .map_err(|e| CoreError::Mls(format!("mls: merge_pending_commit: {e:?}")))?;

        let welcome_bytes = welcome_out
            .tls_serialize_detached()
            .map_err(|e| CoreError::Mls(format!("mls: welcome serialize: {e}")))?;
        let commit_bytes = commit_out
            .tls_serialize_detached()
            .map_err(|e| CoreError::Mls(format!("mls: commit serialize: {e}")))?;

        self.state = GroupState::Active {
            epoch: self.inner.epoch().as_u64(),
        };
        Ok((welcome_bytes, commit_bytes))
    }
```

- [ ] **Step 4: Add the `load_signer` and `own_public_key` helpers**

At the bottom of `crates/core/src/mls/group.rs` (module scope, below the `impl Group`), add:

```rust
/// Re-materialize the `SignatureKeyPair` stored in this provider by
/// public key. OpenMLS state-advancing calls require a `&impl Signer`;
/// we reconstruct it from storage rather than threading a long-lived
/// reference through `Group`.
fn load_signer(
    provider: &MlsProvider,
    public_key: &[u8],
) -> Result<openmls_basic_credential::SignatureKeyPair> {
    openmls_basic_credential::SignatureKeyPair::read(
        provider.as_openmls().storage(),
        public_key,
        openmls::prelude::SignatureScheme::ED25519.into(),
    )
    .ok_or_else(|| CoreError::Mls("mls: load_signer: missing signer".into()))
}

/// Extract our own signature public key from the group. In a 2-member
/// scenario we always have exactly one leaf (ours before add_member,
/// first leaf after). Caller must not invoke post-Remove.
fn own_public_key(group: &openmls::group::MlsGroup) -> Result<Vec<u8>> {
    let own_leaf = group
        .own_leaf()
        .ok_or_else(|| CoreError::Mls("mls: own_public_key: no own leaf".into()))?;
    Ok(own_leaf.signature_key().as_slice().to_vec())
}
```

- [ ] **Step 5: Run the test**

```bash
cargo test -p skattr-core --lib mls::group::tests::add_member_emits_welcome_and_commit_and_bumps_epoch_to_1
```

Expected: PASS.

- [ ] **Step 6: Verify clippy + fmt**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

Both clean.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/mls/group.rs
git commit -m "$(cat <<'EOF'
mls: Group::add_member emits (Welcome, Commit) and merges eagerly

add_member accepts a KeyPackage, runs MlsGroup::add_members, merges
the pending commit immediately (safe for 2-member; multi-member
Phase 2 will gate this behind the PendingCommit state), and returns
the TLS-serialized Welcome + Commit bytes. Epoch advances 0→1.
Guards against 3rd member via members().count() >= 2 check.

load_signer + own_public_key helpers re-materialize the signer from
provider storage on every state-advancing call — keeps Group from
carrying a long-lived reference to the signer.
EOF
)"
```

---

## Task 7: `Group::join_from_welcome` (no PSK)

**Goal:** Bob receives Alice's Welcome + joins the group. Post-join, Bob's group is at epoch 1 with the same GroupId as Alice's.

**Files:**
- Modify: `crates/core/src/mls/group.rs`

- [ ] **Step 1: Write the failing test**

Append inside `mod tests`:

```rust
    #[test]
    fn join_from_welcome_lands_at_epoch_1_with_matching_group_id() {
        let pool = Pool::in_memory();
        let kp_repo = crate::storage::KeyPackageRepo::new(&pool);

        let alice_id = alice();
        let bob_id = IdentityKey::generate().unwrap();
        let bob_provider = MlsProvider::new();
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo).unwrap();

        let mut alice = Group::create_solo(&alice_id, None).unwrap();
        let (welcome, _commit) = alice.add_member(&bob_kp, None).unwrap();

        let bob = Group::join_from_welcome(&bob_id, &welcome, None).unwrap();

        assert_eq!(bob.epoch(), 1);
        assert_eq!(bob.id(), alice.id(), "both sides see the same group id");
        assert!(matches!(bob.state(), GroupState::Active { epoch: 1 }));
    }
```

**Important:** This test creates a fresh `MlsProvider` for Bob inside `join_from_welcome`. Bob's `bob_provider` (used to generate the KeyPackage) holds the PRIVATE key material that matches `bob_kp.init_key`. If `join_from_welcome` creates yet another provider, Bob can't decrypt the Welcome — he doesn't have the private half.

**Resolution:** `join_from_welcome` must take the provider as an argument OR the caller must have registered the KeyPackage's private half into the same provider before calling. The cleanest fit is:

- `Group::join_from_welcome(identity, welcome, psk, provider)` — caller owns the provider (the same one used to generate the KP).

Revise the public signature accordingly. Rewrite the test to pass `bob_provider` through:

```rust
    #[test]
    fn join_from_welcome_lands_at_epoch_1_with_matching_group_id() {
        let pool = Pool::in_memory();
        let kp_repo = crate::storage::KeyPackageRepo::new(&pool);

        let alice_id = alice();
        let bob_id = IdentityKey::generate().unwrap();
        let bob_provider = MlsProvider::new();
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo).unwrap();

        let mut alice = Group::create_solo(&alice_id, None).unwrap();
        let (welcome, _commit) = alice.add_member(&bob_kp, None).unwrap();

        let bob = Group::join_from_welcome(&bob_id, &welcome, None, bob_provider).unwrap();

        assert_eq!(bob.epoch(), 1);
        assert_eq!(bob.id(), alice.id());
        assert!(matches!(bob.state(), GroupState::Active { epoch: 1 }));
    }
```

And update `Group::create_solo` similarly for symmetry — except `create_solo` generates a brand-new private signing key, so it can own its provider. The asymmetry is fine: `create_solo` owns its provider, `join_from_welcome` takes one.

Actually — symmetry is cleaner. Revise both to take an owned provider:

```rust
pub(crate) fn create_solo(
    identity: &IdentityKey,
    psk: Option<&[u8; 32]>,
    provider: MlsProvider,
) -> Result<Self>;

pub(crate) fn join_from_welcome(
    identity: &IdentityKey,
    welcome: &[u8],
    psk: Option<&[u8; 32]>,
    provider: MlsProvider,
) -> Result<Self>;
```

Existing tests from Task 5 + 6 pass `MlsProvider::new()` in as the third argument. Let me revise those too — apply this as part of Task 7.

- [ ] **Step 2: Run — panic on `todo!()`**

```bash
cargo test -p skattr-core --lib mls::group::tests::join_from_welcome_lands_at_epoch_1_with_matching_group_id
```

Expected: compile error (sig mismatch — `Group::create_solo` needs the extra argument).

- [ ] **Step 3: Update `create_solo` + `add_member` tests + implement `join_from_welcome`**

Modify `Group::create_solo`'s signature in `crates/core/src/mls/group.rs`:

```rust
    pub(crate) fn create_solo(
        identity: &IdentityKey,
        _psk: Option<&[u8; 32]>,
        provider: MlsProvider,
    ) -> Result<Self> {
        let signer = signer_from_identity(identity, &provider)?;
        // ... rest as before, just replace `let provider = MlsProvider::new();` with the argument
```

Update every call site in the existing tests (`Group::create_solo(&id, None)` → `Group::create_solo(&id, None, MlsProvider::new())`).

Implement `join_from_welcome`:

```rust
    pub(crate) fn join_from_welcome(
        identity: &IdentityKey,
        welcome: &[u8],
        _psk: Option<&[u8; 32]>,
        provider: MlsProvider,
    ) -> Result<Self> {
        // Make sure the signer from our identity is registered in this
        // provider so OpenMLS can verify our own leaf in later ops.
        let _ = signer_from_identity(identity, &provider)?;

        let welcome_msg = MlsMessageIn::tls_deserialize_exact(welcome)
            .map_err(|e| CoreError::Mls(format!("mls: welcome deserialize: {e}")))?;
        let welcome = match welcome_msg.extract() {
            MlsMessageBodyIn::Welcome(w) => w,
            _ => return Err(CoreError::Mls("mls: welcome: wrong message type".into())),
        };

        // Ciphersuite check (spec exit criterion).
        if welcome.ciphersuite() != MLS_CIPHERSUITE {
            return Err(CoreError::Mls("mls: welcome: ciphersuite mismatch".into()));
        }

        let join_config = MlsGroupJoinConfig::builder()
            .use_ratchet_tree_extension(true)
            .build();

        let staged = StagedWelcome::new_from_welcome(
            provider.as_openmls(),
            &join_config,
            welcome,
            None,
        )
        .map_err(|e| CoreError::Mls(format!("mls: welcome process: {e:?}")))?;

        let inner = staged
            .into_group(provider.as_openmls())
            .map_err(|e| CoreError::Mls(format!("mls: welcome into_group: {e:?}")))?;

        let gid = GroupId(inner.group_id().to_vec());
        let epoch = inner.epoch().as_u64();
        Ok(Self {
            id: gid,
            state: GroupState::Active { epoch },
            provider,
            inner,
        })
    }
```

- [ ] **Step 4: Also update the Task 6 `add_member` test to pass `MlsProvider::new()` into the `create_solo` call**

Open `crates/core/src/mls/group.rs` `mod tests`; in `add_member_emits_welcome_and_commit_and_bumps_epoch_to_1`, change:

```rust
        let mut alice = Group::create_solo(&alice_id, None).unwrap();
```

to:

```rust
        let mut alice = Group::create_solo(&alice_id, None, MlsProvider::new()).unwrap();
```

Do the same in the Task 5 tests (`create_solo_is_active_at_epoch_0`, `save_load_round_trip_preserves_epoch_and_id`).

- [ ] **Step 5: Run the full mls test module**

```bash
cargo test -p skattr-core --lib mls::
```

Expected: all tests pass. If `Group::load` breaks because it also constructs its own provider, that's fine — `load` reads the provider bytes from disk and constructs a fresh provider-from-bytes, so it doesn't accept an external one. Keep as-is.

- [ ] **Step 6: Verify clippy + fmt**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

Both clean.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/mls/group.rs
git commit -m "$(cat <<'EOF'
mls: Group::join_from_welcome + provider ownership symmetry

join_from_welcome takes an MlsProvider (the invitee's provider used
to generate their KeyPackage — it holds the init_key private half).
Deserializes the Welcome, validates ciphersuite matches the locked
0x0001, and stages + commits via StagedWelcome.

create_solo also takes the provider as an argument for API symmetry;
all existing tests updated to pass MlsProvider::new() through.

Post-join, Bob's group is Active{epoch: 1} and Bob.id() == alice.id().
EOF
)"
```

---

## Task 8: Bidirectional `encrypt` + `decrypt`

**Goal:** Both sides of the group can encrypt Envelopes as MLS application messages and decrypt incoming ones.

**Files:**
- Modify: `crates/core/src/mls/group.rs`

- [ ] **Step 1: Write the failing test**

Append inside `mod tests`:

```rust
    fn pair_no_psk() -> (Group, Group) {
        let pool = Pool::in_memory();
        let kp_repo = crate::storage::KeyPackageRepo::new(&pool);
        let alice_id = alice();
        let bob_id = IdentityKey::generate().unwrap();
        let bob_provider = MlsProvider::new();
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo).unwrap();
        let mut alice = Group::create_solo(&alice_id, None, MlsProvider::new()).unwrap();
        let (welcome, _commit) = alice.add_member(&bob_kp, None).unwrap();
        let bob = Group::join_from_welcome(&bob_id, &welcome, None, bob_provider).unwrap();
        (alice, bob)
    }

    fn test_envelope(text: &str) -> Envelope {
        use crate::envelope::{Kind, MessageId};
        Envelope {
            v: 1,
            id: MessageId::generate(),
            ts: 0,
            reply_to: None,
            kind: Kind::Text {
                body: text.to_string(),
            },
        }
    }

    #[test]
    fn bidirectional_encrypt_decrypt() {
        let (mut alice, mut bob) = pair_no_psk();

        let msg_a = test_envelope("hi from alice");
        let ct_a = alice.encrypt(&msg_a).unwrap();
        let got_a = bob.decrypt(&ct_a).unwrap();
        assert_eq!(format!("{got_a:?}"), format!("{msg_a:?}"));

        let msg_b = test_envelope("hi from bob");
        let ct_b = bob.encrypt(&msg_b).unwrap();
        let got_b = alice.decrypt(&ct_b).unwrap();
        assert_eq!(format!("{got_b:?}"), format!("{msg_b:?}"));
    }
```

Check what `Kind` looks like in `crates/core/src/envelope/kinds.rs`:

```bash
cat crates/core/src/envelope/kinds.rs
```

If `Kind::Text { body: String }` doesn't exist (e.g. `Kind` is a different shape), adapt the test to use whatever variant DOES exist with a minimal payload. `Envelope` derives `Debug + Clone + Serialize + Deserialize` already.

- [ ] **Step 2: Run — panic on `todo!()`**

```bash
cargo test -p skattr-core --lib mls::group::tests::bidirectional_encrypt_decrypt
```

Expected: panic in `todo!("Task 8")`.

- [ ] **Step 3: Implement `encrypt` + `decrypt`**

Replace the two `todo!("Task 8")` bodies:

```rust
    pub(crate) fn encrypt(&mut self, envelope: &Envelope) -> Result<Vec<u8>> {
        if !self.state.can_send() {
            return Err(CoreError::Mls(format!(
                "mls: encrypt: invalid state {:?}",
                self.state
            )));
        }

        let signer = load_signer(&self.provider, &own_public_key(&self.inner)?)?;
        let plaintext = envelope.encode()?;

        let out = self
            .inner
            .create_message(self.provider.as_openmls(), &signer, &plaintext)
            .map_err(|e| CoreError::Mls(format!("mls: encrypt: {e:?}")))?;

        out.tls_serialize_detached()
            .map_err(|e| CoreError::Mls(format!("mls: encrypt: serialize: {e}")))
    }

    pub(crate) fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Envelope> {
        if !self.state.can_send() {
            return Err(CoreError::Mls(format!(
                "mls: decrypt: invalid state {:?}",
                self.state
            )));
        }

        let msg_in = MlsMessageIn::tls_deserialize_exact(ciphertext)
            .map_err(|e| CoreError::Mls(format!("mls: decrypt: deserialize: {e}")))?;
        let protocol_message: ProtocolMessage = match msg_in.extract() {
            MlsMessageBodyIn::PrivateMessage(pm) => pm.into(),
            MlsMessageBodyIn::PublicMessage(pm) => pm.into(),
            _ => return Err(CoreError::Mls("mls: decrypt: unsupported message type".into())),
        };

        let processed = self
            .inner
            .process_message(self.provider.as_openmls(), protocol_message)
            .map_err(|e| CoreError::Mls(format!("mls: authentication failed: {e:?}")))?;

        let plaintext_bytes = match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(app) => app.into_bytes(),
            ProcessedMessageContent::StagedCommitMessage(_) => {
                return Err(CoreError::Mls(
                    "mls: decrypt: received Commit on encrypt path — route to process_incoming_commit"
                        .into(),
                ));
            }
            ProcessedMessageContent::ProposalMessage(_) => {
                return Err(CoreError::Mls("mls: decrypt: received Proposal".into()));
            }
            ProcessedMessageContent::ExternalJoinProposalMessage(_) => {
                return Err(CoreError::Mls(
                    "mls: decrypt: received ExternalJoinProposal".into(),
                ));
            }
        };

        Envelope::decode(&plaintext_bytes)
    }
```

- [ ] **Step 4: Run the test**

```bash
cargo test -p skattr-core --lib mls::group::tests::bidirectional_encrypt_decrypt
```

Expected: PASS. Both `Envelope` instances round-trip byte-equal (via `format!("{:?}")` as a structural equality proxy since `Envelope` lacks `PartialEq`).

- [ ] **Step 5: Verify clippy + fmt**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/mls/group.rs
git commit -m "$(cat <<'EOF'
mls: Group::encrypt + decrypt via MLS application messages

encrypt encodes the Envelope as CBOR, calls MlsGroup::create_message
with the loaded signer, and tls_serializes the MlsMessageOut.
decrypt inverts: deserialize, extract Private/PublicMessage into a
ProtocolMessage, process_message, and decode the application bytes
back to Envelope. State-guard: encrypt/decrypt reject non-Active
states with "mls: encrypt: invalid state ..." / "mls: decrypt: ...".
EOF
)"
```

---

## Task 9: External PSK injection

**Goal:** When `create_solo` / `add_member` / `join_from_welcome` receive `Some(psk_bytes)`, the PSK is registered with the provider and proposed into the first Commit. Matching PSKs succeed; mismatched fail with `"mls: external PSK mismatch"` (or an authentication-failure variant).

**Files:**
- Modify: `crates/core/src/mls/group.rs`

- [ ] **Step 1: Write the happy-path PSK test**

Append inside `mod tests`:

```rust
    fn pair_with_psk(psk_alice: [u8; 32], psk_bob: [u8; 32]) -> Result<(Group, Group)> {
        let pool = Pool::in_memory();
        let kp_repo = crate::storage::KeyPackageRepo::new(&pool);
        let alice_id = alice();
        let bob_id = IdentityKey::generate().unwrap();
        let bob_provider = MlsProvider::new();
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo)?;
        let mut alice = Group::create_solo(&alice_id, Some(&psk_alice), MlsProvider::new())?;
        let (welcome, _commit) = alice.add_member(&bob_kp, Some(&psk_alice))?;
        let bob = Group::join_from_welcome(&bob_id, &welcome, Some(&psk_bob), bob_provider)?;
        Ok((alice, bob))
    }

    #[test]
    fn external_psk_match_succeeds() {
        let psk = [0xEEu8; 32];
        let (alice, bob) = pair_with_psk(psk, psk).unwrap();
        assert_eq!(alice.epoch(), 1);
        assert_eq!(bob.epoch(), 1);
        assert_eq!(alice.id(), bob.id());
    }

    #[test]
    fn external_psk_mismatch_fails_on_bob_join() {
        let result = pair_with_psk([0xAAu8; 32], [0xBBu8; 32]);
        let err = result.expect_err("mismatched PSK must fail");
        match err {
            CoreError::Mls(s) => {
                assert!(
                    s.contains("external PSK mismatch")
                        || s.contains("authentication")
                        || s.contains("welcome process"),
                    "unexpected message: {s}"
                );
            }
            other => panic!("expected CoreError::Mls, got {other:?}"),
        }
    }

    #[test]
    fn no_psk_path_still_works() {
        let pool = Pool::in_memory();
        let kp_repo = crate::storage::KeyPackageRepo::new(&pool);
        let alice_id = alice();
        let bob_id = IdentityKey::generate().unwrap();
        let bob_provider = MlsProvider::new();
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo).unwrap();
        let mut alice = Group::create_solo(&alice_id, None, MlsProvider::new()).unwrap();
        let (welcome, _commit) = alice.add_member(&bob_kp, None).unwrap();
        let bob = Group::join_from_welcome(&bob_id, &welcome, None, bob_provider).unwrap();
        assert_eq!(alice.epoch(), 1);
        assert_eq!(bob.epoch(), 1);
    }
```

- [ ] **Step 2: Run — expect `external_psk_match_succeeds` to fail (no PSK wiring yet)**

```bash
cargo test -p skattr-core --lib mls::group::tests::external_psk_match_succeeds
```

Expected: PASS or FAIL — depends on whether the current `create_solo` / `add_member` / `join_from_welcome` silently ignore the `psk` argument. Without PSK wiring, both sides still reach epoch 1 without ever binding the PSK — the "match succeeds" test is actually a degenerate pass. The real signal is `external_psk_mismatch_fails_on_bob_join`, which will currently pass even with mismatched PSKs (because neither is registered) — that's a semantic bug we fix below.

- [ ] **Step 3: Implement PSK registration helpers**

At the bottom of `crates/core/src/mls/group.rs` (module scope), add:

```rust
/// The external-PSK identifier byte string. Matches the HKDF label from
/// 1.B so the two layers use the same constant. Both sides must register
/// a PSK under this identifier for the Add Commit's PSK proposal to
/// decrypt on the other side.
const PSK_ID_BYTES: &[u8] = b"skattr-binding-v1";

/// Register a 32-byte external PSK with the provider under the
/// canonical Skattr PSK identifier. Returns a `PreSharedKeyId` that
/// the caller embeds in the Commit's PSK proposal (inviter side) or
/// that gets derived from the Welcome (invitee side).
fn register_external_psk(
    provider: &MlsProvider,
    psk: &[u8; 32],
) -> Result<PreSharedKeyId> {
    let psk_id = PreSharedKeyId::external(PSK_ID_BYTES.to_vec(), vec![0u8; 32]);
    psk_id
        .store(provider.as_openmls(), psk)
        .map_err(|e| CoreError::Mls(format!("mls: psk register: {e:?}")))?;
    Ok(psk_id)
}
```

Note: the nonce in `PreSharedKeyId::external` is per-application, not per-registration — any 32-byte value works on Bob's side since his `PreSharedKeyId` won't be used in a Commit (he's processing one, not creating it). On Alice's side the nonce gets re-rolled inside `propose_external_psk_by_value` (via `PreSharedKeyId::new` internally — OpenMLS generates a fresh random nonce on every proposal). The `store` call is keyed only on the `Psk` variant, so the nonce we pass here is ignored for storage purposes.

- [ ] **Step 4: Wire PSK into `create_solo`**

Replace `_psk: Option<&[u8; 32]>` with `psk: Option<&[u8; 32]>` in `create_solo`, and after `MlsGroup::new(...)` succeeds, add:

```rust
        if let Some(psk_bytes) = psk {
            register_external_psk(&provider, psk_bytes)?;
        }
```

The register call returns a `PreSharedKeyId` we don't use here — the PSK is consumed when Alice builds the Add Commit (Task 9 Step 5). `register_external_psk`'s side effect (writing the secret to provider storage) is the important part.

- [ ] **Step 5: Wire PSK into `add_member`**

Replace `_psk: Option<&[u8; 32]>` with `psk: Option<&[u8; 32]>` in `add_member`. After the `self.inner.members().count() >= 2` guard and before the `load_signer` line, add:

```rust
        // If a PSK is present, register it (idempotent with create_solo's
        // registration) and propose the external-PSK reference into the
        // pending-proposals queue. `add_members` below will commit the
        // whole queue (Add + PSK) in a single Commit.
        if let Some(psk_bytes) = psk {
            let psk_id = register_external_psk(&self.provider, psk_bytes)?;
            let signer = load_signer(&self.provider, &own_public_key(&self.inner)?)?;
            self.inner
                .propose_external_psk(self.provider.as_openmls(), &signer, psk_id)
                .map_err(|e| CoreError::Mls(format!("mls: propose external psk: {e:?}")))?;
        }
```

- [ ] **Step 6: Wire PSK into `join_from_welcome`**

Replace `_psk: Option<&[u8; 32]>` with `psk: Option<&[u8; 32]>` in `join_from_welcome`. BEFORE `StagedWelcome::new_from_welcome(...)`, register the PSK if present:

```rust
        if let Some(psk_bytes) = psk {
            register_external_psk(&provider, psk_bytes)?;
        }
```

If the Welcome references the PSK and Bob's registered PSK doesn't match, `StagedWelcome::new_from_welcome` will fail when it tries to derive the joiner secret — surfacing as `"mls: welcome process: ..."`. Our error assertion in `external_psk_mismatch_fails_on_bob_join` matches on any of `"external PSK mismatch"`, `"authentication"`, or `"welcome process"` to accommodate whichever exact error OpenMLS emits.

- [ ] **Step 7: Run all PSK tests**

```bash
cargo test -p skattr-core --lib mls::group::tests::external_psk_match_succeeds
cargo test -p skattr-core --lib mls::group::tests::external_psk_mismatch_fails_on_bob_join
cargo test -p skattr-core --lib mls::group::tests::no_psk_path_still_works
```

Expected: 3 PASS. The mismatch test should now fail because Bob can't derive the joiner secret without Alice's PSK registered under the same id.

- [ ] **Step 8: Verify clippy + fmt**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

Both clean.

- [ ] **Step 9: Commit**

```bash
git add crates/core/src/mls/group.rs
git commit -m "$(cat <<'EOF'
mls: external PSK injection — inviter proposes, invitee registers

On the inviter side, create_solo registers the PSK and add_member
proposes it via propose_external_psk; MlsGroup::add_members then
commits the Add + PSK proposal in a single Commit. On the invitee
side, join_from_welcome registers the PSK before calling
StagedWelcome::new_from_welcome — if the registered PSK differs,
the derive fails and we surface "mls: welcome process: ..." or
similar. PSK identifier is the byte string "skattr-binding-v1"
(same as the HKDF label from 1.B).
EOF
)"
```

---

## Task 10: `advance_epoch` + `process_incoming_commit`

**Goal:** PCS primitive. Alice `advance_epoch()` returns a Commit. Bob `process_incoming_commit(commit)` advances his epoch to match. Both sides can encrypt/decrypt at the new epoch.

**Files:**
- Modify: `crates/core/src/mls/group.rs`

- [ ] **Step 1: Write the failing test**

Append inside `mod tests`:

```rust
    #[test]
    fn advance_epoch_bumps_epoch_and_both_sides_ratchet() {
        let (mut alice, mut bob) = pair_no_psk();
        assert_eq!(alice.epoch(), 1);
        assert_eq!(bob.epoch(), 1);

        let commit = alice.advance_epoch().unwrap();
        assert_eq!(alice.epoch(), 2);

        bob.process_incoming_commit(&commit).unwrap();
        assert_eq!(bob.epoch(), 2);

        // Both sides can still encrypt/decrypt at epoch 2.
        let env = test_envelope("post-PCS");
        let ct = alice.encrypt(&env).unwrap();
        let got = bob.decrypt(&ct).unwrap();
        assert_eq!(format!("{got:?}"), format!("{env:?}"));
    }
```

- [ ] **Step 2: Run — panic on `todo!()`**

```bash
cargo test -p skattr-core --lib mls::group::tests::advance_epoch_bumps_epoch_and_both_sides_ratchet
```

- [ ] **Step 3: Implement `advance_epoch`**

Replace the `todo!("Task 10")` body in `advance_epoch`:

```rust
    pub(crate) fn advance_epoch(&mut self) -> Result<Vec<u8>> {
        if !self.state.can_send() {
            return Err(CoreError::Mls(format!(
                "mls: advance_epoch: invalid state {:?}",
                self.state
            )));
        }

        let signer = load_signer(&self.provider, &own_public_key(&self.inner)?)?;
        let (commit_out, _welcome_out, _group_info) = self
            .inner
            .self_update(
                self.provider.as_openmls(),
                &signer,
                LeafNodeParameters::default(),
            )
            .map_err(|e| CoreError::Mls(format!("mls: self_update: {e:?}")))?
            .into_messages();

        self.inner
            .merge_pending_commit(self.provider.as_openmls())
            .map_err(|e| CoreError::Mls(format!("mls: merge_pending_commit: {e:?}")))?;

        let bytes = commit_out
            .tls_serialize_detached()
            .map_err(|e| CoreError::Mls(format!("mls: advance_epoch: serialize: {e}")))?;
        self.state = GroupState::Active {
            epoch: self.inner.epoch().as_u64(),
        };
        Ok(bytes)
    }
```

Note the `.into_messages()` call — OpenMLS 0.8's `self_update` returns a `CommitMessageBundle` that needs to be destructured into `(MlsMessageOut commit, Option<MlsMessageOut> welcome, Option<GroupInfo>)`. Check the exact return shape if the code doesn't compile:

```bash
grep -n "into_messages\|CommitMessageBundle" /home/myggiz/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/openmls-0.8.1/src/group/mls_group/updates.rs
```

Adjust the destructure pattern to the actual API.

- [ ] **Step 4: Implement `process_incoming_commit`**

Replace the `todo!("Task 10")` body in `process_incoming_commit`:

```rust
    pub(crate) fn process_incoming_commit(&mut self, commit: &[u8]) -> Result<()> {
        let msg_in = MlsMessageIn::tls_deserialize_exact(commit)
            .map_err(|e| CoreError::Mls(format!("mls: process_commit: deserialize: {e}")))?;
        let protocol_message: ProtocolMessage = match msg_in.extract() {
            MlsMessageBodyIn::PublicMessage(pm) => pm.into(),
            MlsMessageBodyIn::PrivateMessage(pm) => pm.into(),
            _ => {
                return Err(CoreError::Mls(
                    "mls: process_commit: wrong message type".into(),
                ));
            }
        };

        let processed = self
            .inner
            .process_message(self.provider.as_openmls(), protocol_message)
            .map_err(|e| CoreError::Mls(format!("mls: authentication failed: {e:?}")))?;

        match processed.into_content() {
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                self.inner
                    .merge_staged_commit(self.provider.as_openmls(), *staged)
                    .map_err(|e| CoreError::Mls(format!("mls: merge_staged_commit: {e:?}")))?;
                self.state = GroupState::Active {
                    epoch: self.inner.epoch().as_u64(),
                };
                Ok(())
            }
            _ => Err(CoreError::Mls(
                "mls: process_commit: not a Commit message".into(),
            )),
        }
    }
```

- [ ] **Step 5: Run the test**

```bash
cargo test -p skattr-core --lib mls::group::tests::advance_epoch_bumps_epoch_and_both_sides_ratchet
```

Expected: PASS. Both sides at epoch 2, post-PCS encrypt/decrypt round-trips cleanly.

- [ ] **Step 6: Verify clippy + fmt**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/mls/group.rs
git commit -m "$(cat <<'EOF'
mls: advance_epoch + process_incoming_commit (PCS primitives)

advance_epoch wraps MlsGroup::self_update with default LeafNodeParameters,
merges the pending commit, and returns the serialized Commit for the
peer. process_incoming_commit deserializes, process_messages, and
merges the staged commit, updating our epoch. Test verifies both
sides ratchet to the same new epoch and can still exchange messages
at that epoch. 24h / 100-message scheduler wiring stays in 1.E/1.F.
EOF
)"
```

---

## Task 11: State guards

**Goal:** `add_member` on a 2-member group returns the fixed error. `encrypt` / `decrypt` / `advance_epoch` reject `Corrupt` state.

**Files:**
- Modify: `crates/core/src/mls/group.rs`

- [ ] **Step 1: Write the failing test — already-2-member add**

The guard is already in `add_member` from Task 6, but we haven't exercised it yet. Append inside `mod tests`:

```rust
    #[test]
    fn add_member_rejects_when_already_2_member() {
        let pool = Pool::in_memory();
        let kp_repo = crate::storage::KeyPackageRepo::new(&pool);
        let (mut alice, _bob) = pair_no_psk();

        let charlie_id = IdentityKey::generate().unwrap();
        let charlie_provider = MlsProvider::new();
        let charlie_kp =
            KeyPackage::generate(&charlie_id, &charlie_provider, &kp_repo).unwrap();

        let err = alice.add_member(&charlie_kp, None).expect_err("must reject 3rd");
        match err {
            CoreError::Mls(s) => assert!(s.contains("already 2-member"), "got: {s}"),
            other => panic!("expected CoreError::Mls, got {other:?}"),
        }
    }

    #[test]
    fn encrypt_rejects_corrupt_state() {
        let (mut alice, _bob) = pair_no_psk();
        // Force corrupt state directly (private field access via the
        // same module, since we're in mls::group::tests).
        alice.state = GroupState::Corrupt {
            reason: "forced for test".into(),
        };
        let env = test_envelope("should fail");
        let err = alice.encrypt(&env).expect_err("corrupt must reject");
        match err {
            CoreError::Mls(s) => assert!(s.starts_with("mls: encrypt: invalid state")),
            other => panic!("expected CoreError::Mls, got {other:?}"),
        }
    }

    #[test]
    fn advance_epoch_rejects_corrupt_state() {
        let (mut alice, _bob) = pair_no_psk();
        alice.state = GroupState::Corrupt {
            reason: "forced".into(),
        };
        let err = alice.advance_epoch().expect_err("corrupt must reject");
        match err {
            CoreError::Mls(s) => assert!(s.starts_with("mls: advance_epoch: invalid state")),
            other => panic!("expected CoreError::Mls, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run the tests**

```bash
cargo test -p skattr-core --lib mls::group::tests::add_member_rejects_when_already_2_member
cargo test -p skattr-core --lib mls::group::tests::encrypt_rejects_corrupt_state
cargo test -p skattr-core --lib mls::group::tests::advance_epoch_rejects_corrupt_state
```

Expected: 3 PASS. The guards are already in place from Tasks 6, 8, and 10 — this task just exercises them. If any fail due to the field `state` being private to the tests, verify that `mod tests` is INSIDE the file that defines `Group` (same module = same privacy scope).

- [ ] **Step 3: Verify clippy + fmt**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/mls/group.rs
git commit -m "$(cat <<'EOF'
mls: exercise state guards — 3rd member, encrypt/advance in Corrupt

Three tests:
- add_member_rejects_when_already_2_member: post-Bob, Alice rejects
  a Charlie KP with "mls: add_member: already 2-member".
- encrypt_rejects_corrupt_state: force GroupState::Corrupt,
  encrypt returns "mls: encrypt: invalid state ...".
- advance_epoch_rejects_corrupt_state: same pattern for self_update.

Guards themselves were added in Tasks 6/8/10; this task locks them
into the regression suite.
EOF
)"
```

---

## Task 12: Integration test — `crates/tests/src/mls_pair.rs`

**Goal:** A feature-gated integration test that stands up full Alice↔Bob flow with in-memory `Pool`s, exchanges Envelopes both ways, saves, drops, re-loads, and resumes encrypt/decrypt. Proves 1.C's exit criterion end-to-end.

**Files:**
- Create: `crates/tests/src/mls_pair.rs`
- Modify: `crates/tests/src/lib.rs`
- Modify: `crates/core/src/lib.rs` (extend `test_exports`)

- [ ] **Step 1: Extend `lib.rs::test_exports`**

Open `crates/core/src/lib.rs`. Extend the `test_exports` module with MLS items:

```rust
    // Phase 1.C additions:
    pub use crate::mls::{Group, GroupId, GroupState, KeyPackage};
    pub use crate::storage::KeyPackageRepo;

    /// Test-only helper: construct an `MlsProvider` for integration
    /// tests that can't reach the `pub(crate)` module directly.
    #[must_use]
    pub fn new_mls_provider() -> crate::mls::provider::MlsProvider {
        crate::mls::provider::MlsProvider::new()
    }
```

Also expose `MlsGroupRepo`:

```rust
    pub use crate::storage::MlsGroupRepo;
```

(if not already exposed).

And widen `mls::provider::MlsProvider` to `pub` under the `test-harness` feature by adding a twin-arm at the top of `crates/core/src/mls/mod.rs`:

```rust
#[cfg(feature = "test-harness")]
pub use provider::MlsProvider;
```

- [ ] **Step 2: Declare the new test module in `crates/tests/src/lib.rs`**

Open `crates/tests/src/lib.rs`. Find the existing module declarations (likely `pub mod arti_echo;` and similar). Add:

```rust
#[cfg(feature = "test-harness")]
pub mod mls_pair;
```

If the existing `arti_echo` is not feature-gated but uses `test_exports`, skattr-tests already has `skattr-core = { workspace = true, features = ["test-harness"] }` in its `[dependencies]`. Verify:

```bash
grep -n "test-harness" crates/tests/Cargo.toml
```

If the `test-harness` feature isn't enabled, add it to the skattr-core dep there.

- [ ] **Step 3: Write the integration test**

Create `crates/tests/src/mls_pair.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Integration test: Alice ↔ Bob MLS 2-member group, exchange messages
//! both ways, survive restart, resume exchange. Runs against in-memory
//! `Pool`s — no Tor, no Noise. 1.E layers MLS over an authenticated
//! Noise channel.

#![cfg(feature = "test-harness")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use skattr_core::envelope::{Envelope, Kind, MessageId};
use skattr_core::identity::IdentityKey;
use skattr_core::test_exports::{
    new_mls_provider, Group, GroupId, KeyPackage, KeyPackageRepo, MlsGroupRepo,
};
use skattr_core::test_exports::Pool;

fn env(body: &str) -> Envelope {
    Envelope {
        v: 1,
        id: MessageId::generate(),
        ts: 0,
        reply_to: None,
        kind: Kind::Text {
            body: body.to_string(),
        },
    }
}

#[test]
fn alice_bob_exchange_messages_and_survive_restart() {
    let psk = [0x5Au8; 32];

    // -- First session --
    let alice_pool = Pool::in_memory();
    let bob_pool = Pool::in_memory();
    let bob_kp_repo = KeyPackageRepo::new(&bob_pool);
    let alice_group_repo = MlsGroupRepo::new(&alice_pool);
    let bob_group_repo = MlsGroupRepo::new(&bob_pool);

    let alice_id = IdentityKey::generate().unwrap();
    let bob_id = IdentityKey::generate().unwrap();

    let bob_provider = new_mls_provider();
    let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &bob_kp_repo).unwrap();

    let mut alice = Group::create_solo(&alice_id, Some(&psk), new_mls_provider()).unwrap();
    let (welcome, _commit) = alice.add_member(&bob_kp, Some(&psk)).unwrap();
    let mut bob = Group::join_from_welcome(&bob_id, &welcome, Some(&psk), bob_provider).unwrap();

    assert_eq!(alice.epoch(), 1);
    assert_eq!(bob.epoch(), 1);
    assert_eq!(alice.id(), bob.id());
    let gid: GroupId = alice.id().clone();

    // Exchange four messages: A→B, B→A, A→B, B→A.
    let m1 = env("hi bob");
    let ct1 = alice.encrypt(&m1).unwrap();
    let got1 = bob.decrypt(&ct1).unwrap();
    assert_eq!(format!("{got1:?}"), format!("{m1:?}"));

    let m2 = env("hi alice");
    let ct2 = bob.encrypt(&m2).unwrap();
    let got2 = alice.decrypt(&ct2).unwrap();
    assert_eq!(format!("{got2:?}"), format!("{m2:?}"));

    let m3 = env("how's it going");
    let ct3 = alice.encrypt(&m3).unwrap();
    let got3 = bob.decrypt(&ct3).unwrap();
    assert_eq!(format!("{got3:?}"), format!("{m3:?}"));

    let m4 = env("all good");
    let ct4 = bob.encrypt(&m4).unwrap();
    let got4 = alice.decrypt(&ct4).unwrap();
    assert_eq!(format!("{got4:?}"), format!("{m4:?}"));

    // Persist on both sides.
    alice.save(&alice_group_repo).unwrap();
    bob.save(&bob_group_repo).unwrap();

    // -- Simulated restart: drop both groups, then reload --
    drop(alice);
    drop(bob);

    let mut alice = Group::load(&gid, &alice_group_repo).unwrap().expect("alice");
    let mut bob = Group::load(&gid, &bob_group_repo).unwrap().expect("bob");
    assert_eq!(alice.epoch(), 1);
    assert_eq!(bob.epoch(), 1);

    // Resume exchange.
    let m5 = env("still here after restart");
    let ct5 = alice.encrypt(&m5).unwrap();
    let got5 = bob.decrypt(&ct5).unwrap();
    assert_eq!(format!("{got5:?}"), format!("{m5:?}"));

    let m6 = env("bob too");
    let ct6 = bob.encrypt(&m6).unwrap();
    let got6 = alice.decrypt(&ct6).unwrap();
    assert_eq!(format!("{got6:?}"), format!("{m6:?}"));
}
```

Note: the integration test uses `Pool::in_memory()` — we need to confirm `Pool` is reachable from `test_exports`. From Task 5 we know it is — `test_exports::Pool` was added in Phase 0.D.

- [ ] **Step 4: Run the integration test**

```bash
cargo test -p skattr-tests --features test-harness --test mls_pair -- --nocapture
```

If the crate structure uses `lib.rs` with feature-gated `pub mod mls_pair` (rather than `tests/*.rs` files), run:

```bash
cargo test -p skattr-tests --features test-harness mls_pair::alice_bob_exchange_messages_and_survive_restart
```

Expected: PASS.

- [ ] **Step 5: Run the full workspace test suite**

```bash
cargo test --workspace --all-features --release
```

Expected: all tests (including the new integration test) pass.

- [ ] **Step 6: Verify clippy + fmt**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

Both clean.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/lib.rs crates/core/src/mls/mod.rs \
        crates/tests/src/lib.rs crates/tests/src/mls_pair.rs
git commit -m "$(cat <<'EOF'
tests: integration test for Alice↔Bob MLS pair + restart

crates/tests/src/mls_pair.rs stands up two in-memory Pools, runs
Alice create_solo + add_member → Bob join_from_welcome with a
shared PSK, exchanges four envelopes both directions, saves, drops,
reloads via Group::load on both sides, and resumes exchange. Proves
1.C's exit criterion end-to-end.

test_exports widens with Group, KeyPackage, KeyPackageRepo,
MlsGroupRepo, and a new_mls_provider() helper gated on test-harness
so the integration test can reach the pub(crate) types.
EOF
)"
```

---

## Task 13: CHANGELOG + CLAUDE.md + final verification

**Goal:** Document what 1.C shipped, refresh the repository-state paragraph, and run the full check matrix one more time.

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add CHANGELOG bullet**

Open `CHANGELOG.md`. Under `## [Unreleased]` → `### Added`, immediately after the Phase 1.B bullet, add:

```markdown
- **Phase 1.C MLS 2-member groups:** `mls::Group` wraps `openmls::group::MlsGroup` with a `{Active, PendingJoin, Corrupt}` state machine. `Group::create_solo` builds the single-member group, `add_member` produces a (Welcome, Commit) pair bumping epoch 0→1, `join_from_welcome` lands the invitee at epoch 1. Bidirectional `encrypt(Envelope)` / `decrypt` wrap MLS application messages. `advance_epoch` + `process_incoming_commit` cover the PCS primitive (policy wiring stays in 1.E/1.F). Persistence is checkpoint-snapshot: `MlsProvider::snapshot` ciborium-encodes the `openmls_rust_crypto::MemoryStorage` HashMap into `mls_groups.state_blob`; `load` reverses. External PSK (`h_transport` from 1.B) is registered under the identifier `b"skattr-binding-v1"`, proposed by the inviter, registered by the invitee; mismatch fails with `"mls: welcome process: ..."`. New `KeyPackage` newtype (+ `generate`, `to_bytes`, `from_bytes`, `hash`) persisted via a new `KeyPackageRepo` (migration 0002 adds a `key_packages` table with single-use `consumed` flag — enforcement deferred to 1.D). Ciphersuite locked to `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519` (code-point 0x0001). Coverage: 15+ unit tests across `mls::{group, provider, key_package}` + `storage::key_packages` + one integration test at `crates/tests/src/mls_pair.rs` simulating Alice↔Bob exchange + restart.
```

- [ ] **Step 2: Refresh the CLAUDE.md Repository-state paragraph**

Open `CLAUDE.md`. Find the "Phase 0 is complete; Phase 1.A ... Phase 1.B ... are done" paragraph and replace it with:

```markdown
**Phase 0 is complete; Phase 1.A (frame codec), 1.B (Noise_XK handshake),
and 1.C (MLS 2-member groups) are done.** Phase 0 shipped all five
workstreams (0.A scaffold, 0.B identity & crypto, 0.C Arti integration,
0.D storage layer, 0.E documentation baseline). Phase 1.A added
`transport::frame::FrameCodec`. Phase 1.B added
`transport::noise::handshake_{initiator,responder}` + the stateful
`AuthenticatedConnection<S>` wrapper, plus the Ed25519 → X25519 bridge
on `IdentityKey`. Phase 1.C added `mls::Group` (2-member only;
create_solo / add_member / join_from_welcome / encrypt / decrypt /
advance_epoch / process_incoming_commit / save / load), the
`MlsProvider` checkpoint-snapshot persistence layer, `KeyPackage`
newtype + `KeyPackageRepo`, and migration 0002 for the `key_packages`
table. `h_transport` from 1.B is now injected as the external PSK in
the first MLS Commit.
```

Update the "Phase 1 continues with" paragraph to remove 1.C:

```markdown
Phase 1 continues with 1.D invite + contact, 1.E delivery semantics,
1.F CLI integration, 1.G message storage & search — see
`docs/superpowers/specs/2026-04-21-phase-1-decomposition.md` for the
full Phase 1 split. The bootstrap prompt remains authoritative for
file layout, module boundaries, type signatures, and visibility rules
— match it exactly.
```

- [ ] **Step 3: Full check matrix**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features --release
```

Expected: all green. `cargo test` must show the new mls tests + the new integration test passing and every prior test still passing.

- [ ] **Step 4: Sanity-check integration suites still compile**

```bash
cargo test -p skattr-tests --release --no-run
```

Expected: clean compile.

- [ ] **Step 5: Commit**

```bash
git add CHANGELOG.md CLAUDE.md
git commit -m "$(cat <<'EOF'
docs: CHANGELOG + CLAUDE.md — Phase 1.C MLS 2-member groups done

CHANGELOG captures the scope: Group wrapper, checkpoint-snapshot
persistence, KeyPackage + KeyPackageRepo + migration 0002, external
PSK injection of h_transport. CLAUDE.md's Repository-state paragraph
now reflects 1.A + 1.B + 1.C complete and points 1.D-1.G at the
decomposition doc.
EOF
)"
```

---

## Exit verification

After Task 13, the worktree satisfies every item in the design spec's **Exit criteria** section:

1. All unit tests in `mls::group::tests`, `mls::provider::tests`, `mls::key_package::tests`, `storage::key_packages::tests` pass — covered by Tasks 2–11.
2. Integration test in `crates/tests/src/mls_pair.rs` passes under `--features test-harness` — Task 12.
3. `cargo fmt --check` / `cargo clippy --all-features -- -D warnings` / `cargo test --workspace --all-features --release` all green — Task 13 Step 3.
4. Both sides' `state_blob` in `mls_groups` round-trips through drop + load — integration test Task 12.
5. External PSK is injected — verified by Task 9 (match + mismatch + no-PSK).
6. CHANGELOG + CLAUDE.md updated — Task 13.
7. `GroupState` is exactly `{Active, PendingJoin, Corrupt}` — Task 1.
8. Migration 0002 applies cleanly — Task 2.
9. `add_member` on a 2-member group returns `"mls: add_member: already 2-member"` — Task 11.
10. No new fuzz target, no PCS timer, no multi-member support, no `PendingCommit`/`CatchingUp`/`Removed` — explicitly out of scope.

After confirming all boxes tick, the subagent-driven-development flow merges `phase-1c-mls-groups` → `master` with `--no-ff` and removes the worktree.
