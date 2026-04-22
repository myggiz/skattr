# Phase 1.C — MLS 2-member Groups Design

**Status:** Approved 2026-04-22. Sub-project 1.C of the Phase 1 decomposition (`2026-04-21-phase-1-decomposition.md`). Depends on 0.D (storage) and, transitively via the `h_transport` binding, on 1.B (Noise handshake).

## Goal

Two peers form a 2-member MLS group, exchange application messages in both directions, and have their group state survive daemon restart. The transport↔MLS binding token produced by 1.B (`h_transport`) is injected as an external PSK into the first MLS Commit so MLS state can't be replayed across different Noise sessions. This closes the gap between 1.B (authenticated bytes) and 1.E (delivery over those bytes) — after 1.C, the daemon has an end-to-end encrypted channel, just not yet the outbox/retry logic to drive it over real Tor.

## Scope

**In scope**

- `Group::{create_solo, add_member, join_from_welcome, encrypt, decrypt, advance_epoch, process_incoming_commit, save, load}`.
- `KeyPackage` newtype with `generate`, `to_bytes`, `from_bytes`, `hash`.
- `MlsProvider` — thin wrapper around `openmls_rust_crypto::OpenMlsRustCrypto` with `snapshot()` / `load(bytes)`.
- `KeyPackageRepo` — new repo under `storage/` with `insert`, `get`, `mark_consumed`.
- Migration `0002_key_packages.sql` — new `key_packages` table; bumps `schema_version` 1→2.
- External-PSK plumbing: `create_solo` / `add_member` / `join_from_welcome` all take `psk: Option<&[u8; 32]>`; matching PSKs succeed, mismatched PSKs fail with a fixed-string error.
- `GroupState` shrunk to three variants: `Active { epoch }`, `PendingJoin`, `Corrupt { reason }`.
- 11 unit tests (happy paths, error paths, save/load round-trip, PSK match/mismatch, state guards) + 1 integration test (`crates/tests/src/mls_pair.rs`, gated on `test-harness`) that simulates a full Alice↔Bob exchange + restart.
- Error taxonomy funnelled through `CoreError::Mls(String)` with fixed prefixes.

**Out of scope**

- **`PendingCommit`, `CatchingUp`, `Removed` state variants.** Reintroduced in 1.E (delivery) and Phase 2 (multi-member) when they're actually reachable. For 1.C, 2-member with sync `&mut self` can't reach them.
- **Multi-member group mechanics.** `add_member` called on a 2-member group returns an error (`"mls: add_member: already 2-member"`). 3+ members land in Phase 2.
- **PCS timer / message-count policy.** `advance_epoch` is a primitive that the caller can invoke. The 24-h-or-100-message policy wiring belongs in 1.E/1.F where delivery state lives.
- **Actor-per-group tokio task.** Sync `&mut self` API. Callers wanting concurrent access wrap in `tokio::sync::Mutex<Group>`.
- **KeyPackage single-use policy enforcement.** 1.C persists KPs + their consumed flag; 1.D flips the flag on successful join and rejects re-use.
- **Tor-level integration test.** 1.C's integration test uses in-memory `Pool`s and direct function calls. 1.E layers MLS over a real Noise-authenticated `AuthenticatedConnection`.
- **Fuzz target for MLS messages.** Workstream 4.B.

## Locked decisions (settled during brainstorming)

| Decision | Choice |
|---|---|
| State machine scope | (A) Minimal. `GroupState::{Active{epoch}, PendingJoin, Corrupt{reason}}` only. Drop `PendingCommit`, `CatchingUp`, `Removed`. |
| Persistence strategy | (B) `OpenMlsRustCrypto` + checkpoint-snapshot into `mls_groups.state_blob`. No full `StorageProvider` implementation. |
| PCS policy | (A) Defer. 1.C ships the primitive `advance_epoch` + one test; 1.E/1.F wires the schedule. |
| KeyPackage single-use | 1.C tracks KPs in a new `key_packages` table with a `consumed` boolean; 1.D enforces the policy. |
| API model | Sync `&mut Group`. No actor. |
| External PSK | Parameter on `create_solo` / `add_member` / `join_from_welcome`. No lookup. |
| `psk_id` for OpenMLS `PreSharedKeyID` | Byte string `b"skattr-binding-v1"` (matches the HKDF label — symmetric with 1.B). |
| First-Commit merge timing | Alice calls `merge_pending_commit` immediately after `add_members`, *before* shipping the Commit. In a 2-member world there's no conflicting proposal possible. |

