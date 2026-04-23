# Phase 1.G — Message Storage & Search Design

**Status:** Approved 2026-04-23. Scope locked by `docs/superpowers/specs/2026-04-21-phase-1-decomposition.md` §1.G.
**Depends on:** 0.D (storage Pool, MessageRepo), 1.E (delivery::receiver write site, daemon broadcast bus), 1.F (IPC layer, MessageRecord wire type, contacts.group_id, daemon dispatch).
**Exit criterion (verbatim from decomposition):** "FTS5 virtual table populated; `messages::recent / search / unread_count / export` APIs + `skattr tail` / `skattr search`."

## 1. Scope

In scope:

- Migration 0006 — three new columns on `messages` (`body_text`, `mls_generation`, `ts_daemon_recv`), recreated `messages_fts` virtual table referencing `body_text`, three FTS5 sync triggers, two new covering indexes, and a `read_state` table for per-group last-read pointers.
- One-shot `MessageRepo::backfill_body_text` helper invoked at daemon startup; decodes any pre-1.G text-kind row that still has `body_text IS NULL`. Phase 1 has no production users so the cost is bounded.
- `MessageRepo` API expansion: `insert(InsertParams)` (signature change), `search`, `unread_count`, `mark_read`, `export_page`, `prune_before`, `prune_keep_last`.
- `ReadStateRepo` (new module `storage/read_state.rs`) — `get`, `set` for `(group_id, last_read_message_id, updated_at)`.
- `delivery::receiver::receive` populates `mls_generation` (= `GroupEpoch.as_u64()` post-decrypt) and `ts_daemon_recv` (= `now()`); broadcasts `Event::MessageReceived { contact, record }` after persist+ACK.
- `daemon::dispatch::send_message` populates the same two fields on the local outgoing insert, captured post-encrypt.
- `daemon::dispatch::recent_by_contact` `ORDER BY` upgraded from 1.F's placeholder `(id DESC)` to `(mls_generation DESC, id DESC)`.
- Four new IPC commands: `SearchMessages`, `MarkRead`, `PruneHistory`, `ExportHistory`. Matching `CommandResult` variants. New `EventFilter::Messages { contact }` and `Event::MessageReceived`. New `DaemonErrorKind::SearchSyntax`.
- Daemon-owned hourly retention sweep (`tokio::spawn` task in `Daemon::run`); ticks every 3600 s; deletes any row where `ts_daemon_recv < now() - retention_days * 86400`; respects `[history] retention_days = 0` no-op default.
- CLI: `skattr search`, `skattr export`, `skattr prune`; `skattr tail --follow` upgraded from 1.F stub.
- Config: new `[history]` section with `retention_days` (default 0 = infinite).
- 100k-message FTS benchmark, `#[ignore]`-gated; informational p50/p95/p99 output, asserts p95 < 50 ms.

Out of scope (deferred to later phases or explicitly rejected):

- Multi-member group ordering with packed `(epoch, generation)`. 2-member groups + `id DESC` tie-break is sufficient for Phase 1; Phase 2 multi-member can extend the column type if it actually matters.
- `--raw` FTS5 query mode that bypasses tokenize-and-AND. Power-user escape hatch deferred until someone asks.
- File / reaction / edit / delete kind handling in FTS or export. Phase 1 only ships `Kind::Text` over the wire; Phase 3.E expands the kinds.
- TUI scrollback / pagination / line editing in `tail`. Phase 2 UI work owns the rich CLI experience.
- Encrypted export. Export inherits the same trust as the data-dir backup story — anyone who can read the export file can read the database; the user controls both.
- Wire-level read receipts. `read_state` is local-only; no `MessageRead` IPC event broadcasts to the peer.
- Per-contact retention overrides. Single global `retention_days`. Per-contact knob can land in Phase 2 with the UI.
- Daemon writes to user-controlled paths during export. Pagination over IPC + CLI-side file write keeps the daemon (which holds vault keys in process memory) from being tricked into clobbering arbitrary files.

## 2. Decisions locked during brainstorming

