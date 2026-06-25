# Phase 4.B — Documentation Truthfulness — Design

**Date:** 2026-06-25
**Status:** Approved (brainstorm) — ready for implementation planning
**Depends on:** Phase 4.D merged (`fe4c83b`); v1.0 pull-forward-vs-disclose
decision record (`docs/superpowers/specs/2026-06-23-v1.0-pull-forward-vs-disclose-decisions.md`)
**Sibling sub-projects:** 4.A (release/CI integrity), 4.C (UI robustness + D1
mitigation) — out of scope here.

---

## Purpose

Make every user-facing and design document **true against the shipped v1.0
code**. The v1.0 readiness audit found false capability/security claims on the
front page and a threat model that under-discloses real metadata leaks; the
pull-forward decision record fixed *which* limitations must be disclosed. This
sub-project executes that disclosure and corrects the audit's
documentation-truthfulness findings.

**Pure docs. No code changes. No key generation.** (The real minisign/PGP keys
are a separate maintainer/pre-tag task — see Non-goals.)

---

## Truthfulness principle (the bar for every edit)

A claim stays only if it is **true of the merged code today**. Anything
aspirational becomes either:

1. **removed**, or
2. **explicitly marked deferred / out of v1.0 scope**, with the reason.

In a security document, **silence about a real limitation is itself a
truthfulness gap** — the threat model must name the leak, not omit it.

Every factual correction is paired with the code that is its source of truth
(see Verification). If the code and a doc disagree, the code wins and the doc
is corrected.

---

## Scope

Decided during brainstorming:

- **In scope:** core user-facing docs (`README.md`, `THREAT_MODEL.md`,
  `SECURITY.md`, `docs/install/*`) **plus** the audit-flagged internal
  doc-drift (`ARCHITECTURE.md`, `docs/PROTOCOL.md`, `docs/skattr-deep-dives.md`,
  `docs/operations/passphrase-recovery.md`, `docs/skattr-design.md`).
- **Keys:** **disclose-only** — the docs truthfully state the keys are
  placeholders and the verification chain is not yet usable. Generation is a
  separate task.
- **First-run wizard copy:** **left as-is** (see Non-goals).

### The disclosure set (must appear, truthfully)

From the decision record (D1/D2/D3 + standing list) and audit T2-6:

| Tag | Disclosure | Primary surface |
|---|---|---|
| **D1** | First contact requires both peers online at once (Welcome is direct-only). **Baseline wording only** — the "app retries automatically" append is contingent on 4.C shipping the joiner auto-retry; 4.B uses the always-true baseline, 4.C amends if it ships the retry. | `THREAT_MODEL.md`, `README.md` |
| **D2** | "Rotate onion" is degenerate: republishes the *same* address with a new card version; true rotation is v1.1. | `THREAT_MODEL.md`, `README.md` |
| **D3** | Offline attachments are best-effort: held ~7 days, dropped if never fetched within the window; files > 10 MiB transfer only while both peers are online. | `THREAT_MODEL.md`, `README.md` |
| **Standing v1.1 list** | No third-party audit; no metadata-minimization (size padding, timing jitter, cover traffic/polling); no multi-member groups (> 2); reactions / edits / delete-for-everyone / typing / read receipts inert; no multi-device; **the stable recipient-hash mailbox-correlation leak**. | `THREAT_MODEL.md` (+ already partly in `SECURITY.md`) |
| **Withhold-detection (audit T2-6)** | Downgrade from claimed-defense to truth: MLS generation gaps make withholding detectable *in principle*, but the client surfaces **no alert** today — latent, not an active defense. | `THREAT_MODEL.md` |
| **Identity-hash correlation (audit T2-6)** | State plainly: the recipient hash is stable and non-rotating; two colluding mailboxes can confirm a shared recipient. | `THREAT_MODEL.md` |

Disclosure wording may be lifted from the decision record's "Disclosure language
seeds" section. The threat model and README **cite the decision record**.

---

## Per-file change specification

### User-facing

**`README.md`**
- Remove **"Group-ready … groups scale to ~50 members in v1"** → "v1.0 is 1:1 (a
  2-member MLS group); multi-member groups are deferred to a later release."
  Source of truth: the 2-member gate in `crates/core/src/mls/group.rs`.
- Replace the stale status line ("Phase 1 … almost complete") with an honest
  current-state summary **without internal phase numbers** (they mean nothing to
  a reader): messaging, security hardening, and attachments work; release
  hardening (docs/signing) is in progress.
- Move shipped features (send, FTS search, mailboxes, UI, attachments) out of any
  "what doesn't work yet" framing.
- Add a brief **Limitations** pointer: D1/D2/D3 one-liners + "see
  `THREAT_MODEL.md`."

**`THREAT_MODEL.md`** (largest lift)
- Re-stamp the header: drop "v0 / end of Phase 0 / will be revised before any
  public release" → the v1.0 threat model, dated, citing the decision record.
- **Withhold-detection:** rewrite the malicious-mailbox (A4) claim to the truth
  (detectable in principle via MLS generation gaps; no client alert).
- **Identity-hash correlation:** add a plain statement under malicious-mailbox —
  stable, non-rotating recipient hash; two colluding mailboxes can confirm a
  shared recipient.
- **Attachments:** disclose they are **in** v1.0 scope with the D3 best-effort
  caveat (currently the doc is silent on attachments).
