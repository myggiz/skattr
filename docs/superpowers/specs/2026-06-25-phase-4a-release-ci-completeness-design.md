# Phase 4.A — Release Integrity & CI Completeness — Design

**Date:** 2026-06-25
**Status:** Approved (brainstorm) — ready for implementation planning
**Depends on:** Phases 4.D, 4.B, 4.C merged. The last Phase-4 sub-project before
the real signing keys + v1.0 tag.
**Sibling sub-projects:** 4.B/4.C/4.D — all merged.

---

## Purpose

Make the project's own quality evidence actually gate merges, and keep the
Flatpak packaging from silently rotting. The v1.0 readiness audit (T2-3) found
the CI `ui` job runs `pnpm build` + clippy + `cargo test` + `pnpm test` (vitest)
but **not** Playwright e2e or `pnpm check` (svelte-check), and `release.yml`
builds `.deb`/AppImage/`.dmg`/`.msi` but never the Flatpak the docs advertise.
This sub-project closes those gaps plus the one tracked unit-test hole (P4).

**CI / test / packaging only — no product-behavior change, no protocol change,
no ADR.** The one product-adjacent edit (`ConfigPatch.download_dir`) is a
type-consistency fix needed to unblock `pnpm check`.

### Non-goal (explicit)

**Real minisign / PGP key generation.** The `release.yml` signing plumbing is
already correctly wired — it reads `MINISIGN_SECRET_KEY` (base64) +
`MINISIGN_PASSWORD` from secrets, signs `SHA256SUMS`, and verifies against
`docs/install/minisign.pub`. Generating the real keys and configuring the CI
secrets is a separate maintainer action with secret material, done just before
tagging v0.1.0 — not part of this docs/CI sub-project. 4.B already discloses the
keys are placeholders.

---

## Item A — Playwright e2e in CI

**Problem.** The 13 e2e specs (`crates/ui/src-svelte/tests/e2e/*.spec.ts`,
Playwright, against a **mock** Tauri backend via `TAURI_MOCK=1`) are run locally
(`pnpm test:e2e`) but never in CI, so they don't gate merges.

**Design.** Add a hard-gating step to the existing `ui` job in
`.github/workflows/ci.yml`, after the vitest step:
1. `npx playwright install chromium` — the browser binary is not preinstalled on
   `ubuntu-latest` (Playwright is a devDependency, but the browser is separate).
   Use `--with-deps` if the runner lacks the system libs Chromium needs.
2. `pnpm test:e2e` — runs `TAURI_MOCK=1 playwright test`. The Playwright
   `webServer` config builds + previews the SvelteKit app (`pnpm build && pnpm
   preview --port 4173`) with the mock backend. Chromium runs **headless by
   default**, so no `xvfb` is required.

**Hard gate** (the whole point is to make these gate merges). Mock-backend e2e
is deterministic (no Argon2id, no real daemon), so flakiness risk is low; keep
`retries: 0`. If CI proves flaky, the contained fallback is a `webServer.timeout`
bump (already 120 s) or a browser-launch wait — **not** retry-masking and **not**
loosening assertions.

**Verification.** The CI step itself; locally `pnpm test:e2e` is green (13/13).

---

## Item B — `pnpm check` (svelte-check) in CI

**Problem.** `pnpm check` is not a CI gate, blocked by 4 type errors. Adding it
requires clearing those (and, per the brainstorm decision, the 4 a11y warnings)
so the gate is fully clean.

### B1 — Fix the 4 `download_dir` type errors (root cause)

The ts-rs-generated `ConfigPatch` (`src/lib/ipc/types/ConfigPatch.ts`) has
`download_dir: string` (**required**), while every other patch field is
`T | null` (`null` = "leave unchanged"). So a partial patch that doesn't touch
the download dir (e.g. a notification-mode change) fails to type-check at 4
sites: `config.ts:48` and `config.ts:83` (`_pendingPatch` init/reset),
`routes/settings/notifications/+page.svelte:23`, and
`routes/settings/history/+page.svelte:33`.

