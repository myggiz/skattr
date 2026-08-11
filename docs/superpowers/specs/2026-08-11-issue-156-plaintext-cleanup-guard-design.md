# #156 + #52 — plaintext cleanup guard (design)

**Issues:** #156 (`security`, `attachments`, v0.1.2) and #52 (`security`, v0.1.2) — closed together, see §2.
**Branch:** `156-plaintext-cleanup-guard`
**Relates to:** PR #155 / #118, where #156 was found by review and deferred as a core change.

**No wire-format change, no migration, no new dependency, no new public API** — the guard is `pub(crate)`.

---

## 1. Problem

Skattr keeps attachments **encrypted at rest**. Plaintext is meant to exist only where and when the user asks for it. Two paths break that on failure:

**#156 — the reassembly temp file.** `attachment::reassembler::reassemble` streams decrypted plaintext to `<output_path>.part` and renames it into place at the end. The validation paths (hash / AEAD / size) remove the temp, but the `?`-propagating I/O errors on `create` / `write_all` / `sync_all` / `rename` do not. A disk-full, a read-only mount, or a permissions change mid-write therefore leaves a complete decrypted copy at a path the user never chose. No adversary is required.

**#52 — the open-attachment cache.** `daemon::state::wipe_open_cache` clears `<data_dir>/cache/open/` at boot (`state.rs:329`) and clean shutdown (`state.rs:597`), but not if `run_with_transport` returns early or a spawned task panics mid-teardown. A decrypted attachment then lingers until the next boot.

Both are the same shape: *decrypted plaintext is written to a path, and an error or panic can leave it there.*

### The existing decision this revisits

`reassembler.rs` currently carries a comment defending the behaviour:

> Validation-error paths (hash/AEAD/size) below remove `tmp`; the `?`-propagating I/O errors (create/write/sync/rename) deliberately don't — those are genuine disk failures, the `.part` is never mistaken for output (rename only happens on full success), and a re-run truncates it.

That reasoning is sound *on its own terms*: it answers "could the `.part` be mistaken for real output?" (no) and "does it leak into normal operation?" (no). It does not address the security question — the `.part` holds plaintext of a file the product otherwise keeps encrypted, so leaving it breaks the at-rest guarantee regardless of whether anything mistakes it for output. "A re-run truncates it" also assumes a re-run happens, to the same destination.

This spec **replaces** that comment rather than silently contradicting it. Whoever reads the new code must be able to see that the trade-off was revisited deliberately.

---

## 2. Why the two issues ship together

They want the same mechanism, and building it twice invites drift. Doing both also means the abstraction has two real users on the day it lands, rather than being speculative — which is the bar `~/standards/restraints.md` sets for introducing one at all.

**A correction to the obvious shape:** a guard that stores a path and calls `remove_file` serves #156 only. `wipe_open_cache` removes a **directory tree**, not a file. So the shared piece is "run this cleanup on drop", not "delete this path".

---

## 3. The guard

New internal module `crates/core/src/on_drop.rs`, registered as `pub(crate) mod on_drop;`. It goes at the top level rather than inside `attachment/` because #52's user is `daemon/state.rs`; `prelude` is public re-exports only and is not a home for internals.

```rust
/// Runs `f` when dropped, unless disarmed.
pub(crate) struct OnDrop<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> OnDrop<F> {
    pub(crate) fn new(f: F) -> Self;
    /// Cancel the cleanup — consumes the guard.
    pub(crate) fn disarm(self);
}

impl<F: FnOnce()> Drop for OnDrop<F> { /* runs f if still armed */ }
```

`disarm` takes `self` by value so a disarmed guard cannot be used again, and so the disarm site reads as the moment ownership of the plaintext transfers.

The closure must not panic: a panic inside `Drop` during unwinding aborts the process. Both call sites use `let _ = std::fs::remove*`, which cannot panic.

---

## 4. #156 — the reassembler

`reassemble` arms the guard on the `.part` path immediately after computing it, and disarms only after `rename` returns `Ok`. At that point the temp no longer exists, so disarming is about intent, not necessity.

Consequences:
- The three existing explicit `let _ = std::fs::remove_file(&tmp);` calls on the validation paths become redundant and are **removed** — the guard covers them. Leaving both would be two mechanisms for one job.
- Every `?` on `create` / `write_all` / `sync_all` / `rename` now cleans up.
- A panic anywhere in the loop cleans up.

**Behaviour that must not change:** the function still produces no partial output — `rename` still happens only on full success, and the error variants returned are unchanged. This is a cleanup change, not a semantics change.

---

## 5. #52 — the open-attachment cache

`run_with_transport` arms a guard on `wipe_open_cache(data_dir)` near the top, and **never disarms** it: the cache should be wiped on every exit, clean or not.

The existing explicit calls stay:
- `state.rs:329` (boot) — a drop guard cannot cover the *previous* process's residue.
- `state.rs:597` (clean shutdown) — kept so the wipe happens at the intended moment rather than whenever the frame unwinds; the guard is the backstop for paths that never reach it.

`wipe_open_cache` already swallows and warns its own errors, so it is safe to call from `Drop` and safe to call twice.

---

## 6. What this does *not* do

- **No overwrite-before-delete.** On journalling filesystems and SSDs with wear levelling, overwriting a file does not reliably erase the underlying blocks, so it buys confidence rather than security. `remove_file` is what `wipe_open_cache` already does; matching it keeps one story.
- **No relocation of the temp file** to a daemon-controlled directory. The destination may be on a different mount, and `rename` across filesystems fails — which would break the atomicity that makes "no partial output" true. The temp must stay adjacent to the destination.
- **No path validation** — #54 still owns that.
- **No change to the CLI's directory check** from #155. It stays as a UX guard; this work is what makes it non-load-bearing.

---

## 7. Testing

1. **I/O failure mid-stream leaves no `.part`** — a `ChunkSource` whose `get` returns `Err` on a later index; assert `reassemble` errors and no `.part` remains. This is the #156 regression guard.
2. **A panic mid-reassembly leaves no `.part`** — a `ChunkSource` whose `get` panics, caught with `catch_unwind`; assert the temp is gone. This is what distinguishes the guard from explicit cleanup.
3. **The existing validation paths still clean up** — the current hash / AEAD / size tests must pass unchanged, now via the guard.
4. **Success still renames and leaves no temp** — existing round-trip tests, plus an assertion that no `.part` survives.
5. **`OnDrop` unit tests** — runs on drop; does not run after `disarm`; runs during unwind.
6. **#52: an early return from teardown still wipes the cache** — arrange `run_with_transport` to exit early with a populated cache dir and assert it is empty afterwards.

**Gate:** `cargo fmt --all -- --check`, `cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings`, `cargo test`, `cargo deny check`. CI runs the UI job on the PR.

---

## 8. Acceptance

| Issue | Criterion | Where |
|---|---|---|
| #156 | An I/O failure during reassembly leaves no decrypted bytes on disk | §4, tests 1-2 |
| #156 | Proven by a test that forces a write/rename failure | test 1 |
| #156 | Holds for every caller of `SaveAttachment`, not only the CLI | §4 — the guard is inside `reassemble`, below every caller |
| #52 | The open cache is wiped even on a panicked / early-return teardown | §5, test 6 |
| #52 | Test simulates an early return and asserts the cache is empty | test 6 |
| both | The superseded comment in `reassembler.rs` is replaced, not left contradicting the code | §4 |
