# #114 Attachment Observability + Honest Sender Status — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a stalled attachment transfer visible — in the default log on both sides, and on the sender's UI bubble — instead of silently reading as "Delivered".

**Architecture:** Three layers, each independently testable. (1) A new pure `ServedTracker` in `chunk_transfer.rs` owns sender-side served-index state, so the counting logic is unit-testable without driving the actor. (2) `peer.rs` wires the tracker into the `ChunkRequest` arm to emit `attachment_progress`, and gains six tracing calls at currently-silent success sites. (3) `FileAttachmentBubble.svelte` lets transfer state supersede the manifest-ack delivery icon for outgoing bubbles.

**Tech Stack:** Rust 2021 (tokio, tracing, hex), Svelte 5 runes + TypeScript, vitest + @testing-library/svelte.

**Spec:** `docs/superpowers/specs/2026-07-25-issue-114-attachment-observability-design.md`

**Branch:** `114-attachment-observability` (already created; spec committed as `f63156e`)

## Global Constraints

- **No wire-format change.** No new `Frame` variant, no new `Event` variant, no new IPC command, no migration. `Event::AttachmentProgress` already carries `(attachment_id, received, total)`.
- **Redaction:** tracing may carry `attachment_id` (hex), `index`, `received`, `total`, `bytes`. **Never** `filename` (user content) or peer pubkey / onion (identity material).
- **Log levels:** `info!` for per-transfer milestones (~4 per transfer); `debug!` for per-chunk events; existing `warn!` sites unchanged.
- **Rust:** no `unwrap()` / `expect()` in library code (tests may, via the existing `#![allow]` in test modules). `cargo clippy -D warnings` is the done-gate.
- **TypeScript:** `strict`; no `any`, no `!`, no `ts-ignore`. `pnpm check` must pass at **0 errors / 0 warnings** (`--fail-on-warnings` is wired).
- **Hex encoding:** use `hex::encode(...)` — the established style in core (`attachment/store.rs:26`).
- **Cargo is not on PATH:** prefix every cargo command with `. "$HOME/.cargo/env" &&`.
- **License header:** every `.rs` file starts with the GPLv3 SPDX header; every new `.svelte`/`.ts` file starts with the two-line SPDX/Copyright comment. Only relevant if a file is created — no new files are created by this plan.

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/core/src/delivery/chunk_transfer.rs` | Chunk state machines (`ChunkRx`, serve, sanitize). Gains `ServedTracker` — the sender-side mirror of `ChunkRx`. | Modify: add `ServedTracker` + its tests; add `debug!` to `serve_chunk_request` |
| `crates/core/src/delivery/peer.rs` | Per-peer actor. Wires `ServedTracker` into the `ChunkRequest` arm; hosts five of the six tracing sites. | Modify: 5 tracing sites + tracker wiring |
| `crates/ui/src-svelte/src/lib/components/FileAttachmentBubble.svelte` | Renders a file bubble. Sender-side transfer state supersedes `DeliveryIcon`. | Modify: derivations + template |
| `crates/ui/src-svelte/src/lib/components/FileAttachmentBubble.test.ts` | Bubble behavior tests. | Modify: add 3 outgoing-bubble tests |

**Task order rationale:** Task 1 is pure logic with no dependencies. Task 2 consumes Task 1. Task 3 is tracing-only (no behavior) and touches the same file as Task 2, so it lands after to avoid churn. Task 4 is UI, independent of 1-3 at the code level (it consumes an event the daemon already emits, plus the new sender emissions from Task 2).

---

## Task 1: `ServedTracker` — sender-side served-index state

**Files:**
- Modify: `crates/core/src/delivery/chunk_transfer.rs` (add struct near `ChunkRx`; add tests to the existing `mod tests`)

**Interfaces:**
- Consumes: nothing (pure, no dependencies).
- Produces, used by Task 2:
  - `pub(crate) struct ServedTracker` — `Default`-constructible.
  - `pub(crate) fn record(&mut self, attachment_id: &[u8; 16], index: u32) -> u32` — records a served index, returns the new distinct-served count for that attachment.
  - `pub(crate) fn is_new(&self, attachment_id: &[u8; 16]) -> bool` — true if no index has been recorded for this attachment yet (drives the once-per-transfer `info!` in Task 3).
  - `pub(crate) fn forget(&mut self, attachment_id: &[u8; 16])` — drops all state for a finished attachment.

**Why a set, not a counter:** chunk requests arrive windowed (`CHUNK_WINDOW` = 8) and out of order, and may be retried. A counter would over-count on retry; `max(index)+1` would over-report on out-of-order arrival. Either would display a progress figure that lies — the exact defect class this issue exists to remove.

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` block at the bottom of `crates/core/src/delivery/chunk_transfer.rs` (it already has `use super::*;` and the `#![allow(clippy::unwrap_used, …)]` attributes):

