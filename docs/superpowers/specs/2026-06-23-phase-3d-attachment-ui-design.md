# Phase 3.D — Attachment UI (design)

**Date:** 2026-06-23
**Status:** Approved (brainstorm) — ready for implementation plan
**Depends on:** Phase 3.A/3.B/3.C (attachment core, direct transfer, offline
transfer) — all complete and merged. Consumes the already-shipped `SendFile` /
`FileQueued` / `Kind::File` / `Event::Attachment*` surface.
**No ADR, no core protocol change.** 3.D is presentation + IPC wiring in the
`skattr-ui` crate (Tauri 2 + SvelteKit). The one Rust touch is a UI-shell
`#[tauri::command]` that *reads* a manifest via the existing `pub`
`AttachmentManifest::from_cbor` — not a protocol change.
**Scope boundary:** sender + receiver attachment UX — attach/pick, send, size
limits, progress, inline image preview, open/reveal, failures. Out of scope: any
core change; concurrent attachments per peer; a configurable download folder
(deferred — see §10); the 3.C v1.1 follow-ups; Phase 4.

---

## 1. Goal

Make file attachments usable in the desktop UI: pick a file and send it, see it
in the conversation as a file message, watch a received file download (progress
bar), preview images inline, open/reveal received files, and get clear feedback
on size limits and failures. The core already does the work (chunk/transfer/
reassemble, online + offline); 3.D surfaces it.

## 2. Existing surface (already shipped; 3.D consumes)

- `Command::SendFile { contact, path }` → `CommandResult::FileQueued {
  message_id, attachment_id, total_chunks }`.
- Messages carry `Kind::File { manifest }` (`manifest` = base64 CBOR
  `AttachmentManifest`: `attachment_id [16]`, `filename`, `mime`, `total_size`,
  `chunk_size`, `chunks[]`). A `Kind::File` message is a normal `MessageRecord`.
- `Event::AttachmentReceived { contact, attachment_id, filename, mime, size,
  path }`, `Event::AttachmentProgress { attachment_id, received, total }`,
  `Event::AttachmentFailed { attachment_id, reason }` — **all receiver-side**.
- Limits: `MAX_ATTACHMENT_BYTES = 100 MiB` (daemon hard cap),
  `MAX_OFFLINE_ATTACHMENT_BYTES = 10 MiB` (offline lane only); image metadata is
  stripped before send.
- All ts-rs types already generated into
  `crates/ui/src-svelte/src/lib/ipc/types/`. The IPC client
  (`lib/ipc/{client,tauri}.ts`) already has `request(cmd)` + Channel-based
  `subscribe(filter, onEvent)`. **The UI itself is 100% greenfield** for
  attachments (no event handlers, store, bubble, or file-related Tauri plugins).

## 3. Two reality-checks that shape the design

1. **Progress is receiver-side only.** The sender gets `FileQueued` + the
   manifest message's `DeliveryStatusChanged`; it never sees the recipient's
   download progress. So the **sender bubble** shows a static file card + the
   manifest's delivery status (queued→sent/delivered/deposited) — it must not
   imply "downloaded". The **receiver bubble** shows live progress → preview.
2. **`Kind::File.manifest` is base64 CBOR.** To render filename/size/mime and to
   get the `attachment_id` that correlates a bubble with its
   `AttachmentProgress`/`Received` events, the UI must decode it — done via a
   shell command using the canonical Rust decoder (§7), so the UI can never
   disagree with the daemon.

## 4. Locked decisions (from the brainstorm)

1. **Inline preview via the Tauri asset protocol, scoped to `download_dir`.**
   `convertFileSrc(path)` → `asset://…` on the `<img>`; the webview streams the
   file lazily; the asset-protocol scope is confined to the daemon's downloads
   directory. CSP gains `img-src … asset: http://asset.localhost`. (Chosen over
   `data:`-URI-via-command and the `fs` plugin: lazy/streaming, no big buffers in
   JS, scope-confined.)
2. **Manifest decoded by a shell `#[tauri::command]` using the canonical Rust
   decoder** (`AttachmentManifest::from_cbor`), returning the 4 scalar display
   fields. No JS CBOR dependency; chunk-hash bulk stays in Rust.
3. **Pre-send size gate in the UI, daemon as backstop.** A `file_size` command
   stats the picked file: **> 100 MiB hard-blocked**, **10–100 MiB soft-warned**
   ("only delivered while your contact is online"), **≤ 10 MiB** straight
   through. The daemon's 100 MiB cap remains authoritative.
4. **Default download folder; no configurable-folder setting in 3.D.** Files
   land in `<data_dir>/downloads`; each received bubble has "Reveal in folder".
   A configurable location is deferred (it needs core `ConfigSnapshot`/
   `ConfigPatch` ts_rs fixes — out of 3.D scope; see §10).

## 5. Architecture

**New SvelteKit units (`crates/ui/src-svelte/src/lib/`):**
- `stores/attachments.ts` — global live-transfer store: `writable<Map<string
  /*attachment_id hex*/, AttachmentState>>` (§6), modeled on `delivery.ts`.
