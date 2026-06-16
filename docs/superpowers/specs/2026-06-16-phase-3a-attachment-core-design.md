# Phase 3.A — Attachment Core (Design)

**Date:** 2026-06-16
**Status:** Approved (brainstorming complete); plan to follow.
**Predecessors:** Phase 1 + Phase 2 (all sub-projects) merged. Project docs
refreshed for the v1.0-audit phasing (`76262cb`).
**Source:** `docs/superpowers/specs/2026-06-12-v1.0-roadmap.md` §"Phase 3 —
Attachments subsystem".

The first sub-project of **Phase 3 (attachments)**. Delivers the wire format +
local crypto pipeline that every other attachment sub-project builds on. No
transport, no protocol frames — pure, local, fully unit-testable.

---

## Phase 3 decomposition (context)

Phase 3 is decomposed into four sub-projects (each its own
`spec → plan → implement → verify` cycle, mirroring Phase 2). v1.0 scope
includes the **offline path** (roadmap as written), so 3.A's manifest is
designed so a recipient can correlate opaque chunk blobs fetched from a mailbox
back to the manifest.

- **3.A — Attachment core** *(this spec)*: manifest format, chunker, per-chunk
  encryption + integrity, metadata stripping, on-disk chunk store + transfer-
  state repo, size caps. No transport.
- **3.B — Direct transfer**: one additive `Frame` chunk type; sender streams
  chunks over the live Noise/MLS connection; receiver verifies + reassembles;
  live `run_with_transport` file-round-trip guardrail (online).
