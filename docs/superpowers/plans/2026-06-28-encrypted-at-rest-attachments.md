# Encrypted-at-rest Attachments (decrypt-on-demand) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Received attachments stay encrypted at rest by default; plaintext is produced only on explicit user Open (to a managed cache wiped on exit) or Save (to a user-chosen path).

**Architecture:** Both receive-side finalize lanes stop auto-reassembling to the download dir and instead retain the already-encrypted chunks. Three additive local IPC commands decrypt on demand from those chunks + the MLS-protected manifest. The UI surfaces Open/Save and rehydrates availability after restart.

**Tech Stack:** Rust (`skattr-core`, `skattr-ui` Tauri 2 shell), SvelteKit + ts-rs IPC types, `rusqlite`, `chacha20poly1305`/XChaCha20.

## Global Constraints

- **No ADR / no peer-facing wire change.** New IPC commands are additive, local (UI⟷daemon) only. ADR 0006 `Deposit` and the 3.B transport `FrameType`s are untouched.
- **No `unwrap()`/`expect()` in library code** — `?` + typed errors. (`#[cfg(test)]` exempt.)
- **Every `.rs` file carries its SPDX header** (GPL-3.0-or-later for `core`/`ui`).
- **All secret material zeroizes**; never copy a raw `[u8; 32]`/`file_key` onto the stack un-zeroed. (The reassembler/manifest already own this; do not regress it.)
- **Never log pubkeys, onions, filenames, or paths at info+**; redaction by default. New `warn!`/`error!` lines carry static text + error category only.
- **Cargo isn't on PATH** — prefix every cargo invocation with `. "$HOME/.cargo/env" &&`. Toolchain is pinned; if it floats, `rustup override set 1.95.0`.
- **Gate (must be green before any task is "done"):**
  - `. "$HOME/.cargo/env" && cargo fmt --all --check`
  - `. "$HOME/.cargo/env" && cargo clippy --all-targets -- -D warnings`
  - `. "$HOME/.cargo/env" && cargo test -p skattr-core -p skattr-ui --features skattr-core/test-harness`
  - `cd crates/ui/src-svelte && npx pnpm@10 check && npx pnpm@10 test` (svelte-check 0/0 + vitest)
- **Branch:** create `attachments-encrypted-at-rest` off the current `field-testing-fixes` HEAD before Task 1. Do not work on `master`.
- **ts-rs types regenerate on `cargo test`** (the `#[ts(export)]` test). After changing any `#[derive(ts_rs::TS)]` type, run the core tests so `crates/ui/src-svelte/src/lib/ipc/types/` updates, and commit the regenerated `.ts` files with the task.

---

## File Structure

**Core (`crates/core/src/`):**
- `daemon/events.rs` — `Event::AttachmentReceived` loses its `path` field. (modify)
- `delivery/peer.rs` — `finalize_rx` retains chunks, stops reassembling; `InboundDispatch::attachment_received` drops `path`; the in-test double + loopback assertions updated. (modify)
- `daemon/inbound.rs` — `finalize_offline` retains chunks, stops reassembling; emit site drops `path`. (modify)
- `daemon/commands.rs` — `Command::{OpenAttachment,SaveAttachment,AttachmentAvailable}` + `CommandResult::{AttachmentDecrypted,AttachmentAvailability}`. (modify)
- `daemon/dispatch.rs` — three handlers + dispatch arms. (modify)
- `daemon/state.rs` — `run_with_transport` wipes `<data_dir>/cache/open` at boot + clean shutdown. (modify)

**UI shell (`crates/ui/src/`):**
- `attachments.rs` — `validate_openable` also accepts `<data_dir>/cache/open`; remove `resolve_received_file`; unregister it in `main.rs`. (modify)
- `main.rs` — drop the `resolve_received_file` handler registration. (modify)

**Frontend (`crates/ui/src-svelte/src/`):**
- `lib/stores/attachments.ts` — `applyReceived` drops `path`; add `markAvailable`. (modify)
- `routes/+page.svelte` — `attachment_received` arm drops `path`, marks available. (modify)
- `lib/components/FileAttachmentBubble.svelte` — no inline preview; Open→`OpenAttachment`→`open_file`; Save→dialog→`SaveAttachment`; availability rehydrate via `AttachmentAvailable`. (modify)
- `lib/components/FileAttachmentBubble.test.ts`, `lib/stores/attachments.test.ts` — updated. (modify)

---

## Interfaces (cross-task contract)

Names later tasks rely on — define exactly as written:

- `Command::OpenAttachment { attachment_id: crate::daemon::hex::Hex16 }`
- `Command::SaveAttachment { attachment_id: crate::daemon::hex::Hex16, dest_path: String }`
- `Command::AttachmentAvailable { attachment_id: crate::daemon::hex::Hex16 }`
- `CommandResult::AttachmentDecrypted { path: String }`
- `CommandResult::AttachmentAvailability { available: bool }`
- `Event::AttachmentReceived { contact, attachment_id, filename, mime, size }` — **no `path`**
- `InboundDispatch::attachment_received(&self, peer, attachment_id, filename, mime, size)` — **no `path`**
- Managed cache root: `<data_dir>/cache/open/<hex attachment_id>/<sanitized filename>`
- `Hex16(pub [u8; 16])` — field access `.0` yields the raw id.

---

### Task 1: Receive side stops auto-decrypting; chunks retained; `path` removed

**Files:**
- Modify: `crates/core/src/daemon/events.rs` (AttachmentReceived)
- Modify: `crates/core/src/delivery/peer.rs:151-219` (`finalize_rx`), `:324` (trait method), `:1575` (test double), loopback test `:1791-1805`
- Modify: `crates/core/src/daemon/inbound.rs:594-660` (`finalize_offline`), `:793` (`attachment_received` impl), `:802` emit site
- Test: the existing direct guardrail `attachment_roundtrip_multichunk_over_loopback` and the 3.C offline component tests

**Interfaces:**
- Produces: the no-`path` `Event::AttachmentReceived` and `attachment_received` signatures (above), consumed by Tasks 5 & 6.

- [ ] **Step 1: Update the direct guardrail to assert the new contract (write the failing test)**

In the loopback guardrail test (search `attachment_roundtrip_multichunk_over_loopback`), after the receiver reports completion, replace the "reassembled file is byte-identical in the download dir" assertions with:

```rust
// Encrypted-at-rest: completion must NOT write plaintext to the download dir,
// and the encrypted chunks must be retained for on-demand decrypt.
let entries: Vec<_> = std::fs::read_dir(&download_dir)
    .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
    .unwrap_or_default();
assert!(
    entries.is_empty(),
    "completion must leave no plaintext in the download dir, found: {entries:?}"
);
let chunk_dir = data_dir_receiver.join("attachments").join(hex::encode(aid));
assert!(
    chunk_dir.is_dir() && std::fs::read_dir(&chunk_dir).unwrap().count() > 0,
    "encrypted chunks must be retained after completion"
);
```

