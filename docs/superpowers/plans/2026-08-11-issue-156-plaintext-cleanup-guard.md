# Plaintext Cleanup Guard (#156 + #52) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ensure decrypted attachment plaintext is never left on disk when an error or panic interrupts the code that produced it.

**Architecture:** One `pub(crate)` RAII guard, `OnDrop`, in a new top-level module. It runs a caller-supplied closure on drop unless disarmed, so cleanup happens on every exit path — `?`, early return, and panic — rather than only where someone remembered to write it. Two call sites adopt it: the reassembler's `.part` temp file (#156) and the open-attachment cache wipe (#52).

**Tech Stack:** Rust 2021, std only.

**Spec:** `docs/superpowers/specs/2026-08-11-issue-156-plaintext-cleanup-guard-design.md`

**Branch:** `156-plaintext-cleanup-guard` (created; spec committed as `2f3abae`)

## Global Constraints

- **No new dependency, no new public API.** The guard is `pub(crate)`. No wire-format change, no migration.
- **The Drop closure must never panic.** A panic inside `Drop` during unwinding aborts the process. Both call sites use `let _ = std::fs::remove*`, which cannot panic — keep it that way.
- **No overwrite-before-delete.** On journalling filesystems and wear-levelled SSDs it does not reliably erase the blocks, and it would diverge from what `wipe_open_cache` already does. Plain removal only.
- **Do not relocate the reassembler temp file.** It must stay adjacent to the destination: `rename` across filesystems fails, and that atomic rename is what makes "no partial output" true.
- **`reassemble`'s observable behaviour must not change** — same error variants, still no partial output, `rename` still only on full success. This is a cleanup change, not a semantics change.
- **Replace the superseded comment, do not leave it.** `reassembler.rs` currently contains prose defending the leak; code that contradicts its own comment is worse than either alone.
- No `unwrap()`/`expect()` in non-test code. `cargo clippy -D warnings` is the done-gate.
- Every `.rs` file carries the GPLv3 SPDX header (`// SPDX-License-Identifier: GPL-3.0-or-later` / `// Copyright (C) 2026 Myggiz B.V.`).
- **The repo enforces DCO — every commit needs `git commit -s`.**
- Cargo is NOT on PATH: prefix every cargo command with `. "$HOME/.cargo/env" && `.
- **Run every cargo command in the FOREGROUND.** Backgrounded jobs have repeatedly stalled agents on this repo.

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/core/src/on_drop.rs` | **New.** The `OnDrop` guard and its unit tests. Nothing else. | Create |
| `crates/core/src/lib.rs` | Module registration (one line, beside the other `pub(crate) mod` entries at lines 43-53). | Modify |
| `crates/core/src/attachment/reassembler.rs` | Reassembly. Adopts the guard for its `.part` file; the three explicit `remove_file` calls go away. | Modify |
| `crates/core/src/daemon/state.rs` | Daemon assembly. Arms a never-disarmed guard on `wipe_open_cache`. | Modify |

The guard lives at the top level rather than inside `attachment/` because #52's user is `daemon/state.rs`. `prelude.rs` is public re-exports only and is not a home for internals.

---

## Task 1: The `OnDrop` guard

**Files:**
- Create: `crates/core/src/on_drop.rs`
- Modify: `crates/core/src/lib.rs` (add `pub(crate) mod on_drop;` alongside the existing `pub(crate) mod` lines at 43-53, keeping alphabetical order)

**Interfaces:**
- Consumes: nothing.
- Produces, used by Tasks 2 and 3:
  - `pub(crate) struct OnDrop<F: FnOnce()>`
  - `pub(crate) fn OnDrop::new(f: F) -> Self`
  - `pub(crate) fn OnDrop::disarm(self)` — consumes the guard

- [ ] **Step 1: Write the failing tests**

Create `crates/core/src/on_drop.rs` containing only the header, the doc comment, and this test module for now:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! A scope guard that runs a cleanup closure on drop.
//!
//! Exists so that cleanup of **decrypted plaintext** happens on every exit
//! path — `?`, early return, and panic — rather than only where someone
//! remembered to write it. Attachments are kept encrypted at rest, so a
//! failure part-way through producing plaintext must not leave that plaintext
//! behind (#156, #52).

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn runs_the_closure_on_drop() {
        let hits = Arc::new(AtomicUsize::new(0));
        {
            let h = hits.clone();
            let _g = OnDrop::new(move || {
                h.fetch_add(1, Ordering::SeqCst);
            });
        }
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn disarm_cancels_the_closure() {
        let hits = Arc::new(AtomicUsize::new(0));
        {
            let h = hits.clone();
            let g = OnDrop::new(move || {
                h.fetch_add(1, Ordering::SeqCst);
            });
            g.disarm();
        }
        assert_eq!(hits.load(Ordering::SeqCst), 0, "disarmed guard must not run");
    }

    #[test]
    fn runs_during_unwind() {
        // The property that distinguishes this from explicit cleanup at each
        // error site: a panic between arming and disarming still cleans up.
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _g = OnDrop::new(move || {
                h.fetch_add(1, Ordering::SeqCst);
            });
            panic!("boom");
        }));
        assert!(result.is_err(), "the panic must propagate");
        assert_eq!(hits.load(Ordering::SeqCst), 1, "cleanup must run while unwinding");
    }

    #[test]
    fn runs_exactly_once() {
        let hits = Arc::new(AtomicUsize::new(0));
        {
            let h = hits.clone();
            let _g = OnDrop::new(move || {
                h.fetch_add(1, Ordering::SeqCst);
            });
        }
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
```

