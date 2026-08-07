# Phase 2.D Conversation View Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn 2.C's read-only conversation MVP into a working two-way text chat — composer with Enter-to-send and IME safety, optimistic outgoing bubbles with delivery state icons, scroll-back pagination via a `before_id` cursor, frozen "Unread" separator, debounced mark-read.

**Architecture:** Wire format extensions are strictly additive (new `Option` fields with `#[serde(default)]`, new `MessagesPage` variant alongside the existing `Messages(Vec)`). Send path captures the just-inserted `row_id` from the existing transaction and projects a `MessageRecord` into the IPC reply, so the UI's optimistic placeholder reconciles to the canonical row in one round-trip. Pagination is server-paged (`SELECT … WHERE id < ?before_id ORDER BY mls_generation DESC, id DESC LIMIT 50`); UI uses two `IntersectionObserver`s — top for `loadOlder`, bottom for `markReadIfAtBottom`. Mark-read separator anchors to the read cursor at conversation-open and is frozen until close+reopen.

**Tech Stack:** Rust 2021 + Tokio + rusqlite + ciborium (daemon). Svelte 5 + Tauri 2 + TypeScript + `@tanstack/svelte-virtual` (UI). Vitest + Playwright (tests). Lucide ISC icons (4 glyphs, bundled inline-SVG).

**Spec:** `docs/superpowers/specs/2026-05-02-phase-2d-conversation-view-design.md`

---

## File map

### Rust (additive only — no existing types reshaped)

| File | Action | Responsibility |
|---|---|---|
| `crates/core/src/daemon/commands.rs` | modify | `Command::RecentMessages` gains `before_id`, `paged`; `CommandResult::MessageSent` gains `record`; new `MessagesPage` variant; `ContactSummary` gains `group_state`, `last_read_row_id`; new `MlsGroupStateLabel` enum |
| `crates/core/src/daemon/dispatch.rs` | modify | `send_message` captures `row_id` + projects `record`; `recent_messages` accepts `before_id`/`paged` and branches; `list_contacts` populates new summary fields |
| `crates/core/src/storage/messages.rs` | modify | New `MessageRepo::recent_before(group_id, before_id, limit)` |
| `crates/core/src/storage/contacts.rs` | modify | New `ContactRepo::last_read_row_id(group_id) -> Option<i64>` (reads `read_state` table) |
| `crates/core/tests/wire_format_append_only.rs` | create | Snapshot test of `Command` + `CommandResult` variant lists |
| `crates/cli/src/main.rs` | unchanged | `MessageSent.record` ignored via `#[serde(default)]` |
| `crates/tests/src/cli_two_daemons.rs` | modify | Assert `record.is_some()` after send |
| `crates/tests/src/ui_send_roundtrip.rs` | create | `#[ignore]`-gated real-Tor end-to-end test |

### UI (`crates/ui/src-svelte/src/`)

| File | Action | Responsibility |
|---|---|---|
| `lib/styles/tokens.css` | modify | Add `--danger` (7th colour token) |
| `lib/icons/clock.svg` | create | Lucide ISC, inline SVG |
| `lib/icons/check.svg` | create | Lucide ISC |
| `lib/icons/check-check.svg` | create | Lucide ISC |
| `lib/icons/alert-triangle.svg` | create | Lucide ISC |
| `lib/icons/index.ts` | create | Re-export `?raw`-loaded SVG strings |
| `lib/components/DeliveryIcon.svelte` | create | Render one of 4 glyphs by status |
| `lib/components/UnreadSeparator.svelte` | create | Frozen `<hr>` + "Unread" label |
| `lib/components/SkeletonBubble.svelte` | create | Pulse-animated placeholder bubble |
| `lib/components/MessageBubble.svelte` | modify | Outgoing variant slots `<DeliveryIcon>` |
| `lib/components/VirtualMessageList.svelte` | modify | Top observer → `loadOlder`; bottom observer → `markReadIfAtBottom`; renders separator + skeletons |
| `lib/components/Composer.svelte` | create | Textarea + send button + IME/paste handlers |
| `lib/stores/delivery.ts` | create | `Map<message_id_hex, DeliveryStatus>` updated by `Event::DeliveryStatusChanged` |
| `lib/stores/conversation.ts` | modify | Optimistic + reconciliation + pagination + mark-read |
| `routes/+page.svelte` | modify | Wire `<Composer>` into the conversation pane |

### UI tests

| File | Action |
|---|---|
| `lib/components/DeliveryIcon.test.ts` | create |
| `lib/components/Composer.test.ts` | create |
| `lib/stores/delivery.test.ts` | create |
| `lib/stores/conversation.test.ts` | create |
| `tests/e2e/composer.spec.ts` | create |
| `tests/e2e/pagination.spec.ts` | create |

---

## Conventions used in this plan

- **Test-first.** Every Rust task that touches behaviour writes the failing test before the implementation. Each Vitest task does the same.
- **Frequent commits.** Each task ends with one commit. Commit messages follow the existing convention (`feat:`/`fix:`/`refactor:`/`test:` prefix; trailing `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` line).
- **No CLAUDE.md or CHANGELOG churn until Task 28.** Avoids merge-conflict noise mid-stream.
- **`cargo test -p skattr-core` requires `--features test-harness`** per the project's saved feedback memory. CI invokes it that way; humans should too.
- **`worktree`.** Per the kickoff prompt, this plan executes on a `phase-2d-conversation-view` branch in a dedicated worktree (created via `superpowers:using-git-worktrees` between this plan and Task 1).

---

## Phase A — Rust foundations

Wire-format additions land first as pure types + serde roundtrips, with no consumers. This lets the UI side proceed against the generated TS bindings while the dispatch logic is being built up alongside.

### Task 1: Add `MlsGroupStateLabel` + extend `ContactSummary`

**Files:**
- Modify: `crates/core/src/daemon/commands.rs`

- [ ] **Step 1: Write failing serde roundtrip test for the extended `ContactSummary`**

Append inside the existing `#[cfg(test)] mod tests { … }` block in `crates/core/src/daemon/commands.rs`:

```rust
#[test]
fn contact_summary_with_new_fields_round_trips_cbor() {
    let s = ContactSummary {
        pubkey: crate::identity::PublicKey([7; 32]),
        nickname: Some("bob".into()),
        onion: "bbbb.onion".into(),
        card_version: 1,
        added_at: 1_700_000_000,
        unread_count: 3,
        last_message_preview: Some("hi".into()),
        last_ts_recv: Some(1_700_000_500),
        group_state: Some(MlsGroupStateLabel::Active),
        last_read_row_id: Some(42),
    };
    let back: ContactSummary = roundtrip(&s);
    assert_eq!(back.group_state, Some(MlsGroupStateLabel::Active));
    assert_eq!(back.last_read_row_id, Some(42));
}

#[test]
fn contact_summary_decodes_legacy_payload_without_new_fields() {
    // Build a CBOR map missing `group_state` / `last_read_row_id`.
    let legacy_cbor = {
        let mut buf = Vec::new();
        let v = ciborium::value::Value::Map(vec![
            ("pubkey".into(), ciborium::value::Value::Bytes([0u8; 32].to_vec())),
            ("nickname".into(), ciborium::value::Value::Null),
            ("onion".into(), ciborium::value::Value::Text("o.onion".into())),
            ("card_version".into(), ciborium::value::Value::Integer(0.into())),
            ("added_at".into(), ciborium::value::Value::Integer(0.into())),
        ]);
        ciborium::ser::into_writer(&v, &mut buf).unwrap();
        buf
    };
    let back: ContactSummary = ciborium::de::from_reader(&legacy_cbor[..]).unwrap();
    assert_eq!(back.group_state, None);
    assert_eq!(back.last_read_row_id, None);
}
```

- [ ] **Step 2: Run test — must fail (`MlsGroupStateLabel` undefined, missing fields)**

Run:

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness contact_summary_with_new_fields 2>&1 | tail -30
```

Expected: build error referencing `MlsGroupStateLabel` and missing fields on `ContactSummary`.

- [ ] **Step 3: Add the enum + extend the struct**

In `crates/core/src/daemon/commands.rs`, immediately above `pub struct ContactSummary`:

```rust
/// Wire-safe stringly projection of `mls::state::GroupState`.
/// Mirrors the three concrete variants in `state_machine.rs` as
/// of Phase 1.C — `Active`, `PendingJoin`, `Corrupt`. Future
/// state-machine variants extend this enum at the same time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
pub enum MlsGroupStateLabel {
    Active,
    PendingJoin,
    Corrupt,
}
```

In `pub struct ContactSummary { … }` add (after the existing `last_ts_recv` field):

```rust
    /// MLS group state at summary-build time. `None` for fresh
    /// contacts whose KeyPackage exchange is in flight.
    #[serde(default)]
    pub group_state: Option<MlsGroupStateLabel>,
    /// Highest message-table `id` marked read for this contact's
    /// group (from the `read_state` cursor). UI uses this to
    /// anchor the frozen "Unread" separator at conversation-open.
    /// `None` for fresh contacts with no cursor yet.
    #[serde(default)]
    pub last_read_row_id: Option<i64>,
```

- [ ] **Step 4: Find every existing constructor of `ContactSummary` and add the two new fields with `None`**

Run:

```bash
cd /home/myggiz/development/skattr && grep -rn "ContactSummary {" --include="*.rs" | head
```

For each match, add `group_state: None,` and `last_read_row_id: None,` to the literal. Expected sites: `crates/core/src/daemon/dispatch.rs` (inside `list_contacts`), and existing tests in `crates/core/src/daemon/commands.rs` and `crates/core/src/daemon/dispatch.rs`.

- [ ] **Step 5: Run the test — must pass**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness contact_summary 2>&1 | tail -20
```

Expected: `2 passed`. Also: `cargo build -p skattr-core --features test-harness` clean.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/commands.rs crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
feat(core): add MlsGroupStateLabel + extend ContactSummary

Phase 2.D wire-format addition (additive only). New enum mirrors
the three existing mls::state::GroupState variants. ContactSummary
gains group_state + last_read_row_id (both Option, #[serde(default)])
so legacy CBOR encodings decode unchanged.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: Add `MessageRepo::recent_before` storage method

**Files:**
- Modify: `crates/core/src/storage/messages.rs`

- [ ] **Step 1: Write the failing test** — append inside the existing `#[cfg(test)] mod tests` block at the bottom of `messages.rs`:

```rust
#[test]
fn recent_before_excludes_cursor_and_orders_descending() {
    let pool = Pool::in_memory();
    let repo = MessageRepo::new(&pool);
    let gid = [0xCC; 32];
    // Insert ids 1..=10 with monotonic ts.
    let mut row_ids = Vec::new();
    for i in 0..10 {
        let mut env = sample_envelope(&format!("m{i}"));
        env.ts = 1000 + i as i64;
        let id = repo
            .insert(InsertParams {
                group_id: &gid,
                sender: &[0u8; 32],
                envelope: &env,
                mls_generation: 0,
                ts_daemon_recv: env.ts,
            })
            .unwrap();
        row_ids.push(id);
    }
    // Cursor at the 7th-newest row.
    let cursor = row_ids[6];
    let page = repo.recent_before(&gid, cursor, 5).unwrap();
    assert_eq!(page.len(), 5);
    // Cursor row itself MUST NOT appear.
    assert!(page.iter().all(|m| m.id != cursor));
    // Returned ids strictly less than cursor.
    assert!(page.iter().all(|m| m.id < cursor));
    // Descending by id (matches recent's ordering).
    let ids: Vec<i64> = page.iter().map(|m| m.id).collect();
    let mut sorted = ids.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(ids, sorted);
}

#[test]
fn recent_before_with_orphan_cursor_returns_older_rows() {
    let pool = Pool::in_memory();
    let repo = MessageRepo::new(&pool);
    let gid = [0xDD; 32];
    let env = sample_envelope("only-row");
    repo.insert(InsertParams {
        group_id: &gid,
        sender: &[0u8; 32],
        envelope: &env,
        mls_generation: 0,
        ts_daemon_recv: env.ts,
    })
    .unwrap();
    // Cursor far above any existing row.
    let page = repo.recent_before(&gid, 999_999, 10).unwrap();
    assert_eq!(page.len(), 1, "should return rows older than orphan cursor");
}
```

- [ ] **Step 2: Run test — must fail (method undefined)**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness recent_before 2>&1 | tail -20
```

Expected: `no method named recent_before`.

- [ ] **Step 3: Implement `recent_before`** — add the method on the `impl<'p> MessageRepo<'p>` block, immediately after the existing `recent` method:

```rust
    /// Paginate older messages: rows with `id < before_id`.
    /// Ordering matches `recent` — `(mls_generation DESC, id DESC) LIMIT n`.
    /// Cursor row is excluded (strict-less semantics).
    pub fn recent_before(
        &self,
        group_id: &[u8],
        before_id: i64,
        limit: usize,
    ) -> Result<Vec<StoredMessage>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, group_id, sender, kind, body_blob, ts, delivered_at, \
                            mls_generation, ts_daemon_recv \
                     FROM messages \
                     WHERE group_id = ?1 AND id < ?2 \
                     ORDER BY mls_generation DESC, id DESC LIMIT ?3",
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "prepare recent_before: {e}"
                    )))
                })?;
            let rows = stmt
                .query_map(
                    rusqlite::params![
                        group_id,
                        before_id,
                        i64::try_from(limit).unwrap_or(i64::MAX)
                    ],
                    |r| {
                        Ok(StoredMessage {
                            id: r.get(0)?,
                            group_id: r.get(1)?,
                            sender: r.get(2)?,
                            kind: r.get(3)?,
                            body_blob: r.get(4)?,
                            ts: r.get(5)?,
                            delivered_at: r.get(6)?,
                            mls_generation: r.get(7)?,
                            ts_daemon_recv: r.get(8)?,
                        })
                    },
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "query recent_before: {e}"
                    )))
                })?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "collect recent_before: {e}"
                )))
            })
        })
    }
```

- [ ] **Step 4: Run tests — must pass**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness recent_before 2>&1 | tail -20
```

