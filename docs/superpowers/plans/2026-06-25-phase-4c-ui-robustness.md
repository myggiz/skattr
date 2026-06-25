# Phase 4.C — UI Robustness & Data-Safety UX Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the desktop user honest, actionable feedback and protect them from silent failures and un-guarded data loss — structured errors, stream-death recovery, a first-contact waiting state, and a real backup-export gated wipe.

**Architecture:** Mostly `crates/ui` (Tauri bridge + SvelteKit) plus one additive local-IPC command and supporting core backup machinery. Four independent items, built A→B→C→D (C reuses A's typed errors; D is largest and self-contained).

**Tech Stack:** Rust (tokio, rusqlite, age), Tauri 2, SvelteKit/TypeScript, `ts-rs` generated types, Vitest.

## Global Constraints

- **No peer-facing protocol change.** The new `Command::ExportBackup` is **additive local IPC** — the command set is **append-only** (`crates/core/tests/wire_format_append_only.rs`); adding a variant requires updating that snapshot test (the `command_variant_tag` match arm + `expected_command_variant_set` list, `"export_backup"`) in the **same commit**. Do not touch the frozen ADR-0006 mailbox wire protocol or any peer frame.
- **Toolchain:** pinned 1.95.0 via the dir override (rustc 1.96 SIGSEGVs on arti). Cargo not on PATH — prefix every cargo command with `. "$HOME/.cargo/env" &&`.
- **No `unwrap()`/`expect()` in library (non-test) code** — `?`/typed errors. Test code may unwrap.
- **Frontend:** run pnpm via `npx pnpm@10` only (system pnpm 11 corrupts the lockfile; no corepack). `pnpm check` has 4 KNOWN PRE-EXISTING `ConfigPatch.download_dir` errors — your bar is zero NEW errors. CI `ui` job runs `pnpm build` + clippy + `cargo test` + `pnpm test` (vitest), not `pnpm check`/e2e.
- **D1 mitigation is waiting-state only** — no joiner auto-retry, no persisted first-contact intent, no Welcome mailbox fallback.
- **Backup is offered, not mandatory** before wipe.
- Every `.rs`/`.ts`/`.svelte` file keeps its license header. Run `cargo fmt --all --check` + `cargo clippy -p skattr-core -p skattr-ui --all-targets --all-features -- -D warnings` before committing Rust; both clean.

---

## Item A — Structured error surfacing

### Task 1: Bridge — preserve the structured `IpcError`

**Discovery that simplifies this:** `IpcClient::execute` returns `Result<CommandResult, IpcClientError>`, and `IpcClientError::Server(IpcError)` **already carries the daemon's structured wire error** (the daemon's dispatch maps `CoreError::kind()` → `IpcError::Daemon(kind)` server-side). The bug is only that `ipc_bridge.rs:37` re-flattens it. The fix is to pass `Server(e)` through.

**Files:**
- Modify: `crates/ui/src/ipc_bridge.rs` (the `ipc_request` error arm, lines 35-38)

**Interfaces:**
- Produces: `ipc_request` now returns `IpcResponse::Err(IpcError::Daemon(kind))` for typed daemon errors; `IpcError::Internal` only for transport/codec/not-running failures.

- [ ] **Step 1: Write the failing test**

Add a unit test to `crates/ui/src/ipc_bridge.rs` (`#[cfg(test)] mod tests`). It verifies the *mapping* from an `IpcClientError` to the wire `IpcResponse` (a pure helper, extracted in Step 3):
```rust
    #[test]
    fn server_error_passes_through_structured() {
        use skattr_core::daemon::error_kind::DaemonErrorKind;
        use skattr_core::daemon::ipc::IpcClientError;
        let e = IpcClientError::Server(IpcError::Daemon(DaemonErrorKind::InviteExpired));
        match map_client_err(e) {
            IpcError::Daemon(DaemonErrorKind::InviteExpired) => {}
            other => panic!("expected structured Daemon(InviteExpired), got {other:?}"),
        }
    }

    #[test]
    fn transport_error_becomes_internal() {
        use skattr_core::daemon::ipc::IpcClientError;
        let e = IpcClientError::DaemonNotRunning;
        assert!(matches!(map_client_err(e), IpcError::Internal(_)));
    }
```
> Verify the import path of `IpcClientError` first: `grep -rn "pub use.*IpcClientError\|pub enum IpcClientError" crates/core/src/daemon/ipc/`. It is exported from `skattr_core::daemon::ipc` (sibling of `IpcClient`). Adjust the `use` if the path differs.

- [ ] **Step 2: Run test, verify it fails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-ui ipc_bridge 2>&1 | tail -15`
Expected: FAIL — `map_client_err` not found.

- [ ] **Step 3: Extract the mapping + use it**

In `crates/ui/src/ipc_bridge.rs`, add the helper and call it. Replace the `match client.execute(cmd).await { … }` (lines 35-38) with:
```rust
    match client.execute(cmd).await {
        Ok(result) => Ok(IpcResponse::Ok(result)),
        Err(e) => Ok(IpcResponse::Err(map_client_err(e))),
    }
}

