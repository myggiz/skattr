# Phase 2.D — Conversation view design

**Status:** drafted 2026-05-02; awaiting plan.
**Predecessor:** Phase 2.C UI bootstrap (merged 2026-05-02).
**Umbrella:** `docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md` § "2.D — Conversation view".

## Scope

Phase 2.D turns 2.C's read-only conversation MVP into a working
two-way text chat between paired daemons:

- a composer (Enter to send, Shift+Enter newline, paste-as-plaintext)
- per-message delivery state icons (clock → check → check-check → !)
- scroll-back pagination via a `before_id: i64` cursor
- a frozen "Unread" separator anchored to the read cursor at
  conversation-open time
- a debounced mark-read trigger that fires on open and on
  bottom-of-list intersection

Out of scope (deferred to 2.E / 2.F): invite UX, contact rename /
remove, settings panel, mailbox CRUD UI, notifications, tray.

**Exit criterion:** two daemons paired on one machine exchange
text messages UI-to-UI; the sender's outgoing bubble reaches
`Delivered`; the recipient's `Event::MessageReceived` renders a
new incoming bubble; mark-read advances correctly on both sides;
pagination loads ≥3 pages of 50-row history.

## Locked decisions (from kickoff brainstorm 2026-05-02)

| # | Decision | Resolution |
|---|---|---|
| 1 | Send rendering | **Optimistic placeholder.** Append a temp bubble on Enter; reconcile when `MessageSent.record` arrives. |
| 2 | `RecentMessages` paged response shape | **New variant** `MessagesPage { records, next_before_id }`. Existing `Messages(Vec)` keeps tuple shape — CLI unchanged. UI sets `paged: true` to opt in. |
| 3 | Mark-read trigger | **Both** open-event AND bottom-intersection. Live-arriving messages mark only when scrolled within 100 px of bottom. Debounce 500 ms. |
| 4 | Pagination loading state | **5 skeleton bubbles** above the list. Page size **50**. Topmost-bubble `IntersectionObserver` triggers `loadOlder()`. |
| 5 | Delivery icon family | **Lucide MIT.** Bundled inline SVG. 4 glyphs locked. |
| 6 | Add `--danger` design token | **Yes.** 7th colour token; load-bearing for failure rendering across 2.D / 2.E / 2.F. |
| 7 | State → icon mapping | Optimistic + `Queued` → clock; `Deposited` → single check; `Delivered` → check-check; `Failed` → triangle. |
| 8 | Unread separator persistence | **Frozen** at open time; does not advance live. Re-anchors on close+reopen. |
| 9 | IME composition gating | Enter no-op while `event.isComposing == true` or between unmatched `compositionstart` / `compositionend`. Shift+Enter inserts `\n` regardless. |
| 10 | Pagination cursor | `WHERE id < ?before_id ORDER BY mls_generation DESC, id DESC LIMIT 50`. Strict-less; cursor row excluded. |
| 11 | CLI consumer of `MessageSent.record` | **Ignore.** Field is `Option<MessageRecord>` with `#[serde(default)]`; old CLI builds decode unchanged. |

## §1 — Wire format (additive only)

Every change preserves CBOR backward decode: existing variants are
not reshaped; new fields default; new variants are added alongside
the existing ones.

### `Command`

```rust
pub enum Command {
    // existing variants unchanged…
    RecentMessages {
        contact: Option<PublicKey>,
        limit: u32,
        #[serde(default)]
        before_id: Option<i64>,   // NEW: pagination cursor (strict-less on row_id)
        #[serde(default)]
        paged: bool,              // NEW: opt-in to MessagesPage response
    },
}
```

`paged` is the explicit discriminator. UI sets `true` and gets
`MessagesPage`; CLI omits and gets `Messages(Vec)`. Defaulting
both fields keeps old encoded `RecentMessages { contact, limit }`
forms decoding cleanly.

### `CommandResult`

```rust
pub enum CommandResult {
    // existing variants unchanged (Messages(Vec<MessageRecord>) keeps tuple shape)…

    /// NEW: paged form of recent_messages. Returned when the
    /// request had `paged: true`.
    MessagesPage {
        records: Vec<MessageRecord>,
        next_before_id: Option<i64>,
    },

    MessageSent {
        message_id: Hex16,
        status: SendStatus,
        #[serde(default)]
        record: Option<MessageRecord>,   // NEW: canonical sender-side row
    },
}
```