Expected: `2 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/storage/messages.rs
git commit -m "$(cat <<'EOF'
feat(storage): add MessageRepo::recent_before for pagination

Sibling to MessageRepo::recent. Returns rows with id < before_id
in (mls_generation DESC, id DESC) order, strict-less on cursor.
No new index needed — messages.group_id covers the predicate;
PK covers the order + cursor.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: Extend `Command::RecentMessages` with `before_id` + `paged`

**Files:**
- Modify: `crates/core/src/daemon/commands.rs`

- [ ] **Step 1: Write failing serde test** — append inside the test module:

```rust
#[test]
fn recent_messages_with_before_id_and_paged_round_trips() {
    let cmd = Command::RecentMessages {
        contact: Some(crate::identity::PublicKey([1; 32])),
        limit: 50,
        before_id: Some(123),
        paged: true,
    };
    let back: Command = roundtrip(&cmd);
    match back {
        Command::RecentMessages {
            before_id: Some(123),
            paged: true,
            limit: 50,
            ..
        } => {}
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn recent_messages_without_new_fields_decodes_legacy() {
    // CBOR map missing before_id + paged, mirroring an old client.
    let legacy_cbor = {
        let mut buf = Vec::new();
        let v = ciborium::value::Value::Map(vec![
            ("cmd".into(), ciborium::value::Value::Text("recent_messages".into())),
            ("contact".into(), ciborium::value::Value::Null),
            ("limit".into(), ciborium::value::Value::Integer(50.into())),
        ]);
        ciborium::ser::into_writer(&v, &mut buf).unwrap();
        buf
    };
    let back: Command = ciborium::de::from_reader(&legacy_cbor[..]).unwrap();
    match back {
        Command::RecentMessages {
            before_id: None,
            paged: false,
            limit: 50,
            ..
        } => {}
        other => panic!("unexpected: {other:?}"),
    }
}
```

- [ ] **Step 2: Run — must fail**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness recent_messages_with_before_id 2>&1 | tail -10
```

- [ ] **Step 3: Add the fields** — in `Command::RecentMessages`:

```rust
    RecentMessages {
        /// If `Some`, only messages with this peer (either direction).
        contact: Option<PublicKey>,
        /// Max rows to return.
        limit: u32,
        /// Pagination cursor — return rows with `row_id < before_id`.
        /// `None` = first page (most-recent).
        #[serde(default)]
        before_id: Option<i64>,
        /// Opt-in to the paged response variant `MessagesPage`. CLI
        /// callers omit and receive `Messages(Vec)` unchanged.
        #[serde(default)]
        paged: bool,
    },
```

- [ ] **Step 4: Update existing call sites that construct `Command::RecentMessages`**

```bash
cd /home/myggiz/development/skattr && grep -rn "Command::RecentMessages" --include="*.rs"
```

For each constructor site (CLI, dispatch tests), add `before_id: None,` and `paged: false,` literals.

Specifically:
- `crates/cli/src/main.rs` — two sites (line ~845 and line ~947 per earlier exploration)
- Existing dispatch tests in `crates/core/src/daemon/dispatch.rs`
- Existing test in `commands.rs`

- [ ] **Step 5: Update the dispatch handler signature** — find `recent_messages` in `crates/core/src/daemon/dispatch.rs` (around line 382 from earlier exploration). Change signature:

```rust
async fn recent_messages<S>(
    handle: &Arc<DaemonHandle<S>>,
    contact: Option<crate::identity::PublicKey>,
    limit: u32,
    before_id: Option<i64>,
    paged: bool,
) -> std::result::Result<CommandResult, IpcError>
```

…and the matching call site in `execute_command`:

```rust
        Command::RecentMessages { contact, limit, before_id, paged } => {
            recent_messages(handle, contact, limit, before_id, paged).await
        }
```

(Logic for using these new params lands in Task 6. For now `recent_messages` must compile; ignore them with `let _ = (before_id, paged);` at the top of the function to silence unused warnings.)

- [ ] **Step 6: Run tests — must pass**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness recent_messages 2>&1 | tail -20
. "$HOME/.cargo/env" && cargo build -p skattr-cli && cargo build -p skattr-tests --features test-harness
```

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/daemon/commands.rs crates/core/src/daemon/dispatch.rs crates/cli/src/main.rs
git commit -m "$(cat <<'EOF'
feat(core): add before_id + paged fields to Command::RecentMessages

Both #[serde(default)] so legacy encodings without the fields
decode cleanly. Dispatch handler signature updated to accept the
new params; logic is gated on `paged` in a follow-up task.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Add `CommandResult::MessagesPage` variant

**Files:**
- Modify: `crates/core/src/daemon/commands.rs`

- [ ] **Step 1: Write failing test** — append:

```rust
#[test]
fn messages_page_round_trips_cbor() {
    let p = CommandResult::MessagesPage {
        records: vec![MessageRecord {
            row_id: 7,
            message_id: Hex16::from([2; 16]),
            contact: crate::identity::PublicKey([7; 32]),
            direction: Direction::Incoming,
            kind: Kind::Text { body: "hi".into() },
            mls_generation: 1,
            ts_daemon_recv: 100,
            ts_envelope: 99,
        }],
        next_before_id: Some(6),
    };
    let back: CommandResult = roundtrip(&p);
    match back {
        CommandResult::MessagesPage { next_before_id: Some(6), records } => {
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].row_id, 7);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn messages_page_with_null_cursor_round_trips() {
    let p = CommandResult::MessagesPage {
        records: vec![],
        next_before_id: None,
    };
    let back: CommandResult = roundtrip(&p);
    assert!(matches!(
        back,
        CommandResult::MessagesPage { next_before_id: None, .. }
    ));
}
```

- [ ] **Step 2: Run — must fail (variant undefined)**

- [ ] **Step 3: Add the variant** — inside `CommandResult`, immediately after the existing `Messages(Vec<MessageRecord>)`:

```rust
    /// [`Command::RecentMessages`] completed with `paged: true`.
    /// Most-recent first within the page; `next_before_id` is the
    /// cursor for the next older page (`None` if this was the
    /// last page).
    MessagesPage {
        records: Vec<MessageRecord>,
        next_before_id: Option<i64>,
    },
```

- [ ] **Step 4: Run tests — must pass**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness messages_page 2>&1 | tail -10
```

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/commands.rs
git commit -m "$(cat <<'EOF'
feat(core): add CommandResult::MessagesPage variant

Sibling to Messages(Vec) for paged recent_messages responses.
Carries records + next_before_id. Existing Messages(Vec) tuple
shape preserved — CLI's pattern matching unchanged.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: Extend `CommandResult::MessageSent` with `record`

**Files:**
- Modify: `crates/core/src/daemon/commands.rs`

- [ ] **Step 1: Write failing test:**

```rust
#[test]
fn message_sent_with_record_round_trips() {
    let rec = MessageRecord {
        row_id: 11,
        message_id: Hex16::from([3; 16]),
        contact: crate::identity::PublicKey([4; 32]),
        direction: Direction::Outgoing,
        kind: Kind::Text { body: "hi".into() },
        mls_generation: 1,
        ts_daemon_recv: 200,
        ts_envelope: 199,
    };
    let r = CommandResult::MessageSent {
        message_id: Hex16::from([3; 16]),
        status: SendStatus::Delivered,
        record: Some(rec),
    };
    let back: CommandResult = roundtrip(&r);
    match back {
        CommandResult::MessageSent {
            status: SendStatus::Delivered,
            record: Some(rec),
            ..
        } => assert_eq!(rec.row_id, 11),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn message_sent_legacy_payload_decodes_with_none_record() {
    // Legacy CBOR: only message_id + status, no record.
    let legacy_cbor = {
        let mut buf = Vec::new();
        let v = ciborium::value::Value::Map(vec![
            ("result".into(), ciborium::value::Value::Text("message_sent".into())),
            (
                "data".into(),
                ciborium::value::Value::Map(vec![
                    ("message_id".into(), ciborium::value::Value::Bytes([3u8; 16].to_vec())),
                    ("status".into(), ciborium::value::Value::Text("queued".into())),
                ]),
            ),
        ]);
        ciborium::ser::into_writer(&v, &mut buf).unwrap();
        buf
    };
    let back: CommandResult = ciborium::de::from_reader(&legacy_cbor[..]).unwrap();
    assert!(matches!(
        back,
        CommandResult::MessageSent { record: None, status: SendStatus::Queued, .. }
    ));
}
```

- [ ] **Step 2: Run — must fail (`record` field unknown)**

- [ ] **Step 3: Add the field:**

```rust
    /// [`Command::SendMessage`] completed (either Queued or Delivered).
    MessageSent {
        /// 16-byte per-message id (for correlation with later
        /// `Event::DeliveryStatusChanged`).
        message_id: Hex16,
        /// Outcome after the inline wait.
        status: SendStatus,
        /// Canonical sender-side `MessageRecord` projection. `None`
        /// only on the idempotent-retry branch where the original
        /// row id is not easily recoverable. UI's optimistic
        /// placeholder reconciles to `Some(record)` when present.
        #[serde(default)]
        record: Option<MessageRecord>,
    },
```

- [ ] **Step 4: Update every constructor of `CommandResult::MessageSent` to pass `record: None`**

```bash
cd /home/myggiz/development/skattr && grep -rn "CommandResult::MessageSent" --include="*.rs"
```

Sites (from earlier exploration): `crates/core/src/daemon/dispatch.rs:356,376` (and any test in the same file).

Change each `CommandResult::MessageSent { message_id, status }` to `CommandResult::MessageSent { message_id, status, record: None }`. (Real `Some(record)` projection lands in Task 7.)

- [ ] **Step 5: Run — must pass**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness message_sent 2>&1 | tail -10
. "$HOME/.cargo/env" && cargo build --workspace --features test-harness
```

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/commands.rs crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
feat(core): add record field to CommandResult::MessageSent

Optional MessageRecord projection for UI reconciliation. Carries
#[serde(default)] so legacy CBOR (without the field) decodes to
record: None. All existing call sites pass None — actual
projection in send_message lands next.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: Implement `dispatch::recent_messages` paged branch

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs`

- [ ] **Step 1: Write failing dispatch tests** — add inside `#[cfg(test)] mod tests { … }` near the bottom of `dispatch.rs`. Use `test_handle()` helper that already exists. The test seeds a contact + group + N messages, then exercises both unpaged + paged paths:

```rust
#[tokio::test]
async fn recent_messages_unpaged_returns_messages_tuple_variant() {
    let handle = test_handle();
    let (peer_pk, _gid) = seed_contact_with_group(&handle, "peer1", 5).await;

    let result = execute_command(
        handle.clone(),
        Command::RecentMessages {
            contact: Some(peer_pk),
            limit: 10,
            before_id: None,
            paged: false,
        },
    )
    .await
    .unwrap();

    match result {
        CommandResult::Messages(rows) => assert_eq!(rows.len(), 5),
        other => panic!("expected Messages(Vec), got {other:?}"),
    }
}

#[tokio::test]
async fn recent_messages_paged_first_page_carries_cursor() {
    let handle = test_handle();
    let (peer_pk, _gid) = seed_contact_with_group(&handle, "peer2", 60).await;

    let result = execute_command(
        handle.clone(),
        Command::RecentMessages {
            contact: Some(peer_pk),
            limit: 50,
            before_id: None,
            paged: true,
        },
    )
    .await
    .unwrap();

    match result {
        CommandResult::MessagesPage { records, next_before_id } => {
            assert_eq!(records.len(), 50);
            assert!(next_before_id.is_some());
            // Cursor should be the LAST (oldest in this DESC page) row's id.
            assert_eq!(next_before_id, records.last().map(|r| r.row_id));
        }
        other => panic!("expected MessagesPage, got {other:?}"),
    }
}

#[tokio::test]
async fn recent_messages_paged_last_page_has_null_cursor() {
    let handle = test_handle();
    let (peer_pk, _gid) = seed_contact_with_group(&handle, "peer3", 30).await;

    let result = execute_command(
        handle.clone(),
        Command::RecentMessages {
            contact: Some(peer_pk),
            limit: 50,
            before_id: None,
            paged: true,
        },
    )
    .await
    .unwrap();

    match result {
        CommandResult::MessagesPage { records, next_before_id: None } => {
            assert_eq!(records.len(), 30);
        }
        other => panic!("expected MessagesPage with null cursor, got {other:?}"),
    }
}

#[tokio::test]
async fn recent_messages_paged_with_before_id_excludes_cursor_row() {
    let handle = test_handle();
    let (peer_pk, _gid) = seed_contact_with_group(&handle, "peer4", 30).await;

    // First page to discover row ids.
    let first = execute_command(
        handle.clone(),
        Command::RecentMessages {
            contact: Some(peer_pk),
            limit: 10,
            before_id: None,
            paged: true,
        },
    )
    .await
    .unwrap();
    let cursor = match first {
        CommandResult::MessagesPage { next_before_id: Some(c), .. } => c,
        other => panic!("expected MessagesPage cursor, got {other:?}"),
    };

    // Second page using the cursor — must NOT contain a row with id == cursor.
    let second = execute_command(
        handle.clone(),
        Command::RecentMessages {
            contact: Some(peer_pk),
            limit: 10,
            before_id: Some(cursor),
            paged: true,
        },
    )
    .await
    .unwrap();
    match second {
        CommandResult::MessagesPage { records, .. } => {
            assert!(records.iter().all(|r| r.row_id < cursor));
        }
        other => panic!("expected MessagesPage, got {other:?}"),
    }
}
```

`seed_contact_with_group` is a helper to write — append in the same test module:

```rust
/// Seeds a contact with a group + N text messages and returns the
/// peer pubkey + the group_id. Mirrors the pattern used in
/// `send_message_persists_post_encrypt_mls_generation_and_ts_daemon_recv`.
async fn seed_contact_with_group(
    handle: &Arc<DaemonHandle<tokio::io::DuplexStream>>,
    nickname: &str,
    n_messages: usize,
) -> (crate::identity::PublicKey, Vec<u8>) {
    use crate::envelope::{Envelope, Kind, MessageId};
    use crate::storage::{ContactRepo, MessageRepo};
    use crate::storage::messages::InsertParams;
    use crate::storage::contacts::NewContact;

    let peer_pk = crate::identity::PublicKey([0xAB; 32]);
    let gid = vec![0xCD; 32];

    let contact_repo = ContactRepo::new(&handle.pool);
    contact_repo
        .insert(NewContact {
            identity: peer_pk,
            nickname: Some(nickname.into()),
            group_id: Some(gid.clone()),
        })
        .unwrap();

    let msg_repo = MessageRepo::new(&handle.pool);
    for i in 0..n_messages {
        let env = Envelope {
            v: 1,
            id: MessageId([i as u8; 16]),
            ts: 1_700_000_000 + i as i64,
            reply_to: None,
            kind: Kind::Text { body: format!("m{i}") },
        };
        msg_repo
            .insert(InsertParams {
                group_id: &gid,
                sender: &peer_pk.0,
                envelope: &env,
                mls_generation: 0,
                ts_daemon_recv: env.ts,
            })
            .unwrap();
    }
    (peer_pk, gid)
}
```

Adapt `NewContact` to whatever field names the actual `ContactRepo::insert` accepts — open `crates/core/src/storage/contacts.rs` and check the existing signature. If the helper differs (e.g. takes a `&Contact` struct), adjust accordingly.

- [ ] **Step 2: Run — must fail (helper undefined / paged branch missing)**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness recent_messages_paged 2>&1 | tail -30
```

- [ ] **Step 3: Implement the dispatch logic** — replace the body of `recent_messages` in `dispatch.rs` (currently around line 382-445). Below is the full new body; preserve the existing imports inside the function:

```rust
async fn recent_messages<S>(
    handle: &Arc<DaemonHandle<S>>,
    contact: Option<crate::identity::PublicKey>,
    limit: u32,
    before_id: Option<i64>,
    paged: bool,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::commands::{Direction, MessageRecord};
    use crate::daemon::error_kind::DaemonErrorKind;
    use crate::envelope::Envelope;
    use crate::identity::PublicKey;
    use crate::storage::{ContactRepo, MessageRepo};

    let peer = contact.ok_or(IpcError::Daemon(DaemonErrorKind::ContactNotFound))?;

    let contact_repo = ContactRepo::new(&handle.pool);
    let group_id = match contact_repo.get_group_id(&peer).map_err(map_err)? {
        Some(bytes) if !bytes.is_empty() => bytes,
        _ => return Err(IpcError::Daemon(DaemonErrorKind::ContactNotFound)),
    };

    let msg_repo = MessageRepo::new(&handle.pool);
    let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);
    let rows = match before_id {
        Some(b) => msg_repo
            .recent_before(&group_id, b, limit_usize)
            .map_err(map_err)?,
        None => msg_repo.recent(&group_id, limit_usize).map_err(map_err)?,
    };

    let my_pubkey: PublicKey = handle.identity.public();
    let records: Vec<MessageRecord> = rows
        .into_iter()
        .filter_map(|row| {
            let blob = row.body_blob.as_deref().unwrap_or(&[]);
            let env: Envelope = Envelope::decode(blob).ok()?;

            let mut sender_arr = [0u8; 32];
            if row.sender.len() == 32 {
                sender_arr.copy_from_slice(&row.sender);
            }
            let sender_pk = PublicKey(sender_arr);
            let direction = if sender_pk == my_pubkey {
                Direction::Outgoing
            } else {
                Direction::Incoming
            };
            Some(MessageRecord::project(
                row.id,
                &env,
                peer,
                u64::try_from(row.mls_generation).unwrap_or(0),
                row.ts_daemon_recv,
                direction,
            ))
        })
        .collect();

    if paged {
        let next_before_id = if records.len() == limit_usize {
            records.last().map(|r| r.row_id)
        } else {
            None
        };
        Ok(CommandResult::MessagesPage { records, next_before_id })
    } else {
        Ok(CommandResult::Messages(records))
    }
}
```

- [ ] **Step 4: Run — must pass**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness recent_messages 2>&1 | tail -30
```

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
feat(core): implement paged recent_messages dispatch

before_id branches to MessageRepo::recent_before; paged branches
to CommandResult::MessagesPage { records, next_before_id }.
Unpaged callers (CLI) receive Messages(Vec) unchanged.
next_before_id is the last (oldest) row's id when the page is
full, None when partial.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: `dispatch::send_message` captures `row_id` + projects record

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs`

- [ ] **Step 1: Write failing test** — append:

```rust
#[tokio::test]
async fn send_message_returns_record_with_row_id() {
    use crate::daemon::commands::Direction;
    use crate::envelope::Kind;

    let handle = test_handle();
    // Reuse existing helper that sets up a real 2-member MLS group.
    // (See send_message_with_real_group_yields_queued_without_transport
    // for the exact pattern in this test module.)
    let (peer_pk, _gid) = seed_contact_with_real_group(&handle).await;

    let result = execute_command(
        handle.clone(),
        Command::SendMessage {
            contact: peer_pk,
            kind: Kind::Text { body: "hello".into() },
        },
    )
    .await
    .unwrap();

    match result {
        CommandResult::MessageSent {
            record: Some(rec),
            status: _,
            ..
        } => {
            assert!(rec.row_id > 0, "record.row_id must be set");
            assert_eq!(rec.direction, Direction::Outgoing);
            assert_eq!(rec.contact, peer_pk);
            match &rec.kind {
                Kind::Text { body } => assert_eq!(body, "hello"),
                other => panic!("expected Kind::Text, got {other:?}"),
            }
            assert!(rec.mls_generation > 0, "post-encrypt mls_generation must advance");
            assert!(rec.ts_daemon_recv > 0);
        }
        other => panic!("expected MessageSent with Some(record), got {other:?}"),
    }
}
```

If `seed_contact_with_real_group` doesn't already exist, find the closest helper or copy the setup code from `send_message_with_real_group_yields_queued_without_transport` and reuse it. The helper must set up a real `mls::Group::create_solo` + `KeyPackage` + add to the contact repo.

- [ ] **Step 2: Run — must fail (`record: None` returned today)**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness send_message_returns_record_with_row_id 2>&1 | tail -20
```

- [ ] **Step 3: Modify `send_message`** — find the function (around line 276 in dispatch.rs). The transaction block currently looks like this (paraphrasing from earlier exploration):

```rust
let insert_result: crate::error::Result<()> = handle.pool.transaction(|tx| {
    group.save_in_tx(&group_repo, tx)?;
    let _ = msg_repo.insert_in_tx(tx, InsertParams { … })?;
    let _ = outbox_repo.insert_in_tx(tx, &contact.0, &message_id.0, &ciphertext, 0)?;
    Ok(())
});
```

Change it to capture `row_id`:

```rust
let insert_result: crate::error::Result<i64> = handle.pool.transaction(|tx| {
    group.save_in_tx(&group_repo, tx)?;
    let row_id = msg_repo.insert_in_tx(
        tx,
        crate::storage::messages::InsertParams {
            group_id: &group_id_bytes,
            sender: &handle.identity.public().0,
            envelope: &envelope,
            mls_generation,
            ts_daemon_recv,
        },
    )?;
    let _ = outbox_repo.insert_in_tx(tx, &contact.0, &message_id.0, &ciphertext, 0)?;
    Ok(row_id)
});

let row_id = match insert_result {
    Ok(id) => id,
    Err(CoreError::Storage(StorageErrorKind::DuplicateMessage)) => {
        // Idempotent retry: we don't have the row id, return None.
        return Ok(CommandResult::MessageSent {
            message_id: Hex16::from(message_id.0),
            status: SendStatus::Delivered,
            record: None,
        });
    }
    Err(e) => return Err(map_err(e)),
};
```

After the transaction succeeds, before the `hub.send` call, project the record:

```rust
let record = crate::daemon::commands::MessageRecord::project(
    row_id,
    &envelope,
    contact,
    mls_generation,
    ts_daemon_recv,
    crate::daemon::commands::Direction::Outgoing,
);
```

Update the final `Ok(...)` reply at the bottom of `send_message`:

```rust
Ok(CommandResult::MessageSent {
    message_id: Hex16::from(message_id.0),
    status,
    record: Some(record),
})
```

- [ ] **Step 4: Run — must pass**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness send_message_returns_record 2>&1 | tail -20
```

Also re-run all dispatch tests to ensure no regressions:

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness daemon::dispatch 2>&1 | tail -30
```

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
feat(core): project sender-side MessageRecord into MessageSent reply

Captures row_id from insert_in_tx (was discarded with let _),
builds a MessageRecord::project() and attaches it as
Some(record) on the IPC reply. Idempotent-retry branch returns
record: None — the original row id isn't recoverable there. UI's
optimistic placeholder reconciles to Some(record) when present.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 8: `dispatch::list_contacts` populates `group_state` + `last_read_row_id`

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs`
- Modify: `crates/core/src/storage/contacts.rs` (or add a sibling repo helper for the read cursor)

- [ ] **Step 1: Inspect existing read-cursor storage**

Open `crates/core/src/storage/read_state.rs` (or wherever `ReadStateRepo` lives — Phase 1.G added it). Find the existing API. We need a method `last_read_row_id(group_id) -> Result<Option<i64>>`. If it doesn't exist, add it (single SQL `SELECT last_read_row_id FROM read_state WHERE group_id = ?`).

- [ ] **Step 2: Write failing dispatch test:**

```rust
#[tokio::test]
async fn list_contacts_carries_group_state_and_read_cursor() {
    use crate::daemon::commands::MlsGroupStateLabel;
    use crate::storage::{MessageRepo, ReadStateRepo};

    let handle = test_handle();
    let (peer_pk, gid) = seed_contact_with_real_group(&handle).await;

    // Mark up to row 5 as read for this group.
    let msg_repo = MessageRepo::new(&handle.pool);
    let _ = msg_repo.insert(crate::storage::messages::InsertParams {
        group_id: &gid,
        sender: &peer_pk.0,
        envelope: &crate::envelope::Envelope {
            v: 1,
            id: crate::envelope::MessageId([1; 16]),
            ts: 100,
            reply_to: None,
            kind: crate::envelope::Kind::Text { body: "x".into() },
        },
        mls_generation: 0,
        ts_daemon_recv: 100,
    }).unwrap();
    ReadStateRepo::new(&handle.pool).set(&gid, 5).unwrap();

    let result = execute_command(handle.clone(), Command::ListContacts).await.unwrap();
    let summary = match result {
        CommandResult::Contacts(s) => {
            s.into_iter().find(|c| c.pubkey == peer_pk).expect("seeded peer not found")
        }
        other => panic!("expected Contacts, got {other:?}"),
    };

    assert_eq!(summary.group_state, Some(MlsGroupStateLabel::Active));
    assert_eq!(summary.last_read_row_id, Some(5));
}

#[tokio::test]
async fn list_contacts_reports_corrupt_for_unloadable_group_blob() {
    let handle = test_handle();
    let (peer_pk, gid) = seed_contact_with_group(&handle, "broken", 0).await;

    // Write a garbage blob to the group row to force load failure.
    use crate::storage::MlsGroupRepo;
    MlsGroupRepo::new(&handle.pool).put(&gid, b"\xFF\xFF\xFFnot a valid mls blob", 0).unwrap();

    let result = execute_command(handle.clone(), Command::ListContacts).await.unwrap();
    let summary = match result {
        CommandResult::Contacts(s) => s.into_iter().find(|c| c.pubkey == peer_pk).unwrap(),
        other => panic!("expected Contacts, got {other:?}"),
    };
    use crate::daemon::commands::MlsGroupStateLabel;
    assert_eq!(summary.group_state, Some(MlsGroupStateLabel::Corrupt));
}
```

The exact API of `ReadStateRepo::set` may differ — check the file and adjust the call (e.g. it might be `mark_read(&gid, up_to_id)`).

- [ ] **Step 3: Run — must fail**

- [ ] **Step 4: Implement** — open `dispatch.rs::list_contacts` (around line 74). After computing the existing `(unread_count, last_message_preview, last_ts_recv)` tuple, add the new projections:

```rust
        // group_state projection: load the MLS blob and try to materialise.
        // None  → no group row at all (KP exchange in flight).
        // Active→ Group::load returned Ok.
        // Corrupt → Group::load returned Err.
        let group_state: Option<crate::daemon::commands::MlsGroupStateLabel> = match group_id.as_ref() {
            Some(gid) => {
                use crate::daemon::commands::MlsGroupStateLabel;
                use crate::mls::group::{Group, GroupId};
                use crate::storage::MlsGroupRepo;
                let group_repo = MlsGroupRepo::new(&handle.pool);
                match Group::load(&GroupId(gid.clone()), &group_repo) {
                    Ok(Some(_g)) => Some(MlsGroupStateLabel::Active),
                    Ok(None) => Some(MlsGroupStateLabel::PendingJoin),
                    Err(_) => Some(MlsGroupStateLabel::Corrupt),
                }
            }
            None => None,
        };

        // Per-group read cursor for the frozen unread separator.
        let last_read_row_id: Option<i64> = match group_id.as_ref() {
            Some(gid) => crate::storage::ReadStateRepo::new(&handle.pool)
                .last_read_row_id(gid)
                .map_err(map_err)?,
            None => None,
        };
```

…then update the `summaries.push(ContactSummary { … })` call to include both new fields:

```rust
        summaries.push(ContactSummary {
            // …existing fields…
            group_state,
            last_read_row_id,
        });
```

If `ReadStateRepo::last_read_row_id` doesn't exist, add it to `crates/core/src/storage/read_state.rs`:

```rust
    /// Returns the highest message-table id that has been marked read
    /// in this group, or `None` if no cursor has been set.
    pub fn last_read_row_id(&self, group_id: &[u8]) -> Result<Option<i64>> {
        self.pool.with(|c| {
            let row: rusqlite::Result<i64> = c.query_row(
                "SELECT last_read_row_id FROM read_state WHERE group_id = ?1",
                rusqlite::params![group_id],
                |r| r.get(0),
            );
            match row {
                Ok(v) => Ok(Some(v)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Storage(StorageErrorKind::Other(format!(
                    "last_read_row_id: {e}"
                )))),
            }
        })
    }
```

The exact column name (`last_read_row_id` vs `up_to_message_id` vs other) depends on what migration 0006 created. Open `crates/core/src/storage/migrations/0006_*.sql` (or wherever the read_state table lives) and verify. Adjust the SQL accordingly.

- [ ] **Step 5: Run — must pass**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness list_contacts_carries 2>&1 | tail -30
```

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs crates/core/src/storage/
git commit -m "$(cat <<'EOF'
feat(core): populate group_state + last_read_row_id in ContactSummary

list_contacts dispatch loop loads the MLS blob via Group::load
and projects to MlsGroupStateLabel (Active / PendingJoin /
Corrupt). Per-group read cursor read via
ReadStateRepo::last_read_row_id; UI uses it to anchor the frozen
unread separator at conversation-open.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 9: Wire-format snapshot test (lint)

**Files:**
- Create: `crates/core/tests/wire_format_append_only.rs`

- [ ] **Step 1: Create the test file**

Top-of-file licence header (GPL-3.0-or-later — `crates/core` ships GPLv3):

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Snapshot test: enumerates every variant of `Command` and
//! `CommandResult`, sorts them alphabetically, and compares the
//! result against a frozen list. Phase 2.D exit constraint: the
//! wire format is append-only — adding a variant requires
//! updating this snapshot in the same commit; reshaping a variant
//! is a wire-format BREAKING change and needs a separate spec
//! per the umbrella decomposition spec.

use skattr_core::daemon::commands::{Command, CommandResult};

fn enumerate_command_variants() -> Vec<&'static str> {
    // Build a representative set so ciborium can serialise and
    // surface the variant tag, then collect the tags. We construct
    // one of each variant to ensure the type is exhaustive — the
    // compiler will flag a missing variant on next add.
    let _: Command = Command::ListContacts;
    // List every variant tag explicitly, mirroring the snake_case
    // serde rename. Adding a variant means: (1) construct it
    // above (commented or active), (2) add its tag here.
    vec![
        "add_contact",
        "add_mailbox",
        "create_group",
        "create_invite",
        "daemon_info",
        "export_history",
        "list_contacts",
        "list_mailboxes",
        "mark_read",
        "prune_history",
        "recent_messages",
        "remove_mailbox",
        "rotate_onion",
        "search_messages",
        "send_message",
        "shutdown",
    ]
}

fn enumerate_command_result_variants() -> Vec<&'static str> {
    let _: CommandResult = CommandResult::Ok;
    vec![
        "contact_added",
        "contacts",
        "daemon_info",
        "export_page",
        "invite_created",
        "mailboxes",
        "marked_read",
        "message_sent",
        "messages",
        "messages_page",
        "ok",
        "pruned",
        "search_results",
        "subscribed",
    ]
}