## Architecture

All changes inside `crates/core/src/` plus one new integration-test file under `crates/tests/`.

```
mls/ciphersuite.rs            NO CHANGE
mls/mod.rs                    MODIFY: shrink re-exports; add KeyPackage, MlsProvider
mls/state_machine.rs          SHRINK: GroupState::{Active{epoch}, PendingJoin, Corrupt{reason}}
mls/provider.rs               NEW:    MlsProvider wrapping OpenMlsRustCrypto
mls/key_package.rs            NEW:    KeyPackage newtype, generate, to_bytes, from_bytes, hash
mls/group.rs                  REWRITE: create_solo, add_member, join_from_welcome,
                                       encrypt, decrypt, process_incoming_commit,
                                       advance_epoch, save, load; all ops + checkpoint persist
mls/keystore.rs               DELETE: superseded by provider.rs
mls/welcome.rs                DELETE: logic absorbed into Group::join_from_welcome
mls/commit.rs                 DELETE: logic absorbed into Group::{add_member, advance_epoch,
                                       process_incoming_commit}

storage/key_packages.rs       NEW:    KeyPackageRepo with insert/get/mark_consumed
storage/migrations/
  0002_key_packages.sql       NEW:    schema_version++; create key_packages table
storage/mod.rs                MODIFY: expose KeyPackageRepo (pub(crate))

error.rs                      NO CHANGE: reuse CoreError::Mls(String) with "mls: ..." prefix

lib.rs                        MODIFY: extend test_exports with Group, KeyPackage,
                                      KeyPackageRepo (behind test-harness)

crates/tests/src/mls_pair.rs  NEW:    integration test — full Alice↔Bob exchange + restart
crates/tests/src/lib.rs       MODIFY: declare mls_pair module
```

**Tests:**

- `mls::group::tests` — inline unit tests (happy + state guards + save/load).
- `mls::provider::tests` — provider snapshot round-trip.
- `mls::key_package::tests` — KeyPackage hash + round-trip.
- `storage::key_packages::tests` — repo CRUD.
- `crates/tests/src/mls_pair.rs` — single integration test, `#[cfg(feature = "test-harness")]`, simulates restart.

No new fuzz target. Tracked in workstream 4.B.

## Key types

### `GroupState` (shrunk)

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupState {
    /// Steady-state: the group has an accepted epoch and we can send.
    Active { epoch: u64 },
    /// We have a Welcome but haven't processed it yet.
    PendingJoin,
    /// State is irrecoverably corrupt; only option is recreate.
    Corrupt { reason: String },
}

impl GroupState {
    pub fn can_send(&self) -> bool {
        matches!(self, GroupState::Active { .. })
    }

    pub fn is_recoverable(&self) -> bool {
        !matches!(self, GroupState::Corrupt { .. })
    }
}
```

### `MlsProvider`

```rust
pub(crate) struct MlsProvider {
    inner: openmls_rust_crypto::OpenMlsRustCrypto,
}

impl MlsProvider {
    pub(crate) fn new() -> Self;

    /// Serialise the provider's in-memory storage. Format is
    /// implementation-private; consumers only round-trip through
    /// `snapshot` + `load`.
    pub(crate) fn snapshot(&self) -> Result<Vec<u8>>;

    /// Rehydrate from bytes produced by `snapshot`. Returns an error
    /// if the format is not recognised.
    pub(crate) fn load(bytes: &[u8]) -> Result<Self>;