### `ContactSummary`

```rust
pub struct ContactSummary {
    // existing fields unchanged…
    #[serde(default)]
    pub group_state: Option<MlsGroupStateLabel>,   // NEW
}

/// Wire-safe stringly projection of `mls::state::GroupState`.
/// The internal enum carries non-serializable handles; this
/// label is the wire-safe view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum MlsGroupStateLabel {
    Active,
    PendingJoin,
    PendingCommit,
    CatchingUp,
    Removed,
    Corrupt,
}
```

`group_state` defaults to `None`; old encoded summaries decode
cleanly. UI uses `Some(Removed) | Some(Corrupt)` to disable the
composer; `None` is treated as `Active` for backward decode.

### Cursor semantics

`next_before_id` is computed as `records.last().map(|r| r.row_id)`
when `records.len() == limit`, else `None`. Pagination terminates
on the first page that returned fewer than `limit` rows, OR on a
page that returned `limit` rows but where the next `recent_before`
call would return `[]`. (UI handles both equivalently — when an
empty page lands, `next_before_id` is set to `None`.)

## §2 — UI components + stores

### New components (`crates/ui/src-svelte/src/lib/components/`)

#### `Composer.svelte`

Props: `{ contact: PublicKey, disabled: boolean, disabledReason?: string }`.
Local state: `text: string`, `composing: boolean`.

Handlers:
- `keydown` — if `event.key === "Enter" && !event.shiftKey && !event.isComposing && !composing && text.trim().length > 0`: `event.preventDefault()`, call `conversation.send(text)`, clear `text`.
- `paste` — `event.preventDefault()`, insert `event.clipboardData.getData("text/plain")` at cursor.
- `compositionstart` / `compositionend` — toggle `composing`.

Emits no events; mutates the conversation store directly.

#### `DeliveryIcon.svelte`

Props: `{ status: "pending" | "sent" | "delivered" | "failed", title?: string }`.
Renders one of 4 inline-SVG Lucide glyphs at 14×14 px, coloured
via the appropriate token. The "sent" mode is the visual mapping
of `DeliveryStatus::Deposited`; "delivered" maps `Delivered`; the
caller maps `DeliveryStatus` → icon-status string.

Token mapping:

| Icon status | Glyph | Colour |
|---|---|---|
| `pending` | `clock` | `--text-muted` |
| `sent` | `check` | `--text-muted` |
| `delivered` | `check-check` | `--accent` |
| `failed` | `alert-triangle` | `--danger` |

Tooltip via the native `title=` attribute. No popover for 2.D.

#### `UnreadSeparator.svelte`

A single `<hr>` with a centered "Unread" label. Rendered once
inline in `VirtualMessageList` between the row whose `row_id ==
unreadAnchorRowId` and the next row. Pure CSS; no props.

#### `SkeletonBubble.svelte`

Empty grey rounded rect at typical-bubble height (~72 px). Pure
CSS; no props. Used 5× at the top of the list during pagination
loads. Animated via a CSS keyframe (`opacity` pulse, no JS).

### Modified components

#### `MessageBubble.svelte`

Outgoing variant adds a `<DeliveryIcon>` to the right of the
timestamp. Status looked up from the new `delivery` store keyed
by `record.message_id` (Hex16, hex-stringified).

#### `VirtualMessageList.svelte`

Gains:
1. **Top-of-list `IntersectionObserver`** on the topmost
   virtualised row — fires `conversation.loadOlder()` when
   `next_before_id != null && !loadingOlder`. Debounced 250 ms
   to absorb scroll jitter.
2. **Bottom-of-list `IntersectionObserver`** on the bottommost
   virtualised row — fires `conversation.markReadIfAtBottom()`.
   Debounced 500 ms.
3. **Inline rendering of `<UnreadSeparator>` and
   `<SkeletonBubble>`** as zero-data rows participating in
   virtualization (stable keys: `"unread-separator"`, `"skel-N"`).

