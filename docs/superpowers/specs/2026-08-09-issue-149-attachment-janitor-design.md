# #149 (first slice) — attachment janitor: auto-fail + orphan sweep (design)

**Issue:** #149 (`enhancement`, `attachments`, milestone v1.1) — the deferred
half of #144, and the long-standing 3.B/3.C "no auto-fail/partial-GC janitor"
deferral.
**Branch:** `149-attachment-janitor`
**Relates to:** #144/#146 (retry), #38 (exactly-once terminal claim), #142
(never let a write fail silently).

**No migration. No new config keys. No wire-format change.** One new `Event`
emission (an existing variant), one signature change to
`daemon::retention::spawn_sweep`.

---

## 1. Scope — and what this slice deliberately does *not* do

#149 names three classes of unreclaimed chunks:

1. `'failed'` inbound rows never retried,
2. stalled `'pending'` inbound rows,
3. true orphans — chunk directories with no `attachments` row.

**This slice ships class 3 reclamation plus auto-fail for class 2. It does not
delete chunks for classes 1 or 2.**

That is deliberate, and it means **#149 must stay open after this lands.** The
chunks held by a `'failed'` row are the resume state that makes #146's retry
cheap; deleting them is an age-policy decision that needs a durable
"failed-at" signal this codebase does not yet have (see §3). Auto-fail is
shipped because the issue itself calls it "arguably the more valuable half":
it converts a silently-stuck transfer into a visible, retryable one.

**Net disk reclaimed by this slice comes only from orphan sweeping.** Anyone
reading the issue title should not expect otherwise.

---

## 2. The two constraints that shape everything

**A completed inbound attachment's chunks *are* the user's file.** Since the
encrypted-at-rest rework, `finalize_rx` / `finalize_offline` retain chunks and
never write plaintext; `open_attachment_cmd` / `save_attachment_cmd` decrypt on
demand. A janitor that treats retained chunks as garbage deletes received
files. `status='complete'` is strictly out of scope for both mechanisms, as a
tested invariant rather than an implicit one.

**Reclaiming a `'failed'` row's chunks destroys retry's resume state.** Any
grace period must not race a user about to press Retry. This slice sidesteps
the race entirely by not deleting those chunks at all.

---

## 3. The activity signal: chunk-directory mtime

Auto-fail needs to know when a transfer last made progress. The schema cannot
answer that:

- `attachments` has `created_at` and nothing else — no `updated_at`.
- `attachment_chunks` has `received` (0/1) and no timestamp.
- **`rearm_failed_in` sets `status='pending'` and does not touch `created_at`**
  (`storage/attachments.rs:213`). So a three-week-old attachment retried today
  keeps its old `created_at`; any age policy keyed on `created_at` would
  immediately re-fail the retry. That is the retry race, merely relocated.

**Signal used instead: the mtime of `<data_dir>/attachments/<hex>/`.**

`ChunkStore::put` (`attachment/store.rs:28`) does `create_dir_all` → write
`<index>.part` → rename to `<index>`. The rename modifies the directory, so its
mtime *is* "when a chunk last landed", maintained by the filesystem. It costs no
migration and no write on the per-chunk hot path.

It is retry-safe by construction: a resumed transfer writes chunks, which bumps
mtime, which makes the row not-stalled.

**A useful coincidence:** an attachment with zero chunks has no directory — and
those are exactly the rows with no disk to reclaim. The signal covers precisely
the population this issue cares about.

**Known limits, accepted:** mtime is a heuristic, not a guarantee. A restore or
`cp -r` may reset mtimes to now, and some filesystems have coarse or unusual
semantics. This is safe *because the signal is only ever used to delay
deletion*: a fresh mtime means "leave it alone". A reset clock makes the janitor
wait longer, never act sooner. If a real case ever shows the heuristic
insufficient, the fallback is an explicit `updated_at` column — deliberately not
built now (YAGNI).

---

## 4. Mechanism A — auto-fail stalled inbound transfers

Per tick, for each `direction='in'` row with `status='pending'`:

```
chunk dir exists  AND  now - dir.mtime > STALL_GRACE  →  claim_terminal(Failed)
```

- **Transitions only via `claim_terminal`**, preserving #38's exactly-once
  terminal gate. A row that another lane terminalises concurrently is a no-op
  here, by the same CAS.
- **Chunks are retained.** The row becomes visible in the UI as failed and
  retryable; #146's `rearm_failed_in` + `requeue_attachment` resume from the
  chunks already held.
- **Rows with no chunk directory are skipped** — nothing to reclaim, and no
  mtime to reason about. (A 0-chunk pending row costs no disk; leaving it is
  strictly safer than inventing a signal for it.)

**`STALL_GRACE = 14 days.`** Justification: chunk deposits are made with
`ttl=0`, which the mailbox resolves to `Policy::default_ttl_secs` = 604 800 s =
**7 days** (`crates/mailbox/src/policy.rs:77`; operator max is 30 days). An
offline transfer can therefore legitimately sit in flight for a week waiting for
the receiver to poll. 14 days is 2× that default, so a transfer waiting on
mailbox delivery is never auto-failed before its deposits have certainly
expired. A mailbox configured to the 30-day maximum could still exceed the
grace; that is acceptable — the outcome is a *retryable* failed row, not data
loss.