/// Preserve the daemon's structured `IpcError` instead of flattening it.
/// `IpcClientError::Server` already carries the typed wire error the daemon
/// produced (via `CoreError::kind()`); only genuine transport/codec failures
/// become `Internal`.
fn map_client_err(e: skattr_core::daemon::ipc::IpcClientError) -> IpcError {
    use skattr_core::daemon::ipc::IpcClientError;
    match e {
        IpcClientError::Server(ipc_err) => ipc_err,
        other => {
            let msg: String = format!("{other}").chars().take(256).collect();
            IpcError::Internal(msg)
        }
    }
}
```
(Keep the existing `use` lines; add nothing the test doesn't need.)

- [ ] **Step 4: Run test + clippy/fmt**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-ui ipc_bridge 2>&1 | tail -10 && cargo fmt -p skattr-ui --check && cargo clippy -p skattr-ui --all-targets --all-features -- -D warnings 2>&1 | tail -4`
Expected: both tests PASS; fmt + clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src/ipc_bridge.rs
git commit -m "fix(4.C): preserve structured IpcError through the Tauri bridge"
```

---

### Task 2: Frontend `errorMessage` helper + AddContactDialog wiring

**Files:**
- Create: `crates/ui/src-svelte/src/lib/ipc/errors.ts`
- Create: `crates/ui/src-svelte/src/lib/ipc/errors.test.ts`
- Modify: `crates/ui/src-svelte/src/lib/components/AddContactDialog.svelte`

**Interfaces:**
- Consumes: the `ts-rs` types `IpcError` / `DaemonErrorKind` (`src/lib/ipc/types/`). Shapes: `IpcError = { "err": "daemon", "data": DaemonErrorKind } | { "err": "internal", "data": string } | { "err": "auth_denied" } | { "err": "codec", "data": string } | { "err": "frame_too_large", "data": {got,max} } | { "err": "unknown_command" } | { "err": "vault_not_ready" }`. `DaemonErrorKind = { "kind": "invite_expired" } | { "kind": "invite_consumed" } | { "kind": "invite_signature_invalid" } | { "kind": "contact_not_found" } | { "kind": "contact_ambiguous", "data": {matches} } | { "kind": "delivery_timeout" } | { "kind": "tor_not_ready" } | { "kind": "group_corrupt" } | { "kind": "storage_error" } | { "kind": "search_syntax" } | { "kind": "invalid_argument", "data": {message} } | { "kind": "unauthorized" }`.
- Produces: `errorMessage(err: IpcError): string`.

- [ ] **Step 1: Write the failing test**

Create `crates/ui/src-svelte/src/lib/ipc/errors.test.ts`:
```typescript
import { describe, it, expect } from "vitest";
import { errorMessage } from "./errors";
import type { IpcError } from "./types";

describe("errorMessage", () => {
  it("maps invite_expired", () => {
    const e: IpcError = { err: "daemon", data: { kind: "invite_expired" } };
    expect(errorMessage(e)).toMatch(/expired/i);
  });
  it("maps invite_consumed", () => {
    const e: IpcError = { err: "daemon", data: { kind: "invite_consumed" } };
    expect(errorMessage(e)).toMatch(/already been used/i);
  });
  it("maps invite_signature_invalid", () => {
    const e: IpcError = { err: "daemon", data: { kind: "invite_signature_invalid" } };
    expect(errorMessage(e)).toMatch(/verif/i);
  });
  it("maps delivery_timeout to an offline hint", () => {
    const e: IpcError = { err: "daemon", data: { kind: "delivery_timeout" } };
    expect(errorMessage(e)).toMatch(/offline|reach/i);
  });
  it("uses the message for invalid_argument", () => {
    const e: IpcError = { err: "daemon", data: { kind: "invalid_argument", data: { message: "bad path" } } };
    expect(errorMessage(e)).toBe("bad path");
  });
  it("falls back generically for internal", () => {
    const e: IpcError = { err: "internal", data: "boom" };
    expect(errorMessage(e)).toMatch(/something went wrong/i);
  });
});
```

- [ ] **Step 2: Run test, verify it fails**

Run: `cd crates/ui/src-svelte && npx pnpm@10 test src/lib/ipc/errors.test.ts 2>&1 | tail -15`
Expected: FAIL — `./errors` not found.

- [ ] **Step 3: Implement `errors.ts`**

Create `crates/ui/src-svelte/src/lib/ipc/errors.ts` (keep the license header comment used by sibling `.ts` files — copy the header from `client.ts`):
```typescript
import type { IpcError, DaemonErrorKind } from "./types";

function daemonKindMessage(k: DaemonErrorKind): string {
  switch (k.kind) {
    case "invite_expired": return "This invite link has expired.";
    case "invite_consumed": return "This invite link has already been used.";
    case "invite_signature_invalid":
      return "This invite couldn't be verified — it may be corrupted or tampered with.";
    case "contact_not_found": return "Contact not found.";
    case "contact_ambiguous": return "That name matches more than one contact.";
    case "delivery_timeout": return "Couldn't reach your contact — they may be offline.";
    case "tor_not_ready": return "Still connecting to Tor — try again in a moment.";
    case "group_corrupt": return "This conversation's secure state is damaged.";
    case "storage_error": return "A local storage error occurred.";
    case "search_syntax": return "That search query isn't valid.";
    case "invalid_argument": return k.data.message;
    case "unauthorized": return "Not authorized.";
    default: return "Something went wrong.";
  }
}

/** Human-readable message for a structured IPC error. Never surfaces a raw
 *  internal string as the primary message. */
export function errorMessage(err: IpcError): string {
  switch (err.err) {
    case "daemon": return daemonKindMessage(err.data);
    case "vault_not_ready": return "The app is still starting — try again in a moment.";
    case "auth_denied": return "Not authorized.";
    case "unknown_command": return "This action isn't available.";
    case "frame_too_large": return "That request was too large.";
    case "codec":
    case "internal":
    default: return "Something went wrong.";
  }
}
```
> If `switch (k.kind)` errors on exhaustiveness because the generated discriminant differs, read `src/lib/ipc/types/DaemonErrorKind.ts` and match the exact string literals.

- [ ] **Step 4: Run test, verify it passes**

Run: `cd crates/ui/src-svelte && npx pnpm@10 test src/lib/ipc/errors.test.ts 2>&1 | tail -10`
Expected: 6 tests PASS.

- [ ] **Step 5: Wire AddContactDialog to use it**

Read `crates/ui/src-svelte/src/lib/components/AddContactDialog.svelte`. In `submit()` (around lines 32-52), the success path checks `resp.resp !== "ok"` and sets `error = "Failed to add contact."`. Replace that opaque collapse: when `resp.resp === "err"`, set `error = errorMessage(resp.data)`. Import `errorMessage` from `$lib/ipc/errors`. Keep the `catch (e)` branch (transport-layer throw) as a generic fallback. Leave the rest of the dialog unchanged. (The first-contact-specific wording for an *offline* peer is added in Task 7 — here, `delivery_timeout` already yields "Couldn't reach your contact — they may be offline.")

- [ ] **Step 6: Run the full vitest suite + commit**

Run: `cd crates/ui/src-svelte && npx pnpm@10 test 2>&1 | tail -8`
Expected: all pass (existing + new errors.test.ts).
```bash
cd /home/myggiz/development/skattr
git add crates/ui/src-svelte/src/lib/ipc/errors.ts crates/ui/src-svelte/src/lib/ipc/errors.test.ts crates/ui/src-svelte/src/lib/components/AddContactDialog.svelte
git commit -m "feat(4.C): render structured daemon errors in the UI (add-contact + helper)"
```

---

## Item B — Stream-death signal

### Task 3: Relay emits `ipc:stream-closed` on death

**Files:**
- Modify: `crates/ui/src/events.rs`

**Interfaces:**
- Produces: when the event relay's `next_event()` returns `Err`, the daemon-side relay emits a global Tauri event `ipc:stream-closed` (payload: a short reason `String`) before the task exits. `ipc_subscribe` gains an `app: tauri::AppHandle` parameter.

- [ ] **Step 1: Modify the relay loop**

In `crates/ui/src/events.rs`: add `app: tauri::AppHandle` as a parameter to `ipc_subscribe` (Tauri injects it), add `use tauri::Emitter;` at the top, and replace the `tokio::spawn` body (lines 38-45) with:
```rust
    tokio::spawn(async move {
        loop {
            match client.next_event().await {
                Ok(ev) => {
                    if channel.send(ev).is_err() {
                        // Receiver gone — Svelte unmounted the consumer. Normal.
                        break;
                    }
                }
                Err(e) => {
                    // Stream died (daemon gone / socket closed). Signal the
                    // frontend so it can re-subscribe instead of freezing.
                    let _ = app.emit("ipc:stream-closed", format!("{e}"));
                    break;
                }
            }
        }
    });