(Use whatever the test already binds for the receiver's `download_dir`, `data_dir`, and the attachment id `aid`. If the test asserted on `got_path`, drop that — the event no longer carries a path.)

- [ ] **Step 2: Run it to confirm it fails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness attachment_roundtrip_multichunk_over_loopback`
Expected: FAIL — either a compile error (path removed) once you start, or the chunk-retention assertion fails because `finalize_rx` still deletes chunks.

- [ ] **Step 3: Remove `path` from the event**

In `daemon/events.rs`, `Event::AttachmentReceived`: delete the `path: String` field and its doc comment. Update the variant doc to: `/// An inbound attachment finished transferring and is available (encrypted at rest).`

- [ ] **Step 4: Drop `path` from the trait + both impls + test double**

In `delivery/peer.rs`:
- `InboundDispatch::attachment_received` default method — remove the `_path: &str` parameter.
- The `#[cfg(test)]` test double impl at `:1575` — remove the `path` param; if it records `(aid, path)`, record just `aid` and update the awaited tuple at `:1791` (`let got_aid = done.expect(...)`).

In `daemon/inbound.rs`, the production `attachment_received` impl at `:793`: remove the `path` param and the `path:` line from the `Event::AttachmentReceived { .. }` it sends.

- [ ] **Step 5: Rewrite `finalize_rx` to retain chunks and emit without a path**

In `delivery/peer.rs` `finalize_rx`, replace the `if won { .. }` block. The `won` lane no longer reassembles or allocates a download path; it just emits availability. Keep the `download_dir`/`store` guards only insofar as `store` (the `chunk_store`) is still needed for nothing here — actually neither is read now; simplify:

```rust
let won = match repo.set_status_if_pending(&aid) {
    Ok(won) => won,
    Err(e) => {
        tracing::warn!(err = %e, "inbound: attachment completion CAS failed");
        return;
    }
};
if won {
    // Encrypted-at-rest: do NOT reassemble and do NOT remove chunks here.
    // The chunks stay in the ChunkStore; plaintext is produced only on an
    // explicit OpenAttachment/SaveAttachment. Emit availability so the UI can
    // surface Open/Save.
    if let Some(d) = inbound.as_ref() {
        d.attachment_received(peer, aid, &safe_name, &manifest.mime, manifest.total_size);
    }
}
// Ack on the wire regardless (we finalized, or the offline lane already did).
```

Remove the now-unused `dir`/`source`/`unique_download_path`/`reassemble`/`fail_rx`-after-claim code in this function. The `download_dir` and `chunk_store` parameters of `finalize_rx` are now unused by the win path; if they become entirely unused, drop them from the signature and its call sites (`peer.rs:138`, `:877-892`); if still referenced elsewhere in the function, leave them. Prefer removing unused params to satisfy clippy. `StoreSource`/`sanitize_filename` imports: keep `sanitize_filename` (still used for `safe_name`), drop `StoreSource` if now unused.

- [ ] **Step 6: Rewrite `finalize_offline` the same way**

In `daemon/inbound.rs` `finalize_offline`, replace the `match repo.set_status_if_pending(attachment_id)` body:

```rust
match repo.set_status_if_pending(attachment_id) {
    Ok(true) => {
        // Encrypted-at-rest: retain chunks, do not reassemble. Emit availability.
        let _ = self.events_tx.send(Event::AttachmentReceived {
            contact,
            attachment_id: crate::daemon::hex::Hex16::from(*attachment_id),
            filename: safe,
            mime: manifest.mime.clone(),
            size: manifest.total_size,
        });
    }
    Ok(false) => {
        // Another lane already finalized; chunks are shared + retained, nothing to do.
    }
    Err(e) => {
        tracing::warn!(err = %e, "inbound: failed to mark attachment complete");
    }
}
```

Remove the now-unused `dir`/`download_dir` read, `std::fs::create_dir_all(&dir)`, `source`, `unique_download_path`, `reassemble`, and `store.remove` for this function. `store`/`source` may now be unused params — if `finalize_offline`'s `store` arg is unused, drop it and update the call site in `dispatch_attachment_chunk` (`self.finalize_offline(&attachment_id, &manifest, &store)` → drop `&store`). Keep `sanitize_filename` for `safe`. Keep `peer_for`/`contact` resolution.

- [ ] **Step 7: Fix all compile breaks, run the gate**

Run: `. "$HOME/.cargo/env" && cargo clippy --all-targets -- -D warnings` then `cargo test -p skattr-core --features test-harness`
Expected: green, including the updated guardrail and any 3.C offline component test (update those tests the same way — assert no plaintext written + chunks retained; drop any path assertion).

- [ ] **Step 8: Commit**

```bash
git add -A && git commit -m "feat(core): retain encrypted chunks on receive; drop AttachmentReceived.path

Both finalize lanes stop auto-reassembling to the download dir and stop
deleting the chunk store. Completion now only flips the CAS status gate and
emits availability; plaintext is produced only on demand (next tasks)."
```

---

### Task 2: On-demand decrypt — `OpenAttachment` / `SaveAttachment` / `AttachmentAvailable`

**Files:**
- Modify: `crates/core/src/daemon/commands.rs` (Command + CommandResult variants)
- Modify: `crates/core/src/daemon/dispatch.rs` (3 handlers + dispatch arms)
- Test: handler unit tests in `dispatch.rs`'s `#[cfg(test)]` (model on the `ExportBackup` test at `:4956`)

**Interfaces:**
- Consumes: `AttachmentRepo::get`, `ChunkStore`, `reassembler::reassemble`, `sanitize_filename`, `map_err`, `Hex16`.
- Produces: the three commands + two result variants (above), consumed by Tasks 5 & 6.

- [ ] **Step 1: Add the command + result variants (write a failing roundtrip test)**

In `commands.rs`, inside `pub enum Command` (after `ExportBackup`):

```rust
/// Decrypt a completed inbound attachment into the managed open-cache and
/// return its path (the UI shell then opens it). Plaintext is ephemeral —
/// the cache is wiped on daemon start + clean shutdown.
OpenAttachment {
    /// 16-byte attachment id.
    attachment_id: crate::daemon::hex::Hex16,
},
/// Decrypt a completed inbound attachment to a user-chosen path (the
/// intentional plaintext export).
SaveAttachment {
    /// 16-byte attachment id.
    attachment_id: crate::daemon::hex::Hex16,
    /// Absolute destination path chosen by the user.
    dest_path: String,
},
/// Report whether a completed, decryptable inbound attachment exists for
/// this id (drives UI rehydration after restart).
AttachmentAvailable {
    /// 16-byte attachment id.
    attachment_id: crate::daemon::hex::Hex16,
},
```

In `pub enum CommandResult` (after `PassphraseAudit`):

```rust
/// Path of a freshly decrypted attachment in the managed open-cache.
AttachmentDecrypted {
    /// Absolute cache path.
    path: String,
},
/// Availability answer for `Command::AttachmentAvailable`.
AttachmentAvailability {
    /// True iff a completed inbound attachment exists for the id.
    available: bool,
},
```

Then in the `#[cfg(test)]` `roundtrip` block (`commands.rs` tests) add a case that round-trips `Command::AttachmentAvailable { attachment_id: Hex16::from([7u8;16]) }` and `CommandResult::AttachmentAvailability { available: true }`.

- [ ] **Step 2: Run it to confirm it fails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness commands::tests`
Expected: FAIL to compile (variants don't exist yet) → then PASS once added. (This step's "test" is the serde roundtrip; the real behavior is Step 5's tests.)

- [ ] **Step 3: Add a shared decrypt helper in `dispatch.rs`**

Add near the attachment handlers a private helper that resolves manifest + chunks and reassembles to a target path:

```rust
/// Resolve a completed inbound attachment's manifest + chunk store and
/// reassemble the plaintext to `out_path`. Synchronous (file I/O + AEAD);
/// callers wrap in spawn_blocking. Errors map to typed IpcError.
fn decrypt_attachment_to<S>(
    handle: &Arc<DaemonHandle<S>>,
    data_dir: &std::path::Path,
    attachment_id: [u8; 16],
    out_path: &std::path::Path,
) -> std::result::Result<(), IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::error_kind::DaemonErrorKind;
    let repo = crate::storage::attachments::AttachmentRepo::new(&handle.pool);
    let row = repo.get(&attachment_id).map_err(map_err)?.ok_or_else(|| {
        IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            message: "attachment not found".into(),
        })
    })?;
    if row.direction != "in" || row.status != "complete" {
        return Err(IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            message: "attachment not available".into(),
        }));
    }
    let manifest =
        crate::attachment::manifest::AttachmentManifest::from_cbor(&row.manifest).map_err(map_err)?;
    let store = crate::attachment::store::ChunkStore::new(data_dir);
    let source = crate::attachment::store::StoreSource::new(&store, attachment_id);
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| IpcError::Internal(format!("create dir: {e}")))?;
    }
    crate::attachment::reassembler::reassemble(&manifest, &source, out_path).map_err(map_err)
}
```

(Confirm `AttachmentRow` fields `direction`/`status`/`manifest` are accessible — they are `pub` per `storage/attachments.rs`. Confirm `ChunkStore::new`/`StoreSource::new`/`reassemble` paths — all `pub(crate)`, reachable inside `core`.)

- [ ] **Step 4: Add the three handlers**

```rust
async fn open_attachment_cmd<S>(
    handle: Arc<DaemonHandle<S>>,
    attachment_id: [u8; 16],
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let data_dir = handle.config.read().await.data_dir.clone();
    // Resolve filename for the cache path from the manifest.
    let repo = crate::storage::attachments::AttachmentRepo::new(&handle.pool);
    let row = repo.get(&attachment_id).map_err(map_err)?.ok_or_else(|| {
        IpcError::Daemon(crate::daemon::error_kind::DaemonErrorKind::InvalidArgument {
            message: "attachment not found".into(),
        })
    })?;
    let manifest =
        crate::attachment::manifest::AttachmentManifest::from_cbor(&row.manifest).map_err(map_err)?;
    let safe = crate::delivery::chunk_transfer::sanitize_filename(&manifest.filename);
    let out_path = data_dir
        .join("cache")
        .join("open")
        .join(hex::encode(attachment_id))
        .join(&safe);
    let h = handle.clone();
    let dd = data_dir.clone();
    let op = out_path.clone();
    tokio::task::spawn_blocking(move || decrypt_attachment_to(&h, &dd, attachment_id, &op))
        .await
        .map_err(|e| IpcError::Internal(format!("spawn_blocking join: {e}")))??;
    Ok(CommandResult::AttachmentDecrypted {
        path: out_path.to_string_lossy().to_string(),
    })
}