### New stores (`crates/ui/src-svelte/src/lib/stores/`)

#### `conversation.ts` (extended)

```ts
type OptimisticMessage = MessageRecord & {
  __tempId: string;
  __optimistic: true;
  __failed?: string;
};

interface ConversationState {
  contact: PublicKey | null;
  messages: (MessageRecord | OptimisticMessage)[];
  nextBeforeId: bigint | null;     // ts-rs emits i64 as bigint
  loadingOlder: boolean;
  unreadAnchorRowId: bigint | null;  // frozen at openConversation
  readCursor: bigint;              // last MarkRead emitted
}
```

Methods:
- `openConversation(contact)` — fetches `MessagesPage` with `paged: true, before_id: null, limit: 50`; sets `nextBeforeId`; reads `unreadAnchorRowId` from `ContactSummary.last_read_row_id` (see §3.4); emits `MarkRead { up_to: max(records.row_id) }`.
- `loadOlder()` — guards on `loadingOlder || nextBeforeId == null`; sets `loadingOlder = true`; fetches with `before_id: nextBeforeId, paged: true`; prepends records; updates `nextBeforeId`.
- `send(text)` — generates `tempId = crypto.randomUUID()`; appends `OptimisticMessage`; calls `await ipcClient.request({ cmd: "send_message", contact, kind: { kind: "text", body: text } })`; on reply calls `reconcile(tempId, record)`; on error calls `markFailed(tempId, reason)`.
- `appendOptimistic(record)` — internal; pushes the temp record.
- `reconcile(tempId, record)` — finds optimistic by `__tempId`, replaces with the canonical wire record (preserving array index).
- `markFailed(tempId, reason)` — flips `__failed: reason`; bubble's `DeliveryIcon` renders `failed`.
- `markReadIfAtBottom(rowId)` — debounced 500 ms; emits IPC `MarkRead` if `rowId > readCursor` AND the bottom-of-list `IntersectionObserver` reports the bottom is in view (proxy for "user is at the bottom"). For `MessageReceived` live-arrival, the explicit proximity check is `(scrollEl.scrollHeight - scrollEl.scrollTop - scrollEl.clientHeight) < 100`. Updates `readCursor` on success.

#### `delivery.ts` (NEW)

```ts
const map = writable<Map<string, DeliveryStatus>>(new Map());
```

Subscribes to `Event::DeliveryStatusChanged` via `EventFilter::Delivery`. On event: `map.update(m => m.set(message_id_hex, status))`. `MessageBubble.svelte` reads via `$derived` keyed by `record.message_id`.

### Send flow end-to-end

1. User types text + Enter in `Composer`.
2. `Composer` calls `conversation.send(text)`.
3. Store generates `tempId`; appends `OptimisticMessage` with `direction: outgoing`, `kind: { kind: "text", body: text }`, `ts_envelope: Date.now() * 1000`, `row_id: -1n`, `mls_generation: 0n`. `delivery` store: `map.set(tempId, "pending")`.
4. Store calls `ipcClient.request({ cmd: "send_message", contact, kind })`.
5. On reply `MessageSent { message_id, status, record: Some(rec) }`: store calls `reconcile(tempId, rec)`. `delivery.set(rec.message_id_hex, status_to_delivery_status(status))`.
6. On reply with `record: None` (idempotent retry): store flips `__optimistic: false` (no failure flag); UI keeps the placeholder visually, treats it as canonical.
7. Subsequent `Event::DeliveryStatusChanged { message, status }`: `delivery.set(message_hex, status)`; bubble re-renders icon.
8. On IPC error: `markFailed(tempId, err.message)`; icon → triangle.

`status_to_delivery_status(SendStatus)` mapping:
- `SendStatus::Queued` → `DeliveryStatus::Queued` (icon: clock — pending)
- `SendStatus::Delivered` → `DeliveryStatus::Delivered` (icon: check-check)

`DeliveryStatus` → icon-status string mapping:
- `Queued` → `"pending"`
- `Delivered` → `"delivered"`
- `Deposited` → `"sent"`
- `Failed(_)` → `"failed"`

## §3 — Daemon side

### §3.1 Send-path record projection

