# Phase 4.A — Release Integrity & CI Completeness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the project's e2e + type-check evidence gate merges, keep the Flatpak manifest from rotting, and close the one tracked unit-test hole — all CI/test/packaging, no product-behavior change.

**Architecture:** Add Playwright e2e + `pnpm check` (0/0) hard gates to the CI `ui` job; root-fix the `ConfigPatch.download_dir` ts-rs annotation + a11y warnings that block `pnpm check`; add a separate `flatpak.yml` that build-validates the manifest; add the indeterminate-progress vitest case.

**Tech Stack:** GitHub Actions, Playwright (mock-backend, `TAURI_MOCK=1`), svelte-check, Vitest, ts-rs, `flatpak-builder`.

## Global Constraints

- **CI/test/packaging only** — no product-behavior change, no protocol change, no ADR. The one product-adjacent edit (`ConfigPatch.download_dir` ts-rs annotation) is a type-consistency fix.
- **Non-goal:** real minisign/PGP key generation — the `release.yml` signing plumbing is already wired; key gen is a separate pre-tag maintainer action.
- **Toolchain:** pinned 1.95.0 via the dir override (rustc 1.96 SIGSEGVs on arti). Cargo not on PATH — prefix every cargo command with `. "$HOME/.cargo/env" &&`.
- **Frontend:** run pnpm via `npx pnpm@10` only locally (system pnpm 11 corrupts the lockfile; no corepack on the dev box). **CI uses corepack `pnpm@10`** (already configured in `ci.yml`) — don't change that.
- After 4.A, `pnpm check` is **0 errors, 0 warnings** (retires the "4 pre-existing `ConfigPatch.download_dir` errors" caveat).
- No `unwrap()`/`expect()` in non-test Rust. Keep license headers. Run `cargo fmt --all --check` + `cargo clippy -p skattr-core -p skattr-ui --all-targets --all-features -- -D warnings` before committing Rust; both clean.

---

### Task 1: Indeterminate-progress unit test (P4)

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/components/FileAttachmentBubble.test.ts`

**Interfaces:** none consumed downstream.

- [ ] **Step 1: Add the failing test**

The component's indeterminate branch is `receiving && (!xferState || xferState.total === 0)` → renders `<div class="progress indeterminate"><span class="label">Downloading…</span></div>`. The existing test drives `applyProgress(AID, 1, 4)` (determinate). Add a new case to `FileAttachmentBubble.test.ts` (after the "shows a progress bar while receiving" test), using the existing `AID`/`fileRecord`/`applyProgress` helpers:
```typescript
  test("shows the indeterminate 'Downloading…' state when total is 0", async () => {
    const { container, findByText } = render(FileAttachmentBubble, {
      props: { record: fileRecord("incoming") },
    });
    await findByText("photo.jpg");
    applyProgress(AID, 0, 0); // received=0, total=0 → indeterminate
    await tick();
    const progress = container.querySelector(".progress");
    expect(progress).not.toBeNull();
    expect(progress?.classList.contains("indeterminate")).toBe(true);
    await findByText("Downloading…");
    // No determinate percentage bar in the indeterminate state.
    expect(container.querySelector(".progress .bar")).toBeNull();
  });
```
> Verify the imports the file already has (`tick` from `svelte`, `render`/`findByText` from the testing lib, `applyProgress` from `$lib/stores/attachments`). If `applyProgress(AID, 0, 0)` doesn't set `status === "receiving"` (the `receiving` flag may need a status), read `applyProgress` in `stores/attachments.ts` and the component's `receiving` derivation; drive whatever makes `receiving === true` with `total === 0` (e.g. an explicit receiving status + 0 total). Adapt the call to produce the indeterminate state.

- [ ] **Step 2: Run it (RED → GREEN)**

Run: `cd crates/ui/src-svelte && npx pnpm@10 test src/lib/components/FileAttachmentBubble.test.ts 2>&1 | tail -15`
Expected: the new case passes (the branch already exists; this is coverage). If it fails because the indeterminate state wasn't triggered, fix the test setup per Step 1's note (not the component).

- [ ] **Step 3: Full suite + commit**

Run: `cd crates/ui/src-svelte && npx pnpm@10 test 2>&1 | tail -6`
Expected: full vitest green (130).
```bash
cd /home/myggiz/development/skattr
git add crates/ui/src-svelte/src/lib/components/FileAttachmentBubble.test.ts
git commit -m "test(4.A): cover the indeterminate 'Downloading…' progress branch (P4)"
```

---

### Task 2: Fix `ConfigPatch.download_dir` type (unblock `pnpm check` errors)

**Root cause:** the Rust `ConfigPatch.download_dir` is correctly `Option<PathBuf>`, but the `#[ts(type = "string")]` annotation forces the generated TS type to `string` (non-nullable), while every other `Option` field maps to `T | null`. So a partial patch that omits `download_dir` fails to type-check. The apply logic (`config.rs:293`, `if let Some(d) = &patch.download_dir`) already skips `None` — no logic change needed.

