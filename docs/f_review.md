# Skattr — Full Codebase Review

**Date:** 2026-07-17
**Reviewer:** Claude Code (8 parallel read-only subagents + controller verification)
**Scope:** the entire workspace — `core`, `mailbox`, `cli`, `tests`, and the `ui` crate (Rust shell + SvelteKit frontend + Tauri config). ~63k lines read in full.
**Nature:** read-only review. **No code was changed.** This is a findings document; nothing here has been applied.

> **On the crypto:** this reviews how Skattr *uses* Noise_XK / MLS / `age` / Argon2id (correctness, key handling, invariant drift). It does not modify or re-implement any cryptographic primitive.

---

## Method & how to read severities

Eight subsystem reviewers each read their files completely, verified findings against call sites, and graded them. The controller then **independently re-verified every "Critical" against the actual code** — and all four dissolved on inspection (dead scaffold, user-chosen path, config-only edge, non-secret CLI arg). Net: **0 verified Critical.** Important findings below are reviewer results the controller found credible on the cited evidence; the ones marked ✅ were additionally code-verified by the controller.

Severities: **Important** = fix before it bites (security defense-in-depth, real correctness edge, or maintainability risk). **Minor** = worth doing, low stakes. **Nit** = cosmetic. Two findings duplicate already-tracked issues **#38 / #39** and are excluded.

---

## Executive summary

The codebase is **in strong shape**: careful zeroization of primary secrets, disciplined HKDF domain separation, no secret logging at info+, correct MLS ratchet serialization, atomic storage transactions on the hot paths, a clean pure-OS daemon lock, correct peer authentication (Unix peer-cred + Windows SID/DACL), and a safe UI (message bodies/nicknames render as text, file-open paths are canonicalize+prefix confined, CSP blocks inline script). Test coverage is genuinely good and driven through the real assembly.

The "downgraded Criticals" are all quality issues, not live bugs. The **most valuable real findings** are a small cluster of **defense-in-depth security hardening** items and **one correctness edge** in delivery scheduling:

1. **Unbounded 16 MiB allocation from an attacker-controlled frame length** on the authenticated p2p receive path (before `snow` rejects it). ✅
2. **Inbound MLS KeyPackages aren't pinned to the locked ciphersuite** at the trust boundary. ✅
3. **Direct-redelivery starvation:** the retry tick's query mixes direct + mailbox rows under one limit and skips the mailbox ones, so a mailbox backlog can crowd out direct retries. ✅
4. **Codec-error framing rebuild silently discards buffered bytes** (a hostile client can self-desync a mailbox connection); the code also contradicts its own doc-comment.
5. **`run_with_transport` maintainability:** 20 positional parameters + setup logic triplicated across three entry points.

None are exploitable for message decryption, key compromise, or remote code execution. Everything below is hardening, correctness-edge, or cleanliness.

---

## Important findings (verified-credible)