    /// Borrow the underlying OpenMLS provider for OpenMLS calls.
    pub(crate) fn as_openmls(&self) -> &openmls_rust_crypto::OpenMlsRustCrypto;
}
```

**Snapshot fallback.** If OpenMLS 0.8's `MemoryStorage` doesn't expose a clean iteration/serialisation hook, `snapshot` serialises only the registered PSKs and any other application-level state `MlsProvider` layers on top; `MlsGroup` state is serialised separately via OpenMLS's built-in `serde` support on `MlsGroup`, and `load` rebuilds a fresh provider + re-registers PSKs. Either path results in the same public contract.

### `KeyPackage`

```rust
pub(crate) struct KeyPackage(openmls::key_packages::KeyPackage);

impl KeyPackage {
    /// Generate a fresh KeyPackage for this identity. Persists it +
    /// its hash via `kp_repo` with `direction = "ours", consumed = false`.
    pub(crate) fn generate(
        identity: &IdentityKey,
        provider: &MlsProvider,
        kp_repo: &KeyPackageRepo,
    ) -> Result<Self>;

    /// Serialise for wire transmission (used by 1.D invite flow).
    pub(crate) fn to_bytes(&self) -> Result<Vec<u8>>;

    /// Deserialise from wire bytes. Validates ciphersuite + signature.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self>;

    /// 32-byte SHA-256 of the serialised KeyPackage. Used as the
    /// `kp_hash` key in the `key_packages` table.
    pub(crate) fn hash(&self) -> [u8; 32];
}
```

### `Group`

```rust
pub struct Group {
    pub(crate) id: GroupId,
    pub(crate) state: GroupState,
    inner: openmls::group::MlsGroup,
    provider: MlsProvider,
}

pub(crate) type WelcomeBytes = Vec<u8>;
pub(crate) type CommitBytes = Vec<u8>;

impl Group {
    /// Create a solo (1-member) group. The group becomes 2-member when
    /// `add_member` is called. `psk`, when present, is registered with
    /// the provider and will be injected into the Add Commit.
    pub(crate) fn create_solo(
        identity: &IdentityKey,
        psk: Option<&[u8; 32]>,
    ) -> Result<Self>;

    /// Add the KeyPackage's owner to the group. Returns the Welcome
    /// (for the invitee) and the Commit (for any existing members —
    /// for 2-member this is no-op on the wire since there are none).
    /// Alice calls `merge_pending_commit` before returning.
    pub(crate) fn add_member(
        &mut self,
        invitee_kp: &KeyPackage,
        psk: Option<&[u8; 32]>,
    ) -> Result<(WelcomeBytes, CommitBytes)>;

    /// Construct a Group from a received Welcome. The invitee must
    /// have registered the same PSK with its provider beforehand if
    /// the Welcome's Commit referenced one.
    pub(crate) fn join_from_welcome(
        identity: &IdentityKey,
        welcome: &[u8],
        psk: Option<&[u8; 32]>,
    ) -> Result<Self>;

    /// Encrypt an Envelope as an MLS application message.
    pub(crate) fn encrypt(&mut self, envelope: &Envelope) -> Result<Vec<u8>>;

    /// Decrypt an incoming MLS application message into an Envelope.
    pub(crate) fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Envelope>;

    /// Process an incoming Commit from a peer. Advances our epoch.
    pub(crate) fn process_incoming_commit(&mut self, commit: &[u8]) -> Result<()>;

    /// Build an empty self-Commit that ratchets our epoch forward.
    /// Caller ships the returned bytes to the peer.
    pub(crate) fn advance_epoch(&mut self) -> Result<Vec<u8>>;

    /// Persist current state (provider snapshot + MlsGroup serialisation)
    /// via MlsGroupRepo keyed by our group_id.
    pub(crate) fn save(&self, repo: &MlsGroupRepo) -> Result<()>;

    /// Restore from persisted state. Returns None if `group_id` is unknown.
    pub(crate) fn load(group_id: &GroupId, repo: &MlsGroupRepo) -> Result<Option<Self>>;

