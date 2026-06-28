# Encrypted-at-rest attachments (decrypt-on-demand) — Design

**Date:** 2026-06-28
**Status:** Approved design, pre-plan
**Scope:** v1.0 pull-forward fix. Own branch + plan off the field-testing work.
**ADR:** none — no peer-facing wire-format change. The new IPC commands are
additive, local (UI⟷daemon) only. ADR 0006 (mailbox `Deposit`) and the 3.B
transport `FrameType`s are untouched.

## Problem

A received attachment is encrypted at rest in two layers that already exist:

- **Chunks** live at `<data_dir>/attachments/<hex id>/<index>` as opaque AEAD
  ciphertext, keyed from the manifest's `file_key` (`attachment/store.rs`).
- **The manifest** (which holds `file_key`) rides in MLS as a `Kind::File`
  message and is persisted in the `age`-encrypted SQLite DB
  (`AttachmentRow.manifest`).

The single plaintext leak is the **final completion step**. Both finalize
lanes — `finalize_rx` (`delivery/peer.rs:151`, the 3.B direct lane) and
`finalize_offline` (`daemon/inbound.rs:594`, the 3.C offline lane) — on the
CAS-winning lane today:

1. allocate `unique_download_path` in the configured download dir,
2. `reassemble()` → write **plaintext** to that path,
3. `store.remove(attachment_id)` → **delete** the encrypted chunks,
4. emit `Event::AttachmentReceived { path: <plaintext path>, .. }`.

Result: every received file lands decrypted in `~/Downloads` (or the configured
dir) the instant it completes, with no user choice. Field-test report:
*"the file is saved in /home/myggiz/Downloads … unencrypted … this should only
be by choice. Until the user selects to 'save it decrypted' it should live in
the skattr directory and attachments as encrypted."*

## Principle

Received attachments stay **encrypted at rest by default**, inside
`<data_dir>/`, **indefinitely**. Plaintext is produced **only on an explicit
user action** (Open or Save), by reassembling on demand from the chunks that
are already on disk. We do not add a new encryption scheme — we stop the
auto-decrypt step and reuse the existing chunk/manifest crypto for on-demand
decryption.

## Decisions (locked during brainstorming)

- **Open lifecycle:** decrypt to a **managed cache** `<data_dir>/cache/open/`,
  wiped on daemon start **and** on clean shutdown. Plaintext never auto-lands in
  `~/Downloads` or `/tmp`, and is gone next launch.
- **Image preview:** **none in v1.0**. Images are treated like any other file
  (card only); no decrypt-on-scroll. Inline preview is a v1.1 candidate.
- **Save** is the only route that writes plaintext **outside** the managed
  cache, and only to a user-chosen path.

## Architecture

Three interlocking changes, each independently testable:

1. **Receive side** stops auto-decrypting (core, both lanes).
2. **On-demand decrypt** via new local IPC commands backed by the existing
   reassembler (core + UI shell).
3. **UI** surfaces Open / Save and rehydrates availability after restart.

### 1. Receive side — stop auto-decrypting (both lanes)

`finalize_rx` (`delivery/peer.rs`) and `finalize_offline`
(`daemon/inbound.rs`) change identically. On the CAS-winning lane
(`set_status_if_pending` returns `Ok(true)`):

- **Remove** the `unique_download_path` + `reassemble()` → download-dir write.
- **Remove** the `store.remove(attachment_id)` call — **keep** the encrypted
  chunks; they are now the durable at-rest representation.
- Emit `Event::AttachmentReceived` as a *"complete & available (encrypted)"*
  signal, **with no plaintext path** (see §5).

The CAS gate (`set_status_if_pending`) **stays** — it remains the cross-lane
dedup gate so the `AttachmentReceived` event fires exactly once even under
simultaneous direct+offline completion. With reassembly gone from this path,
the post-claim failure branches simplify: there is no reassembly to fail, so
`finalize_rx` no longer has a `fail_rx`-after-claim arm and `finalize_offline`
no longer reverts pending→complete on a reassembly error. The losing lane
(`set_status_if_pending` returns `Ok(false)`) now does **nothing** with the
chunk store: chunks are retained for on-demand decrypt, so the prior
`store.remove` on the losing lane is dropped. Both lanes leave the single
shared chunk set intact. The wire `AttachmentComplete` ack to the sender is
unchanged (it still fires once the transfer is complete on the wire).

