# Phase 4.D — T3 Security Hardening (design)

**Date:** 2026-06-23
**Status:** Approved (brainstorm) — ready for implementation plan
**Depends on:** Phases 1–3 complete (all merged). Consumes the decision record
`2026-06-23-v1.0-pull-forward-vs-disclose-decisions.md` (P1/P2/P3/P5 pulled
forward) and the v1.0 audit's cluster-6 T3 findings
(`docs/V1.0-READINESS-AUDIT.md` §§163–173).
**No protocol change, no ADR, no new feature.** 4.D is a bundle of small,
independent security/correctness hardening fixes across the existing code.

## 1. Goal

Close the audit's cluster-6 T3 hardening findings (minus one rejected item) and
land the four pulled-forward fixes from the decision record, as one cohesive
hardening sub-project. Every item is a small, isolated change to existing code;
none touch the wire protocol, MLS/handshake semantics, or add a subsystem.

## 2. Scope

**In scope — nine items:** the five kept audit cluster-6 findings (renumbered
**Items 1–5** in §4) plus the four pulled-forward fixes (**P1, P2, P3, P5**). See
§4.

**Explicitly dropped — the HS-key encryption "salt" finding** (`AUDIT.md:164`;
NOT one of the renumbered Items 1–5 below). The audit
(`AUDIT.md:164`) claims `hex(HKDF(seed))`-as-passphrase yields "deterministic
ciphertext, no per-file salt, correlatable." **This premise is false.**
`transport/hs_key.rs:58/83` calls `age::Encryptor::with_user_passphrase`, whose
scrypt recipient writes a **fresh random 16-byte salt per encryption** (the age
`-> scrypt <salt> <work>` stanza). Re-encrypting the same key therefore produces
**different** ciphertext — it is neither deterministic nor correlatable. The only
real (and security-neutral) observation is that scrypt is unnecessary work on an
already-256-bit-entropy derived key; switching the format to XChaCha20-Poly1305
would **break every existing on-disk HS-key file** (the daemon could not decrypt
the saved onion key → a new `.onion` address → existing contacts can no longer
route to the user). Rejected: no security benefit, real migration risk. Recorded
here so 4.B's threat-model rewrite does not list it as an open issue.

**Out of scope:** the rest of Phase 4 (4.A release/CI, 4.B docs, 4.C UI
robustness); anything requiring a protocol/ADR change.

## 3. Constraints

- **No custom crypto.** None of the nine items introduce or modify a crypto
  primitive. Item 2 (Zeroizing) changes only buffer *handling*, not algorithms.
- **No `unwrap()`/`expect()` in library/command bodies** (existing rule);
  `#[cfg(test)]` may use them under the established `#[allow(...)]`.
- **License header on every new/edited file** (GPLv3 core/cli/tests; the UI
  shell is GPLv3).
- **Cross-platform:** item 3 (0700) and item 5 (getuid) are Unix-only paths
  guarded by `#[cfg(unix)]`; Windows behavior is unchanged (no-op / existing).
- **Toolchain:** Rust gates run under the pinned **1.95.0** (the repo's
  `rust-toolchain.toml` floats to 1.96 locally, which SIGSEGVs the arti tree;
  `rustup override set 1.95.0`). CI uses its own stable.
- **Regression safety is paramount** for the two widest-blast-radius items:
  item 4 (the migration runner — runs on every DB open) and P2 (the attachment
  completion path). All existing core tests, the Phase-3 attachment guardrails,
  Vitest, and Playwright e2e must stay green.

## 4. The nine items

Each item is a self-contained change with its own test. Locations are current as
of this spec (verified against the code, not the 2026-06-12 audit line numbers).

### Item 1 — Log-redaction bypass (security: info leak)
`daemon/dispatch.rs:1573,1578` log the full `CoreError` via `tracing::warn!(?err …)`.
`?err` renders the error's `Display`/`Debug`, which can carry **untrusted input
text** (malformed invite URLs containing base64 key material, decrypt-error
detail). That text lands in the ring buffer streamed to the UI via `TailLogs`,
bypassing the redactor (which only acts on formatted info+ text).
**Fix:** log the error **category** only — keep `?kind` on the typed-error line
(1573) and drop `?err`; on the internal-error line (1578) log a stable category
string / `err.kind()` rather than the raw error. Review `:433`
(`warn!(?e, "add_contact: could not build self-card …")`) and apply the same
category-only treatment if `e` can carry untrusted text.
**Test:** unit — install a capturing `tracing` subscriber, trigger a typed and an
internal error, assert the emitted line contains the category and **not** a
sentinel untrusted-body marker. Mirror the existing mailbox log-redaction test.