- **3.C — Offline transfer**: chunk blobs via the mailbox path (reuse the frozen
  `Deposit`, or extend ADR 0006 — decided in 3.C's spec), inheriting Phase 2
  caps; resume semantics.
- **3.D — UI**: attach / send / download / inline preview / progress / size
  limits.

Dependency order: **3.A → 3.B → 3.C**, with **3.D** after the wire stabilizes.

---

## Ground truth (verified against code 2026-06-16)

- `envelope::kinds::Kind` already has a placeholder variant:
  `File { manifest: Vec<u8> }` (CBOR-encoded manifest as raw bytes; `#[ts(type
  = "string")]` for the UI). Clients ignore unknown `Kind` variants
  (forward-compatible). The manifest therefore rides as a normal MLS app
  message — confidential + authenticated by the existing
  send→encrypt→deliver→decrypt→persist path; no new envelope variant needed.
- `identity::derive::hkdf_expand::<N>(ikm, info)` + domain-separated `INFO_*`
  constants are the established HKDF helper; existing uses (storage key, hs
  key, backup key, `h_transport`) all add a fresh `INFO_*`.
- `chacha20poly1305` (XChaCha20-Poly1305) is already a dependency (the identity
  vault uses it). `sha2` (SHA-256) is already a dependency (mailbox recipient
  hashing). **No new crypto** is introduced — only existing primitives.
- The mailbox `Deposit.ciphertext` cap is `max_deposit_size = 1 MiB` (default);
  256 KiB plaintext chunks + AEAD tag + framing sit well under it, so a 3.C
  offline chunk fits one `Deposit` without a protocol change.
- `storage::migrations` runs `include_str!`'d SQL keyed by `schema_version`,
  currently through `0014`; new tables land as migration `0015`.
- At-rest model (Phase 2.B): the encrypted DB is the at-rest story; anything
  written to `data_dir` as plaintext is a gap. 3.A's staged chunk blobs are AEAD
  **ciphertext**, so storing them under `data_dir` is acceptable.

## Locked decisions (brainstorming, 2026-06-16)

1. **Per-chunk keying = single file key + HKDF-per-chunk.** The manifest carries
   one random 32-byte `file_key`; each chunk's key + nonce is derived as
   `HKDF(file_key, "skattr-attach-v1" || u32_be(index))`. Compact manifest;
   domain-separated; no new crypto. The manifest (hence `file_key`) is
   confidential because it travels inside MLS.
2. **Sizes: 256 KiB plaintext chunks, 100 MiB max total.** Oversize is rejected
   up front with a typed error.
3. **Metadata stripping = image EXIF/metadata for common formats** (JPEG, PNG,
   and TIFF/WebP where the chosen crate supports it), send-side only. Other file
   types pass through unchanged; the gap is disclosed in the threat model.
4. **`ciphertext_hash = SHA-256(chunk_ciphertext)`** — the content address +
   tamper-binding that lets a recipient match/verify an opaque blob against the
   manifest before decrypting.
5. **Staged blobs are ciphertext-only; plaintext is transient** — input file on
   send, output file on receive (caller-chosen path, never `data_dir`).

---

## Architecture — units

### A1. Manifest type (`envelope` / `attachment`)

A CBOR struct serialized into `Kind::File { manifest: Vec<u8> }`:

```rust
struct AttachmentManifest {
    manifest_version: u8,      // 1; an unknown version is rejected (forward-compat)
    attachment_id: [u8; 16],   // random; correlates the transfer end-to-end
    filename: String,          // post-strip display name (sanitized: no path separators)
    mime: String,              // declared/sniffed content type
    total_size: u64,           // plaintext bytes after metadata stripping
    chunk_size: u32,           // 262_144 (256 KiB)
    file_key: [u8; 32],        // random; Zeroizing in memory
    chunks: Vec<ChunkRef>,     // index-ordered
}
struct ChunkRef {
    index: u32,
    ciphertext_hash: [u8; 32], // SHA-256 of the chunk ciphertext
    len: u32,                  // ciphertext length (≤ chunk_size + AEAD overhead)
}
```

`file_key` is wrapped so it zeroizes; CBOR encode/decode via `ciborium` with the
project's `.map_err(|e| CoreError::Cbor…)` pattern. `[u8; N>32]` is not used
(all arrays ≤ 32), so no `BigArray`.

### A2. Chunker (send) — `attachment::chunker`

`chunk_file(input_path, mime_hint) -> Result<(AttachmentManifest, Vec<StagedChunk>)>`:
1. Strip metadata (A4) into an in-memory / temp plaintext buffer.
2. Reject if `total_size > MAX_ATTACHMENT_BYTES` (100 MiB) → `AttachmentErrorKind::TooLarge`.
3. Generate `attachment_id` + `file_key` (random).
4. For each 256 KiB plaintext chunk at `index`:
   - derive `(key, nonce) = hkdf_expand`-based per-chunk material (see A1 decision 1),
   - XChaCha20-Poly1305 encrypt → ciphertext,
   - `ciphertext_hash = SHA-256(ciphertext)`,
   - record `ChunkRef { index, ciphertext_hash, len }`.
5. Return the manifest + the staged ciphertext chunks (written to the store, A5).

The chunker is deterministic given `(file_key, attachment_id, plaintext)` so
tests can assert exact round-trips; randomness is confined to id/key generation.

### A3. Reassembler (receive) — `attachment::reassembler`

`reassemble(manifest, chunk_source, output_path) -> Result<()>`:
1. For each `ChunkRef` in index order, read the ciphertext blob from the source,
   verify `SHA-256(blob) == ciphertext_hash` (→ `ChunkHashMismatch` on failure,
   **before** decrypt),
2. derive the per-chunk key, AEAD-decrypt (tag failure → `AeadFailed`),
3. stream the plaintext into a temp file alongside `output_path`,
4. after the last chunk, verify the running plaintext length == `total_size`
   (→ `SizeMismatch`), then atomically `rename` temp → `output_path`.

No partial output is ever visible (temp + rename). `output_path` is
caller-supplied and validated to not escape an allowed download directory
(`filename` from the manifest is sanitized: basename only, no `/`, `..`, NUL).

### A4. Metadata stripping (send) — `attachment::strip`

`strip_metadata(bytes, mime) -> Result<(Vec<u8>, String /*effective mime*/)>`:
strips EXIF/GPS/metadata from common image formats via a vetted pure-Rust crate
(**candidate: `img-parts`** — container-level manipulation that drops metadata
segments without re-encoding pixels; the implementation plan confirms the exact
crate passes `cargo-deny` and covers JPEG/PNG, TIFF/WebP where supported).
Non-image / unsupported types pass through unchanged. A unit test asserts a
fixture JPEG with planted EXIF/GPS comes out with that metadata gone and pixels
intact.

### A5. Chunk store + transfer-state repo (`storage`)

- **On-disk chunk store** (`storage::attachment_store` or
  `attachment::store`): ciphertext blobs at
  `<data_dir>/attachments/<hex attachment_id>/<index>`. Put/get/remove a chunk;
  remove an attachment's whole dir on completion/cancel. Ciphertext-only (no
  at-rest gap).