`crates/core/src/daemon/dispatch.rs::send_message` —
capture `row_id` from `insert_in_tx` (currently discarded with
`let _ =`), project a `MessageRecord`, attach to result:

```rust
let row_id = handle.pool.transaction(|tx| {
    group.save_in_tx(&group_repo, tx)?;
    let row_id = msg_repo.insert_in_tx(tx, InsertParams { /* … */ })?;
    let _ = outbox_repo.insert_in_tx(tx, &contact.0, &message_id.0, &ciphertext, 0)?;
    Ok(row_id)
})?;

let record = MessageRecord::project(
    row_id,
    &envelope,
    contact,
    mls_generation,
    ts_daemon_recv,
    Direction::Outgoing,
);

// after hub.send + ACK wait:
Ok(CommandResult::MessageSent {
    message_id: Hex16::from(message_id.0),
    status,
    record: Some(record),
})
```

The duplicate-retry branch (`StorageErrorKind::DuplicateMessage`,
existing dispatch.rs:352) returns `record: None` — the original
`row_id` is not easily recoverable in that path, and the UI's
optimistic placeholder is already on screen. UI flips
`__optimistic: false` on `record: None` with `status: Delivered`.

### §3.2 Pagination dispatch

```rust
async fn recent_messages<S>(
    handle: &Arc<DaemonHandle<S>>,
    contact: Option<PublicKey>,
    limit: u32,
    before_id: Option<i64>,
    paged: bool,
) -> Result<CommandResult, IpcError> {
    // existing group resolution unchanged…
    let rows = match before_id {
        Some(b) => msg_repo.recent_before(&group_id, b, limit as usize)?,
        None => msg_repo.recent(&group_id, limit as usize)?,
    };
    let records: Vec<MessageRecord> = /* existing projection loop */;

    if paged {
        let next_before_id = if records.len() == limit as usize {
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

### §3.3 Storage — `MessageRepo::recent_before`

New sibling method to `recent`:

```rust
pub fn recent_before(
    &self,
    group_id: &[u8],
    before_id: i64,
    limit: usize,
) -> Result<Vec<StoredMessage>> {
    self.pool.with(|c| {
        let mut stmt = c.prepare(
            "SELECT id, group_id, sender, kind, body_blob, ts, delivered_at, \
                    mls_generation, ts_daemon_recv \
             FROM messages \
             WHERE group_id = ?1 AND id < ?2 \
             ORDER BY mls_generation DESC, id DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![group_id, before_id, i64::try_from(limit).unwrap_or(i64::MAX)],
            /* same row mapping as recent */,
        )?;
        // collect…
    })
}
```

No new index — existing `messages.group_id` covers the predicate;
`messages.id` (PK) covers the order + cursor.

### §3.4 Read cursor surface

The UI needs the per-contact stored read cursor at open time to
compute `unreadAnchorRowId`. Two options:

- **Option A (chosen):** add the cursor to `ContactSummary` as
  `last_read_row_id: Option<i64>` (`#[serde(default)]`). Surfaced
  alongside `unread_count` in `ListContacts` results — no new
  command, one extra column join in `ContactRepo::list_summaries`.
- Option B: new `Command::GetReadCursor`. Rejected — adds wire
  surface for what is already a near-free addition to an existing
  query.

```rust
pub struct ContactSummary {
    // existing fields…
    #[serde(default)]
    pub group_state: Option<MlsGroupStateLabel>,
    #[serde(default)]
    pub last_read_row_id: Option<i64>,    // NEW
}
```

`unreadAnchorRowId = last_read_row_id` at open; if `None`
(fresh contact, no cursor), no separator is rendered.

### §3.5 No new events

`Event::DeliveryStatusChanged` is already filterable via
`EventFilter::Delivery` (Phase 2.B). UI subscribes with that
filter.

### §3.6 `ContactSummary.group_state` source

`ContactRepo::list_summaries` is extended to load each group blob
via `MlsGroupRepo::load` and project its `mls::state::GroupState`
to `MlsGroupStateLabel`. Cost: one extra SQL fetch per contact in
the listing path. For 2-member groups this is ≤32 KiB per blob;
for the contact list sizes 2.D needs (single-digit contacts) the
overhead is irrelevant. If list_summaries becomes hot in later
phases, the label can be denormalised onto the `contacts` row.