### Item 2 — Secret CBOR buffers not zeroized
`identity/key.rs:116-135` (`sign_cbor`/`verify_cbor`) serialize the signed body
into a plain `Vec<u8>` scratch buffer; bodies can include the invite PSK.
**Fix:** wrap the scratch buffer in `zeroize::Zeroizing` so it wipes on drop.
`mls/key_package.rs:130` (`secret_bytes.to_vec()` handed to OpenMLS `from_raw`)
is **best-effort**: OpenMLS takes the `Vec<u8>` by value and owns it thereafter,
so we cannot guarantee its wipe — minimize the lifetime of our copy and add an
explanatory comment; do **not** widen the OpenMLS surface.
**Test:** type-level — the scratch buffer's type is `Zeroizing<Vec<u8>>` (compile
assertion); no behavioral change to signatures.

### Item 3 — `data_dir` created world-readable
`storage/pool.rs:65` `create_dir_all(data_dir)` uses the umask default (commonly
0755). The data dir holds the encrypted DB + sentinels.
**Fix:** on Unix, create with / enforce mode `0700` (`DirBuilderExt::mode(0o700)`
or a `set_permissions` after create); `#[cfg(unix)]`-guarded, no-op on Windows.
**Test:** `#[cfg(unix)]` — open a pool in a tempdir, stat the dir, assert
`mode & 0o777 == 0o700`.

### Item 4 — No schema-downgrade guard (data-integrity)
`storage/migrations.rs` reads `current = MAX(schema_version)` then applies
migrations `> current`. An **older** binary opening a **newer** DB
(`current > max(known migrations)`) applies nothing and silently "succeeds,"
then operates on a schema it does not understand.
**Fix:** after reading `current`, if `current > ALL_MIGRATIONS.max().version`,
return a typed refuse-error (new `StorageErrorKind` variant, e.g.
`SchemaTooNew { found, max_known }`) before applying anything. Highest blast
radius — every DB open passes here.
**Test:** stamp `schema_version` to `max+1` in a temp DB, call the migration
runner, assert it returns the refuse-error and does not mutate the DB.

### Item 5 — Fragile uid fallback (auth boundary)
`daemon/ipc/server/unix.rs:97` `current_uid()` stats `/proc/self`, then falls
back to `$UID`, then **`0`**. On non-`/proc` Unix (macOS — in the CI matrix)
this is fragile, and the `→ 0` fallback could spuriously match root and weaken
the IPC peer-cred check.
**Fix:** use `unsafe { libc::getuid() }` (`libc` is already a **direct** dep of
core, `Cargo.toml:85`; POSIX `getuid()` is infallible). Update the
"without using `unsafe`" doc comment to reflect the deliberate, trivially-safe
syscall on a security boundary. The `$UID` env override was a test convenience;
the boundary stays covered by the separately-testable `check_peer_uid`.
**Test:** existing `check_peer_uid` tests cover the boundary; add one smoke
assert that `current_uid()` returns the process uid.

### Item P1 — Opener path not confined to downloads dir (security boundary)
`crates/ui/src/attachments.rs` `validate_openable` canonicalizes + asserts
regular-file but does not confine the path to `<data_dir>/downloads`. Today this
is safe only by context (paths are daemon-authored from
`Event::AttachmentReceived`; webview locked by `script-src 'self'`).
**Fix:** thread the downloads dir into `validate_openable` (and `open_file` /
`reveal_in_folder`) and reject any canonical path not under it
(`canon.starts_with(&downloads)`). Makes the commands safe-by-construction. The
downloads dir is already resolved in the shell (`main.rs` setup); pass it via
managed state or recompute from `app_data_dir()/skattr/downloads`.
**Test:** Rust shell unit — a temp file inside the downloads dir → `Ok`; a file
outside → `Err` containing "outside download dir".

### Item P2 — 3.C completion not atomic across lanes
The attachment-complete check (`received_indices().len() >= total` → reassemble →
emit `AttachmentReceived` → `set_status('complete')`) is not atomic with the
status flip, and the direct (3.B) + offline (3.C) lanes run in separate tasks.
Simultaneous completion can double-fire `AttachmentReceived` (event-level only —
no corruption; the UI store keys on `attachment_id` — but a duplicate desktop
notification can surface).
**Fix:** add a compare-and-set repo method
`set_status_if_pending(attachment_id) -> rows_affected`
(`UPDATE attachments SET status='complete' WHERE attachment_id=? AND
status='pending'`) and gate the reassemble-and-emit on `rows_affected == 1`, so
exactly one lane fires the event. Locate both finalize sites in planning.
**Test:** repo-level — first `set_status_if_pending` returns 1, second returns 0;
plus a finalize-path test asserting one emit under a simulated double-completion.