- `components/FileAttachmentBubble.svelte` — renders a `Kind::File` message:
  decoded-manifest static card + live state overlay (progress / preview / open).
- `lib/attachments.ts` — helpers: `formatBytes`, mime→icon, `isImage`, the
  `decode_attachment_manifest` wrapper + a per-`message_id` decode memo.

**New `skattr-ui` Rust-shell commands (`crates/ui/src/`, registered in
`generate_handler!`):**
- `decode_attachment_manifest(manifest: String) -> ManifestSummary`
  (`{ attachment_id: hex, filename, mime, total_size }`).
- `file_size(path: String) -> Result<u64>`.
- `open_file(path)` / `reveal_in_folder(path)` (via the `opener` plugin;
  canonicalize + exists + regular-file validation).
- File picking uses `@tauri-apps/plugin-dialog` `open()` directly from JS (or a
  thin `pick_file()` wrapper if cleaner).

**Wiring into existing code:**
- `routes/+page.svelte` event dispatcher: 3 new `else if` arms
  (`attachment_received`/`attachment_progress`/`attachment_failed`) → the store
  update fns (mirrors the existing `delivery_status_changed` arm).
- `components/MessageBubble.svelte`: switch on `record.kind.kind` — `text`
  (existing) / **`file` → `<FileAttachmentBubble>`** / others unchanged.
- `components/Composer.svelte`: a paperclip "attach" button (house CSS-var
  style) → picker → size gate → `SendFile`.
- `tauri.conf.json` + capability: add `dialog` + `opener` plugins; enable
  `assetProtocol`; extend CSP `img-src`.

## 6. State model

```ts
type AttachmentStatus = "queued" | "sending" | "receiving" | "complete" | "failed";
interface AttachmentState {
  status: AttachmentStatus;
  received: number;   // chunks (receiver)
  total: number;      // chunks
  filename?: string;
  mime?: string;
  size?: number;      // bytes (≤100 MiB → number is safe)
  path?: string;      // local path when complete (receiver)
  reason?: string;    // when failed
}
export const attachments = writable<Map<string /*aid hex*/, AttachmentState>>(new Map());
// markQueued / applyProgress / applyReceived / applyFailed + attachmentFor(aidHex)
```
Update fns do the immutable `new Map(m)` copy (the `delivery.ts` pattern);
`hex16ToString` is reused for keys.

**Three inputs reconciled by `attachment_id`:** `FileQueued` (sender →
`queued`), the decoded `Kind::File` manifest (static card + the id key, both
sides), and the receiver events (`receiving`/`complete`/`failed`).

**Status semantics:** the **sender** bubble shows `queued` then the manifest
message's `DeliveryStatusChanged` (the attachments store stays `queued` — no
receiver events reach the sender). The **receiver** bubble: idle → `receiving`
(first progress) → `complete` (`AttachmentReceived`) / `failed`
(`AttachmentFailed`).

**Order-independent + global:** everything keys on `attachment_id` and the store
is global (decoupled from the active conversation, which `conversation.ts` holds
only for the open contact). So events arriving before the bubble, during a
conversation switch, or for a background conversation all just record into the
store; the bubble reads current state whenever it mounts. The store is
**session-scoped** (cleared on app restart → the deferred restart case, §10).

## 7. The file bubble (`FileAttachmentBubble.svelte`)

A file card in the house CSS-var style, inside the existing `.bubble`. States:

- **Sender (outgoing):** always a file card (icon + filename + size) + the
  manifest message's delivery icon. No preview (the sent file isn't in the
  sender's `download_dir`, and the asset scope is downloads-only).
- **Receiver, downloading:** file card + a progress bar from `received/total`
  ("Downloading N%"). If the file is ≤ 8 chunks the daemon may emit no
  intermediate progress → show an indeterminate "Downloading…" until completion.
- **Receiver, complete + image** (`status==="complete" && isImage(mime) &&
  path`): an inline `<img>` via `convertFileSrc(path)` + filename/size + `[Open]`
  `[Reveal]`.
- **Receiver, complete + non-image:** file card + `[Open]` `[Reveal]`.
- **Receiver, failed:** ⚠️ + filename + `reason` (no in-UI retry; recovery is a
  resend).

`[Open]` → `open_file(path)`; `[Reveal]` → `reveal_in_folder(path)`. Icon is
mime-derived (image/pdf/archive/audio/video/generic). `<img on:error>` falls
back to the file card (no broken-image glyph).

## 8. Flows

**Attach & send:** paperclip → `dialog.open()` → (cancel = no-op) → `file_size`
→ size gate (§4.3): > 100 MiB block + toast; 10–100 MiB confirm; else proceed →
`request({ cmd: "send_file", contact, path })` → `unwrapOk` → `FileQueued`. Insert
an **optimistic** outgoing `Kind::File` bubble (the conversation store already
has an `OptimisticMessage` path for text — reuse it; carry the picked
filename/size for instant display, reconcile to the real `MessageRecord` by
`message_id`). `markQueued(attachment_id, {filename, size})`. On `SendFile`
error → failed-send affordance + toast.