- [ ] **Step 2: Register the module**

In `crates/core/src/lib.rs`, add beside the other internal modules (they sit at lines 43-53; keep alphabetical order):

```rust
pub(crate) mod on_drop;
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib on_drop`
Expected: FAIL — `cannot find struct OnDrop in this scope` (compile error).

- [ ] **Step 4: Implement the guard**

Insert above the `#[cfg(test)] mod tests` block in `crates/core/src/on_drop.rs`:

```rust
/// Runs `f` when dropped, unless [`OnDrop::disarm`] was called first.
///
/// The closure **must not panic**: a panic inside `Drop` during unwinding
/// aborts the process. Cleanup here is `let _ = std::fs::remove*`, which
/// cannot panic.
pub(crate) struct OnDrop<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> OnDrop<F> {
    /// Arm a guard that will run `f` on drop.
    pub(crate) fn new(f: F) -> Self {
        Self(Some(f))
    }

    /// Cancel the cleanup.
    ///
    /// Takes `self` by value so a disarmed guard cannot be reused, and so the
    /// call site reads as the moment responsibility for the plaintext is
    /// handed over (e.g. immediately after a successful `rename`).
    pub(crate) fn disarm(mut self) {
        self.0 = None;
    }
}

impl<F: FnOnce()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib on_drop`
Expected: PASS — 4 tests.

- [ ] **Step 6: Gate**

```bash
. "$HOME/.cargo/env" && cargo fmt --all -- --check \
  && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings
```
Expected: clean.

If clippy reports `OnDrop` as never used at this point, that is expected — Tasks 2 and 3 add the call sites. Do **not** silence it with `#[allow(dead_code)]`; if `-D warnings` fails the build for that reason, note it in your report and proceed. (`lib.rs` has a workspace-level `dead_code = "allow"`, so this most likely will not fire.)

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/on_drop.rs crates/core/src/lib.rs
git commit -s -m "feat(core): add OnDrop scope guard for plaintext cleanup

Runs a cleanup closure on drop unless disarmed, so cleanup happens on every
exit path — ? , early return, and panic — rather than only where someone
remembered to write it.

A closure rather than a stored path because the two users clean up different
things: the reassembler removes a temp file, the daemon wipes a directory
tree.

Refs #156, #52"
```

---

## Task 2: Adopt the guard in the reassembler (#156)

**Files:**
- Modify: `crates/core/src/attachment/reassembler.rs` — `reassemble` and its `mod tests`

**Interfaces:**
- Consumes from Task 1: `crate::on_drop::OnDrop::{new, disarm}`.
- Produces: nothing consumed by later tasks.

**Background the implementer needs.** `reassemble` streams decrypted plaintext to `<output_path>.part`, then `rename`s it into place. Today the validation paths (hash / AEAD / size) call `std::fs::remove_file(&tmp)` explicitly — three sites — but the `?`-propagating I/O errors on `create` / `write_all` / `sync_all` / `rename` do not, so a disk-full or read-only mount leaves a complete decrypted copy behind. The file also carries a comment defending that behaviour, which this task replaces.

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` in `crates/core/src/attachment/reassembler.rs`. It already has a `MemSource` and a `mem_source(chunks)` helper — reuse them, and add a failing source beside them:

```rust
    /// Two chunks: one full, one short. Two is the minimum that lets a
    /// failure happen *after* plaintext has already been written to the temp
    /// file — a single-chunk fixture would not exercise the leak at all.
    fn fixture_multi_chunk() -> (AttachmentManifest, Vec<Vec<u8>>, Vec<u8>) {
        let plaintext = vec![9u8; crate::attachment::CHUNK_SIZE + 7];
        let (manifest, chunks) =
            crate::attachment::chunker::chunk_plaintext(&plaintext, "f", "m").unwrap();
        (manifest, chunks, plaintext)
    }

    /// A source that yields real chunks up to `fail_at`, then errors — to
    /// simulate an I/O failure part-way through reassembly.
    struct FailingSource {
        inner: MemSource,
        fail_at: u32,
    }
    impl ChunkSource for FailingSource {
        fn get(&self, index: u32) -> Result<Vec<u8>> {
            if index >= self.fail_at {
                // Any error works; this is the one `MemSource` itself returns
                // for a missing index, so the fixtures stay consistent.
                return Err(crate::attachment::AttachmentErrorKind::SizeMismatch.into());
            }
            self.inner.get(index)
        }
    }

    /// The `.part` path `reassemble` uses for a given output path.
    fn part_path(out: &std::path::Path) -> std::path::PathBuf {
        let mut s = out.as_os_str().to_owned();
        s.push(".part");
        std::path::PathBuf::from(s)
    }

    #[test]
    fn io_failure_midway_leaves_no_plaintext_behind() {
        // #156: the `.part` holds decrypted plaintext. An error part-way
        // through must not leave it on disk.
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out.bin");
        let (manifest, chunks, _plaintext) = fixture_multi_chunk();
        let source = FailingSource {
            inner: mem_source(chunks),
            fail_at: 1, // chunk 0 succeeds and is written, chunk 1 errors
        };

        let res = reassemble(&manifest, &source, &out);

        assert!(res.is_err(), "expected the source failure to propagate");
        assert!(!part_path(&out).exists(), "decrypted .part must be removed");
        assert!(!out.exists(), "no partial output");
    }

    #[test]
    fn panic_midway_leaves_no_plaintext_behind() {
        // The property explicit per-site cleanup cannot give us.
        struct PanickingSource(MemSource);
        impl ChunkSource for PanickingSource {
            fn get(&self, index: u32) -> Result<Vec<u8>> {
                if index >= 1 {
                    panic!("simulated panic mid-reassembly");
                }
                self.0.get(index)
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out.bin");
        let (manifest, chunks, _plaintext) = fixture_multi_chunk();
        let source = PanickingSource(mem_source(chunks));

        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = reassemble(&manifest, &source, &out);
        }));

        assert!(res.is_err(), "the panic must propagate");
        assert!(!part_path(&out).exists(), "decrypted .part must be removed on unwind");
    }

    #[test]
    fn success_leaves_no_part_file() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out.bin");
        let (manifest, chunks, plaintext) = fixture_multi_chunk();

        reassemble(&manifest, &mem_source(chunks), &out).unwrap();

        assert_eq!(std::fs::read(&out).unwrap(), plaintext);
        assert!(!part_path(&out).exists(), "temp must not survive success");
    }
```

Both helpers above are ready to use as written — verified against the source:

- `chunk_plaintext(&plaintext, "f", "m") -> (AttachmentManifest, Vec<Vec<u8>>)` is exactly what the existing `round_trips_byte_identical` test uses, so the encryption and hashes stay consistent. `CHUNK_SIZE` is 49 152 (`attachment/mod.rs:25`), so `CHUNK_SIZE + 7` yields **two** chunks — one full, one of 7 bytes.
- `AttachmentErrorKind::SizeMismatch` exists and is what `MemSource::get` already returns for a missing index.