#[test]
fn command_variant_set_is_frozen() {
    let mut got = enumerate_command_variants();
    got.sort();
    let expected = {
        let mut e = enumerate_command_variants();
        e.sort();
        e
    };
    // The list above IS the snapshot. If you're adding a variant,
    // append its tag (in snake_case) and re-run. The point of the
    // sort + assert is to make accidental removals/reshapes
    // produce a clear test diff.
    assert_eq!(got, expected);
}

#[test]
fn command_result_variant_set_is_frozen() {
    let mut got = enumerate_command_result_variants();
    got.sort();
    let expected = {
        let mut e = enumerate_command_result_variants();
        e.sort();
        e
    };
    assert_eq!(got, expected);
}
```

The naive form above is a tautology — it asserts the list against itself. The intent is to make adding a Command variant require a deliberate edit here; the compiler-enforced exhaustiveness comes from constructing a specimen of each variant. To make this lint actually catch missing variants, replace the body of `enumerate_command_variants` with a `match` over a `&Command` value that returns the tag as `&'static str`:

```rust
fn variant_tag(c: &Command) -> &'static str {
    match c {
        Command::AddContact { .. } => "add_contact",
        Command::AddMailbox { .. } => "add_mailbox",
        Command::CreateGroup { .. } => "create_group",
        Command::CreateInvite { .. } => "create_invite",
        Command::DaemonInfo => "daemon_info",
        Command::ExportHistory { .. } => "export_history",
        Command::ListContacts => "list_contacts",
        Command::ListMailboxes => "list_mailboxes",
        Command::MarkRead { .. } => "mark_read",
        Command::PruneHistory { .. } => "prune_history",
        Command::RecentMessages { .. } => "recent_messages",
        Command::RemoveMailbox { .. } => "remove_mailbox",
        Command::RotateOnion => "rotate_onion",
        Command::SearchMessages { .. } => "search_messages",
        Command::SendMessage { .. } => "send_message",
        Command::Shutdown => "shutdown",
    }
}
```