async fn save_attachment_cmd<S>(
    handle: Arc<DaemonHandle<S>>,
    attachment_id: [u8; 16],
    dest_path: String,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let data_dir = handle.config.read().await.data_dir.clone();
    let out = std::path::PathBuf::from(&dest_path);
    let h = handle.clone();
    let dd = data_dir.clone();
    let op = out.clone();
    tokio::task::spawn_blocking(move || decrypt_attachment_to(&h, &dd, attachment_id, &op))
        .await
        .map_err(|e| IpcError::Internal(format!("spawn_blocking join: {e}")))??;
    Ok(CommandResult::Ok)
}

async fn attachment_available_cmd<S>(
    handle: Arc<DaemonHandle<S>>,
    attachment_id: [u8; 16],
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let repo = crate::storage::attachments::AttachmentRepo::new(&handle.pool);
    let available = matches!(
        repo.get(&attachment_id).map_err(map_err)?,
        Some(row) if row.direction == "in" && row.status == "complete"
    );
    Ok(CommandResult::AttachmentAvailability { available })
}
```

- [ ] **Step 5: Wire dispatch arms + write behavior tests**

In `dispatch.rs`'s command match (near `Command::ExportBackup => ...`):

```rust
Command::OpenAttachment { attachment_id } => {
    open_attachment_cmd(handle, attachment_id.0).await
}
Command::SaveAttachment { attachment_id, dest_path } => {
    save_attachment_cmd(handle, attachment_id.0, dest_path).await
}
Command::AttachmentAvailable { attachment_id } => {
    attachment_available_cmd(handle, attachment_id.0).await
}
```

Add `#[cfg(test)]` tests (model the harness on the `ExportBackup` test at `:4956` which builds a handle over a tempdir pool). Tests:

```rust
// 1. Stage a complete inbound attachment (insert manifest+chunks via
//    AttachmentRepo + ChunkStore from a known plaintext), set status=complete.
// 2. open_attachment_cmd → returns a path under <data_dir>/cache/open/<id>/;
//    file bytes == original plaintext.
// 3. save_attachment_cmd to <tmp>/saved.bin → bytes == original plaintext.
// 4. attachment_available_cmd → AttachmentAvailability { available: true };
//    for an unknown id and for a pending row → available: false.
// 5. open_attachment_cmd on a pending (status!='complete') row → Err(InvalidArgument).
```

Build the staged attachment with `crate::attachment::chunker::chunk_plaintext(&plaintext, "f.bin", "application/octet-stream")` → insert each ciphertext chunk via `ChunkStore::put`, `AttachmentRepo::insert(direction="in", manifest=&manifest.to_cbor()?, ...)`, mark every chunk received, then `set_status(id, "complete")`. Assert byte-identity against `plaintext`.

- [ ] **Step 6: Run the gate**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness` (regenerates the `Command`/`CommandResult` TS types). Then `cargo clippy --all-targets -- -D warnings`.
Expected: green; new `.ts` under `crates/ui/src-svelte/src/lib/ipc/types/` for the new variants.

- [ ] **Step 7: Commit** (include regenerated `.ts`)

```bash
git add -A && git commit -m "feat(core): OpenAttachment/SaveAttachment/AttachmentAvailable IPC commands

Decrypt on demand from the retained encrypted chunks + MLS manifest. Open
targets the managed cache; Save targets a user-chosen path; Available drives
UI rehydration. Handlers run on spawn_blocking; typed errors throughout."
```

---

### Task 3: Wipe the managed open-cache on boot and clean shutdown

**Files:**
- Modify: `crates/core/src/daemon/state.rs` (`run_with_transport` setup ~`:321` + teardown ~`:534`)
- Test: a unit test for a small extracted `wipe_open_cache(data_dir)` helper

**Interfaces:**
- Consumes: `data_dir` (already in scope in `run_with_transport`).

- [ ] **Step 1: Write the failing test for a wipe helper**

Add to `state.rs` `#[cfg(test)]` (or a small `daemon/cache.rs` if you prefer a dedicated unit — keep it where the test can reach it):

```rust
#[test]
fn wipe_open_cache_removes_decrypted_plaintext() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache").join("open").join("aa").join("x.bin");
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    std::fs::write(&cache, b"plaintext").unwrap();
    super::wipe_open_cache(tmp.path());
    assert!(!tmp.path().join("cache").join("open").exists());
    // Idempotent: a second wipe on an absent dir is a no-op (no panic, no error).
    super::wipe_open_cache(tmp.path());
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness wipe_open_cache_removes`
Expected: FAIL — `wipe_open_cache` undefined.

- [ ] **Step 3: Implement the helper**

In `state.rs` (module scope):

```rust
/// Best-effort wipe of the managed attachment open-cache
/// (`<data_dir>/cache/open`). Decrypted plaintext lives here only while an
/// attachment is open; clearing it at boot + clean shutdown keeps plaintext
/// ephemeral. Failures are warned, never fatal.
pub(crate) fn wipe_open_cache(data_dir: &std::path::Path) {
    let dir = data_dir.join("cache").join("open");
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(error = %e, "open-cache wipe failed"),
    }
}
```

- [ ] **Step 4: Call it at boot and clean shutdown**

In `run_with_transport`, in the setup region just after `let chunk_store = ...` (~`:321`):

```rust
// Clear any decrypted plaintext left in the open-cache from a previous run
// (covers abnormal exits where the shutdown wipe never ran).
crate::daemon::state::wipe_open_cache(data_dir);
```

(Use the correct in-scope reference to the helper — same-module call is just `wipe_open_cache(data_dir);`.)

In the teardown region, right after the `pool.close()` block (~`:536`):

```rust
// Remove decrypted plaintext so a clean shutdown leaves none on disk.
wipe_open_cache(data_dir);
```

- [ ] **Step 5: Run the gate**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness wipe_open_cache && cargo clippy --all-targets -- -D warnings`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(core): wipe the attachment open-cache on boot + clean shutdown"
```

---

### Task 4: UI shell — confine Open to the cache dir; remove the FS rehydrate probe

