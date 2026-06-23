# Phase 3.D — Attachment UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface the already-shipped attachment core (3.A/B/C) in the desktop UI: pick & send a file, see it as a file message with delivery status, watch received files download with a progress bar, preview images inline, open/reveal received files, and get clear size-limit / failure feedback.

**Architecture:** Presentation + IPC wiring only, in the `skattr-ui` crate (Tauri 2 + SvelteKit). Four small `#[tauri::command]` shell functions (`decode_attachment_manifest`, `file_size`, `open_file`, `reveal_in_folder`) expose canonical Rust behavior to the webview; a global session-scoped Svelte store keyed by `attachment_id` reconciles three event sources (`FileQueued`, the decoded `Kind::File` manifest, receiver `Event::Attachment*`); a new `FileAttachmentBubble.svelte` renders it. **No core/protocol change, no ADR.**

**Tech Stack:** Tauri 2 (`=2.11.0`), `tauri-plugin-dialog` v2, `tauri-plugin-opener` v2, SvelteKit 2 / Svelte 5 (runes), vitest 2 + `@testing-library/svelte`, Playwright 1.47 (`TAURI_MOCK=1`), `cargo test -p skattr-ui`.

## Global Constraints

- **License header on every new `.rs` / `.svelte` / `.ts` file.** Rust/TS: `// SPDX-License-Identifier: GPL-3.0-or-later` then `// Copyright (C) 2026 Myggiz AB`. Svelte: `<!-- SPDX-License-Identifier: GPL-3.0-or-later -->` then `<!-- Copyright (C) 2026 Myggiz AB -->`.
- **No `unwrap()` / `expect()` in shell command bodies.** Return `Result<T, String>`; map errors with `.map_err(|e| format!("…: {e}"))`. (`#[cfg(test)]` modules may use `unwrap` under the existing `#[allow(clippy::unwrap_used, …)]` attribute — mirror `main.rs:305-306`.)
- **CSS uses house custom properties only:** `--bg`, `--bg-elevated`, `--text`, `--text-muted`, `--accent`, `--danger`, `--s-1`/`--s-2`/`--s-3` (spacing), `--t-ui`/`--t-display` (type). No hard-coded colors except the existing `rgba(255,255,255,0.7)` precedent.
- **`manifest` is a runtime byte array.** `Kind::File.manifest` is Rust `Vec<u8>` (no `serde_bytes`); serde_json serializes it as a JSON **number array**, even though the generated TS type says `manifest: string` (a `#[ts(type="string")]` annotation that does not match runtime). The UI passes `record.kind.manifest` straight to the Rust decode command (param type `Vec<u8>`), casting through `unknown` in TS. **Never** base64-decode it in JS.
- **`attachment_id` is the universal key.** Hex-encoded via the existing `hex16ToString` helper (`src/lib/stores/delivery.ts`). The attachments store, bubble correlation, and all three event sources key on it.
- **Daemon caps are authoritative:** `MAX_ATTACHMENT_BYTES = 100 MiB` (hard), `MAX_OFFLINE_ATTACHMENT_BYTES = 10 MiB` (offline lane). The UI size gate is convenience only.
- **ts-rs types are generated — never hand-edit** files under `src/lib/ipc/types/`. They already include the attachment `Event` variants, `Kind::File`, `Command::SendFile`, `CommandResult::FileQueued`.
- **Pin compatibility:** Tauri Rust deps pin to the existing `tauri = "=2.11.0"` / `tauri-build = "=2.6.0"` line — use `tauri-plugin-dialog = "2"` / `tauri-plugin-opener = "2"` (the `2` caret resolves a compatible release). JS plugins: `@tauri-apps/plugin-dialog@^2` / `@tauri-apps/plugin-opener@^2`.

---

## File Structure

**New Rust (`crates/ui/src/`):**
- `attachments.rs` — the four shell commands + `ManifestSummary` display type + `#[cfg(test)]` tests.

**Modified Rust:**
- `crates/ui/src/main.rs` — `mod attachments;`, register 4 commands in `generate_handler!`, add the two plugins, register the asset-protocol scope for `<data_dir>/downloads` in `setup()`.
- `crates/ui/Cargo.toml` — add `tauri-plugin-dialog`, `tauri-plugin-opener`; add `"protocol-asset"` to `tauri` features.

**New Tauri config:**
- `crates/ui/capabilities/default.json` — capability granting the window the core + dialog + opener permissions (no capabilities file exists today; Tauri auto-includes everything under `capabilities/`).

**Modified Tauri config:**
- `crates/ui/tauri.conf.json` — CSP `img-src` adds `asset: http://asset.localhost`; `app.security.assetProtocol` enabled; register `dialog` + `opener` plugin config stanzas.

**New SvelteKit (`crates/ui/src-svelte/src/lib/`):**
- `stores/attachments.ts` + `stores/attachments.test.ts` — global live-transfer store.
- `attachments.ts` + `attachments.test.ts` — formatBytes / mime helpers / decode wrapper + per-message memo.
- `components/FileAttachmentBubble.svelte` + `components/FileAttachmentBubble.test.ts`.
- `icons/file.svg`, `icons/image.svg`, `icons/paperclip.svg` (Lucide ISC SVGs) registered in `icons/index.ts`.

**Modified SvelteKit:**
- `components/MessageBubble.svelte` — switch on `record.kind.kind`: `file` → `<FileAttachmentBubble>`.
- `components/Composer.svelte` — paperclip attach button → picker → size gate → send.
- `stores/conversation.ts` — `sendFile()` + an optimistic `Kind::File` placeholder variant.
- `routes/+page.svelte` — 3 new dispatcher arms.
- `src/lib/test/tauri-mock.ts` — mock the 4 new commands + a `?fixture=attachments` flow.
- `package.json` — add the two JS plugins.

**New tests:**
- `tests/e2e/attachments.spec.ts`.

**Modified CI:**
- `.github/workflows/ci.yml` — add `pnpm test` (vitest) hard-gate to the `ui` job.

---

## Task 1: Rust shell — `decode_attachment_manifest` + `ManifestSummary`

**Files:**
- Create: `crates/ui/src/attachments.rs`
- Test: same file, `#[cfg(test)]` module.

