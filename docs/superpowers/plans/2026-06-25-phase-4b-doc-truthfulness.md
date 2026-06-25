# Phase 4.B — Documentation Truthfulness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every user-facing and audit-flagged internal document true against the shipped v1.0 code — no false capability/security claims, every known limitation disclosed.

**Architecture:** Pure documentation edits. Each task corrects one coherent doc (or tightly-related pair), and follows a doc-adapted TDD rhythm: **(1) prove the current claim is false/stale by quoting the code that is its source of truth, (2) write the correction, (3) verify the correction now matches the code and introduces no new overclaim, (4) commit.** No code changes, no key generation.

**Tech Stack:** Markdown. Verification is `grep`/read against the Rust codebase (no compiler/test involvement — docs are `paths-ignore`d in CI).

## Global Constraints

- **Truthfulness principle:** a claim stays only if true of the merged code today; anything aspirational is either removed or explicitly marked deferred/out-of-v1.0-scope with the reason. In security docs, silence about a real limitation is itself a truthfulness gap.
- **Pure docs.** No `.rs`/`.ts`/config changes. No key generation. If a correction seems to need a code change, stop and flag it — it is out of 4.B scope.
- **Cite the decision record** (`docs/superpowers/specs/2026-06-23-v1.0-pull-forward-vs-disclose-decisions.md`) where THREAT_MODEL.md / README.md disclose D1/D2/D3.
- **No internal phase numbers in user-facing copy** (README, install docs, first-run). "Phase 1.G", "Phase 4" etc. mean nothing to a reader; describe state in plain feature terms.
- **Do not introduce markdownlint** or any docs CI gate.
- **Every factual correction is paired with its code source-of-truth** (`file:line`/const) and that pairing is recorded in the commit body or the task's report — this is the claims-checklist the final review verifies.
- **No ADR backfill, no first-run wizard edits, no real-key generation** — all out of scope (see spec Non-goals).
- Authoritative disclosure wording for D1/D2/D3 is in the decision record's "Disclosure language seeds" section; reuse it.

---

### Task 1: README.md — v1.0 truthfulness rewrite

The README is pervasively stale: it is written in mid-Phase-1 voice (status says "Phase 1 … almost complete"; "What works now (through Phase 1.E)"; "What doesn't work yet" lists mailbox/UI/groups-beyond-2/CLI-send — all of which now ship). It also carries the false "groups scale to ~50 members in v1" claim.

**Files:**
- Modify: `README.md`

**Interfaces:**
- Produces: nothing consumed by later tasks. Task 2 (THREAT_MODEL) and this task both disclose D1/D2/D3; keep wording consistent with the decision record's seeds.

- [ ] **Step 1: Prove the stale claims against code**

Run and read the output (these are the sources of truth):
```bash
sed -n '113,117p' crates/core/src/mls/group.rs        # the 2-member hard gate
ls crates/core/src/storage/migrations/ | tail -1      # latest migration (0016…)
grep -rn "AttachmentReceived\|SendFile" crates/core/src/daemon/ | head -3   # attachments ship
```
Confirm: `group.rs:115` is `if self.inner.members().count() >= 2 { … "already 2-member" }` (so >2 members is *not* supported — the "scale to ~50" claim is false); mailbox delivery, the Tauri UI, CLI send, and attachments are all shipped (Phases 1–3 complete per CLAUDE.md). The README's "what doesn't work yet" list is therefore wrong.

- [ ] **Step 2: Rewrite the status paragraph (lines 7–17)**

Replace the entire `**Status: Phase 1 on-line messaging almost complete.** …` paragraph with a v1.0-reality summary, no internal phase numbers:
```markdown
**Status: approaching a v1.0 release.** Skattr is a working 1:1 (two-party),
attachment-capable, Tor-only encrypted messenger: real two-daemon messaging in
both directions, first-contact via signed invite links, offline delivery through
semi-trusted mailboxes, file attachments (online and offline), and a desktop
(Tauri) app plus a CLI. Remaining work before tagging v1.0 is release hardening —
honest docs, a working download-verification chain, and real signing keys. See
[ARCHITECTURE.md](ARCHITECTURE.md) and [`docs/`](docs/) for the design, and
[THREAT_MODEL.md](docs/THREAT_MODEL.md) for security properties and limitations.
```