```

- [ ] **Step 2: Verify the command still registers**

`ipc_subscribe` is registered in the Tauri `generate_handler!` list (find it: `grep -rn "ipc_subscribe" crates/ui/src/main.rs`). Adding an injected `AppHandle` parameter needs no registration change (Tauri injects it like `State`). Confirm with a build.

Run: `. "$HOME/.cargo/env" && cargo build -p skattr-ui 2>&1 | tail -6 && cargo clippy -p skattr-ui --all-targets --all-features -- -D warnings 2>&1 | tail -4`
Expected: clean build (the `app: AppHandle` injection compiles) + clippy clean.

> No Rust unit test here — emitting a Tauri event needs an app runtime. The behavior is exercised by the frontend store test (Task 4) + manual/e2e. Note this in your report.

- [ ] **Step 3: fmt + commit**

Run: `. "$HOME/.cargo/env" && cargo fmt -p skattr-ui --check`
```bash
git add crates/ui/src/events.rs
git commit -m "feat(4.C): emit ipc:stream-closed on event-relay death instead of silent break"
```

---

### Task 4: Connection store + re-subscribe + banner

**Files:**
- Create: `crates/ui/src-svelte/src/lib/stores/connection.ts`
- Create: `crates/ui/src-svelte/src/lib/stores/connection.test.ts`
- Modify: the app shell that owns the event subscription (find it: `grep -rln "ipcEvents\|ipc_subscribe\|subscribe(" crates/ui/src-svelte/src/routes/ crates/ui/src-svelte/src/lib/ipc/tauri.ts`) — wire the banner + re-subscribe trigger.

**Interfaces:**
- Consumes: the existing subscribe entry point in `src/lib/ipc/tauri.ts` (`invoke("ipc_subscribe", { filter, channel })`, line 21) and the Tauri event API (`@tauri-apps/api/event` `listen`).
- Produces: `connection` writable store `{ state: 'live' | 'reconnecting' | 'dead' }`; a `startConnectionWatch(resubscribe: () => Promise<void>)` that listens for `ipc:stream-closed` and drives reconnect with backoff.

- [ ] **Step 1: Write the failing store test**

Create `crates/ui/src-svelte/src/lib/stores/connection.test.ts`:
```typescript
import { describe, it, expect, vi } from "vitest";
import { get } from "svelte/store";
import { connection, handleStreamClosed, __resetForTest } from "./connection";

describe("connection store", () => {
  it("starts live", () => {
    __resetForTest();
    expect(get(connection).state).toBe("live");
  });

  it("goes reconnecting then live on a successful re-subscribe", async () => {
    __resetForTest();
    const resubscribe = vi.fn().mockResolvedValue(undefined);
    await handleStreamClosed(resubscribe);
    expect(resubscribe).toHaveBeenCalledOnce();
    expect(get(connection).state).toBe("live");
  });

  it("goes dead after exhausting retries", async () => {
    __resetForTest();
    const resubscribe = vi.fn().mockRejectedValue(new Error("down"));
    await handleStreamClosed(resubscribe, { maxAttempts: 3, baseDelayMs: 0 });
    expect(resubscribe).toHaveBeenCalledTimes(3);
    expect(get(connection).state).toBe("dead");
  });
});
```

- [ ] **Step 2: Run test, verify it fails**

Run: `cd crates/ui/src-svelte && npx pnpm@10 test src/lib/stores/connection.test.ts 2>&1 | tail -15`
Expected: FAIL — `./connection` not found.

- [ ] **Step 3: Implement `connection.ts`**

Create `crates/ui/src-svelte/src/lib/stores/connection.ts` (copy the license header from a sibling store like `delivery.ts`):
```typescript
import { writable } from "svelte/store";

export type ConnState = "live" | "reconnecting" | "dead";
export const connection = writable<{ state: ConnState }>({ state: "live" });

/** Test seam: reset to live. */
export function __resetForTest(): void {
  connection.set({ state: "live" });
}

interface RetryOpts { maxAttempts?: number; baseDelayMs?: number; }

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** On stream death: flip to reconnecting and retry `resubscribe` with bounded
 *  exponential backoff; → live on success, → dead after maxAttempts. */