**Interfaces:**
- Consumes: `skattr_core::attachment::manifest::AttachmentManifest::{to_cbor, from_cbor}` — `pub fn from_cbor(bytes: &[u8]) -> Result<Self>`; fields `manifest_version: u8`, `attachment_id: [u8;16]`, `filename: String`, `mime: String`, `total_size: u64`, `chunk_size: u32`. (Confirm `AttachmentManifest` is reachable: `skattr_core::attachment` is `pub(crate)` inside core — check `crates/core/src/lib.rs` / `crates/core/src/attachment/mod.rs` for a `pub use`. If not public, add `pub use attachment::manifest::AttachmentManifest;` to core's public surface in this task, since 3.D is explicitly allowed the "one Rust touch … reads a manifest via the existing `pub` `AttachmentManifest::from_cbor`".)
- Produces: `#[tauri::command] pub async fn decode_attachment_manifest(manifest: Vec<u8>) -> Result<ManifestSummary, String>`; `pub struct ManifestSummary { attachment_id: String /*hex*/, filename: String, mime: String, total_size: u64 }`.

- [ ] **Step 1: Confirm manifest reachability from skattr-ui**

Run: `grep -rn "pub use\|pub mod attachment\|pub(crate) mod attachment" crates/core/src/lib.rs crates/core/src/attachment/mod.rs`
Expected: determine whether `skattr_core::attachment::manifest::AttachmentManifest` is importable from `skattr-ui`. If the path is `pub(crate)`, add a re-export in core: in `crates/core/src/lib.rs` add `pub use attachment::manifest::AttachmentManifest;` (and ensure `from_cbor`/`to_cbor`/fields are `pub` — they are). Record the exact import path used below.

- [ ] **Step 2: Write the failing test**

Create `crates/ui/src/attachments.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 3.D attachment UI-shell commands: manifest decode, file stat,
//! and open/reveal of received files. Presentation-only — no protocol.

use serde::Serialize;

// NOTE: adjust this path to whatever Step 1 confirmed is public.
use skattr_core::attachment::manifest::AttachmentManifest;

/// Display-only projection of an [`AttachmentManifest`] for the file bubble.
/// Not a ts-rs/core type — lives entirely in the UI shell.
#[derive(Debug, Clone, Serialize)]
pub struct ManifestSummary {
    /// Hex-encoded 16-byte attachment id (matches `hex16ToString` keys).
    pub attachment_id: String,
    /// Sanitized filename from the manifest.
    pub filename: String,
    /// MIME type (post metadata-strip).
    pub mime: String,
    /// Plaintext total size in bytes.
    pub total_size: u64,
}

/// Decode a `Kind::File` manifest (raw CBOR bytes as serialized by serde_json:
/// a JSON number array) into the four scalar display fields. Rejects unknown
/// manifest versions via the canonical core decoder.
#[tauri::command]
pub async fn decode_attachment_manifest(manifest: Vec<u8>) -> Result<ManifestSummary, String> {
    let m = AttachmentManifest::from_cbor(&manifest).map_err(|e| format!("decode manifest: {e}"))?;
    Ok(ManifestSummary {
        attachment_id: hex::encode(m.attachment_id),
        filename: m.filename,
        mime: m.mime,
        total_size: m.total_size,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn decode_roundtrips_a_real_manifest() {
        // Build a real manifest via core's own constructor path. If a test
        // helper exists (e.g. AttachmentManifest::new / a chunker output),
        // prefer it; otherwise construct the struct literal directly.
        let manifest = AttachmentManifest {
            manifest_version: skattr_core::attachment::MANIFEST_VERSION,
            attachment_id: [0xab; 16],
            filename: "photo.jpg".to_string(),
            mime: "image/jpeg".to_string(),
            total_size: 1234,
            chunk_size: 49152,
            file_key: [0u8; 32],
            chunks: vec![],
        };
        let bytes = manifest.to_cbor().unwrap();
        let summary = decode_attachment_manifest(bytes).await.unwrap();
        assert_eq!(summary.attachment_id, "ab".repeat(16));
        assert_eq!(summary.filename, "photo.jpg");
        assert_eq!(summary.mime, "image/jpeg");
        assert_eq!(summary.total_size, 1234);
    }

    #[tokio::test]
    async fn decode_rejects_garbage() {
        let err = decode_attachment_manifest(vec![0xff, 0x00, 0x13]).await.unwrap_err();
        assert!(err.contains("decode manifest"));
    }
}
```

> If `AttachmentManifest`'s struct literal can't be built from the shell (private fields like `file_key`/`chunks`), use the public constructor the chunker exposes, or add a `#[cfg(feature = "test-harness")] pub fn for_test(...)`. Confirm field visibility in Step 1 and adjust the literal accordingly — do **not** leave a non-compiling literal.

- [ ] **Step 3: Wire `hex` + `tokio` test macro deps**

`hex` and `tokio` (with `macros`, `rt`) must be available to `skattr-ui`. Check `crates/ui/Cargo.toml`:
Run: `grep -n "hex\|tokio" crates/ui/Cargo.toml`
Expected: `tokio = { workspace = true }` is present (it is). If `hex` is absent, add `hex = "0.4"` to `[dependencies]`. For `#[tokio::test]`, ensure `tokio` features include `macros` and `rt` — the workspace `tokio` likely has `full`; if not, add `tokio = { workspace = true, features = ["macros", "rt"] }`. (Alternatively, encode hex inline without the `hex` crate using `m.attachment_id.iter().map(|b| format!("{b:02x}")).collect::<String>()` to avoid a new dep — prefer this if adding `hex` triggers a cargo-deny review.)

Decision: **use the inline `format!("{b:02x}")` collect to avoid a new dependency.** Replace `hex::encode(m.attachment_id)` with:
```rust
attachment_id: m.attachment_id.iter().map(|b| format!("{b:02x}")).collect::<String>(),
```
and drop the `hex` import.

- [ ] **Step 4: Add `mod attachments;` so the test compiles**

In `crates/ui/src/main.rs`, after `mod ipc_bridge;` (line 12), add:
```rust
mod attachments;
```

- [ ] **Step 5: Run the test to verify it fails (then passes once compiling)**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-ui attachments:: 2>&1 | tail -30`
Expected: first run may fail to compile until Step 1's import path is correct; once it compiles, both tests PASS. Iterate on the import path / struct literal until green. **Do not proceed until both `decode_roundtrips_a_real_manifest` and `decode_rejects_garbage` pass.**

- [ ] **Step 6: Commit**

```bash
git add crates/ui/src/attachments.rs crates/ui/src/main.rs crates/core/src/lib.rs crates/ui/Cargo.toml
git commit -m "feat(3.D): decode_attachment_manifest shell command + ManifestSummary"
```

---

## Task 2: Rust shell — `file_size`

**Files:**
- Modify: `crates/ui/src/attachments.rs`

**Interfaces:**
- Produces: `#[tauri::command] pub async fn file_size(path: String) -> Result<u64, String>` — stats a regular file, returns its byte length; errors if missing / not a regular file.

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `crates/ui/src/attachments.rs`:
```rust
    #[tokio::test]
    async fn file_size_reports_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("blob.bin");
        std::fs::write(&p, vec![7u8; 99]).unwrap();
        let n = file_size(p.to_string_lossy().to_string()).await.unwrap();
        assert_eq!(n, 99);
    }

    #[tokio::test]
    async fn file_size_errors_on_missing() {
        let err = file_size("/no/such/file/xyz".to_string()).await.unwrap_err();
        assert!(err.contains("file_size"));
    }

    #[tokio::test]
    async fn file_size_errors_on_directory() {
        let dir = tempfile::tempdir().unwrap();
        let err = file_size(dir.path().to_string_lossy().to_string()).await.unwrap_err();
        assert!(err.contains("not a regular file"));
    }
```
(`tempfile` is already a dev-dependency — see `crates/ui/Cargo.toml`.)

- [ ] **Step 2: Run test, verify it fails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-ui attachments::tests::file_size 2>&1 | tail -20`
Expected: FAIL — `file_size` not found.

- [ ] **Step 3: Implement**

Add to `crates/ui/src/attachments.rs` (after `decode_attachment_manifest`):
```rust
/// Stat a local file and return its byte length. Used by the pre-send size
/// gate. Rejects non-existent paths and non-regular files (dirs, symdirs).
#[tauri::command]
pub async fn file_size(path: String) -> Result<u64, String> {
    let meta = std::fs::metadata(&path).map_err(|e| format!("file_size {path}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("file_size {path}: not a regular file"));
    }
    Ok(meta.len())
}
```

- [ ] **Step 4: Run tests, verify pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-ui attachments::tests::file_size 2>&1 | tail -20`
Expected: all three `file_size_*` tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src/attachments.rs
git commit -m "feat(3.D): file_size shell command for pre-send size gate"
```

---

## Task 3: Rust shell — `open_file` / `reveal_in_folder` + opener plugin

**Files:**
- Modify: `crates/ui/src/attachments.rs`, `crates/ui/Cargo.toml`

**Interfaces:**
- Consumes: `tauri-plugin-opener` (Rust). Confirm the exact API: the plugin exposes `tauri_plugin_opener::OpenerExt` with `open_path(path, with: Option<&str>)` and `reveal_item_in_dir(path)`. Verify names in Step 1.
- Produces: `#[tauri::command] pub async fn open_file(app, path: String) -> Result<(), String>`; `#[tauri::command] pub async fn reveal_in_folder(app, path: String) -> Result<(), String>`. Both canonicalize, assert exists + regular file, then delegate to the opener plugin. (`app: tauri::AppHandle`.)

- [ ] **Step 1: Confirm the opener Rust API**

Run: `. "$HOME/.cargo/env" && cargo add tauri-plugin-opener@2 -p skattr-ui --dry-run 2>&1 | tail -5` then add for real:
```bash
. "$HOME/.cargo/env" && cargo add tauri-plugin-opener@2 -p skattr-ui
```
Then confirm the trait + method names:
Run: `. "$HOME/.cargo/env" && grep -rn "pub fn reveal_item_in_dir\|pub fn open_path\|pub trait OpenerExt" ~/.cargo/registry/src/*/tauri-plugin-opener-*/src/ 2>/dev/null | head`
Expected: confirms `OpenerExt::open_path` and `OpenerExt::reveal_item_in_dir` signatures. If `reveal_item_in_dir` is absent in this version, fall back to opening the parent directory via `open_path(parent_dir, None)`. Record the real signatures here before writing code.

- [ ] **Step 2: Write the failing tests** (path validation is the unit-testable part)

Append to the `tests` module:
```rust
    #[test]
    fn validate_existing_regular_file_ok() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        std::fs::write(&p, b"hi").unwrap();
        let got = validate_openable(&p.to_string_lossy()).unwrap();
        assert!(got.is_absolute());
    }

    #[test]
    fn validate_missing_file_errs() {
        let err = validate_openable("/no/such/zzz").unwrap_err();
        assert!(err.contains("not found") || err.contains("canonicalize"));
    }

    #[test]
    fn validate_directory_errs() {
        let dir = tempfile::tempdir().unwrap();
        let err = validate_openable(&dir.path().to_string_lossy()).unwrap_err();
        assert!(err.contains("not a regular file"));
    }
```

- [ ] **Step 3: Run, verify fail**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-ui attachments::tests::validate 2>&1 | tail -20`
Expected: FAIL — `validate_openable` not found.

- [ ] **Step 4: Implement the validator + the two commands**

Add to `crates/ui/src/attachments.rs`:
```rust
use std::path::PathBuf;
use tauri_plugin_opener::OpenerExt;

/// Canonicalize a UI-supplied path and assert it points at an existing
/// regular file. Defense-in-depth: received-file paths are always
/// daemon-authored (from `Event::AttachmentReceived`), but validate anyway.
fn validate_openable(path: &str) -> Result<PathBuf, String> {
    let canon = std::fs::canonicalize(path).map_err(|e| format!("canonicalize {path}: {e}"))?;
    let meta = std::fs::metadata(&canon).map_err(|e| format!("{path}: not found: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{path}: not a regular file"));
    }
    Ok(canon)
}

/// Open a received file with the OS default handler.
#[tauri::command]
pub async fn open_file(app: tauri::AppHandle, path: String) -> Result<(), String> {
    let canon = validate_openable(&path)?;
    app.opener()
        .open_path(canon.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| format!("open_file: {e}"))
}

/// Reveal a received file in the OS file manager.
#[tauri::command]
pub async fn reveal_in_folder(app: tauri::AppHandle, path: String) -> Result<(), String> {
    let canon = validate_openable(&path)?;
    app.opener()
        .reveal_item_in_dir(canon)
        .map_err(|e| format!("reveal_in_folder: {e}"))
}
```
> Adjust `open_path` / `reveal_item_in_dir` calls to match the exact signatures confirmed in Step 1. If `reveal_item_in_dir` is unavailable, implement reveal as `open_path(parent_of(canon), None)` and leave a `// fallback: opener vN lacks reveal_item_in_dir` comment.

- [ ] **Step 5: Run, verify pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-ui attachments::tests::validate 2>&1 | tail -20`
Expected: all three `validate_*` tests PASS. (The command bodies aren't unit-tested — they need a running app; they're exercised by the e2e mock in Task 12 and clippy/compile here.)

- [ ] **Step 6: Clippy gate**

Run: `. "$HOME/.cargo/env" && cargo clippy -p skattr-ui --all-targets --all-features -- -D warnings 2>&1 | tail -20`
Expected: no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/ui/src/attachments.rs crates/ui/Cargo.toml
git commit -m "feat(3.D): open_file/reveal_in_folder shell commands + opener plugin"
```

---

## Task 4: Register commands, plugins, capabilities, asset protocol, CSP

**Files:**
- Modify: `crates/ui/src/main.rs`, `crates/ui/Cargo.toml`, `crates/ui/tauri.conf.json`
- Create: `crates/ui/capabilities/default.json`

**Interfaces:**
- Consumes: the 4 commands from Tasks 1–3.
- Produces: a fully-registered shell — `generate_handler!` includes the 4 commands; `dialog` + `opener` plugins initialized; the asset-protocol scope allows `<data_dir>/downloads`; CSP permits `asset:` images.

- [ ] **Step 1: Add `tauri-plugin-dialog` + `protocol-asset` feature to Cargo.toml**

```bash
. "$HOME/.cargo/env" && cargo add tauri-plugin-dialog@2 -p skattr-ui
```
Then edit `crates/ui/Cargo.toml` — change the `tauri` line to enable the asset protocol:
```toml
tauri = { version = "=2.11.0", features = ["tray-icon", "protocol-asset"] }
```
(`tauri-plugin-opener` was added in Task 3.)

- [ ] **Step 2: Register plugins + commands in `main.rs`**

In `crates/ui/src/main.rs`, in the `tauri::Builder` chain (after `.plugin(tauri_plugin_deep_link::init())`, line 172), add:
```rust
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
```
Extend the `generate_handler!` macro (lines 173-184) to add the 4 commands — insert before the closing `]`:
```rust
            attachments::decode_attachment_manifest,
            attachments::file_size,
            attachments::open_file,
            attachments::reveal_in_folder,
```

- [ ] **Step 3: Register the asset-protocol scope at runtime in `setup()`**

In `main.rs` `setup()`, the download dir is `<data_dir>/downloads`. After `*state.data_dir.write() = Some(data_dir);` (line 196), add:
```rust
            // Scope the asset protocol to the daemon's downloads dir so the
            // webview can lazily stream received images via convertFileSrc.
            // The dir is created lazily by the daemon on first receive; create
            // it now so the scope grant targets an existing path.
            let downloads = data_dir.join("downloads");
            std::fs::create_dir_all(&downloads).ok();
            app.asset_protocol_scope().allow_directory(&downloads, true)
                .map_err(|e| format!("asset scope: {e}"))?;
```
> Confirm the API: in Tauri 2, `tauri::Manager::asset_protocol_scope()` returns a scope handle with `allow_directory(path, recursive)`. Verify with:
> Run: `grep -rn "fn asset_protocol_scope\|fn allow_directory" ~/.cargo/registry/src/*/tauri-2.11.0/src/ 2>/dev/null | head`
> If the signature differs (e.g. returns `()` on error vs `Result`), adapt the `.map_err` accordingly. `data_dir` is moved into managed state above — clone it (`let data_dir2 = data_dir.clone();`) before the `Some(data_dir)` move if the borrow checker complains, and build `downloads` from the clone.

- [ ] **Step 4: Create the capability file**

Create `crates/ui/capabilities/default.json`:
```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "description": "Core window + attachment dialog/opener/asset permissions for the main window.",
  "windows": ["main"],
  "permissions": [
    "core:default",
    "dialog:allow-open",
    "opener:allow-open-path",
    "opener:allow-reveal-item-in-dir"
  ]
}
```
> Confirm permission identifiers against the generated ACL once plugins are added. Run `grep -rn "allow-open-path\|allow-reveal-item-in-dir\|allow-open" crates/ui/gen/schemas/*.json` after a build; if the opener permission is named `opener:allow-open-url` / `opener:default`, use the names the generated schema lists. The asset-protocol read is granted by the runtime scope (Step 3) + `protocol-asset` feature, not a capability permission.

- [ ] **Step 5: Update `tauri.conf.json` — CSP, asset protocol, plugin config**

In `crates/ui/tauri.conf.json`, replace the `app.security` block (line 23-25) with:
```json
    "security": {
      "csp": "default-src 'self'; img-src 'self' data: asset: http://asset.localhost; font-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self' ipc: tauri:; script-src 'self'",
      "assetProtocol": {
        "enable": true,
        "scope": []
      }
    }
```
(The `scope: []` static entry is intentionally empty — the real grant is the runtime `allow_directory` in Step 3, which targets the dynamic per-install downloads dir.)

In the `plugins` object (line ~298), add `dialog` and `opener` keys alongside the existing `updater`/`deep-link`:
```json
    "dialog": {},
    "opener": {}
```

- [ ] **Step 6: Build the shell**

Run: `. "$HOME/.cargo/env" && cargo build -p skattr-ui 2>&1 | tail -25`
Expected: clean build. Fix any plugin-init / scope-API mismatches surfaced here.

- [ ] **Step 7: Clippy + test gate**

Run: `. "$HOME/.cargo/env" && cargo clippy -p skattr-ui --all-targets --all-features -- -D warnings && cargo test -p skattr-ui --all-targets 2>&1 | tail -25`
Expected: no warnings; all Rust tests (incl. Task 1–3) PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/ui/src/main.rs crates/ui/Cargo.toml crates/ui/tauri.conf.json crates/ui/capabilities/default.json
git commit -m "feat(3.D): register attachment commands, dialog/opener plugins, asset-protocol scope"
```

---

## Task 5: JS plugin dependencies

**Files:**
- Modify: `crates/ui/src-svelte/package.json`

**Interfaces:**
- Produces: `@tauri-apps/plugin-dialog` (`open`) and `@tauri-apps/plugin-opener` available to JS; `convertFileSrc` is already in `@tauri-apps/api/core`.

- [ ] **Step 1: Add the JS plugins**

```bash
cd crates/ui/src-svelte && pnpm add @tauri-apps/plugin-dialog@^2 @tauri-apps/plugin-opener@^2
```
> If pnpm isn't on PATH, activate via corepack: `corepack enable && corepack prepare pnpm@10 --activate` (matches CI).

- [ ] **Step 2: Verify lockfile + types resolve**

Run: `cd crates/ui/src-svelte && pnpm install --frozen-lockfile 2>&1 | tail -5 && pnpm build 2>&1 | tail -15`
Expected: install clean against the updated lockfile; build succeeds (no missing-module errors — though nothing imports them yet).

- [ ] **Step 3: Commit**

```bash
git add crates/ui/src-svelte/package.json crates/ui/src-svelte/pnpm-lock.yaml
git commit -m "build(3.D): add @tauri-apps/plugin-dialog + plugin-opener JS deps"
```

---

## Task 6: `stores/attachments.ts` — global transfer store

**Files:**
- Create: `crates/ui/src-svelte/src/lib/stores/attachments.ts`
- Test: `crates/ui/src-svelte/src/lib/stores/attachments.test.ts`

**Interfaces:**
- Consumes: `hex16ToString` from `./delivery`; `Hex16` from `$lib/ipc/types`.
- Produces:
  - `type AttachmentStatus = "queued" | "sending" | "receiving" | "complete" | "failed"`
  - `interface AttachmentState { status; received: number; total: number; filename?: string; mime?: string; size?: number; path?: string; reason?: string }`
  - `const attachments: Writable<Map<string, AttachmentState>>`
  - `markQueued(aidHex, info: { filename?; size?; total?: number })`
  - `applyManifest(aidHex, info: { filename; mime; size; total })`
  - `applyProgress(aidHex, received: number, total: number)`
  - `applyReceived(aidHex, info: { filename; mime; size; path })`
  - `applyFailed(aidHex, reason: string)`
  - `attachmentFor(aidHex): AttachmentState | undefined`

- [ ] **Step 1: Write the failing test**

Create `crates/ui/src-svelte/src/lib/stores/attachments.test.ts`:
```ts
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { describe, expect, test, beforeEach } from "vitest";
import { get } from "svelte/store";
import {
  attachments,
  markQueued,
  applyManifest,
  applyProgress,
  applyReceived,
  applyFailed,
  attachmentFor,
} from "./attachments";

describe("attachments store", () => {
  beforeEach(() => attachments.set(new Map()));

  test("markQueued seeds a queued entry", () => {
    markQueued("aa", { filename: "f.bin", size: 10, total: 3 });
    expect(attachmentFor("aa")).toEqual({
      status: "queued", received: 0, total: 3, filename: "f.bin", size: 10,
    });
  });

  test("applyManifest fills static fields without clobbering progress", () => {
    applyProgress("bb", 2, 5);
    applyManifest("bb", { filename: "p.jpg", mime: "image/jpeg", size: 99, total: 5 });
    const s = attachmentFor("bb")!;
    expect(s.received).toBe(2);
    expect(s.filename).toBe("p.jpg");
    expect(s.mime).toBe("image/jpeg");
    expect(s.status).toBe("receiving");
  });

  test("applyProgress sets receiving + counts", () => {
    applyProgress("cc", 1, 4);
    expect(attachmentFor("cc")).toMatchObject({ status: "receiving", received: 1, total: 4 });
  });

  test("applyReceived marks complete with path", () => {
    applyProgress("dd", 4, 4);
    applyReceived("dd", { filename: "x.png", mime: "image/png", size: 5, path: "/d/x.png" });
    expect(attachmentFor("dd")).toMatchObject({
      status: "complete", path: "/d/x.png", filename: "x.png", mime: "image/png", size: 5,
    });
  });

  test("applyFailed marks failed with reason", () => {
    applyProgress("ee", 1, 4);
    applyFailed("ee", "timeout");
    expect(attachmentFor("ee")).toMatchObject({ status: "failed", reason: "timeout" });
  });

  test("updates are immutable (new Map each time)", () => {
    markQueued("ff", { total: 1 });
    const first = get(attachments);
    applyProgress("ff", 1, 1);
    expect(get(attachments)).not.toBe(first);
  });
});
```

- [ ] **Step 2: Run, verify fail**

Run: `cd crates/ui/src-svelte && pnpm test src/lib/stores/attachments.test.ts 2>&1 | tail -20`
Expected: FAIL — module `./attachments` not found.

- [ ] **Step 3: Implement the store**

Create `crates/ui/src-svelte/src/lib/stores/attachments.ts`:
```ts
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { writable, get } from "svelte/store";

export type AttachmentStatus =
  | "queued"
  | "sending"
  | "receiving"
  | "complete"
  | "failed";

export interface AttachmentState {
  status: AttachmentStatus;
  received: number; // chunks (receiver-side)
  total: number; // chunks
  filename?: string;
  mime?: string;
  size?: number; // bytes (≤100 MiB → JS number is exact)
  path?: string; // local path once complete (receiver)
  reason?: string; // when failed
}

/**
 * Global, session-scoped live-transfer state keyed by hex attachment_id.
 * Decoupled from the active conversation so events that arrive before a
 * bubble mounts, during a conversation switch, or for a background
 * conversation are all recorded; the bubble reads current state on mount.
 * Cleared on app restart (the deferred restart case — see design §10/§12).
 */
export const attachments = writable<Map<string, AttachmentState>>(new Map());

function patch(aidHex: string, fn: (prev: AttachmentState) => AttachmentState): void {
  attachments.update((m) => {
    const next = new Map(m);
    const prev = next.get(aidHex) ?? { status: "queued" as AttachmentStatus, received: 0, total: 0 };
    next.set(aidHex, fn(prev));
    return next;
  });
}

export function markQueued(
  aidHex: string,
  info: { filename?: string; size?: number; total?: number },
): void {
  patch(aidHex, (prev) => ({
    ...prev,
    status: "queued",
    total: info.total ?? prev.total,
    filename: info.filename ?? prev.filename,
    size: info.size ?? prev.size,
  }));
}

export function applyManifest(
  aidHex: string,
  info: { filename: string; mime: string; size: number; total: number },
): void {
  patch(aidHex, (prev) => ({
    ...prev,
    filename: info.filename,
    mime: info.mime,
    size: info.size,
    total: prev.total || info.total,
  }));
}

export function applyProgress(aidHex: string, received: number, total: number): void {
  patch(aidHex, (prev) => ({
    ...prev,
    status: prev.status === "complete" ? "complete" : "receiving",
    received,
    total,
  }));
}

export function applyReceived(
  aidHex: string,
  info: { filename: string; mime: string; size: number; path: string },
): void {
  patch(aidHex, (prev) => ({
    ...prev,
    status: "complete",
    received: prev.total || prev.received,
    filename: info.filename,
    mime: info.mime,
    size: info.size,
    path: info.path,
  }));
}

export function applyFailed(aidHex: string, reason: string): void {
  patch(aidHex, (prev) => ({ ...prev, status: "failed", reason }));
}

export function attachmentFor(aidHex: string): AttachmentState | undefined {
  return get(attachments).get(aidHex);
}
```

- [ ] **Step 4: Run, verify pass**

Run: `cd crates/ui/src-svelte && pnpm test src/lib/stores/attachments.test.ts 2>&1 | tail -20`
Expected: all 6 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src-svelte/src/lib/stores/attachments.ts crates/ui/src-svelte/src/lib/stores/attachments.test.ts
git commit -m "feat(3.D): global attachments transfer store"
```

---

## Task 7: `lib/attachments.ts` helpers + decode wrapper

**Files:**
- Create: `crates/ui/src-svelte/src/lib/attachments.ts`
- Test: `crates/ui/src-svelte/src/lib/attachments.test.ts`

**Interfaces:**
- Consumes: `invoke` from `@tauri-apps/api/core`; `Kind` from `$lib/ipc/types`.
- Produces:
  - `formatBytes(n: number): string`
  - `isImage(mime: string | undefined): boolean`
  - `mimeIconName(mime: string | undefined): "image" | "file"` (icon registry keys from Task 8b)
  - `MANIFEST_SIZE_HARD = 104_857_600` (100 MiB), `MANIFEST_SIZE_SOFT = 10_485_760` (10 MiB)
  - `decodeManifest(manifest: Kind & { kind: "file" }): Promise<ManifestSummary>` where `interface ManifestSummary { attachment_id: string; filename: string; mime: string; total_size: number }`
  - `decodeManifestMemo(messageIdHex: string, manifest): Promise<ManifestSummary>` — per-message-id memo so each `Kind::File` bubble decodes once.

- [ ] **Step 1: Write the failing test**

Create `crates/ui/src-svelte/src/lib/attachments.test.ts`:
```ts
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { describe, expect, test, vi, beforeEach } from "vitest";

// Mock the Tauri core module before importing the SUT.
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

import {
  formatBytes,
  isImage,
  mimeIconName,
  decodeManifest,
  decodeManifestMemo,
  MANIFEST_SIZE_HARD,
  MANIFEST_SIZE_SOFT,
} from "./attachments";

describe("formatBytes", () => {
  test("scales units", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(1024)).toBe("1.0 KiB");
    expect(formatBytes(1_572_864)).toBe("1.5 MiB");
  });
});

describe("mime helpers", () => {
  test("isImage", () => {
    expect(isImage("image/png")).toBe(true);
    expect(isImage("application/pdf")).toBe(false);
    expect(isImage(undefined)).toBe(false);
  });
  test("mimeIconName", () => {
    expect(mimeIconName("image/jpeg")).toBe("image");
    expect(mimeIconName("text/plain")).toBe("file");
    expect(mimeIconName(undefined)).toBe("file");
  });
});

describe("size constants", () => {
  test("match daemon caps", () => {
    expect(MANIFEST_SIZE_HARD).toBe(100 * 1024 * 1024);
    expect(MANIFEST_SIZE_SOFT).toBe(10 * 1024 * 1024);
  });
});

describe("decodeManifest", () => {
  beforeEach(() => invokeMock.mockReset());

  test("passes raw bytes to the shell command and returns the summary", async () => {
    invokeMock.mockResolvedValue({
      attachment_id: "ab".repeat(16), filename: "p.jpg", mime: "image/jpeg", total_size: 5,
    });
    const out = await decodeManifest({ kind: "file", manifest: [1, 2, 3] as unknown as string });
    expect(invokeMock).toHaveBeenCalledWith("decode_attachment_manifest", { manifest: [1, 2, 3] });
    expect(out.filename).toBe("p.jpg");
  });

  test("memo decodes once per message id", async () => {
    invokeMock.mockResolvedValue({
      attachment_id: "cd".repeat(16), filename: "x", mime: "text/plain", total_size: 1,
    });
    const m = { kind: "file", manifest: [9] as unknown as string } as const;
    await decodeManifestMemo("msg1", m);
    await decodeManifestMemo("msg1", m);
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });
});
```

- [ ] **Step 2: Run, verify fail**

Run: `cd crates/ui/src-svelte && pnpm test src/lib/attachments.test.ts 2>&1 | tail -20`
Expected: FAIL — module `./attachments` not found.

- [ ] **Step 3: Implement**

Create `crates/ui/src-svelte/src/lib/attachments.ts`:
```ts
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { invoke } from "@tauri-apps/api/core";
import type { Kind } from "$lib/ipc/types";

/** Daemon hard cap (100 MiB) — > this is blocked pre-send. */
export const MANIFEST_SIZE_HARD = 100 * 1024 * 1024;
/** Offline-lane cap (10 MiB) — 10–100 MiB is soft-warned. */
export const MANIFEST_SIZE_SOFT = 10 * 1024 * 1024;

export interface ManifestSummary {
  attachment_id: string; // hex, matches hex16ToString keys
  filename: string;
  mime: string;
  total_size: number;
}

/** Human-readable binary size. */
export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KiB", "MiB", "GiB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v.toFixed(1)} ${units[i]}`;
}