**Fix at the root** so a partial patch needn't supply `download_dir`: make it a
proper *optional* patch field consistent with the others. The implementer
determines the exact mechanism by reading the Rust `ConfigPatch` definition:
- If the Rust field is `download_dir: String`, change it to `Option<String>`
  (and the `apply`/merge logic to skip on `None`), regenerate the ts-rs binding
  (so TS becomes `string | null`), then set `download_dir: null` at the 4 sites.
- If the Rust field is already `Option<String>` but ts-rs mis-generated it as
  `string` (a missing/incorrect `#[ts(...)]`), fix the annotation + regenerate.

Either way: `ConfigPatch.download_dir` becomes nullable-in-patch, the 4 sites set
`download_dir: null`, and the existing dynamic helper `singlePatch()` in
`advanced/+page.svelte` (which currently casts to bypass the check) can keep
working or drop its cast. **The Rust gate (fmt/clippy/core+ui tests) must stay
green** after the type change — verify the SetConfig apply path and any reader of
`ConfigPatch.download_dir`.

### B2 — Fix the 4 a11y warnings

`svelte-check` exits non-zero only on errors, but per the brainstorm decision the
gate is fully clean (0/0). Close the warnings, mirroring the wipe-dialog a11y
pattern from 4.C:
- `SearchPalette.svelte:158` — non-interactive `<div role="dialog">` with
  `onkeydown`: add the appropriate `svelte-ignore` *with justification* (the
  dialog role legitimizes the handler) or restructure; `:170` `autofocus`; `:179`
  `<li>` with `onclick` needs a keyboard equivalent (Enter/Space) or
  `svelte-ignore` with reason.
- `routes/settings/mailboxes/+page.svelte:109` — overlay `<div>` with
  `onclick`/`onkeydown`: same treatment (Escape/keyboard handling or a justified
  `svelte-ignore`).
Prefer real keyboard handling for the genuinely-interactive ones; use
`svelte-ignore` (with a one-line reason) only where the role already justifies
the markup.

### B3 — Add the gate

Add `pnpm check` to the `ui` job (after vitest/e2e) as a **hard gate**. It must
report **0 errors and 0 warnings**. (Note: this removes the long-standing "4
pre-existing `ConfigPatch.download_dir` errors" caveat referenced throughout the
4.B/4.C work — after 4.A, `pnpm check` is clean.)

**Verification.** `pnpm check` → 0 errors, 0 warnings; CI `ui` job gates on it.

---

## Item C — Flatpak manifest-build validation in CI

**Problem.** `packaging/flatpak/net.myggiz.skattr.yml` + `docs/build/flatpak.md`
exist, but `release.yml` never invokes `flatpak-builder`, so the manifest is
unvalidated and can rot. The v1.0 decision is to **validate it builds in CI**, not
ship a `.flatpak` artifact yet (docs already say build-from-source).

**Design.** A **separate** workflow `.github/workflows/flatpak.yml` (kept off the
fast per-PR path) that:
1. Installs `flatpak` + `flatpak-builder` and the
   `org.freedesktop.Platform//23.08` + `org.freedesktop.Sdk//23.08` runtimes
   (cache the runtime/`.flatpak-builder` dir to keep it tolerable).