## §4 — Error handling, edge cases, disabled states

### §4.1 Composer disabled states

| Trigger | Composer behavior | Detection source |
|---|---|---|
| Daemon down (IPC connection lost) | Disabled; placeholder "Daemon not running" | 2.C IPC adapter connection state |
| MLS `group_state == Removed \| Corrupt` | Disabled; placeholder "Conversation unavailable" | `ContactSummary.group_state` |
| Contact removed (Phase 2.E soft-delete) | Out of scope for 2.D | — |

### §4.2 Optimistic send failure paths

| Failure | Bubble rendering | Recovery |
|---|---|---|
| IPC reply `IpcError::Daemon(GroupCorrupt)` | Triangle, tooltip "Group unavailable" | Composer re-disables on next `ContactSummary` refresh |
| Other IPC `Err(_)` | Triangle, tooltip = error message | None in 2.D — retry affordance lands in 2.E |
| IPC connection drops mid-send | Triangle, tooltip "Daemon disconnected" | 2.C-supplied reconnect logic |
| `MessageSent { record: None, status: Delivered }` (idempotent retry) | Optimistic kept; `__optimistic: false`, no failure flag | Treated as success; the previous attempt's row is canonical |

### §4.3 Pagination edge cases

| Case | Behavior |
|---|---|
| `next_before_id == null` returned | `nextBeforeId = null`; topmost-bubble observer no longer triggers loads |
| User scrolls to top while page in flight | Skeleton bubbles render; `loadingOlder` guard prevents parallel fetches |
| Empty page returned (`records.len() == 0`) | Same as `next_before_id == null` |
| `before_id` references a pruned row | Query returns rows older than the (orphan) cursor; pagination terminates cleanly |

### §4.4 Mark-read edge cases

| Case | Behavior |
|---|---|
| Empty conversation opened | `MarkRead` skipped (no `up_to` candidate) |
| New `MessageReceived` while still loading first page | Live message goes through `appendMessage`; mark-read trigger re-checks `scrollEl` at append time |
| `Command::MarkRead` IPC fails | Logged, swallowed — non-critical; next open retries |
| Multiple bursts within debounce window | Coalesced; final `up_to` is the highest `row_id` seen |

### §4.5 IME edge cases

| Case | Behavior |
|---|---|
| Enter mid-IME composition | No-op (gated by `isComposing`) |
| Shift+Enter mid-composition | Inserts `\n`; composition continues |
| Cmd/Ctrl+Enter | Same as Enter (gated identically) — standard chat-app convention |
| Enter on empty/whitespace input | No-op |

### §4.6 Unread separator edge cases

| Case | Behavior |
|---|---|
| Zero unread on open | No separator (`unreadAnchorRowId == null`) |
| Open → read → close → reopen | Separator re-anchors to new (larger) cursor; previously-unread messages lose their separator |
| Stored cursor exceeds any existing row (pruned) | `unreadAnchorRowId = null` defensively; no separator |

## §5 — Testing strategy

### §5.1 Rust unit + integration

| Test | Asserts |
|---|---|
| `daemon::dispatch::tests::send_message_returns_record` | `MessageSent.record` carries the just-inserted `row_id`, `Direction::Outgoing`, the envelope's `kind`, post-encrypt `mls_generation` |
| `daemon::dispatch::tests::send_message_duplicate_returns_record_none` | Idempotent-retry branch: `record: None`, `status: Delivered` |
| `daemon::dispatch::tests::recent_messages_paged_first_page` | `paged: true, before_id: None`, len == limit → `next_before_id == Some(oldest_row_id)` |
| `daemon::dispatch::tests::recent_messages_paged_last_page` | len < limit → `next_before_id == None` |
| `daemon::dispatch::tests::recent_messages_unpaged_unchanged` | `paged: false` (or omitted) → `Messages(Vec)` — CLI backward compat |
| `daemon::dispatch::tests::recent_messages_before_id_excludes_cursor` | Cursor row itself NOT in result |
| `daemon::dispatch::tests::list_contacts_carries_group_state` | `ContactSummary.group_state == Some(Active)` for a normal 2-member group |
| `daemon::dispatch::tests::list_contacts_carries_last_read_row_id` | After `MarkRead { up_to: 7 }`, summary reports `last_read_row_id: Some(7)` |
| `storage::messages::tests::recent_before_orders_correctly` | 200 inserted; `recent_before(g, 100, 50)` returns ids 99..50 in correct DESC order |
| `storage::messages::tests::recent_before_with_pruned_cursor` | `before_id` pointing at pruned row returns rows older than that id, no panic |
| `cli_two_daemons` (existing) | Updated: assert `MessageSent.record.is_some()` after send |
| **NEW** `crates/tests/src/ui_send_roundtrip.rs` (`#[ignore]`-gated, real-Tor) | Two paired daemons over loopback; `MessageSent.record.row_id == DB[A].max_id`; `MessageReceived.record.row_id == DB[B].max_id` |

