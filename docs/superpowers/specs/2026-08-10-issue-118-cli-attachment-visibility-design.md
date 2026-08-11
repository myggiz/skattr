# #118 — CLI attachment visibility + save (design)

**Issue:** #118 (`enhancement`, `attachments`, milestone v0.1.2)
**Branch:** `118-cli-save-attachment`
**Relates to:** #76 (this blindness is what made the #76 diagnosis cost two machines and disk forensics), #114 (observability), #116/#117 (the IPC reconnect contract this must obey), #54 (path validation — deliberately *not* fixed here).

**Purely CLI-side.** No new daemon command, no new IPC surface, no wire-format change, no migration. All four attachment commands already exist and are dispatched.

---

## 1. Problem

The `skattr` CLI can send a file (`send-file`) but has no way to see or retrieve one it received.

Received attachments are **encrypted at rest** in the `ChunkStore`; plaintext is produced only via the daemon's `SaveAttachment` / `OpenAttachment`. The Tauri UI wires these; the CLI exposes nothing equivalent. So a fully successful transfer is invisible from the CLI.

That is not a hypothetical. During the #115 two-machine real-Tor test we read an empty `download_dir` as "the transfer failed" for hours, when in fact it had completed and the chunks were staged exactly as designed. The CLI gave us no way to look.

Worse, the CLI actively misrenders what it *does* have. Both `tail` and `export` fall through to:

```rust
let body = match &row.kind {
    Kind::Text { body } => body.clone(),
    other => format!("({other:?})"),
};
```

`Kind::File` carries a single field — the CBOR manifest — and **no filename** (`envelope/kinds.rs:33`). So a received file renders as the `Debug` of a byte array: hundreds of integers, no name, no size, no id. The information needed to act on the attachment is present in that blob and unreadable.

---

## 2. Scope

**In:** decode-and-render `Kind::File` in `tail`, `tail --follow` and `export`; a `save-attachment` subcommand.

**Out, deliberately:**
- No `attachments` list subcommand — the tail rendering makes ids discoverable, which is what the acceptance needs.
- No `retry-attachment` verb (`Command::RetryAttachment` exists and stays unused by the CLI for now).
- No `open-attachment` — launching a system opener is a GUI concern; `save` is the CLI primitive.
- No new daemon command. Listing and availability are composed from commands that already exist.
- **No path validation** — #54 owns that for both `SetConfig download_dir` and `SaveAttachment dest`. This spec absolutizes the path (§4.2) but does not validate it, and must not be read as closing #54.

---

## 3. Part A — render `Kind::File` properly

### 3.1 What is rendered

```
[12:05] <- alice  📎 logs.tar.gz  2.4 MiB  id=78890935b23ec125  available
```

- **filename** and **total_size** come from decoding the manifest — they exist nowhere else on the record.
- **id** is the first 16 hex chars (8 bytes) of `attachment_id`. Long enough to be unambiguous in practice, short enough to read and retype; `save-attachment` accepts any unique prefix (§4.1), so a truncated display is still directly usable.
- **availability** — see §3.3.

`AttachmentManifest` is publicly re-exported (`skattr_core::AttachmentManifest`) and `skattr-cli` already depends on `skattr-core`, so `from_cbor` needs no new plumbing. Fields used (`attachment_id`, `filename`, `total_size`) are all `pub`.

### 3.2 Decode failure is not fatal

A manifest that fails `from_cbor` renders as `📎 (unreadable manifest)` and the tail continues. One corrupt or future-version row must not abort a listing — the point of this feature is visibility, and a hard failure here would restore exactly the blindness it removes.

### 3.3 Availability

Availability comes from `Command::AttachmentAvailable { attachment_id }`, issued **only for inbound rows**: `attachment_available_cmd` returns true iff the row is `direction='in', status='complete'`, so an outgoing row is definitionally unavailable and probing it would print a misleading `incomplete` next to a file the user themselves sent. Outgoing file rows render filename/size/id with no availability field.

Rendered states: `available` when the probe returns true, `incomplete` when false.

