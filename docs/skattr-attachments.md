# Skattr: Attachment Specification

Companion to `skattr-design.md`, `skattr-implementation-plan.md`, and `skattr-deep-dives.md`. This document specifies how media and file sharing works end-to-end.

## Goals

- Send any file type (images, video, audio, documents, archives) with the same privacy properties as text messages: end-to-end encrypted, metadata-minimized, delivered over Tor
- Efficient for the common case (small images) and robust for the large case (video up to 1 GiB)
- Resumable transfers, parallel chunk fetches, per-chunk integrity
- No leakage of file contents or recipient identity to any mailbox operator
- Coarse-only leakage of file size to network observers

## Non-goals for v1

- Live video or voice calls (see Future Work)
- Collaborative editing / real-time shared documents
- Peer-assisted / swarm delivery (torrent-style)
- Federated CDN or caching beyond the sender's mailbox

---

## 1. Data model

Attachments are carried by a small **manifest** embedded in an MLS application message. The file contents travel as separately-addressed encrypted chunks.

### 1.1 FileRef (the manifest)

```rust
pub struct FileRef {
    pub id: [u8; 16],              // random attachment id
    pub filename: String,          // display name, UTF-8, max 256 chars
    pub mime: String,               // RFC 6838, max 127 chars
    pub size: u64,                  // total plaintext bytes
    pub key: [u8; 32],              // per-attachment symmetric key
    pub chunks: Vec<ChunkRef>,      // ordered; index is position in plaintext
    pub thumbnail: Option<Thumbnail>, // small preview, inline if small enough
    pub metadata_stripped: MetadataReport, // what was removed, for UI disclosure
}

pub struct ChunkRef {
    pub hash: [u8; 32],             // sha256 of the ciphertext
    pub ciphertext_size: u32,       // exact size for fetch validation
    pub nonce: [u8; 24],            // XChaCha20 nonce, random per chunk
    pub locations: Vec<Location>,   // ordered by preference
}

pub enum Location {
    Inline,                                    // ciphertext in manifest (small files only)
    Mailbox { onion: OnionAddress, blob_id: [u8; 32] },
    Direct { peer: PublicKey },                // try peer-to-peer first if both online
}

pub struct Thumbnail {
    pub mime: String,              // "image/webp" typically
    pub width: u16,
    pub height: u16,
    pub data: Vec<u8>,              // encrypted with same attachment key, nonce 0
}

pub struct MetadataReport {
    pub stripped: Vec<StrippedItem>, // ["EXIF.GPS", "EXIF.DeviceSerial", "XMP.CreatorTool"]
    pub warnings: Vec<String>,       // things we couldn't strip but flagged
}
```

### 1.2 CBOR encoding

Canonical CBOR, sorted map keys, definite lengths. The manifest schema version lives in the outer envelope (`Envelope.v`), not here.

### 1.3 Size bounds

| Item | Limit | Rationale |
|------|-------|-----------|
| `filename` | 256 chars | Prevents display issues, defeats filename-as-data abuse |
| `mime` | 127 chars | RFC 6838 upper bound |
| `thumbnail.data` | 32 KiB | Inline fits in one MLS message comfortably |
| `chunks.len()` | 8192 | 8192 × 256 KiB = 2 GiB theoretical max |
| Total manifest size | 512 KiB | Fits in one MLS application message with headroom |

Files exceeding these limits are rejected at send time.

---

## 2. Chunk format

### 2.1 Chunking

- Fixed chunk size: **256 KiB** of plaintext
- Last chunk is padded to 256 KiB with random bytes; actual plaintext size recorded in the manifest
- Padding is part of the encrypted chunk (attacker learns only that chunks are 256 KiB)

### 2.2 Encryption

- Algorithm: **XChaCha20-Poly1305**
- Key: the per-attachment 32-byte `FileRef.key`
- Nonce: 24 random bytes per chunk, stored in `ChunkRef.nonce`
- Associated data: `attachment_id || chunk_index_u32_be` — binds chunk to its position, prevents reordering attacks