export async function handleStreamClosed(
  resubscribe: () => Promise<void>,
  opts: RetryOpts = {},
): Promise<void> {
  const maxAttempts = opts.maxAttempts ?? 6;
  const baseDelayMs = opts.baseDelayMs ?? 500;
  connection.set({ state: "reconnecting" });
  for (let attempt = 0; attempt < maxAttempts; attempt++) {
    try {
      await resubscribe();
      connection.set({ state: "live" });
      return;
    } catch {
      const delay = Math.min(baseDelayMs * 2 ** attempt, 8000);
      if (delay > 0) await sleep(delay);
    }
  }
  connection.set({ state: "dead" });
}
```

- [ ] **Step 4: Run test, verify it passes**

Run: `cd crates/ui/src-svelte && npx pnpm@10 test src/lib/stores/connection.test.ts 2>&1 | tail -10`
Expected: 3 tests PASS.

- [ ] **Step 5: Wire the listener + banner into the app shell**

Read the current subscription wiring (the file from `grep` above — likely `src/lib/ipc/tauri.ts` exposes the subscribe, and a route/layout calls it on mount). In the app shell (`+layout.svelte` or `+page.svelte` where the subscription is established):
- On mount, `import { listen } from "@tauri-apps/api/event"` and register `listen<string>("ipc:stream-closed", () => handleStreamClosed(reSubscribe))`, where `reSubscribe` re-invokes the same `ipc_subscribe` call the shell already uses (extract it into a named function if inline).
- Add a small banner element bound to `$connection.state`: hidden when `live`; "Reconnecting to the app service…" when `reconnecting`; "Disconnected — retry" with a button calling `handleStreamClosed(reSubscribe)` when `dead`.
- Unlisten on unmount.
Keep the banner minimal and consistent with existing UI styling (reuse the toast/banner pattern if one exists).

- [ ] **Step 6: Full vitest + build + commit**

Run: `cd crates/ui/src-svelte && npx pnpm@10 test 2>&1 | tail -6 && npx pnpm@10 build 2>&1 | tail -3`
Expected: all vitest pass; build clean.
```bash
cd /home/myggiz/development/skattr
git add crates/ui/src-svelte/src/lib/stores/connection.ts crates/ui/src-svelte/src/lib/stores/connection.test.ts crates/ui/src-svelte/src/routes/
git commit -m "feat(4.C): connection store + reconnect banner self-heals a dead event stream"
```

---

## Item C — D1 first-contact waiting state

### Task 5: Typed dial-failure error + first-contact message

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (`add_contact`, the dial at line ~344-349)
- Modify: `crates/ui/src-svelte/src/lib/components/AddContactDialog.svelte`

**Interfaces:**
- Produces: `add_contact` surfaces a recognizable `IpcError::Daemon(DaemonErrorKind::DeliveryTimeout)` when the inviter can't be reached (instead of an opaque `Internal`).

- [ ] **Step 1: Map the dial failure to a recognizable kind**

In `crates/core/src/daemon/dispatch.rs`, `add_contact` dials the inviter at lines 344-349 with `.map_err(map_err)?`. A dial failure to an offline inviter currently flows through generic `map_err`, which yields `Internal` if the underlying transport error has no `kind()`. Make first-contact failures recognizable: replace the `.map_err(map_err)?` on the `connect_and_ingest_at` call with an explicit mapping to `DeliveryTimeout` (the closest existing kind — "couldn't reach"):
```rust
    let h_transport = handle
        .hub
        .connect_and_ingest_at(inviter, &inviter_onion)
        .await
        .map_err(|e| {
            // First contact requires reaching the inviter now; a dial failure
            // (offline / Tor flaky) is surfaced as DeliveryTimeout so the UI can
            // show the "both must be online" guidance. Preserve a more specific
            // kind if the underlying error already has one.
            match e.kind() {
                Some(k) => IpcError::Daemon(k),
                None => IpcError::Daemon(crate::daemon::error_kind::DaemonErrorKind::DeliveryTimeout),
            }
        })?;
```
> Verify `e` (the `connect_and_ingest_at` error) has a `.kind()` method (it is a `CoreError`; `CoreError::kind()` exists at `error.rs:133`). If the error type differs, adapt — the intent is: a recognizable `DeliveryTimeout` for the unreachable case. Confirm `map_err` and `IpcError` are already in scope in this function (they are — used elsewhere in `add_contact`).

- [ ] **Step 2: Run the existing add_contact / dispatch tests**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness add_contact 2>&1 | tail -15; cargo test -p skattr-core --lib dispatch 2>&1 | tail -8`
Expected: existing tests still pass (the change only refines the error kind on the dial-failure path). If a test asserted the old opaque error, update it to expect `DeliveryTimeout` and note it.

- [ ] **Step 3: First-contact wording in AddContactDialog**