**One probe = one connection.** The daemon's IPC connection is single-request (#116), so each probe opens its own. On a local socket this is microseconds. In `--follow` it *must* be a separate connection regardless, because Execute-after-Subscribe hangs — the lesson already encoded in `chat`'s per-line send.

A probe that errors renders the row without an availability field rather than failing the tail (same reasoning as §3.2).

---

## 4. Part B — `skattr save-attachment <id> <dest>`

### 4.1 Id resolution accepts a unique prefix

Mirrors the existing `resolve_contact` (`cli/src/main.rs:994`), which lowercases and matches with `starts_with`, and already has tests for the unique / ambiguous / no-match cases.

The candidate set is built by fetching recent messages, decoding each `Kind::File` manifest, and matching the hex `attachment_id` against the supplied prefix. Errors mirror `resolve_contact`'s shape: no match, or ambiguous with the count of matches.

This is **resolve-then-act**: one connection to fetch messages, a second to save. It must use the reconnect pattern from #116/#117 — reusing the connection is exactly the bug that broke eight CLI commands.

### 4.2 The destination path is absolutized CLI-side

`Command::SaveAttachment.dest_path` is documented as absolute. The daemon's working directory is not the CLI's, so passing a relative path through would resolve against the wrong directory — silently writing somewhere the user did not mean.

The CLI therefore joins a relative `dest` onto its own current directory before sending. It does **not** validate the result: that is #54's job, and doing a partial version here would muddy which issue owns the guarantee.

### 4.3 Output and exit codes

- Success prints `saved <size> -> <path>` and exits 0. `SaveAttachment` returns `CommandResult::Ok`, so the size shown comes from the manifest already decoded during resolution.
- **Not available** prints `not available yet (transfer incomplete)` and exits **non-zero**, so a script can branch on it. This is the acceptance criterion "reports unavailable rather than erroring" — it is a clean diagnostic, not an error dump.
- Respects the existing **global `--json`** flag (`main.rs:40`), matching how `invite` / `add` / `contacts` already switch output.

---

## 5. Testing

**Unit (in `crates/cli`):**
1. Id-prefix resolver: unique prefix resolves; ambiguous prefix errors with the match count; no match errors. Mirrors the three existing `resolve_contact` tests.
2. File-row renderer: given a real CBOR-encoded manifest, the rendered line contains the filename, a human-readable size, and the id prefix.
3. Renderer with undecodable bytes produces the `(unreadable manifest)` form rather than panicking.

**Integration (in `crates/tests`):** the acceptance round-trip — a received, completed attachment is saved to a chosen path and the bytes are **byte-identical to the original**, verified by comparing the sha256 against the manifest. This belongs beside the existing attachment integration tests, which already stand up a real transfer.

**Gate:** `cargo fmt --all -- --check`, `cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings`, `cargo test`, `cargo deny check`. CI additionally runs the UI job on the PR.

---

## 6. Acceptance criteria (mapped to #118)

| #118 acceptance | Where |
|---|---|
| A received-and-complete attachment can be **listed with availability** | §3 — tail/export render filename, size, id, availability |
| …and **saved to a chosen path, byte-identical** (sha256 matches the manifest) | §4 + the integration test in §5 |
| A **not-yet-complete** attachment reports unavailable rather than erroring | §4.3 — clean message, non-zero exit |
| Resolve-then-act uses the reconnect pattern (#116/#117) | §4.1 |

---

## 7. Deliberately excluded (YAGNI)

- **An `attachments` list subcommand.** Tail rendering already makes ids discoverable; a second surface would need its own resolution, formatting and tests for the same information.
- **`retry-attachment`.** Worth having now that the janitor auto-fails stalled transfers (#149), but it is a separate verb with separate semantics — its own issue if wanted.
- **Verifying sha256 in the CLI on save.** Chunks are sha256-verified against the manifest on receipt, so the daemon cannot hand back bytes that fail that check. Re-verifying in the CLI would be theatre; the *test* verifies it, which is where the guarantee belongs.
- **Caching availability across rows.** Each row is probed independently. A tail of twenty messages with five attachments costs five extra local-socket round-trips — not worth a cache.