```rust
    const AID_A: [u8; 16] = [0xA1; 16];
    const AID_B: [u8; 16] = [0xB2; 16];

    #[test]
    fn served_tracker_counts_sequential_serves() {
        let mut t = ServedTracker::default();
        assert_eq!(t.record(&AID_A, 0), 1);
        assert_eq!(t.record(&AID_A, 1), 2);
        assert_eq!(t.record(&AID_A, 2), 3);
    }

    #[test]
    fn served_tracker_counts_distinct_not_max_index() {
        // Out-of-order arrival: index 5 first, then 0. Count is 1 then 2 —
        // never 6 (which `max(index)+1` would wrongly report).
        let mut t = ServedTracker::default();
        assert_eq!(t.record(&AID_A, 5), 1);
        assert_eq!(t.record(&AID_A, 0), 2);
    }

    #[test]
    fn served_tracker_ignores_duplicate_index() {
        // A retried request for an already-served index must not advance.
        let mut t = ServedTracker::default();
        assert_eq!(t.record(&AID_A, 3), 1);
        assert_eq!(t.record(&AID_A, 3), 1);
        assert_eq!(t.record(&AID_A, 4), 2);
    }

    #[test]
    fn served_tracker_keeps_attachments_separate() {
        let mut t = ServedTracker::default();
        assert_eq!(t.record(&AID_A, 0), 1);
        assert_eq!(t.record(&AID_B, 0), 1);
        assert_eq!(t.record(&AID_A, 1), 2);
    }

    #[test]
    fn served_tracker_is_new_only_before_first_record() {
        let mut t = ServedTracker::default();
        assert!(t.is_new(&AID_A));
        t.record(&AID_A, 0);
        assert!(!t.is_new(&AID_A));
        // A different attachment is still new.
        assert!(t.is_new(&AID_B));
    }

    #[test]
    fn served_tracker_forget_drops_state() {
        let mut t = ServedTracker::default();
        t.record(&AID_A, 0);
        t.record(&AID_A, 1);
        t.forget(&AID_A);
        assert!(t.is_new(&AID_A));
        assert_eq!(t.record(&AID_A, 7), 1);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core served_tracker`
Expected: FAIL — `cannot find type ServedTracker in this scope` (compile error).

- [ ] **Step 3: Implement `ServedTracker`**

Add immediately **after** the `impl ChunkRx { … }` block in `crates/core/src/delivery/chunk_transfer.rs` (before the `sanitize_filename` section), so the sender-side mirror sits next to the receiver-side state machine:

```rust
/// Sender-side mirror of [`ChunkRx`]: tracks which chunk indices we have
/// actually served, per attachment, so the sender can report honest progress.
///
/// Distinct indices are tracked (not a bare counter, not `max(index) + 1`)
/// because chunk requests arrive windowed and out of order, and may be retried
/// — either shortcut would report a progress figure that overstates reality.
///
/// In-memory and per-actor: this resets on reconnect, which is acceptable for a
/// progress indicator (see the design's "no durable sender progress" exclusion).
#[derive(Default)]
pub(crate) struct ServedTracker {
    served: HashMap<[u8; 16], std::collections::HashSet<u32>>,
}

impl ServedTracker {
    /// Record `index` as served for `attachment_id`; returns the new count of
    /// distinct indices served for that attachment.
    pub(crate) fn record(&mut self, attachment_id: &[u8; 16], index: u32) -> u32 {
        let set = self.served.entry(*attachment_id).or_default();
        set.insert(index);
        set.len() as u32
    }

    /// True when nothing has been served yet for `attachment_id` — used to log
    /// the once-per-transfer "serving" milestone exactly once.
    pub(crate) fn is_new(&self, attachment_id: &[u8; 16]) -> bool {
        !self.served.contains_key(attachment_id)
    }

    /// Drop all state for a finished attachment.
    pub(crate) fn forget(&mut self, attachment_id: &[u8; 16]) {
        self.served.remove(attachment_id);
    }
}
```

`HashMap` is already imported at the top of this file (used by `ChunkRx::inflight`). If `cargo build` reports it is not, add `use std::collections::HashMap;` to the existing import block rather than fully qualifying it.

- [ ] **Step 4: Run tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core served_tracker`
Expected: PASS — 6 tests.

- [ ] **Step 5: Run the gate for this file**

Run:
```bash
. "$HOME/.cargo/env" && cargo fmt --all -- --check \
  && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings
```
Expected: clean, no output from clippy beyond the compile summary.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/delivery/chunk_transfer.rs
git commit -m "feat(delivery): add ServedTracker for sender-side chunk progress

Tracks distinct served indices per attachment. A counter would over-count
on retried requests and max(index)+1 would over-report on out-of-order
arrival — both would display progress that overstates reality.

Refs #114"
```

---

## Task 2: Emit sender-side progress as chunks are served

**Files:**
- Modify: `crates/core/src/delivery/peer.rs` — actor local state (~line 521, beside `active_rx`), the `Frame::ChunkRequest` arm (~line 808), the `Frame::AttachmentComplete` arm (~line 954)