---

## 5. Mechanism B — orphan sweep

Per tick, list directories under `<data_dir>/attachments/`. For each entry whose
name parses as a 32-char hex attachment id:

```
no attachments row (ANY status)  AND  now - dir.mtime > ORPHAN_GRACE
  →  remove_dir_all
```

- **`ORPHAN_GRACE = 1 hour.`** `ChunkStore::put` calls `create_dir_all` before
  the row is guaranteed visible to another connection; the grace closes that
  window rather than relying on write ordering. One hour is far beyond any
  plausible insert latency and costs nothing (orphans are, by definition, not
  urgent).
- **A directory whose name does not parse as a hex id is left alone**, not
  deleted. The janitor only removes what it positively recognises.
- Requires one new repo method: `AttachmentRepo::all_ids() -> Result<Vec<[u8;
  16]>>` (no such enumerator exists today).

---

## 6. Wiring

`daemon::retention::spawn_sweep` gains the janitor as a third step in the
existing hourly `tokio::select!` tick, beside message pruning and
outstanding-invite expiry. It already re-reads config per tick and is the
module that owns scheduled cleanup.

**Signature change:** `spawn_sweep` gains `events_tx:
broadcast::Sender<Event>` and a `data_dir: PathBuf` (the janitor needs the
chunk-store root; the existing steps are DB-only). Auto-fail emits
`Event::AttachmentFailed { attachment_id, reason }` so the UI reflects the
status change live rather than only on next refresh.

Call sites to update: `daemon/state.rs` ×4, `retention.rs` tests ×3,
`crates/tests/src/history_sweep.rs` ×1, plus the `test_exports` re-export in
`lib.rs`.

**Reason string:** `"transfer stalled"` — human-readable and non-sensitive, per
the `Event::AttachmentFailed` doc contract. No filename, no peer identity.

---

## 7. Observability

Per #142's lesson, nothing here fails silently:

- `info!` when either mechanism reclaims or transitions anything, with counts
  (`stalled = N`, `orphans = N`) — not per item, to keep an hourly tick quiet.
- `warn!` on any error, and the sweep continues to the next item rather than
  aborting the tick. A single unreadable directory must not stop the janitor.
- Fields limited to counts and `attachment_id` (hex). Never filenames.

---

## 8. Testing

The invariants are the point; each gets a test.

1. **`status='complete'` is never touched by auto-fail** — a complete inbound
   row with an ancient mtime stays complete.
2. **`status='complete'` is never touched by orphan sweep** — its directory has
   a row, so it is not an orphan; assert the directory survives.
3. **A stalled pending row past the grace is failed**, and its chunks survive.
4. **A pending row inside the grace is untouched** (both status and chunks).
5. **A retried attachment is not immediately re-failed** — old `created_at`,
   fresh mtime → not stalled. This is the regression guard for the race in §3.
6. **Orphan sweep deletes a directory with no row** past `ORPHAN_GRACE`.
7. **Orphan sweep spares a directory with a row in any status** (`pending`,
   `failed`, `complete`).
8. **Orphan sweep spares a fresh directory** inside `ORPHAN_GRACE`.
9. **A non-hex directory name is left alone.**

Tests set directory mtimes explicitly rather than sleeping, using
`std::fs::File::set_times` with `std::fs::FileTimes` (stable since Rust 1.75;
the pinned toolchain is 1.95). **No new dependency** — `filetime` is
deliberately not added for this. The existing retention tests already inject a
short `tick`, so the sweep itself needs no clock injection.

**Gate:** `cargo fmt --all -- --check`, `cargo clippy --workspace --exclude
skattr-ui --all-targets --features test-harness -- -D warnings`, `cargo test`,
`cargo deny check`. CI additionally runs the UI job on the PR.

---

## 9. Acceptance criteria (mapped to #149)

| #149 acceptance | This slice |
|---|---|
| Chunks for transfers that will never complete are eventually reclaimed under a documented policy | **Partial** — class 3 (orphans) only. Classes 1/2 are documented as retained; #149 stays open. |
| A test proves `status='complete'` is never touched | ✅ tests 1 and 2 |
| A test proves a recently-failed attachment inside the grace is still retryable | ✅ test 5 (the retry race), plus chunks-survive assertions in 3 |
| Whatever is not reclaimed by design is stated in the v1.0 limitations | ✅ CLAUDE.md limitation updated to say auto-fail exists and that failed/pending chunks are still retained pending the age-GC half |

---

## 10. Deliberately excluded (YAGNI)

- **Age-based GC of `'failed'` / stalled `'pending'` chunks** — needs a durable
  failed-at signal and a retention policy; the actual remaining work on #149.
- **An `updated_at` column** — not needed by anything in this slice (§3).
- **Config keys for the two grace constants** — hardcoded with rationale,
  matching the `MAX_WELCOME_AGE_MS` precedent. Add config when someone needs to
  tune it, not before.
- **Auto-fail for rows with no chunk directory** — no disk to reclaim, and it
  would require inventing a signal that reintroduces the retry race.