| # | Area | File:line | Finding | Grade |
|---|------|-----------|---------|-------|
| I-1 ✅ | Security (transport) | `core/src/transport/connection.rs:137` | `recv` does `vec![0u8; cipher.len()]` where `cipher` is a `Frame::MlsApp` payload up to the outer `MAX_FRAME_SIZE` (16 MiB). An authenticated peer can force a 16 MiB zeroed alloc per frame before `snow` rejects anything >65535. Bounded (authed peer, single conn) but an attacker-length-driven allocation. **Direction:** reject `cipher.len() > 65535` before allocating, symmetric with the `send`-side cap at `connection.rs:93`. | Important |
| I-2 ✅ | Security (MLS) | `core/src/mls/key_package.rs:82-98` | `from_bytes` runs OpenMLS `validate(..., Mls10)` but never asserts `kp.ciphersuite() == 0x0003`. A cross-suite peer KeyPackage passes the trust boundary and is only rejected later with an opaque error. Doc even claims it checks "ciphersuite." **Direction:** compare to `MLS_CIPHERSUITE` right after `validate`; fix the doc. | Important |
| I-3 ✅ | Correctness (delivery) | `core/src/delivery/peer.rs:582` | Direct retry tick calls `ob.due(now, 32)`, which returns **all** outbox kinds by `next_retry_at`; the loop then `continue`s past every `Mailbox` row. A backlog of due mailbox rows fills the 32-row window with skips, starving genuinely-due **direct** redelivery (fallback path; main send is dial-on-demand). **Direction:** add a `due_direct(now, limit)` query (mirror of `due_mailbox`) so the batch is spent on actionable rows. | Important |
| I-4 | Correctness/robustness (mailbox server) | `mailbox/src/server.rs:118-137` (+ doc `:92-94`) | On a codec error the accept loop rebuilds `Framed` via `into_inner()` + `Framed::new`, discarding buffered bytes. A hostile client can pipeline `[garbage][valid-Deposit]` in one write; the valid tail is dropped and framing desyncs (self-inflicted, single-connection — CBOR/type checks still prevent forgery). The doc-comment says codec errors *close* the connection; the code keeps it open. **Direction:** close on codec error (matches the doc) or preserve `read_buffer()` across rebuild. | Important |
| I-5 | Maintainability (daemon) | `core/src/daemon/state.rs:273` + `:104-265, 600-682, 700-783` | `run_with_transport` takes ~20 positional params (incl. 4 near-identical `IdentityKey` copies); the pre-bootstrap setup (5× vault open, pool, migrations, backfills, sweep) is duplicated across `run_with_sink` / `run_loopback` / `run_loopback_with_mailbox`, so a change must be mirrored in three places or drift. **Direction:** extract a `bootstrap_vault_and_pool()` helper; group identities + sweep channels into small structs (~12 params). | Important |
| I-6 | Correctness (storage) | `core/src/storage/attachments.rs:255-274` | `AttachmentRepo::delete()` runs two separate `DELETE`s (chunks, then row) via `with_mut`, not a transaction; a crash between them orphans chunk rows (no cascade FK). Low blast radius (~KB, retention cleans up) but violates the crate's atomicity invariant. **Direction:** wrap both in `pool.transaction(...)`. | Important (low-impact) |