Why XChaCha20-Poly1305 instead of ChaCha20-Poly1305 with a counter nonce: random 24-byte nonces make accidental nonce reuse effectively impossible without a separate counter discipline. The tradeoff is 24 bytes per chunk instead of 0 (counter implicit) — trivial overhead.

### 2.3 Chunk integrity

- `ChunkRef.hash` is `SHA-256(ciphertext_including_poly1305_tag)`
- Recipient fetches chunk, verifies hash before decryption (fail fast on corruption)
- Then decrypts with key + nonce + AAD; Poly1305 catches any tampering the hash missed

### 2.4 Chunk encoding on the wire

```
+----------------+----------------+----------------+
| nonce (24 B)   | ciphertext +   |   (none)       |
|                | poly1305 tag   |                |
+----------------+----------------+----------------+
```

Nonce is included with the ciphertext when transferred so the recipient doesn't need a separate channel for it. `ChunkRef.nonce` is the authoritative copy in the manifest; if the transferred nonce doesn't match, reject.

### 2.5 Last chunk padding

Plaintext layout of the last chunk:

```
[ actual_data | random_padding ]
```

Padding length is implicit: `256 KiB - (size mod 256 KiB)`. Recipient trims after decryption based on `FileRef.size`.

---

## 3. Send pipeline

Ordered list of operations on the sender's side. Each step is a discrete stage with clear inputs and outputs; failures at any stage abort the whole send with a clear error to the user.

### 3.1 File ingest

1. User picks, drops, or pastes a file
2. Determine source type by file extension and `mime_guess`; if mismatch, prefer the magic-byte detection (via `infer` crate) and warn the user
3. Reject early if: file is 0 bytes, file is over user-configured max size, file type is in the blocklist (see §8)
4. Read file into a streaming pipeline — do not buffer entire file in memory for large files

### 3.2 Metadata stripping

Per-file-type pipeline; see §7 for details. Produces:
- A stripped file (in a temporary location)
- A `MetadataReport` describing what was removed

### 3.3 Thumbnail generation

If the file type supports it (see §8), generate a thumbnail:
- Image: decode, resize preserving aspect to max 400×400, re-encode as WebP quality 60
- Video: extract first keyframe at t≈1s, same processing as image
- PDF: render first page to raster, same processing as image
- Audio: generate waveform preview (PNG of RMS bars) 400×80
- Everything else: no thumbnail; UI shows generic icon

Thumbnail is encrypted with `FileRef.key` using nonce `[0; 24]` (reserved — regular chunks use random nonces, never this one). If under 32 KiB, stored inline in `FileRef.thumbnail`; otherwise treated as a regular chunk.

### 3.4 Chunking and encryption