- [ ] **Step 2: Run tests to verify they fail**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib reassembler`
Expected: `io_failure_midway_leaves_no_plaintext_behind` and `panic_midway_leaves_no_plaintext_behind` FAIL on the `.part` assertion (the file is left behind). `success_leaves_no_part_file` should already pass. Report the actual output.

- [ ] **Step 3: Adopt the guard**

In `reassemble`, replace the comment block and add the guard. The current code reads:

```rust
    let tmp = std::path::PathBuf::from(tmp_os);
    // Validation-error paths (hash/AEAD/size) below remove `tmp`; the
    // `?`-propagating I/O errors (create/write/sync/rename) deliberately don't
    // — those are genuine disk failures, the `.part` is never mistaken for
    // output (rename only happens on full success), and a re-run truncates it.
    let mut out = std::fs::File::create(&tmp)?;
```

Replace with:

```rust
    let tmp = std::path::PathBuf::from(tmp_os);
    // The temp file holds DECRYPTED plaintext of an attachment that is
    // otherwise kept encrypted at rest, so no failure may leave it behind —
    // not a validation error, not a disk error, not a panic (#156). An
    // earlier version cleaned up only the validation paths on the grounds
    // that a stray `.part` is never mistaken for real output; true, but it
    // answers a correctness question rather than the security one.
    let cleanup = crate::on_drop::OnDrop::new({
        let tmp = tmp.clone();
        move || {
            let _ = std::fs::remove_file(&tmp);
        }
    });
    let mut out = std::fs::File::create(&tmp)?;
```

Then **delete all three** `let _ = std::fs::remove_file(&tmp);` lines on the hash-mismatch, AEAD-failure and size-mismatch paths — the guard now covers them, and keeping both would be two mechanisms for one job.

Finally, disarm after the successful rename:

```rust
    std::fs::rename(&tmp, output_path)?;
    // Renamed away: there is no temp left to clean up.
    cleanup.disarm();
    Ok(())
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib reassembler`
Expected: PASS — the new tests plus every pre-existing reassembler test. The hash / AEAD / size tests must still pass unchanged: they now clean up via the guard rather than explicitly, but their assertions are about the returned error and must not have been touched.

- [ ] **Step 5: Full core suite**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness 2>&1 | tail -15`
Expected: 0 failures. `reassemble` is on the attachment save path, so a regression here surfaces broadly.

- [ ] **Step 6: Gate**

```bash
. "$HOME/.cargo/env" && cargo fmt --all -- --check \
  && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings
```
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/attachment/reassembler.rs
git commit -s -m "fix(attachment): never leave decrypted plaintext after a failed reassembly

reassemble streams plaintext to <output>.part and renames it into place.
The validation paths removed the temp explicitly, but the ?-propagating I/O
errors on create/write/sync/rename did not — so a disk-full, a read-only
mount, or a permissions change mid-write left a complete decrypted copy at a
path the user never chose. No adversary required.

An OnDrop guard now covers every exit path including panics, and the three
explicit remove_file calls are gone with it — one mechanism, not two.

The previous comment argued the leak was acceptable because a stray .part is
never mistaken for real output. That is true and answers a correctness
question; it does not answer whether plaintext is lying around after a
failure, which is what matters for a product that keeps attachments
encrypted at rest. Replaced rather than left contradicting the code.

Behaviour is otherwise unchanged: same error variants, rename still only on
full success, still no partial output.

Refs #156"
```

---

## Task 3: Adopt the guard for the open-attachment cache (#52)

**Files:**
- Modify: `crates/core/src/daemon/state.rs` — `run_with_transport` (the boot wipe is at `:329`, the clean-shutdown wipe at `:597`)

**Interfaces:**
- Consumes from Task 1: `crate::on_drop::OnDrop::new`.
- Produces: nothing.

**Background.** `wipe_open_cache(data_dir)` removes `<data_dir>/cache/open/`, where an attachment is decrypted on demand. It runs at boot and at clean shutdown, but not when `run_with_transport` returns early or a spawned task panics mid-teardown — so plaintext can linger until the next boot. The severity is low by #52's own text (the cache is inside the `0700` data dir and the next boot clears it), but it defeats the decrypt-on-demand-then-wipe guarantee.

- [ ] **Step 1: Arm a never-disarmed guard**

In `run_with_transport`, immediately after the existing boot wipe at `state.rs:329`:

```rust
    wipe_open_cache(data_dir);
