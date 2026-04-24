# Phase 1.H — Hardening design

**Status:** draft, pending user review
**Date:** 2026-04-24
**Predecessor:** Phase 1.G (message storage & search) merged at `dedc206`.
**Successor:** Phase 2 (Tauri 2 + SvelteKit UI).

## Goal

Close out every hardening item surfaced during Phase 1.G reviews so the
daemon presents a stable correctness / error-taxonomy / CI surface to
the Phase 2 UI layer. "Close out the review thread" scope (all 11
items), not a minimal subset.

## Non-goals

- No new user-facing features.
- No changes to the 2-member group invariant (multi-member is Phase 2+).
- No new dependencies beyond `serial_test` (dev-only, for item #11).
- No IPC wire protocol breaks other than adding `DaemonErrorKind`
  variants (additive — unknown variants already round-trip as opaque
  values on the client).

## Scope

11 items from `docs/superpowers/kickoffs/2026-04-24-phase-1h-hardening.md`,
grouped into four lanes:

| Lane | Items | Summary |
|------|-------|---------|
| **L1 — Storage correctness** | #2, #3, #8 | Migration 0007 (envelope_id column + unique index), tx-wrapped send/receive, backfill_body_text in a single transaction |
| **L2 — Error taxonomy** | #4, #5 | Subsystem sub-enums replace string matching in `CoreError::kind()`; add `DaemonErrorKind::InvalidArgument { message }` |
| **L3 — IPC/API polish** | #1, #7 | `ContactRepo::contact_for_group` helper, `MessageRecord.row_id` surfaced for UI correlation |
| **L4 — Hygiene & infra** | #6, #9, #10, #11 | Hoist `now_unix_seconds` into `daemon::clock`, `[u8; 32]` group_id on `ReceiveOutcome::New`, cargo-deny CI job, `serial_test` replacing the socket-path Mutex |

## Architecture decisions

### L1.a — Migration 0007: envelope_id column + unique index (item #2)

Add a persisted `envelope_id BLOB NOT NULL CHECK(length(envelope_id) = 16)`
column to `messages`, backed by a unique index on
`(group_id, envelope_id)`. Raw 16-byte payload matches
`MessageId([u8; 16])` directly; hex would bloat the index with no
debugging win.

**Migration SQL** (`0007_messages_envelope_id.sql`):

```sql
-- Add column nullable first (SQLite can't add NOT NULL mid-life).
-- Existing rows (if any from pre-1.H installs) get NULL; the startup
-- backfill (below) populates them. The BEFORE-INSERT trigger enforces
-- 16-byte shape on all *new* inserts, which is what we need — the
-- backfill uses UPDATEs, which the trigger does not intercept.
ALTER TABLE messages ADD COLUMN envelope_id BLOB;

CREATE TRIGGER IF NOT EXISTS messages_envelope_id_shape
BEFORE INSERT ON messages
WHEN new.envelope_id IS NULL OR length(new.envelope_id) <> 16
BEGIN
    SELECT RAISE(ABORT, 'envelope_id must be 16 bytes');
END;

-- Unique index enforces (group_id, envelope_id) uniqueness. NULLs
-- compare distinct by default in SQLite, so pre-backfill legacy rows
-- don't collide — once backfilled, they acquire their true 16-byte id
-- and the constraint becomes meaningful.
CREATE UNIQUE INDEX IF NOT EXISTS messages_group_envelope_uniq
    ON messages(group_id, envelope_id);
```

The migration runner is SQL-only (see `storage/migrations.rs` —
`execute_batch` of the `.sql` file inside a single tx, plus a
`schema_version` bump). Backfill is therefore a **separate
startup-time Rust step**, mirroring 1.G's `backfill_body_text`: on
every daemon start, `MessageRepo::backfill_envelope_id` runs
idempotently inside one transaction (item #8's pattern). This is
safe because:

- New `MessageRepo::insert` always binds a 16-byte envelope_id (the
  trigger enforces this).
- Pre-1.H rows have NULL envelope_id; the unique index's NULL-is-
  distinct rule means no index collisions during the backfill window.
- Backfill is an `UPDATE messages SET envelope_id = ? WHERE id = ?`
  loop decoding `body_blob`'s CBOR-encoded envelope; the trigger
  doesn't fire on UPDATE, so no chicken-and-egg problem.
- On completion, all rows have envelope_id; duplicates — if any — are
  resolved by the backfill keeping the lowest row id and deleting the
  rest (log what's deleted; in practice there should be zero).

**Repo changes:**

- `MessageRepo::insert` reads `params.envelope.id.0` and binds it as
  the new column. No signature change (envelope_id is derivable from
  the `Envelope` already passed).
- `MessageRepo::backfill_envelope_id(&self)` — opens a single tx,
  decodes each row's `body_blob`, extracts `envelope.id`, updates in
  place. Skips rows whose blob fails to decode (matches 1.G's
  `backfill_body_text` resilience pattern). Resolves any pre-existing
  `(group_id, envelope_id)` collisions by keeping the lowest `id` and
  deleting the rest; logs each deletion at `warn` level.
- `Daemon::run` (see `crates/core/src/daemon/mod.rs`) calls both
  `backfill_body_text` and `backfill_envelope_id` during startup,
  after migrations, before opening IPC. Both are idempotent.
- Duplicate insert returns `CoreError::Storage(StorageErrorKind::
  DuplicateMessage)` (see L2); `send_message` maps this to
  `SendStatus::Delivered` (already-persisted row means the earlier
  attempt succeeded).

### L1.b — Transactional send/receive (item #3)

Current gap: `group.save` persists the ratchet *before* `MessageRepo::
insert`. Crash between the two → ratchet advanced on disk, no history
row, outbox never enqueues.

Wrap `group.save + MessageRepo::insert + OutboxRepo::insert` (send) and
`group.save + receive() (which inserts)` (receive) in a single
`Pool::transaction`.

**Key refactor: `MlsProvider::save_in_tx`.** OpenMLS's `StorageProvider`
writes through its own blob to `mls_groups`; we wrap it. Add a new entry
point that accepts a `&rusqlite::Transaction` so the outer transaction
encloses the provider's write. Sketch:

```rust
impl MlsProvider {
    /// Write the provider's cached state into `tx`, leaving commit to
    /// the caller. Mirrors `save()` but short-circuits the internal
    /// `Pool::with` so `MlsGroupRepo::write_snapshot_in_tx` runs inside
    /// the outer tx.
    pub(crate) fn save_in_tx(
        &self,
        group: &Group,
        tx: &rusqlite::Transaction<'_>,
    ) -> Result<()> { ... }
}
```

`Group::save_in_tx(&self, repo, tx)` becomes the new entry point used
by `send_message` and `dispatch_for_group`. The existing `Group::save`
keeps working for non-send callers (invite add, etc.) — it just opens
its own single-stmt tx.

**Send path** (`daemon::dispatch::send_message`) becomes:

```
// 1. Resolve group, build envelope.
// 2. group.encrypt (ratchet advances in memory).
// 3. pool.transaction(|tx| {
//      Group::save_in_tx(tx)?;
//      MessageRepo::insert_in_tx(tx)?;   // may fail with DuplicateMessage
//      OutboxRepo::insert_in_tx(tx)?;
//    })
// 4. If tx Err(DuplicateMessage): return SendStatus::Delivered.
// 5. If tx Ok: kick the hub, wait for ACK, return Delivered | Queued.
```

If the transaction fails, the in-memory ratchet advance is discarded —
we drop the `Group` value. Next send re-loads from disk.

**Receive path** (`daemon::inbound::dispatch_for_group`) becomes:

```
// 1. Load group, decrypt (ratchet advances in memory).
// 2. pool.transaction(|tx| {
//      Group::save_in_tx(tx)?;
//      receive_in_tx(tx, ...)?;   // persists message row + seen_messages
//    })
// 3. Emit MessageReceived event outside the tx (event broadcast is
//    best-effort; failed subscribers do not roll back persistence).
```

`receive_in_tx` is a new sibling of `receive()` taking an explicit
transaction. The existing `receive()` becomes a thin wrapper that opens
its own tx — useful for unit tests.

### L1.c — backfill_body_text in a transaction (item #8)

Wrap the N-row UPDATE loop in `MessageRepo::backfill_body_text` with
`pool.transaction`. Currently each row auto-commits → N fsyncs. Same
cheap fix for the new `backfill_envelope_id` (L1.a already calls for
this).

### L2 — Subsystem error sub-enums (items #4, #5)

Replace the six string-matching arms in `CoreError::kind()` with
structural matches on typed sub-enums. Each `CoreError::<Subsystem>`
variant becomes `CoreError::<Subsystem>(<Subsystem>ErrorKind)`.

**New sub-enums** (all `#[derive(Debug, thiserror::Error)]`,
`#[non_exhaustive]`):

```rust
pub enum ContactErrorKind {
    #[error("contact not found")]
    NotFound,
    #[error("contact ambiguous ({matches} matches)")]
    Ambiguous { matches: u32 },
    #[error("contact: {0}")]
    Other(String),
}

pub enum InviteErrorKind {
    #[error("invite expired")]
    Expired,
    #[error("invite consumed")]
    Consumed,
    #[error("invite signature invalid")]
    SignatureInvalid,
    #[error("invite: {0}")]
    Other(String),
}

pub enum MlsErrorKind {
    #[error("group corrupt")]
    GroupCorrupt,
    #[error("mls: {0}")]
    Other(String),
}

pub enum DeliveryErrorKind {
    #[error("delivery timeout")]
    Timeout,
    #[error("delivery: {0}")]
    Other(String),
}

pub enum TransportErrorKind {
    #[error("tor not ready")]
    TorNotReady,
    #[error("transport: {0}")]
    Other(String),
}

pub enum StorageErrorKind {
    #[error("fts5 syntax error: {0}")]
    FtsSyntax(String),
    #[error("duplicate message")]
    DuplicateMessage,
    #[error("storage: {0}")]
    Other(String),
}
```

`CoreError::kind()` becomes a pure structural match, no `str::contains`:

```rust
pub fn kind(&self) -> Option<DaemonErrorKind> {
    use DaemonErrorKind as K;
    Some(match self {
        CoreError::Contact(ContactErrorKind::NotFound) => K::ContactNotFound,
        CoreError::Contact(ContactErrorKind::Ambiguous { matches }) =>
            K::ContactAmbiguous { matches: *matches },
        CoreError::Invite(InviteErrorKind::Expired) => K::InviteExpired,
        CoreError::Invite(InviteErrorKind::Consumed) => K::InviteConsumed,
        CoreError::Invite(InviteErrorKind::SignatureInvalid) =>
            K::InviteSignatureInvalid,
        CoreError::Mls(MlsErrorKind::GroupCorrupt) => K::GroupCorrupt,
        CoreError::Delivery(DeliveryErrorKind::Timeout) => K::DeliveryTimeout,
        CoreError::Transport(TransportErrorKind::TorNotReady) => K::TorNotReady,
        CoreError::Storage(StorageErrorKind::FtsSyntax(_)) => K::SearchSyntax,
        CoreError::Sqlite(_) | CoreError::Storage(_) => K::StorageError,
        _ => return None,
    })
}
```

**Callsite migration strategy:** subsystem by subsystem, each a single
commit. Within each subsystem, replace every
`CoreError::<Subsystem>("X".into())` with the typed variant. Where the
string was free-form, use `<Subsystem>ErrorKind::Other(s)`. Existing
`From<X>` impls at subsystem boundaries (e.g., `From<rusqlite::Error>`
for storage) stay intact — they bind to `Other(e.to_string())`.

**Item #4 — `DaemonErrorKind::InvalidArgument { message }`:**

```rust
pub enum DaemonErrorKind {
    // existing variants...
    InvalidArgument { message: String },
}
```

Dispatch sites (`prune_history` validation at lines 509 and 535) stop
returning `IpcError::Internal(...)` and return
`IpcError::Daemon(DaemonErrorKind::InvalidArgument { message })`
instead. CLI `src/main.rs` maps this to exit code 2 ("argument error"),
distinct from internal-error exit code 1.

### L3.a — `ContactRepo::contact_for_group` helper (item #1)

Fix: `dispatch.rs::search_messages` currently sets
`contact_for_record = contact.unwrap_or(sender_pk)` on unscoped search.
For outgoing rows (sender == local identity), `sender_pk` is the local
pubkey — wrong for any UI rendering a peer avatar.

Add to `ContactRepo`:

```rust
/// Reverse-lookup: given a 2-member-group group_id, return the peer's
/// PublicKey (i.e. the member that is NOT us). Returns `Ok(None)` if
/// no contact row has this group_id.
///
/// Phase 1.H: scoped to 2-member groups per CLAUDE.md. Multi-member
/// dispatch lands in Phase 2+.
pub fn contact_for_group(&self, group_id: &[u8; 32])
    -> Result<Option<PublicKey>>;
```

In `search_messages`, each hit resolves `contact_for_record` via
`ContactRepo::contact_for_group(&hit.group_id)` instead of
`sender_pk`. Cache the result for the request if profiling demands it
— the `messages` query can trivially JOIN `contacts` server-side, but
we keep the lookup in Rust to stay on the existing query shape.

### L3.b — Surface `MessageRecord.row_id` (item #7)

`MessageRecord::project` already receives `row_id: i64` and drops it.
Add `pub row_id: i64` to `MessageRecord`, populate in `project()`. UI
uses this for scroll anchoring, mark_read cursor targeting, and trace
correlation. Same addition to `SearchHitRecord` via its inner
`MessageRecord`.

### L4.a — `daemon::clock::now_unix_seconds` (item #6)

Hoist into a new module `crates/core/src/daemon/clock.rs`
(`pub(crate)`). Four duplicate sites converge:

- `daemon/inbound.rs::now_unix_seconds` → removed, `use daemon::clock::*`.
- `daemon/dispatch.rs` (lines 121–123 inlined version) → use helper.
- Three integration test copies → use helper via `test_exports`.

Add `test_exports::now_unix_seconds` behind `feature = "test-harness"`.

### L4.b — Fixed-width group_id on `ReceiveOutcome::New` (item #9)

Narrow fix: change `ReceiveOutcome::New`'s `group_id: Vec<u8>` field to
`group_id: [u8; 32]`. The receive path already receives a `&[u8]`
group_id resolved via `ContactRepo::get_group_id`; construction sites
do `group_id.try_into().map_err(|_| CoreError::Storage(
StorageErrorKind::Other("group_id must be 32 bytes".into())))?`
— surface the invariant violation rather than silently zero-fill.
Narrow, local change — do not touch `GroupId(Vec<u8>)` globally.

### L4.c — cargo-deny in CI (item #10)

Add to `.github/workflows/ci.yml` (or existing workflow):

```yaml
cargo-deny:
  name: cargo-deny check
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4
    - uses: EmbarkStudios/cargo-deny-action@v2
      with:
        log-level: warn
        command: check
        arguments: --all-features
```

Gates merge (required status check). `deny.toml` is already clean per
1.G's last commits (6fb715b).

### L4.d — `serial_test` for socket-path env tests (item #11)

Replace `crates/cli/src/ipc/resolve_socket_path` test Mutex with
`#[serial_test::serial]` attributes. Add `serial_test = "3"` as a
dev-dependency on `skattr-cli`. Pure test-only; no runtime dep change.

## Data flow — send path (post-L1)

```
CLI → IPC SendMessage
  → daemon::dispatch::send_message
      → ContactRepo::get_group_id
      → Group::load
      → Group::encrypt         (ratchet advances in memory)
      → Pool::transaction:
          → Group::save_in_tx
          → MessageRepo::insert_in_tx
              (UNIQUE may fire → CoreError::Storage(DuplicateMessage))
          → OutboxRepo::insert_in_tx
      → (if duplicate: drop Group, return SendStatus::Delivered)
      → (if ok: hub.send → ACK/timeout → Delivered|Queued)
```

## Data flow — receive path (post-L1)

```
Transport → PeerConnection → InboundDispatch::dispatch
  → daemon::inbound::dispatch_for_group
      → Group::load
      → Group::decrypt         (ratchet advances in memory)
      → Pool::transaction:
          → Group::save_in_tx
          → receive_in_tx       (seen-messages dedup + MessageRepo::insert_in_tx)
      → events_tx.send(MessageReceived)   // outside tx, best-effort
```

## Testing plan

**L1 — Storage correctness:**

- `send_message_rolls_back_group_save_on_insert_failure` — inject a
  `MessageRepo` whose `insert_in_tx` returns `Err(DuplicateMessage)`,
  assert the on-disk `mls_groups` row did not advance its epoch.
- `receive_rolls_back_group_save_on_persist_failure` — mirror on
  receive, via a seen-messages repo that errors.
- `duplicate_envelope_id_maps_to_send_delivered` — seed a row with
  envelope_id X, attempt `send_message` that produces X, assert
  `SendStatus::Delivered`. (In practice the IPC path generates a fresh
  MessageId per call, so this test uses the repo directly.)
- `migration_0007_backfill_populates_envelope_id_and_enforces_uniq` —
  seed N rows with `envelope_id = NULL`, run migration, assert
  backfilled + unique index enforced via trigger.
- `backfill_body_text_single_transaction` — assert N fsyncs drop to 1
  (indirect: time or sqlite pragma `journal_size_limit` side effect).

**L2 — Error taxonomy:**

- `core_error_kind_is_pure_structural_match` — unit test per subsystem,
  constructs each typed variant, asserts the expected
  `DaemonErrorKind`. Zero `str::contains` in production code
  (enforced by a deny-list grep in CI or as a clippy-like note).
- `invalid_argument_returns_exit_code_2` — CLI integration test feeds
  `skattr prune --contact X --keep-last 3 --before-ts 0`, asserts exit
  code 2 + stderr mentions "exactly one of".

**L3 — IPC/API polish:**

- `search_messages_unscoped_resolves_outgoing_contact_via_group` — seed
  an outgoing row (sender = local), unscoped search, assert
  `record.contact == peer` (not local pubkey). Direct regression of
  item #1.
- `contact_for_group_returns_peer_for_2_member_group` — unit test on
  `ContactRepo`.
- `message_record_surfaces_row_id` — assert `record.row_id` matches
  the DB row id for `recent_messages`, `search_messages`, and
  `export_history`.

**L4 — Hygiene & infra:**

- `now_unix_seconds_is_monotonic_and_nonzero` — trivial smoke test.
- `cargo deny check` as a CI job that must pass.
- `serial_test` attributes verified by running the socket-path tests
  in parallel CI matrix (no regressions in concurrent test runs).

## Risks & mitigations

| Risk | Mitigation |
|------|-----------|
| `MlsProvider::save_in_tx` requires restructuring OpenMLS storage | OpenMLS's `StorageProvider` trait writes to blobs we already own. We wrap, don't subclass. If this turns out to require OpenMLS-upstream changes, fall back to **Option B** from Q2 (insert-before-save reorder) as an in-plan pivot. |
| Migration 0007 backfill on large history | Phase 1 has no production data. `backfill_body_text` already establishes the tx pattern; `backfill_envelope_id` copies it. |
| L2 subsystem refactor touches tests asserting on error strings | Inventory affected tests in the first commit of L2; update in the same commit as the subsystem refactor. |
| cargo-deny job wedges on a transitive advisory | `deny.toml` documents ignored advisories; any new ones get an ADR before being added. |
| `serial_test` as new dev-dep | Audit for transitive advisories via the new cargo-deny CI job. |

## Exit criteria

- `cargo fmt --check` clean.
- `cargo clippy --all-targets --all-features -- -D warnings` clean.
- `cargo test` + `cargo test --release -- --ignored` green.
- `cargo deny check` clean, both locally and in CI.
- All 11 kickoff items closed, each with a commit or PR reference in
  the CHANGELOG entry.
- CLAUDE.md's "Repository state" paragraph extended with a "Phase 1.H"
  sub-paragraph matching the 1.A–1.G style.
- No `str::contains` in `CoreError::kind()` (grep check).
- `docs/superpowers/kickoffs/2026-04-24-phase-1h-hardening.md` archived
  or cross-referenced from the new "next phase" kickoff.

## Open questions

None pinned. If `MlsProvider::save_in_tx` proves infeasible mid-
implementation, fall back to Option B (reorder: insert-first,
group.save-second) per the Q2 discussion.