| # | Decision | Rationale |
|---|----------|-----------|
| D1 | **Read tracking = `read_state(group_id PK, last_read_message_id, updated_at)` table.** `unread_count = COUNT(*) WHERE group_id=? AND id > last_read_message_id`. | Cheap (one row per group), conventional (Slack/Discord shape), avoids per-message UPDATE storms. The `read_state` table is the natural home for future per-conversation cursor metadata (mute, pinned). |
| D2 | **Retention = daemon-owned hourly sweep + manual `skattr prune`.** Default `retention_days = 0` makes the sweep a no-op. | Manual command preserves user agency and is required for tests. Hourly sweep makes "set retention to 90 days" actually work without cron. Insert-time pruning (rejected) would put a DELETE in the hot path of every receive. |
| D3 | **Search query syntax = tokenize-and-AND.** Whitespace-split user input, wrap each token in FTS5-escaped double quotes, join with ` AND `. Empty after escape → return zero results without hitting FTS5. | Matches user expectation across every messaging app they have used. Hides FTS5 grammar (which most users will not learn). Power-user `--raw` flag deferred. Parameter binding everywhere — never string-interpolate the query. |
| D4 | **`tail --follow` = `Subscribe` to `Event::MessageReceived` with server-side `EventFilter::Messages { contact }`.** Receiver emits the event after persist+ACK. CLI renders one line per event using the same formatter as one-shot tail. | Closes the Phase 1 demo loop without making `tail` a refresh-on-press command. Server-side filtering keeps the broadcast bus from sending every contact's traffic to every CLI subscriber. |
| D5 | **Export = `--format json\|text`, default JSON.** Daemon paginates `Vec<MessageRecord>` over IPC; CLI loops until `next_after_id` is `None`, writes the file itself. | JSON matches 1.F's `--json` global flag. Pagination stays inside 1.F's 1 MiB IPC body cap (D3) without inventing a streaming pattern. CLI-side file write avoids letting the daemon clobber user-controlled paths. |
| D6 | **Search ranking = FTS5 BM25 + `id DESC` tie-break.** `--newest-first` flips to pure `id DESC` ordering. Never sort by `ts_envelope`. | BM25 is exactly what FTS5 is for. `id DESC` (autoincrement) is the per-side write-order proxy CLAUDE.md endorses; `ts_envelope` is display-only. |
| D7 | **FTS sync = SQL triggers off a `body_text` mirror column on `messages`.** AFTER INSERT / DELETE / UPDATE OF body_text, kind. Triggers WHERE-guard on `kind='text' AND body_text IS NOT NULL`. | Atomic with the row write; can't drift; no CBOR decode in SQL. The mirror column is populated by Rust at insert; the trigger only copies it into the FTS index. |
| D8 | **`mls_generation = GroupEpoch.as_u64()`** captured at the encrypt site (sender) and decrypt site (receiver). Stored as `INTEGER NOT NULL DEFAULT 0` on `messages`. | Sufficient for 2-member groups where every member action rolls the epoch. Multi-member fine-grained ordering (per-leaf generation) can extend the schema in a future migration if Phase 2 needs it; today, the wire schema is already `mls_generation: u64` so the encoding stays additive. |

## 3. Architecture

```
┌───────────────────────────────────────────────────────────────┐
│ delivery::receiver::receive                                   │
│   MLS decrypt → (envelope, mls_generation = epoch.as_u64())   │
│   MessageRepo::insert(InsertParams { ts_daemon_recv = now() })│
│   ACK                                                          │
│   broadcast Event::MessageReceived { contact, record }        │
└──────────────────────┬────────────────────────────────────────┘
                       │
                       ▼
┌───────────────────────────────────────────────────────────────┐
│ storage::messages::MessageRepo (expanded)                     │
│   insert(InsertParams)                                         │
│   recent_by_contact (ORDER BY (mls_generation DESC, id DESC)) │
│   search(query, group?, limit, offset, newest_first)          │
│   unread_count(group)                                          │
│   mark_read(group, up_to_id)                                  │
│   export_page(group, after_id, limit)                         │
│   prune_before(group?, before_ts_recv)                        │
│   prune_keep_last(group, keep)                                │
│   backfill_body_text()  [startup one-shot]                    │
└──────────┬───────────────────────────┬────────────────────────┘
           │                           │
           │ INSERT / DELETE / UPDATE  │
           ▼                           ▼
┌──────────────────────────┐  ┌─────────────────────────────────┐
│ messages (table)         │  │ messages_fts (FTS5 ext-content) │
│   ... existing cols ...  │  │   body_text                      │
│   body_text TEXT          │──▶  content='messages'              │
│   mls_generation INT      │  │   content_rowid='id'             │
│   ts_daemon_recv INT      │  │   tokenize='unicode61'           │
└──────────────────────────┘  └─────────────────────────────────┘
              ▲    via SQL triggers (ai/ad/au)
              │
┌──────────────────────────┐
│ read_state               │
│   group_id PK            │
│   last_read_message_id   │
│   updated_at             │
└──────────────────────────┘

┌───────────────────────────────────────────────────────────────┐
│ daemon::dispatch                                              │
│   SearchMessages → MessageRepo::search → SearchHitRecord[]    │
│   MarkRead → ReadStateRepo::set                                │
│   PruneHistory → MessageRepo::prune_*                          │
│   ExportHistory → MessageRepo::export_page → MessageRecord[]   │
│   Subscribe(Messages) → broadcast filter → MessageReceived    │
└───────────────────────────────────────────────────────────────┘

┌───────────────────────────────────────────────────────────────┐
│ daemon::retention (new)                                       │
│   spawn(sweep_loop(handle, every 3600s))                      │
│   if retention_days > 0: prune_before(now - days*86400)       │
└───────────────────────────────────────────────────────────────┘

┌───────────────────────────────────────────────────────────────┐
│ skattr CLI                                                    │
│   search / export / prune (one-shot Execute)                  │
│   tail [--follow] (Subscribe + render)                         │
└───────────────────────────────────────────────────────────────┘
```