The match is exhaustive — adding a `Command` variant without updating `variant_tag` is a compile error. `enumerate_command_variants` then returns the static list, and the test asserts the static list matches the sorted snapshot. Do the same for `CommandResult` (don't forget to handle struct vs tuple variants in the match arms).

- [ ] **Step 2: Run — must pass**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --test wire_format_append_only 2>&1 | tail -10
```

- [ ] **Step 3: Verify the lint catches a regression** — temporarily remove a `Command` variant arm from `variant_tag` (e.g. comment out `Command::Shutdown`). Re-run; `cargo build` must fail with E0004 ("non-exhaustive patterns"). Restore.

- [ ] **Step 4: Commit**

```bash
git add crates/core/tests/wire_format_append_only.rs
git commit -m "$(cat <<'EOF'
test(core): wire-format snapshot lint for Command + CommandResult

Phase 2.D exit constraint — wire format is append-only.
Exhaustive match arms in variant_tag make adding a variant a
compile error here, forcing a deliberate edit. Snapshot list +
sort detects accidental removals or reshapes.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase B — UI primitives

UI work begins. The Rust changes from Phase A are emitting fresh `ts-rs` bindings into `crates/ui/src-svelte/src/lib/ipc/types/`; the UI consumes those.

### Task 10: Add `--danger` colour token

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/styles/tokens.css`

- [ ] **Step 1: Open the file** and find the existing 6-token palette in `:root` (dark) + `prefers-color-scheme: light` overrides.

- [ ] **Step 2: Add `--danger`** with both dark + light values:

```css
:root {
  /* …existing 6 tokens… */
  --danger: #ef4444;
}

@media (prefers-color-scheme: light) {
  :root {
    /* …existing light overrides… */
    --danger: #dc2626;
  }
}
```

The hex values match Tailwind's `red-500` / `red-600` — chosen for accessibility against both `--bg` and `--bg-elevated` on dark + light themes.

- [ ] **Step 3: Verify the existing UI still compiles**

```bash
cd /home/myggiz/development/skattr/crates/ui/src-svelte && pnpm install --frozen-lockfile && pnpm build
```

(or whatever the existing UI build command is — check `package.json` scripts).

- [ ] **Step 4: Commit**

```bash
git add crates/ui/src-svelte/src/lib/styles/tokens.css
git commit -m "$(cat <<'EOF'
feat(ui): add --danger colour token (7th)

Phase 2.D failure-state rendering. Dark: #ef4444 (red-500).
Light: #dc2626 (red-600). Used by DeliveryIcon's failed state
and reused by 2.E (contact remove confirmations) + 2.F
(notification severity).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 11: Bundle Lucide ISC SVG icons

**Files:**
- Create: `crates/ui/src-svelte/src/lib/icons/clock.svg`
- Create: `crates/ui/src-svelte/src/lib/icons/check.svg`
- Create: `crates/ui/src-svelte/src/lib/icons/check-check.svg`
- Create: `crates/ui/src-svelte/src/lib/icons/alert-triangle.svg`
- Create: `crates/ui/src-svelte/src/lib/icons/index.ts`
- Create: `crates/ui/src-svelte/src/lib/icons/LICENSE` (MIT, with Lucide attribution)

- [ ] **Step 1: Fetch the four SVGs from Lucide's released bundle**

The Lucide source repo is `https://github.com/lucide-icons/lucide`. Pick stable SVGs at MIT licence revision. For each glyph, paste the contents into the corresponding `.svg` file with this exact body:

`clock.svg`:

```svg
<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/></svg>
```

`check.svg`:

```svg
<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>
```

`check-check.svg`:

```svg
<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18 6 7 17l-5-5"/><path d="m22 10-7.5 7.5L13 16"/></svg>
```

`alert-triangle.svg`:

```svg
<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"/><line x1="12" x2="12" y1="9" y2="13"/><line x1="12" x2="12.01" y1="17" y2="17"/></svg>
```

- [ ] **Step 2: Add MIT licence file**

`crates/ui/src-svelte/src/lib/icons/LICENSE`:

```
ISC License

Copyright (c) for portions of Lucide are held by Cole Bemis 2013-2022 as
part of Feather (MIT). All other copyright (c) for Lucide are held by
Lucide Contributors 2022.

Permission to use, copy, modify, and/or distribute this software for any
purpose with or without fee is hereby granted, provided that the above
copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
```

(Lucide is actually ISC, not MIT — verify by visiting https://github.com/lucide-icons/lucide/blob/main/LICENSE before committing. Update the licence file to match exactly.)

- [ ] **Step 3: Create the re-export module**

`crates/ui/src-svelte/src/lib/icons/index.ts`:

```typescript
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
//
// Inlines bundled Lucide icons as raw SVG strings so they can be
// dropped into Svelte components via {@html ...}. Vite's `?raw`
// query parameter loads the file as text at build time. Glyphs
// are bundled (LICENSE adjacent) — no remote fetching, no CDN.

import clockSvg from "./clock.svg?raw";
import checkSvg from "./check.svg?raw";
import checkCheckSvg from "./check-check.svg?raw";
import alertTriangleSvg from "./alert-triangle.svg?raw";

export const icons = {
  clock: clockSvg,
  check: checkSvg,
  "check-check": checkCheckSvg,
  "alert-triangle": alertTriangleSvg,
} as const;

export type IconName = keyof typeof icons;
```

- [ ] **Step 4: Verify Vite `?raw` import works** — run `pnpm build` from `crates/ui/src-svelte/`. Should succeed with no warnings about unresolved `?raw` modules.

- [ ] **Step 5: Update `cargo deny` if it scans UI deps for licences** — `crates/ui/` is set `publish = false` per the existing `deny.toml` carve-out (CHANGELOG notes "publish=false on ui"), so no action expected. Verify with:

```bash
. "$HOME/.cargo/env" && cargo deny check 2>&1 | tail -10
```

If the SVG files trigger any new licence finding, add `crates/ui/src-svelte/src/lib/icons/` to the deny exclusions.

- [ ] **Step 6: Commit**

```bash
git add crates/ui/src-svelte/src/lib/icons/
git commit -m "$(cat <<'EOF'
feat(ui): bundle 4 Lucide icons (clock, check, check-check, alert-triangle)

ISC-licensed inline SVG (Lucide upstream is ISC, not MIT — see
LICENSE in the directory). Loaded via Vite's ?raw query so the
strings can be dropped into Svelte via {@html}. Phase 2.D
delivery-state icon family — no remote CDN, no HTML rendering of
content.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 12: `DeliveryIcon.svelte` + Vitest snapshot test

**Files:**
- Create: `crates/ui/src-svelte/src/lib/components/DeliveryIcon.svelte`
- Create: `crates/ui/src-svelte/src/lib/components/DeliveryIcon.test.ts`

- [ ] **Step 1: Write the failing test first**

`DeliveryIcon.test.ts`:

```typescript
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
import { describe, expect, test } from "vitest";
import { render } from "@testing-library/svelte";
import DeliveryIcon from "./DeliveryIcon.svelte";

describe("DeliveryIcon", () => {
  test("renders clock for pending", () => {
    const { container } = render(DeliveryIcon, { props: { status: "pending" } });
    const svg = container.querySelector("svg");
    expect(svg).not.toBeNull();
    // clock.svg's distinguishing feature: <circle cx="12" cy="12" r="10"/>
    expect(container.innerHTML).toContain('<circle cx="12" cy="12" r="10"');
  });

  test("renders check for sent", () => {
    const { container } = render(DeliveryIcon, { props: { status: "sent" } });
    expect(container.innerHTML).toContain('<polyline points="20 6 9 17 4 12"');
  });

  test("renders check-check for delivered", () => {
    const { container } = render(DeliveryIcon, { props: { status: "delivered" } });
    // check-check has TWO <path d="..."/> elements; check has zero <path>.
    const paths = container.querySelectorAll("svg path");
    expect(paths.length).toBe(2);
  });

  test("renders alert-triangle for failed", () => {
    const { container } = render(DeliveryIcon, { props: { status: "failed" } });
    expect(container.innerHTML).toMatch(/8-14a2 2 0 0 0-3\.48 0/);
  });

  test("title attribute set when provided", () => {
    const { container } = render(DeliveryIcon, {
      props: { status: "delivered", title: "Delivered" },
    });
    expect(container.querySelector("[title='Delivered']")).not.toBeNull();
  });
});
```

If `@testing-library/svelte` is not yet a dev dependency, add it. Check first:

```bash
cd /home/myggiz/development/skattr/crates/ui/src-svelte && grep testing-library/svelte package.json
```

If absent, add it: `pnpm add -D @testing-library/svelte`.

- [ ] **Step 2: Run — must fail (component undefined)**

```bash
cd /home/myggiz/development/skattr/crates/ui/src-svelte && pnpm vitest run DeliveryIcon 2>&1 | tail -20
```

- [ ] **Step 3: Implement `DeliveryIcon.svelte`:**

```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<script lang="ts">
  import { icons } from "$lib/icons";

  type Status = "pending" | "sent" | "delivered" | "failed";

  let { status, title }: { status: Status; title?: string } = $props();

  const glyph = $derived(
    status === "pending"   ? icons["clock"]
  : status === "sent"      ? icons["check"]
  : status === "delivered" ? icons["check-check"]
                           : icons["alert-triangle"],
  );
</script>

<span class="icon" class:pending={status === "pending"} class:sent={status === "sent"}
      class:delivered={status === "delivered"} class:failed={status === "failed"}
      title={title ?? ""}>
  {@html glyph}
</span>

<style>
  .icon {
    display: inline-flex;
    align-items: center;
    width: 14px;
    height: 14px;
    margin-left: var(--s-1);
    vertical-align: middle;
  }
  .icon :global(svg) {
    width: 14px;
    height: 14px;
  }
  .pending  :global(svg) { color: var(--text-muted); }
  .sent     :global(svg) { color: var(--text-muted); }
  .delivered :global(svg) { color: var(--accent); }
  .failed   :global(svg) { color: var(--danger); }
</style>
```

- [ ] **Step 4: Run — must pass**

```bash
cd /home/myggiz/development/skattr/crates/ui/src-svelte && pnpm vitest run DeliveryIcon 2>&1 | tail -10
```

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src-svelte/src/lib/components/DeliveryIcon.svelte crates/ui/src-svelte/src/lib/components/DeliveryIcon.test.ts
git commit -m "$(cat <<'EOF'
feat(ui): add DeliveryIcon component (4 states)

Renders clock / check / check-check / alert-triangle from the
bundled Lucide SVGs. Inline {@html} of the raw SVG string
inherits CSS color via stroke="currentColor". 14×14 px to match
--t-ui line height. State-keyed colour tokens
(--text-muted / --accent / --danger).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 13: `UnreadSeparator.svelte`

**Files:**
- Create: `crates/ui/src-svelte/src/lib/components/UnreadSeparator.svelte`

- [ ] **Step 1: Create the component** (no test — it's a static visual primitive; it'll get exercised in the integration tests of `VirtualMessageList` and the e2e flow):

```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<div class="separator" role="separator" aria-label="Unread messages below">
  <span class="line"></span>
  <span class="label">Unread</span>
  <span class="line"></span>
</div>

<style>
  .separator {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    margin: var(--s-3) 0;
    color: var(--accent);
    font: var(--t-ui);
  }
  .line {
    flex: 1;
    height: 1px;
    background: var(--accent);
    opacity: 0.4;
  }
  .label {
    text-transform: uppercase;
    letter-spacing: 0.05em;
    font-size: 0.75rem;
  }
</style>
```

- [ ] **Step 2: Verify build clean**

```bash
cd /home/myggiz/development/skattr/crates/ui/src-svelte && pnpm build 2>&1 | tail -5
```

- [ ] **Step 3: Commit**

```bash
git add crates/ui/src-svelte/src/lib/components/UnreadSeparator.svelte
git commit -m "$(cat <<'EOF'
feat(ui): add UnreadSeparator component

Phase 2.D frozen separator. Renders once per conversation,
anchored to the read cursor at conversation-open. Rendered
inline by VirtualMessageList between row_id == unreadAnchor
and the next row.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 14: `SkeletonBubble.svelte`

**Files:**
- Create: `crates/ui/src-svelte/src/lib/components/SkeletonBubble.svelte`

- [ ] **Step 1: Create the component:**

```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<div class="skeleton" aria-hidden="true"></div>

<style>
  .skeleton {
    background: var(--bg-elevated);
    border-radius: 12px;
    margin: var(--s-1) 0;
    height: 56px;
    max-width: 60ch;
    animation: pulse 1.6s ease-in-out infinite;
    opacity: 0.6;
  }
  @keyframes pulse {
    0%, 100% { opacity: 0.4; }
    50% { opacity: 0.7; }
  }
  @media (prefers-reduced-motion: reduce) {
    .skeleton { animation: none; opacity: 0.5; }
  }
</style>
```

- [ ] **Step 2: Build clean**

- [ ] **Step 3: Commit**

```bash
git add crates/ui/src-svelte/src/lib/components/SkeletonBubble.svelte
git commit -m "$(cat <<'EOF'
feat(ui): add SkeletonBubble pagination loading placeholder

Phase 2.D scroll-back loading state. CSS-only pulse animation,
no JS. Honours prefers-reduced-motion. Rendered 5× at the top of
VirtualMessageList during in-flight loadOlder() calls.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase C — UI stores

### Task 15: `delivery.ts` store + tests

**Files:**
- Create: `crates/ui/src-svelte/src/lib/stores/delivery.ts`
- Create: `crates/ui/src-svelte/src/lib/stores/delivery.test.ts`

- [ ] **Step 1: Write the failing test**

```typescript
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
import { describe, expect, test, beforeEach } from "vitest";
import { get } from "svelte/store";
import { delivery, recordDeliveryStatus, statusForMessageHex } from "./delivery";

describe("delivery store", () => {
  beforeEach(() => {
    delivery.set(new Map());
  });

  test("recordDeliveryStatus sets the entry", () => {
    recordDeliveryStatus("aabbccdd", { type: "queued" } as any);
    const map = get(delivery);
    expect(map.get("aabbccdd")).toEqual({ type: "queued" });
  });

  test("statusForMessageHex returns undefined for missing", () => {
    expect(statusForMessageHex("ffeeddcc")).toBeUndefined();
  });

  test("recordDeliveryStatus overwrites prior entry", () => {
    recordDeliveryStatus("aabb", { type: "queued" } as any);
    recordDeliveryStatus("aabb", { type: "delivered" } as any);
    expect(get(delivery).get("aabb")).toEqual({ type: "delivered" });
  });
});
```

- [ ] **Step 2: Run — must fail**

- [ ] **Step 3: Implement `delivery.ts`:**

```typescript
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
import { writable, get } from "svelte/store";
import type { DeliveryStatus, Hex16 } from "$lib/ipc/types";

/**
 * Map keyed by lowercase-hex `message_id` (Hex16 stringified).
 * Updated by Event::DeliveryStatusChanged (subscribed elsewhere)
 * and by send.ts when MessageSent reply arrives (sets
 * Queued or Delivered immediately).
 */
export const delivery = writable<Map<string, DeliveryStatus>>(new Map());

export function recordDeliveryStatus(
  messageHex: string,
  status: DeliveryStatus,
): void {
  delivery.update((m) => {
    const next = new Map(m);
    next.set(messageHex, status);
    return next;
  });
}

export function statusForMessageHex(messageHex: string): DeliveryStatus | undefined {
  return get(delivery).get(messageHex);
}

/**
 * Convert a Hex16 (object emitted by ts-rs) to lowercase hex string.
 * ts-rs emits Hex16 as either { "0": [bytes…] } or as a direct hex
 * string depending on the serde format; client.ts normalises this.
 * For now we accept both shapes.
 */
export function hex16ToString(h: Hex16): string {
  if (typeof h === "string") return h.toLowerCase();
  // h is { 0: number[] } — ts-rs default for tuple structs
  const bytes = (h as unknown as { "0": number[] })["0"];
  return bytes.map((b) => b.toString(16).padStart(2, "0")).join("");
}
```

The `Hex16` representation depends on ts-rs's emission. Open `crates/ui/src-svelte/src/lib/ipc/types/Hex16.ts` and verify the actual type. If it's `string`, drop the bytes branch; if it's a tuple-struct object, keep both branches. Adjust the test mock accordingly.

- [ ] **Step 4: Map `DeliveryStatus` → icon-status** — add to the same file:

```typescript
import type { IconName } from "$lib/icons";

/** Icon-status string for a wire DeliveryStatus. */
export function deliveryToIconStatus(s: DeliveryStatus | undefined): "pending" | "sent" | "delivered" | "failed" {
  if (!s) return "pending";
  // DeliveryStatus is a tagged union from ts-rs.
  // Variants: Queued | Delivered | Deposited | Failed(string)
  switch (s.type ?? (s as any)) {
    case "queued":
      return "pending";
    case "delivered":
      return "delivered";
    case "deposited":
      return "sent";
    case "failed":
      return "failed";
    default:
      return "pending";
  }
}
```

The exact discriminator key (`type` vs `event` vs lowercase tag) depends on how ciborium + ts-rs emit the tagged union. Check by looking at `crates/ui/src-svelte/src/lib/ipc/types/DeliveryStatus.ts` and matching the actual shape. If the file shows e.g. `{ Queued: null } | { Delivered: null } | …`, switch on `Object.keys(s)[0]` instead.

- [ ] **Step 5: Run — must pass**

```bash
cd /home/myggiz/development/skattr/crates/ui/src-svelte && pnpm vitest run delivery 2>&1 | tail -10
```

- [ ] **Step 6: Commit**

```bash
git add crates/ui/src-svelte/src/lib/stores/delivery.ts crates/ui/src-svelte/src/lib/stores/delivery.test.ts
git commit -m "$(cat <<'EOF'
feat(ui): add delivery store + status mapping helpers

Map<message_hex, DeliveryStatus> writable store. Helper to map
the wire DeliveryStatus tagged union into the four icon-status
strings consumed by DeliveryIcon. Updated by send.ts (immediate
SendStatus mapping) and by Event::DeliveryStatusChanged
subscription wired in routes/+page.svelte.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 16: Extend `conversation.ts` with optimistic + reconciliation

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/stores/conversation.ts`
- Create: `crates/ui/src-svelte/src/lib/stores/conversation.test.ts`

- [ ] **Step 1: Write failing tests**

```typescript
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
import { describe, expect, test, beforeEach, vi } from "vitest";
import { get } from "svelte/store";
import { conversation, appendOptimistic, reconcile, markFailed } from "./conversation";
import type { MessageRecord, PublicKey } from "$lib/ipc/types";

const peer: PublicKey = { "0": new Array(32).fill(7) } as any;

beforeEach(() => {
  conversation.set({
    contact: peer,
    messages: [],
    nextBeforeId: null,
    loadingOlder: false,
    unreadAnchorRowId: null,
    readCursor: 0n,
  });
});

function fakeRecord(rowId: number, body: string): MessageRecord {
  return {
    row_id: BigInt(rowId),
    message_id: { "0": new Array(16).fill(0) } as any,
    contact: peer,
    direction: "outgoing",
    kind: { kind: "text", body },
    mls_generation: 1n,
    ts_daemon_recv: 100n,
    ts_envelope: 99n,
  } as any;
}

describe("optimistic send + reconcile", () => {
  test("appendOptimistic adds a placeholder", () => {
    appendOptimistic(peer, "hello", "tmp-1");
    const state = get(conversation);
    expect(state.messages.length).toBe(1);
    const msg = state.messages[0] as any;
    expect(msg.__tempId).toBe("tmp-1");
    expect(msg.__optimistic).toBe(true);
    expect(msg.kind.body).toBe("hello");
  });

  test("reconcile replaces placeholder by tempId, preserving index", () => {
    appendOptimistic(peer, "hello", "tmp-1");
    appendOptimistic(peer, "world", "tmp-2");
    const canonical = fakeRecord(7, "hello");
    reconcile("tmp-1", canonical);
    const state = get(conversation);
    expect(state.messages.length).toBe(2);
    expect((state.messages[0] as any).__tempId).toBeUndefined();
    expect(state.messages[0].row_id).toBe(7n);
    expect((state.messages[1] as any).__tempId).toBe("tmp-2");
  });

  test("markFailed flips placeholder to failed", () => {
    appendOptimistic(peer, "hello", "tmp-1");
    markFailed("tmp-1", "boom");
    const state = get(conversation);
    const msg = state.messages[0] as any;
    expect(msg.__failed).toBe("boom");
    expect(msg.__optimistic).toBe(true);
  });

  test("appendOptimistic on a different contact is ignored", () => {
    const other: PublicKey = { "0": new Array(32).fill(9) } as any;
    appendOptimistic(other, "ignored", "tmp-x");
    expect(get(conversation).messages.length).toBe(0);
  });
});
```

- [ ] **Step 2: Run — must fail**

- [ ] **Step 3: Update `conversation.ts`** — replace the existing file with:

```typescript
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
import { writable, get } from "svelte/store";

import { ipcClient } from "$lib/ipc/tauri";
import { unwrapOk } from "$lib/ipc/client";
import type { MessageRecord, PublicKey } from "$lib/ipc/types";

export type OptimisticMessage = MessageRecord & {
  __tempId: string;
  __optimistic: true;
  __failed?: string;
};

interface ConversationState {
  contact: PublicKey | null;
  messages: (MessageRecord | OptimisticMessage)[];
  nextBeforeId: bigint | null;
  loadingOlder: boolean;
  unreadAnchorRowId: bigint | null;
  readCursor: bigint;
}

export const conversation = writable<ConversationState>({
  contact: null,
  messages: [],
  nextBeforeId: null,
  loadingOlder: false,
  unreadAnchorRowId: null,
  readCursor: 0n,
});

function pubkeyEq(a: PublicKey, b: PublicKey): boolean {
  // PublicKey is emitted by ts-rs as { "0": number[32] }.
  const ax = (a as unknown as { "0": number[] })["0"];
  const bx = (b as unknown as { "0": number[] })["0"];
  if (ax.length !== bx.length) return false;
  for (let i = 0; i < ax.length; i++) if (ax[i] !== bx[i]) return false;
  return true;
}

export function appendOptimistic(
  contact: PublicKey,
  body: string,
  tempId: string,
): void {
  conversation.update((state) => {
    if (state.contact === null || !pubkeyEq(state.contact, contact)) {
      return state;
    }
    const placeholder: OptimisticMessage = {
      __tempId: tempId,
      __optimistic: true,
      row_id: -1n,
      message_id: { "0": new Array(16).fill(0) } as any,
      contact,
      direction: "outgoing",
      kind: { kind: "text", body },
      mls_generation: 0n,
      ts_daemon_recv: BigInt(Math.floor(Date.now() / 1000)),
      ts_envelope: BigInt(Date.now()),
    } as OptimisticMessage;
    return { ...state, messages: [...state.messages, placeholder] };
  });
}

export function reconcile(tempId: string, canonical: MessageRecord): void {
  conversation.update((state) => {
    const idx = state.messages.findIndex(
      (m) => (m as OptimisticMessage).__tempId === tempId,
    );
    if (idx < 0) return state;
    const next = [...state.messages];
    next[idx] = canonical;
    return { ...state, messages: next };
  });
}

export function markFailed(tempId: string, reason: string): void {
  conversation.update((state) => {
    const idx = state.messages.findIndex(
      (m) => (m as OptimisticMessage).__tempId === tempId,
    );
    if (idx < 0) return state;
    const target = { ...(state.messages[idx] as OptimisticMessage), __failed: reason };
    const next = [...state.messages];
    next[idx] = target;
    return { ...state, messages: next };
  });
}

export async function openConversation(contact: PublicKey): Promise<void> {
  // First-page fetch using the new paged variant.
  const resp = await ipcClient.request({
    cmd: "recent_messages",
    contact,
    limit: 50,
    before_id: null,
    paged: true,
  });
  const result = unwrapOk(resp);
  const records: MessageRecord[] = [];
  let nextBeforeId: bigint | null = null;
  if (result.result === "messages_page") {
    records.push(...[...result.data.records].reverse());
    nextBeforeId = result.data.next_before_id ?? null;
  }
  conversation.set({
    contact,
    messages: records,
    nextBeforeId,
    loadingOlder: false,
    unreadAnchorRowId: null, // populated from ContactSummary in Task 17
    readCursor: 0n,
  });
}

export function appendMessage(record: MessageRecord): void {
  conversation.update((state) => {
    if (state.contact !== null && pubkeyEq(record.contact as PublicKey, state.contact)) {
      return { ...state, messages: [...state.messages, record] };
    }
    return state;
  });
}
```

- [ ] **Step 4: Run — must pass**

```bash
cd /home/myggiz/development/skattr/crates/ui/src-svelte && pnpm vitest run conversation 2>&1 | tail -15
```

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src-svelte/src/lib/stores/conversation.ts crates/ui/src-svelte/src/lib/stores/conversation.test.ts
git commit -m "$(cat <<'EOF'
feat(ui): add optimistic + reconcile to conversation store

OptimisticMessage type carries __tempId / __optimistic / __failed
metadata. appendOptimistic appends a placeholder with the typed
text body; reconcile swaps in the canonical MessageRecord at the
same array index; markFailed flips the failure flag for the
DeliveryIcon to render. openConversation now uses MessagesPage
(paged: true) to populate nextBeforeId.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 17: Pagination + unread-anchor in conversation store

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/stores/conversation.ts`
- Modify: `crates/ui/src-svelte/src/lib/stores/conversation.test.ts`

- [ ] **Step 1: Write failing tests** — append to `conversation.test.ts`:

```typescript
import { loadOlder, openConversationFromSummary } from "./conversation";
import { ipcClient } from "$lib/ipc/tauri";

describe("pagination", () => {
  test("loadOlder is a no-op when nextBeforeId is null", async () => {
    conversation.set({
      contact: peer,
      messages: [],
      nextBeforeId: null,
      loadingOlder: false,
      unreadAnchorRowId: null,
      readCursor: 0n,
    });
    const spy = vi.spyOn(ipcClient, "request");
    await loadOlder();
    expect(spy).not.toHaveBeenCalled();
  });

  test("loadOlder prepends records and updates cursor", async () => {
    conversation.set({
      contact: peer,
      messages: [fakeRecord(60, "newer")],
      nextBeforeId: 60n,
      loadingOlder: false,
      unreadAnchorRowId: null,
      readCursor: 0n,
    });
    vi.spyOn(ipcClient, "request").mockResolvedValueOnce({
      resp: "ok",
      data: {
        result: "messages_page",
        data: {
          records: [fakeRecord(58, "older2"), fakeRecord(59, "older1")],
          next_before_id: 58n,
        },
      },
    } as any);
    await loadOlder();
    const state = get(conversation);
    expect(state.messages.map((m) => Number(m.row_id))).toEqual([58, 59, 60]);
    expect(state.nextBeforeId).toBe(58n);
    expect(state.loadingOlder).toBe(false);
  });

  test("loadOlder is idempotent under concurrent calls", async () => {
    conversation.set({
      contact: peer,
      messages: [],
      nextBeforeId: 100n,
      loadingOlder: false,
      unreadAnchorRowId: null,
      readCursor: 0n,
    });
    let resolveFirst: (v: any) => void = () => {};
    const spy = vi.spyOn(ipcClient, "request").mockImplementationOnce(
      () => new Promise((r) => (resolveFirst = r)),
    );
    const p1 = loadOlder();
    const p2 = loadOlder();   // must short-circuit on loadingOlder flag
    resolveFirst({
      resp: "ok",
      data: { result: "messages_page", data: { records: [], next_before_id: null } },
    });
    await Promise.all([p1, p2]);
    expect(spy).toHaveBeenCalledTimes(1);
  });
});

describe("openConversationFromSummary", () => {
  test("populates unreadAnchorRowId from summary", async () => {
    vi.spyOn(ipcClient, "request").mockResolvedValueOnce({
      resp: "ok",
      data: {
        result: "messages_page",
        data: { records: [], next_before_id: null },
      },
    } as any);
    await openConversationFromSummary({
      pubkey: peer,
      nickname: null,
      onion: "",
      card_version: 0n,
      added_at: 0n,
      unread_count: 0n,
      last_message_preview: null,
      last_ts_recv: null,
      group_state: "active",
      last_read_row_id: 12n,
    } as any);
    expect(get(conversation).unreadAnchorRowId).toBe(12n);
    expect(get(conversation).readCursor).toBe(12n);
  });
});
```

- [ ] **Step 2: Run — must fail (functions not exported)**

- [ ] **Step 3: Implement** — append to `conversation.ts`:

```typescript
import type { ContactSummary } from "$lib/ipc/types";

export async function openConversationFromSummary(summary: ContactSummary): Promise<void> {
  const resp = await ipcClient.request({
    cmd: "recent_messages",
    contact: summary.pubkey,
    limit: 50,
    before_id: null,
    paged: true,
  });
  const result = unwrapOk(resp);
  const records: MessageRecord[] = [];
  let nextBeforeId: bigint | null = null;
  if (result.result === "messages_page") {
    records.push(...[...result.data.records].reverse());
    nextBeforeId = result.data.next_before_id ?? null;
  }
  const anchor = summary.last_read_row_id ?? null;
  conversation.set({
    contact: summary.pubkey,
    messages: records,
    nextBeforeId,
    loadingOlder: false,
    unreadAnchorRowId: anchor,
    readCursor: anchor ?? 0n,
  });
  // Mark-read for the largest row in the page so the contact-list
  // badge clears on open. Daemon is idempotent if up_to <= current.
  if (records.length > 0) {
    const maxRowId = records.reduce<bigint>(
      (acc, r) => (r.row_id > acc ? r.row_id : acc),
      0n,
    );
    if (maxRowId > 0n) {
      void ipcClient.request({
        cmd: "mark_read",
        contact: summary.pubkey,
        up_to_message_id: maxRowId,
      });
    }
  }
}

export async function loadOlder(): Promise<void> {
  const state = get(conversation);
  if (state.loadingOlder || state.nextBeforeId === null || state.contact === null) return;
  conversation.update((s) => ({ ...s, loadingOlder: true }));
  try {
    const resp = await ipcClient.request({
      cmd: "recent_messages",
      contact: state.contact,
      limit: 50,
      before_id: state.nextBeforeId,
      paged: true,
    });
    const result = unwrapOk(resp);
    if (result.result === "messages_page") {
      const olderChrono = [...result.data.records].reverse();
      conversation.update((s) => ({
        ...s,
        messages: [...olderChrono, ...s.messages],
        nextBeforeId: result.data.next_before_id ?? null,
        loadingOlder: false,
      }));
    } else {
      conversation.update((s) => ({ ...s, loadingOlder: false }));
    }
  } catch (e) {
    conversation.update((s) => ({ ...s, loadingOlder: false }));
    throw e;
  }
}
```

- [ ] **Step 4: Migrate call sites off the old `openConversation(contact)` and remove it**

`openConversationFromSummary` is the single entry point going forward (it has the data the legacy form doesn't). Find every caller of `openConversation`:

```bash
cd /home/myggiz/development/skattr && grep -rn "openConversation" crates/ui/src-svelte/src crates/ui/src-svelte/tests
```

For each call site outside the store itself, replace `openConversation(contact)` with `openConversationFromSummary(summary)` — passing the matching `ContactSummary` from the `contacts` store. Then delete the `export async function openConversation` definition from `conversation.ts`.

If any 2.C test directly imports `openConversation`, update it to `openConversationFromSummary` with a fixture summary.

- [ ] **Step 5: Run — must pass**

```bash
cd /home/myggiz/development/skattr/crates/ui/src-svelte && pnpm vitest run conversation 2>&1 | tail -15
cd /home/myggiz/development/skattr/crates/ui/src-svelte && pnpm vitest run 2>&1 | tail -15
```

The second command runs the full UI test suite to catch any 2.C regression from removing the legacy entry point.

- [ ] **Step 6: Commit**

```bash
git add crates/ui/src-svelte/src/lib/stores/conversation.ts crates/ui/src-svelte/src/lib/stores/conversation.test.ts crates/ui/src-svelte/src
git commit -m "$(cat <<'EOF'
feat(ui): pagination + unread-anchor in conversation store

openConversationFromSummary populates unreadAnchorRowId from
ContactSummary.last_read_row_id (frozen at open per spec §4.6).
loadOlder calls recent_messages with before_id cursor; guards on
loadingOlder for idempotent concurrent calls. Both prepend
chronologically to the existing messages array. Auto-emits
mark_read on open for the largest row_id seen in the first page.
Legacy openConversation(contact) entry point removed; all
callers migrated to the summary-based form.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 18: `markReadIfAtBottom` debounce + bottom-proximity helper

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/stores/conversation.ts`
- Modify: `crates/ui/src-svelte/src/lib/stores/conversation.test.ts`

- [ ] **Step 1: Write failing tests** — append:

```typescript
import { markReadIfAtBottom, isWithinBottomThreshold } from "./conversation";

describe("mark-read", () => {
  test("isWithinBottomThreshold true when scrolled near bottom", () => {
    const el = { scrollTop: 900, scrollHeight: 1000, clientHeight: 100 } as any;
    expect(isWithinBottomThreshold(el)).toBe(true);
  });
  test("isWithinBottomThreshold false when scrolled up", () => {
    const el = { scrollTop: 100, scrollHeight: 1000, clientHeight: 100 } as any;
    expect(isWithinBottomThreshold(el)).toBe(false);
  });

  test("markReadIfAtBottom debounces multiple bursts to a single IPC", async () => {
    vi.useFakeTimers();
    conversation.update((s) => ({ ...s, contact: peer, readCursor: 0n }));
    const spy = vi.spyOn(ipcClient, "request").mockResolvedValue({
      resp: "ok",
      data: { result: "marked_read", data: { up_to: 7 } },
    } as any);
    markReadIfAtBottom(3n);
    markReadIfAtBottom(5n);
    markReadIfAtBottom(7n);
    vi.advanceTimersByTime(600);
    await Promise.resolve(); // flush microtasks
    expect(spy).toHaveBeenCalledTimes(1);
    expect(spy).toHaveBeenCalledWith({
      cmd: "mark_read",
      contact: peer,
      up_to_message_id: 7n,
    });
    vi.useRealTimers();
  });

  test("markReadIfAtBottom skips when rowId <= readCursor", async () => {
    vi.useFakeTimers();
    conversation.update((s) => ({ ...s, contact: peer, readCursor: 10n }));
    const spy = vi.spyOn(ipcClient, "request");
    markReadIfAtBottom(5n);
    vi.advanceTimersByTime(600);
    expect(spy).not.toHaveBeenCalled();
    vi.useRealTimers();
  });
});
```

- [ ] **Step 2: Run — must fail**

- [ ] **Step 3: Implement** — append to `conversation.ts`:

```typescript
const MARK_READ_DEBOUNCE_MS = 500;
const BOTTOM_PROXIMITY_PX = 100;

export function isWithinBottomThreshold(el: HTMLElement): boolean {
  return el.scrollHeight - el.scrollTop - el.clientHeight < BOTTOM_PROXIMITY_PX;
}

let markReadTimer: ReturnType<typeof setTimeout> | null = null;
let pendingHighestRowId: bigint = 0n;

export function markReadIfAtBottom(rowId: bigint): void {
  const state = get(conversation);
  if (state.contact === null) return;
  if (rowId <= state.readCursor) return;
  if (rowId > pendingHighestRowId) pendingHighestRowId = rowId;
  if (markReadTimer) clearTimeout(markReadTimer);
  markReadTimer = setTimeout(async () => {
    const target = pendingHighestRowId;
    pendingHighestRowId = 0n;
    markReadTimer = null;
    const cur = get(conversation);
    if (cur.contact === null || target <= cur.readCursor) return;
    try {
      await ipcClient.request({
        cmd: "mark_read",
        contact: cur.contact,
        up_to_message_id: target,
      });
      conversation.update((s) => ({ ...s, readCursor: target }));
    } catch (e) {
      // Swallow per spec §4.4 — non-critical; next open retries.
      console.warn("mark_read failed:", e);
    }
  }, MARK_READ_DEBOUNCE_MS);
}
```

- [ ] **Step 4: Run — must pass**

```bash
cd /home/myggiz/development/skattr/crates/ui/src-svelte && pnpm vitest run conversation 2>&1 | tail -15
```

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src-svelte/src/lib/stores/conversation.ts crates/ui/src-svelte/src/lib/stores/conversation.test.ts
git commit -m "$(cat <<'EOF'
feat(ui): debounced markReadIfAtBottom + bottom-proximity helper

Module-scoped timer + highest-rowid accumulator coalesce bursts
within a 500 ms window into one mark_read IPC. Skips when
rowId <= readCursor (idempotency at the daemon level is also
guaranteed). isWithinBottomThreshold encapsulates the 100 px
"near bottom" rule used by VirtualMessageList's intersection
observer + by appendMessage live-arrival handling.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase D — UI integration

### Task 19: Wire `<DeliveryIcon>` into `MessageBubble.svelte`

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/components/MessageBubble.svelte`

- [ ] **Step 1: Update the component:**

```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<script lang="ts">
  import type { MessageRecord } from "$lib/ipc/types";
  import type { OptimisticMessage } from "$lib/stores/conversation";
  import DeliveryIcon from "./DeliveryIcon.svelte";
  import { delivery, deliveryToIconStatus, hex16ToString } from "$lib/stores/delivery";

  let { record }: { record: MessageRecord | OptimisticMessage } = $props();

  let body = $derived(
    record.kind && record.kind.kind === "text" ? record.kind.body : "",
  );
  let isOutgoing = $derived(record.direction === "outgoing");
  let tsMs = $derived(Number(record.ts_daemon_recv) * 1000);

  let optimistic = $derived((record as OptimisticMessage).__optimistic === true);
  let failed = $derived((record as OptimisticMessage).__failed);

  let iconStatus = $derived.by(() => {
    if (!isOutgoing) return null;
    if (failed) return "failed" as const;
    if (optimistic) return "pending" as const;
    const hex = hex16ToString(record.message_id);
    return deliveryToIconStatus($delivery.get(hex));
  });

  let iconTitle = $derived.by(() => {
    if (failed) return failed;
    if (optimistic) return "Pending";
    return iconStatus === "delivered" ? "Delivered"
         : iconStatus === "sent"      ? "Delivered to mailbox"
         : iconStatus === "failed"    ? "Failed"
                                      : "Pending";
  });
</script>

<div class="bubble" class:outgoing={isOutgoing}>
  <p class="body">{body}</p>
  <div class="meta">
    <time class="ts">{new Date(tsMs).toLocaleTimeString()}</time>
    {#if isOutgoing && iconStatus}
      <DeliveryIcon status={iconStatus} title={iconTitle} />
    {/if}
  </div>
</div>

<style>
  .bubble {
    background: var(--bg-elevated);
    color: var(--text);
    padding: var(--s-2) var(--s-3);
    border-radius: 12px;
    margin: var(--s-1) 0;
    max-width: 60ch;
  }
  .bubble.outgoing { background: var(--accent); color: var(--bg); margin-left: auto; }
  .body { margin: 0; white-space: pre-wrap; word-break: break-word; }
  .meta {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--s-1);
    margin-top: var(--s-1);
  }
  .ts { color: var(--text-muted); font: var(--t-ui); }
  .bubble.outgoing .ts { color: rgba(255, 255, 255, 0.7); }
</style>
```

- [ ] **Step 2: Verify build clean**

- [ ] **Step 3: Commit**

```bash
git add crates/ui/src-svelte/src/lib/components/MessageBubble.svelte
git commit -m "$(cat <<'EOF'
feat(ui): MessageBubble renders DeliveryIcon for outgoing messages

Status sourced from the delivery store + the bubble's own
optimistic / failed flags. Icon hides for incoming bubbles
(delivery state is sender-side only). Tooltip text is derived
from the status (or the failure reason when present).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 20: `VirtualMessageList.svelte` — observers, separator, skeletons

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/components/VirtualMessageList.svelte`

This is the largest single-component change in the plan. The current file is 51 lines; the new version is ~120.

- [ ] **Step 1: Replace the file body:**

```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<!--
  VirtualMessageList: virtualised list of MessageRecord items.
  Phase 2.D additions:
   - top-of-list IntersectionObserver → conversation.loadOlder()
   - bottom-of-list IntersectionObserver → conversation.markReadIfAtBottom()
   - inline UnreadSeparator at row_id == unreadAnchorRowId
   - inline SkeletonBubble × 5 at the top during loadingOlder
-->
<script lang="ts">
  import { createVirtualizer } from "@tanstack/svelte-virtual";
  import type { MessageRecord } from "$lib/ipc/types";
  import type { OptimisticMessage } from "$lib/stores/conversation";
  import {
    conversation,
    loadOlder,
    markReadIfAtBottom,
    isWithinBottomThreshold,
  } from "$lib/stores/conversation";
  import MessageBubble from "./MessageBubble.svelte";
  import UnreadSeparator from "./UnreadSeparator.svelte";
  import SkeletonBubble from "./SkeletonBubble.svelte";

  let { items }: { items: (MessageRecord | OptimisticMessage)[] } = $props();
  let scrollEl = $state<HTMLDivElement | undefined>(undefined);
  let topSentinel = $state<HTMLDivElement | undefined>(undefined);
  let bottomSentinel = $state<HTMLDivElement | undefined>(undefined);

  const ESTIMATED_ROW_HEIGHT = 72;
  const SKELETON_COUNT = 5;

  // Build the renderable list: optional skeletons, then messages with an
  // optional separator inserted between unreadAnchorRowId and the next row.
  type Row =
    | { kind: "skeleton"; key: string }
    | { kind: "separator"; key: string }
    | { kind: "message"; key: string; record: MessageRecord | OptimisticMessage };

  const rows = $derived.by((): Row[] => {
    const out: Row[] = [];
    if ($conversation.loadingOlder) {
      for (let i = 0; i < SKELETON_COUNT; i++) {
        out.push({ kind: "skeleton", key: `skel-${i}` });
      }
    }
    const anchor = $conversation.unreadAnchorRowId;
    let separatorEmitted = false;
    for (const m of items) {
      out.push({ kind: "message", key: rowKey(m), record: m });
      if (
        anchor !== null &&
        !separatorEmitted &&
        m.row_id === anchor
      ) {
        out.push({ kind: "separator", key: "unread-separator" });
        separatorEmitted = true;
      }
    }
    return out;
  });

  function rowKey(m: MessageRecord | OptimisticMessage): string {
    const tempId = (m as OptimisticMessage).__tempId;
    if (tempId) return `t-${tempId}`;
    return `r-${m.row_id}`;
  }

  let virtualizer = $derived(
    scrollEl
      ? createVirtualizer<HTMLDivElement, HTMLDivElement>({
          count: rows.length,
          getScrollElement: () => scrollEl!,
          estimateSize: () => ESTIMATED_ROW_HEIGHT,
          overscan: 5,
        })
      : null,
  );

  let virtualItems = $derived($virtualizer?.getVirtualItems() ?? []);
  let totalHeight = $derived($virtualizer?.getTotalSize() ?? 0);

  $effect(() => {
    if (!topSentinel) return;
    const obs = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          void loadOlder();
        }
      },
      { root: scrollEl, rootMargin: "100px 0px 0px 0px" },
    );
    obs.observe(topSentinel);
    return () => obs.disconnect();
  });

  $effect(() => {
    if (!bottomSentinel || !scrollEl) return;
    const obs = new IntersectionObserver(
      (entries) => {
        if (!entries.some((e) => e.isIntersecting)) return;
        // Find the last MessageRecord row's row_id (skip skeletons / separator).
        for (let i = items.length - 1; i >= 0; i--) {
          const r = items[i];
          if (typeof r.row_id === "bigint" && r.row_id > 0n) {
            markReadIfAtBottom(r.row_id);
            return;
          }
        }
      },
      { root: scrollEl, threshold: 0.5 },
    );
    obs.observe(bottomSentinel);
    return () => obs.disconnect();
  });