- Add a consolidated **"v1.0 known limitations / out of scope"** block carrying
  D1, D2 (onion rotation degenerate), D3, and the standing v1.1 list — each one
  line, citing the decision record.

**`SECURITY.md`**
- Keep the existing three-tier known-limitations structure (it is largely
  accurate); tighten "deferred to v1.1+" phrasing to "out of v1.0 scope" where it
  reads as merely delayed.
- **Placeholder honesty:** add an explicit note that the **PGP key** (this file)
  and the **minisign public key** (`docs/install/minisign.pub`) are
  **placeholders** — the verification chain is **not usable yet** and becomes
  real only when the maintainer publishes the real keys (separate pre-tag task).
  Do not describe the chain as if it works today.

**`docs/install/README.md`** (+ a one-line caveat where linux/macos/windows
reference verification)
- Add a prominent caveat at the top of the verification steps: the published
  minisign key is currently a **placeholder**; signature verification **will not
  succeed** until the real key ships with v0.1.0. Keep the step-by-step procedure
  (it is correct *procedure*) — just stop implying it works now.

### Internal doc-drift (targeted corrections, not rewrites)

**`ARCHITECTURE.md`**
- Socket name `daemon.sock` → `ipc.sock`.
- "migrations through 0006" → through 0016.
- "`mailbox::client` is `todo!()`" → fully implemented.
- Spot-check remaining structural claims against the current module tree while in
  the file.

**`docs/PROTOCOL.md`**
- Correct the codepoint `0x0001` → `0x0003` to match the code.
- Verify every other codepoint in the file against the real frame-type
  constants; correct any further mismatches found.

**`docs/skattr-deep-dives.md`**
- Mark **superseded** (with a pointer to the authoritative source) rather than
  rewriting: Part 3's HELLO/REGISTER/SUBSCRIBE mailbox protocol (replaced by ADR
  0006); §1.2's dead `Daemon::start/execute/shutdown` API; §3.9's mailbox-policy
  defaults that diverge from the shipped `Policy` defaults. These are historical
  design docs; a superseded-banner + pointer is the correct, honest treatment.

**`docs/operations/passphrase-recovery.md`** (safety-critical correction)
- The "lost passphrase → delete `skattr.sqlite.age`" advice causes **data loss**:
  the DB key is seed-derived, so the correct recovery is **restore from the seed
  phrase** (re-derives the key and decrypts history). Rewrite the procedure to
  lead with seed-restore; reframe deletion as a last-resort wipe carrying an
  explicit **"this destroys your message history"** warning.
  Source of truth: the seed → HKDF DB-key derivation.

**`docs/skattr-design.md`**
- §1.5: replace the abandoned 256-bit AES suite with the locked
  `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519`.

---

## Verification (definition of done)

Prose has no unit tests; truthfulness is verified by **claim-by-claim
cross-check against the code**:

1. **Claims checklist (authored in the plan):** every corrected factual claim is
   paired with the `file:line` (or const/function) in the codebase that is its
   source of truth — e.g. 2-member gate → `mls/group.rs`; codepoint `0x0003` →
   the frame-type const; seed-derived DB key → the HKDF derivation; degenerate
   rotation → `Command::RotateOnion` / `contact/rotation.rs`. The reviewer
   verifies each pair.
2. **Dedicated truthfulness review** (opus): the reviewer reads each doc diff
   *against the cited code*, asking one question per claim — "is this now true?"
   — and sweeps for **new** overclaims introduced by the edits. This is a
   correctness review, not a style review.
3. **No CI impact:** docs are `paths-ignore`d in `ci.yml`; `markdownlint` is not
   gated and is **not** introduced here.
4. **CodeRabbit** reviews the PR as the second pair of eyes (per the project's
   completion-review rule).

A pre-merge sweep confirms no remaining doc in scope contradicts the shipped
code on any claim in the disclosure set or the per-file list.

---

## Non-goals (explicit)

- **Real key generation / CI-secret setup.** The minisign keypair and the
  `SECURITY.md` PGP key stay placeholders here; 4.B only discloses that
  truthfully. Generating the real keys + configuring `MINISIGN_SECRET_KEY` /
  `MINISIGN_PASSWORD` is a separate maintainer action (tracked for 4.A / pre-tag)
  — it requires secret key material and is not a documentation task.
- **First-run wizard copy.** It is already accurate and does not oversell. It is
  silent on D1/D3, but onboarding — before the user has any contacts — is the
  wrong place for those caveats; they belong **in context** (when adding a
  contact / sending a large file), which lands with **4.C's** in-UI work.
- **ADR backfill.** The 5 missing ADRs (Noise pattern, `h_transport` binding, MLS
  ciphersuite, `skattr://invite/v1` link format, envelope wire format) are real
  authoring work and a separate task before the Phase 5 external audit.
- **`docs/V1.0-READINESS-AUDIT.md` itself** is a point-in-time record and is not
  edited.

---

## Risks

- **Re-introducing an overclaim while fixing another.** Mitigated by the
  truthfulness review's explicit "new overclaims" sweep.
- **Correcting a doc against code that itself changes later.** Low for v1.0
  (the code is frozen for these areas); the claims-checklist makes any future
  drift easy to re-verify.
- **Scope creep into rewriting historical design docs.** Mitigated by the
  "mark superseded + pointer" treatment for `skattr-deep-dives.md` rather than
  wholesale rewrites.