In `AddContactDialog.svelte` `submit()`, when `resp.resp === "err"` and the kind is `delivery_timeout`, show the first-contact-specific message and **keep the dialog open** so the user can re-submit (the invite was not consumed on dial failure). `tor_not_ready` is local Tor startup state, not peer reachability — it falls through to `errorMessage(d)` which already returns "Still connecting to Tor — try again in a moment.":
```typescript
      if (resp.resp === "err") {
        const d = resp.data;
        if (d.err === "daemon" && d.data.kind === "delivery_timeout") {
          error = "Couldn't reach your contact. First contact needs both of you online at the same time — try again when they're back online.";
        } else {
          error = errorMessage(d);
        }
        return; // keep the dialog open for retry
      }
```
(Adjust to the dialog's actual control flow read in Task 2.)

- [ ] **Step 4: Verify + commit**

Run: `. "$HOME/.cargo/env" && cargo fmt -p skattr-core --check && cargo clippy -p skattr-core --all-targets --all-features -- -D warnings 2>&1 | tail -4 && cd crates/ui/src-svelte && npx pnpm@10 test 2>&1 | tail -6`
Expected: clippy/fmt clean; vitest green.
```bash
cd /home/myggiz/development/skattr
git add crates/core/src/daemon/dispatch.rs crates/ui/src-svelte/src/lib/components/AddContactDialog.svelte
git commit -m "feat(4.C): first-contact offline shows 'both must be online' with clean retry"
```

---

### Task 6: "Connecting…" badge for a pending first contact

**Files:**
- Modify: the contact list/header component(s) that render a `ContactSummary` (find: `grep -rln "group_state\|PendingJoin\|ContactSummary" crates/ui/src-svelte/src`)
- Test: a vitest unit for the badge logic (a small `pending`-derivation helper, or a component test if the codebase has them for contacts)

**Interfaces:**
- Consumes: `ContactSummary.group_state` (TS: a `MlsGroupStateLabel | null`; value `"pending_join"` while the Welcome is in flight) + the existing contact store updated by `ContactUpdated` events.
- Produces: a "Connecting…" indicator shown while `group_state === "pending_join"`, cleared automatically when the contact becomes active.

- [ ] **Step 1: Confirm the state shape**

Read the generated `ContactSummary` / `MlsGroupStateLabel` TS types (`grep -rn "group_state\|MlsGroupStateLabel" crates/ui/src-svelte/src/lib/ipc/types/`). Confirm the exact discriminant string for the pending state (expected `"pending_join"`). Use the exact literal.

- [ ] **Step 2: Write the failing test**

Add a vitest for a pure helper `isConnecting(c: ContactSummary): boolean` (create it next to the contact store, e.g. `src/lib/contacts.ts` or extend an existing helper module):
```typescript
import { describe, it, expect } from "vitest";
import { isConnecting } from "./contacts";

describe("isConnecting", () => {
  it("true while group_state is pending_join", () => {
    expect(isConnecting({ group_state: "pending_join" } as any)).toBe(true);
  });
  it("false when active", () => {
    expect(isConnecting({ group_state: "active" } as any)).toBe(false);
  });
  it("false when null", () => {
    expect(isConnecting({ group_state: null } as any)).toBe(false);
  });
});
```

- [ ] **Step 3: Run test, verify it fails; then implement**

Run: `cd crates/ui/src-svelte && npx pnpm@10 test contacts 2>&1 | tail -12` → FAIL.
Implement `isConnecting`:
```typescript
import type { ContactSummary } from "$lib/ipc/types";
/** A contact whose first-contact Welcome is still in flight. */
export function isConnecting(c: ContactSummary): boolean {
  return c.group_state === "pending_join";
}
```
(Use the exact literal confirmed in Step 1.)

- [ ] **Step 4: Render the badge**

In the contact list/header component, when `isConnecting(contact)` show a small muted "Connecting…" badge next to the name. It clears automatically because the contact store re-fetches / updates on `ContactUpdated` + delivery events (existing behavior — no new event wiring). Match existing badge styling.

- [ ] **Step 5: Verify + commit**

Run: `cd crates/ui/src-svelte && npx pnpm@10 test 2>&1 | tail -6 && npx pnpm@10 build 2>&1 | tail -3`
```bash
cd /home/myggiz/development/skattr
git add crates/ui/src-svelte/src/lib/
git commit -m "feat(4.C): show a 'Connecting…' badge while first contact completes"
```

---

## Item D — Backup export + wipe gate + completion signal

### Task 7: Derive a backup key at daemon boot

The runtime seed is consumed at boot (`derive_storage_seed` consumes the `IdentityKey`), so the backup key (`HKDF(seed,"skattr-backup-v1")`) isn't available later. Derive it at boot — where `seed` is still in scope — and retain only the derived key (not the root seed) on the handle.

**Files:**
- Modify: `crates/core/src/daemon/state.rs` (where `let seed = derive_storage_seed(...)` is, ~line 126; and the `DaemonHandle` construction)
- Modify: the `DaemonHandle` struct definition (find: `grep -rn "pub struct DaemonHandle" crates/core/src/`)

**Interfaces:**
- Produces: `DaemonHandle.backup_key: Zeroizing<[u8; 32]>` — the precomputed `HKDF(seed, "skattr-backup-v1")` output, used by `ExportBackup`.

- [ ] **Step 1: Add the field**

In the `DaemonHandle<S>` struct, add:
```rust
    /// Precomputed backup key (`HKDF(seed, "skattr-backup-v1")`). Derived at
    /// boot because the root seed is consumed during identity setup and not
    /// retained. Used by `Command::ExportBackup`.
    pub(crate) backup_key: zeroize::Zeroizing<[u8; 32]>,
```

- [ ] **Step 2: Derive it at boot**

In `state.rs`, where `let seed = derive_storage_seed(identity_for_seed)?;` is (~line 126), add immediately after (while `seed` is in scope, before it drops):
```rust
    let backup_key = crate::identity::derive::hkdf_expand::<32>(
        seed.as_bytes(),
        crate::identity::derive::INFO_BACKUP_V1,
    )?;
```
Then thread `backup_key` into every `DaemonHandle { … }` construction in `state.rs` (there may be more than one — `grep -n "DaemonHandle {" crates/core/src/daemon/state.rs`; the test-helper constructor in `dispatch.rs`/`test_exports` may also need a value — use `zeroize::Zeroizing::new([0u8; 32])` there).

- [ ] **Step 3: Build + existing tests**

Run: `. "$HOME/.cargo/env" && cargo build -p skattr-core 2>&1 | tail -6 && cargo test -p skattr-core --features test-harness --lib 2>&1 | grep -E "test result|error\[" | tail -5`
Expected: builds (all `DaemonHandle {…}` sites updated); existing tests pass.

- [ ] **Step 4: fmt/clippy + commit**

Run: `. "$HOME/.cargo/env" && cargo fmt -p skattr-core --check && cargo clippy -p skattr-core --all-targets --all-features -- -D warnings 2>&1 | tail -4`
```bash
git add crates/core/src/daemon/state.rs crates/core/src/daemon/handle.rs
git commit -m "feat(4.C): derive a backup key at daemon boot for live export"
```
(`handle.rs` = wherever `DaemonHandle` is defined; adjust the path.)

---

### Task 8: `Pool::snapshot_encrypted` + `export_backup_from_parts`

**Files:**
- Modify: `crates/core/src/storage/pool.rs` (new `snapshot_encrypted` method)
- Modify: `crates/core/src/storage/backup.rs` (new `export_backup_from_parts`)

**Interfaces:**
- Produces:
  - `Pool::snapshot_encrypted(&self, out_age: &Path) -> Result<()>` — checkpoints the WAL, writes a consistent plaintext snapshot via `VACUUM INTO`, encrypts it to `out_age` under the pool's storage passphrase, and removes the temp plaintext.
  - `storage::backup::export_backup_from_parts(data_dir: &Path, db_age: &Path, out_path: &Path, backup_key: &[u8; 32]) -> Result<()>` — bundles `identity.vault` + `hs.key.age` (from `data_dir`) + the DB `.age` at `db_age` (named `skattr.sqlite.age` in the archive), age-encrypted under `backup_key`.

- [ ] **Step 1: Write the failing test for the snapshot**

Add to `crates/core/src/storage/pool.rs` tests:
```rust
    #[test]
    fn snapshot_encrypted_produces_decryptable_db() {
        let dir = tempfile::tempdir().unwrap();
        let seed = crate::identity::Seed::generate().unwrap();
        let pool = Pool::open(dir.path(), &seed).unwrap();
        // write a row so the snapshot has content
        pool.with_mut(|c| { c.execute("CREATE TABLE t(x)", []).unwrap(); c.execute("INSERT INTO t VALUES (42)", []).unwrap(); Ok(()) }).unwrap();
        let out = dir.path().join("snap.age");
        pool.snapshot_encrypted(&out).unwrap();
        assert!(out.exists(), "snapshot .age written");
        // temp plaintext snapshot must be gone
        assert!(!dir.path().join("skattr.sqlite.snapshot").exists());
    }
```
> Mirror the existing pool-test idiom (`Pool::open(dir, &seed)` / `with_mut`); adjust the temp-snapshot filename to whatever Step 3 uses.

- [ ] **Step 2: Run test, verify it fails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib snapshot_encrypted 2>&1 | tail -12` → FAIL (method missing).

- [ ] **Step 3: Implement `snapshot_encrypted`**

In `pool.rs`, add to `impl Pool` (reuse the existing private `encrypt_db(working, encrypted, passphrase)` helper that `close()` uses at line 202, and the `self.passphrase` / `self.working_path` fields):
```rust
    /// Write a consistent, encrypted snapshot of the live DB to `out_age`
    /// without closing the pool. Checkpoints the WAL, `VACUUM INTO` a temp
    /// plaintext copy, encrypts it under the storage passphrase, and removes
    /// the temp. Used by `Command::ExportBackup`.
    pub(crate) fn snapshot_encrypted(&self, out_age: &std::path::Path) -> Result<()> {
        let snap = self.working_path.with_extension("snapshot");
        {
            let guard = self.conn.lock().map_err(|_| {
                CoreError::Storage(StorageErrorKind::Other("pool mutex poisoned".into()))
            })?;
            let conn = guard.as_ref().ok_or_else(|| {
                CoreError::Storage(StorageErrorKind::Other("pool closed".into()))
            })?;
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("checkpoint: {e}"))))?;
            // VACUUM INTO writes a consistent snapshot even with an active WAL.
            conn.execute("VACUUM INTO ?1", [snap.to_string_lossy().as_ref()])
                .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("vacuum into: {e}"))))?;
        }
        let res = encrypt_db(&snap, out_age, &self.passphrase);
        let _ = std::fs::remove_file(&snap); // always clean up the plaintext temp
        res
    }