**Receive:** `Kind::File` `MessageReceived` → bubble decodes the manifest (once,
memoized by `message_id`) → static card; `AttachmentProgress` → `applyProgress`
→ bar; `AttachmentReceived{path}` → `applyReceived` → preview/open;
`AttachmentFailed` → `applyFailed`.

## 9. Failures & edge cases

- `AttachmentFailed` → failed bubble (§7); no retry (documented).
- Manifest decode fails (corrupt/unknown version) → neutral "📎 Attachment
  (unavailable)" card + warn log; never throws into render.
- Send errors (unreadable file / IPC / daemon-cap backstop) → failed-send + toast.
- `file_size` fails (file gone between pick and send) → error toast, nothing sent.
- `Open`/`Reveal` fails (file moved/deleted) → "File not found" toast; commands
  validate canonicalize+exists+regular-file (defense-in-depth; paths are always
  daemon-authored from `AttachmentReceived`).
- Image fails to load via asset → `<img on:error>` → file card fallback.
- Progress before the bubble / non-active conversation → recorded in the global
  store; bubble reads on mount.
- Tiny file (≤ 8 chunks) → possibly no intermediate progress → indeterminate
  "Downloading…" then complete.

## 10. Tauri config & shell changes

- **Plugins:** `dialog` (`@tauri-apps/plugin-dialog` + `tauri-plugin-dialog`),
  `opener` (`@tauri-apps/plugin-opener` + `tauri-plugin-opener`). Add to Rust
  deps, JS deps, and the plugin registrations.
- **`assetProtocol`:** enable it; the scope must cover the daemon's
  `<data_dir>/downloads`, which is dynamic — so **register the asset-protocol
  scope at runtime** once the shell knows the download dir at startup (cleanest;
  exact folder, no over-broad glob). The static `tauri.conf.json` enables the
  protocol.
- **CSP:** `img-src 'self' data: asset: http://asset.localhost;`.
- **Capabilities:** grant `dialog:allow-open`,
  `opener:allow-open`/`reveal-item-in-dir`, and the scoped asset read. Minimal —
  no broad `fs`.
- **`ManifestSummary`** is a UI-shell display type (not core/ts_rs).
- **Plan must confirm** Tauri 2's reveal API name (`opener` →
  `reveal_item_in_dir`); fall back to opening the parent dir if absent.

## 11. Testing & verification

UI-layer (core behavior already proven by 3.A/B/C `run_with_transport`
guardrails — 3.D adds no core behavior):
- **Unit (vitest + `@testing-library/svelte`):** `stores/attachments.ts`
  (update fns, selector, hex keying, immutable updates — mirror
  `delivery.test.ts`); `lib/attachments.ts` helpers; `FileAttachmentBubble`
  state rendering (each state shows the right affordances; preview only when
  `complete && isImage`; Open/Reveal only with a `path`).
- **E2E (Playwright + `TAURI_MOCK=1`):** extend `src/lib/test/tauri-mock.ts` to
  mock `pick_file`/`file_size`/`decode_attachment_manifest`/`open_file`/
  `reveal_in_folder` and emit attachment events on the subscribe channel; drive
  attach→size-gate→`SendFile`→`FileQueued`→bubble, then inject progress +
  received and assert bar→preview/Open. Add a >100 MiB block case and a
  `AttachmentFailed` case.
- **Rust shell (`cargo test -p skattr-ui`):** `decode_attachment_manifest`
  round-trip against a real `AttachmentManifest`; `file_size`; path validation
  in `open_file`/`reveal_in_folder`.
- **CI:** add **`pnpm test` (vitest) to the `ui` job** as a hard gate (the job
  currently runs `pnpm build` + `clippy`/`cargo build`/`cargo test` for
  `skattr-ui` but not `pnpm test`). Playwright e2e is local-required /
  CI-best-effort (needs `pnpm exec playwright install`).

Success = `ui` job green (`pnpm build`, `pnpm test`, `clippy -p skattr-ui -D
warnings`, `cargo test -p skattr-ui`) + the e2e mock flow passing locally.

## 12. Deferred / explicitly-not-3.D

- **Post-restart received-attachment state** — the session-scoped store is empty
  after an app restart; an incoming `Kind::File` bubble with no store entry shows
  a **static file card** (filename/size from the manifest) with no
  preview/Open/Reveal (the path isn't persisted UI-side and `download_dir` isn't
  readable). A `Command::AttachmentStatus` query (core) or a UI-side path cache
  would fix it — deferred (no core change in 3.D). Users find files via the OS in
  the downloads folder.
- **Configurable download folder** — needs core `ConfigSnapshot` (read) +
  `ConfigPatch` nullability (the 3.B `download_dir: string` ts_rs quirk) fixes;
  deferred with the read-gap.
- **In-UI retry** of a failed transfer — recovery is a resend.
- **Sender-side download progress** — not possible (no receiver→sender progress
  events in the pull/deposit model); the sender shows delivery status only.
- Concurrent attachments per peer; Phase 4 (release/signing).