- [ ] **Step 3: Fix the false "groups" bullet (line 23)**

Replace:
```markdown
- **Group-ready.** Built on [MLS](https://datatracker.ietf.org/doc/rfc9420/). 1:1 is a 2-member group; groups scale to ~50 members in v1.
```
with:
```markdown
- **1:1, built on MLS.** v1.0 is two-party messaging — a 2-member [MLS](https://datatracker.ietf.org/doc/rfc9420/) group. Multi-member groups (> 2) are deferred to a later release.
```
Also fix line 24 (`Desktop first. Native app via Tauri (arriving in Phase 2).`) → drop the "(arriving in Phase 2)" — the Tauri app ships. Use: `**Desktop first.** Native app via Tauri, with a CLI alongside for power users and scripting.`

- [ ] **Step 4: Replace the three status sections**

Replace the whole block from `## What works now (through Phase 1.E)` through the end of `## What doesn't work yet` (lines 34–66) with a single honest pair of sections:
```markdown
## What works

- Create/restore a BIP39-backed identity (`skattr init` / `skattr restore`).
- At-rest encryption for identity, HS key, and the message database; backup/restore as a portable archive.
- Tor via embedded Arti: publishes a v3 onion service at a seed-derived address.
- Real two-party messaging in both directions over the production transport, with per-peer retry, ACK correlation, and exactly-once delivery (CI-proven).
- First contact via signed `skattr://invite/v1#…` links (single-use) and signed `ContactCard`s.
- `Noise_XK_25519_ChaChaPoly_BLAKE2s` transport auth bound to the MLS group (`h_transport` PSK).
- Offline delivery through semi-trusted mailboxes (deposit/fetch, failover, drain on removal).
- File attachments: send/receive with metadata stripping, online (direct) and offline (mailbox), inline image preview in the UI.
- Scrolling message history with full-text search; configurable retention.
- Desktop UI (Tauri) and a `skattr` CLI.

## Limitations (v1.0)

- **Two-party only.** Multi-member groups (> 2) are deferred.
- **First contact needs both peers online at once** — first contact is direct-only (no mailbox fallback); if your contact is offline when you add them, the connection will not complete until they are online.
- **"Rotate onion address" is not yet real** — it republishes your current address with a new card version; true address rotation is planned for a later release.
- **Offline attachments are best-effort** — held by a mailbox for ~7 days and dropped if never fetched; files over 10 MiB transfer only while both peers are online.
- Not a low-latency chat (Tor round-trips cost seconds), not mobile in v1.0, and not "anonymous" — your contacts know who you are.

See [THREAT_MODEL.md](docs/THREAT_MODEL.md) for the full security model and the v1.1 deferral list, and the [disclosure decision record](docs/superpowers/specs/2026-06-23-v1.0-pull-forward-vs-disclose-decisions.md).
```
(Leave the `## What Skattr is` / `## What Skattr isn't` framing sections at lines 19–32 in place, except the two edits in Step 3.)

- [ ] **Step 5: Verify no new overclaim + features list matches reality**