    pub fn id(&self) -> &GroupId;
    pub fn epoch(&self) -> u64;
    pub fn state(&self) -> &GroupState;
}
```

All mutation methods (`add_member`, `join_from_welcome`, `encrypt`, `decrypt`, `process_incoming_commit`, `advance_epoch`) update `self.state` to `Active { epoch: new_epoch }` on success. On Welcome processing, transient `PendingJoin` appears between deserialisation and validation. On any operation that returns a typed error from OpenMLS, the state transitions to `Corrupt { reason: ... }` and subsequent ops are rejected.

### `KeyPackageRepo`

```rust
pub(crate) struct KeyPackageRepo<'p> {
    pool: &'p Pool,
}

impl<'p> KeyPackageRepo<'p> {
    pub(crate) fn new(pool: &'p Pool) -> Self;

    /// Insert a freshly-generated KeyPackage. `direction` is "ours"
    /// for KPs we will share with peers and "theirs" for KPs we
    /// received (Phase 2 concern; always "ours" in 1.C).
    pub(crate) fn insert(
        &self,
        hash: &[u8; 32],
        bytes: &[u8],
        direction: &str,
    ) -> Result<()>;

    /// Look up by 32-byte hash. Returns `(bytes, consumed)`.
    pub(crate) fn get(
        &self,
        hash: &[u8; 32],
    ) -> Result<Option<(Vec<u8>, bool)>>;

    /// Mark a KeyPackage consumed. Idempotent; no-op if already consumed.
    /// 1.D calls this on successful join.
    pub(crate) fn mark_consumed(&self, hash: &[u8; 32]) -> Result<()>;
}
```

### Migration `0002_key_packages.sql`

```sql
-- skattr schema migration 0002: key packages
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

## Wire flow — 2-member happy path

```
Alice                                                 Bob
-----                                                 ---
(already has AuthenticatedConnection with             (already has AuthenticatedConnection with
 HandshakeOutcome.h_transport from 1.B)                HandshakeOutcome.h_transport from 1.B;
                                                       both h_transports are byte-equal)

Group::create_solo(alice_id, Some(&h_t))
  → mls_groups row written; epoch 0

                                                      KeyPackage::generate(bob_id, provider, repo)
                                                        → key_packages row, consumed=false

                             [bob_kp bytes delivered to Alice — 1.D invite path;
                              in 1.C integration test, direct function call]

Group::add_member(&mut self, bob_kp, Some(&h_t))
  → OpenMLS proposes Add + builds Commit referencing
    the external PSK (psk_id = "skattr-binding-v1",
    psk_bytes = h_t)
  → merge_pending_commit on Alice's provider
  → returns (welcome_bytes, commit_bytes)
  → mls_groups row bumped epoch 0→1
  → alice.state = Active { epoch: 1 }

        [welcome_bytes delivered to Bob]    →   Group::join_from_welcome(bob_id, welcome, Some(&h_t))
                                                  → Bob's provider registers PSK first, then
                                                    MlsGroup::new_from_welcome decrypts
                                                  → bob.state = Active { epoch: 1 }
                                                  → mls_groups row written

Group::encrypt(envelope) → ct                 →   Group::decrypt(ct) → envelope
Group::decrypt(ct)        ← ct                ←   Group::encrypt(envelope) → ct
(both sides may encrypt concurrently; no race — MLS generation numbers order within the epoch)

Group::advance_epoch()
  → Self-Commit bytes
                                              →   Group::process_incoming_commit(commit)
  → alice epoch 1→2                                 → bob epoch 1→2
```

If Bob joins with a wrong PSK, his `MlsGroup::new_from_welcome` fails decryption → `CoreError::Mls("mls: external PSK mismatch")`. If Alice's PSK registration was correct but Bob registered none, same failure mode.

## Error surface

All MLS errors funnel through `CoreError::Mls(String)`:

| Condition | Surfaced as |
|---|---|
| OpenMLS builder rejects our ciphersuite / config | `"mls: builder: {detail}"` |
| `MlsGroup::create_message` fails | `"mls: encrypt: {detail}"` |
| `MlsGroup::process_message` auth failure (generic) | `"mls: authentication failed"` |
| Welcome references unknown KeyPackage | `"mls: welcome: unknown KeyPackage"` |
| Welcome ciphersuite mismatch | `"mls: welcome: ciphersuite mismatch"` |
| External PSK mismatch (any side) | `"mls: external PSK mismatch"` |
| Epoch regression (received Commit from already-merged epoch) | `"mls: stale commit: epoch {got} < {current}"` |
| Load failure — blob doesn't deserialise | `"mls: load: corrupt state blob"` |
| `advance_epoch` called while `state != Active` | `"mls: advance_epoch: invalid state {state:?}"` |
| `add_member` called when already 2-member | `"mls: add_member: already 2-member"` |
| `encrypt` / `decrypt` called when `state != Active` | `"mls: {op}: invalid state {state:?}"` |

Nothing logs key material, plaintext, or internal OpenMLS state at `info` level or higher.

## Testing strategy

### Unit tests (inline `#[cfg(test)] mod tests`)

1. **`mls::group::tests::create_solo_persists_at_epoch_0`** — create, save, list via `MlsGroupRepo`, assert row exists with `epoch = 0`.
2. **`mls::group::tests::save_load_round_trip`** — create, mutate (encrypt a message), save, drop, load, assert epoch + state equal.
3. **`mls::group::tests::add_member_emits_welcome_and_commit_and_epoch_1`** — Alice creates, Bob `KeyPackage::generate`, Alice `add_member(bob_kp, None)`, assert both blobs non-empty and `alice.epoch() == 1`.
4. **`mls::group::tests::join_from_welcome_lands_at_epoch_1`** — drive Alice→Bob flow, assert `bob.epoch() == 1` and `bob.id() == alice.id()`.
5. **`mls::group::tests::bidirectional_encrypt_decrypt`** — Alice sends, Bob receives; Bob sends, Alice receives. `Envelope` contents round-trip byte-equal.
6. **`mls::group::tests::external_psk_match_succeeds`** — Alice + Bob with same PSK `[0xEE; 32]`. Flow completes; `alice.epoch() == bob.epoch() == 1`.
7. **`mls::group::tests::external_psk_mismatch_fails`** — Alice `[0xAA; 32]`, Bob `[0xBB; 32]`. Bob's `join_from_welcome` returns `CoreError::Mls(s)` where `s.contains("external PSK mismatch") || s.contains("authentication")` (fallback for snow-style wrapping).
8. **`mls::group::tests::no_psk_path_works`** — both sides `None`. Flow completes.
9. **`mls::group::tests::advance_epoch_bumps_and_persists`** — Alice `advance_epoch`, Bob `process_incoming_commit`, both at epoch 2, both encrypt/decrypt still work, both persist.
10. **`mls::group::tests::encrypt_rejects_non_active_state`** — force `state = Corrupt{...}`, assert encrypt returns error whose message starts with `"mls: encrypt: invalid state"`.
11. **`mls::group::tests::add_member_rejects_3rd_member`** — after Bob joins, try `alice.add_member(charlie_kp, None)`; assert `"mls: add_member: already 2-member"`.

Plus separate tests for the smaller pieces:

- **`mls::provider::tests::snapshot_load_round_trip`** — provider → snapshot bytes → load → equivalent provider. Test by registering a PSK, snapshotting, loading, asserting the PSK is still registered (via a trial lookup).
- **`mls::key_package::tests::hash_is_stable_and_bytes_round_trip`** — generate a KP, hash it twice (same output), `to_bytes`/`from_bytes` round-trip preserves hash.
- **`storage::key_packages::tests::insert_get_consume_flow`** — repo round-trip: insert → get returns `(bytes, false)` → mark_consumed → get returns `(bytes, true)` → double mark_consumed is idempotent.

### Integration test

`crates/tests/src/mls_pair.rs`, gated on `#[cfg(feature = "test-harness")]`, one test:

- **`alice_bob_exchange_messages_and_survive_restart`** — stands up two in-memory `Pool`s + `KeyPackageRepo`s + `MlsGroupRepo`s in tempdirs. Alice `create_solo`, Bob `generate_key_package`, Alice `add_member`, Bob `join_from_welcome`. Both encrypt/decrypt two envelopes each. Both `save`. Drop everything. On fresh handles, `load` on both sides. Resume encrypt/decrypt one more envelope in each direction. Assert all envelopes decode byte-equal to what was sent. Uses a random PSK `[0x5A; 32]` on both sides for completeness.

## Dependencies

Already present in workspace (`Cargo.toml`):

- `openmls = "0.8"`, `openmls_traits = "0.5"`, `openmls_rust_crypto = "0.5"` (don't bump — see CLAUDE.md "Dep version gotchas")
- `ciborium = "0.2"` (Envelope CBOR)
- `sha2 = "0.10"` (KeyPackage::hash)
- `serde = "1"`, `zeroize = "1"`, `rand = "0.8"` (all already used)

No new third-party deps. No cargo workspace edits.

## Risks

- **OpenMLS 0.8 `MemoryStorage` serialisation.** The snapshot strategy assumes we can extract provider state in some form. Mitigation: Task 2 of the plan is a focused spike on this; if no clean hook exists, fall back to serialising only `MlsGroup` + re-creating a fresh provider on load + re-registering any external PSKs (which we own — they're just `h_transport`). Both paths satisfy the public contract.
- **External PSK registration timing.** OpenMLS requires the PSK to be registered with the provider *before* the Add Commit is processed on the receiver. `join_from_welcome` must register the PSK first, then call `MlsGroup::new_from_welcome`. Mitigation: `join_from_welcome` does this in one atomic operation; unit test 7 proves the sequence works.
- **`MlsGroup::add_members` return type.** OpenMLS 0.8 returns `(MlsMessageOut commit, Welcome, Option<GroupInfo>)`. We discard `GroupInfo` (used for external commits, not needed for 1.C). The `Welcome` must be serialised via `tls_codec::Serialize`. Mitigation: Task's code pins the exact serialisation call.
- **Ciphersuite code-point drift.** Locked to `0x0001` (`MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519`). Guarded by `ciphersuite.rs`'s constant and a test that rejects Welcomes for any other code-point. A future upgrade requires an ADR.
- **Epoch 0→1 first-Commit semantics.** Alice merges her staged Commit *before* shipping. This is safe for 2-member because there's no other member to race. When Phase 2 lands 3+ members, this merges-before-broadcast pattern will need the `PendingCommit` state back; tracked.
- **`MlsGroup::save` vs. our `save`.** OpenMLS's `MlsGroup::save(provider)` writes to provider storage. Our `Group::save(repo)` then snapshots the provider + the group and writes the blob to `MlsGroupRepo`. Two calls, different targets, same name — risk of confusion. Mitigation: never call OpenMLS's `save` directly from `Group`; only `Group::save` is in the public API, and internally it invokes provider serialisation explicitly. Documented in the `Group` doc comment.

## Exit criteria

1. All unit tests in `mls::group::tests`, `mls::provider::tests`, `mls::key_package::tests`, `storage::key_packages::tests` pass.
2. The integration test in `crates/tests/src/mls_pair.rs` passes under `--features test-harness`.
3. `cargo fmt --check` / `cargo clippy --workspace --all-targets --all-features -- -D warnings` / `cargo test --workspace --all-features --release` all green.
4. Both sides' `state_blob` in `mls_groups` round-trips through a drop + load and the reloaded groups can still encrypt/decrypt.
5. External PSK is injected into the first Commit — verified by unit test 7 (match succeeds) and unit test 6 (mismatch fails with fixed string).
6. CHANGELOG bullet and CLAUDE.md Repository-state paragraph updated with "Phase 1.C complete."
7. `GroupState` is exactly `{Active, PendingJoin, Corrupt}` — no dead arms left over from the Phase 0 scaffold.
8. Migration `0002_key_packages.sql` applies cleanly to a fresh database and to an existing `0001_init.sql`-populated database.
9. `add_member` on a 2-member group returns `"mls: add_member: already 2-member"` — verified by unit test 11.
10. No new fuzz target, no PCS timer, no multi-member support, no `PendingCommit`/`CatchingUp`/`Removed` code paths (these are explicitly out of scope).