**Files:**
- Modify: `crates/ui/src/attachments.rs` (`validate_openable`, remove `resolve_received_file`)
- Modify: `crates/ui/src/main.rs` (drop the `resolve_received_file` registration)
- Test: `attachments.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `downloads_dir(&state)` (existing). Produces: an `open_file`/`reveal_in_folder` that also accepts `<data_dir>/cache/open`.

- [ ] **Step 1: Write the failing confinement test**

In `attachments.rs` tests, add (adapt to the existing `validate_openable` signature, which takes `(path, downloads)` — see Step 3 for the new signature):

```rust
#[test]
fn validate_openable_accepts_cache_and_rejects_outside() {
    let dir = tempfile::tempdir().unwrap();
    let downloads = dir.path().join("downloads");
    let cache = dir.path().join("cache").join("open").join("aa");
    std::fs::create_dir_all(&downloads).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    let f = cache.join("x.bin");
    std::fs::write(&f, b"x").unwrap();
    // In-cache file is accepted.
    assert!(validate_openable(f.to_str().unwrap(), &downloads, &cache.parent().unwrap().parent().unwrap().join("open")).is_ok());
    // A file outside both roots is rejected.
    let outside = dir.path().join("outside.bin");
    std::fs::write(&outside, b"x").unwrap();
    assert!(validate_openable(outside.to_str().unwrap(), &downloads, &cache).is_err());
}
```

(The exact `validate_openable` arity is your choice in Step 3 — match the test to it. Simplest: pass a slice of allowed roots.)

- [ ] **Step 2: Run it to confirm it fails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-ui validate_openable`
Expected: FAIL — signature mismatch / cache not allowed.

- [ ] **Step 3: Generalize `validate_openable` to a set of allowed roots**

Rewrite to accept any of several confinement roots:

```rust
/// Canonicalize a UI-supplied path, assert it is an existing regular file, AND
/// confine it to one of `roots` (downloads or the managed open-cache).
fn validate_openable(path: &str, roots: &[std::path::PathBuf]) -> Result<PathBuf, String> {
    let canon = std::fs::canonicalize(path).map_err(|e| format!("canonicalize {path}: {e}"))?;
    let ok = roots.iter().any(|r| {
        std::fs::canonicalize(r)
            .map(|cr| canon.starts_with(&cr))
            .unwrap_or(false)
    });
    if !ok {
        return Err(format!("{path}: outside allowed dirs"));
    }
    let meta = std::fs::metadata(&canon).map_err(|e| format!("{path}: not found: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{path}: not a regular file"));
    }
    Ok(canon)
}

/// `<data_dir>/cache/open` — the managed decrypt cache.
fn open_cache_dir(state: &tauri::State<'_, crate::daemon::AppState>) -> Result<PathBuf, String> {
    let dd = state.data_dir.read().clone().ok_or_else(|| "data_dir not initialised".to_string())?;
    Ok(dd.join("cache").join("open"))
}
```

Update `open_file`/`reveal_in_folder` to build `roots = vec![downloads_dir(&state)?, open_cache_dir(&state)?]` and pass `validate_openable(&path, &roots)`. Create the cache dir lazily isn't needed (the daemon created it on decrypt); `canonicalize` of a missing root simply fails that root, which is fine. Update the test in Step 1 to the final signature (`validate_openable(path, &[downloads, cache_open_root])`).

- [ ] **Step 4: Remove `resolve_received_file`**

Delete the `resolve_received_file` command from `attachments.rs` and its registration in `main.rs`'s `tauri::generate_handler![...]`. (Task 6 replaces its caller with `AttachmentAvailable`.)

- [ ] **Step 5: Run the gate**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-ui && cargo clippy --all-targets -- -D warnings`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(ui): confine attachment open to downloads + open-cache; drop FS rehydrate probe"
```

---

### Task 5: Frontend store + event dispatcher — availability instead of path

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/stores/attachments.ts`
- Modify: `crates/ui/src-svelte/src/routes/+page.svelte`
- Test: `crates/ui/src-svelte/src/lib/stores/attachments.test.ts`

**Interfaces:**
- Consumes: the no-`path` `attachment_received` event (Task 1); produces a store with an `available` flag (no `path`), consumed by Task 6.

- [ ] **Step 1: Update the store test (failing)**

In `attachments.test.ts`, replace any `applyReceived(..., { path })` usage:

```ts
it("applyReceived marks complete without a path", () => {
  applyReceived("aa".repeat(16), { filename: "f.bin", mime: "application/octet-stream", size: 10 });
  const s = attachmentFor("aa".repeat(16))!;
  expect(s.status).toBe("complete");
  expect("path" in s).toBe(false);
});