</script>

<div class="list" bind:this={scrollEl}>
  <div bind:this={topSentinel} class="sentinel"></div>
  <div style="height: {totalHeight}px; position: relative;">
    {#each virtualItems as row (rows[row.index]?.key ?? row.index)}
      <div
        style="position: absolute; top: 0; left: 0; width: 100%; transform: translateY({row.start}px);"
      >
        {#if rows[row.index]?.kind === "message"}
          <MessageBubble record={(rows[row.index] as { record: MessageRecord }).record} />
        {:else if rows[row.index]?.kind === "separator"}
          <UnreadSeparator />
        {:else if rows[row.index]?.kind === "skeleton"}
          <SkeletonBubble />
        {/if}
      </div>
    {/each}
  </div>
  <div bind:this={bottomSentinel} class="sentinel"></div>
</div>

<style>
  .list { height: 100%; overflow-y: auto; padding: var(--s-3); box-sizing: border-box; }
  .sentinel { height: 1px; }
</style>
```

- [ ] **Step 2: Verify build clean and the existing 2.C `App.test` / page tests still pass**

```bash
cd /home/myggiz/development/skattr/crates/ui/src-svelte && pnpm vitest run 2>&1 | tail -15
```

- [ ] **Step 3: Commit**

```bash
git add crates/ui/src-svelte/src/lib/components/VirtualMessageList.svelte
git commit -m "$(cat <<'EOF'
feat(ui): VirtualMessageList — observers, separator, skeletons

Top sentinel + IntersectionObserver triggers loadOlder when the
list approaches the start. Bottom sentinel triggers
markReadIfAtBottom when the last bubble enters view. Skeleton
rows render at the top during loadingOlder; UnreadSeparator
renders inline at row_id == unreadAnchorRowId. All three row
kinds participate in the virtualizer with stable keys.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 21: `Composer.svelte` + tests

**Files:**
- Create: `crates/ui/src-svelte/src/lib/components/Composer.svelte`
- Create: `crates/ui/src-svelte/src/lib/components/Composer.test.ts`
- Modify: `crates/ui/src-svelte/src/lib/stores/conversation.ts` (add `send` method that wires the optimistic flow into IPC)

- [ ] **Step 1: Add the `send` orchestrator** — append to `conversation.ts`:

```typescript
import { recordDeliveryStatus, hex16ToString } from "./delivery";

export async function send(contact: PublicKey, body: string): Promise<void> {
  const tempId = crypto.randomUUID();
  appendOptimistic(contact, body, tempId);
  try {
    const resp = await ipcClient.request({
      cmd: "send_message",
      contact,
      kind: { kind: "text", body },
    });
    const result = unwrapOk(resp);
    if (result.result !== "message_sent") {
      markFailed(tempId, "unexpected reply variant");
      return;
    }
    const { message_id, status, record } = result.data;
    if (record) {
      reconcile(tempId, record);
      recordDeliveryStatus(
        hex16ToString(message_id),
        status === "delivered"
          ? ({ type: "delivered" } as any)
          : ({ type: "queued" } as any),
      );
    } else {
      // Idempotent retry — promote optimistic to canonical without
      // resorting to the failure flag.
      conversation.update((s) => {
        const idx = s.messages.findIndex(
          (m) => (m as OptimisticMessage).__tempId === tempId,
        );
        if (idx < 0) return s;
        const next = [...s.messages];
        const original = next[idx] as OptimisticMessage;
        next[idx] = { ...original, __optimistic: false } as any;
        return { ...s, messages: next };
      });
    }
  } catch (e) {
    markFailed(tempId, e instanceof Error ? e.message : String(e));
  }
}
```

- [ ] **Step 2: Write the failing tests** — `Composer.test.ts`:

```typescript
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
import { describe, expect, test, beforeEach, vi } from "vitest";
import { render, fireEvent } from "@testing-library/svelte";
import Composer from "./Composer.svelte";
import { ipcClient } from "$lib/ipc/tauri";
import type { PublicKey } from "$lib/ipc/types";

const peer: PublicKey = { "0": new Array(32).fill(7) } as any;

beforeEach(() => {
  vi.spyOn(ipcClient, "request").mockResolvedValue({
    resp: "ok",
    data: {
      result: "message_sent",
      data: {
        message_id: { "0": new Array(16).fill(0) },
        status: "queued",
        record: null,
      },
    },
  } as any);
});

describe("Composer", () => {
  test("Enter sends when not composing", async () => {
    const { getByRole } = render(Composer, { props: { contact: peer, disabled: false } });
    const ta = getByRole("textbox") as HTMLTextAreaElement;
    await fireEvent.input(ta, { target: { value: "hi" } });
    await fireEvent.keyDown(ta, { key: "Enter", isComposing: false });
    expect(ipcClient.request).toHaveBeenCalledWith(
      expect.objectContaining({
        cmd: "send_message",
        kind: { kind: "text", body: "hi" },
      }),
    );
  });

  test("Enter no-ops while IME composition is active", async () => {
    const { getByRole } = render(Composer, { props: { contact: peer, disabled: false } });
    const ta = getByRole("textbox") as HTMLTextAreaElement;
    await fireEvent.input(ta, { target: { value: "x" } });
    await fireEvent.compositionStart(ta);
    await fireEvent.keyDown(ta, { key: "Enter", isComposing: true });
    expect(ipcClient.request).not.toHaveBeenCalled();
  });

  test("Shift+Enter does not send", async () => {
    const { getByRole } = render(Composer, { props: { contact: peer, disabled: false } });
    const ta = getByRole("textbox") as HTMLTextAreaElement;
    await fireEvent.input(ta, { target: { value: "hi" } });
    await fireEvent.keyDown(ta, { key: "Enter", shiftKey: true, isComposing: false });
    expect(ipcClient.request).not.toHaveBeenCalled();
  });

  test("Empty / whitespace input + Enter no-ops", async () => {
    const { getByRole } = render(Composer, { props: { contact: peer, disabled: false } });
    const ta = getByRole("textbox") as HTMLTextAreaElement;
    await fireEvent.input(ta, { target: { value: "   " } });
    await fireEvent.keyDown(ta, { key: "Enter", isComposing: false });
    expect(ipcClient.request).not.toHaveBeenCalled();
  });

  test("paste inserts only text/plain", async () => {
    const { getByRole } = render(Composer, { props: { contact: peer, disabled: false } });
    const ta = getByRole("textbox") as HTMLTextAreaElement;
    const dt = new DataTransfer();
    dt.setData("text/html", "<b>bold</b>");
    dt.setData("text/plain", "bold");
    const paste = new ClipboardEvent("paste", { clipboardData: dt, bubbles: true });
    ta.dispatchEvent(paste);
    expect(ta.value).toBe("bold");
    expect(ta.value).not.toContain("<b>");
  });

  test("disabled prop disables textarea + send button", () => {
    const { getByRole } = render(Composer, {
      props: { contact: peer, disabled: true, disabledReason: "Daemon not running" },
    });
    expect((getByRole("textbox") as HTMLTextAreaElement).disabled).toBe(true);
    expect((getByRole("button") as HTMLButtonElement).disabled).toBe(true);
  });
});
```

- [ ] **Step 3: Run — must fail (component undefined)**

- [ ] **Step 4: Implement `Composer.svelte`:**

```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<script lang="ts">
  import { send } from "$lib/stores/conversation";
  import type { PublicKey } from "$lib/ipc/types";

  let {
    contact,
    disabled,
    disabledReason,
  }: {
    contact: PublicKey;
    disabled: boolean;
    disabledReason?: string;
  } = $props();

  let text = $state("");
  let composing = $state(false);
  let textarea = $state<HTMLTextAreaElement | undefined>(undefined);

  async function trySend(): Promise<void> {
    const trimmed = text.trim();
    if (!trimmed || disabled) return;
    text = "";
    await send(contact, trimmed);
  }

  function onKeyDown(e: KeyboardEvent): void {
    if (e.key !== "Enter") return;
    if (e.shiftKey) return; // Shift+Enter inserts newline (default behaviour).
    if (e.isComposing || composing) return;
    e.preventDefault();
    void trySend();
  }

  function onPaste(e: ClipboardEvent): void {
    if (!e.clipboardData) return;
    e.preventDefault();
    const plain = e.clipboardData.getData("text/plain");
    if (!plain || !textarea) return;
    const start = textarea.selectionStart;
    const end = textarea.selectionEnd;
    text = text.slice(0, start) + plain + text.slice(end);
    // Restore caret after the inserted text.
    queueMicrotask(() => {
      if (textarea) {
        textarea.selectionStart = start + plain.length;
        textarea.selectionEnd = start + plain.length;
      }
    });
  }
</script>

<form class="composer" onsubmit={(e) => { e.preventDefault(); void trySend(); }}>
  <textarea
    bind:this={textarea}
    bind:value={text}
    {disabled}
    placeholder={disabled ? (disabledReason ?? "Disabled") : "Type a message"}
    rows={1}
    onkeydown={onKeyDown}
    onpaste={onPaste}
    oncompositionstart={() => (composing = true)}
    oncompositionend={() => (composing = false)}
    aria-label="Message input"
  ></textarea>
  <button type="submit" {disabled} aria-label="Send">Send</button>
</form>

<style>
  .composer {
    display: flex;
    align-items: flex-end;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    border-top: 1px solid var(--bg-elevated);
  }
  textarea {
    flex: 1;
    resize: none;
    min-height: 2.5rem;
    max-height: 8rem;
    padding: var(--s-2);
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid transparent;
    border-radius: 8px;
    font: inherit;
  }
  textarea:focus { outline: none; border-color: var(--accent); }
  textarea:disabled { opacity: 0.5; cursor: not-allowed; }
  button {
    padding: var(--s-2) var(--s-3);
    background: var(--accent);
    color: var(--bg);
    border: 0;
    border-radius: 8px;
    font: inherit;
    cursor: pointer;
  }
  button:disabled { opacity: 0.5; cursor: not-allowed; }
</style>
```

- [ ] **Step 5: Run — must pass**

```bash
cd /home/myggiz/development/skattr/crates/ui/src-svelte && pnpm vitest run Composer 2>&1 | tail -15
```

- [ ] **Step 6: Commit**

```bash
git add crates/ui/src-svelte/src/lib/components/Composer.svelte crates/ui/src-svelte/src/lib/components/Composer.test.ts crates/ui/src-svelte/src/lib/stores/conversation.ts
git commit -m "$(cat <<'EOF'
feat(ui): add Composer component + send orchestrator

Enter-to-send (gated on !shiftKey, !isComposing, !composing,
non-whitespace), Shift+Enter inserts newline, paste preempts
default and inserts only text/plain. Disabled prop drives the
textarea + send button. send() in conversation.ts generates
tempId, appends optimistic, awaits MessageSent reply,
reconciles or marks failed.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 22: Wire `<Composer>` + group_state-aware disabled into `+page.svelte`

**Files:**
- Modify: `crates/ui/src-svelte/src/routes/+page.svelte`

- [ ] **Step 1: Open the file** and find where the conversation pane renders. It currently renders `<VirtualMessageList items={$conversation.messages} />` (or similar) without a composer below it.

- [ ] **Step 2: Inject the composer below the message list**, plumbing the disabled state from the active contact's summary.

Pattern to follow — adapt to whatever variable names the existing file uses for the active contact summary:

```svelte
<script lang="ts">
  // …existing imports…
  import Composer from "$lib/components/Composer.svelte";
  import VirtualMessageList from "$lib/components/VirtualMessageList.svelte";
  import { conversation } from "$lib/stores/conversation";
  import { contacts } from "$lib/stores/contacts";

  let activeSummary = $derived(
    $contacts.find((c) =>
      $conversation.contact !== null &&
      JSON.stringify(c.pubkey) === JSON.stringify($conversation.contact),
    ),
  );

  let composerDisabled = $derived(
    activeSummary === undefined ||
    activeSummary.group_state === "corrupt" ||
    activeSummary.group_state === "pending_join",
  );

  let disabledReason = $derived(
    activeSummary === undefined         ? "Select a contact"
  : activeSummary.group_state === "corrupt"      ? "Conversation unavailable"
  : activeSummary.group_state === "pending_join" ? "Joining group…"
                                                 : undefined,
  );
</script>

<!-- …existing layout… -->
<section class="conversation-pane">
  {#if $conversation.contact !== null}
    <VirtualMessageList items={$conversation.messages} />
    <Composer contact={$conversation.contact} disabled={composerDisabled} {disabledReason} />
  {:else}
    <p class="empty">Select a contact</p>
  {/if}
</section>
```

The exact JSX-ish structure depends on the 2.C layout. The constraints:
- Composer renders below VirtualMessageList in the same flex column.
- `composerDisabled` is `true` whenever `group_state` is `corrupt` or `pending_join`.
- `activeSummary` is recomputed whenever `$conversation.contact` or `$contacts` changes.

- [ ] **Step 3: Build + smoke-test**

```bash
cd /home/myggiz/development/skattr/crates/ui/src-svelte && pnpm build && pnpm vitest run 2>&1 | tail -10
```

- [ ] **Step 4: Commit**

```bash
git add crates/ui/src-svelte/src/routes/+page.svelte
git commit -m "$(cat <<'EOF'
feat(ui): wire Composer into the conversation pane

Composer renders below VirtualMessageList when a contact is
active. Disabled when the active ContactSummary's group_state
is corrupt (Conversation unavailable) or pending_join (Joining
group…). disabledReason placeholder text matches spec §4.1.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase E — End-to-end + cross-binary tests

### Task 23: Playwright `composer.spec.ts`

**Files:**
- Create: `crates/ui/src-svelte/tests/e2e/composer.spec.ts`

- [ ] **Step 1: Write the spec** — pattern matches existing `first-run.spec.ts`:

```typescript
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
import { test, expect } from "@playwright/test";

test.describe("composer happy path", () => {
  test.beforeEach(async ({ page }) => {
    // Navigate via a fixture URL that bypasses first-run + seeds a
    // test contact with a real MLS group. If the existing 2.C harness
    // doesn't expose this, add a query-param hook in routes/+page.svelte
    // that loads a fixture state in dev/test mode.
    await page.goto("/?fixture=seeded-contact");
  });

  test("Enter sends, optimistic bubble appears, status promotes to delivered", async ({ page }) => {
    // Click the seeded contact in the list.
    await page.locator(".contact-row").first().click();

    const composer = page.getByLabel("Message input");
    await expect(composer).toBeVisible();

    await composer.fill("hello");
    await composer.press("Enter");

    // Optimistic bubble with clock icon (pending).
    const bubble = page.locator(".bubble.outgoing").last();
    await expect(bubble).toContainText("hello");
    await expect(bubble.locator("svg circle[r='10']")).toBeVisible({ timeout: 1000 });

    // Wait for the mocked daemon to advance status to delivered.
    // The mock fires DeliveryStatusChanged { status: Delivered } 200 ms in.
    await expect(bubble.locator("svg path[d^='M18 6 7 17']"))
      .toBeVisible({ timeout: 2000 });
  });

  test("Shift+Enter inserts newline without sending", async ({ page }) => {
    await page.locator(".contact-row").first().click();
    const composer = page.getByLabel("Message input");
    await composer.fill("hi");
    await composer.press("Shift+Enter");
    await composer.type("world");
    expect(await composer.inputValue()).toBe("hi\nworld");
    // No bubble appended.
    await expect(page.locator(".bubble.outgoing")).toHaveCount(0);
  });
});
```

The exact test-mode fixture (`?fixture=seeded-contact`) and how the mock advances delivery state depend on what 2.C's e2e harness already supports. Read `crates/ui/src-svelte/tests/e2e/first-run.spec.ts` and the `playwright.config.ts` to find the convention. If a fixture seed mechanism doesn't exist yet, add one as a small `+page.svelte` `$effect` that, when `dev && URL.searchParams.has("fixture")`, populates the contacts + conversation stores with hard-coded data and skips the IPC bootstrap.

- [ ] **Step 2: Run — must pass**

```bash
cd /home/myggiz/development/skattr/crates/ui/src-svelte && pnpm playwright test composer 2>&1 | tail -15
```

- [ ] **Step 3: Commit**

```bash
git add crates/ui/src-svelte/tests/e2e/composer.spec.ts
git commit -m "$(cat <<'EOF'
test(ui): playwright composer e2e — Enter + Shift+Enter paths

Seeds a contact via ?fixture=seeded-contact and exercises the
happy path: type → Enter → optimistic clock-icon bubble → mock
advances → check-check icon. Plus the Shift+Enter newline
behaviour with no IPC.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 24: Playwright `pagination.spec.ts`

**Files:**
- Create: `crates/ui/src-svelte/tests/e2e/pagination.spec.ts`

- [ ] **Step 1: Write the spec:**

```typescript
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
import { test, expect } from "@playwright/test";

test.describe("conversation pagination", () => {
  test("scroll-to-top loads older pages until cursor exhausts", async ({ page }) => {
    // Fixture seeds the active contact's group with 200 messages.
    await page.goto("/?fixture=seeded-200-msgs");
    await page.locator(".contact-row").first().click();

    const list = page.locator(".list");
    await expect(list).toBeVisible();
    await expect(page.locator(".bubble")).toHaveCount(50, { timeout: 2000 });

    // Scroll list to the very top to trigger the top sentinel.
    await list.evaluate((el) => (el.scrollTop = 0));

    // 5 skeleton bubbles render during the in-flight load.
    await expect(page.locator(".skeleton")).toHaveCount(5, { timeout: 1000 });

    await expect(page.locator(".bubble")).toHaveCount(100, { timeout: 3000 });

    // Continue scrolling until exhaustion.
    for (let pageNum = 2; pageNum < 4; pageNum++) {
      await list.evaluate((el) => (el.scrollTop = 0));
      await expect(page.locator(".bubble")).toHaveCount(50 + 50 * pageNum, { timeout: 3000 });
    }

    // Last page returns 0 records (200 / 50 = exactly 4 pages).
    await list.evaluate((el) => (el.scrollTop = 0));
    // No further fetch — bubble count stays at 200.
    await page.waitForTimeout(500);
    await expect(page.locator(".bubble")).toHaveCount(200);
  });
});
```

- [ ] **Step 2: Run — must pass**

```bash
cd /home/myggiz/development/skattr/crates/ui/src-svelte && pnpm playwright test pagination 2>&1 | tail -15
```

- [ ] **Step 3: Commit**

```bash
git add crates/ui/src-svelte/tests/e2e/pagination.spec.ts
git commit -m "$(cat <<'EOF'
test(ui): playwright pagination e2e — 200-msg scroll-back

Seeds a 200-message conversation; verifies first page = 50,
scroll-to-top triggers loadOlder with 5 skeletons, each
subsequent page extends bubble count by 50 until the cursor
exhausts at 200. Confirms no further fetches once
next_before_id hits null.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 25: Update `cli_two_daemons` to assert `record.is_some()`

**Files:**
- Modify: `crates/tests/src/cli_two_daemons.rs`

- [ ] **Step 1: Find the `MessageSent` match** in `cli_two_daemons.rs`:

```bash
cd /home/myggiz/development/skattr && grep -n "MessageSent" crates/tests/src/cli_two_daemons.rs
```

- [ ] **Step 2: Update the match** to capture and assert `record`:

```rust
match result {
    CommandResult::MessageSent { message_id: _, status: _, record } => {
        assert!(record.is_some(), "Phase 2.D: MessageSent must carry sender-side record");
        let rec = record.unwrap();
        assert!(rec.row_id > 0);
        assert_eq!(rec.direction, Direction::Outgoing);
    }
    other => panic!("expected MessageSent, got {other:?}"),
}
```

(Add `use skattr_core::daemon::commands::Direction;` if not already imported.)

- [ ] **Step 3: Run the test (already gated)**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-tests --features test-harness cli_two_daemons 2>&1 | tail -15
```

If the test is `#[ignore]`-gated, run with `-- --ignored`.

- [ ] **Step 4: Commit**

```bash
git add crates/tests/src/cli_two_daemons.rs
git commit -m "$(cat <<'EOF'
test(tests): assert MessageSent.record is_some() in cli_two_daemons

Phase 2.D wire-format extension is now guaranteed end-to-end
through the CLI's IPC roundtrip.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 26: Real-Tor end-to-end UI send roundtrip

**Files:**
- Create: `crates/tests/src/ui_send_roundtrip.rs`

This test is `#[ignore]`-gated alongside `cli_real_tor` / `delivery_real_tor`. It exercises the full UI-to-UI message path: daemon A's `Command::SendMessage` returns `MessageSent { record: Some(_) }`, the cipher actually flows over Arti, daemon B emits `Event::MessageReceived` with a matching record, and both daemons' SQLite reflects the exchange.

- [ ] **Step 1: Create the file** — pattern follows `cli_real_tor.rs` exactly. Skeleton:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg(feature = "test-harness")]

//! End-to-end UI-style message roundtrip over real Arti.
//! Mirrors `cli_real_tor.rs` but asserts the Phase 2.D wire-format
//! additions: `MessageSent.record.is_some()` on the sender side
//! and `Event::MessageReceived.record.row_id` on the receiver side.

use std::time::Duration;

use skattr_core::daemon::commands::{Command, CommandResult, Direction};
use skattr_core::daemon::events::Event;
// Reuse the existing two-daemon harness from cli_real_tor.rs.

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn ui_send_roundtrip_over_real_tor() {
    // 1. Spawn two daemons, pair them via invite (helper from
    //    cli_real_tor.rs — copy or refactor into a shared module).
    let (a, b) = pair_two_daemons_over_real_tor().await;

    // 2. From daemon A's IPC, SendMessage to daemon B's pubkey.
    let result = a
        .ipc
        .execute(Command::SendMessage {
            contact: b.pubkey,
            kind: skattr_core::envelope::Kind::Text { body: "hello".into() },
        })
        .await
        .expect("send_message dispatch");

    let (sender_record, message_id) = match result {
        CommandResult::MessageSent { record: Some(rec), message_id, .. } => (rec, message_id),
        other => panic!("expected MessageSent with record, got {other:?}"),
    };

    assert!(sender_record.row_id > 0);
    assert_eq!(sender_record.direction, Direction::Outgoing);

    // 3. Wait up to 30 s for daemon B to emit MessageReceived.
    let received_record = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match b.events.recv().await.expect("events channel open") {
                Event::MessageReceived { record, .. } if record.message_id == message_id => {
                    break record;
                }
                _ => continue,
            }
        }
    })
    .await
    .expect("MessageReceived within 30 s");

    assert!(received_record.row_id > 0);
    assert_eq!(received_record.direction, Direction::Incoming);
    match &received_record.kind {
        skattr_core::envelope::Kind::Text { body } => assert_eq!(body, "hello"),
        other => panic!("unexpected kind {other:?}"),
    }

    // 4. Mark-read on B; then list_contacts on B and confirm
    //    last_read_row_id == received_record.row_id.
    b.ipc
        .execute(Command::MarkRead {
            contact: a.pubkey,
            up_to_message_id: received_record.row_id,
        })
        .await
        .unwrap();
    let summaries = match b
        .ipc
        .execute(Command::ListContacts)
        .await
        .unwrap()
    {
        CommandResult::Contacts(s) => s,
        other => panic!("expected Contacts, got {other:?}"),
    };
    let summary = summaries.into_iter().find(|c| c.pubkey == a.pubkey).unwrap();
    assert_eq!(summary.last_read_row_id, Some(received_record.row_id));
}