```
> Verify the exact names: `encrypt_db` (free fn in pool.rs), `self.conn` (`Mutex<Option<Connection>>`), `self.passphrase`, `self.working_path`. Adjust if they differ. `VACUUM INTO` is supported by the bundled rusqlite/sqlite — confirm the test passes.

- [ ] **Step 4: Implement `export_backup_from_parts`**

In `backup.rs`, add (factoring the tar-gz + age-encrypt body out of `export_backup`, or duplicating it minimally — DRY preferred):
```rust
/// Bundle `identity.vault` + `hs.key.age` (from `data_dir`) and the DB snapshot
/// `.age` at `db_age` (stored as `skattr.sqlite.age`) into an age-encrypted
/// archive at `out_path`, encrypted under `backup_key`. Used by live export.
pub(crate) fn export_backup_from_parts(
    data_dir: &Path,
    db_age: &Path,
    out_path: &Path,
    backup_key: &[u8; 32],
) -> Result<()> {
    let members: [(&Path, &str); 3] = [
        (&data_dir.join("identity.vault"), "identity.vault"),
        (&data_dir.join("hs.key.age"), "hs.key.age"),
        (db_age, "skattr.sqlite.age"),
    ];
    // (build gzipped tar over `members`, then age-encrypt the tarball under
    //  `Zeroizing::new(hex::encode(backup_key))`, then atomic write — identical
    //  to the existing `export_backup` body, parameterized over `members` and the
    //  key. Extract a shared `fn write_encrypted_archive(members, out, passphrase)`
    //  and call it from BOTH export_backup and here to stay DRY.)
}
```
Refactor `export_backup` to derive its `backup_key` from `seed` then call the shared `write_encrypted_archive`, so both paths share the tar/age/atomic-write logic. Show the full shared helper in your implementation (no ellipsis in the actual code — the block above is the plan's shape, not the final code).

- [ ] **Step 5: Run snapshot test + existing backup tests**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib "snapshot_encrypted" 2>&1 | tail -8 && cargo test -p skattr-core --features test-harness backup 2>&1 | tail -12`
Expected: snapshot test passes; existing backup/restore round-trip tests still pass.

- [ ] **Step 6: fmt/clippy + commit**

```bash
git add crates/core/src/storage/pool.rs crates/core/src/storage/backup.rs
git commit -m "feat(4.C): Pool::snapshot_encrypted + export_backup_from_parts for live backup"
```

---

### Task 9: `Command::ExportBackup` + handler + append-only test

**Files:**
- Modify: `crates/core/src/daemon/commands.rs` (`Command::ExportBackup` variant)
- Modify: `crates/core/src/daemon/dispatch.rs` (handler + dispatch arm)
- Modify: `crates/core/tests/wire_format_append_only.rs` (match arm + snapshot list)

**Interfaces:**
- Produces: `Command::ExportBackup { dest_path: String }` → `CommandResult::Ok` / typed error. Wire tag `"export_backup"`.

- [ ] **Step 1: Add the Command variant + append-only snapshot (RED via compile)**

In `commands.rs`, add to `Command`:
```rust
    /// Export an encrypted backup archive of the live state to `dest_path`.
    ExportBackup {
        /// Absolute destination path for the archive.
        dest_path: String,
    },
```
In `crates/core/tests/wire_format_append_only.rs`: add the match arm `Command::ExportBackup { .. } => "export_backup",` to `command_variant_tag`, and `"export_backup",` to the `expected_command_variant_set()` vec.