### §5.2 TypeScript unit (Vitest)

| Test | Asserts |
|---|---|
| `Composer.test.ts: enter_sends_when_not_composing` | Mock IPC; type "hi"; Enter → `ipc.request` called with `{cmd: "send_message", kind: {kind: "text", body: "hi"}}` |
| `Composer.test.ts: enter_during_ime_composition_no_op` | Dispatch `compositionstart`, type, Enter with `isComposing: true` → no IPC |
| `Composer.test.ts: shift_enter_inserts_newline` | "hi"+Shift+Enter+"world" → textarea is `"hi\nworld"`, no IPC |
| `Composer.test.ts: paste_strips_html` | `getData("text/html")` returning `<b>bold</b>`; `getData("text/plain")` returning `"bold"` → textarea has `"bold"` |
| `Composer.test.ts: empty_enter_no_op` | Whitespace-only + Enter → no IPC |
| `Composer.test.ts: disabled_states` | Three matrix cases (daemon-down, MLS-Removed, MLS-Corrupt) → composer disabled with correct placeholder |
| `conversation.test.ts: append_optimistic_then_reconcile` | `appendOptimistic` + `reconcile(tempId, record)` → array index preserved, canonical record present |
| `conversation.test.ts: load_older_prepends_and_updates_cursor` | Mock returning `MessagesPage { records, next_before_id: 50n }` → `nextBeforeId === 50n`, records prepended chronologically |
| `conversation.test.ts: load_older_idempotent_under_concurrent_calls` | Two `loadOlder()` calls in flight → only one IPC fires |
| `conversation.test.ts: mark_read_debounce` | 5 `markReadIfAtBottom` within 500 ms → 1 IPC with highest `row_id` |
| `conversation.test.ts: unread_anchor_frozen_at_open` | Open with cursor=10; receive 5 messages while open → `unreadAnchorRowId === 10` (unchanged) |
| `delivery.test.ts: status_map_updates_from_event` | Dispatch `DeliveryStatusChanged { status: Deposited }` → store map updates |
| `DeliveryIcon.test.ts: glyph_for_each_state` | Snapshot the 4 rendered SVGs (clock / check / check-check / alert-triangle) |

### §5.3 Playwright e2e

| Spec | Flow |
|---|---|
| `composer.e2e.ts` | Open existing conversation → type "hello" → Enter → optimistic bubble + clock icon → mock advance to `Delivered` → check-check icon within 50 ms |
| `pagination.e2e.ts` | Open conversation with 200 mocked messages → 50 visible → scroll to top → skeletons render → mock returns next page → 100 in DOM → continue until `next_before_id == null` → no further fetches |

### §5.4 Lint / spec-compliance

| Test | Asserts |
|---|---|
| `crates/core/tests/wire_format_append_only.rs` | Snapshot of `Command` and `CommandResult` JSON-Schema (or sorted variant list); failing diff signals an unintended wire change |
| `crates/ui/src-svelte/tests/lint_no_remote_assets.test.ts` (existing in 2.C) | Extended to scan new components for inline-SVG only (no `<img src>` for icons) |

## Risks + mitigations