2. Runs a **build-only** validation:
   `flatpak-builder --force-clean --repo=<tmp-repo> --disable-rofiles-fuse
   <build-dir> packaging/flatpak/net.myggiz.skattr.yml` — a full build (compiles
   `skattr-ui` under the sandbox via the manifest's `cargo tauri build
   --no-bundle`), **no bundle, no upload**. Fails the job if the manifest doesn't
   build.
3. Does NOT upload a `.flatpak` to any release (deferred to v1.1).

**Triggers** (recommendation, approved in brainstorm): `on: push` to `master` +
a weekly `schedule` (cron) + `workflow_dispatch` — **not** on every PR (the build
compiles the whole app under the sandbox, ~a second full Tauri build + ~GB SDK;
gating every PR ~doubles CI time for an unshipped artifact). This catches rot
within a day without taxing PR latency.

**Verification.** The `flatpak.yml` job builds the manifest successfully on
`master` / on demand. (Local `flatpak-builder` validation only if the dev box has
the tooling — CI is the authoritative gate; note in the report if local tooling
is absent.)

---

## Item D — Indeterminate-progress unit test (P4)

**Problem.** `FileAttachmentBubble.svelte`'s receiving-progress logic has an
`indeterminate` branch (`receiving && (!xferState || xferState.total === 0)` →
the "Downloading…" label with no percentage). The existing
`FileAttachmentBubble.test.ts` test drives `applyProgress(AID, 1, 4)`
(`total > 0`, determinate), so the indeterminate branch is untested.

**Design.** Add one Vitest case: render an incoming `FileAttachmentBubble`, drive
a receiving state with `total === 0` (e.g. `applyProgress(AID, 0, 0)`), and
assert the `.indeterminate` class + the "Downloading…" label render **and** that
no percentage (`Downloading NN%`) / no width-styled `.bar` appears. Mirror the
existing determinate test's setup.

**Verification.** `pnpm test` includes the new case; it fails before the
assertions are correct (RED) and passes after (GREEN — the branch already exists,
so this is a coverage-adding test, not a behavior change).

---

## File structure (where changes land)

- `.github/workflows/ci.yml` — add the **e2e** step + the **`pnpm check`** step to
  the `ui` job (both hard gates), after the existing `pnpm test`.
- `.github/workflows/flatpak.yml` (**new**) — the manifest-build validation job
  (master-push + weekly schedule + `workflow_dispatch`).
- **Rust:** the `ConfigPatch` definition (wherever `download_dir` is declared) +
  its apply/merge logic, if B1 is a Rust type change; regenerate the ts-rs
  binding (`ConfigPatch.ts`).
- **Frontend:** `src/lib/stores/config.ts` (2 sites) +
  `routes/settings/{notifications,history}/+page.svelte` (`download_dir: null`);
  `SearchPalette.svelte` + `routes/settings/mailboxes/+page.svelte` (a11y);
  `src/lib/components/FileAttachmentBubble.test.ts` (the new test).

Natural task order: **D** (tiny test) → **B1/B2** (clear svelte-check) → **B3**
(add the check gate) → **A** (add the e2e gate) → **C** (Flatpak workflow). B
precedes its own gate; A and C are independent.

---

## Non-goals

- **Real key generation / CI-secret setup** (see Non-goal above) — separate
  pre-tag maintainer action.
- **Shipping a `.flatpak` artifact** in `release.yml` — deferred to v1.1; 4.A
  only validates the manifest builds.
- **Reproducible builds / SLSA / notarization / Authenticode** — out of v1.0
  scope (disclosed in the threat model).
- **Adding macOS-x86_64 to the matrix** — still deferred (`macos-latest` is
  Apple-Silicon only, per CLAUDE.md).
- Any change to product behavior, the mailbox/peer protocol, or an ADR.

---

## Risks

- **e2e flakiness in CI.** Mock-backend e2e is far more deterministic than the
  daemon-loopback Rust guardrails (no Argon2id/Tor); low risk. Mitigation if it
  surfaces: a contained `webServer`/launch timeout bump, never retries or
  loosened assertions.
- **`ConfigPatch.download_dir` change rippling.** Making it optional touches the
  Rust apply path + ts-rs regen + 4 call sites; verify the Rust gate stays green
  and no reader assumes a non-null `download_dir`.
- **Flatpak job runtime / flakiness.** The SDK download + sandbox build is slow;
  mitigated by caching + keeping it off the per-PR path (master + schedule). A
  transient Flathub/runtime fetch failure fails the job — acceptable for a
  scheduled rot-check (re-run on demand).
- **`playwright install` cost.** Adds a browser download to the `ui` job;
  acceptable (cached by the Actions runner image / pnpm store where possible).