- [ ] **Step 2: Run the freeze test, verify it now passes with the update**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --test wire_format_append_only 2>&1 | tail -8`
Expected: PASS (the exhaustive match compiles and the snapshot includes `export_backup`). If you forget either edit, it fails — that's the guard working.

- [ ] **Step 3: Implement the handler + dispatch arm**

In `dispatch.rs`, add the handler and wire it in the `dispatch` match:
```rust
async fn export_backup_cmd<S>(
    handle: Arc<DaemonHandle<S>>,
    dest_path: String,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let data_dir = handle.config.read().await.data_dir.clone();
    let dest = std::path::PathBuf::from(&dest_path);
    let db_age = data_dir.join("skattr.sqlite.backup.age"); // temp, distinct from the live .age
    handle.pool.snapshot_encrypted(&db_age).map_err(map_err)?;
    let res = crate::storage::backup::export_backup_from_parts(
        &data_dir, &db_age, &dest, &handle.backup_key,
    );
    let _ = std::fs::remove_file(&db_age); // clean up the temp DB .age
    res.map_err(map_err)?;
    Ok(CommandResult::Ok)
}
```
Add the dispatch arm: `Command::ExportBackup { dest_path } => export_backup_cmd(handle, dest_path).await,` (match the surrounding dispatch style — `Arc::clone(&handle)` etc. as neighbors do).

- [ ] **Step 4: Test the handler end-to-end on an in-process daemon**

Add an integration test (in `crates/tests/` or a `--features test-harness` core test, mirroring an existing daemon-handler test) that runs `ExportBackup { dest_path }` against a live in-process daemon and asserts the archive file exists and the temp `skattr.sqlite.backup.age` was removed. If a full daemon harness is heavy, a focused test that calls `export_backup_cmd` with a real `DaemonHandle` (test constructor) is acceptable — note the choice.

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness export_backup 2>&1 | tail -12`

- [ ] **Step 5: fmt/clippy + commit**

```bash
git add crates/core/src/daemon/commands.rs crates/core/src/daemon/dispatch.rs crates/core/tests/wire_format_append_only.rs
git commit -m "feat(4.C): Command::ExportBackup — live encrypted backup over IPC"
```

---

### Task 10: Settings "Export backup…" action

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/stores/config.ts` (or new `backup.ts`) — `exportBackup(path)` IPC wrapper
- Modify: `crates/ui/src-svelte/src/routes/settings/advanced/+page.svelte` — the action + save-dialog

**Interfaces:**
- Consumes: `Command::ExportBackup { dest_path }`; the Tauri dialog plugin save-picker (`@tauri-apps/plugin-dialog` `save`, already a dep from 3.D).
- Produces: `exportBackup(destPath: string): Promise<void>`; a Settings button that picks a path and calls it.

- [ ] **Step 1: IPC wrapper + test**

In `config.ts` (next to `wipeAllData`, lines 172-177), add:
```typescript
export async function exportBackup(destPath: string): Promise<void> {
  const resp = await ipcClient.request({ cmd: "export_backup", dest_path: destPath });
  if (resp.resp !== "ok") {
    throw new Error(errorMessage(resp.data));
  }
}
```
(Import `errorMessage` from `$lib/ipc/errors`.) Add a vitest mirroring the existing `wipeAllData` test (mock `ipcClient.request`): resolves on `{resp:"ok"}`, throws the mapped message on `{resp:"err"}`.

- [ ] **Step 2: Run wrapper test (RED→GREEN)**

Run: `cd crates/ui/src-svelte && npx pnpm@10 test config 2>&1 | tail -10`

- [ ] **Step 3: Settings action**

In `routes/settings/advanced/+page.svelte`, add an "Export backup…" button (above the danger zone). On click: `import { save } from "@tauri-apps/plugin-dialog"`, `const path = await save({ defaultPath: "skattr-backup.age", filters: [{ name: "Skattr backup", extensions: ["age"] }] })`; if `path`, `await exportBackup(path)` then `toast.show("Backup saved.")`; on throw, `toast.show(<message>)`. Confirm `dialog`'s `save` is allowed in `capabilities/default.json` (the `dialog` plugin is already wired for 3.D's open-picker; add the `dialog:allow-save` permission if missing — `grep -rn "dialog" crates/ui/capabilities/`).

- [ ] **Step 4: Build + vitest + commit**

Run: `cd crates/ui/src-svelte && npx pnpm@10 test 2>&1 | tail -6 && npx pnpm@10 build 2>&1 | tail -3`
```bash
cd /home/myggiz/development/skattr
git add crates/ui/src-svelte/src/lib/stores/ crates/ui/src-svelte/src/routes/settings/advanced/+page.svelte crates/ui/capabilities/
git commit -m "feat(4.C): Settings export-backup action (GUI backup, no CLI needed)"
```

---

### Task 11: Three-way wipe gate

**Files:**
- Modify: `crates/ui/src-svelte/src/routes/settings/advanced/+page.svelte`

**Interfaces:**
- Consumes: `exportBackup` (Task 10), the existing two-stage `ConfirmDialog` flow (lines 181-209), and `wipeAllData`.

- [ ] **Step 1: Make the first confirm three-way**

Replace the first confirm (the `confirmStage1` dialog) so it offers three actions: **Export backup first** / **Continue without backup** / **Cancel**. "Export backup first" runs the Task-10 picker+`exportBackup`; on success it advances to `confirmStage2` (the existing final "Are you absolutely sure?" dialog). "Continue without backup" advances to `confirmStage2` directly. "Cancel" closes. The existing `ConfirmDialog` may only support confirm/cancel — if so, either extend it with an optional third action or use a small bespoke dialog here. Keep the second-stage confirm (`wipe()`) unchanged.

- [ ] **Step 2: Verify the flow (e2e if present)**

Run the e2e if the suite covers settings (`cd crates/ui/src-svelte && npx pnpm@10 test:e2e 2>&1 | tail -15`) — otherwise verify the build + a vitest component test for the three-way branch if the repo has component tests for this route. Confirm "Continue without backup" still reaches the wipe and "Export backup first" exports then reaches the final confirm.

- [ ] **Step 3: Commit**

```bash
git add crates/ui/src-svelte/src/routes/settings/advanced/+page.svelte
git commit -m "feat(4.C): gate the wipe behind a one-click backup offer"
```

---

### Task 12: Deterministic wipe completion signal (T3-3)

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (`wipe_all_data`)
- Modify: `crates/core/src/daemon/ipc/server/mod.rs` (run teardown after the flushed `Bye`)

**Interfaces:** none external; replaces the blind `sleep(150ms)` with a flush-ordered teardown.

- [ ] **Step 1: Understand the ordering**

The connection loop (`server/mod.rs`) writes the `Ok` response (line 127) then the terminal `Bye` (line 165) before `handle_connection` returns and the stream drops. The current `wipe_all_data` (`dispatch.rs:1679-1714`) spawns a detached task that `sleep(150ms)`s hoping that flush happened. The deterministic fix: run the teardown **after** the loop has written the `Bye`, not on a timer.

- [ ] **Step 2: Implement the flush-ordered teardown**

Approach (contained to these two files — do not redesign IPC): have `wipe_all_data` signal intent via a shared one-shot/flag on the handle rather than self-spawning the timer, and have the connection loop, after writing the terminal `Bye`, perform the wipe+exit if that flag is set.
- In `wipe_all_data`: set a `wipe_requested` signal (e.g. `handle.wipe_requested.store(true, Ordering::SeqCst)` where `wipe_requested: Arc<AtomicBool>` is added to the handle, or a `OnceCell`/`Notify`) and stash `data_dir`. Return `Ok(CommandResult::Ok)` immediately. Do **not** spawn the sleep.
- In `server/mod.rs`, after the terminal `let _ = write_frame(&mut stream, &IpcResponse::Bye).await;` (line 165), check the signal; if set, run the teardown synchronously: `drop` the executor/handle, `tokio::fs::remove_dir_all(&data_dir).await` (log on error), `std::process::exit(0)`. This runs only after the `Bye` is flushed — no race, no sleep.
> If threading the signal cleanly to the connection loop proves to reach beyond a contained change (the executor trait doesn't expose handle state to `handle_connection`), fall back to: in `wipe_all_data`, replace the *first* `sleep(150ms)` with awaiting an explicit flush — or, minimally, keep a single bounded `sleep` but document T3-3 as partially-addressed in your report. Prefer the deterministic path; only fall back with a written reason.

- [ ] **Step 3: Test**

Add/update a test: a `WipeAllData` against an in-process daemon returns `Ok` and the data dir is removed (the existing wipe test, if any — `grep -rn "wipe_all_data\|WipeAllData" crates/core/ crates/tests/`). Assert no fixed-sleep dependency in the new path (the test shouldn't need to `sleep` to observe the reply). If `process::exit` makes a direct test infeasible (it kills the test process), test the ordering at the unit level (the signal is set; the teardown function removes the dir) without calling `exit`, and note the limitation.

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness wipe 2>&1 | tail -12`