export function isImage(mime: string | undefined): boolean {
  return typeof mime === "string" && mime.startsWith("image/");
}

export function mimeIconName(mime: string | undefined): "image" | "file" {
  return isImage(mime) ? "image" : "file";
}

/**
 * Decode a `Kind::File` manifest via the canonical Rust shell command.
 *
 * `manifest` is declared `string` by ts-rs but is a runtime number[] (the
 * serde_json serialization of the core `Vec<u8>` field). We pass it through
 * untouched; the Rust command param is `Vec<u8>`. Never base64-decode here.
 */
export async function decodeManifest(
  fileKind: Extract<Kind, { kind: "file" }>,
): Promise<ManifestSummary> {
  const manifest = fileKind.manifest as unknown as number[];
  return await invoke<ManifestSummary>("decode_attachment_manifest", { manifest });
}

const _memo = new Map<string, Promise<ManifestSummary>>();

/** Decode-once-per-message-id memo (avoids re-decoding on every re-render). */
export function decodeManifestMemo(
  messageIdHex: string,
  fileKind: Extract<Kind, { kind: "file" }>,
): Promise<ManifestSummary> {
  const hit = _memo.get(messageIdHex);
  if (hit) return hit;
  const p = decodeManifest(fileKind);
  _memo.set(messageIdHex, p);
  // On rejection, drop from memo so a later mount can retry.
  p.catch(() => _memo.delete(messageIdHex));
  return p;
}
```

- [ ] **Step 4: Run, verify pass**

Run: `cd crates/ui/src-svelte && pnpm test src/lib/attachments.test.ts 2>&1 | tail -20`
Expected: all tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src-svelte/src/lib/attachments.ts crates/ui/src-svelte/src/lib/attachments.test.ts
git commit -m "feat(3.D): attachment UI helpers + canonical manifest decode wrapper"
```