- **Transfer-state repo** (`storage::attachments`, migration `0015`):
  `attachments(attachment_id BLOB PK, direction TEXT('out'|'in'), manifest BLOB,
  total_chunks INTEGER, status TEXT, created_at INTEGER)` plus per-chunk receipt
  tracking (`attachment_chunks(attachment_id, index, received INTEGER, PRIMARY
  KEY(attachment_id, index))`) so 3.B/3.C can record progress and resume. 3.A
  defines + unit-tests the repo; 3.B/3.C populate received chunks.

### A6. Errors + caps

A new `AttachmentErrorKind` sub-enum under `CoreError` (structural `kind()`
match preserved; build-time `str::contains` guard still holds): `TooLarge`,
`ChunkHashMismatch`, `AeadFailed`, `SizeMismatch`, `ManifestInvalid`,
`UnsupportedStrip`, `Io`. Caps as module constants: `CHUNK_SIZE = 262_144`,
`MAX_ATTACHMENT_BYTES = 100 * 1024 * 1024`.

## Error handling

All failures are typed `CoreError::Attachment(AttachmentErrorKind)` — no
panics, no `unwrap`/`expect` in non-test code. Reassembly writes to a temp file
and renames only on full success (no partial/corrupt output). Hash check runs
**before** decrypt so a tampered blob is rejected without spending the key.

## Security posture

- **No new crypto:** XChaCha20-Poly1305 + SHA-256 + HKDF (all existing deps,
  domain-separated `"skattr-attach-v1"`).
- **Confidentiality + integrity of keys:** `file_key` travels only inside the
  MLS-encrypted manifest; staged/transferred blobs are opaque AEAD ciphertext.
- **Tamper-evidence for the offline path:** `ciphertext_hash` binds each blob to
  the manifest; a malicious mailbox (3.C) cannot substitute a blob without
  detection.
- **No plaintext at rest:** staged blobs are ciphertext; plaintext input/output
  is transient and never written to `data_dir`.
- **Filename safety:** the manifest `filename` is sanitized (basename only) so a
  hostile sender can't path-traverse the receiver's download dir.
- **Metadata-leak reduction:** EXIF/GPS stripped from common images on send;
  the non-image gap is disclosed (threat model / SECURITY.md, updated in Phase 4).
- Storage-only + envelope-only: **no transport/protocol/wire-frame change** in
  3.A (the manifest reuses the existing `Kind::File`).

## Out of scope (this sub-project)

- Moving bytes between daemons — direct transfer is **3.B**, offline mailbox
  transfer is **3.C**.
- UI (attach/preview/progress) — **3.D**.
- Resume *protocol* — 3.A persists per-chunk receipt state so 3.B/3.C can
  resume; the resume logic itself is theirs.
- PDF/document metadata stripping (v1.1).

## Exit criteria

1. A file chunked by the chunker and reassembled by the reassembler round-trips
   **byte-identically** (post-strip plaintext in == out).
2. A flipped ciphertext byte fails the `ciphertext_hash` check (before decrypt);
   a flipped AEAD tag fails decryption — both surface typed errors, no panic.
3. An attachment > 100 MiB is rejected with `AttachmentErrorKind::TooLarge`
   before any chunking.
4. A fixture image with planted EXIF/GPS comes out metadata-free with pixels
   intact; a non-image passes through unchanged.
5. The `AttachmentManifest` CBOR round-trips; an unknown `manifest_version` is
   rejected.
6. The chunk store + `attachments`/`attachment_chunks` repo (migration 0015)
   put/get/remove + receipt-state round-trip; staged blobs are ciphertext only.
7. `cargo fmt --check`, `cargo clippy --workspace --exclude skattr-ui
   --all-targets --all-features -- -D warnings`, the new dependency passes
   `cargo deny check`, and the full core + tests suites are green.

## Delivery model

`spec (this doc) → writing-plans → subagent-driven execution → two-stage review
per task → verification → finish branch`.