| Risk | Mitigation |
|---|---|
| `paged: bool` discriminator feels hacky | Lock it explicitly in this spec; if it grates in 2.E, deprecate `Messages(Vec)` in a follow-up phase per the kickoff's "wire-format BREAKING changes need a separate spec" rule |
| Optimistic reconciliation ordering bug under concurrent sends | `reconcile` looks up by `__tempId` (UUID), not by array index — multiple in-flight sends are safe; explicit test in §5.2 |
| Skeleton bubbles cause virtualizer height drift | Skeletons participate in virtualization with stable keys + the same estimated row height; the virtualizer self-corrects after measurement |
| `IntersectionObserver` doesn't fire reliably under `@tanstack/svelte-virtual` | The lib renders absolute-positioned children inside a sized container — observers on the bound DOM nodes fire on scroll. Confirmed in 2.C; if regressions surface, fall back to scroll-event handlers gated on `scrollTop` thresholds |
| `group_state` projection requires loading the group blob — slow for many contacts | Acceptable for 2.D's contact count (single digits); denormalise onto `contacts` row in a later phase if list_summaries becomes hot |
| MLS `Removed` / `Corrupt` rare in 2.D testing | E2e mock spec covers both; Rust integration test provides a third path |

## Out of scope (deferred)

- Invite link generation + scan / paste UX (2.E)
- Contact rename / remove with soft-delete (2.E)
- Settings panel (2.F)
- Mailbox CRUD UI (2.F)
- Notification system (2.F)
- Tray + minimize-to-tray (2.F)
- Packaging / installers (2.G)
- Multi-member groups (Phase 3)
- Avatars / reactions / replies / edits / typing indicators (Phase 3)
- Attachments / file send (Phase 3)
- Phase 2.B follow-ups (Tasks 20.5 / 22.5 / 23.5)
- Wire-format BREAKING changes — including any decision to deprecate `Messages(Vec)` or remove `paged: bool` — require a separate spec

## Files touched (preview, exhaustive in plan)

**Rust (`crates/core/`):**
- `daemon/commands.rs` — extend `Command::RecentMessages`, `CommandResult::MessageSent`, add `MessagesPage`, extend `ContactSummary`, add `MlsGroupStateLabel`
- `daemon/dispatch.rs::send_message` — capture `row_id`, project record
- `daemon/dispatch.rs::recent_messages` — accept `before_id`, `paged`; branch on `paged`
- `daemon/dispatch.rs::list_contacts` — populate `group_state` and `last_read_row_id`
- `storage/messages.rs` — add `recent_before`
- `daemon/events.rs` — unchanged (no new events)
- `tests/wire_format_append_only.rs` — NEW snapshot test
- `tests/recent_before_paging.rs` — NEW (or extend existing)

**Rust (`crates/cli/`):**
- `main.rs` — no behavioural changes; `MessageSent.record` ignored. (Field defaulting in serde keeps decode green.)

**Rust (`crates/tests/`):**
- `src/ui_send_roundtrip.rs` — NEW (`#[ignore]`-gated)
- `src/cli_two_daemons.rs` — assert `record.is_some()`

**TypeScript (`crates/ui/src-svelte/src/`):**
- `lib/components/Composer.svelte` — NEW
- `lib/components/DeliveryIcon.svelte` — NEW
- `lib/components/UnreadSeparator.svelte` — NEW
- `lib/components/SkeletonBubble.svelte` — NEW
- `lib/components/MessageBubble.svelte` — add `<DeliveryIcon>` slot
- `lib/components/VirtualMessageList.svelte` — add observers, separator, skeletons
- `lib/stores/conversation.ts` — extend with optimistic + pagination + mark-read
- `lib/stores/delivery.ts` — NEW
- `lib/icons/{clock,check,check-check,alert-triangle}.svg` — NEW (Lucide MIT)
- `lib/styles/tokens.css` — add `--danger`
- `routes/+page.svelte` — wire `Composer` into the conversation pane

**Tests:**
- `crates/ui/src-svelte/src/lib/components/*.test.ts` — Vitest specs per §5.2
- `crates/ui/src-svelte/e2e/composer.e2e.ts` — NEW
- `crates/ui/src-svelte/e2e/pagination.e2e.ts` — NEW