**Interfaces:**
- Consumes from Task 1: `ServedTracker::{default, record, is_new, forget}` (signatures in Task 1's Interfaces block).
- Consumes (existing, unchanged): `InboundDispatch::attachment_progress(&self, attachment_id: [u8; 16], received: u32, total: u32)` — a defaulted no-op trait method on the `Option<Arc<dyn InboundDispatch>>` named `inbound` in the actor.
- Produces: sender-side `Event::AttachmentProgress` emissions, consumed by Task 4's UI.

**Behavior:** after a `Frame::Chunk` reply is **successfully sent**, record the index and emit progress. Not on a nack; not when the send errored (that path drops the connection).

- [ ] **Step 1: Declare the tracker in the actor loop**

In `crates/core/src/delivery/peer.rs`, find this line (~521):

```rust
    let mut active_rx: Option<crate::delivery::chunk_transfer::ChunkRx> = None;
```

Add immediately after it:

```rust
    // Sender-side served-index state, so we can report honest outbound
    // progress. Per-actor and in-memory: resets on reconnect (#114).
    let mut served = crate::delivery::chunk_transfer::ServedTracker::default();
```

- [ ] **Step 2: Emit progress in the `ChunkRequest` arm**

In the `Ok(Some(Frame::ChunkRequest { attachment_id, index }))` arm (~line 808), locate the send block, which currently reads:

```rust
                            if let Some(c) = conn.as_mut() {
                                if let Err(e) = c.send(reply).await {
                                    tracing::warn!(
                                        err = %e,
                                        "peer: failed to send chunk reply; dropping connection"
                                    );
                                    conn = None;
                                    drain_pending(&mut pending);
                                }
                            }
```

Replace it with the version below. Note `served_this_reply` is computed **before** the send (the `reply` value is moved into `send`), and progress is emitted only on the success branch:

```rust
                            // Only a real Chunk reply counts as served — a nack
                            // means we could not serve this index.
                            let served_this_reply =
                                matches!(reply, Frame::Chunk { .. }).then_some(index);
                            if let Some(c) = conn.as_mut() {
                                if let Err(e) = c.send(reply).await {
                                    tracing::warn!(
                                        err = %e,
                                        "peer: failed to send chunk reply; dropping connection"
                                    );
                                    conn = None;
                                    drain_pending(&mut pending);
                                } else if let Some(i) = served_this_reply {
                                    let count = served.record(&attachment_id, i);
                                    if let Some(d) = inbound.as_ref() {
                                        d.attachment_progress(attachment_id, count, total);
                                    }
                                }
                            }
```

`total` is already in scope in this arm (bound ~line 815 from the attachment row).

- [ ] **Step 3: Release tracker state on completion**

In the `Ok(Some(Frame::AttachmentComplete { attachment_id }))` arm (~line 954), find:

```rust
                        if let Some(store) = chunk_store.as_ref() {
                            let _ = store.remove(&attachment_id);
                        }
```

Add immediately after that block:

```rust
                        served.forget(&attachment_id);
```

- [ ] **Step 4: Build and run the existing suites to verify no regression**

Run:
```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core \
  && cargo test -p skattr-tests attachment
```
Expected: PASS. In particular `attachment_roundtrip_multichunk_over_loopback` (in `skattr-tests`) must still pass — it drives the real `run_with_transport` assembly through this arm.

- [ ] **Step 5: Run the gate**

Run:
```bash
. "$HOME/.cargo/env" && cargo fmt --all -- --check \
  && cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings
```
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/delivery/peer.rs
git commit -m "feat(delivery): emit sender-side attachment progress as chunks are served

The sender previously got a progress signal only when AttachmentComplete
arrived, so an in-flight or stalled transfer was indistinguishable from a
finished one. Emit progress per successfully-served chunk (nacks and failed
sends excluded), and release tracker state on completion.

Refs #114"
```

---

## Task 3: Tracing the six silent sites

**Files:**
- Modify: `crates/core/src/delivery/chunk_transfer.rs` — `serve_chunk_request` success arm (~line 212, the `Ok(ct) => Frame::Chunk { … }` match arm)
- Modify: `crates/core/src/delivery/peer.rs` — `maybe_start_next_rx` (~line 138), `finalize_rx` (~line 171), `Frame::ChunkRequest` arm (~line 808), `Frame::Chunk` arm (~line 872), `Frame::AttachmentComplete` arm (~line 954)

**Interfaces:**
- Consumes from Task 1: `ServedTracker::is_new` (drives the once-per-transfer A2 milestone).
- Consumes from Task 2: the `served` local declared in the actor loop.
- Produces: nothing consumed by later tasks. Tracing has no behavior.

**No new tests.** Asserting on log output would test the tracing subscriber, not this code. The requirement is that existing suites stay green.

- [ ] **Step 1: A1 — `serve_chunk_request` success arm (`debug!`)**

In `crates/core/src/delivery/chunk_transfer.rs`, the current success arm is:

```rust
        Ok(ct) => Frame::Chunk {
            attachment_id: *attachment_id,
            index,
            ciphertext: ct,
        },
```

Replace with:

```rust
        Ok(ct) => {
            tracing::debug!(
                aid = %hex::encode(attachment_id),
                index,
                bytes = ct.len(),
                "attachment: served chunk"
            );
            Frame::Chunk {
                attachment_id: *attachment_id,
                index,
                ciphertext: ct,
            }
        }
```

- [ ] **Step 2: A2 — first ChunkRequest for an attachment (`info!`)**

In `crates/core/src/delivery/peer.rs`, in the `Ok(Some(Frame::ChunkRequest { attachment_id, index }))` arm, immediately **after** the `let total = …;` binding (~line 815) and **before** the `let reply = …` binding, insert:

```rust
                            if served.is_new(&attachment_id) {
                                tracing::info!(
                                    aid = %hex::encode(attachment_id),
                                    total,
                                    "attachment: serving to peer"
                                );
                            }
```

This fires once per transfer because Task 2's `served.record(...)` runs later in the same arm — a 213-chunk transfer logs one line, not 213.

- [ ] **Step 3: A3 — chunk received and verified (`debug!`)**

In the `Ok(Some(Frame::Chunk { attachment_id, index, ciphertext }))` arm, find the existing progress computation (~line 872):

```rust
                                    rx.on_received(index);
                                    let (recv, total) = rx.progress();
```

Insert immediately after those two lines:

```rust
                                    tracing::debug!(
                                        aid = %hex::encode(attachment_id),
                                        index,
                                        received = recv,
                                        total,
                                        "attachment: chunk received"
                                    );
```

- [ ] **Step 4: A4 — inbound transfer begins (`info!`)**

In `maybe_start_next_rx`, the tail of the loop currently reads:

```rust
        let reqs = rx.next_requests();
        let aid = rx.attachment_id();
        let _ = send_chunk_requests(conn, aid, &reqs).await;
        return Some(rx);
```

Replace with:

```rust
        let reqs = rx.next_requests();
        let aid = rx.attachment_id();
        let (_, total) = rx.progress();
        tracing::info!(
            aid = %hex::encode(aid),
            total,
            "attachment: fetching from peer"
        );
        let _ = send_chunk_requests(conn, aid, &reqs).await;
        return Some(rx);
```

- [ ] **Step 5: A5 — finalize outcome (`info!`)**

In `finalize_rx`, the CAS result is currently used as:

```rust
    if won {
```

Insert immediately **before** that `if won {` line:

```rust
    let (_, total) = rx.progress();
    tracing::info!(
        aid = %hex::encode(aid),
        total,
        won,
        "attachment: transfer complete"
    );
```

Logging both outcomes is deliberate: `won = false` means the other lane (direct vs offline) finalized first, and "which lane won" was itself an open question during the #76 diagnosis. The `Err` path above already `warn!`s and returns, so it is not covered here.

- [ ] **Step 6: A6 — sender sees the completion ack (`info!`)**

In the `Ok(Some(Frame::AttachmentComplete { attachment_id }))` arm, insert as the **first** statement after `last_traffic = tokio::time::Instant::now();`:

```rust
                        tracing::info!(
                            aid = %hex::encode(attachment_id),
                            "attachment: delivery acked by peer"
                        );
```

- [ ] **Step 7: Verify no forbidden field slipped in**

Run:
```bash
grep -nE "tracing::(info|debug)!" crates/core/src/delivery/peer.rs crates/core/src/delivery/chunk_transfer.rs \
  | grep -iE "filename|peer = |onion|pubkey"
```
Expected: **no output.** Any hit is a redaction-rule violation and must be removed before committing.

- [ ] **Step 8: Run the full core + integration suites**

Run:
```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core && cargo test -p skattr-tests
```
Expected: PASS, no regressions.

- [ ] **Step 9: Run the gate**

Run:
```bash
. "$HOME/.cargo/env" && cargo fmt --all -- --check \
  && cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings
```
Expected: clean.

- [ ] **Step 10: Commit**

```bash
git add crates/core/src/delivery/peer.rs crates/core/src/delivery/chunk_transfer.rs
git commit -m "feat(delivery): trace the attachment chunk path

The chunk path emitted tracing only on failure, so a successful or stalled
transfer produced zero lines at any level — AttachmentComplete was literally
unloggable, which cost hours of two-machine forensics in #115.

Six sites: serve (debug), first-serve (info), chunk-received (debug),
fetch-start (info), finalize (info), delivery-acked (info). ~4 info lines
per transfer. Fields limited to attachment_id/index/counts — never filename
or peer identity.

Refs #114, #76"
```

---

## Task 4: Honest sender bubble

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/components/FileAttachmentBubble.svelte` — derivations (~lines 79-86) and template
- Modify: `crates/ui/src-svelte/src/lib/components/FileAttachmentBubble.test.ts` — add 3 tests

**Interfaces:**
- Consumes from Task 2: sender-side `Event::AttachmentProgress` emissions, which reach the store via the existing `+page.svelte` dispatcher arm (`attachment_progress` → `applyProgress`). No change to `+page.svelte` or `stores/attachments.ts` is needed or permitted by this task.
- Consumes (existing): `xferState` = `$attachments.get(aidHex)` with shape `{ status, received, total, filename?, mime?, size?, available?, reason? }`; `deliveryStatus` = `deliveryToIconStatus($delivery.get(hex16ToString(record.message_id)))`.

**The defect being fixed:** the outgoing bubble renders `<DeliveryIcon status={deliveryStatus} />`, where `deliveryStatus` is the MLS Ack of the *manifest message*. The receiver acks that the moment it ingests the manifest — before any chunk moves — so the sender shows "Delivered" on a transfer that has not started. Transfer state must take precedence; `DeliveryIcon` stays as the fallback for when no transfer state exists (pre-first-chunk, or post-restart).

- [ ] **Step 1: Write the failing tests**

Add these three tests inside the existing `describe("FileAttachmentBubble", …)` block in `crates/ui/src-svelte/src/lib/components/FileAttachmentBubble.test.ts`. The file's existing helpers (`fileRecord`, `AID`, `applyProgress`, `markAvailable`, the `beforeEach` reset) are reused as-is.

Add this import near the other imports (the test file does not import from `$lib/stores/delivery` yet):

```ts
import { delivery, recordDeliveryStatus } from "$lib/stores/delivery";
```

Also reset the delivery store in the existing `beforeEach`, beside `attachments.set(new Map())`, so the third test cannot leak into others:

```ts
  delivery.set(new Map());
```

Tests:

```ts
  test("outgoing bubble shows chunk progress, not Delivered, while serving", async () => {
    const { container, findByText } = render(FileAttachmentBubble, {
      props: { record: fileRecord("outgoing") },
    });
    await findByText("photo.jpg");
    applyProgress(AID, 3, 12);
    await tick();
    // Progress row rendered with the served/total figure.
    expect(container.querySelector(".progress")).not.toBeNull();
    await findByText("Sending 3/12");
    // Must NOT claim the transfer finished.
    expect(container.textContent).not.toContain("Delivered");
  });

  test("outgoing bubble shows Delivered once the transfer completes", async () => {
    const { container, findByText } = render(FileAttachmentBubble, {
      props: { record: fileRecord("outgoing") },
    });
    await findByText("photo.jpg");
    applyProgress(AID, 12, 12);
    markAvailable(AID, { filename: "photo.jpg", mime: "image/jpeg", size: 2048 });
    await tick();
    await findByText("Delivered");
    // The in-flight progress row is gone.
    expect(container.querySelector(".progress")).toBeNull();
  });

  test("manifest ack alone does not claim the file transferred (#114 regression)", async () => {
    // The manifest message is MLS-acked before any chunk moves. With no
    // transfer state, the bubble may show the delivery icon but must not
    // assert the transfer completed.
    // ts-rs emits DeliveryStatus as "Queued" | "Delivered" | "Deposited"
    // | { "Failed": string } — the capitalised literal is the wire value.
    recordDeliveryStatus("cd".repeat(16), "Delivered");
    const { container, findByText } = render(FileAttachmentBubble, {
      props: { record: fileRecord("outgoing") },
    });
    await findByText("photo.jpg");
    await tick();
    expect(container.textContent).not.toContain("Delivered");
    expect(container.querySelector(".progress")).toBeNull();
    // The pre-transfer fallback icon is still rendered.
    expect(container.querySelector(".icon")).not.toBeNull();
  });
```

Signature reference (already verified): `recordDeliveryStatus(messageHex: string, status: DeliveryStatus): void`, where `messageHex` is the lowercase-hex `message_id` — `fileRecord` sets it to `"cd".repeat(16)`.

- [ ] **Step 2: Run tests to verify they fail**

Run:
```bash
cd crates/ui/src-svelte && pnpm exec vitest run FileAttachmentBubble
```
Expected: the first two FAIL (no "Sending 3/12" / no "Delivered" text is rendered for outgoing bubbles — every transfer derivation is gated on `!isOutgoing`). The third may pass already; it is a regression guard that must stay green after the change.

- [ ] **Step 3: Add sender-side derivations**

In `crates/ui/src-svelte/src/lib/components/FileAttachmentBubble.svelte`, the current derivations are:

```svelte
  let receiving = $derived(!isOutgoing && xferState?.status === "receiving");
  let complete = $derived(!isOutgoing && (xferState?.status === "complete" || xferState?.available === true));
  let failed = $derived(!isOutgoing && xferState?.status === "failed");
```

Add immediately after them:

```svelte
  // Sender side: chunk-transfer state supersedes the manifest-ack delivery
  // icon. The manifest is MLS-acked before any chunk moves, so the icon alone
  // must never read as "the file arrived" (#114).
  let sendComplete = $derived(isOutgoing && xferState?.status === "complete");
  let sending = $derived(isOutgoing && xferState !== undefined && !sendComplete);
  let sentPct = $derived(
    xferState && xferState.total > 0 ? `${xferState.received}/${xferState.total}` : null,
  );
```

- [ ] **Step 4: Update the template**

Current markup in the card:

```svelte
      {#if isOutgoing && deliveryStatus}
        <DeliveryIcon status={deliveryStatus} />
      {/if}
```

Replace with (the icon becomes the fallback only when there is no transfer state):

```svelte
      {#if isOutgoing && sendComplete}
        <span class="delivered">Delivered</span>
      {:else if isOutgoing && !sending && deliveryStatus}
        <DeliveryIcon status={deliveryStatus} />
      {/if}
```

Then, immediately **after** the existing receiver progress block (the `{#if receiving} … {/if}` block that ends the `{:else}` branch), add the sender progress row:

```svelte
    {#if sending}
      <div class="progress">
        {#if sentPct}
          <span class="label">Sending {sentPct}</span>
        {:else}
          <span class="label">Sending…</span>
        {/if}
      </div>
    {/if}
```

Add to the `<style>` block, beside the existing `.failed` rule:

```css
  .delivered { color: var(--text-muted); font: var(--t-ui); }
  .file-bubble.outgoing .delivered { color: rgba(255, 255, 255, 0.7); }
```

(`--text-muted` plus the outgoing-bubble override mirrors the existing `.fsize` rules at lines 203-204, which handle the same light-text-on-accent-bubble problem.)

- [ ] **Step 5: Run tests to verify they pass**

Run:
```bash
cd crates/ui/src-svelte && pnpm exec vitest run FileAttachmentBubble
```
Expected: PASS — all tests in the file, including the pre-existing receiver tests (they must be unaffected: every new derivation is gated on `isOutgoing`).

- [ ] **Step 6: Run the full UI gate**

Run:
```bash
cd crates/ui/src-svelte && pnpm check && pnpm exec vitest run
```
Expected: `pnpm check` reports **0 errors, 0 warnings**; vitest all green.

Then:
```bash
. "$HOME/.cargo/env" && cargo clippy -p skattr-ui --all-targets -- -D warnings
```
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/ui/src-svelte/src/lib/components/FileAttachmentBubble.svelte \
        crates/ui/src-svelte/src/lib/components/FileAttachmentBubble.test.ts
git commit -m "fix(ui): sender bubble shows chunk-transfer state, not the manifest ack

The outgoing file bubble rendered the MLS Ack of the manifest message, which
the receiver sends before any chunk moves — so a stalled send displayed
'Delivered'. Transfer state now supersedes the delivery icon: 'Sending N/M'
until completion, then 'Delivered'. The icon remains the fallback when no
transfer state exists (pre-first-chunk or post-restart).

Refs #114"
```

---

## Task 5: Full-gate verification and issue close-out

**Files:** none modified (verification only).

**Interfaces:** consumes the complete branch from Tasks 1-4.

- [ ] **Step 1: Run the complete local gate**

The repo is local-first (CI is `workflow_dispatch`-only), so this gate is authoritative:

```bash
. "$HOME/.cargo/env" \
  && cargo fmt --all -- --check \
  && cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings \
  && cargo test \
  && cargo clippy -p skattr-ui --all-targets -- -D warnings \
  && cargo deny check
```
Expected: every command exits 0. Capture the test summary line for the PR body.

- [ ] **Step 2: Run the UI gate**

```bash
cd crates/ui/src-svelte && pnpm check && pnpm exec vitest run
```
Expected: 0 errors / 0 warnings; all vitest specs pass.

- [ ] **Step 3: Confirm the redaction rule holds across the branch**

```bash
git diff master...HEAD -- '*.rs' | grep -E "^\+.*tracing::(info|debug)!" -A4 \
  | grep -iE "filename|peer = |onion|pubkey"
```
Expected: **no output.**

- [ ] **Step 4: Verify the acceptance criteria by inspection**

Confirm each, and note the evidence for the PR body:
1. Sender UI distinguishes completed from in-progress — Task 4 tests 1-2.
2. Sender log records chunk-serve activity, not only failures — Task 3 A1/A2/A6.
3. Existing text delivery-status behavior unchanged — no edit to `delivery.ts` or `+page.svelte` anywhere in the branch (`git diff --stat master...HEAD` must not list them).
4. Receiver side no longer silent — Task 3 A3/A4/A5.

- [ ] **Step 5: Push and open the PR**

```bash
git push -u origin 114-attachment-observability
gh pr create --repo myggiz/skattr --base master \
  --title "fix(#114): attachment observability + honest sender status" \
  --body "<summary of both parts, the gate output from Steps 1-2, and 'Closes #114'>"
```

The PR body must include the actual gate output (not a claim that it passed) and `Closes #114`.

---

## Self-Review

**1. Spec coverage**

| Spec section | Task |
|---|---|
| §3.3 A1 serve chunk (`debug`) | Task 3 Step 1 |
| §3.3 A2 first-serve (`info`) | Task 3 Step 2 |
| §3.3 A3 chunk received (`debug`) | Task 3 Step 3 |
| §3.3 A4 fetch start (`info`) | Task 3 Step 4 |
| §3.3 A5 finalize both outcomes (`info`) | Task 3 Step 5 |
| §3.3 A6 delivery acked (`info`) | Task 3 Step 6 |
| §3.2 field/redaction policy | Task 3 Step 7 grep + Task 5 Step 3 |
| §4.1 served-index set + lifecycle | Tasks 1 and 2 |
| §4.2 UI precedence + fallback | Task 4 Steps 3-4 |
| §4.3 no rename of status strings | Enforced by Task 4's Interfaces block (no store edits) |
| §7 tests 1-4 (core) | Task 1 Step 1 (6 tests, covering all 4 cases) |
| §7 tests 5-7 (UI) | Task 4 Step 1 |
| §7 gate | Task 5 Steps 1-2 |
| §8 acceptance criteria | Task 5 Step 4 |

No gaps.

**2. Placeholder scan:** No TBD/TODO. Every code step carries literal code. Two steps intentionally instruct verification of an existing signature before use (`recordDeliveryStatus` in Task 4 Step 1; the CSS variable in Step 4) — these are guarded with the exact `grep` to run and a stated fallback, not open-ended "figure it out" instructions.

**3. Type consistency:** `ServedTracker::{record, is_new, forget}` are defined in Task 1 and used with identical signatures in Tasks 2 and 3. `attachment_progress(attachment_id, count, total)` matches the existing trait signature `(&self, [u8; 16], u32, u32)` — `record` returns `u32` and `total` is `u32` in the arm, so no casts are needed. Svelte derivations `sendComplete` / `sending` / `sentPct` are defined once in Task 4 Step 3 and used under those exact names in Step 4.

**One deviation from the spec, deliberate:** §4.1 describes the served state as a bare `HashMap<[u8;16], HashSet<u32>>` local to the actor. This plan wraps it in a `ServedTracker` struct in `chunk_transfer.rs` instead. Rationale: it makes the spec's tests 1-4 pure unit tests rather than requiring the whole peer actor to be driven, and it matches the repo's functional-core/imperative-shell standard. The data structure and lifecycle are exactly as the spec describes.