## 4. Schema — migration 0006

Full SQL of `crates/core/src/storage/migrations/0006_history_search.sql`:

```sql
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz AB
--
-- Skattr storage schema, version 6.
-- Phase 1.G: wire FTS5, persist mls_generation + ts_daemon_recv, add read_state.

-- New columns on messages.
-- body_text mirrors the decoded body of text-kind messages so SQL triggers can
-- populate messages_fts without decoding CBOR. NULL for non-text kinds.
ALTER TABLE messages ADD COLUMN body_text TEXT;
-- mls_generation = GroupEpoch.as_u64() captured at encrypt/decrypt site.
ALTER TABLE messages ADD COLUMN mls_generation INTEGER NOT NULL DEFAULT 0;
-- ts_daemon_recv = local clock at the moment the daemon persisted the row.
-- Authoritative for retention ordering. Display still uses ts (envelope ts).
ALTER TABLE messages ADD COLUMN ts_daemon_recv INTEGER NOT NULL DEFAULT 0;

-- Covering index for tail/recent queries (ORDER BY (mls_generation DESC, id DESC)).
CREATE INDEX IF NOT EXISTS idx_messages_group_gen
    ON messages(group_id, mls_generation DESC, id DESC);

-- Index for retention sweep (range scan on ts_daemon_recv).
CREATE INDEX IF NOT EXISTS idx_messages_ts_recv
    ON messages(ts_daemon_recv);

-- Recreate messages_fts so its declared column matches messages.body_text.
-- The 0001 schema declared "body" which is not a real column on messages,
-- making the external-content link broken. Drop + recreate is safe: no
-- production users yet, and the trigger set below repopulates from body_text.
DROP TABLE IF EXISTS messages_fts;
CREATE VIRTUAL TABLE messages_fts USING fts5(
    body_text,
    content='messages',
    content_rowid='id',
    tokenize='unicode61'
);

-- AFTER INSERT: index any text row that arrived with a populated body_text.
CREATE TRIGGER IF NOT EXISTS messages_ai_text
    AFTER INSERT ON messages
    WHEN NEW.kind = 'text' AND NEW.body_text IS NOT NULL
BEGIN
    INSERT INTO messages_fts(rowid, body_text)
        VALUES (NEW.id, NEW.body_text);
END;

-- AFTER DELETE: drop from FTS only if the row was indexed.
CREATE TRIGGER IF NOT EXISTS messages_ad_text
    AFTER DELETE ON messages
    WHEN OLD.kind = 'text' AND OLD.body_text IS NOT NULL
BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, body_text)
        VALUES('delete', OLD.id, OLD.body_text);
END;

-- AFTER UPDATE OF body_text or kind: replay delete+reinsert.
-- Triggers do not fire on unrelated column updates (e.g. mark_delivered).
CREATE TRIGGER IF NOT EXISTS messages_au_text
    AFTER UPDATE OF body_text, kind ON messages
BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, body_text)
        SELECT 'delete', OLD.id, OLD.body_text
        WHERE OLD.kind = 'text' AND OLD.body_text IS NOT NULL;
    INSERT INTO messages_fts(rowid, body_text)
        SELECT NEW.id, NEW.body_text
        WHERE NEW.kind = 'text' AND NEW.body_text IS NOT NULL;
END;

-- Per-group last-read pointer. unread_count = COUNT(*) of rows with
-- group_id matching and id > last_read_message_id.
CREATE TABLE IF NOT EXISTS read_state (
    group_id BLOB PRIMARY KEY,
    last_read_message_id INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

-- Bump schema_version row.
INSERT OR REPLACE INTO schema_version (version) VALUES (6);
```

Notes:

- The `DROP TABLE messages_fts` is correct because the existing 0001 declaration is broken (column name `body` does not exist on `messages`). No production users exist yet; the index is empty in every real database.
- For dev databases that already have rows pre-1.G, all rows pass the `body_text IS NULL` predicate and stay out of FTS until startup backfill runs.
- The `idx_messages_group_gen` index uses DESC ordering on both ranking columns; SQLite uses it directly for `recent_by_contact` ORDER BY without an extra sort step.

## 5. APIs — `MessageRepo` and `ReadStateRepo`

### 5.1 Insert path change

Today's signature:

```rust
pub fn insert(&self, group_id: &[u8], sender: &[u8], envelope: &Envelope) -> Result<i64>;
```

After 1.G:

```rust
pub struct InsertParams<'a> {
    pub group_id: &'a [u8],
    pub sender: &'a [u8],
    pub envelope: &'a Envelope,
    pub mls_generation: u64,
    pub ts_daemon_recv: i64,
}

pub fn insert(&self, p: InsertParams<'_>) -> Result<i64>;
```

`insert` extracts `body_text` from the envelope (`Some(body.clone())` for `Kind::Text`, `None` otherwise) and writes it to the new column. Triggers fire automatically. Two call sites update: `delivery::receiver::receive` and `daemon::dispatch::send_message`.

### 5.2 New query / mutation APIs

```rust
pub struct SearchHit {
    pub message: StoredMessage,
    pub bm25: f64,
    pub snippet: String,  // FTS5 snippet(messages_fts, 0, '\u{2}', '\u{3}', '...', 32)
}

impl<'p> MessageRepo<'p> {
    /// Search across all groups (or one) using tokenize-and-AND.
    /// Empty query (or empty after escape) returns Ok(vec![]) without hitting FTS5.
    /// `newest_first = true` overrides BM25 with `id DESC`.
    pub fn search(
        &self,
        query: &str,
        group_id: Option<&[u8]>,
        limit: usize,
        offset: usize,
        newest_first: bool,
    ) -> Result<Vec<SearchHit>>;

    /// COUNT(*) of messages in `group_id` with `id > last_read_message_id`,
    /// where the cursor comes from `read_state`. Returns total messages if no cursor exists.
    pub fn unread_count(&self, group_id: &[u8]) -> Result<u64>;

    /// One page of messages (oldest-first) for export.
    /// `after_id = None` starts from the beginning. Caller loops until empty.
    pub fn export_page(
        &self,
        group_id: &[u8],
        after_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<StoredMessage>>;

    /// Delete rows older than `before_ts_recv`. `group_id = None` prunes globally.
    /// Returns the number of rows deleted (cascades to messages_fts via triggers).
    pub fn prune_before(
        &self,
        group_id: Option<&[u8]>,
        before_ts_recv: i64,
    ) -> Result<u64>;

    /// Keep the most recent `keep` rows in `group_id`; delete the rest.
    /// Returns the number of rows deleted.
    pub fn prune_keep_last(&self, group_id: &[u8], keep: u64) -> Result<u64>;

    /// One-shot startup helper: decode CBOR for any text-kind row that has
    /// body_text IS NULL, populate body_text + indirectly trigger FTS index.
    /// Returns the number of rows backfilled. Idempotent.
    pub(crate) fn backfill_body_text(&self) -> Result<u64>;
}
```

### 5.3 `recent_by_contact` ordering upgrade

1.F adds `recent_by_contact(contact_pk: &PublicKey, limit: usize) -> Result<Vec<StoredMessage>>` with `ORDER BY id DESC` placeholder. 1.G upgrades the SQL to:

```sql
SELECT ... FROM messages
WHERE group_id = (SELECT group_id FROM contacts WHERE identity_pubkey = ?1)
ORDER BY mls_generation DESC, id DESC
LIMIT ?2;
```

`idx_messages_group_gen` makes this an index-only scan.

### 5.4 `ReadStateRepo` (new module)

`crates/core/src/storage/read_state.rs`:

```rust
pub struct ReadStateRepo<'p> {
    pool: &'p Pool,
}

impl<'p> ReadStateRepo<'p> {
    pub fn new(pool: &'p Pool) -> Self;
    /// Returns Some(last_read_message_id) if a cursor exists, None otherwise.
    pub fn get(&self, group_id: &[u8]) -> Result<Option<i64>>;
    /// Upsert. Idempotent. `updated_at` set to now() by the caller.
    pub fn set(&self, group_id: &[u8], last_read_message_id: i64, updated_at: i64) -> Result<()>;
}
```

Re-exported from `storage::mod` as `pub(crate) use read_state::ReadStateRepo;` and as `pub use` under `test_exports` for integration tests.

### 5.5 FTS5 query escaper

Lives in `storage::messages` as a `pub(super) fn`:

```rust
/// Tokenize-and-AND: split on whitespace, wrap each token in FTS5-escaped
/// double quotes (FTS5 doubles internal `"` to `""`), join with ` AND `.
/// Returns None if the result would be empty (caller short-circuits to []).
pub(super) fn fts5_tokenize_and_and(query: &str) -> Option<String>;
```

Examples:

- `arti` → `Some("\"arti\"".into())`
- `arti tor` → `Some("\"arti\" AND \"tor\"".into())`
- `she said "hi"` → `Some("\"she\" AND \"said\" AND \"\"\"hi\"\"\"".into())` (FTS5 escape doubles the `"`)
- `   ` → `None`
- `` → `None`

## 6. IPC additions (server + wire)

### 6.1 New `Command` variants

Append to `daemon::commands::Command`:

```rust
pub enum Command {
    // ... 1.D / 1.F variants ...

    SearchMessages {
        query: String,
        contact: Option<PublicKey>,
        limit: u32,
        offset: u32,
        newest_first: bool,
    },
    MarkRead {
        contact: PublicKey,
        up_to_message_id: i64,
    },
    PruneHistory {
        contact: Option<PublicKey>,
        before_ts_recv: Option<i64>,
        keep_last: Option<u64>,
    },
    ExportHistory {
        contact: PublicKey,
        after_id: Option<i64>,
        limit: u32,
    },
}
```

### 6.2 New `CommandResult` variants

```rust
pub enum CommandResult {
    // ... 1.D / 1.F variants ...

    SearchResults(Vec<SearchHitRecord>),
    MarkedRead { up_to: i64 },
    Pruned { rows_deleted: u64 },
    ExportPage {
        records: Vec<MessageRecord>,
        next_after_id: Option<i64>,
    },
}

pub struct SearchHitRecord {
    pub record: MessageRecord,
    pub bm25: f64,
    pub snippet: String,
}
```

### 6.3 Event filter / event variants

```rust
pub enum EventFilter {
    // ... 1.F variants ...
    Messages { contact: Option<PublicKey> },
}

pub enum Event {
    // ... 1.F variants ...
    MessageReceived {
        contact: PublicKey,
        record: MessageRecord,
    },
}
```

Server-side filter logic (in `daemon::ipc::server` per-connection task):

```
for each broadcast Event:
    match (sub_filter, event):
        (EventFilter::Messages { contact: None }, Event::MessageReceived { .. }) => emit
        (EventFilter::Messages { contact: Some(c) }, Event::MessageReceived { contact, .. })
            if c == contact => emit
        _ => skip
```

### 6.4 Error model addition

```rust
pub enum DaemonErrorKind {
    // ... 1.F variants ...
    SearchSyntax,  // empty query after tokenize-and-AND, or FTS5 rejected the bound query.
}
```

`SearchSyntax` is rare in normal use: `fts5_tokenize_and_and` returns `None` for whitespace-only input, which the dispatcher converts to `Ok(SearchResults(vec![]))` instead of an error. The `SearchSyntax` variant exists for the unlikely case where SQLite FTS5 rejects an escaped query at runtime; the rich error stays in the server log.

### 6.5 Dispatch invariants per command