### Downgraded from "Critical" (verified not live bugs)
- **`Daemon::start/shutdown/execute` are `todo!()` stubs** (`state.rs:44-65`). Reviewer flagged `shutdown()` as a panic-on-call Critical — but **nothing calls them**; production runs through `Daemon::run` → `run_with_sink`, and teardown is the caller-supplied `shutdown` future. → **Minor: delete the dead scaffold** (it misleads readers and the doc-comments describe behavior that doesn't exist).
- **`--passphrase-file` path visible in `/proc/*/cmdline`** (`cli/src/main.rs`). The *path* is not the secret (file perms protect the content); this is standard CLI behavior. → **Minor: docs note.**
- **Config-store rollback can drop a concurrent pending patch** (`ui/.../stores/config.ts:100`). Config-only, requires two edits racing a failed flush. → **Minor.**
- **`SaveAttachment` accepts an unvalidated dest path** (`dispatch.rs:1850`). By design — it's the user's "Save As" target from the UI dialog, not attacker-controlled (contrast `OpenAttachment`, which *is* confined to the cache). → **Minor: doc the intent.**

---

## Cross-cutting themes (where the Minors cluster)

These are the patterns worth a single focused pass each, rather than one-off fixes.

### A. Zeroization hygiene (defensible today, inconsistent)
Primary secrets (`Seed`, `Mnemonic`, `IdentityKey`, `InvitePsk`, storage key, `h_transport`) are correctly `Zeroize`/`Zeroizing`. The gaps are transient copies and one misleading rationale:
- `identity/key.rs:158-178` — comment claims the `Sha512::digest` output "auto-zeroizes via ZeroizeOnDrop"; **it does not** — only the explicit `full.zeroize()` wipes it. The code is correct but the false rationale could lead a future editor to delete the real wipe. **(Minor, safety-doc — fix the comment.)**
- `identity/seed.rs:69` + `derive.rs:78-82` — `from_storage_bytes` takes `[u8;32]` by value; the caller derefs a `Zeroizing`, leaving a brief un-wiped stack copy. **(Nit — accept `Zeroizing<[u8;32]>`.)**
- `storage/outstanding_invites.rs:167-174, 230-232` — PSK read into a plain `Vec<u8>` then copied into `Zeroizing<[u8;32]>`; the `Vec` isn't wiped. **(Minor.)**
- `identity/vault.rs:130-131` — CBOR serialize buffer not `Zeroizing` (holds only sealed ciphertext + public params, so no secret — flagged for consistency only). **(Minor.)**
- `commands.rs` / `dispatch.rs:1688` — `ChangePassphrase` passphrases cross IPC as plain `String`, wrapped in `Zeroizing` only on decode. Correct, but no doc marks the field security-sensitive or states the client's zeroize responsibility. **(Minor — doc.)**

### B. Doc / comment drift
- `delivery/attachment/mod.rs:18-24` & `chunk_transfer.rs:21` — stale `256 KiB` chunk-size math (now 48 KiB / 49 KiB); the "≈2 MiB window" is actually ≈384 KiB.
- `daemon/state.rs:130, 347` — comments hard-code line numbers (`:162`, `:213`) that will drift.
- `mailbox/server.rs:92-94` — doc says codec errors close the connection; code keeps it open (see I-4).
- `delivery/hub.rs` — module/`ensure_mailbox_fallback` doc describes a `(0..len).cycle().skip()` walk; code uses `(primary+offset)%n`.
- Phase-number references that no longer match: `dispatch.rs:1047` ("Phase 1.F"), `cli/src/main.rs:7-8` ("Phase 0 … placeholder"), various `storage/messages.rs` phase comments.

### C. Dead / stubbed / `#[allow(dead_code)]` code
Consistent cleanup opportunity — several `pub` stubs and unreachable variants:
- `daemon/state.rs:44-65` — `Daemon::start`/`execute`/`shutdown` `todo!()` stubs (see downgraded-Critical).
- `contact/rotation.rs:21` — `pub async fn rotate_onion` body is `todo!()` (a reachable panic if ever called from lib code — return a typed error instead). *(Note: real rotation is tracked as #36; this is just the stub's panic-shape.)*
- `mls/state_machine.rs` — `GroupState::PendingJoin` is never constructed (dead state; reconcile the "three variants" doc).
- `transport/arti_transport.rs:17-28`, `tor.rs:41` — `#[allow(dead_code)]` / unused `socks_port` "used by Task 7" markers — verify against current wiring and drop if stale.
- `delivery/hub.rs:77` (`MailboxFallbackShared.identity`), `delivery/peer.rs:31` (`PeerCtrl::Shutdown`), `mailbox/poll.rs:110` (`run_one_poll_tick` — test-only, duplicates `poll_dispatch_once` with *different* delete semantics → real maintenance hazard), `mailbox/poll.rs:161` (`NoopDispatch` duplicating a default-trait no-op).

### D. Duplication (drift risk)
- `sanitize_filename` + bidi/control filtering implemented **twice** (`attachment/manifest.rs:75` vs `delivery/chunk_transfer.rs:207`) with *different fallback names* and only one having the 200-char cap — exactly the divergence duplication invites. **Unify.**
- Mailbox `MailboxFrameCodec` duplicated across `core/src/mailbox/codec.rs` and `mailbox/src/codec.rs` (deliberate crate split, ADR-0006-frozen — but a future v2 type-byte must be mirrored by hand).
- CLI contact-resolution block (`ListContacts` → `resolve_contact` → exit 6) copy-pasted across `send`/`send_file`/`tail`/`chat`/`search`/`export`/`prune`. Factor one helper.
- Test helpers `sha256` / `now_ms` duplicated across test files; belong in `loopback_harness`.

### E. Performance (all acceptable at v1 scale; flagged for growth)
- **Mailbox `Store::insert`** (`mailbox/src/store.rs:92-169`) recomputes full-table `SUM(LENGTH(ciphertext))` / `COUNT(DISTINCT)` up to ~4× per deposit **inside one transaction while holding the connection mutex** — every depositor serializes behind these scans. Fine now; consider a maintained counters row if deposit volume grows.
- **Chunk `verify()`** (`chunk_transfer.rs:113-119`) is an O(chunks) `iter().find()` per received chunk → ~O(n²) over a large attachment (~2100 chunks for 100 MiB). Build an `index → ChunkRef` map (manifests are dense, so `index` == position works).
- **`ContactRepo::list()`** N+1 (`contacts.rs:151-156`, acknowledged) — one `latest_card()` per contact; batch with a `MAX(version)` join before the list grows past "dozens."
- **UI**: attachment-manifest memo map is unbounded over a long session (`lib/attachments.ts:53`); `attachment_progress` events aren't debounced (`+page.svelte:125`) → store churn on large transfers (DOM is virtualized, so cosmetic).
- **Blocking calls in async** (Minor): `std::fs::create_dir_all` + `set_permissions` and `Pool::open` run directly in `run_with_sink` (`state.rs:115/124/136`). Fast on local disk; could stall the executor on a slow/NFS data dir — consider `spawn_blocking` for `Pool::open`.
- **Deposit rate-limit token spent before cheap validation** (`mailbox/dispatch.rs:99-120`) — a guaranteed-reject deposit (bad TTL / full box) still burns a global token. Defensible as attempt-throttling; move the pure size/version/TTL checks ahead of the global `try_acquire` to avoid a shared-bucket drain via guaranteed-reject requests. **(Minor.)**

---

## Notable robustness Minors (not in a theme)
- `daemon/state.rs:789` — `wipe_open_cache` runs on clean startup/shutdown but not on a *panicked* early return; a decrypted open-attachment could linger in `<data_dir>/cache/open/` (0700, inside the encrypted dir). **Direction:** RAII guard so it always runs.
- `daemon/logs.rs:193-217` — redactor strips 64-char hex + 56-char `.onion` + labeled fields, but not <64-char hashes or multiline bodies in tracing fields. Belt-and-suspenders already; extend the heuristic and test against real Arti logs.
- `daemon/smoke.rs:246` — `safe_tempdir` falls back to world-writable `/tmp/...-no-home` when `$HOME` unset (Arti `fs-mistrust` will reject it); prefer `$RUNNER_TEMP`/`$TMPDIR` first.
- `delivery/strip.rs:47` — metadata stripping is **image-only**; PDFs/Office docs relay author/GPS metadata verbatim. A real (documented) metadata-leak surface — confirm it's tracked (relates to #41 metadata-minimization; consider a dedicated note).
- `mls/group.rs:198` — `let _ = signer_from_identity(...)` swallows a genuine store error, surfacing later as an opaque "welcome process" failure.
- UI `+page.svelte:117-141` — event dispatcher has no `else` for unknown event variants; a new core `Event` built without rebuilding the shell is silently dropped. Add `console.warn`.

---

## Per-subsystem verdict

| Subsystem | Verdict | Headline |
|-----------|---------|----------|
| identity / MLS / envelope / invite / contact | **Strong** | Excellent secret hygiene + HKDF separation; pin inbound KeyPackage ciphersuite (I-2). |
| transport / Noise / mailbox client+server | **Strong** | ADR-0006 freeze intact, auth-before-processing correct; cap the recv allocation (I-1) + close-on-codec-error (I-4). |
| delivery / attachments | **Good, intricate** | Correct AEAD/opacity, path-traversal-safe, cross-lane idempotent; fix direct-retry starvation (I-3); unify the two `sanitize_filename`s. |
| storage / migrations / age | **Strong** | Great crash resilience, zero SQL-injection surface, atomic hot paths; make attachment-delete transactional (I-6). |
| daemon assembly / lifecycle / lock | **Good** | Sound start/stop ordering, clean OS lock; refactor `run_with_transport` (I-5); delete dead stubs. |
| IPC / commands / dispatch | **Strong** | Correct peer-auth both platforms, append-only wire, redacted errors; doc the passphrase/zeroize contract. |
| CLI + integration tests | **Good** | Real-assembly test coverage; standardize exit codes, tighten one poll loop, factor CLI duplication. |
| UI shell + Svelte frontend | **Strong** | **XSS-safe** (text rendering, escaped search, hex-validated deep-links), paths confined, CSP tight; config-rollback edge + unbounded memo. |

---

## Verified-clean (coverage evidence — checked, no action)
- **No secret logging**: grep of all crypto modules found zero pubkey/onion/PSK/body at any log level; redacting `Debug` impls throughout.
- **HKDF domain separation**: every use has a distinct centralized `info` label; separation is unit-tested.
- **Vault AEAD**: Argon2id m=64MiB/t=3/p=4, fresh salt+nonce per encrypt, AAD binds version, decrypt lands in `Zeroizing` (never a `Vec`), atomic write.
- **Noise key separation**: identity keys are the Noise statics, distinct from the Ed25519 onion key; whole-handshake 30 s timeout; PSK3 fails closed on unilateral/mismatched.
- **Mailbox freeze + auth**: signature verified before any store read/delete; nonce single-use; recipient-scoped deletes; storage caps enforced atomically with reject-after-expired (never evicts accepted messages).
- **Storage**: all queries parameterized (zero injection surface); sentinel + re-encrypt-on-boot; schema-downgrade guard; FTS5 trigger sync.
- **Lock discipline**: no `std::sync::Mutex` guard held across `.await` in the mailbox server or the delivery hub/peer paths (spot-checked and confirmed).
- **UI security**: message bodies/nicknames rendered as text; `SearchPalette` escapes before `@html`; deep-link IDs hex+colon validated; `open_file`/`reveal_in_folder` dual-canonicalize + prefix-confine to the downloads/cache dir; CSP `script-src 'self'`.

---

## Suggested order of attack (described, not coded — nothing applied)

1. **Two cheap security caps (I-1, I-2).** A length check before the recv allocation and a ciphersuite equality check after KeyPackage validate — both small, both close attacker-controlled-input classes at the trust boundary.
2. **Direct-retry starvation (I-3).** Add `due_direct` and point the retry tick at it — a real correctness edge on the offline/mixed-outbox path.
3. **Codec-error handling (I-4).** Close the mailbox connection on codec error (also resolves the doc contradiction).
4. **Storage atomic delete (I-6)** and the **`wipe_open_cache` RAII guard** — small correctness/robustness wins.
5. **Cleanliness pass** (own PR, no behavior change): unify `sanitize_filename`, delete the dead `Daemon` stubs / unreachable variants, refactor `run_with_transport` (I-5), factor CLI contact-resolution, fix the doc-drift cluster.
6. **Performance** is not urgent at v1 scale; revisit `Store::insert` aggregates and chunk `verify()` before scaling deposit volume / attachment size.

**Already tracked (not re-filed):** dup `AttachmentReceived` on simultaneous lane completion → **#38**; offline-attachment TTL-loss / forever-pending / 10 MiB cap → **#39**; real onion rotation → **#36**; metadata-minimization (incl. the PDF-metadata surface above) → **#41**.

---

*Read-only review — no files were modified. Full per-subsystem reports were produced during this pass and can be re-generated on request.*