**Files:**
- Modify: `crates/core/src/daemon/config.rs` (the `#[ts(type = "string")]` on `download_dir`, ~line 105)
- Regenerate: `crates/ui/src-svelte/src/lib/ipc/types/ConfigPatch.ts`
- Modify: `crates/ui/src-svelte/src/lib/stores/config.ts` (2 sites: the `_pendingPatch` init ~line 48 and the reset inside the flush timer ~line 84)
- Modify: `crates/ui/src-svelte/src/routes/settings/notifications/+page.svelte` (~line 23) and `routes/settings/history/+page.svelte` (~line 33)

**Interfaces:**
- Produces: `ConfigPatch.download_dir` becomes `string | null` in TS (nullable-in-patch, consistent with the other fields).

- [ ] **Step 1: Fix the ts-rs annotation**

In `crates/core/src/daemon/config.rs`, change the `download_dir` field annotation:
```rust
    /// If `Some`, set the attachment download directory. New in 3.B.
    #[serde(default)]
    #[ts(type = "string | null")]
    pub download_dir: Option<std::path::PathBuf>,
```
(Only `#[ts(type = "string")]` → `#[ts(type = "string | null")]`. The field type, serde, and the apply logic are unchanged.)

- [ ] **Step 2: Regenerate the TS binding**