| Command | Dispatch path |
|---|---|
| `SearchMessages` | Resolve `contact` → `group_id` via `ContactRepo` if present (`ContactNotFound`/`ContactAmbiguous` from 1.F apply). `MessageRepo::search(&query, group_id.as_deref(), limit, offset, newest_first)`. Project each `SearchHit` to `SearchHitRecord` (reuses 1.F's `MessageRecord` projection). |
| `MarkRead` | Resolve contact → `group_id`. `ReadStateRepo::set(group_id, up_to_message_id, now())`. Returns `MarkedRead { up_to }`. |
| `PruneHistory` | Resolve `contact` → optional `group_id`. Validate exactly one of `before_ts_recv` / `keep_last` is `Some`. Call the matching `MessageRepo::prune_*` and return `Pruned { rows_deleted }`. |
| `ExportHistory` | Resolve contact → `group_id` (mandatory). `MessageRepo::export_page(group_id, after_id, limit.min(EXPORT_PAGE_MAX))` where `EXPORT_PAGE_MAX = 1000` keeps the response under the 1 MiB IPC body cap. `next_after_id = records.last().map(|r| r.id)` if the page was full, else `None`. |

## 7. Daemon retention sweep

New module `crates/core/src/daemon/retention.rs`:

```rust
pub(crate) fn spawn_sweep(
    handle: Arc<DaemonHandle>,
    retention_days: u32,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()>;
```

Internals:

```
loop {
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(3600)) => {
            if retention_days == 0 { continue; }
            let cutoff = now() - i64::from(retention_days) * 86400;
            match MessageRepo::new(&handle.pool).prune_before(None, cutoff) {
                Ok(n) if n > 0 => tracing::info!(rows = n, cutoff_ts_recv = cutoff, "retention sweep deleted rows"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "retention sweep failed; will retry next tick"),
            }
        }
        _ = shutdown.changed() => break,
    }
}
```

`Daemon::run` spawns this task right after subsystem initialisation. Test path: a `#[cfg(any(test, feature = "test-harness"))]` constructor that takes a `tick_interval: Duration` so integration tests can use 50 ms ticks.

## 8. Delivery receiver wiring

`crates/core/src/delivery/receiver.rs::receive` change:

```
// Existing flow: ts-window check → dedup check → MLS decrypt → persist → ACK.
// 1.G additions (after MLS decrypt, before persist):

let mls_generation = group.epoch().as_u64();
let ts_daemon_recv = i64::try_from(unix_now_secs()).unwrap_or(i64::MAX);
let row_id = messages.insert(InsertParams {
    group_id: &gid,
    sender: &sender_pk,
    envelope: &envelope,
    mls_generation,
    ts_daemon_recv,
})?;

// Existing ACK send.
// 1.G addition (after successful ACK):
let record = MessageRecord::project(row_id, &envelope, mls_generation, ts_daemon_recv, Direction::In);
let _ = handle.events.send(Event::MessageReceived { contact: contact_pk, record });
// `events.send` is a tokio::sync::broadcast; `_ = ` because zero subscribers is a normal state.
```

`MessageRecord::project` is a small helper added next to the existing wire types in `daemon::commands` that maps `(StoredMessage-equivalent fields, direction)` to the wire type. Both the receiver path and `daemon::dispatch::recent_by_contact` use it, so the `mls_generation: 0` placeholder from 1.F's `recent_by_contact` is removed in the same commit.

## 9. CLI surface

```
skattr search <query> [--contact <name|prefix>] [--limit N] [--offset N] [--newest-first] [--json]
skattr export <contact> [--format json|text] [--output <path>]
skattr prune [--contact <name|prefix>] [--before <RFC3339>] [--keep-last N]
skattr tail [--contact <name|prefix>] [--limit N] [--follow] [--json]
```

### 9.1 `skattr search`

- `<query>`: free-form; passed through to `Command::SearchMessages` as-is. Daemon tokenize-and-ANDs.
- `--contact <name|prefix>`: optional; absent = search across all groups.
- `--limit N` (default 20), `--offset N` (default 0): standard pagination.
- `--newest-first`: pass `newest_first = true` (overrides BM25).
- `--json`: emit the raw `Vec<SearchHitRecord>` as JSON.

Default human output, one line per hit:

```
[2026-04-23T14:02:11Z] alice (id=42 epoch=7) ...the merge conflict was easier than expected...
```

The snippet uses FTS5's default ellipsis (`...`) and 32-token window.

### 9.2 `skattr export`

- `<contact>`: required (single-group export).
- `--format json|text`: default `json`.
- `--output <path>`: required; CLI opens file with `O_CREAT|O_EXCL` (refuse to overwrite existing).
- CLI loop: `after_id = None`; repeatedly issue `Command::ExportHistory { contact, after_id, limit: 1000 }`; write each `MessageRecord` (JSON line or formatted text) to the output file; advance `after_id = response.next_after_id`; stop when `next_after_id` is `None`.

JSON shape (one JSON array, written incrementally as JSON Lines is *not* used to keep the file a valid single JSON document):

```
[
  { "id": 1, "direction": "in", "kind": { "Text": { "body": "hi" } }, ... },
  { "id": 2, ... },
  ...
]
```

Plaintext shape, one line per message, oldest-first:

```
[2026-04-23T14:02:11Z] ab12cd34: hello world
[2026-04-23T14:02:14Z] ef56789a: hi back
```

Sender prefix is the first 8 hex chars of the sender Ed25519 pubkey.

### 9.3 `skattr prune`

- `--contact <name|prefix>`: optional; absent = prune globally.
- `--before <RFC3339>`: parse to `i64` seconds-since-epoch; pass as `before_ts_recv`.
- `--keep-last N`: pass as `keep_last`.
- Exactly one of `--before` / `--keep-last` must be present (CLI-side validation).
- Output: `Deleted N messages.` (or `--json: { "rows_deleted": N }`).

### 9.4 `skattr tail --follow`

- Without `--follow`: identical to 1.F (one-shot `RecentMessages`).
- With `--follow`: after the one-shot fetch, send `Subscribe(EventFilter::Messages { contact: <resolved> })`; render each `Event::MessageReceived` as one line (same formatter as the one-shot fetch); `Ctrl-C` clean-exits the subscription before closing the socket.

## 10. Config additions

Append to `Config`:

```rust
#[derive(Debug, Deserialize, Serialize)]
pub struct HistoryConfig {
    /// Days of history to retain. 0 = infinite (default; sweep no-ops).
    #[serde(default)]
    pub retention_days: u32,
}

impl Default for HistoryConfig {
    fn default() -> Self { Self { retention_days: 0 } }
}
```

TOML:

```toml
[history]
retention_days = 0
```

`Config::load` reads `[history]` with the existing `serde(default)` flow; missing section = defaults. Unknown keys still hard-error per 1.F's D8 setting.

## 11. Performance plan

- `idx_messages_group_gen` covers tail / `recent_by_contact` ordering and `unread_count` range scans.
- `idx_messages_ts_recv` covers `prune_before`'s range delete.
- `messages_fts` with default BM25 + unicode61 tokenizer handles up to ~1M rows on commodity hardware in well under 50 ms; the 100k target leaves headroom.
- The age-encrypted SQLite layer adds a constant per-statement cost dominated by AEAD; FTS5 sees encrypted pages just like the rest of the database.

100k benchmark (`crates/core/tests/fts_search_p95.rs`):

- Plain `#[test] #[ignore]` integration test (no `criterion` dependency — keeps the dep graph small and matches the cadence of `delivery_real_tor` in 1.E).
- Generates 100k synthetic text messages with bodies drawn from a 200-word vocabulary, seeded via `rand::SeedableRng` (using the workspace `rand 0.8` dep already present in `core`).
- Runs 50 random 1-token and 50 random 2-token AND queries; records p50 / p95 / p99 via `eprintln!` so `cargo test -- --nocapture` prints the distribution.
- Asserts p95 < 50 ms; the assertion is the only failure mode, which keeps the `#[ignore]`-gated test usable as both bench and regression guard.

Run on developer machines with `cargo test -p skattr-core --release --test fts_search_p95 -- --ignored --nocapture`. Not CI-gated.

## 12. Testing strategy

### 12.1 Unit tests (per-module)

`crates/core/src/storage/messages.rs` `mod tests`:

- `insert_populates_body_text_for_text_kind`
- `insert_leaves_body_text_null_for_non_text`
- `insert_writes_mls_generation_and_ts_daemon_recv`
- `search_no_match_returns_empty`
- `search_single_token_finds_one`
- `search_multi_token_ands`
- `search_scoped_to_group_id`
- `search_newest_first_overrides_bm25`
- `search_empty_query_short_circuits`
- `unread_count_returns_total_when_no_cursor`
- `unread_count_returns_zero_after_cursor_advances_past_all`
- `mark_read_advances_cursor_idempotent`
- `export_page_yields_oldest_first_and_pages_correctly`
- `prune_before_deletes_old_rows_and_cascades_to_fts`
- `prune_keep_last_keeps_most_recent`
- `backfill_body_text_decodes_legacy_rows`
- `recent_by_contact_orders_by_generation_then_id`

`crates/core/src/storage/read_state.rs` `mod tests`:

- `get_returns_none_initially`
- `set_then_get_round_trips`
- `set_overwrites_existing_cursor`

`crates/core/src/storage/messages.rs` (helper):

- `fts5_tokenize_and_and_single_token`
- `fts5_tokenize_and_and_multi_token`
- `fts5_tokenize_and_and_escapes_quotes`
- `fts5_tokenize_and_and_empty_returns_none`
- `fts5_tokenize_and_and_whitespace_only_returns_none`

`crates/core/src/daemon/retention.rs`:

- `sweep_no_op_when_retention_days_zero`
- `sweep_deletes_rows_older_than_cutoff`
- `sweep_logs_warn_on_storage_error_and_continues` (mock pool that returns Err once)

### 12.2 Integration tests

`crates/tests/src/cli_search.rs`:

- Spawn one daemon (mocked transport via 1.E harness); seed three contacts; insert 12 messages across them.
- `Command::SearchMessages { query: "merge", contact: None, .. }` returns BM25-ranked hits.
- `Command::SearchMessages { query: "merge", contact: Some(alice_pk), .. }` filters to alice's group only.
- `Command::SearchMessages { query: "  ", .. }` returns empty `SearchResults` without an error.
- CLI `skattr search merge` (driven via a `Command::execute` test wrapper) prints the human format with the snippet.

`crates/tests/src/history_sweep.rs`:

- Spawn daemon with `retention_days = 1` and a `tick_interval = 50 ms` test override.
- Insert 10 rows: 5 with `ts_daemon_recv = now()`, 5 with `ts_daemon_recv = now() - 2*86400`.
- Wait two ticks; assert exactly 5 rows remain and `messages_fts` row count matches.

`crates/tests/src/cli_tail_follow.rs`:

- Spawn two daemons (1.E mocked-transport harness).
- Bob runs `Subscribe(EventFilter::Messages { contact: Some(alice_pk) })`.
- Alice sends two messages; assert Bob's subscription receives two `Event::MessageReceived` events, both with `record.direction == Direction::In`.

`crates/tests/src/cli_export.rs`:

- Single daemon; insert 2500 messages in one group (forces 3 export pages at `EXPORT_PAGE_MAX = 1000`).
- CLI `skattr export <contact> --format json --output /tmp/...`; parse the resulting file as `serde_json::Value`; assert array length 2500 and oldest-first ordering.

### 12.3 Benchmark (`#[ignore]`)

`crates/core/benches/fts_search.rs` as described in §11.

## 13. File layout

```
crates/core/src/storage/migrations/0006_history_search.sql       (new)
crates/core/src/storage/messages.rs                              (expand: InsertParams, search, unread_count, mark_read, export_page, prune_*, backfill_body_text, fts5_tokenize_and_and, mod tests)
crates/core/src/storage/read_state.rs                            (new: ReadStateRepo + tests)
crates/core/src/storage/mod.rs                                   (declare read_state; pub(crate) re-export ReadStateRepo; extend test_exports)
crates/core/src/storage/migrations.rs                            (extend ALL_MIGRATIONS with version=6; add migration_0006 test)
crates/core/src/daemon/commands.rs                               (Command + CommandResult + EventFilter + Event + DaemonErrorKind + SearchHitRecord variants; MessageRecord::project helper)
crates/core/src/daemon/dispatch.rs                               (Search/MarkRead/Prune/Export handlers; recent_by_contact ORDER BY upgrade)
crates/core/src/daemon/state.rs                                  (call retention::spawn_sweep + call MessageRepo::backfill_body_text once at startup)
crates/core/src/daemon/retention.rs                              (new: sweep_loop + spawn_sweep + tests)
crates/core/src/daemon/config.rs                                 (HistoryConfig + serde(default))
crates/core/src/daemon/mod.rs                                    (re-export new types where 1.F's mod boundary covers them)
crates/core/src/delivery/receiver.rs                             (capture mls_generation + ts_daemon_recv; emit Event::MessageReceived after ACK)
crates/core/src/error.rs                                         (CoreError::kind() maps a SearchSyntax-equivalent variant)
crates/core/src/lib.rs                                           (extend test_exports with ReadStateRepo, MessageRepo additions, retention helpers)
crates/cli/src/main.rs                                           (add search/export/prune subcommands; tail --follow upgrade)
crates/cli/Cargo.toml                                            (add `time = { version = "0.3", features = ["parsing", "formatting", "macros"] }` for `--before` RFC3339 parsing; no other new deps)
crates/tests/src/cli_search.rs                                   (new)
crates/tests/src/history_sweep.rs                                (new)
crates/tests/src/cli_tail_follow.rs                              (new)
crates/tests/src/cli_export.rs                                   (new)
crates/core/tests/fts_search_p95.rs                              (new — `#[test] #[ignore]` 100k bench)
```

All new files carry the standard GPLv3 license header per CLAUDE.md.

## 14. Open questions / follow-ups

Not blocking 1.G; tracked here so the implementation plan can skip re-deciding them.

- **Per-contact retention overrides.** Single global `retention_days` ships in 1.G; per-contact knob lands with the Phase 2 UI when there is a place to surface it.
- **`--raw` FTS5 query mode.** Power-user escape hatch deferred until requested.
- **Encrypted export.** Export inherits the same trust as the data dir; encrypted variant is a Phase 3 backup-story concern.
- **Snippet rendering tweaks.** The default FTS5 `snippet()` ellipsis and 32-token window are fine; UI work in Phase 2 may want richer markup.
- **Multi-member ordering.** When Phase 2 ships multi-member groups, decide whether to add a `mls_leaf_generation` column or pack `(epoch, generation)` into the existing `mls_generation` u64.
- **`backfill_body_text` migration mode.** Today it runs at every startup but is idempotent (only touches rows with `body_text IS NULL AND kind = 'text'`). If the per-startup query becomes a measurable cost, gate it on a "backfill_done" row in `schema_version` or similar.

## 15. Phase 1.G exit criteria (verifiable)

All of the following must be green before this sub-project merges to master:

1. `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` all pass on Linux and macOS CI.
2. Migration 0006 lands; `cargo test -p skattr-core --lib storage::migrations::tests::migration_0006` asserts the columns, indexes, triggers, and `read_state` table all exist.
3. After daemon startup on a database with pre-1.G text rows, no row matches `kind = 'text' AND body_text IS NULL` (`backfill_body_text` ran and reported `n > 0`).
4. `cargo test -p skattr-tests cli_search` exercises a full IPC round-trip (BM25 ordering, contact filter, empty-query short-circuit).
5. `cargo test -p skattr-tests history_sweep` exercises the daemon-owned retention sweep with a 50 ms test interval.
6. `cargo test -p skattr-tests cli_tail_follow` proves `Event::MessageReceived` reaches a CLI subscriber after `delivery::receiver` ACKs.
7. `cargo test -p skattr-tests cli_export` round-trips 2500 messages through paginated `ExportHistory` into a parseable JSON file with oldest-first ordering.
8. `cargo test -p skattr-core --release --test fts_search_p95 -- --ignored --nocapture` reports search p95 < 50 ms over 100k synthetic text messages.
9. `skattr prune --keep-last 10 --contact alice` deletes the expected rows; `SELECT COUNT(*) FROM messages_fts` drops by the same amount (proving trigger + cascade correctness).
10. `cargo deny check` passes with no new advisories or banned crates (the only new dep is `time = "0.3"` for CLI-side RFC3339 parsing; MIT/Apache-2.0 license is already on the allowlist).
11. CHANGELOG.md, CLAUDE.md status line, and `docs/ARCHITECTURE.md`'s "send one message" trace are updated to reflect persisted `mls_generation` / `ts_daemon_recv`, FTS-backed search, retention sweep, and the new IPC commands.
