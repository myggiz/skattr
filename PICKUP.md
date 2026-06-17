# PICKUP — resume state (2026-06-17, post 3.B implementation)

Scratch/handoff doc to resume work after an environment update/reboot.
Authoritative project state lives in `CLAUDE.md` (Repository state) and the
specs/plans under `docs/superpowers/`.

## TL;DR — where we are

- **Phases 1, 2 (2.A–2.D), and 3.A: complete and merged to local `master`.**
  3.A merged at `9a132d1`.
- **Phase 3.B (direct attachment transfer): IMPLEMENTED on branch
  `phase-3b-direct-attachment-transfer`** (not yet merged). Final whole-branch
  review "Ready to merge"; gate green for everything 3.B touches — `fmt`/`clippy
  --all-features -D warnings`/`test --features test-harness` all pass on
  core/mailbox/cli/tests; loopback guardrail + actor-level resume green; zero
  new deps. (`cargo-deny` not installed locally; `skattr-ui` clippy hits a local
  `sda3` filesystem `os error 74` in the Tauri build script — env only, ui job
  is separate in CI and the branch doesn't touch `crates/ui`.)
- **Next workstream: Phase 3.C (offline transfer)** — chunk blobs via the
  mailbox path; cross-session resume. Not started.
- `master` is **local-only** (not pushed). 3.A pre-merge history backed up on
  `origin/phase-3a-attachment-core`.

## IMMEDIATE NEXT ACTION (resume here)

Decide how to integrate `phase-3b-direct-attachment-transfer` (merge to local
`master` vs. PR) via `superpowers:finishing-a-development-branch`. Then start the
**Phase 3.C design pass** (`superpowers:brainstorming` → spec → ADR? →
`writing-plans` → subagent-driven execution → finishing-branch).

The section below is the original 3.B design sketch — now realized in
`docs/superpowers/specs/2026-06-17-phase-3b-direct-attachment-transfer-design.md`
+ ADR 0010; kept only as historical context (delete once 3.C is underway).

3.B = **direct attachment transfer (online, both peers reachable)**. 3.A already
provides: the manifest rides in MLS via `Kind::File`; chunk blobs are opaque
AEAD ciphertext keyed from the manifest; `ChunkStore` (stage), `AttachmentRepo`
(per-chunk receipt state), reassembler + `ChunkSource`, caps, metadata strip.

3.B adds:
- **ADR + an additive transport `Frame`** for chunk movement (`FrameType` has
  free bytes 0x0A+; 0x03/0x05–0x09 are in use). Carries `{attachment_id, index,
  ciphertext}` (opaque — Noise-encrypted by the channel, NOT MLS-wrapped).
- **Send path:** `Command::SendFile { contact, path }` (additive wire) → strip →
  chunk → stage (`ChunkStore`) → persist manifest (`AttachmentRepo`,
  `direction='out'`) → send the `Kind::File` manifest as an MLS message → drive
  chunk delivery through the per-peer `delivery::peer` actor.
- **Receive path:** on a `Kind::File` manifest → persist (`direction='in'`) →
  obtain chunks → per chunk verify hash + `ChunkStore::put` + `mark_received` →
  on completion reassemble to a download location → emit `Event::AttachmentReceived`
  (+ progress events).
- **Per-peer actor arms** for the chunk frames, interleaved with normal messages
  (don't starve chat); **in-session resume** on reconnect (re-request missing
  indices — 3.A persisted receipt state for this).
- **Guardrail:** two loopback daemons round-trip a multi-chunk file end-to-end
  through the real `run_with_transport`, byte-identical, metadata stripped.

**The key open design question to put to the user FIRST at the 3.B brainstorm:**
push vs. pull chunk delivery. Recommended: **pull/request-driven** (receiver,
holding the manifest, requests missing indices) — resume falls out naturally and
the receiver gets flow control; push+ack is simpler. Other decisions: where
reassembled files land (download dir/config), progress-event granularity,
concurrent-transfer gating.

Boundaries: 3.B is online-only direct; **3.C** = offline mailbox-blob path +
cross-session resume (in v1.0 scope); **3.D** = Tauri attach/preview/progress UI.
3.B must be provable via a CLI/integration round-trip without the UI.

## Phase 3 remaining after 3.B

- **3.C — offline transfer:** chunk blobs via the mailbox path (reuse the frozen
  `Deposit` vs. extend ADR 0006 — decide in 3.C's spec), inheriting Phase 2 caps;
  cross-session resume.
- **3.D — UI:** attach / send / download / inline preview / progress / size limits.

## v1.0 remaining beyond Phase 3

- **Phase 4 — release integrity, docs, signing:** honest user-facing docs;
  working download-verification chain; **real minisign + PGP keypairs** (both are
  committed PLACEHOLDERS — `docs/install/minisign.pub`, `SECURITY.md` PGP block;
  procedure in `docs/install/README-MAINTAINER-MINISIGN.md`). MUST be real before
  any public `v0.1.0` tag.
- **v1.1+ deferrals (disclosed in the threat model):** third-party audit;
  metadata-minimization (size padding, timing jitter, cover traffic);
  multi-member groups (>2); real onion-key rotation (Task 23.5); first-contact
  `Welcome` mailbox fallback (currently direct-only); reactions/edit/delete/
  typing/read-receipts (inert); multi-device.

## Process conventions to preserve

- **Workflow:** superpowers — brainstorming → writing-plans →
  subagent-driven-development (fresh subagent per task + spec-review then
  code-quality-review; fold cheap nits via Edit + `git commit --amend`) →
  finishing-a-development-branch.
- **Model routing** (CLAUDE.md "## Model routing"): opus = architecture/design/
  review; sonnet-4-6 = standard/integration; haiku-4-5 = mechanical 1–2-file.
- **Commit trailer:** `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- **Branching:** never commit feature work on `master`; branch, then merge
  `--no-ff`, verify on the merged tip, delete the local branch. Cargo isn't on
  PATH — prefix `. "$HOME/.cargo/env" &&`.
- **Per-task gate:** `cargo fmt --all` + `cargo clippy --workspace --exclude
  skattr-ui --all-targets --all-features -- -D warnings` + `cargo test -p
  skattr-core --features test-harness`. Final gate adds single-threaded
  `cargo test -p skattr-tests -- --test-threads=1` + `cargo deny check` + CLI build.
  (Multi test-name filters go AFTER `--`.)
- **No `unwrap`/`expect` in non-test code; secrets zeroize; no new crypto; no
  pubkeys/onions/ciphertext logged above debug.**

## Push state (read before pushing)

- **Local-only convention all session.** `master` is ~78 commits ahead of
  `origin/master`; `master` is NOT pushed.
- **CI:** `.github/workflows/ci.yml` triggers `on: push: branches:[main, master]`
  and `on: pull_request`. So pushing `master` or opening a PR runs CI; per the
  current "no CI if possible" instruction, **do not push master / open a PR**
  unless intentionally accepting a CI run. `release.yml` fires only on `v*` tags.
- **Backups on origin (CI-safe, no PR):** `origin/phase-3a-attachment-core`
  (pre-merge state `0c83109` — carries the full pre-merge history) and
  `origin/master-backup-2026-06-16` (the post-merge `master` tip, pushed as a
  reboot backup). Pushing to these non-`master`/`main` refs does NOT trigger CI.
  After the reboot, local `master` is the source of truth; reconcile/clean these
  backup refs as desired.

(Delete this file once 3.B is well underway.)