- [ ] **Step 4: fmt/clippy + commit**

```bash
git add crates/core/src/daemon/dispatch.rs crates/core/src/daemon/ipc/server/mod.rs crates/core/src/daemon/handle.rs
git commit -m "fix(4.C): deterministic wipe teardown after the flushed Bye (drop the 150ms sleep)"
```

---

### Task 13: Full gate (verification-before-completion)

**Files:** none (verification only).

- [ ] **Step 1: Rust gate**

Run:
```bash
. "$HOME/.cargo/env" && \
cargo fmt --all --check && \
cargo clippy -p skattr-core -p skattr-ui --all-targets --all-features -- -D warnings && \
cargo test -p skattr-core --features test-harness 2>&1 | tail -20 && \
cargo test -p skattr-ui 2>&1 | tail -10
```
Expected: fmt clean; no clippy warnings; core lib+integration green (incl. `wire_format_append_only`, the snapshot + export-backup tests); ui Rust green.

- [ ] **Step 2: Integration guardrails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-tests 2>&1 | tail -20`
Expected: all pass (no regression — 4.C adds a command + UI; the attachment/messaging guardrails are unaffected).

- [ ] **Step 3: Frontend gate**

Run:
```bash
cd crates/ui/src-svelte && \
CI=true npx pnpm@10 install --frozen-lockfile && \
npx pnpm@10 test 2>&1 | tail -8 && \
npx pnpm@10 build 2>&1 | tail -3
```
Expected: install clean; full vitest suite PASS (incl. errors / connection / config wrapper tests); build clean. (`pnpm check`'s 4 pre-existing errors are unrelated; confirm none in 4.C files.)

- [ ] **Step 4: e2e (local)**

Run: `cd crates/ui/src-svelte && npx pnpm@10 test:e2e 2>&1 | tail -20`
Expected: all e2e specs pass (4.C is additive UI; existing flows unaffected).

- [ ] **Step 5: Branch status + handoff**

Run: `git status && git log --oneline master..HEAD`
Expected: clean tree; the item A→D commits listed. Hand to the whole-branch review → PR → CodeRabbit babysit → merge. Do NOT merge before the whole-branch review.

---

## Self-Review (completed against the spec)

**Spec coverage:** Item A → Tasks 1 (bridge), 2 (frontend helper + add-contact). Item B → Tasks 3 (relay event), 4 (store + banner). Item C → Tasks 5 (typed dial error + first-contact msg), 6 (Connecting badge); waiting-state-only (no auto-retry) honored — no retry/persistence task exists. Item D → Tasks 7 (boot backup key), 8 (pool snapshot + parts), 9 (Command + handler + append-only test), 10 (Settings export), 11 (wipe gate), 12 (completion signal); 13 = gate. Audit T2-7 (A+B), T2-9 (D7-11), T3-3 (D12). The T1-2 note (already fixed in 2.B) is why D snapshots the live DB rather than relying on the at-rest `.age`.

**Placeholder scan:** The one intentional shape-only block is Task 8 Step 4's `export_backup_from_parts` body, explicitly flagged "no ellipsis in the actual code — extract a shared `write_encrypted_archive` from the existing verbatim `export_backup`" (whose full body is quoted from the repo in the plan's research). The "verify the exact name/path" notes (IpcClientError import, DaemonErrorKind literals, DaemonHandle file, encrypt_db/conn/passphrase field names, the subscribe-wiring shell file) are concrete read-then-confirm anchors with the expected value named — not deferrals. Task 12 carries an explicit guard-railed fallback with a written-reason requirement.

**Type consistency:** `errorMessage(IpcError)` defined in Task 2, reused in Tasks 5 (dialog) + 10 (wrapper). `handleStreamClosed(resubscribe, opts)` defined in Task 4. `backup_key: Zeroizing<[u8;32]>` defined Task 7, consumed Task 9. `Pool::snapshot_encrypted(out_age)` + `export_backup_from_parts(data_dir, db_age, out_path, backup_key)` defined Task 8, called Task 9. `Command::ExportBackup { dest_path }` / wire tag `"export_backup"` consistent across Tasks 9 (Rust) + 10 (TS `cmd: "export_backup", dest_path`). `isConnecting`/`"pending_join"` confined to Task 6.