`download_dir` is no longer consulted by either finalize lane. It remains a
config field used only as the **default destination** offered by the Save
dialog (§4).

### 2. On-demand decrypt — core IPC commands

Append-only, local IPC (UI⟷daemon). Defined in `daemon` command/response enums
alongside the existing `SendFile` / `ExportBackup` commands; no peer-facing
protocol. The handler resolves the manifest via the existing
`AttachmentRepo::get(attachment_id)` → `AttachmentManifest::from_cbor(row.manifest)`
(the `manifest` column is populated for `direction='in'` rows; no new accessor
required), builds a `StoreSource` over the `ChunkStore`, and calls the existing
`reassembler::reassemble`.

- **`Command::OpenAttachment { attachment_id }` → `Response`/event carrying a
  path `String`.**
  - Validates the row is `direction='in'` and `status='complete'` (else a
    typed `DaemonErrorKind` — reuse `StorageError`; no new variant).
  - Reassembles into `<data_dir>/cache/open/<hex id>/<sanitized filename>`
    (create dirs as needed; reuse `sanitize_filename`). If the cache file
    already exists from an earlier Open this session, reassemble is idempotent
    (atomic `.part` → rename in the reassembler), so re-Open just overwrites.
  - Returns the absolute cache path. The **UI shell** then opens it via the
    opener plugin (§4) — the daemon does not shell out to a viewer.
- **`Command::SaveAttachment { attachment_id, dest_path }` → ok / typed error.**
  - Same manifest/chunk resolution; reassembles directly to the user-chosen
    `dest_path` (the intentional plaintext export). Returns ok; the UI shows a
    toast with the saved location. No opener/reveal is invoked (keeps path
    confinement tight — see §3).
- **`Command::AttachmentAvailable { attachment_id } → bool`.**
  - True iff the row is `direction='in'`, `status='complete'`. (Chunk presence
    is implied by completion; an explicit `ChunkStore` existence probe is an
    optional belt-and-suspenders, not required.) Drives UI rehydration after a
    restart, replacing the filesystem-probe `resolve_received_file`
    (`ui/src/attachments.rs`), which is removed.

Run `OpenAttachment`/`SaveAttachment` handlers on `spawn_blocking` (file I/O +
AEAD over potentially many chunks), matching the `ExportBackup` precedent.

### 3. Managed cache lifecycle & path confinement

- **Cache root:** `<data_dir>/cache/open/`.
- **Wipe on daemon start:** best-effort `remove_dir_all(<data_dir>/cache/open)`
  in the `run_with_transport` setup region (near the existing chunk-store / data
  setup), before serving. So any plaintext from a previous run is cleared even
  after an abnormal exit.
- **Wipe on clean shutdown:** best-effort `remove_dir_all` in the
  `run_with_transport` teardown region (alongside the deterministic
  `pool.close()`), so a clean exit leaves no decrypted plaintext.
- **Opener confinement (4.D P1):** `validate_openable` in
  `ui/src/attachments.rs` currently confines `open_file`/`reveal_in_folder` to
  `<data_dir>/downloads`. It must also accept `<data_dir>/cache/open` (the Open
  target). Both dirs are canonicalized and matched component-wise as today.
  Save uses a user-chosen path and invokes **no** opener/reveal, so it needs no
  confinement relaxation.

### 4. UI — `FileAttachmentBubble.svelte` and wiring

- **Receiver card:** no inline image preview (v1.0). File icon + name + size.
  Once available (live `AttachmentReceived`, or `AttachmentAvailable` on
  load), show **Open** and **Save…** actions.
- **Open** → invoke `OpenAttachment` → receive the cache path → open it via the
  existing opener path (`open_file` command). Keep the current
  unknown-extension fallback: on opener failure, `ask(...)` → offer
  "Show in folder" (`reveal_in_folder` on the cache path, now in-scope).
- **Save…** → `@tauri-apps/plugin-dialog` **save** picker (default dir =
  `resolved_download_dir`, default name = manifest filename) → `SaveAttachment`
  → success toast with the path. Cancel = no-op.
- **Rehydration:** on conversation load, replace the `resolve_received_file`
  probe with an `AttachmentAvailable` query keyed by `attachment_id`; enable
  Open/Save when true. The session-scoped live-transfer store
  (`stores/attachments.ts`) is unchanged for in-flight progress.

### 5. Event shape (`Event::AttachmentReceived`)