1. Split stripped file into 256 KiB chunks
2. For each chunk in parallel (bounded concurrency, e.g. 4 workers):
   - Generate 24-byte random nonce
   - Encrypt with XChaCha20-Poly1305, AAD = `attachment_id || chunk_index`
   - Compute SHA-256 of ciphertext
   - Write ciphertext to a temp location (sender's cache dir, subdirectory per attachment)
3. Build `ChunkRef` list with hashes and nonces (no locations yet)

### 3.5 Storage routing decision

For each attachment, pick a routing strategy based on size and connectivity:

| Size | Strategy | Rationale |
|------|----------|-----------|
| ≤ 32 KiB | Inline in manifest | One round trip, no mailbox upload |
| 32 KiB – 4 MiB | Recipient's mailbox | Recipient fetches from a mailbox they already trust |
| > 4 MiB | Sender's mailbox + direct transfer when online | Doesn't bloat recipient's mailbox quota |

"Direct transfer" means: if both parties are online at send time, try a direct peer-to-peer chunk exchange first; fall back to mailbox if it fails. See §4.

### 3.6 Chunk upload

For chunks with a mailbox `Location`:
1. Upload to the selected mailbox via `BLOB_UPLOAD` (see §5)
2. Mailbox returns `blob_id`
3. Populate `ChunkRef.locations` with the mailbox address + blob_id

Uploads are parallelized with the same bounded-concurrency worker pool, rate-limited per mailbox.

### 3.7 Manifest delivery

1. Build complete `FileRef` with all locations filled in
2. Wrap in `Envelope { kind: Kind::File(FileRef), ... }`
3. Send via the normal MLS message delivery path
4. Message is marked as "sending attachment" until all chunks are uploaded AND manifest is delivered

### 3.8 Cache policy

- Encrypted chunks remain in the sender's cache until delivery ACK from recipient (or from all recipients in a group)
- On ACK: delete local encrypted copies; keep the plaintext file only if user's original lives elsewhere
- Cache directory has a configurable max size (default 2 GiB); LRU eviction when exceeded
- Abandoned uploads (no ACK after 7 days) garbage-collected

---

## 4. Receive pipeline

### 4.1 Manifest ingestion

1. Decrypted MLS application message contains `Envelope` with `Kind::File(FileRef)`
2. Validate manifest: size bounds (§1.3), hash lengths, chunk count, MIME string well-formed
3. Persist manifest in `messages` row; mark attachment as `Pending`
4. If thumbnail present, decrypt and store for immediate UI display
5. Do not auto-download chunks; wait for user action (tap "download") unless user has enabled auto-download per-conversation

### 4.2 Chunk fetch strategy

When user initiates download (or auto-download triggers):

1. For each chunk, try locations in listed order:
   - `Direct`: attempt direct connection to sender if they're known-online; timeout 5s, fall through on failure
   - `Mailbox`: `BLOB_FETCH` to the specified mailbox with the blob_id
   - `Inline`: already have it, skip
2. Parallel fetches with bounded concurrency (default 4)
3. As each chunk arrives, verify hash, decrypt, verify Poly1305 tag, write decrypted bytes to temp assembly file at offset `chunk_index * 256 KiB`

### 4.3 Resume

Every chunk is content-addressed and self-authenticating. If a download is interrupted:
- Already-verified chunks remain in temp assembly file
- On resume, skip chunks already present (tracked via a small bitmap persisted alongside the temp file)
- Only missing chunks are re-fetched

### 4.4 Reassembly

1. When all chunks are fetched, trim last chunk to `FileRef.size`
2. Move temp assembly file to the user's downloads / attachments cache
3. Mark attachment as `Complete`, emit event to UI
4. Optionally verify whole-file hash if manifest included one (not required, chunks are individually authenticated)

### 4.5 Receive cache policy

- Downloaded files land in an app-managed attachment cache, not the user's `Downloads` folder
- User can "Save as..." to export outside the app cache
- Cache shows total size in Settings → Storage, with per-conversation breakdown
- Clear-cache options: per-conversation, per-type (videos only, etc.), or all

---

## 5. Mailbox protocol extension

Phase 3 extends the mailbox wire protocol with blob storage. This is separate from the existing `DEPOSIT` path, because blobs have different access semantics (content-addressed, uploader-owned, not tied to a recipient hash).

### 5.1 New frame types

| Type | Name | Direction | Purpose |
|------|------|-----------|---------|
| 0x90 | BLOB_UPLOAD | C→S | Store a blob owned by the uploader |
| 0x91 | BLOB_UPLOAD_OK | S→C | Returns blob_id |
| 0x92 | BLOB_FETCH | C→S | Retrieve a blob by id |
| 0x93 | BLOB_FETCH_RESULT | S→C | Blob bytes or error |
| 0x94 | BLOB_DELETE | C→S | Uploader deletes their blob |
| 0x95 | BLOB_DELETE_OK | S→C | Deletion confirmed |
| 0x96 | BLOB_QUOTA | C→S | Query uploader's quota usage |
| 0x97 | BLOB_QUOTA_RESULT | S→C | Current usage stats |

### 5.2 BLOB_UPLOAD

```
BLOB_UPLOAD {
  "uploader_pubkey": <32 bytes>,
  "ciphertext": <bytes, 24-byte nonce || XChaCha20-Poly1305 encrypted chunk>,
  "expires_at": <unix seconds>,
  "signature": <Ed25519 over auth string + sha256(ciphertext)>,
}
```

- Uploader must be registered with the mailbox (blob operations always require registration, unlike regular `DEPOSIT`)
- Server generates `blob_id = sha256(ciphertext)[..32]` — content-addressed, deduplicates automatically
- If blob already exists for this uploader, returns existing `blob_id` without storing a duplicate

Response:
```
BLOB_UPLOAD_OK { "blob_id": <32 bytes>, "expires_at": <actual>, "deduplicated": <bool> }
```

Error codes (in addition to the base set): `QuotaExceeded`, `BlobTooLarge`.

### 5.3 BLOB_FETCH

```
BLOB_FETCH { "blob_id": <32 bytes> }
```

No authentication required. Anyone holding the blob_id (which implies they received the manifest) can fetch. The blob_id doesn't leak anything useful on its own (random-looking hash).

Response:
```
BLOB_FETCH_RESULT { "ciphertext": <bytes> }
```

Or `ERROR { "code": "NotFound" }`.

Rate limiting: per-Tor-circuit limit (default 300 fetches/hour) to prevent mailbox operators being used as a file-hosting free-for-all.

### 5.4 BLOB_DELETE

```
BLOB_DELETE {
  "uploader_pubkey": <32 bytes>,
  "blob_ids": [<32 bytes>, ...],
  "signature": <Ed25519 over auth string>,
}
```

Only the original uploader can delete. Response: `BLOB_DELETE_OK { "deleted": <count> }`.

### 5.5 Quota and limits

Default operator limits for blob storage (separate from regular deposits):

| Limit | Default | Notes |
|-------|---------|-------|
| Max blob size | 1 MiB | One encrypted chunk's worth |
| Max blobs per uploader | 4096 | 1 GiB worth of chunks |
| Max total bytes per uploader | 1 GiB | |
| Blob TTL | 14 days | Typical message age; uploader can explicitly extend |
| Fetch rate per circuit | 300/hour | |

Operators can tune these in `mailbox.toml`.

### 5.6 Storage schema (server)

```sql
CREATE TABLE blobs (
  blob_id BLOB PRIMARY KEY,           -- sha256, 32 bytes
  uploader_hash BLOB NOT NULL,        -- sha256 of uploader pubkey
  ciphertext BLOB NOT NULL,
  uploaded_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  size_bytes INTEGER NOT NULL         -- denormalized for quota queries
);

CREATE INDEX idx_blobs_uploader ON blobs(uploader_hash);
CREATE INDEX idx_blobs_expiry ON blobs(expires_at);
```

Blob expiry sweep runs alongside the existing deposit sweep.

---

## 6. Direct peer-to-peer transfer

When both parties are online, skip the mailbox entirely. Cheaper, faster, no operator metadata.

### 6.1 When it's attempted

- Recipient is known to be online (active connection or recent presence)
- Attachment size exceeds inline threshold (< 32 KiB stays inline anyway)
- `Location::Direct` is listed first in `ChunkRef.locations`

### 6.2 Protocol sketch

Direct transfer runs on the existing authenticated connection (Noise + Tor) between the two peers. New frame types:

| Type | Name | Purpose |
|------|------|---------|
| 0x0C | CHUNK_REQUEST | Sender requests chunk(s) from peer (for receive) or offers them (for send) |
| 0x0D | CHUNK_DATA | Chunk ciphertext |
| 0x0E | CHUNK_ERROR | Unavailable / declined |

Protocol is request/response: recipient requests specific chunk hashes, sender responds with matching ciphertext. Recipient doesn't need to know sender has the chunks — they just ask. If sender doesn't have them (already garbage-collected), recipient falls back to the mailbox `Location`.

> **TODO / FUTURE:** Full direct-transfer wire protocol spec including flow control, cancellation, partial response handling, and interaction with the existing connection's frame multiplexing. Current design assumes one chunk transfer at a time per connection; larger design needed for concurrent transfers without blocking application messages.

---

## 7. Metadata stripping

Media files carry more identifying information than users expect. Stripping is not optional in this app.

### 7.1 Per-type pipeline

| Input type | Library (Rust) | What's stripped |
|-----------|---------------|-----------------|
| JPEG, HEIC, TIFF | `kamadak-exif`, `img-parts` | All EXIF (GPS, camera model/serial, timestamps, software), XMP, IPTC, MakerNotes, ICC profile (re-added standard sRGB) |
| PNG | `png` crate, manual chunk filter | tEXt, iTXt, zTXt, tIME; keep only IHDR, PLTE, IDAT, IEND, transparency chunks |
| WebP | `image` crate re-encode | Re-encode drops all metadata |
| GIF | `image` crate re-encode | Comments extension dropped |
| MP4, MOV | `mp4parse` or external `ffmpeg` sandbox | udta atoms (location, author, software), meta atoms |
| PDF | `lopdf` | Document Info dict (author, producer, creation date), XMP metadata stream, form fields with PII, embedded files |
| Office (docx, xlsx, pptx) | `zip` + manual XML filter | `docProps/core.xml`, `docProps/app.xml`, revision history, comments with author |
| Audio (MP3, M4A, FLAC) | `lofty` | ID3v1/v2, Vorbis comments, iTunes atoms (except essentials like duration) |

### 7.2 UI disclosure

After stripping, user sees before sending:

```
Attachment: vacation.jpg (3.4 MB)

Removed from this file:
  • GPS location (Lat 37.7749, Lon -122.4194 — San Francisco area)
  • Camera: iPhone 14 Pro
  • Timestamp: 2024-07-03 14:32
  • Software: Photos 10.0

           [Send]    [Send with original metadata]    [Cancel]
```

The "Send with original metadata" button is there for legitimate cases (sending a photo to a journalist with verifiable provenance, sharing a document with deliberate authorship). It requires a tap each time — never set as default.

### 7.3 Unknown or unhandleable types

If the file type isn't in the stripping table (rare: archives, executables, unknown formats), show:

```
⚠  This file type can't be automatically scanned for personal information.

   Skattr will send it exactly as-is. If it came from another app or
   service, it may contain hidden data like your account identifier,
   creation timestamps, or usage history.

           [Send anyway]    [Cancel]
```

### 7.4 The "strip but not re-encode" question

For some image types (JPEG especially), stripping metadata without re-encoding preserves pixel-perfect quality. For others (PNG re-encoded to a different compressor), output bytes differ from input but content is identical.

Default: strip-only where possible, re-encode only when necessary. Users sending a JPEG get bit-identical pixel data; users sending a weird format get a re-encoded version with a disclosure note.

> **TODO / FUTURE:** Dedicated metadata-stripping specification with per-format handlers, test vectors, known-tricky cases (animated WebP, multi-track video, encrypted PDFs, password-protected archives). Each format handler needs its own review. This spec assumes library choices but doesn't pin versions or test inputs.

---

## 8. File type handling matrix

Not every file type is equally safe to preview. The app maintains an explicit handling policy per MIME type.

| Category | Example types | Preview in-app? | Open externally? | Notes |
|----------|--------------|----------------|-----------------|-------|
| Image (common) | jpeg, png, webp, gif | Yes, via decoder | Yes | Decode via `image` crate, render as bitmap |
| Image (raw/uncommon) | raw, svg, tiff | No | Yes with warning | SVG can contain scripts; never render in-app |
| Video | mp4, webm, mov | Yes, sandboxed | Yes | Use `ffmpeg` in sandbox; no audio autoplay |
| Audio | mp3, m4a, ogg, opus | Yes, decode-only | Yes | Waveform shown, no cover art from embedded metadata |
| PDF | pdf | First page preview only | Yes with warning | Do not embed a PDF renderer; render first page as image |
| Office | docx, xlsx, pptx | Icon only | Yes with warning | Word/Excel have macro history; warn |
| Archive | zip, tar, 7z | Icon + file list | Yes with warning | Show contents tree; don't auto-extract |
| Text | txt, md, csv | Yes | Yes | Render as plain text; no HTML evaluation |
| Code | py, rs, js, etc. | Yes, syntax highlighted | Yes | Treated as text |
| Executable | exe, dmg, sh, app, msi | No preview | Yes with strong warning | "This file can run code on your computer" |
| Unknown | anything else | Icon only | Yes with warning | Generic file icon |

### 8.1 Executable warning

For anything that can execute:

```
⚠  This file can run code on your computer.

   If you weren't expecting this file from this person, don't open it.
   Even if you trust the sender, their device could be compromised.

   File: malicious_looking_name.exe (2.1 MB)
   From: Alice

                                 [Cancel]    [Open anyway]
```

### 8.2 Never-allowed types

- `.lnk`, `.scf`, `.desktop` and other shortcut/launcher formats (can trigger actions without "opening")
- `.html`, `.htm` when the renderer would be a full browser (acceptable if rendered as source text only)

These types are sendable but preview is replaced with an "export to see" affordance.

> **TODO / FUTURE:** Dedicated secure-preview architecture doc. Sandbox design (OS-level: macOS sandbox-exec / Windows AppContainer / Linux bubblewrap; or language-level: WASM decoder), process isolation model, crash containment, timeout policies, resource limits per preview. The current spec says "sandboxed decoders" without specifying the sandbox boundary — a real implementation needs that nailed down before shipping video preview.

---

## 9. Size privacy

Attachment size leaks content category in ways observers can exploit. Mitigations stack.

### 9.1 Always-on: chunk padding

Fixed 256 KiB chunks mean file size is quantized to chunk count. A 237 KiB file and a 12 KiB file both upload as one 256 KiB chunk. A 1.1 MiB file and a 1.0 MiB file both upload as five chunks.

### 9.2 Always-on: size bucketing

Despite chunk quantization, chunk *count* is still visible to a mailbox observer. Bucket manifest-declared sizes for transmitted sizes:

| Bucket | Range | Padded to |
|--------|-------|-----------|
| Tiny | 0–32 KiB | inline (no bucket) |
| Small | ≤ 1 MiB | 1 MiB |
| Medium | ≤ 16 MiB | 16 MiB |
| Large | ≤ 64 MiB | 64 MiB |
| Huge | ≤ 256 MiB | 256 MiB |
| Very huge | ≤ 1 GiB | 1 GiB |

"Padded to" means: enough dummy chunks are added at the end to reach the bucket size. Recipient knows the true size from `FileRef.size` and trims the assembly.

This wastes bandwidth — a 1.1 MiB file becomes a 16 MiB upload — so it's a setting (default: on for files under 4 MiB where the waste is bounded, off for larger files where the cost is significant).

### 9.3 Opt-in: upload timing decorrelation

Rather than uploading all chunks back-to-back in 1 second, spread uploads over 10–60 seconds with jitter. Defeats "X uploaded 40 chunks at 14:03:12" traffic signatures.

Cost: latency. Default off; available as a per-conversation setting.

### 9.4 Opt-in: cover uploads

Periodically upload random-content chunks to the mailbox, then delete them. Blends real upload patterns with noise.

Cost: bandwidth, storage churn. Default off.

> **TODO / FUTURE:** Formal traffic-analysis threat model for attachments. Quantify what a mailbox operator, a Tor exit observer, and a global passive adversary can learn from each combination of the above mitigations. This spec has reasonable defaults but hasn't been stress-tested against modern traffic-analysis research.

---

## 10. Voice messages

Voice notes are just a tightly-constrained audio attachment.

### 10.1 Recording

- Codec: Opus at 16 kbps, mono, 16 kHz sample rate
- Container: Opus in Ogg
- Max duration: 5 minutes (300s × 16 kbps = 600 KB — fits in ~3 chunks)
- Recording UI: press-and-hold to record, release to stop; or tap-to-start / tap-to-stop for accessibility
- Waveform generated during recording, embedded as `FileRef.thumbnail`

### 10.2 Metadata

Voice messages have no source metadata to strip (recorded by the app, not imported). The encoder produces deterministic output given the same input, with no timestamps or device identifiers in the file.

### 10.3 Playback

- Inline playback in the message bubble (play/pause, scrub, speed)
- Waveform visualization during playback
- No autoplay when message arrives (privacy: notification sounds leak arrival to anyone in earshot)

---

## 11. UX considerations

### 11.1 Send progress

During send, the message bubble shows:

```
┌─────────────────────────────────┐
│ [attachment icon] vacation.jpg  │
│ 3.4 MB · Uploading 8/14 chunks  │
│ ████████████░░░░░░  60%          │
│                        [Cancel] │
└─────────────────────────────────┘
```

States: `Preparing` (stripping, chunking), `Uploading N/M`, `Delivering` (uploading manifest), `Sent`, `Delivered to N of M` (groups).

### 11.2 Receive progress

Attachment arrives as manifest + thumbnail; full file not yet downloaded:

```
┌─────────────────────────────────┐
│ [thumbnail]  vacation.jpg       │
│              3.4 MB             │
│              [Download]         │
└─────────────────────────────────┘
```

On tap, inline progress:

```
│              ████████░░░░  65%  │
│              Downloading…       │
│              [Cancel]           │
```

### 11.3 Cancellation

- Send cancel: stop further chunk uploads, send delete requests for already-uploaded chunks to the mailbox, remove local temp files, mark message as canceled
- Receive cancel: stop further fetches, keep partial temp file for 24h in case user resumes, then delete

### 11.4 Retries

- Failed chunk uploads/fetches retry with exponential backoff (1s, 2s, 4s, ... 5min cap)
- After 5 failed attempts, surface to user: "Upload stalled. Retry, or send over a different mailbox?"
- Never silently give up on a user-initiated transfer

### 11.5 Per-recipient delivery state in groups

For a 10-person group chat, the UI shows aggregate state (`Delivered to 8 of 10`) with a tap-to-expand list showing each recipient's state (`Alice: read · Bob: delivered · Carol: pending`).

Delivery state is based on mailbox-deposit ACKs and, for read, on explicit read receipts (off by default, see design doc).

### 11.6 Attachment cache UI

Settings → Storage shows:

```
Attachment cache                          2.1 GiB
├─ Images                                 840 MiB
├─ Videos                                 1.1 GiB
├─ Audio                                   34 MiB
└─ Other files                            120 MiB

                          [Clear by type]    [Clear all]
```

Also visible: per-conversation breakdown, ability to auto-delete older than N days.

### 11.7 Export

User can "Save as..." any received attachment to outside the app cache. This is an explicit action, separate from viewing, to make the user aware that export moves the file outside the app's privacy boundaries.

---

## 12. Integration with the implementation plan

The Phase 3.D workstream from `skattr-implementation-plan.md` is replaced/expanded to:

### 3.D Attachments (Phase 3, weeks 17–24 portion)

**3.D.1** File ingest + type detection + size checks
**3.D.2** Metadata stripping pipeline — image formats (JPEG/PNG/WebP/GIF) for v1
**3.D.3** Thumbnail generation — images only for v1; video/PDF thumbnails flagged as Phase 3 stretch
**3.D.4** Chunker + XChaCha20-Poly1305 encryptor with test vectors
**3.D.5** `FileRef` manifest type + CBOR serde
**3.D.6** Mailbox `BLOB_UPLOAD` / `BLOB_FETCH` / `BLOB_DELETE` protocol + server impl
**3.D.7** Client upload orchestration with bounded concurrency
**3.D.8** Client fetch orchestration with resume support
**3.D.9** Size bucketing logic (default-on for small files)
**3.D.10** UI: send progress, receive progress, cancel, retry
**3.D.11** Attachment cache manager + Settings storage UI
**3.D.12** Voice message recorder + player
**3.D.13** Secure preview: images (decode-and-render), audio (decode-only), text
**3.D.14** Executable warning + external-open flow
**3.D.15** Integration tests: end-to-end send/receive for each attachment type

Estimated: 4–5 weeks of the 8-week Phase 3 window (groups and attachments share the phase).

### Moved to Phase 4 (hardening)

- Video thumbnail generation (requires sandboxed ffmpeg; security review first)
- PDF preview (first-page render)
- Office format metadata stripping (docx/xlsx/pptx XML filtering)
- Direct peer-to-peer chunk transfer (complex, marginal benefit for v1)

### Moved to post-1.0

- Cover-upload traffic analysis mitigation
- Archive content tree view
- Full secure-preview sandbox architecture for video

---

## 13. Future work (flagged throughout)

Collected in one place for easy reference. Each of these deserves its own spec doc before implementation.

### Deferred to Phase 4 or later

| Topic | Reason | Minimum design needed before starting |
|-------|--------|--------------------------------------|
| **Direct peer-to-peer chunk transfer** | Wire protocol complexity, marginal latency benefit | Flow control, cancel, multiplexing with app messages |
| **Per-format metadata stripping spec** | Each format has tricky edge cases; needs test vectors | Per-format handler doc with known-tricky inputs catalogued |
| **Secure preview sandbox architecture** | Preview of video/PDF needs OS-level sandboxing; platform-specific | Sandbox boundary choice (bubblewrap / AppContainer / sandbox-exec / WASM), IPC model, crash policy |
| **Traffic-analysis threat model** | Need quantitative analysis of mitigations, not hand-waving | Adversary model, measurement methodology, mitigation cost/benefit |
| **Cover upload scheme** | Dummy-upload patterns need to be indistinguishable from real | Generation distribution, storage/deletion timing, interaction with real traffic |

### Post-1.0 candidates

| Topic | Why not v1 |
|-------|-----------|
| **Live voice calls** | WebRTC over Tor has severe latency issues; substantial research required |
| **Live video calls** | Same as voice plus bandwidth concerns; TURN servers reintroduce metadata |
| **Peer-assisted delivery (swarm)** | Privacy analysis of swarm participation is nontrivial |
| **Attachment search (OCR/indexing)** | Requires local compute; text indexing of decrypted attachments raises new forensic concerns |
| **Collaborative docs / shared editing** | Different product category; likely separate from messaging app |
| **Expiring attachments** | Requires cooperative-client assumption that chunks actually get deleted; honest UX is hard |
| **Attachment forwarding** | Privacy question: does forwarding re-upload or reference original blob? Design needed |

### Open design questions (v1 decisions to revisit)

- Is 256 KiB the right chunk size? Benchmark against alternatives (64 KiB, 1 MiB) once v1 has traffic data
- Is size bucketing default-on the right call? Measure actual bandwidth waste vs. privacy benefit with beta users
- Should thumbnails live alongside chunks in the mailbox for large files, or always inline in the manifest (waste space for small-but-common case)?
- Should the "send with original metadata" option be gated behind a confirmation dialog *every time* or remembered per-contact?

---

## Cross-cutting: security checklist for attachment code

Because attachments expand the attack surface significantly, anything touching attachment code should pass through this checklist in review:

- [ ] All file reads bounded (no `read_to_end` on untrusted files)
- [ ] All decoders run either out-of-process or via memory-safe libraries with fuzz coverage
- [ ] Filenames sanitized before display and before writing to disk (no path traversal, no control characters, no RTL override characters)
- [ ] MIME types are validated, not trusted from the sender
- [ ] Magic-byte detection disagrees with claimed type → warn user
- [ ] Temp files created with restrictive permissions (0600 on Unix, equivalent on Windows)
- [ ] Temp files cleaned up on all exit paths (panic, cancel, success)
- [ ] Zero-length files rejected early
- [ ] Decompression bombs detected (ratio check on archives before extraction)
- [ ] Chunk-count bounds enforced on manifests (§1.3)
- [ ] Nonce randomness from OS CSPRNG, never derived
- [ ] AAD always includes attachment_id and chunk_index
- [ ] Hashes verified before decryption attempt
- [ ] Cache directory not world-readable
- [ ] No attachment data in logs, ever (not even file names at info level)