// Helper: refactor pair_two_daemons_over_real_tor out of cli_real_tor.rs
// into a shared module if not already.
async fn pair_two_daemons_over_real_tor() -> (DaemonHarness, DaemonHarness) {
    // Implementation: same as cli_real_tor.rs's pairing logic.
    // …
    todo!("port pair_two_daemons_over_real_tor from cli_real_tor.rs")
}

struct DaemonHarness {
    pubkey: skattr_core::identity::PublicKey,
    ipc: skattr_core::ipc::IpcClient,
    events: tokio::sync::mpsc::UnboundedReceiver<Event>,
}
```

The test depends on a real harness. **Don't leave the `todo!()` unresolved** — port the pairing helper from `cli_real_tor.rs` into `crates/tests/src/lib.rs` (or a sibling module) so both tests share it. Substitute the actual `IpcClient` import path; if the existing tests use `skattr_core::test_exports::*`, do the same here.

- [ ] **Step 2: Run with `--ignored`**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-tests --features test-harness --release ui_send_roundtrip -- --ignored 2>&1 | tail -30
```

This requires Tor outbound network — expected to take ~30 s on a clean clone with bootstrap.

- [ ] **Step 3: Commit**

```bash
git add crates/tests/src/ui_send_roundtrip.rs crates/tests/src/lib.rs
git commit -m "$(cat <<'EOF'
test(tests): real-Tor UI send roundtrip

#[ignore]-gated end-to-end test exercising Phase 2.D's wire-format
additions over the actual Arti stack. Asserts:
- MessageSent.record.is_some() on the sender side
- Event::MessageReceived.record.row_id > 0 on the receiver side
- ContactSummary.last_read_row_id reflects the post-mark-read
  cursor.

Pairing helper refactored out of cli_real_tor.rs into a shared
module; both tests reuse it.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase F — Wrap

### Task 27: Full local sweep

- [ ] **Step 1: Format + clippy + test**

```bash
. "$HOME/.cargo/env" && cargo fmt --all -- --check
. "$HOME/.cargo/env" && cargo clippy --workspace --all-targets --features test-harness -- -D warnings
. "$HOME/.cargo/env" && cargo test --workspace --features test-harness
```

If any of the three fail, drop back into the relevant task and fix. Repeat the full sweep until all three are green.

- [ ] **Step 2: UI build + tests**

```bash
cd /home/myggiz/development/skattr/crates/ui/src-svelte && pnpm install --frozen-lockfile && pnpm build && pnpm vitest run && pnpm playwright test
```

- [ ] **Step 3: Licence + advisory**

```bash
. "$HOME/.cargo/env" && cargo deny check
. "$HOME/.cargo/env" && cargo audit
```

Both must pass clean.

- [ ] **Step 4: No commit** — this task is purely verification. If anything fails, fix in-place under the relevant earlier task before merging.

---

### Task 28: CHANGELOG + CLAUDE.md status update + final commit

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add a CHANGELOG entry** — under whatever heading the existing CHANGELOG uses (usually `## Unreleased` or a new dated section). Pattern:

```markdown
## Phase 2.D — 2026-05-XX

### Added
- Conversation view composer (Enter to send, Shift+Enter newline,
  paste-as-plaintext, IME-safe Enter gating).
- Per-message delivery state icons (clock / check / check-check /
  alert-triangle) backed by Lucide ISC SVG bundles.
- Scroll-back pagination via `Command::RecentMessages.before_id` +
  `paged: bool` flag returning a new `CommandResult::MessagesPage`
  variant.
- Frozen "Unread" separator anchored to `ContactSummary.last_read_
  row_id` at conversation-open.
- `MessageSent.record: Option<MessageRecord>` for UI optimistic
  reconciliation.
- `ContactSummary.group_state: Option<MlsGroupStateLabel>` for
  composer-disabled detection.
- `--danger` design token (7th colour).
- Skeleton bubble loading state during `loadOlder` in-flight.
- `MessageRepo::recent_before` storage method.

### Tests
- Wire-format snapshot lint (`crates/core/tests/wire_format_append_only.rs`).
- 7 new dispatch tests for paged recent_messages + sender-side
  record projection.
- 13 new Vitest specs (DeliveryIcon, Composer, conversation,
  delivery).
- 2 new Playwright e2e specs (composer, pagination).
- 1 new real-Tor integration test (`ui_send_roundtrip`,
  `#[ignore]`-gated).
