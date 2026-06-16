# PICKUP — resume state (2026-06-16)

Scratch/handoff doc to resume work after an environment update/reboot. Delete
once we're rolling again. Authoritative project state lives in `CLAUDE.md`
(Repository state) and the specs/plans under `docs/superpowers/`.

## TL;DR — where we are

- **Phases 1 + 2 (v1.0 audit): complete and merged to `master`.**
- **Phase 3.A (attachment core): implemented, all reviews passed, NOT yet
  merged.** It sits on branch `phase-3a-attachment-core`. Two steps remain:
  (1) final whole-branch review, (2) merge to `master`. Both were blocked
  mid-step by a transient platform 529 overload (the command-safety classifier
  + subagent dispatch were unavailable), not by any code problem.
- Everything is committed. Working tree should be clean.

## IMMEDIATE NEXT ACTION (resume here)

Finish Phase 3.A via `superpowers:finishing-a-development-branch` (the same flow
used for every prior phase):

1. **Re-run the authoritative gate** on `phase-3a-attachment-core`:
   ```bash
   . "$HOME/.cargo/env"
   cargo fmt --all -- --check
   cargo clippy --workspace --exclude skattr-ui --all-targets --all-features -- -D warnings
   cargo deny check
   cargo test -p skattr-core --features test-harness
   cargo test -p skattr-tests -- --test-threads=1
   cargo build -p skattr-cli
   ```
   Expected: all green. (It was last verified green on commit `8e09ad5`; the
   final commit `5c728c3` is a test-only amend confirmed clippy-clean + e2e
   passing.) NOTE: multi-test-name filters go AFTER `--`.
2. **(Optional) re-dispatch the final whole-branch reviewer** — it 529'd before
   returning a verdict. Diff range: `76262cb..<tip of phase-3a-attachment-core>`.
   Spec: `docs/superpowers/specs/2026-06-16-phase-3a-attachment-core-design.md`.
3. **Merge** (the established per-phase flow): from `master`,
   `git merge --no-ff phase-3a-attachment-core -m "Merge phase-3a-attachment-core: attachment core (Phase 3.A)"`,
   re-verify tests on the merged tip, then `git branch -d phase-3a-attachment-core`.
   Do NOT push master (see "Push state" below).
4. Delete this `PICKUP.md`.

## Phase 3.A status (branch `phase-3a-attachment-core`)

All 7 tasks implemented via subagent-driven-development; each spec-reviewed ✅
and code-quality-reviewed ✅, with review nits folded:

| Task | Commit | Delivered |
|---|---|---|
| 1 | `f43a366` | `AttachmentManifest`/`ChunkRef` (CBOR, carried in `Kind::File`), `AttachmentErrorKind`, `CoreError::Attachment(#[from] …)`, `sanitize_filename` (path-traversal + Unicode bidi-override spoofing hardened) |
| 2 | `3149897` | `INFO_ATTACH_V1` + `chunk_key_material(file_key, index) -> Zeroizing<[u8;56]>` (key‖nonce via HKDF) |
| 3 | `5305459` | pure chunker: split → per-chunk XChaCha20-Poly1305 → SHA-256 ciphertext-hash → manifest; oversize→`TooLarge` |
| 4 | `30bc146` | pure reassembler: verify ciphertext-hash **before** decrypt → AEAD → temp+rename (no partial output) + `ChunkSource` trait |
| 5 | `b22e8e2` | image EXIF/metadata stripping via `img-parts` (NEW dep); magic-bytes-before-mime; malformed-image-claiming-image-mime → reject |
| 6 | `52587a3` | storage: migration `0015` (`attachments` + `attachment_chunks`) + `AttachmentRepo` + ciphertext-only on-disk `ChunkStore`/`StoreSource` |
| — | `a145590` | `deny.toml`: documented ignore of `RUSTSEC-2026-0173` (`proc-macro-error2` unmaintained — pre-existing arti-transitive, fails on `master` too) |
| 7 | `5c728c3` | end-to-end local round-trip test (strip→chunk→store→reassemble) |