---

## Task 8: Icons (paperclip / file / image)

**Files:**
- Create: `crates/ui/src-svelte/src/lib/icons/paperclip.svg`, `file.svg`, `image.svg`
- Modify: `crates/ui/src-svelte/src/lib/icons/index.ts`

**Interfaces:**
- Produces: `icons["paperclip"]`, `icons["file"]`, `icons["image"]` (raw SVG strings), `IconName` widened.

- [ ] **Step 1: Add the three Lucide SVGs** (ISC-licensed, matching the existing bundled set)

Create `crates/ui/src-svelte/src/lib/icons/paperclip.svg`:
```svg
<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l8.57-8.57A4 4 0 1 1 18 8.84l-8.59 8.57a2 2 0 0 1-2.83-2.83l8.49-8.48"/></svg>
```
Create `crates/ui/src-svelte/src/lib/icons/file.svg`:
```svg
<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"/><path d="M14 2v4a2 2 0 0 0 2 2h4"/></svg>
```
Create `crates/ui/src-svelte/src/lib/icons/image.svg`:
```svg
<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="18" height="18" x="3" y="3" rx="2" ry="2"/><circle cx="9" cy="9" r="2"/><path d="m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21"/></svg>
```

- [ ] **Step 2: Register in `index.ts`**

In `crates/ui/src-svelte/src/lib/icons/index.ts`, add imports after the existing ones (line 14):
```ts
import paperclipSvg from "./paperclip.svg?raw";
import fileSvg from "./file.svg?raw";
import imageSvg from "./image.svg?raw";
```
Add to the `icons` object (before the closing `} as const;`):
```ts
  paperclip: paperclipSvg,
  file: fileSvg,
  image: imageSvg,
```