Re-read the rewritten README. Confirm every "What works" bullet maps to shipped code (cross-check against CLAUDE.md's Phase 1–3 "✅ complete" entries). Confirm no remaining "Phase N", no "~50 members", no "(arriving in Phase 2)", no "what doesn't work yet" listing shipped features.
```bash
grep -nE "Phase [0-9]|50 members|arriving in Phase|doesn't work yet" README.md || echo "CLEAN: no stale phase/overclaim markers"
```
Expected: `CLEAN`.

- [ ] **Step 6: Commit**

```bash
git add README.md
git commit -m "docs(4.B): rewrite README to v1.0 reality (drop ~50-members claim, stale status)"
```

---

### Task 2: THREAT_MODEL.md — v1.0 re-stamp + disclosure set

The threat model is self-bounded to "end of Phase 0 / pre-audit", overclaims withhold-detection as a defense, omits the stable identity-hash correlation leak, is silent on attachments, and frames mitigations as "Phase 1+/Phase 4". This task makes it the truthful v1.0 model.

**Files:**
- Modify: `docs/THREAT_MODEL.md`

**Interfaces:**
- Consumes: D1/D2/D3 wording (keep consistent with README Task 1 + the decision record).

- [ ] **Step 1: Prove the overclaims/omissions against code**

```bash
grep -rn "RotateOnion" crates/core/src/daemon/dispatch.rs | head        # degenerate rotation (D2)
grep -rn "recipient_hash\|sha256(pubkey)\|identity.hash\|identity_hash" crates/core/src/mailbox/ crates/core/src/delivery/ | head   # stable recipient hash
grep -rni "withhold\|generation gap\|mls_generation.*alert" crates/core/src/ | head   # NO alerting code exists
```
Confirm: (a) `RotateOnion` bumps the self-card version but republishes the *same* onion (degenerate — D2; see the `dispatch.rs` doc-comment / CLAUDE.md Task 23.5); (b) the mailbox recipient address is a stable `sha256(pubkey)` hash that never rotates; (c) there is **no** code that compares consecutive MLS generation numbers or raises a withhold alert — so "withholding detectable by the recipient" is latent, not an implemented defense.

- [ ] **Step 2: Re-stamp the header + scope (lines 1–13)**

Replace title `# Skattr Threat Model v0` and the `> **Status:** Draft at Phase 0 exit. …` block with:
```markdown
# Skattr Threat Model (v1.0)

> **Status:** v1.0 release model. Reflects the shipped 1:1, attachment-capable,
> Tor-only client and the mailbox server. A third-party security audit has **not**
> been performed (disclosed below); this document will be revisited after one.
> Disclosure scope follows the
> [v1.0 pull-forward-vs-disclose decision record](superpowers/specs/2026-06-23-v1.0-pull-forward-vs-disclose-decisions.md).
```
In the Scope paragraph, replace "as they exist at the end of Phase 0" → "as shipped for v1.0". Leave the rest of Scope.

- [ ] **Step 3: Correct A4 (malicious mailbox) — lines 70–84**

Replace the **Defenses** and **Residual exposure** paragraphs of A4 with:
```markdown
**Defenses:** Mailbox stores MLS ciphertext only; without the MLS keys, the
contents are random-looking bytes. The mailbox cannot forge deposits (sender
signs). The mailbox CAN withhold or drop messages: per-sender MLS generation
numbers make a withhold **detectable in principle**, but the v1.0 client does
**not** yet surface a withhold alert — treat this as a latent property, not an
active defense.

**Residual exposure:** The recipient address a mailbox sees is a **stable,
non-rotating** hash of the recipient's public key. It does not rotate across
time, so a mailbox can correlate all of a recipient's polls/deposits over its
whole lifetime, and **two colluding mailboxes can confirm they host the same
recipient**. Combined with polling cadence, message sizes, and TTLs, this is
load-bearing metadata v1.0 does not defend against. Mitigations available to the
user: self-host, and register with multiple mailboxes. (Cover polling / traffic
padding are **not** implemented — see v1.1 limitations.)
```

- [ ] **Step 4: Add a consolidated "v1.0 known limitations" section**

Immediately after the `## Non-goals …` section (after line 176), insert:
```markdown
## v1.0 known limitations (deferred to a later release)

These are absent in v1.0 by decision (see the
[disclosure decision record](superpowers/specs/2026-06-23-v1.0-pull-forward-vs-disclose-decisions.md)):

- **First-contact requires both peers online.** The first-contact Welcome is
  delivered directly (no mailbox fallback); if the inviter is offline when the
  joiner sends the Welcome, first contact stalls until both are online. Ordinary
  messages and ContactCard updates *do* have mailbox fallback. (D1)
- **Onion-address rotation is degenerate.** `Command::RotateOnion` bumps the
  self-card version and republishes the *current* onion; it does not generate a
  new address. True rotation is future work. (D2)
- **Offline attachments are best-effort.** Deposited chunks are held by a mailbox
  for ~7 days and dropped if never fetched within the window; files larger than
  10 MiB transfer only while both peers are online. Text messages are not subject
  to these limits. (D3)
- **No metadata-minimization.** No message-size padding, send-timing jitter, or
  cover traffic / cover polling.
- **The recipient-hash mailbox-correlation leak** (A4) is unmitigated.
- **No multi-member groups (> 2).**
- **Reactions / edit / delete-for-everyone / typing / read receipts** are inert
  placeholders.
- **No multi-device.**
- **No third-party security audit** has been performed for v1.0.
```

- [ ] **Step 5: De-stale the remaining "Phase N" mitigation references**

Sweep the file for forward-looking phase references that are now either shipped or v1.1, and correct each to the truth (do not invent new content — adjust framing):
- A1 line 40 "Tor bridges … (Phase 1+)" → "(if supported by your Tor configuration)".
- A4 "enable cover polling (Phase 4)" → already removed in Step 3.
- A6 lines 113–116 "Phase 1+ mitigations: …" → "Future hardening (not in v1.0): …" (at-rest encryption-on-shutdown ships in v1.0, but the plaintext working DB during operation remains — keep that residual honest).
- A7 "Reproducible builds (Phase 4)", "Code signing … (Phase 5)" → "(planned, not in v1.0)".
- "## Open questions, tracked for Phase 1+" (line 178) → "## Open questions (v1.1+)"; keep the items.
- Revision history: add a row `| v1.0 | 2026-06-25 | v1.0 release model: withhold-detection downgraded, identity-hash correlation disclosed, D1/D2/D3 + v1.1 list added. |`.

Run:
```bash
grep -nE "Phase [0-9]" docs/THREAT_MODEL.md
```
Expected: no remaining *forward-looking* "Phase N" promise (a historical mention in revision history is fine; there should be none left in the body).

- [ ] **Step 6: Verify + commit**

Re-read the file; confirm A4 no longer claims withhold-detection as a working defense, the identity-hash leak is stated plainly, attachments/D1/D2/D3 appear in the limitations section, and no new overclaim was introduced.
```bash
git add docs/THREAT_MODEL.md
git commit -m "docs(4.B): v1.0 threat model — disclose id-hash leak, downgrade withhold claim, add D1/D2/D3"
```

---

### Task 3: SECURITY.md + install docs — placeholder-key honesty

The signing/verification chain is described as if usable, but both the minisign public key and the PGP key are committed placeholders. This task makes that truthful without removing the (correct) procedure.

**Files:**
- Modify: `SECURITY.md`
- Modify: `docs/install/README.md`
- Modify: `docs/install/linux.md`, `docs/install/macos.md`, `docs/install/windows.md` (only where they reference signature verification)

**Interfaces:** none consumed downstream.

- [ ] **Step 1: Prove the keys are placeholders**

```bash
head -2 docs/install/minisign.pub          # "PLACEHOLDER — REPLACE BEFORE TAGGING v0.1.0"
grep -n "placeholder\|TBD\|PGP PUBLIC KEY" SECURITY.md | head
```
Confirm both are placeholders (the minisign pub is all-zeros; the `SECURITY.md` PGP block is a placeholder with `Fingerprint: TBD`).

- [ ] **Step 2: SECURITY.md — add a placeholder-status note**

At the top of the section that describes the PGP reporting key, and again where the download-verification chain is described, add an explicit caveat. Insert immediately before the PGP key block:
```markdown
> **⚠️ v1.0 status:** The PGP key below and the minisign public key
> (`docs/install/minisign.pub`) are **placeholders**. Encrypted vulnerability
> reports and download-signature verification are **not usable until the real
> keys are published** (a maintainer action tracked for the v0.1.0 tag, separate
> from this documentation pass). Until then, the verification *procedure* below
> is correct but will not validate against the committed placeholder key.
```
Tighten the "Deferred to v1.1+" limitations heading/items to read "Out of v1.0 scope" where they currently read as merely delayed (do not change the list contents).

- [ ] **Step 3: install/README.md — caveat at the verification steps**

Immediately above the first signature-verification step, insert:
```markdown
> **⚠️ The published `minisign.pub` is currently a placeholder.** Signature
> verification will **not** succeed until the real signing key ships with the
> v0.1.0 release. The steps below are the correct procedure for when it does.
```

- [ ] **Step 4: Platform guides — one-line caveat where they cite minisign**

In each of `docs/install/{linux,macos,windows}.md`, find any sentence that tells the user to trust/verify the minisign signature and append: ` (Note: the published minisign key is a placeholder until v0.1.0; verification is not active yet.)` Only edit files/lines that actually mention minisign verification; if a platform guide does not, leave it untouched and note that in the report.

- [ ] **Step 5: Verify + commit**

```bash
grep -rn "placeholder" SECURITY.md docs/install/README.md && echo "caveats present"
git add SECURITY.md docs/install/
git commit -m "docs(4.B): disclose minisign/PGP keys are placeholders; verification not yet usable"
```

---

### Task 4: ARCHITECTURE.md + PROTOCOL.md — factual corrections

Stale internal facts: wrong socket name, wrong migration count, "`mailbox::client` is `todo!()`", wrong ciphersuite codepoint.

**Files:**
- Modify: `ARCHITECTURE.md`
- Modify: `docs/PROTOCOL.md`

**Interfaces:** none consumed downstream.

- [ ] **Step 1: Prove the facts against code**

```bash
grep -n "ipc.sock\|ipc_socket" crates/core/src/daemon/config.rs | head    # real socket: ipc.sock
ls crates/core/src/storage/migrations/ | tail -1                          # latest: 0016_…
grep -rn "fn poll\|pub.*fn" crates/core/src/mailbox/client.rs | head      # mailbox::client is implemented, not todo!()
sed -n '20,24p' crates/core/src/mls/ciphersuite.rs                        # CIPHERSUITE: u16 = 0x0003
grep -n "0x0001\|daemon.sock\|0006\|todo!()" ARCHITECTURE.md docs/PROTOCOL.md
```
Confirm the doc strings to fix exist and the code values (`ipc.sock`, `0016`, implemented client, `0x0003`).

- [ ] **Step 2: ARCHITECTURE.md corrections**

In `ARCHITECTURE.md`, make these exact replacements (read the file first to find the precise surrounding text):
- `daemon.sock` → `ipc.sock` (every occurrence).
- Any "migrations through 0006" / "schema 0006" / "14 migrations" → "migrations through 0016".
- Any statement that `mailbox::client` is `todo!()` / unimplemented → state it is fully implemented (the v1-protocol client: deposit/fetch/poll/auth).
- Spot-check any other module-status claims against the current `crates/` tree; correct anything provably stale, and note each correction in the report.

- [ ] **Step 3: PROTOCOL.md codepoint**

In `docs/PROTOCOL.md` line ~31, replace `(IANA 0x0001)` with `(IANA 0x0003)` — the MLS ciphersuite `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519` is code-point `0x0003` per RFC 9420 §17 and `crates/core/src/mls/ciphersuite.rs:22`. Scan the file for any other codepoint/frame-type numbers and verify each against the code (frame types `0x01`–`0x0E`); correct any further mismatch found and list it in the report.

- [ ] **Step 4: Verify + commit**

```bash
grep -nE "daemon\.sock|0x0001|through 0006|todo!\(\)" ARCHITECTURE.md docs/PROTOCOL.md || echo "CLEAN"
git add ARCHITECTURE.md docs/PROTOCOL.md
git commit -m "docs(4.B): fix stale facts (ipc.sock, migrations 0016, mailbox client, ciphersuite 0x0003)"
```
Expected: `CLEAN`.

---

### Task 5: skattr-deep-dives.md (superseded banners) + skattr-design.md (ciphersuite)

`skattr-deep-dives.md` documents a superseded mailbox protocol and dead APIs; `skattr-design.md` still names the abandoned AES suite. These are historical design docs — mark superseded rather than rewrite, and fix the one factual ciphersuite name.

**Files:**
- Modify: `docs/skattr-deep-dives.md`
- Modify: `docs/skattr-design.md`

**Interfaces:** none consumed downstream.

- [ ] **Step 1: Prove against code**

```bash
grep -n "HELLO\|REGISTER\|SUBSCRIBE" docs/skattr-deep-dives.md | head     # superseded mailbox protocol
grep -n "Daemon::start\|Daemon::execute\|Daemon::shutdown" docs/skattr-deep-dives.md | head   # dead API
grep -ni "AES\|256-bit\|AES256\|GCM" docs/skattr-design.md | head          # abandoned suite mention
grep -n "MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519" crates/core/src/mls/ciphersuite.rs
```
Confirm the superseded sections exist and the locked suite name (the design doc's prose mentions a 256-bit/AES variant; the bootstrap prompt + code lock the 128-bit ChaCha suite).

- [ ] **Step 2: deep-dives — add superseded banners**

For each superseded section (Part 3 mailbox protocol HELLO/REGISTER/SUBSCRIBE; §1.2 `Daemon::start/execute/shutdown`; §3.9 mailbox-policy defaults), insert a blockquote banner at the top of the section (do not delete the historical content):
```markdown
> **⚠️ Superseded.** This section describes an earlier design that did not ship.
> The authoritative source is **ADR 0006** (frozen mailbox wire protocol) for the
> mailbox protocol, and the current `daemon::state::run_with_transport` assembly
> for the daemon lifecycle. Mailbox `Policy` defaults are defined in
> `crates/mailbox` / `crates/core/src/mailbox`. Kept for historical context.
```
Tailor the pointer per section (mailbox protocol → ADR 0006; `Daemon::start/...` → `run_with_transport`; policy defaults → the `Policy` type).

- [ ] **Step 3: skattr-design.md §1.5 — correct the ciphersuite name**

Replace the prose naming the 256-bit/AES MLS suite with the locked suite: `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519` (IANA `0x0003`). If the doc presents it as a deliberate choice with rationale, keep the rationale but correct the name and note "(the 128-bit ChaCha suite is the locked decision; an earlier draft named a 256-bit AES variant)".

- [ ] **Step 4: Verify + commit**

Re-read the edited sections; confirm banners point to the right authoritative source and the ciphersuite name is corrected.
```bash
git add docs/skattr-deep-dives.md docs/skattr-design.md
git commit -m "docs(4.B): mark superseded deep-dive sections; fix design-doc ciphersuite name"
```

---

### Task 6: passphrase-recovery.md — safety-critical correction

The "lost passphrase → delete `skattr.sqlite.age`" advice causes data loss: the DB key is seed-derived, so the correct recovery is restore-from-seed, which re-derives the key and preserves history.

**Files:**
- Modify: `docs/operations/passphrase-recovery.md`

**Interfaces:** none consumed downstream.

- [ ] **Step 1: Prove the DB key is seed-derived (so deletion is data loss, not recovery)**

```bash
grep -rn "skattr-storage-v1\|skattr-hs-storage-v1\|HKDF" crates/core/src/storage/ crates/core/src/identity/ | head
grep -rn "restore-backup\|restore_backup\|export_backup" crates/core/src/ crates/cli/src/ | head
```
Confirm: `skattr.sqlite.age` is encrypted under `HKDF(seed, "skattr-storage-v1")` (a seed-derived key, also stated in `THREAT_MODEL.md` A5). Therefore the seed phrase alone re-derives the DB key — message history is recoverable from seed + backup via `restore-backup`. Deleting the `.age` file destroys that recoverable history.

- [ ] **Step 2: Rewrite the recovery procedure**

Read the current `docs/operations/passphrase-recovery.md`. Restructure so the procedure **leads with seed-restore** and reframes deletion as a last resort with an explicit data-loss warning. The corrected guidance must convey:
```markdown
## Lost passphrase

Your passphrase protects the local **identity vault** (Argon2id →
XChaCha20-Poly1305). It is **not** the key to your message history: the message
database (`skattr.sqlite.age`) is encrypted under a key derived from your **seed
phrase** (`HKDF(seed, "skattr-storage-v1")`). So if you still have your 24-word
seed phrase, you can recover fully without the old passphrase.

**Recommended recovery (preserves history):**
1. Restore your identity from the seed phrase (`skattr restore "<24 words>"`),
   which sets a new passphrase.
2. Restore your message history from a backup with
   `skattr restore-backup "<24 words>" <backup.age>` — the seed re-derives the
   database key, so your history decrypts.

**⚠️ Last resort only — destroys history:** Deleting `skattr.sqlite.age` (and its
`-wal`/`-shm` sidecars) discards **all local message history permanently**. Do
this only if you have no backup and accept losing history; your identity can
still be restored from the seed phrase afterward.
```
Adapt exact CLI flag names to the real commands found in Step 1 if they differ; do not invent commands.

- [ ] **Step 3: Verify + commit**

Confirm the doc no longer presents deletion as the primary recovery and that seed-restore leads.
```bash
git add docs/operations/passphrase-recovery.md
git commit -m "docs(4.B): fix data-loss-prone passphrase-recovery advice (seed-restore first)"
```

---

### Task 7: Final truthfulness cross-check sweep

A single pass that verifies every corrected claim against its code source-of-truth and that no new overclaim slipped in. No new edits unless a gap is found.

**Files:** none (verification only; fixes go back to the owning task's file if a gap is found).

- [ ] **Step 1: Re-run the source-of-truth checks**

```bash
grep -nE "Phase [0-9]|50 members|arriving in Phase|doesn't work yet" README.md || echo "README CLEAN"
grep -nE "Phase [0-9]" docs/THREAT_MODEL.md | grep -v "Revision\|2026" || echo "THREAT_MODEL body CLEAN"
grep -rnE "daemon\.sock|0x0001|through 0006|todo!\(\)" ARCHITECTURE.md docs/PROTOCOL.md || echo "FACTS CLEAN"
grep -rn "placeholder" SECURITY.md docs/install/README.md >/dev/null && echo "KEY CAVEATS PRESENT"
```
Expected: all CLEAN / PRESENT.

- [ ] **Step 2: Disclosure-set completeness**

Confirm D1, D2, D3, the identity-hash correlation leak, and the standing v1.1 list each appear in `THREAT_MODEL.md`, and D1/D2/D3 appear in `README.md`:
```bash
grep -niE "first contact|both .* online|rotate|10 MiB|colluding|multi-member|metadata-minimization|third-party.*audit|multi-device" docs/THREAT_MODEL.md | head -20
grep -niE "two-party|both peers online|rotate onion|10 MiB" README.md
```
Confirm each disclosure is present and worded consistently with the decision record.

- [ ] **Step 3: New-overclaim sweep**

Read every diff in the branch (`git diff master...HEAD -- '*.md'`) one file at a time, asking for each changed claim: "is this true of the shipped code?" Any claim that cannot be tied to code is a defect — send it back to its owning task. Record the claims-checklist (claim → `file:line` source) in the report.

- [ ] **Step 4: Commit the checklist record (if any notes) / finalize**

If the sweep produced a written claims-checklist file, commit it; otherwise no commit. Hand off to the whole-branch review → PR → CodeRabbit babysit → merge.

---

## Self-Review (completed against the spec)

**Spec coverage:** README rewrite → Task 1; THREAT_MODEL re-stamp + A4 + D1/D2/D3 + limitations + de-stale → Task 2; SECURITY + install placeholder honesty → Task 3; ARCHITECTURE + PROTOCOL facts → Task 4; deep-dives superseded + design ciphersuite → Task 5; passphrase-recovery safety → Task 6; claim-by-claim verification (spec §Verification) → per-task Step 1 + Task 7. Non-goals (keys, first-run, ADRs) excluded. All spec sections mapped.

**Placeholder scan:** every step carries the exact anchor text to find, the exact replacement wording, and the exact `grep`/`sed` source-of-truth command. No "TBD"/"add appropriate"/"similar to Task N". Where a file's full current text could not be pre-quoted (ARCHITECTURE.md, install platform guides, deep-dives sections), the step names the exact string to find + the exact replacement, and instructs reading the file first — not a vague "update as needed".

**Type/string consistency:** D1/D2/D3 wording is the decision-record seed text, used identically in Task 1 (README) and Task 2 (THREAT_MODEL). Source-of-truth values are consistent across tasks: 2-member gate `mls/group.rs:115`; migrations `0016`; socket `ipc.sock`; ciphersuite `0x0003` (`mls/ciphersuite.rs:22`); DB key `HKDF(seed,"skattr-storage-v1")`.