it("markAvailable flips a complete attachment available", () => {
  markAvailable("bb".repeat(16), { filename: "g.bin", mime: "text/plain", size: 3 });
  const s = attachmentFor("bb".repeat(16))!;
  expect(s.status).toBe("complete");
  expect(s.available).toBe(true);
});
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cd crates/ui/src-svelte && npx pnpm@10 test -- attachments.test`
Expected: FAIL — `markAvailable` undefined / `path` still set.

- [ ] **Step 3: Update the store**

In `stores/attachments.ts`:
- Remove `path?: string` from `AttachmentState`; add `available?: boolean`.
- `applyReceived(aidHex, info: { filename; mime; size })` — drop `path`, set `status: "complete"`, `available: true`.
- Add `markAvailable(aidHex, info: { filename; mime; size })` with the same body as `applyReceived` (used by the post-restart `AttachmentAvailable` query). Keep both names for clarity, or make `applyReceived` delegate to `markAvailable`.

- [ ] **Step 4: Update the dispatcher**

In `+page.svelte`, the `attachment_received` arm:

```ts
} else if (e.event === "attachment_received") {
  applyReceived(hex16ToString(e.data.attachment_id), {
    filename: e.data.filename,
    mime: e.data.mime,
    size: Number(e.data.size),
  });
}
```

(Drop `path: e.data.path` — the field no longer exists on the regenerated `Event` type.)

- [ ] **Step 5: Run the gate**

Run: `cd crates/ui/src-svelte && npx pnpm@10 check && npx pnpm@10 test`
Expected: green (svelte-check 0/0; vitest passes).

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(ui): attachment store tracks availability, not a plaintext path"
```

---

### Task 6: `FileAttachmentBubble` — Open/Save on demand, no inline preview

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/components/FileAttachmentBubble.svelte`
- Test: `crates/ui/src-svelte/src/lib/components/FileAttachmentBubble.test.ts`

**Interfaces:**
- Consumes: `OpenAttachment`/`SaveAttachment`/`AttachmentAvailable` (Task 2), `open_file`/`reveal_in_folder` (Task 4), the store's `available` (Task 5).

- [ ] **Step 1: Update the bubble vitest (failing)**

In `FileAttachmentBubble.test.ts`, replace `resolve_received_file`/preview expectations:
- A `complete`/`available` receiver bubble renders **Open** and **Save…** buttons and **no `<img>`**.
- Clicking **Save…** invokes the dialog `save` then the `SaveAttachment` IPC request.
- On mount of a received-but-not-in-store bubble, the component issues an `AttachmentAvailable` request and, on `{ available: true }`, calls `markAvailable`.

Mock `@tauri-apps/api/core` `invoke` and `ipcClient.request` (follow the existing test's mocking; the file already mocks `invoke`). Assert no element with `class="preview"` exists.

- [ ] **Step 2: Run it to confirm it fails**

Run: `cd crates/ui/src-svelte && npx pnpm@10 test -- FileAttachmentBubble`
Expected: FAIL.

- [ ] **Step 3: Rewrite the bubble script**

- Remove `convertFileSrc` import and the `showImage`/`imgBroken`/`isImage` preview logic and the `<img class="preview">` branch entirely.
- Replace the `resolve_received_file` `$effect` with an availability rehydrate:

**IPC shape (verified against the codebase):** `Command` is internally tagged
`{ cmd: "<snake_case>", ...fields }`; `CommandResult` is tagged
`{ result: "<snake_case>", data: <payload> }`; `IpcResponse` is
`{ resp: "ok", data: CommandResult } | { resp: "err", ... } | ...`. The repo
unwraps `ok` via `unwrapOk(resp)` (see `stores/contacts.ts`). So:
- `{ cmd: "open_attachment", attachment_id }` → `{ result: "attachment_decrypted", data: { path } }`
- `{ cmd: "save_attachment", attachment_id, dest_path }` → `{ result: "ok" }`
- `{ cmd: "attachment_available", attachment_id }` → `{ result: "attachment_availability", data: { available } }`

```ts
import { ipcClient } from "$lib/ipc/tauri";
import { unwrapOk } from "$lib/ipc/client"; // export at crates/ui/src-svelte/src/lib/ipc/client.ts:37
import { save } from "@tauri-apps/plugin-dialog";
import { markAvailable } from "$lib/stores/attachments";

// Rehydrate after restart: the transfer store is session-scoped. Ask the
// daemon whether this completed attachment is decryptable and, if so, enable
// Open/Save. No plaintext is produced by this query.
$effect(() => {
  if (isOutgoing || !summary || xferState?.available) return;
  const s = summary;
  const aid = s.attachment_id; // hex string
  ipcClient
    .request({ cmd: "attachment_available", attachment_id: aid } as any)
    .then((resp) => {
      if (resp.resp !== "ok") return;
      const result = resp.data;
      if (result.result === "attachment_availability" && result.data.available) {
        markAvailable(aid, { filename: s.filename, mime: s.mime, size: s.total_size });
      }
    })
    .catch(() => {});
});
```

(`unwrapOk` throws on `err`; here we tolerate failure, so branch on `resp.resp === "ok"` directly. `attachment_id` is the hex string from `summary.attachment_id`.)

- Replace `complete` to not require a path: `let complete = $derived(!isOutgoing && (xferState?.status === "complete" || xferState?.available));`
- A shared decrypt helper, then `doOpen`/`doReveal`/`doSave`:

```ts
// Returns the managed-cache plaintext path, or throws.
async function decryptToCache(): Promise<string> {
  const resp = await ipcClient.request({ cmd: "open_attachment", attachment_id: aidHex } as any);
  const result = unwrapOk(resp); // throws on err
  if (result.result !== "attachment_decrypted") throw new Error("unexpected result");
  return result.data.path;
}