- [ ] **Step 3: Verify build resolves the `?raw` imports**

Run: `cd crates/ui/src-svelte && pnpm build 2>&1 | tail -10`
Expected: build succeeds.

- [ ] **Step 4: Commit**

```bash
git add crates/ui/src-svelte/src/lib/icons/
git commit -m "feat(3.D): add paperclip/file/image icons"
```

---

## Task 9: `FileAttachmentBubble.svelte` + tests

**Files:**
- Create: `crates/ui/src-svelte/src/lib/components/FileAttachmentBubble.svelte`
- Test: `crates/ui/src-svelte/src/lib/components/FileAttachmentBubble.test.ts`

**Interfaces:**
- Consumes: `MessageRecord`/`OptimisticMessage`; `attachments` store + `applyManifest`/`hex16ToString`; `decodeManifestMemo`/`isImage`/`mimeIconName`/`formatBytes`; `convertFileSrc` from `@tauri-apps/api/core`; `icons`; `DeliveryIcon` + `delivery`/`deliveryToIconStatus`/`hex16ToString` (for the sender delivery glyph).
- Produces: a component `FileAttachmentBubble` taking `{ record }` — renders the file card per §7 state matrix.

- [ ] **Step 1: Write the failing test**

Create `crates/ui/src-svelte/src/lib/components/FileAttachmentBubble.test.ts`:
```ts
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { describe, expect, test, vi, beforeEach } from "vitest";
import { render } from "@testing-library/svelte";
import { tick } from "svelte";

// convertFileSrc + invoke come from @tauri-apps/api/core.
const invokeMock = vi.fn();
const convertFileSrcMock = vi.fn((p: string) => `asset://localhost/${p}`);
vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
  convertFileSrc: convertFileSrcMock,
}));

import FileAttachmentBubble from "./FileAttachmentBubble.svelte";
import { attachments, applyReceived, applyProgress } from "$lib/stores/attachments";
import type { MessageRecord } from "$lib/ipc/types";

const AID = "ab".repeat(16);

function fileRecord(direction: "incoming" | "outgoing"): MessageRecord {
  return {
    row_id: 1n,
    message_id: "cd".repeat(16),
    contact: "ef".repeat(32),
    direction,
    kind: { kind: "file", manifest: [1, 2, 3] as unknown as string },
    mls_generation: 0n,
    ts_daemon_recv: 1_700_000_000n,
    ts_envelope: 1_700_000_000n,
  };
}

beforeEach(() => {
  attachments.set(new Map());
  invokeMock.mockReset();
  invokeMock.mockResolvedValue({
    attachment_id: AID, filename: "photo.jpg", mime: "image/jpeg", total_size: 2048,
  });
});

describe("FileAttachmentBubble", () => {
  test("renders the decoded filename as a static card", async () => {
    const { findByText } = render(FileAttachmentBubble, { props: { record: fileRecord("incoming") } });
    expect(await findByText("photo.jpg")).toBeTruthy();
  });

  test("shows a progress bar while receiving", async () => {
    const { container, findByText } = render(FileAttachmentBubble, {
      props: { record: fileRecord("incoming") },
    });
    await findByText("photo.jpg");
    applyProgress(AID, 1, 4);
    await tick();
    expect(container.querySelector(".progress")).not.toBeNull();
  });

  test("renders an inline <img> when complete + image", async () => {
    const { container, findByText } = render(FileAttachmentBubble, {
      props: { record: fileRecord("incoming") },
    });
    await findByText("photo.jpg");
    applyReceived(AID, { filename: "photo.jpg", mime: "image/jpeg", size: 2048, path: "/dl/photo.jpg" });
    await tick();
    const img = container.querySelector("img");
    expect(img).not.toBeNull();
    expect(convertFileSrcMock).toHaveBeenCalledWith("/dl/photo.jpg");
  });

  test("complete + non-image shows Open/Reveal, no img", async () => {
    invokeMock.mockResolvedValue({
      attachment_id: AID, filename: "doc.pdf", mime: "application/pdf", total_size: 10,
    });
    const { container, findByText, getByRole } = render(FileAttachmentBubble, {
      props: { record: fileRecord("incoming") },
    });
    await findByText("doc.pdf");
    applyReceived(AID, { filename: "doc.pdf", mime: "application/pdf", size: 10, path: "/dl/doc.pdf" });
    await tick();
    expect(container.querySelector("img")).toBeNull();
    expect(getByRole("button", { name: /open/i })).toBeTruthy();
    expect(getByRole("button", { name: /reveal/i })).toBeTruthy();
  });

  test("outgoing bubble shows a delivery icon and no progress bar", async () => {
    const { container, findByText } = render(FileAttachmentBubble, {
      props: { record: fileRecord("outgoing") },
    });
    await findByText("photo.jpg");
    expect(container.querySelector(".progress")).toBeNull();
    expect(container.querySelector(".icon")).not.toBeNull();
  });

  test("decode failure shows the unavailable card", async () => {
    invokeMock.mockRejectedValue(new Error("bad version"));
    const { findByText } = render(FileAttachmentBubble, { props: { record: fileRecord("incoming") } });
    expect(await findByText(/unavailable/i)).toBeTruthy();
  });
});
```

- [ ] **Step 2: Run, verify fail**

Run: `cd crates/ui/src-svelte && pnpm test src/lib/components/FileAttachmentBubble.test.ts 2>&1 | tail -20`
Expected: FAIL — component not found.

- [ ] **Step 3: Implement the component**

Create `crates/ui/src-svelte/src/lib/components/FileAttachmentBubble.svelte`:
```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { invoke, convertFileSrc } from "@tauri-apps/api/core";
  import type { MessageRecord } from "$lib/ipc/types";
  import type { OptimisticMessage } from "$lib/stores/conversation";
  import { attachments, applyManifest } from "$lib/stores/attachments";
  import { delivery, deliveryToIconStatus, hex16ToString } from "$lib/stores/delivery";
  import { decodeManifestMemo, isImage, mimeIconName, formatBytes } from "$lib/attachments";
  import type { ManifestSummary } from "$lib/attachments";
  import { icons } from "$lib/icons";
  import { toast } from "$lib/stores/toast";
  import DeliveryIcon from "./DeliveryIcon.svelte";

  let { record }: { record: MessageRecord | OptimisticMessage } = $props();

  let isOutgoing = $derived(record.direction === "outgoing");
  // The optimistic outgoing path carries display fields directly (Task 10).
  let optimisticName = $derived((record as OptimisticMessage).__attachName as string | undefined);
  let optimisticSize = $derived((record as OptimisticMessage).__attachSize as number | undefined);

  let summary = $state<ManifestSummary | null>(null);
  let decodeFailed = $state(false);
  let imgBroken = $state(false);

  // Decode the manifest once per message id; on success seed the store's
  // static fields so the bubble can render filename/size even if no transfer
  // events have arrived yet.
  $effect(() => {
    if (record.kind.kind !== "file") return;
    const fileKind = record.kind;
    const mid = hex16ToString(record.message_id);
    decodeManifestMemo(mid, fileKind)
      .then((s) => {
        summary = s;
        applyManifest(s.attachment_id, {
          filename: s.filename, mime: s.mime, size: s.total_size, total: 0,
        });
      })
      .catch(() => (decodeFailed = true));
  });

  let aidHex = $derived(summary ? summary.attachment_id : null);
  let state = $derived(aidHex ? $attachments.get(aidHex) : undefined);

  // Display fields: prefer decoded manifest, fall back to optimistic send info.
  let filename = $derived(summary?.filename ?? optimisticName ?? "");
  let mime = $derived(summary?.mime);
  let size = $derived(summary?.total_size ?? optimisticSize);

  let receiving = $derived(!isOutgoing && state?.status === "receiving");
  let complete = $derived(!isOutgoing && state?.status === "complete" && !!state?.path);
  let failed = $derived(!isOutgoing && state?.status === "failed");
  let showImage = $derived(complete && isImage(mime) && !imgBroken);

  let pct = $derived(
    state && state.total > 0 ? Math.round((state.received / state.total) * 100) : 0,
  );
  let indeterminate = $derived(receiving && (!state || state.total === 0));

  let deliveryStatus = $derived(
    isOutgoing ? deliveryToIconStatus($delivery.get(hex16ToString(record.message_id))) : null,
  );

  async function doOpen() {
    if (!state?.path) return;
    try {
      await invoke("open_file", { path: state.path });
    } catch {
      toast.show("File not found");
    }
  }
  async function doReveal() {
    if (!state?.path) return;
    try {
      await invoke("reveal_in_folder", { path: state.path });
    } catch {
      toast.show("File not found");
    }
  }

  let iconGlyph = $derived(icons[mimeIconName(mime)]);