The ts-rs bindings are generated by running the core tests (CI's "Generate ts-rs bindings" step is `cargo test -p skattr-core --features test-harness`). Run it to regenerate `ConfigPatch.ts`:
```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness export_bindings 2>&1 | tail -8
```
Then confirm the generated file updated:
```bash
grep -n "download_dir" crates/ui/src-svelte/src/lib/ipc/types/ConfigPatch.ts
```
Expected: `download_dir: string | null,` (was `download_dir: string,`). If the path/command differs, find the ts-rs export test: `grep -rn "export_bindings_configpatch\|ConfigPatch.*ts(export" crates/core/src/`.

- [ ] **Step 3: Add `download_dir: null` at the 4 call sites**

- `config.ts` — add `download_dir: null,` to BOTH object literals (the `_pendingPatch` initializer ~line 48 and the reset inside the `setTimeout` ~line 84), each currently ending after `persist_logs_to_disk: null,`.
- `routes/settings/notifications/+page.svelte` `setMode` (~line 23) — add `download_dir: null,` to the `patchConfig({...})` literal.
- `routes/settings/history/+page.svelte` `setRetention` (~line 33) — add `download_dir: null,` to the `patchConfig({...})` literal.
(`advanced/+page.svelte`'s `singlePatch()` helper builds patches dynamically and casts; after this fix its cast is no longer needed, but leaving it is harmless — drop the `as Parameters<...>` cast only if it's clean to do so.)

- [ ] **Step 4: Verify the 4 errors are gone + Rust gate green**

Run:
```bash
cd crates/ui/src-svelte && npx pnpm@10 check 2>&1 | grep -E "svelte-check found|download_dir"
```
Expected: NO `download_dir` errors remain (the count drops from 4 errors to 0 errors; the 4 a11y *warnings* remain until Task 3).
Run the Rust gate (the annotation change must not break anything):
```bash
. "$HOME/.cargo/env" && cargo fmt -p skattr-core --check && cargo clippy -p skattr-core --all-targets --all-features -- -D warnings 2>&1 | tail -3 && cargo test -p skattr-core --features test-harness config 2>&1 | grep -E "test result|FAILED" | tail -3
```
Expected: fmt/clippy clean; config tests pass.

- [ ] **Step 5: Commit**

```bash
cd /home/myggiz/development/skattr
git add crates/core/src/daemon/config.rs crates/ui/src-svelte/src/lib/ipc/types/ConfigPatch.ts crates/ui/src-svelte/src/lib/stores/config.ts crates/ui/src-svelte/src/routes/settings/notifications/+page.svelte crates/ui/src-svelte/src/routes/settings/history/+page.svelte
git commit -m "fix(4.A): ConfigPatch.download_dir is nullable-in-patch (unblock svelte-check)"
```

---

### Task 3: Clear the 4 svelte-check a11y warnings

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/components/SearchPalette.svelte`
- Modify: `crates/ui/src-svelte/src/routes/settings/mailboxes/+page.svelte`

**Interfaces:** none.

- [ ] **Step 1: Enumerate the exact warnings**

Run: `cd crates/ui/src-svelte && npx pnpm@10 check 2>&1 | grep -iE "Warn|a11y" | head -20`
Expected (from the audit): 4 warnings — `SearchPalette.svelte` (the `role="dialog"` div `onkeydown` ~:158; `autofocus` ~:170; the `<li role="option">` with `onclick` ~:179) and `mailboxes/+page.svelte` (the `modal-overlay` div with `onclick`/`onkeydown` ~:109). The file already has some `svelte-ignore a11y_interactive_supports_focus` comments — address the REMAINING ones the run reports. Note each warning code + line.

- [ ] **Step 2: Address each warning**

For each, prefer real keyboard handling; use a `<!-- svelte-ignore <code> -->` with a one-line justification only where the ARIA role already legitimizes the markup:
- **SearchPalette `<div role="dialog">` `onkeydown`** — the dialog role + `onkeydown={onPanelKey}` is the standard modal pattern; add `<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->` (or whatever code the run reports) directly above the element with a reason comment.
- **SearchPalette `autofocus`** — `a11y_autofocus`: for a search palette that opens on demand, autofocus is intentional UX; add `<!-- svelte-ignore a11y_autofocus -->` above the `<input>`.
- **SearchPalette `<li role="option">` `onclick`** — `a11y_click_events_have_key_events`: the list is keyboard-navigable via the panel's `onPanelKey` (arrows + Enter on the highlighted item), so per-`<li>` keydown is redundant; add the matching `<!-- svelte-ignore -->` with that reason, OR add an `onkeydown` to the `<li>` that triggers the same select on Enter/Space if cleaner.
- **mailboxes `modal-overlay` div** — it already has `onkeydown` (Escape) + `role="dialog"`; the warning is likely `a11y_click_events_have_key_events` / non-interactive role. Add the matching `<!-- svelte-ignore -->` with a reason (the overlay's Escape/click-to-dismiss is the standard pattern), or move the dismiss to a `<button>`.
Use the EXACT warning code the run reports in each `svelte-ignore` (a wrong code leaves the warning unsuppressed).

- [ ] **Step 3: Verify 0 warnings + vitest still green**

Run: `cd crates/ui/src-svelte && npx pnpm@10 check 2>&1 | grep "svelte-check found"`
Expected: `svelte-check found 0 errors and 0 warnings` (Task 2 cleared the errors; this clears the warnings).
Run: `npx pnpm@10 test 2>&1 | tail -4 | head -2` (vitest unaffected) and `npx pnpm@10 build 2>&1 | tail -2` (build clean — `svelte-ignore` comments don't break the build).

- [ ] **Step 4: Commit**

```bash
cd /home/myggiz/development/skattr
git add crates/ui/src-svelte/src/lib/components/SearchPalette.svelte crates/ui/src-svelte/src/routes/settings/mailboxes/+page.svelte
git commit -m "a11y(4.A): clear the 4 svelte-check warnings (search palette, mailbox overlay)"
```

---

### Task 4: Gate the CI `ui` job on `pnpm check` + Playwright e2e

**Files:**
- Modify: `.github/workflows/ci.yml` (the `ui` job, after the "Unit tests (vitest)" step)
- Possibly modify: `crates/ui/src-svelte/package.json` (ensure `check` fails on warnings)

**Interfaces:** none.

**Depends on Tasks 2 + 3** (so `pnpm check` is 0/0) — do not add the check gate before they land or CI goes red.

- [ ] **Step 1: Make `pnpm check` fail on warnings**

`svelte-check` exits 0 on warnings by default; to enforce 0 warnings the gate needs `--fail-on-warnings`. Read `crates/ui/src-svelte/package.json`'s `check` script (e.g. `"check": "svelte-check --tsconfig ./tsconfig.json"`). Add the flag so warnings fail it:
```json
    "check": "svelte-check --tsconfig ./tsconfig.json --fail-on-warnings",
```
(If the script uses `svelte-kit sync &&` first, keep that prefix.) Verify locally:
```bash
cd crates/ui/src-svelte && npx pnpm@10 check; echo "exit=$?"
```
Expected: exit 0 (0/0). Then sanity-check it FAILS on a warning by temporarily reverting one `svelte-ignore` (optional) — or trust that `--fail-on-warnings` is correct and move on.

- [ ] **Step 2: Add the two CI steps**

In `.github/workflows/ci.yml`, in the `ui` job, immediately after the "Unit tests (vitest)" step (the `pnpm test` step), insert:
```yaml
      - name: Type check (svelte-check, 0 errors / 0 warnings)
        working-directory: crates/ui/src-svelte
        run: pnpm check

      - name: Install Playwright browser
        working-directory: crates/ui/src-svelte
        run: pnpm exec playwright install --with-deps chromium

      - name: E2E tests (Playwright, mock backend)
        working-directory: crates/ui/src-svelte
        run: pnpm test:e2e
```
Notes: CI uses corepack `pnpm@10` (already activated earlier in the job), so use `pnpm`/`pnpm exec` (NOT `npx pnpm@10` — that's a dev-box-only workaround). `playwright install --with-deps chromium` installs Chromium + its system libs on the runner. `pnpm test:e2e` = `TAURI_MOCK=1 playwright test`; its `webServer` runs `pnpm build && pnpm preview --port 4173` (mock backend) and Chromium runs headless — no `xvfb` needed.

- [ ] **Step 3: Local verification of the gated commands**

The authoritative proof is CI on the PR, but verify the commands locally first:
```bash
cd crates/ui/src-svelte && npx pnpm@10 check 2>&1 | grep "svelte-check found"
npx pnpm@10 exec playwright install chromium 2>&1 | tail -2
npx pnpm@10 test:e2e 2>&1 | tail -6
```
Expected: 0/0; e2e 13/13.

- [ ] **Step 4: Commit**

```bash
cd /home/myggiz/development/skattr
git add .github/workflows/ci.yml crates/ui/src-svelte/package.json
git commit -m "ci(4.A): gate the ui job on svelte-check (0/0) + Playwright e2e"
```

---

### Task 5: Flatpak manifest-build validation workflow

**Files:**
- Create: `.github/workflows/flatpak.yml`

**Interfaces:** none.

- [ ] **Step 1: Read the existing build procedure**

The manifest is `packaging/flatpak/net.myggiz.skattr.yml` (runtime `org.freedesktop.Platform//23.08`, builds `skattr-ui` via `cargo tauri build --no-bundle --release`). `docs/build/flatpak.md` documents the local build commands. Read BOTH to learn the exact `flatpak-builder` invocation, the runtime/SDK install, and any build-time network/option needs (Rust builds inside the sandbox may need `--share=network` or vendored deps — use whatever the docs/manifest specify).

- [ ] **Step 2: Write the workflow**

Create `.github/workflows/flatpak.yml` — a build-validation job that does NOT upload an artifact:
```yaml
# SPDX-License-Identifier: GPL-3.0-or-later
name: flatpak

on:
  push:
    branches: [master]
  schedule:
    - cron: "0 6 * * 1"   # weekly, Monday 06:00 UTC — catch manifest rot
  workflow_dispatch: {}

jobs:
  build-validate:
    name: flatpak manifest build-validate
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6

      - name: Install flatpak + flatpak-builder
        run: |
          sudo apt-get update
          sudo apt-get install -y flatpak flatpak-builder

      - name: Add Flathub + install runtime/SDK (23.08)
        run: |
          flatpak remote-add --user --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo
          flatpak install --user -y flathub org.freedesktop.Platform//23.08 org.freedesktop.Sdk//23.08

      - name: Cache flatpak-builder
        uses: actions/cache@v5
        with:
          path: .flatpak-builder
          key: flatpak-builder-${{ hashFiles('packaging/flatpak/net.myggiz.skattr.yml') }}
          restore-keys: |
            flatpak-builder-

      - name: Build-validate the manifest (no bundle, no upload)
        run: |
          flatpak-builder --user --force-clean --disable-rofiles-fuse \
            --repo="$RUNNER_TEMP/repo" \
            "$RUNNER_TEMP/build" \
            packaging/flatpak/net.myggiz.skattr.yml
```
> Adapt the `flatpak-builder` flags + any `--install-deps-from`/network options to match `docs/build/flatpak.md`'s documented command. If the manifest's Rust build needs network (cargo fetch) inside the sandbox, add the build option the manifest/docs prescribe. The job FAILS if the manifest doesn't build — that's the rot-check. Triggers are master-push + weekly + manual (NOT every PR), per the spec.

- [ ] **Step 3: Local + syntax verification**

YAML-validate the workflow (e.g. `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/flatpak.yml'))" && echo OK`). If the dev box has `flatpak-builder`, run the build command locally to confirm the manifest builds; if not (likely), NOTE that in your report — CI (on master/dispatch) is the authoritative validation. Do NOT block the task on local flatpak tooling.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/flatpak.yml
git commit -m "ci(4.A): flatpak.yml — build-validate the manifest (master + weekly), no shipped artifact"
```

---

### Task 6: Full gate + CI verification (verification-before-completion)

**Files:** none (verification only).

- [ ] **Step 1: Local frontend gate**

```bash
cd crates/ui/src-svelte && \
CI=true npx pnpm@10 install --frozen-lockfile && \
npx pnpm@10 check 2>&1 | grep "svelte-check found" && \
npx pnpm@10 test 2>&1 | tail -4 | head -2 && \
npx pnpm@10 build 2>&1 | tail -2 && \
npx pnpm@10 test:e2e 2>&1 | tail -6
```
Expected: `0 errors and 0 warnings`; vitest 130/130; build clean; e2e 13/13.

- [ ] **Step 2: Rust gate (the ConfigPatch change)**

```bash
. "$HOME/.cargo/env" && cargo fmt --all --check && \
cargo clippy -p skattr-core -p skattr-ui --all-targets --all-features -- -D warnings && \
cargo test -p skattr-core --features test-harness --lib 2>&1 | grep -E "test result|FAILED" | tail -2
```
Expected: fmt/clippy clean; core lib green.

- [ ] **Step 3: Workflow YAML sanity**

```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml')); yaml.safe_load(open('.github/workflows/flatpak.yml')); print('YAML OK')"
```

- [ ] **Step 4: Branch status + CI handoff**

Run: `git status && git log --oneline master..HEAD`. Hand to whole-branch review → PR → CodeRabbit + **CI** babysit. **The authoritative proof of Task 4 (the new e2e + check gates) and Task 5 (flatpak on master) is CI itself** — watch the PR's `ui` job run the new gates green; the `flatpak.yml` job runs on master-push after merge (or via `workflow_dispatch` on the branch to validate before merge — trigger it and confirm green). Do NOT merge before the whole-branch review + a green `ui` job.

---

## Self-Review (completed against the spec)

**Spec coverage:** Item A (e2e gate) → Task 4. Item B → Task 2 (B1 download_dir), Task 3 (B2 a11y), Task 4 (B3 the check gate + `--fail-on-warnings`). Item C (flatpak validation) → Task 5. Item D (indeterminate test, P4) → Task 1. Non-goal (key gen) excluded. Task order D → B1 → B2 → (B3+A) → C → gate matches the spec.

**Placeholder scan:** every step carries the exact edit (the ts annotation one-liner, the 4 `download_dir: null` sites, the verbatim vitest case, the verbatim ci.yml steps, the verbatim flatpak.yml). The judgment points are concrete read-then-act anchors: the indeterminate-state trigger (read `applyProgress`/`receiving` if `(0,0)` doesn't suffice), the exact a11y warning codes (read from `pnpm check` output — a wrong code doesn't suppress), and the `flatpak-builder` flags (match `docs/build/flatpak.md`). None are deferrals.

**Type/string consistency:** `ConfigPatch.download_dir` → `string | null` (Task 2) is the type all 4 call sites (Task 2) and the gate (Task 4) rely on. `pnpm check` 0/0 is produced by Tasks 2+3 and consumed by Task 4's gate. `pnpm test:e2e` (= `TAURI_MOCK=1 playwright test`) and the `flatpak-builder` manifest path `packaging/flatpak/net.myggiz.skattr.yml` are consistent across tasks. CI uses corepack `pnpm` (not `npx pnpm@10`) — flagged in Task 4.