The `path` field is **removed** from `Event::AttachmentReceived` (a local IPC /
ts-rs type; the UI ships in the same build). Consequently:

- The `InboundDispatch::attachment_received` trait method drops its `path`
  argument; the `delivery::peer` test double and the loopback test harness
  drop the path arg / assertion correspondingly.
- `AttachmentProgress` and `AttachmentFailed` are unchanged.
- The frontend `+page.svelte` `AttachmentReceived` dispatcher arm stops reading
  `path` and instead marks the attachment available (triggering the Open/Save
  affordance), keyed by `attachment_id`.

## Components & boundaries

| Unit | Responsibility | Depends on |
|---|---|---|
| `finalize_rx` / `finalize_offline` (edited) | CAS-gate completion; emit availability; **retain** chunks | `AttachmentRepo`, `ChunkStore` |
| `OpenAttachment` / `SaveAttachment` / `AttachmentAvailable` handlers (new) | resolve manifest+chunks, reassemble on demand to cache or chosen path | `AttachmentRepo::get`, `ChunkStore`, `reassembler` |
| cache wipe (new, in `run_with_transport`) | clear `<data_dir>/cache/open` at boot + clean shutdown | `data_dir` |
| `attachments.rs` confinement (edited) | allow cache dir for open/reveal; remove `resolve_received_file` | data_dir/config |
| `FileAttachmentBubble.svelte` + `+page.svelte` (edited) | Open/Save UX; availability rehydration; no preview | new commands |

## Error handling

- `OpenAttachment`/`SaveAttachment` on a non-`complete`/`in` row, a missing
  chunk, a hash/AEAD/size mismatch, or an I/O error → typed `DaemonErrorKind`
  (reuse `StorageError`); the UI surfaces a human message via the existing
  `errorMessage(IpcError)` map (4.C). No partial output: the reassembler writes
  `.part` then atomically renames, and removes `.part` on validation failure.
- Cache wipe failures are best-effort `warn!` only — never fatal to
  boot/shutdown.
- A `SaveAttachment` `dest_path` the OS rejects (permissions, no space) →
  typed error → toast; the encrypted source is untouched.

## Scope & limitations

- **Sender side unchanged.** The sender owns its own source file; it strips →
  chunks → stages encrypted in `ChunkStore` and announces the manifest. No
  plaintext is written by us on the send side.
- **Indefinite retention (disclosed limitation):** completed encrypted chunks
  are retained with no GC, so `<data_dir>/attachments/` grows with received
  files. Acceptable for v1.0; document in README/THREAT_MODEL limitations. A
  per-attachment delete / retention policy is a v1.1 candidate.
- **No inline image preview in v1.0** (decision above).
- **Managed-cache plaintext while an app holds it open:** when the user Opens a
  file, the external viewer keeps a handle to the cache plaintext until it (and
  the skattr session) exit. The cache is wiped on next start regardless. This
  is the irreducible cost of "open with the OS handler" and is acceptable.

## Testing

Core (real assembly / component-level, matching existing guardrail patterns):

- **Update** `attachment_roundtrip_multichunk_over_loopback` (3.B) and the 3.C
  offline guardrails: after completion assert **(a)** the download dir contains
  **no plaintext file**, **(b)** the encrypted chunks are **retained** in
  `ChunkStore`, **(c)** `AttachmentReceived` fired once with no path.
- **New:** `OpenAttachment` decrypts a completed attachment to
  `<data_dir>/cache/open/...`, byte-identical to the original, EXIF stripped;
  re-Open is idempotent.
- **New:** `SaveAttachment` writes byte-identical plaintext to a chosen path;
  a rejected `dest_path` returns a typed error and writes nothing.
- **New:** cache wipe removes `<data_dir>/cache/open` contents on the
  `run_with_transport` boot path (and the teardown path leaves it absent).
- **New:** `AttachmentAvailable` is false for a pending row, true once complete.

UI:

- `FileAttachmentBubble` vitest: available state renders Open/Save; Save invokes
  the dialog then `SaveAttachment`; rehydration calls `AttachmentAvailable`.
- `validate_openable` Rust test: a path under `<data_dir>/cache/open` is
  accepted; a path outside both dirs is rejected.

## Out of scope (v1.1 candidates)

- Inline image preview (decrypt-to-cache on explicit tap).
- Attachment delete / retention GC for the encrypted chunk store.
- Sender-side download progress; concurrent attachments per peer (pre-existing
  deferrals).