</script>

<div class="file-bubble" class:outgoing={isOutgoing} data-row-id={record.row_id}>
  {#if decodeFailed}
    <div class="card">
      <span class="ficon">{@html icons["paperclip"]}</span>
      <span class="fname">📎 Attachment (unavailable)</span>
    </div>
  {:else if showImage && state?.path}
    <img
      class="preview"
      src={convertFileSrc(state.path)}
      alt={filename}
      onerror={() => (imgBroken = true)}
    />
    <div class="card">
      <span class="fname">{filename}</span>
      {#if size !== undefined}<span class="fsize">{formatBytes(size)}</span>{/if}
      <div class="actions">
        <button type="button" onclick={doOpen} aria-label="Open">Open</button>
        <button type="button" onclick={doReveal} aria-label="Reveal in folder">Reveal</button>
      </div>
    </div>
  {:else}
    <div class="card">
      <span class="ficon">{@html iconGlyph}</span>
      <span class="fname">{filename}</span>
      {#if size !== undefined}<span class="fsize">{formatBytes(size)}</span>{/if}
      {#if isOutgoing && deliveryStatus}
        <DeliveryIcon status={deliveryStatus} />
      {/if}
      {#if complete}
        <div class="actions">
          <button type="button" onclick={doOpen} aria-label="Open">Open</button>
          <button type="button" onclick={doReveal} aria-label="Reveal in folder">Reveal</button>
        </div>
      {/if}
      {#if failed}
        <span class="failed">⚠️ {state?.reason ?? "Transfer failed"}</span>
      {/if}
    </div>
    {#if receiving}
      <div class="progress" class:indeterminate>
        {#if indeterminate}
          <span class="label">Downloading…</span>
        {:else}
          <div class="bar" style={`width:${pct}%`}></div>
          <span class="label">Downloading {pct}%</span>
        {/if}
      </div>
    {/if}
  {/if}
</div>

<style>
  .file-bubble {
    background: var(--bg-elevated);
    color: var(--text);
    padding: var(--s-2) var(--s-3);
    border-radius: 12px;
    margin: var(--s-1) 0;
    max-width: 60ch;
  }
  .file-bubble.outgoing { background: var(--accent); color: var(--bg); margin-left: auto; }
  .card { display: flex; align-items: center; gap: var(--s-2); flex-wrap: wrap; }
  .ficon :global(svg) { width: 20px; height: 20px; }
  .fname { font: var(--t-ui); word-break: break-word; }
  .fsize { color: var(--text-muted); font: var(--t-ui); }
  .file-bubble.outgoing .fsize { color: rgba(255, 255, 255, 0.7); }
  .preview { max-width: 100%; max-height: 320px; border-radius: 8px; display: block; margin-bottom: var(--s-1); }
  .actions { display: flex; gap: var(--s-1); }
  .actions button {
    padding: 2px var(--s-2);
    background: var(--bg);
    color: var(--text);
    border: 1px solid var(--bg-elevated);
    border-radius: 6px;
    font: var(--t-ui);
    cursor: pointer;
  }
  .progress { position: relative; margin-top: var(--s-1); height: 16px; background: var(--bg); border-radius: 4px; overflow: hidden; }
  .progress .bar { height: 100%; background: var(--accent); transition: width 0.2s; }
  .progress .label { position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; font: var(--t-ui); color: var(--text); }
  .failed { color: var(--danger); font: var(--t-ui); }
</style>
```

- [ ] **Step 4: Run, verify pass**

Run: `cd crates/ui/src-svelte && pnpm test src/lib/components/FileAttachmentBubble.test.ts 2>&1 | tail -30`
Expected: all 6 tests PASS. (If `$effect`/`tick` timing flakes, add `await findBy*` waits; do not weaken assertions.)

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src-svelte/src/lib/components/FileAttachmentBubble.svelte crates/ui/src-svelte/src/lib/components/FileAttachmentBubble.test.ts
git commit -m "feat(3.D): FileAttachmentBubble component (card/progress/preview/open)"
```

---

## Task 10: Optimistic file send + Composer attach button + size gate

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/stores/conversation.ts`, `crates/ui/src-svelte/src/lib/components/Composer.svelte`
- Test: `crates/ui/src-svelte/src/lib/components/Composer.test.ts` (extend)

**Interfaces:**
- Consumes: `markQueued` from `$lib/stores/attachments`; `open` from `@tauri-apps/plugin-dialog`; `invoke` (`file_size`); `MANIFEST_SIZE_HARD`/`MANIFEST_SIZE_SOFT`/`formatBytes`; `toast`.
- Produces:
  - `OptimisticMessage` gains optional `__attachName?: string; __attachSize?: number`.
  - `sendFile(contact: PublicKey, path: string, filename: string, size: number): Promise<void>` in `conversation.ts`.

- [ ] **Step 1: Extend `OptimisticMessage` + add `sendFile`**

In `crates/ui/src-svelte/src/lib/stores/conversation.ts`, extend the type (line 9-13):
```ts
export type OptimisticMessage = MessageRecord & {
  __tempId: string;
  __optimistic: true;
  __failed?: string;
  __attachName?: string;
  __attachSize?: number;
};
```
Add `markQueued` import near the other store imports (top of file, after line 7):
```ts
import { markQueued } from "./attachments";
```
Add `sendFile` (after `send`, ~line 251):
```ts
/**
 * Optimistically insert an outgoing Kind::File bubble, issue SendFile, and
 * reconcile on FileQueued. The sender never receives download progress
 * (pull/deposit model), so we record the manifest message's delivery status
 * only; the attachments store entry stays "queued".
 */
export async function sendFile(
  contact: PublicKey,
  path: string,
  filename: string,
  size: number,
): Promise<void> {
  const tempId = crypto.randomUUID();
  conversation.update((state) => {
    if (state.contact === null || !pubkeyEq(state.contact, contact)) return state;
    const placeholder: OptimisticMessage = {
      __tempId: tempId,
      __optimistic: true,
      __attachName: filename,
      __attachSize: size,
      row_id: -1n,
      message_id: "00000000000000000000000000000000",
      contact,
      direction: "outgoing",
      kind: { kind: "file", manifest: [] as unknown as string },
      mls_generation: 0n,
      ts_daemon_recv: BigInt(Math.floor(Date.now() / 1000)),
      ts_envelope: BigInt(Date.now()),
    };
    return { ...state, messages: [...state.messages, placeholder] };
  });
  try {
    const resp = await ipcClient.request({ cmd: "send_file", contact, path });
    if (get(conversation).contact !== contact) return;
    const result = unwrapOk(resp);
    if (result.result !== "file_queued") {
      markFailed(tempId, "unexpected reply variant");
      return;
    }
    const { message_id, attachment_id, total_chunks } = result.data;
    markQueued(hex16ToString(attachment_id), { filename, size, total: total_chunks });
    recordDeliveryStatus(hex16ToString(message_id), "Queued");
    // Promote the optimistic bubble to non-optimistic; keep the carried
    // display fields so the bubble still shows name/size until the real
    // MessageRecord arrives via message_received (if it does).
    conversation.update((s) => {
      const idx = s.messages.findIndex((m) => (m as OptimisticMessage).__tempId === tempId);
      if (idx < 0) return s;
      const next = [...s.messages];
      next[idx] = { ...(next[idx] as OptimisticMessage), __optimistic: false } as OptimisticMessage;
      return { ...s, messages: next };
    });
  } catch (e) {
    if (get(conversation).contact !== contact) return;
    markFailed(tempId, e instanceof Error ? e.message : String(e));
  }
}
```

- [ ] **Step 2: Write the failing Composer test** (size gate + send)

Append to `crates/ui/src-svelte/src/lib/components/Composer.test.ts` (mock dialog + core + conversation):
```ts
// --- attachment attach button (Task 10) ---
import { vi } from "vitest";

vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { open as dialogOpen } from "@tauri-apps/plugin-dialog";
import { invoke as coreInvoke } from "@tauri-apps/api/core";
import * as conversation from "$lib/stores/conversation";

describe("Composer attach", () => {
  beforeEach(() => {
    vi.mocked(dialogOpen).mockReset();
    vi.mocked(coreInvoke).mockReset();
  });

  test("picking a ≤10 MiB file calls sendFile", async () => {
    vi.mocked(dialogOpen).mockResolvedValue("/picked/photo.jpg");
    vi.mocked(coreInvoke).mockResolvedValue(1024); // file_size
    const spy = vi.spyOn(conversation, "sendFile").mockResolvedValue();

    const { getByLabelText } = render(Composer, {
      props: { contact: "ab".repeat(32), disabled: false },
    });
    await getByLabelText("Attach file").click();
    await vi.waitFor(() =>
      expect(spy).toHaveBeenCalledWith("ab".repeat(32), "/picked/photo.jpg", "photo.jpg", 1024),
    );
  });

  test("cancelling the picker is a no-op", async () => {
    vi.mocked(dialogOpen).mockResolvedValue(null);
    const spy = vi.spyOn(conversation, "sendFile").mockResolvedValue();
    const { getByLabelText } = render(Composer, {
      props: { contact: "ab".repeat(32), disabled: false },
    });
    await getByLabelText("Attach file").click();
    await new Promise((r) => setTimeout(r, 0));
    expect(spy).not.toHaveBeenCalled();
  });

  test("a >100 MiB file is blocked", async () => {
    vi.mocked(dialogOpen).mockResolvedValue("/picked/huge.bin");
    vi.mocked(coreInvoke).mockResolvedValue(200 * 1024 * 1024);
    const spy = vi.spyOn(conversation, "sendFile").mockResolvedValue();
    const { getByLabelText } = render(Composer, {
      props: { contact: "ab".repeat(32), disabled: false },
    });
    await getByLabelText("Attach file").click();
    await new Promise((r) => setTimeout(r, 0));
    expect(spy).not.toHaveBeenCalled();
  });
});
```
> Confirm the existing `Composer.test.ts` import block (`render`, `describe`, `test`, `expect`, `beforeEach`) — reuse those; don't redeclare. If the file mocks modules at top-level, hoist these `vi.mock` calls to the top with the others (vitest hoists `vi.mock` automatically, but keep them module-scoped).

- [ ] **Step 3: Run, verify fail**

Run: `cd crates/ui/src-svelte && pnpm test src/lib/components/Composer.test.ts 2>&1 | tail -25`
Expected: FAIL — no "Attach file" button.

- [ ] **Step 4: Add the attach button + handler to Composer**

In `crates/ui/src-svelte/src/lib/components/Composer.svelte`, extend the `<script>` imports:
```ts
  import { send, sendFile } from "$lib/stores/conversation";
  import { open as pickFile } from "@tauri-apps/plugin-dialog";
  import { invoke } from "@tauri-apps/api/core";
  import { toast } from "$lib/stores/toast";
  import { MANIFEST_SIZE_HARD, MANIFEST_SIZE_SOFT, formatBytes } from "$lib/attachments";
  import { icons } from "$lib/icons";
```
Add the handler (in `<script>`, near `trySend`):
```ts
  async function tryAttach(): Promise<void> {
    if (disabled) return;
    let selected: string | string[] | null;
    try {
      selected = await pickFile({ multiple: false, directory: false });
    } catch (e) {
      toast.show("Could not open file picker");
      return;
    }
    if (selected === null || Array.isArray(selected)) return; // cancelled
    const path = selected;
    let size: number;
    try {
      size = await invoke<number>("file_size", { path });
    } catch {
      toast.show("File is unavailable");
      return;
    }
    if (size > MANIFEST_SIZE_HARD) {
      toast.show(`File too large (max ${formatBytes(MANIFEST_SIZE_HARD)})`);
      return;
    }
    if (size > MANIFEST_SIZE_SOFT) {
      const ok = window.confirm(
        `This file is ${formatBytes(size)}. It will only be delivered while your contact is online. Send anyway?`,
      );
      if (!ok) return;
    }
    const filename = path.split(/[/\\]/).pop() ?? "attachment";
    await sendFile(contact, path, filename, size);
  }
```
Add the button to the form (before the `Send` button in the `<form>`):
```svelte
  <button
    type="button"
    class="attach"
    {disabled}
    onclick={() => void tryAttach()}
    aria-label="Attach file"
    title="Attach file"
  >{@html icons["paperclip"]}</button>
```
Add CSS:
```css
  .attach {
    display: inline-flex;
    align-items: center;
    padding: var(--s-2);
    background: var(--bg-elevated);
    color: var(--text);
    border: 0;
    border-radius: 8px;
    cursor: pointer;
  }
  .attach :global(svg) { width: 18px; height: 18px; }
  .attach:disabled { opacity: 0.5; cursor: not-allowed; }
```

- [ ] **Step 5: Run, verify pass**

Run: `cd crates/ui/src-svelte && pnpm test src/lib/components/Composer.test.ts 2>&1 | tail -25`
Expected: the 3 new attach tests PASS and the existing composer tests still PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/ui/src-svelte/src/lib/stores/conversation.ts crates/ui/src-svelte/src/lib/components/Composer.svelte crates/ui/src-svelte/src/lib/components/Composer.test.ts
git commit -m "feat(3.D): composer attach button, size gate, optimistic file send"
```

---

## Task 11: Render file bubbles + wire dispatcher arms

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/components/MessageBubble.svelte`, `crates/ui/src-svelte/src/routes/+page.svelte`

**Interfaces:**
- Consumes: `FileAttachmentBubble` (Task 9); `applyProgress`/`applyReceived`/`applyFailed` (Task 6).
- Produces: `Kind::File` messages render `<FileAttachmentBubble>`; the 3 attachment events drive the store.

- [ ] **Step 1: Switch on kind in MessageBubble**

In `crates/ui/src-svelte/src/lib/components/MessageBubble.svelte`, import the bubble (after line 6):
```ts
  import FileAttachmentBubble from "./FileAttachmentBubble.svelte";
```
Wrap the existing markup so a `file` kind renders the file bubble instead of the text bubble. Replace the top-level `<div class="bubble" …>…</div>` (lines 41-49) with:
```svelte
{#if record.kind.kind === "file"}
  <FileAttachmentBubble {record} />
{:else}
  <div class="bubble" class:outgoing={isOutgoing} class:focus-highlight={highlighted} data-row-id={record.row_id}>
    <p class="body">{body}</p>
    <div class="meta">
      <time class="ts">{new Date(tsMs).toLocaleTimeString()}</time>
      {#if isOutgoing && iconStatus}
        <DeliveryIcon status={iconStatus} title={iconTitle} />
      {/if}
    </div>
  </div>
{/if}
```
(Leave the `<style>` untouched — `FileAttachmentBubble` is self-styled.)

- [ ] **Step 2: Add the 3 dispatcher arms in `+page.svelte`**

In `crates/ui/src-svelte/src/routes/+page.svelte`, add the imports (after line 19):
```ts
  import { applyProgress, applyReceived, applyFailed } from "$lib/stores/attachments";
```
Extend the event dispatcher (the `subscribe` callback, after the `delivery_status_changed` arm, line 93-94) — add:
```ts
        } else if (e.event === "attachment_progress") {
          applyProgress(hex16ToString(e.data.attachment_id), e.data.received, e.data.total);
        } else if (e.event === "attachment_received") {
          applyReceived(hex16ToString(e.data.attachment_id), {
            filename: e.data.filename,
            mime: e.data.mime,
            size: Number(e.data.size),
            path: e.data.path,
          });
        } else if (e.event === "attachment_failed") {
          applyFailed(hex16ToString(e.data.attachment_id), e.data.reason);
```

- [ ] **Step 3: Type-check + build**

Run: `cd crates/ui/src-svelte && pnpm check 2>&1 | tail -20 && pnpm build 2>&1 | tail -10`
Expected: no type errors; build succeeds. (`e.data.size` is `bigint` → `Number(...)`; `received`/`total` are already `number`.)

- [ ] **Step 4: Run the full vitest suite**

Run: `cd crates/ui/src-svelte && pnpm test 2>&1 | tail -25`
Expected: all unit tests (existing + new) PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src-svelte/src/lib/components/MessageBubble.svelte crates/ui/src-svelte/src/routes/+page.svelte
git commit -m "feat(3.D): render file bubbles + wire attachment event dispatcher"
```

---

## Task 12: e2e mock + Playwright flow

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/test/tauri-mock.ts`
- Create: `crates/ui/src-svelte/tests/e2e/attachments.spec.ts`

**Interfaces:**
- Consumes: the e2e mock harness; the `?fixture=` URL-param pattern.
- Produces: a `?fixture=attachments` flow mocking `file_size`/`decode_attachment_manifest`/`open_file`/`reveal_in_folder` + the dialog plugin, and emitting attachment events; a Playwright spec driving attach→send→bubble→progress→preview, a >100 MiB block, and a failure case.

- [ ] **Step 1: Extend the mock**

In `crates/ui/src-svelte/src/lib/test/tauri-mock.ts`:
1. Add a fixture flag near the others (after line 34):
```ts
const _fixtureAttachments =
  typeof window !== "undefined" &&
  new URLSearchParams(window.location.search).get("fixture") === "attachments";
```
and include it in the `_vault` OR-chain (line 36).
2. In `list_contacts` (the `_fixtureSeeded` branch area), add an `else if (_fixtureAttachments)` returning the same single-contact shape as `_fixtureSeeded` (pubkey `FIXTURE_PEER_PUBKEY`, `group_state: "active"`).
3. In `recent_messages`, return empty page for the attachments fixture (default branch already does).
4. Add new top-level `case` arms in the `switch (cmd)` (before `default:`):
```ts
    case "file_size": {
      // Drive size-gate branches by filename convention.
      const p = String(args?.path ?? "");
      if (p.includes("huge")) return (200 * 1024 * 1024) as unknown as T;
      if (p.includes("big")) return (50 * 1024 * 1024) as unknown as T;
      return 2048 as unknown as T;
    }
    case "decode_attachment_manifest": {
      return {
        attachment_id: "ab".repeat(16),
        filename: "photo.jpg",
        mime: "image/jpeg",
        total_size: 2048,
      } as unknown as T;
    }
    case "open_file":
    case "reveal_in_folder":
      return undefined as unknown as T;
    case "plugin:dialog|open": {
      // @tauri-apps/plugin-dialog open() routes through invoke under this id.
      return "/picked/photo.jpg" as unknown as T;
    }
```
5. In the `ipc_request` block, add a `send_file` arm (mirroring `send_message`): return `file_queued` and schedule attachment events on `_subscribeChannel`:
```ts
      if (cmdObj.cmd === "send_file") {
        const fileCmd = cmdObj as { cmd: string; contact: string; path: string };
        // Emit an incoming Kind::File message + progress + received so the
        // e2e can assert the receive path too (sender-side has no progress).
        setTimeout(() => {
          if (!_subscribeChannel) return;
          _subscribeChannel._emit({
            event: "message_received",
            data: {
              contact: fileCmd.contact,
              record: {
                row_id: 2, message_id: "11".repeat(16), contact: fileCmd.contact,
                direction: "incoming", kind: { kind: "file", manifest: [1, 2, 3] },
                mls_generation: 1, ts_daemon_recv: Math.floor(Date.now() / 1000),
                ts_envelope: Date.now(),
              },
            },
          });
          _subscribeChannel._emit({
            event: "attachment_progress", data: { attachment_id: "ab".repeat(16), received: 1, total: 2 },
          });
          _subscribeChannel._emit({
            event: "attachment_received",
            data: {
              attachment_id: "ab".repeat(16), contact: fileCmd.contact,
              filename: "photo.jpg", mime: "image/jpeg", size: 2048, path: "/dl/photo.jpg",
            },
          });
        }, 100);
        return {
          resp: "ok",
          data: { result: "file_queued", data: { message_id: "22".repeat(16), attachment_id: "ab".repeat(16), total_chunks: 2 } },
        } as unknown as T;
      }
```
> Confirm how `@tauri-apps/plugin-dialog`'s `open()` dispatches in the mock. The Vite alias replaces `@tauri-apps/api/core`; the dialog plugin calls `invoke("plugin:dialog|open", …)`. If the plugin is NOT aliased through the mock (it imports `@tauri-apps/api/core` internally, which IS aliased), the `plugin:dialog|open` case handles it. Verify by running the spec; if the command id differs, capture it from the thrown `unhandled invoke cmd=` error and rename the case. For `convertFileSrc`, the real `@tauri-apps/api/core` is mocked — add a `convertFileSrc` export to the mock returning `asset://localhost/${path}` so `<img>` has a src.

6. Add to the mock's exports (the file currently exports `invoke` + `Channel`): add
```ts
export function convertFileSrc(path: string): string {
  return `asset://localhost/${path}`;
}
```

- [ ] **Step 2: Verify the Vite alias covers `convertFileSrc`**

Run: `grep -rn "tauri-mock\|@tauri-apps/api/core" crates/ui/src-svelte/vite.config.ts crates/ui/src-svelte/vitest.config.ts`
Expected: find the alias that maps `@tauri-apps/api/core` → `tauri-mock.ts` under `TAURI_MOCK`. Confirm both `invoke`, `Channel`, and now `convertFileSrc` are exported by the mock (callers import all three from `@tauri-apps/api/core`). If the alias is e2e-only (vite.config) and unit tests mock per-file (they do, via `vi.mock`), no change needed for vitest.

- [ ] **Step 3: Write the e2e spec**

Create `crates/ui/src-svelte/tests/e2e/attachments.spec.ts`:
```ts
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { test, expect } from "@playwright/test";

test.describe("attachments", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/?fixture=attachments");
    await expect(page.locator(".shell")).toBeVisible({ timeout: 10_000 });
    await page.locator(".rail .row").first().click();
  });

  test("attach → send → receive → inline preview", async ({ page }) => {
    await page.getByLabel("Attach file").click();
    // Incoming Kind::File bubble decodes to the filename.
    await expect(page.getByText("photo.jpg")).toBeVisible({ timeout: 3_000 });
    // Progress bar appears, then the inline image once received.
    await expect(page.locator(".file-bubble img.preview")).toBeVisible({ timeout: 3_000 });
    await expect(page.getByRole("button", { name: /open/i })).toBeVisible();
  });
});
```
> If driving the picker through the dialog plugin proves brittle under the mock, add a deterministic data-path: gate the picker behind the mock so `getByLabel("Attach file").click()` resolves to `/picked/photo.jpg`. Keep the assertion on the **received** bubble (filename + preview), which exercises the dispatcher + store + bubble regardless of picker mechanics.

- [ ] **Step 4: Run the e2e locally**

Run: `cd crates/ui/src-svelte && pnpm exec playwright install chromium 2>&1 | tail -3 && pnpm test:e2e attachments 2>&1 | tail -30`
Expected: the attachments spec PASSES. Iterate on the mock command-id for the dialog open if the first run throws `unhandled invoke cmd=`.

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src-svelte/src/lib/test/tauri-mock.ts crates/ui/src-svelte/tests/e2e/attachments.spec.ts
git commit -m "test(3.D): e2e attachment flow + tauri-mock attachment commands"
```

---

## Task 13: CI — hard-gate vitest in the `ui` job

**Files:**
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Produces: the `ui` job runs `pnpm test` (vitest) as a required step.

- [ ] **Step 1: Add the vitest step**

In `.github/workflows/ci.yml`, in the `ui` job, after the `Build SvelteKit frontend` step (the `pnpm build` step), add:
```yaml
    - name: Unit tests (vitest)
      working-directory: crates/ui/src-svelte
      run: pnpm test
```
(Playwright e2e stays local-required / CI-best-effort — do NOT add `pnpm test:e2e` to CI, per design §11; it needs `pnpm exec playwright install`.)

- [ ] **Step 2: Sanity-check the YAML**

Run: `grep -n "pnpm test\|pnpm build\|Unit tests" .github/workflows/ci.yml`
Expected: the new step sits between `pnpm build` and the clippy step.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci(3.D): hard-gate vitest in the ui job"
```

---

## Task 14: Full local gate (verification-before-completion)

**Files:** none (verification only).

- [ ] **Step 1: Rust gate**

Run:
```bash
. "$HOME/.cargo/env" && \
cargo clippy -p skattr-ui --all-targets --all-features -- -D warnings && \
cargo build -p skattr-ui --all-targets && \
cargo test -p skattr-ui --all-targets 2>&1 | tail -30
```
Expected: no clippy warnings; build clean; all `skattr-ui` Rust tests PASS.

- [ ] **Step 2: Core gate (the manifest re-export, if added in Task 1)**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness 2>&1 | tail -15`
Expected: core lib tests + ts-rs bindings regen PASS (confirms the `pub use` didn't break the public surface and types are unchanged).

- [ ] **Step 3: Frontend gate**

Run:
```bash
cd crates/ui/src-svelte && \
pnpm install --frozen-lockfile && \
pnpm check 2>&1 | tail -15 && \
pnpm build 2>&1 | tail -10 && \
pnpm test 2>&1 | tail -20
```
Expected: install clean; no svelte-check type errors; build succeeds; all vitest suites PASS.

- [ ] **Step 4: e2e gate (local)**

Run: `cd crates/ui/src-svelte && pnpm test:e2e 2>&1 | tail -30`
Expected: all e2e specs (existing + attachments) PASS.

- [ ] **Step 5: Final commit / branch status**

Confirm the working tree is clean and all commits are on `phase-3d-attachment-ui`:
Run: `git status && git log --oneline master..HEAD`
Expected: clean tree; the Task 1–13 commits listed. Hand off to whole-branch review (do NOT merge — follow `superpowers:finishing-a-development-branch`).

---

## Self-Review (completed against the spec)

**Spec coverage:**
- §1 goal / §2 surface → Tasks 1–13 consume `SendFile`/`FileQueued`/`Kind::File`/`Event::Attachment*` exactly as generated.
- §3 reality-checks → byte-array manifest (Global Constraints + Task 1/7); sender shows delivery status only, no download progress (Task 9 outgoing branch, Task 10 `sendFile` keeps store `queued`).
- §4 locked decisions → asset-protocol inline preview (Task 4 scope + Task 9 `convertFileSrc`); Rust manifest decode (Task 1); pre-send size gate (Task 10); default download folder + Reveal (Tasks 3/9).
- §5 architecture → store (Task 6), bubble (Task 9), helpers (Task 7), 4 shell commands (Tasks 1–3), dispatcher arms + MessageBubble switch + Composer button + tauri.conf/capabilities (Tasks 4, 10, 11).
- §6 state model → Task 6 store mirrors the spec's `AttachmentState` + update fns + `attachmentFor`; order-independent/global/session-scoped documented.
- §7 bubble states → Task 9 covers sender card+delivery icon, receiver progress (determinate + indeterminate for ≤8 chunks), complete+image preview, complete+non-image Open/Reveal, failed, decode-fail card, `onerror` fallback.
- §8 flows → attach&send (Task 10) incl. optimistic bubble; receive (Task 11 dispatcher).
- §9 failures → AttachmentFailed (Task 9), decode-fail card (Task 9), send errors + file_size fail + size block (Task 10), Open/Reveal not-found toast (Task 9), img onerror (Task 9), progress-before-bubble (global store, Task 6), tiny-file indeterminate (Task 9).
- §10 Tauri config → Task 4 (CSP, assetProtocol enable + runtime scope, capabilities, plugin stanzas), Task 5 (JS deps), Task 3 (reveal API name confirmation).
- §11 testing → vitest (Tasks 6/7/9/10), e2e (Task 12), Rust shell tests (Tasks 1/2/3), CI vitest gate (Task 13), full gate (Task 14).
- §12 deferrals → not implemented by design (post-restart state, configurable folder, in-UI retry, sender progress) — no tasks, correct.

**Placeholder scan:** every code step carries full literal code; no "TBD"/"handle errors"/"similar to". The few "confirm the exact API name" notes (opener reveal, asset scope, dialog command-id, manifest re-export path) are genuine environment-verification steps with explicit fallbacks, not deferrals — each names what to run and what to do with the result.

**Type consistency:** `attachment_id` keys are hex via `hex16ToString` everywhere; store fns named `markQueued`/`applyManifest`/`applyProgress`/`applyReceived`/`applyFailed`/`attachmentFor` used identically in Tasks 6/9/10/11; `ManifestSummary` fields (`attachment_id`/`filename`/`mime`/`total_size`) match between Rust (Task 1) and TS (Task 7/9); `sendFile(contact, path, filename, size)` signature matches between Task 10 store and Composer caller; `Event.attachment_received.size` is `bigint` → `Number(...)` at the one consumption site (Task 11).