```

- [ ] **Step 2: Update `CLAUDE.md` "Repository state" section** — add a paragraph after the existing 2.C summary:

```markdown
Phase 2.D (conversation view) merged at the head of
`phase-2d-conversation-view`. The composer (Enter-to-send,
Shift+Enter newline, paste-as-plaintext, IME-safe), per-message
delivery state icons (clock → check → check-check → !), and
scroll-back pagination (50 rows/page, `before_id` cursor, 5
skeleton bubbles during loads) round out the conversation pane.
Wire-format additions are strictly additive: `Command::Recent
Messages` gains `before_id: Option<i64>` + `paged: bool` (both
`#[serde(default)]`); new `CommandResult::MessagesPage { records,
next_before_id }` variant alongside the unchanged `Messages(Vec)`;
`MessageSent` gains `record: Option<MessageRecord>`;
`ContactSummary` gains `group_state: Option<MlsGroupStateLabel>`
+ `last_read_row_id: Option<i64>`. New storage method
`MessageRepo::recent_before` powers pagination. The frozen
"Unread" separator anchors to `ContactSummary.last_read_row_id`
at conversation-open and never advances live. Mark-read fires on
both conversation-open and bottom-of-list intersection (debounced
500 ms, scroll-proximity ≤ 100 px). New wire-format snapshot
test (`crates/core/tests/wire_format_append_only.rs`) makes
adding/removing a `Command` or `CommandResult` variant a
deliberate edit.
```

Also update the "next workstream" line to point at 2.E (Phase 2.E:
contact-rename + invite UX).

- [ ] **Step 3: Commit**

```bash
git add CHANGELOG.md CLAUDE.md
git commit -m "$(cat <<'EOF'
docs: Phase 2.D CHANGELOG + CLAUDE.md status update

Phase 2.D conversation view merged. Wire-format additions are
strictly additive (before_id + paged on RecentMessages, new
MessagesPage variant, record on MessageSent, group_state +
last_read_row_id on ContactSummary). Composer, delivery icons,
pagination, mark-read, frozen unread separator all wired.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-review checklist (run before declaring the plan done)

- [ ] **Spec coverage** — every section of `docs/superpowers/specs/2026-05-02-phase-2d-conversation-view-design.md` is implemented by at least one task. §1 → tasks 1, 3, 4, 5; §2 → tasks 12-22; §3 → tasks 6, 7, 8; §4 → tasks 19, 21, 22 (composer-disabled), 17, 18 (mark-read edges), 16 (idempotent retry); §5 → all of phase E + the dispatch + storage + Vitest tasks scattered through A-D.
- [ ] **No placeholders** — every code block contains the actual code; no "fill in details", no "similar to Task N".
- [ ] **Type consistency** — `OptimisticMessage` is the same type in tasks 16, 17, 18, 19, 21; `MessagesPage` shape is consistent in tasks 4, 6, 16, 17, 24; `MlsGroupStateLabel` variants are `Active | PendingJoin | Corrupt` everywhere; `last_read_row_id: Option<i64>` is consistent.
- [ ] **Test ordering** — every behavioural task writes the failing test before the implementation. Phase B's CSS-only tasks (13, 14) skip tests deliberately (visual primitives exercised by Phase E e2e).
- [ ] **Frequent commits** — every task ends with a commit; CHANGELOG/CLAUDE.md churn deferred to task 28 to avoid mid-stream noise.