### Item P3 — Swallowed sweep writes (observability)
`delivery/chunk_sweep.rs:54,72,89,90,91,104` drop write results with `let _ =`
(`mark_deposited`, `delete_for_attachment`, `set_status`, `reschedule`).
**Fix:** `warn!` on failure of the meaningful writes (keep the non-fatal
control flow; just surface the error). No behavior change.
**Test:** none dedicated (observability); existing `chunk_sweep` tests stay
green.

### Item P5 — Unsound promotion cast (type safety)
`stores/conversation.ts` `sendFile` promotes the optimistic bubble with
`{ …, __optimistic: false } as unknown as OptimisticMessage` — the brand
requires `__optimistic: true`.
**Fix:** introduce a small `PromotedMessage` type (or drop the optimistic brand
fields on promotion) so the cast is sound.
**Test:** `pnpm check` (svelte-check) passes; existing composer/conversation
tests stay green.

## 5. Testing & verification strategy

- **Per-item unit tests** as listed in §4 — TDD (failing test first). Core items
  use Rust `#[cfg(test)]`; UI items use the Rust shell test (P1) or `pnpm check`
  + existing Vitest (P5).
- **No new live `run_with_transport` guardrail.** These are hardening fixes, not
  new transport behavior; the audit's live-guardrail rule applies to new
  behaviors. Coverage = targeted unit tests + **all existing Phase-3 guardrails
  staying green** (the regression net, especially for items 4 and P2).
- **Local pre-push gates (run by the developer before each push; Rust under
  pinned 1.95.0):** `cargo fmt --all --check` **(run locally — this was the CI
  miss in 3.D)**, `cargo clippy -p skattr-core -p skattr-ui --all-targets
  --all-features -- -D warnings`, `cargo test -p skattr-core -p skattr-ui`,
  `pnpm test`, and `pnpm check`. **`pnpm check` stays a local gate in 4.D — it
  is deliberately NOT added to CI here.** Wiring `pnpm check` (and Playwright)
  into the CI `ui` job is **Phase 4.A's scope (T2-3)**, and it is blocked on
  first fixing the 4 pre-existing `ConfigPatch.download_dir` svelte-check errors
  in the settings pages (out of 4.D scope). 4.D's only `pnpm check` obligation is
  to **introduce no new** svelte-check errors in the files it touches (P1, P5).
- **CI gates (the existing `ui` + workspace jobs):** the merge-gating CI is
  unchanged by 4.D — `cargo fmt`/clippy/test on the workspace and `pnpm build` +
  `pnpm test` in the `ui` job (the Vitest gate landed in 3.D). 4.D adds no CI
  steps.
- **Review tiers** (subagent-driven): security/auth-sensitive items —
  **1, 2, 5, P1, P2 → opus review** (CLAUDE.md "crypto/protocol/auth → second
  reviewer," read broadly to include info-leak / secret-handling / auth-boundary
  / completion-integrity). Mechanical items — **3, 4-impl, P3, P5 → standard
  review.** (Item 4's data-integrity weight still merits a careful review.)

## 6. Sequencing

The nine items touch disjoint files and have **no inter-item dependencies** —
each is its own implementation task. Execute **risk-ordered**, front-loading the
widest blast radius so regressions surface while attention is fresh:

1. **First (delicate / high blast radius):** Item 4 (migration runner), P2
   (completion CAS).
2. **Core security (opus review):** Item 1 (log leak), Item 2 (zeroize), Item 5
   (uid/auth), P1 (opener boundary).
3. **Mechanical last (standard review):** Item 3 (perms), P3 (logs), P5 (cast).

Order is risk-management only; functionally any order works. New error variants
(item 4 `SchemaTooNew`) are additive and map onto existing `StorageErrorKind`
patterns — no wire-format impact (IPC error surface already categorizes).

## 7. Deliverables

- The nine fixes with their tests, landed on a `phase-4d-t3-hardening` branch.
- A spec note (this §2) that audit item 2 was verified-and-rejected, for 4.B to
  cite in the threat-model rewrite.
- Gate green: Rust clippy/test (1.95.0) + fmt, Vitest, e2e; existing Phase-3
  guardrails unbroken.
- PR → CodeRabbit babysit → merge, per the completion-review rule.