```

add:

```rust
    // Backstop for every exit path this function has, including an early
    // return or a panic during teardown — the clean-shutdown wipe below only
    // runs when we reach it (#52). Never disarmed: the cache should be empty
    // however we leave. `wipe_open_cache` warns on its own errors and is
    // idempotent, so running twice on the clean path is harmless.
    let _open_cache_guard = crate::on_drop::OnDrop::new({
        let data_dir = data_dir.to_path_buf();
        move || crate::daemon::state::wipe_open_cache(&data_dir)
    });
```

Bind it to `_open_cache_guard`, not `_` — `let _ = ...` drops the guard **immediately**, which would run the wipe at once and defeat the entire purpose. This is the single most likely way to get this task wrong.

**Leave both existing `wipe_open_cache` calls in place.** The boot one at `:329` clears the *previous* process's residue, which a drop guard cannot do. The shutdown one at `:597` keeps the wipe happening at the intended moment rather than whenever the frame unwinds.

If `data_dir`'s type does not offer `.to_path_buf()` at that point, adapt to whatever owned form is available (it is a `&Path` in this signature) — the closure must own its copy, since it outlives the borrow.

- [ ] **Step 2: Write the test**

Add to `state.rs`'s existing `mod tests` (it already contains `wipe_open_cache_removes_decrypted_plaintext` at `:987`, which shows the fixture style — a `tempfile::tempdir`, a populated `cache/open/`, then an assertion):

```rust
    #[test]
    fn open_cache_guard_wipes_on_early_return() {
        // #52: the wipe must happen even when the clean-shutdown call is never
        // reached. Exercises the guard directly rather than standing up a whole
        // daemon: the property under test is "dropping the guard wipes", and
        // `run_with_transport` arms exactly this guard.
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache").join("open");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("decrypted.bin"), b"plaintext").unwrap();
        assert!(cache.join("decrypted.bin").exists());

        {
            let data_dir = tmp.path().to_path_buf();
            let _guard = crate::on_drop::OnDrop::new(move || super::wipe_open_cache(&data_dir));
            // Simulate an early return: the guard goes out of scope without any
            // explicit wipe having run.
        }

        assert!(!cache.exists(), "open cache must be wiped on early return");
    }
```

- [ ] **Step 3: Run the tests**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib -- wipe_open_cache open_cache_guard`
Expected: PASS — the new test and the pre-existing `wipe_open_cache_removes_decrypted_plaintext`.

- [ ] **Step 4: Verify the daemon still shuts down cleanly**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-tests 2>&1 | tail -15`
Expected: 0 failures. In particular `clean_shutdown_leaves_only_encrypted_db` and the loopback guardrails drive `run_with_transport` to completion — they prove the guard did not disturb teardown ordering or fire at the wrong time.

- [ ] **Step 5: Gate**

```bash
. "$HOME/.cargo/env" && cargo fmt --all -- --check \
  && cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings
```
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/state.rs
git commit -s -m "fix(daemon): wipe the open-attachment cache on every exit path

wipe_open_cache ran at boot and at clean shutdown, but not when
run_with_transport returned early or a spawned task panicked during
teardown — so a decrypted attachment could linger until the next boot.

An OnDrop guard, armed after the boot wipe and never disarmed, makes the
wipe happen however the function exits. Both existing calls stay: the boot
one clears the previous process's residue, which a drop guard cannot, and
the shutdown one keeps the wipe at its intended moment rather than whenever
the frame unwinds. wipe_open_cache is idempotent and warns on its own
errors, so the overlap is harmless.

Refs #52"
```

---

## Task 4: Full gate, docs, and issue bookkeeping

**Files:**
- Modify: `CHANGELOG.md`

**Interfaces:** consumes the complete branch.

- [ ] **Step 1: Full local gate**

```bash
. "$HOME/.cargo/env" \
  && cargo fmt --all -- --check \
  && cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings \
  && cargo test \
  && cargo clippy -p skattr-ui --all-targets -- -D warnings \
  && cargo deny check
```
Expected: every command exits 0. Capture the test counts for the PR body.