Design locks: no new crypto (existing XChaCha20-Poly1305 / SHA-256 / HKDF,
domain-separated `"skattr-attach-v1"`); `ciphertext_hash` = SHA-256(ciphertext)
content-addresses + tamper-binds each blob for the offline path; staged blobs
are ciphertext-only (no plaintext at rest); plaintext is transient. No
transport/wire/protocol change — 3.A is pure/local.

## Phase 3 remaining (after 3.A merges)

Decomposition (in the 3.A spec preamble): `3.A → 3.B → 3.C`, `3.D` after the
wire stabilizes. Offline IS in v1.0 scope.

- **3.B — direct transfer**: one additive `Frame` chunk type; sender streams
  chunks over the live Noise/MLS connection; receiver verifies + reassembles;
  live `run_with_transport` file-round-trip guardrail. Online-only.
- **3.C — offline transfer**: chunk blobs via the mailbox path (reuse the frozen
  `Deposit` vs. extend ADR 0006 — decide in 3.C's spec), inheriting Phase 2
  caps; resume semantics (3.A already persists per-chunk receipt state).
- **3.D — UI**: attach / send / download / inline preview / progress / size limits.

Each sub-project: brainstorm → spec (+ADR if protocol/auth) → writing-plans →
subagent-driven execution → two-stage review per task → finishing-branch merge.

## v1.0 remaining beyond Phase 3

- **Phase 4 — release integrity, docs, signing**: honest user-facing docs;
  working download-verification chain; **real minisign + PGP keypairs** (both are
  committed PLACEHOLDERS today — `docs/install/minisign.pub` and `SECURITY.md`'s
  PGP block; maintainer procedure in `docs/install/README-MAINTAINER-MINISIGN.md`).
  Both MUST be real before any public `v0.1.0` tag.
- **v1.1+ deferrals (disclosed as absent in the threat model):** third-party
  audit; metadata-minimization (size padding, timing jitter, cover traffic);
  multi-member groups (>2); real onion-key rotation (Task 23.5 —
  `RotateOnion` only bumps the ContactCard version today); first-contact
  `Welcome` mailbox fallback (currently direct-only); reactions/edit/delete/
  typing/read-receipts (inert placeholders); multi-device.

## Process conventions to preserve

- **Workflow:** superpowers skills — brainstorming → writing-plans →
  subagent-driven-development (fresh subagent per task + spec-review then
  code-quality-review per task; fold cheap review nits via Edit + `--amend`) →
  finishing-a-development-branch.
- **Model routing** (CLAUDE.md "## Model routing"): opus = architecture/design/
  review; sonnet-4-6 = standard/integration; haiku-4-5 = mechanical 1–2-file.
- **Commit trailer** for this work: `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- **Branching:** never commit feature work on `master`; branch, then merge
  `--no-ff`. Cargo isn't on PATH — prefix `. "$HOME/.cargo/env" &&`.
- **Per-task gate:** `cargo fmt --all` + `cargo clippy --workspace --exclude
  skattr-ui --all-targets --all-features -- -D warnings` + `cargo test -p
  skattr-core --features test-harness`. Final gate adds single-threaded
  `cargo test -p skattr-tests -- --test-threads=1` + `cargo deny check` + CLI build.
- **No `unwrap`/`expect` in non-test code; secrets zeroize; no new crypto; no
  pubkeys/onions/ciphertext logged above debug.**

## Push state (read before pushing)

Per the standing convention this session has been **local-only** — `master` is
~70+ commits ahead of `origin/master` and nothing was pushed. For the
reboot we are pushing the work branch as a remote backup. **CI constraint:**
`.github/workflows/ci.yml` triggers `on: push: branches:[main, master]` and
`on: pull_request`. Therefore:
- Pushing the **feature branch** `phase-3a-attachment-core` (no PR opened) does
  NOT trigger CI — and because it descends from `master`, it carries the entire
  local commit history to `origin` as a backup. ✅ This is the CI-safe backup.
- Pushing **`master`** WOULD trigger CI → avoid it until we deliberately accept a
  CI run. Do not open a PR (also triggers CI) unless intended.
- `release.yml` triggers only on `v*` tags — unaffected by branch pushes.