async function doOpen() {
  if (!aidHex) return;
  try {
    const path = await decryptToCache();
    await invoke("open_file", { path });
  } catch {
    const showFolder = await ask(
      "Your system doesn't have an app set to open this type of file. Open its folder instead, so you can open it yourself?",
      { title: "Can't open file", kind: "warning" },
    );
    if (showFolder) await doReveal();
  }
}

async function doReveal() {
  if (!aidHex) return;
  try {
    const path = await decryptToCache();
    await invoke("reveal_in_folder", { path });
  } catch {
    toast.show("Couldn't open the folder");
  }
}

async function doSave() {
  if (!aidHex) return;
  const dest = await save({ defaultPath: filename || undefined });
  if (!dest) return; // cancelled
  try {
    const resp = await ipcClient.request({
      cmd: "save_attachment",
      attachment_id: aidHex,
      dest_path: dest,
    } as any);
    if (resp.resp !== "ok") throw new Error("save failed");
    toast.show(`Saved to ${dest}`);
  } catch {
    toast.show("Couldn't save the file");
  }
}
```

- Replace both action button rows with the same two buttons (no image branch):

```svelte
<div class="actions">
  <button type="button" onclick={doOpen} aria-label="Open">Open</button>
  <button type="button" onclick={doSave} aria-label="Save decrypted file">Save…</button>
</div>
```

Keep "Show in folder" only if you want a third button; the design lists Open + Save as the primary actions. Drop the `xferState?.path` guards (no path now) — gate on `complete`.

- [ ] **Step 4: Run the gate**

Run: `cd crates/ui/src-svelte && npx pnpm@10 check && npx pnpm@10 test`
Expected: green. Resolve any remaining `path` references the check flags.

- [ ] **Step 5: Full workspace gate**

Run:
```bash
. "$HOME/.cargo/env" && cargo fmt --all --check \
  && cargo clippy --all-targets -- -D warnings \
  && cargo test -p skattr-core -p skattr-ui --features skattr-core/test-harness
cd crates/ui/src-svelte && npx pnpm@10 check && npx pnpm@10 test
```
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(ui): attachment bubble decrypts on demand (Open to cache, Save to chosen path)

No inline image preview; availability rehydrates via AttachmentAvailable.
Open decrypts to the managed cache then opens; Save writes to a user path."
```

---

### Task 7: Docs — disclose retention + the new behavior

**Files:**
- Modify: `README.md` (v1.0 limitations), `docs/THREAT_MODEL.md` (at-rest section), the attachment design deep-dive banner if one references auto-download.

**Interfaces:** none (docs only).

- [ ] **Step 1: Update limitations**

Add to README v1.0 limitations and THREAT_MODEL at-rest notes:
- Received attachments are stored encrypted at rest and decrypted only on explicit Open/Save.
- **Retention:** completed encrypted chunks are retained indefinitely (no GC) → `<data_dir>/attachments/` grows with received files; a delete/retention policy is a v1.1 candidate.
- Open decrypts to `<data_dir>/cache/open/`, which is wiped on app start + clean shutdown.

- [ ] **Step 2: Verify no doc claims auto-save to Downloads**

Run: `grep -rn "Downloads\|download dir\|reassembl" README.md docs/THREAT_MODEL.md docs/skattr-deep-dives.md`
Fix any line that still claims attachments auto-land decrypted in the download dir.

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "docs: disclose encrypted-at-rest attachments + indefinite chunk retention"
```

---

## Self-Review

**Spec coverage:** §1 receive-side → Task 1. §2 commands → Task 2. §3 cache lifecycle + confinement → Tasks 3 (wipe) & 4 (confinement). §4 UI → Tasks 5 & 6. §5 event shape → Task 1 (core) + Task 5 (frontend). Limitations disclosure → Task 7. All covered.

**Placeholder scan:** every code step carries real code; the one deliberately flexible point (exact `ipcClient.request`/`IpcResponse` unwrapping in the frontend) is pinned to "mirror an existing command call, e.g. `GetConfig`" — concrete enough to follow.

**Type consistency:** `Command::*Attachment*` variants use `Hex16` (`.0` → `[u8;16]`); handlers take `[u8;16]`; `CommandResult::AttachmentDecrypted{path}` / `AttachmentAvailability{available}` match their dispatch arms and the frontend unwrapping; `Event::AttachmentReceived` and `attachment_received` lose `path` consistently across Task 1 and Task 5; the store's `applyReceived`/`markAvailable` signatures match the dispatcher and bubble call sites.