- [ ] **Step 2: Verify the two guarantees by name**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --lib -- io_failure_midway panic_midway open_cache_guard on_drop 2>&1 | grep -E "^test |test result:"
```
Expected: the leak-prevention tests and the four `OnDrop` tests present and passing. These are the security case for this branch.

- [ ] **Step 3: Confirm the superseded comment is gone**

```bash
grep -n "deliberately don't" crates/core/src/attachment/reassembler.rs && echo "STILL PRESENT — must be replaced" || echo "superseded comment removed"
```
Expected: `superseded comment removed`. Code contradicting its own comment is a defect in itself.

- [ ] **Step 4: CHANGELOG entry**

`CHANGELOG.md` has a live `## [Unreleased] — targeting v0.1.14` section with a `### Fixed` list. Add an entry covering both issues, in the **user-facing, symptom-first** voice of its neighbours (read two of them first — they describe what the user would have experienced, not the implementation).

The user-visible substance: if saving a received file failed part-way — a full disk, for example — a readable copy of it could be left on disk next to where you asked it to go, even though attachments are otherwise kept encrypted. Decrypted copies are now always cleaned up when something goes wrong. Reference `(#156, #52)`.

- [ ] **Step 5: Commit**

```bash
git add CHANGELOG.md
git commit -s -m "docs(changelog): record the plaintext-cleanup fix (#156, #52)"
```

**Do NOT push or open the PR** — the maintainer handles that.

---

## Self-Review

**1. Spec coverage**

| Spec section | Task |
|---|---|
| §3 the `OnDrop` guard, `disarm(self)` by value, non-panicking closure | Task 1 |
| §4 reassembler adopts it; three explicit `remove_file` calls removed; disarm after rename | Task 2 Step 3 |
| §4 behaviour unchanged (error variants, no partial output) | Task 2 Steps 4-5 (pre-existing tests unchanged) |
| §5 cache guard armed, never disarmed; both existing calls retained | Task 3 Step 1 |
| §6 exclusions — no overwrite, no relocation, no path validation | Global Constraints; no task does any |
| §7 tests 1-2 (I/O failure, panic) | Task 2 Step 1 |
| §7 test 3 (validation paths still clean up) | Task 2 Step 4 — pre-existing tests, assertions untouched |
| §7 test 4 (success leaves no temp) | Task 2 Step 1, `success_leaves_no_part_file` |
| §7 test 5 (`OnDrop` units) | Task 1 Step 1 — 4 tests |
| §7 test 6 (#52 early return) | Task 3 Step 2 |
| §8 comment replaced, not left | Task 2 Step 3, verified in Task 4 Step 3 |

No gaps.

**2. Placeholder scan:** No TBD/TODO. Every code step carries literal code. The first draft left two "you must build this" hedges in Task 2 Step 1; both were resolved against the source rather than passed to the implementer. `fixture_multi_chunk()` is now written out, built on the same `chunk_plaintext(&plaintext, "f", "m")` call the existing `round_trips_byte_identical` test uses, with `CHUNK_SIZE + 7` (49 152 + 7) confirmed to yield the two chunks the test requires. `AttachmentErrorKind::SizeMismatch` is confirmed to exist and is the variant `MemSource::get` itself returns for a missing index. A stray typo in the third test's signature was also fixed.

**3. Type consistency:** `OnDrop<F: FnOnce()>` with `new(f) -> Self` and `disarm(self)` is defined in Task 1 and used with that exact shape in Tasks 2 and 3. Both call sites clone an owned path into the closure, matching `FnOnce()` with no arguments and no return. `wipe_open_cache(&Path)` matches the existing signature at `state.rs:902`.

**One risk worth naming:** Task 3's guard must be bound to a named variable (`_open_cache_guard`). Writing `let _ = OnDrop::new(...)` drops it immediately and wipes the cache at once — which would very likely still pass the tests in Task 3 Step 3 (the cache would be empty either way) but would break the daemon at runtime by deleting the cache directory during startup. Task 3 Step 1 calls this out explicitly, and Step 4's `skattr-tests` run is what would catch it, since those drive a real daemon that decrypts and opens attachments.
