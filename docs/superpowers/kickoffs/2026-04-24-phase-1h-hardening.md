# Phase 1.H — Hardening kickoff prompt

> **Usage:** Paste the fenced block below as the first message of a
> fresh Claude Code session. Keep the surrounding meta-text out of
> the paste — only the fenced block is the prompt itself.

---

```
We just merged Phase 1.G (message storage & search) to master at dedc206.
Before starting Phase 2 (Tauri 2 + SvelteKit UI) we want to close out
hardening items surfaced during 1.G reviews. Call this Phase 1.H.

Please start by invoking `superpowers:brainstorming` to explore scope
and pin priorities. Treat the list below as raw material, not a locked
scope — brainstorm which items are genuinely blocking a UI layer vs
which are follow-ups that can wait, and whether to bundle in any items
not on this list.

## Raw material — items surfaced during 1.G reviews

### Correctness (Important)

1. **`contact_for_record` on unscoped search emits local pubkey on
   outgoing rows** (dispatch.rs search_messages handler, Task 17 review).
   For `Command::SearchMessages { contact: None }` with an outgoing hit
   (sender == local identity), the projected `record.contact` is the
   local pubkey — the CLI renders "conversation with <self>". Fine on a
   plain-text CLI dump; will be visibly wrong on a UI showing a peer
   avatar. Fix: resolve peer by group_id via ContactRepo on each row,
   or add a `ContactRepo::contact_for_group(&[u8]) -> Option<PublicKey>`
   helper and call it per hit.

2. **No uniqueness constraint on `(group_id, envelope.id)` in the
   `messages` table** (Task 15 review). CLI `skattr send` retries after
   a 2s ACK timeout produce duplicate `messages` rows with the same
   envelope.id but different mls_generation + ts_daemon_recv. The
   0006_history_search.sql migration didn't add this; a new migration
   0007 should. Open question: does `envelope.id` need to be a persisted
   column first (currently only embedded in `body_blob`)?

3. **Durability gap in `send_message`**: `group.save` persists the
   advanced ratchet BEFORE `MessageRepo::insert`. If the insert fails
   (disk full, SQLITE_BUSY, crash), the ratchet advanced but no history
   row was written — the message is lost from local history AND
   outbound never enqueues. Options: wrap group_save + insert + outbox
   in one transaction, OR reorder insert-before-save (ratchet lives in
   memory until next load; insert failure leaves no durable state to
   reconcile). Same gap on the receive path in daemon/inbound.rs.

### Error taxonomy (Minor-to-Important)

4. **`IpcError::Internal(String)` for client validation errors** (Task
   19 review). `Command::PruneHistory` validation ("exactly one of
   before_ts_recv/keep_last") and `"keep_last requires a contact"`
   surface as Internal, which operators can't distinguish from
   daemon-side bugs. Add `DaemonErrorKind::InvalidArgument { message }`
   and thread it through; update CLI exit-code mapping.

5. **`CoreError::kind()` string-matching for FTS errors** (Task 11
   review). Works but brittle — a storage-layer error-string rename
   silently reverts to StorageError. Consider structured error
   sub-enums with `thiserror #[from]` rather than string matching.

### Hygiene (Minor)

6. **`now_unix_seconds()` duplicated across four sites** (Tasks 13-15
   reviews). Module-local in daemon/inbound.rs; inlined in
   daemon/dispatch.rs; copied three times in integration tests.
   Hoist to `daemon::clock::now_unix_seconds() -> i64` (pub(crate))
   + an analogue in `test_exports`.

7. **`MessageRecord::project`'s unused `_row_id`** (Task 12 review).
   Either drop the parameter entirely or surface it on `MessageRecord`
   for tracing correlation.

8. **`backfill_body_text` no transaction wrapping** (Task 10 review).
   N-row UPDATE loop auto-commits each row → N fsyncs. Wrap in a
   single `pool.transaction`. Cheap.

9. **`group_id: Vec<u8>` allocation on every ReceiveOutcome::New**
   (Task 13 review). Fixed-width 32-byte group IDs don't need a heap
   alloc on the hot path.

### CI / infra

10. **Wire cargo-deny into CI** (Task 34 exit criterion). The project
    has a `deny.toml`, now clean per 1.G's last commits, but no CI
    job invokes it.

11. **CLI socket-path env tests — Mutex is one option; `serial_test`
    crate is cleaner** (80ce7c7 follow-up). Low value.

## After brainstorming

Once scope is pinned, follow the normal flow:
- `superpowers:writing-plans` for the per-task plan.
- `superpowers:using-git-worktrees` to branch off master.
- `superpowers:subagent-driven-development` or `executing-plans` to
  implement.

CLAUDE.md is binding: GPLv3 headers, no unwrap/expect in non-test,
cargo fmt + clippy -D warnings + cargo test + cargo deny check clean
before shipping. Preserve the 2-member group invariant per CLAUDE.md —
multi-member lookup is Phase 2+.

Questions before you start brainstorming: Any Phase 2 UI requirement
that would reorder this list? Any items not here that you want to
fold in?
```
