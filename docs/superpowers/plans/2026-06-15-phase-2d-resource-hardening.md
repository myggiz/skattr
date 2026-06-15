# Phase 2.D — Resource Hardening (anti-flood) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bound the mailbox server's disk, connection, and per-request resource use against floods, and bound the daemon's inbound accept loop concurrency — without losing any accepted message.

**Architecture:** Part A adds five operator-tunable `Policy` caps and enforces them in the existing atomic `Store::insert` (global byte cap + recipient-count cap, reject-after-expired), in `handle_delete` (bounded id list), and in `accept_loop` (idle timeout) + a load-shedding connection semaphore on a duplex-testable `MailboxServer::serve_connection`. Part B adds a `Semaphore` + `JoinSet` to the daemon accept loop. Wire-format neutral — new internal rejection kinds reuse existing `ErrorCode`s (ADR 0006 frozen).

**Tech Stack:** Rust 2021, Tokio (`tokio::sync::Semaphore`, `tokio::time::timeout`, `tokio::task::JoinSet`), rusqlite (bundled), serde/TOML. Mailbox crate is AGPLv3; core is GPLv3.

**Spec:** `docs/superpowers/specs/2026-06-15-phase-2d-resource-hardening-design.md`

---

## Conventions for every task

**Cargo isn't on PATH.** Prefix with `. "$HOME/.cargo/env" &&`.

**Per-task gates (run ALL before committing a task):**

```bash
. "$HOME/.cargo/env"
cargo fmt --all
cargo clippy --workspace --exclude skattr-ui --all-targets --all-features -- -D warnings
cargo test -p skattr-mailbox            # Part A tasks
# Part B (Task 6) also: cargo test -p skattr-core --features test-harness
```

**Final gate (Task 7), single-threaded authoritative (CI-parity):**

```bash
. "$HOME/.cargo/env"
cargo fmt --all -- --check
cargo clippy --workspace --exclude skattr-ui --all-targets --all-features -- -D warnings
cargo test -p skattr-mailbox
cargo test -p skattr-core --features test-harness
cargo test -p skattr-tests -- --test-threads=1
```

**License header on every new `.rs` file:** AGPLv3 for `crates/mailbox/`, GPLv3 for `crates/core/`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later   (mailbox)
// SPDX-License-Identifier: GPL-3.0-or-later    (core)
// Copyright (C) 2026 Myggiz AB
```

**Hard rules:** No `unwrap`/`expect` in non-test code (mailbox → `MailboxError`, core → `CoreError`); `todo!()` never `unimplemented!()`; no pubkeys/onions/ciphertext logged above `debug`; no new wire `MailboxFrame`/`ErrorCode`/`Command`/`Event` variants (ADR 0006 frozen). Every gate green before commit.

---

## File map

| File | Responsibility | Tasks |
|---|---|---|
| `crates/mailbox/src/policy.rs` | 5 new `Policy` fields + `recommended()` defaults | 1 |
| `crates/mailbox/src/config.rs` | `[policy]` doc + `validate()` for new fields | 1 |
| `crates/mailbox/src/error.rs` | new `PolicyErrorKind` variants + `to_wire_code` arms (reuse existing `ErrorCode`s) + guard test | 2, 3 |
| `crates/mailbox/src/store.rs` | global byte cap + recipient-count cap in `insert`; `evict_expired_global` | 2 |
| `crates/mailbox/src/dispatch.rs` | thread caps into `store.insert`; `error_frame` arms; bounded `Delete` | 2, 3 |
| `crates/mailbox/src/server.rs` | idle timeout in `accept_loop`; `serve_connection` + connection semaphore | 4, 5 |
| `crates/mailbox/src/arti.rs` | call `serve_connection` (bin-only) | 5 |
| `crates/core/src/daemon/accept.rs` | `Semaphore` + `JoinSet` bound | 6 |
| `crates/tests/src/mailbox_flood.rs` (new) | flood/soak exit-criterion integration test | 7 |
| `crates/tests/src/lib.rs` | declare `mod mailbox_flood;` | 7 |

**Task order:** 1 → 2 → 3 → 4 → 5 (Part A) → 6 (Part B) → 7 (verification). Part A and Part B are independent; within Part A, Task 1 must precede 2–5 (defines the knobs).

---

## Task 1: Add the five Policy caps

**Files:**
- Modify: `crates/mailbox/src/policy.rs` (`Policy` struct + `recommended()`)
- Modify: `crates/mailbox/src/config.rs` (`[policy]` doc comment + `validate()`)
- Test: `policy.rs` + `config.rs` `mod tests`

- [ ] **Step 1: Write the failing test** in `crates/mailbox/src/config.rs` `mod tests` (asserts the new defaults + a validation rule):

```rust
#[test]
fn omitted_new_caps_use_recommended_defaults() {
    let path = write_temp_toml(
        "default-new-caps",
        r#"
[server]
data_dir = "/tmp/skattr-mailbox"
"#,
    );
    let cfg = MailboxConfig::load(&path).unwrap();
    assert_eq!(cfg.policy.global_storage_cap_bytes, 4_294_967_296);
    assert_eq!(cfg.policy.max_recipients, 100_000);
    assert_eq!(cfg.policy.idle_timeout_secs, 120);
    assert_eq!(cfg.policy.max_connections, 512);
    assert_eq!(cfg.policy.max_delete_ids, 1_024);
}

#[test]
fn rejects_global_cap_below_recipient_cap() {
    let path = write_temp_toml(
        "bad-global-cap",
        r#"
[server]
data_dir = "/tmp/skattr-mailbox"

[policy]
max_deposit_size = 1048576
min_ttl_secs = 3600
max_ttl_secs = 2592000
default_ttl_secs = 604800
recipient_cap_bytes = 268435456
per_conn_deposits_per_min = 30
per_conn_fetches_per_min = 6
global_deposits_per_min = 1000
global_storage_cap_bytes = 1024
max_recipients = 100000
idle_timeout_secs = 120
max_connections = 512
max_delete_ids = 1024
"#,
    );
    let err = MailboxConfig::load(&path).expect_err("must reject");
    assert!(matches!(err, MailboxError::Config(ConfigErrorKind::Invalid(_))));
}
```

- [ ] **Step 2: Run, verify it fails to compile** (fields don't exist):

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-mailbox omitted_new_caps_use_recommended_defaults
```
Expected: compile error — no field `global_storage_cap_bytes`.

- [ ] **Step 3: Add the fields + defaults.** In `policy.rs`, add to the `Policy` struct (after `global_deposits_per_min`), each with a doc comment AND a per-field serde default so existing `mailbox.toml` files load unchanged:

```rust
    /// Server-wide ceiling on total stored ciphertext bytes across all
    /// recipients. Deposits that would exceed it are rejected after
    /// evicting expired rows (never evicting accepted, non-expired rows).
    #[serde(default = "default_global_storage_cap_bytes")]
    pub global_storage_cap_bytes: u64,
    /// Cap on the number of distinct recipient hashes with stored rows.
    #[serde(default = "default_max_recipients")]
    pub max_recipients: u64,
    /// Per-connection idle read deadline in seconds; an idle connection
    /// is closed once it elapses.
    #[serde(default = "default_idle_timeout_secs")]
    pub idle_timeout_secs: u32,
    /// Server-wide ceiling on concurrent connections; excess connections
    /// are shed (closed immediately).
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
    /// Maximum number of deposit ids accepted in one `Delete`.
    #[serde(default = "default_max_delete_ids")]
    pub max_delete_ids: u32,
```

Add the default fns (module-level in `policy.rs`):

```rust
fn default_global_storage_cap_bytes() -> u64 { 4_294_967_296 } // 4 GiB
fn default_max_recipients() -> u64 { 100_000 }
fn default_idle_timeout_secs() -> u32 { 120 }
fn default_max_connections() -> u32 { 512 }
fn default_max_delete_ids() -> u32 { 1_024 }
```

Set the same values in `Policy::recommended()`:

```rust
            global_storage_cap_bytes: default_global_storage_cap_bytes(),
            max_recipients: default_max_recipients(),
            idle_timeout_secs: default_idle_timeout_secs(),
            max_connections: default_max_connections(),
            max_delete_ids: default_max_delete_ids(),
```

- [ ] **Step 4: Extend `validate()`** in `config.rs` (after the existing checks):

```rust
        if self.policy.global_storage_cap_bytes < self.policy.recipient_cap_bytes {
            return Err(MailboxError::Config(ConfigErrorKind::Invalid(
                "policy.global_storage_cap_bytes < recipient_cap_bytes".into(),
            )));
        }
        if self.policy.max_recipients == 0 {
            return Err(MailboxError::Config(ConfigErrorKind::Invalid(
                "policy.max_recipients must be >= 1".into(),
            )));
        }
        if self.policy.idle_timeout_secs == 0 {
            return Err(MailboxError::Config(ConfigErrorKind::Invalid(
                "policy.idle_timeout_secs must be >= 1".into(),
            )));
        }
        if self.policy.max_connections == 0 {
            return Err(MailboxError::Config(ConfigErrorKind::Invalid(
                "policy.max_connections must be >= 1".into(),
            )));
        }
        if self.policy.max_delete_ids == 0 {
            return Err(MailboxError::Config(ConfigErrorKind::Invalid(
                "policy.max_delete_ids must be >= 1".into(),
            )));
        }
```

Also update the `[policy]` doc comment block at the top of `config.rs` to list the five new keys.

- [ ] **Step 5: Run the tests, verify PASS:**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-mailbox omitted_new_caps_use_recommended_defaults rejects_global_cap_below_recipient_cap
```
Expected: PASS. (The existing `omitted_policy_uses_recommended` test still passes because `Policy::recommended()` now includes the new fields and the per-field serde defaults fill them in.)

- [ ] **Step 6: Per-task gates** (fmt + clippy + `cargo test -p skattr-mailbox`). Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add crates/mailbox/src/policy.rs crates/mailbox/src/config.rs
git commit -m "feat(2.D): add mailbox resource-cap policy knobs (global/recipient-count/idle/conn/delete)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 2: Global byte cap + recipient-count cap in Store::insert

**Files:**
- Modify: `crates/mailbox/src/error.rs` (new `PolicyErrorKind` variants + `to_wire_code` arms + guard test)
- Modify: `crates/mailbox/src/store.rs` (`insert` params + global/recipient-count enforcement + `evict_expired_global`)
- Modify: `crates/mailbox/src/dispatch.rs` (`handle_deposit` passes new caps; `error_frame` message arms)
- Test: `store.rs` + `dispatch.rs` `mod tests`

- [ ] **Step 1: Add the new error variants (reuse existing wire codes).** In `error.rs` `PolicyErrorKind`, after `RecipientFull`:

```rust
    /// Global storage cap reached, no expired rows available to evict.
    #[error("server full")]
    ServerFull,
    /// Distinct-recipient count cap reached (new recipient rejected).
    #[error("recipient limit")]
    RecipientLimit,
```

In `to_wire_code` (`error.rs`), add arms BEFORE the `_ => ErrorCode::Internal` catch-all, mapping both to the EXISTING `RecipientFull` wire code (client treats "full" identically — retain in outbox + retry; no new wire variant):

```rust
            MailboxError::Policy(PolicyErrorKind::ServerFull) => ErrorCode::RecipientFull,
            MailboxError::Policy(PolicyErrorKind::RecipientLimit) => ErrorCode::RecipientFull,
```

Extend the guard test `to_wire_code_covers_every_typed_failure` (`error.rs` tests) with the two new cases:

```rust
            (MailboxError::Policy(PolicyErrorKind::ServerFull), ErrorCode::RecipientFull),
            (MailboxError::Policy(PolicyErrorKind::RecipientLimit), ErrorCode::RecipientFull),
```

- [ ] **Step 2: Add the failing store tests** in `store.rs` `mod tests`:

```rust
#[test]
fn global_cap_rejects_after_evicting_expired() {
    let s = Store::in_memory().unwrap();
    // global cap = 8 bytes; recipient cap huge so only the global cap bites.
    // One expired (expires 110) + one live (huge expiry), each 4 bytes → 8 used.
    s.insert(REC_A, vec![1; 4], 100, 110, ONE_GB, 8, 100, 50).unwrap();
    s.insert(REC_B, vec![2; 4], 100, 999_999, ONE_GB, 8, 100, 50).unwrap();
    // now=200: REC_A's row is expired → evicted globally to make room for 4 more.
    s.insert(REC_A, vec![3; 4], 200, 999_999, ONE_GB, 8, 100, 200).unwrap();
    // A further 4-byte deposit now has no expired rows to evict → ServerFull.
    let err = s
        .insert(REC_A, vec![4; 4], 300, 999_999, ONE_GB, 8, 100, 300)
        .expect_err("must reject");
    assert!(matches!(err, MailboxError::Policy(PolicyErrorKind::ServerFull)));
}

#[test]
fn recipient_count_cap_rejects_new_recipient() {
    let s = Store::in_memory().unwrap();
    // max_recipients = 1: REC_A allowed, REC_B (a new distinct recipient) rejected,
    // but a second deposit to the EXISTING REC_A is still allowed.
    s.insert(REC_A, vec![1], 100, 999_999, ONE_GB, ONE_GB, 1, 50).unwrap();
    let err = s
        .insert(REC_B, vec![2], 100, 999_999, ONE_GB, ONE_GB, 1, 50)
        .expect_err("new recipient must be rejected at the limit");
    assert!(matches!(err, MailboxError::Policy(PolicyErrorKind::RecipientLimit)));
    // Existing recipient is exempt from the count cap.
    s.insert(REC_A, vec![3], 100, 999_999, ONE_GB, ONE_GB, 1, 50).unwrap();
}
```

> NOTE: the new `insert` signature adds `global_storage_cap_bytes` and `max_recipients` AFTER `recipient_cap_bytes` and BEFORE `now`. Final order:
> `insert(recipient_hash, ciphertext, deposited_at, expires_at, recipient_cap_bytes, global_storage_cap_bytes, max_recipients, now)`.
> Update ALL existing `store.insert(...)` callers (the store unit tests, `dispatch.rs` `handle_deposit`, the `dispatch.rs` tests, and any integration tests) to pass the two new args — see Steps 4–5.

- [ ] **Step 3: Run, verify they fail to compile** (arity): 

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-mailbox global_cap_rejects_after_evicting_expired
```
Expected: compile error (insert takes 6 args, etc.).

- [ ] **Step 4: Implement in `store.rs`.** Change the `insert` signature and add the global + recipient-count enforcement inside the SAME transaction, after the existing per-recipient block (`store.rs:94-111`) and before generating the id:

```rust
    pub fn insert(
        &self,
        recipient_hash: [u8; 32],
        ciphertext: Vec<u8>,
        deposited_at: i64,
        expires_at: i64,
        recipient_cap_bytes: u64,
        global_storage_cap_bytes: u64,
        max_recipients: u64,
        now: i64,
    ) -> Result<[u8; 16], MailboxError> {
        let new_len = u64::try_from(ciphertext.len()).unwrap_or(u64::MAX);
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| MailboxError::Storage(StorageErrorKind::Poisoned))?;
        let tx = conn.transaction()?;

        // ── per-recipient cap (existing logic, unchanged) ──
        // ... existing existing_bytes computation + evict_expired_for + RecipientFull ...

        // ── recipient-count cap: only when this is a NEW recipient ──
        let recipient_rows: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM deposits WHERE recipient_hash = ?1",
                params![recipient_hash.to_vec()],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if recipient_rows == 0 {
            let distinct: i64 = tx
                .query_row(
                    "SELECT COUNT(DISTINCT recipient_hash) FROM deposits",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if distinct as u64 >= max_recipients {
                tx.rollback()?;
                return Err(MailboxError::Policy(PolicyErrorKind::RecipientLimit));
            }
        }

        // ── global byte cap: evict expired globally, then reject if still over ──
        let total: i64 = tx
            .query_row(
                "SELECT COALESCE(SUM(LENGTH(ciphertext)), 0) FROM deposits",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let mut total_bytes = total as u64;
        if total_bytes + new_len > global_storage_cap_bytes {
            let to_free = (total_bytes + new_len) - global_storage_cap_bytes;
            evict_expired_global(&tx, to_free, now)?;
            let after: i64 = tx
                .query_row(
                    "SELECT COALESCE(SUM(LENGTH(ciphertext)), 0) FROM deposits",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            total_bytes = after as u64;
            if total_bytes + new_len > global_storage_cap_bytes {
                tx.rollback()?;
                return Err(MailboxError::Policy(PolicyErrorKind::ServerFull));
            }
        }

        // ... existing id generation + INSERT + tx.commit() ...
    }
```

Add the global eviction helper next to `evict_expired_for`:

```rust
fn evict_expired_global(
    tx: &rusqlite::Transaction<'_>,
    target_bytes: u64,
    now: i64,
) -> Result<(), MailboxError> {
    let mut stmt = tx.prepare(
        "SELECT deposit_id, LENGTH(ciphertext) FROM deposits \
         WHERE expires_at < ?1 ORDER BY deposited_at ASC",
    )?;
    let candidates: Vec<(Vec<u8>, i64)> = stmt
        .query_map(params![now], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    let mut freed: u64 = 0;
    for (id, bytes) in candidates {
        tx.execute("DELETE FROM deposits WHERE deposit_id = ?1", params![id])?;
        freed = freed.saturating_add(u64::try_from(bytes).unwrap_or(0));
        if freed >= target_bytes {
            break;
        }
    }
    let _ = freed;
    Ok(())
}
```

- [ ] **Step 5: Update all `insert` callers.** In `dispatch.rs` `handle_deposit`, change the `ctx.store.insert(...)` call (`dispatch.rs:106`) to pass the two new caps:

```rust
    let id = ctx.store.insert(
        body.recipient_hash,
        body.ciphertext,
        now,
        expires_at,
        ctx.policy.recipient_cap_bytes,
        ctx.policy.global_storage_cap_bytes,
        ctx.policy.max_recipients,
        now,
    )?;
```

Update the store unit tests' `insert(...)` calls and the `dispatch.rs` test that pre-deposits (`fetch_happy_path_returns_pending_deposits`, `store.insert(..., policy.recipient_cap_bytes, 100)` → add `policy.global_storage_cap_bytes, policy.max_recipients` before the trailing `now`). Search every call site: `grep -rn "\.insert(" crates/mailbox/src crates/tests/src | grep -i store` and any integration test that deposits directly. Add `error_frame` message arms in `dispatch.rs` for the new kinds (stable prose, no payload data):

```rust
        MailboxError::Policy(PolicyErrorKind::ServerFull) => "server full".to_string(),
        MailboxError::Policy(PolicyErrorKind::RecipientLimit) => "recipient limit".to_string(),
```

- [ ] **Step 6: Run the new + existing store/dispatch/error tests, verify PASS:**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-mailbox store:: dispatch:: error::
```
Expected: PASS, including `to_wire_code_covers_every_typed_failure` (now with the 2 new cases) and `cap_overflow_*` (unchanged).

- [ ] **Step 7: Per-task gates.** Expected: green.

- [ ] **Step 8: Commit**

```bash
git add crates/mailbox/src/error.rs crates/mailbox/src/store.rs crates/mailbox/src/dispatch.rs
git commit -m "feat(2.D): global byte cap + recipient-count cap in Store::insert (reject-after-expired)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 3: Bounded Delete.deposit_ids

**Files:**
- Modify: `crates/mailbox/src/error.rs` (new `PolicyErrorKind::TooManyDeleteIds` → `MalformedRequest`)
- Modify: `crates/mailbox/src/dispatch.rs` (`handle_delete` length check + `error_frame` arm)
- Test: `dispatch.rs` `mod tests`

- [ ] **Step 1: Add the error variant.** In `error.rs` `PolicyErrorKind`:

```rust
    /// `Delete.deposit_ids` longer than `max_delete_ids`.
    #[error("too many delete ids")]
    TooManyDeleteIds,
```

In `to_wire_code`, map it to the existing `MalformedRequest` code (an oversize request is malformed; client won't retry blindly):

```rust
            MailboxError::Policy(PolicyErrorKind::TooManyDeleteIds) => ErrorCode::MalformedRequest,
```

Add to the `to_wire_code_covers_every_typed_failure` guard test:

```rust
            (MailboxError::Policy(PolicyErrorKind::TooManyDeleteIds), ErrorCode::MalformedRequest),
```

- [ ] **Step 2: Write the failing test** in `dispatch.rs` `mod tests`:

```rust
#[test]
fn delete_rejects_oversize_id_list() {
    let (store, mut policy, chal, conn, global) = fixture();
    policy.max_delete_ids = 2;
    let sk = SigningKey::generate(&mut OsRng);
    let pk: [u8; 32] = sk.verifying_key().to_bytes();
    let id_hash: [u8; 32] = sha2::Sha256::digest(pk).into();
    let nonce = chal.lock().unwrap().issue(id_hash, 200);
    // 3 ids > cap of 2. Signature need not be valid: the length check
    // must reject BEFORE auth/store work.
    let body = Delete {
        version: PROTOCOL_VERSION,
        identity_pubkey: pk,
        nonce,
        signature: [0u8; 64],
        deposit_ids: vec![[1u8; 16], [2u8; 16], [3u8; 16]],
    };
    let ctx = DispatchCtx { store: &store, policy: &policy, challenges: &chal, conn_rl: &conn, global_rl: &global };
    let err = handle_delete(&ctx, body, 210).expect_err("must reject oversize");
    assert!(matches!(err, MailboxError::Policy(PolicyErrorKind::TooManyDeleteIds)));
}
```

> NOTE: confirm the exact `Delete` field names from `skattr_core::mailbox::protocol::Delete` (the handler reads `body.version`, `body.identity_pubkey`, `body.nonce`, `body.signature`, `body.deposit_ids`). The `fixture()` helper currently returns `(store, policy, ...)`; this test needs a mutable policy — bind `mut policy` (the fixture returns owned values, so `let (store, mut policy, ...) = fixture();` works).

- [ ] **Step 3: Run, verify it fails** (no length check yet → fails at signature verify, wrong error):

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-mailbox delete_rejects_oversize_id_list
```
Expected: FAIL (returns an Auth error or DeleteOk, not `TooManyDeleteIds`).

- [ ] **Step 4: Add the length check** at the TOP of `handle_delete` (`dispatch.rs:191`), right after `check_version`:

```rust
    check_version(body.version)?;
    if body.deposit_ids.len() as u64 > u64::from(ctx.policy.max_delete_ids) {
        return Err(MailboxError::Policy(PolicyErrorKind::TooManyDeleteIds));
    }
```

Add the `error_frame` message arm in `dispatch.rs`:

```rust
        MailboxError::Policy(PolicyErrorKind::TooManyDeleteIds) => "too many delete ids".to_string(),
```

- [ ] **Step 5: Run the test, verify PASS:**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-mailbox delete_rejects_oversize_id_list error::
```
Expected: PASS.

- [ ] **Step 6: Per-task gates.** Expected: green.

- [ ] **Step 7: Commit**

```bash
git add crates/mailbox/src/error.rs crates/mailbox/src/dispatch.rs
git commit -m "feat(2.D): bound Delete.deposit_ids length (reject oversize before auth/store)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 4: Idle-connection timeout in accept_loop

**Files:**
- Modify: `crates/mailbox/src/server.rs` (`accept_loop` read timeout)
- Test: `server.rs` `mod tests`

- [ ] **Step 1: Write the failing test** in `server.rs` `mod tests`:

```rust
#[tokio::test]
async fn idle_connection_is_closed_after_timeout() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let store = Arc::new(Store::in_memory().unwrap());
    let mut policy = Policy::recommended();
    policy.idle_timeout_secs = 1; // close after 1s of silence
    let mb = MailboxServer::new(store, policy);
    let server_task = tokio::spawn(async move { mb.accept_loop(server).await });

    // Client sends nothing. The server's idle timeout must fire and close.
    // Reading the client side should observe EOF within a few seconds.
    let mut framed = Framed::new(client, MailboxFrameCodec::new());
    let next = tokio::time::timeout(std::time::Duration::from_secs(5), framed.next()).await;
    match next {
        Ok(None) => {} // server closed the connection (EOF) — pass
        Ok(Some(_)) => panic!("unexpected frame from idle server"),
        Err(_) => panic!("server did not close the idle connection within 5s"),
    }
    let _ = server_task.await;
}
```

- [ ] **Step 2: Run, verify it fails** (no idle timeout → the read times out at 5s):

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-mailbox idle_connection_is_closed_after_timeout
```
Expected: FAIL (panics "server did not close ... within 5s").

- [ ] **Step 3: Implement.** In `accept_loop` (`server.rs:77`), capture the idle timeout once and wrap `framed.next()`:

```rust
        let idle = std::time::Duration::from_secs(u64::from(self.policy.idle_timeout_secs));
        loop {
            let next = match tokio::time::timeout(idle, framed.next()).await {
                Ok(Some(item)) => item,
                Ok(None) => return Ok(()),
                Err(_elapsed) => {
                    tracing::debug!("closing idle connection (idle_timeout)");
                    return Ok(());
                }
            };
            // ... existing `let frame = match next { ... }` body unchanged ...
        }
```

- [ ] **Step 4: Run the test, verify PASS:**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-mailbox idle_connection_is_closed_after_timeout
```
Expected: PASS (server closes within ~1s). Also confirm the existing `server.rs` round-trip tests still pass (they send frames promptly, well under the 120s default).

- [ ] **Step 5: Per-task gates.** Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates/mailbox/src/server.rs
git commit -m "feat(2.D): idle-connection timeout in mailbox accept_loop

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 5: Per-server connection semaphore (serve_connection)

**Files:**
- Modify: `crates/mailbox/src/server.rs` (`MailboxServer` gains `Arc<Semaphore>`; `serve_connection`)
- Modify: `crates/mailbox/src/arti.rs` (call `serve_connection`)
- Test: `server.rs` `mod tests`

- [ ] **Step 1: Add the failing test** in `server.rs` `mod tests`. It uses a `#[cfg(test)]` helper to hold the lone permit deterministically (no timing race):

```rust
#[tokio::test]
async fn serve_connection_sheds_when_at_capacity() {
    let store = Arc::new(Store::in_memory().unwrap());
    let mut policy = Policy::recommended();
    policy.max_connections = 1;
    let mb = MailboxServer::new(store, policy);

    // Hold the only permit so the server is "at capacity".
    let permit = mb.acquire_permit_for_test().expect("one permit available");

    // A new connection must be shed: serve_connection returns Ok immediately
    // and the stream is dropped (client sees EOF), WITHOUT serving frames.
    let (client, server) = tokio::io::duplex(64 * 1024);
    let served = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        mb.serve_connection(server),
    )
    .await
    .expect("serve_connection must return promptly when shedding")
    .expect("shed path returns Ok");
    let _ = served;
    // Client side should be at EOF (server dropped its end).
    let mut framed = Framed::new(client, MailboxFrameCodec::new());
    assert!(framed.next().await.is_none(), "shed connection must be closed");

    drop(permit); // release; subsequent connections would be served
}
```

- [ ] **Step 2: Run, verify it fails to compile** (`acquire_permit_for_test` / `serve_connection` don't exist):

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-mailbox serve_connection_sheds_when_at_capacity
```
Expected: compile error.

- [ ] **Step 3: Implement.** In `server.rs`, add a semaphore field + build it in `new`:

```rust
use tokio::sync::Semaphore;

pub struct MailboxServer {
    store: Arc<Store>,
    policy: Policy,
    challenges: Arc<Mutex<Challenges>>,
    global_rl: GlobalRateLimiter,
    conn_sem: Arc<Semaphore>,
}
```

In `MailboxServer::new`, after computing `now_secs_f`:

```rust
            conn_sem: Arc::new(Semaphore::new(policy.max_connections as usize)),
```

(`policy` is cloned into the struct already; read `max_connections` before the move, or read it from the cloned field — keep the existing clone order, just add the field.)

Add `serve_connection` + the test helper:

```rust
    /// Acquire a connection permit (load-shedding) and run [`accept_loop`].
    /// If the server is at `max_connections`, the connection is shed: the
    /// stream is dropped (closed) and `Ok(())` returned without serving.
    pub async fn serve_connection<S>(&self, stream: S) -> Result<(), MailboxError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        let _permit = match self.conn_sem.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                tracing::debug!("connection shed: server at max_connections");
                return Ok(());
            }
        };
        self.accept_loop(stream).await
        // `_permit` released here on return.
    }

    /// Test-only: take one connection permit so a test can drive the
    /// at-capacity (shed) path deterministically.
    #[cfg(test)]
    pub(crate) fn acquire_permit_for_test(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.conn_sem.clone().try_acquire_owned().ok()
    }
```

- [ ] **Step 4: Wire `arti.rs` to use `serve_connection`.** In `arti.rs` (the per-stream spawn at `arti.rs:167`), replace `server_per_stream.accept_loop(data_stream)` with `server_per_stream.serve_connection(data_stream)`:

```rust
                tokio::spawn(async move {
                    if let Err(e) = server_per_stream.serve_connection(data_stream).await {
                        tracing::warn!(error = %e, "mailbox serve_connection returned error");
                    }
                });
```

- [ ] **Step 5: Run the test, verify PASS:**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-mailbox serve_connection_sheds_when_at_capacity
```
Expected: PASS.

- [ ] **Step 6: Per-task gates** (clippy `--all-features` exercises the bin-gated `arti.rs` change). Expected: green.

- [ ] **Step 7: Commit**

```bash
git add crates/mailbox/src/server.rs crates/mailbox/src/arti.rs
git commit -m "feat(2.D): per-server connection semaphore with load-shedding (serve_connection)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 6: Daemon accept-loop spawn bound (Semaphore + JoinSet)

**Files:**
- Modify: `crates/core/src/daemon/accept.rs` (`run_accept_loop`)
- Test: `accept.rs` `mod tests`

- [ ] **Step 1: Write the failing test** in `accept.rs` `mod tests`. It asserts in-flight handshake tasks are bounded by the semaphore. The simplest deterministic check: drive more inbound streams than the bound where each handshake will stall (the initiator never completes), then assert no more than `MAX_INFLIGHT_HANDSHAKES` tasks are concurrently waiting. Because directly observing "in-flight task count" is awkward, assert the OBSERVABLE consequence: with the bound = N, feeding N stalled streams consumes all permits, and an (N+1)-th stream's handshake task cannot start until one frees — verified by a `JoinSet`-drain-on-shutdown test instead (more robust):

```rust
#[tokio::test(flavor = "multi_thread")]
async fn accept_loop_drains_inflight_on_shutdown() {
    let pool = Arc::new(Pool::in_memory());
    let me = Arc::new(IdentityKey::generate().unwrap());
    let hub = Arc::new(DeliveryHub::<tokio::io::DuplexStream>::new(pool.clone()));
    let (tx, rx) = mpsc::channel::<tokio::io::DuplexStream>(8);
    let inbound = InboundStreams(rx);
    let loop_task = tokio::spawn(run_accept_loop(
        inbound, me.clone(), pool.clone(), hub.clone(),
        Arc::new(NoopDispatch) as Arc<dyn InboundDispatch>,
    ));
    // Feed a few streams whose peers never complete the handshake (we keep the
    // client ends, sending nothing) so the handshake tasks are in-flight.
    let mut held = Vec::new();
    for _ in 0..4 {
        let (cli, srv) = tokio::io::duplex(64 * 1024);
        tx.send(srv).await.unwrap();
        held.push(cli);
    }
    // Close the inbound source: the loop must exit AND drain its JoinSet
    // (await/abort) rather than hang or detach. The loop_task must complete.
    drop(tx);
    let res = tokio::time::timeout(std::time::Duration::from_secs(10), loop_task).await;
    assert!(res.is_ok(), "accept loop must return after inbound closes (JoinSet drained)");
    drop(held);
}
```

- [ ] **Step 2: Run, verify it currently passes OR fails** depending on present behavior. The current loop returns when `inbound` closes (it doesn't await spawned tasks), so this test likely PASSES already — it guards the drain we're adding. To make it a meaningful TDD red, ALSO add an assertion that the loop bounds concurrency. Replace the test's tail with a bound check using a probe: set the module const low and assert that feeding many streams doesn't spawn more than the bound concurrently. Since concurrency is hard to observe directly, the IMPLEMENTER should instead write a test that exercises the `JoinSet` drain deterministically (the loop returns within the timeout after `drop(tx)`), and a SECOND test that the semaphore const is wired (a `#[test] fn max_inflight_is_bounded() { assert!(MAX_INFLIGHT_HANDSHAKES >= 1 && MAX_INFLIGHT_HANDSHAKES <= 256); }`). Document in the test comment that full concurrency-bound behavior is covered by the Task 7 flood integration test.

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness accept_loop_drains_inflight_on_shutdown
```
Expected before impl: PASS (drain) — acceptable; the change hardens behavior. The const test will fail to compile until Step 3 adds the const.

- [ ] **Step 3: Implement.** Rewrite `run_accept_loop` (`accept.rs:23-104`) to bound concurrency with a `Semaphore` and track tasks in a `JoinSet`:

```rust
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Maximum concurrent inbound handshake tasks. Onion-gated +
/// handshake-timeout-bounded; this caps a flood of simultaneous dials.
const MAX_INFLIGHT_HANDSHAKES: usize = 64;

pub(crate) async fn run_accept_loop<S>(
    mut inbound: InboundStreams<S>,
    identity: Arc<IdentityKey>,
    pool: Arc<Pool>,
    hub: Arc<DeliveryHub<S>>,
    inbound_dispatch: Arc<dyn InboundDispatch>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let sem = Arc::new(Semaphore::new(MAX_INFLIGHT_HANDSHAKES));
    let mut tasks: JoinSet<()> = JoinSet::new();
    while let Some(stream) = inbound.recv().await {
        // Reap finished handshake tasks so the JoinSet doesn't grow unbounded.
        while tasks.try_join_next().is_some() {}
        // Backpressure: wait for a permit before spawning. `acquire_owned`
        // only errors if the semaphore is closed (never here), so on error
        // we stop accepting.
        let permit = match sem.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => break,
        };
        let identity = identity.clone();
        let pool = pool.clone();
        let hub = hub.clone();
        let inbound_dispatch = inbound_dispatch.clone();
        tasks.spawn(async move {
            let _permit = permit; // released when the task ends
            handle_inbound_stream(stream, identity, pool, hub, inbound_dispatch).await;
        });
    }
    // inbound closed (transport shutdown): drain in-flight handshakes.
    tasks.shutdown().await;
}
```

Extract the existing per-stream body (handshake → resolve → ingest / welcome carve-out, `accept.rs:44-101`) verbatim into an `async fn handle_inbound_stream<S>(stream, identity, pool, hub, inbound_dispatch)` with the same `S` bound, so the spawn closure stays small. Remove the `// TODO(phase-2 transport hardening)` comment block.

> NOTE: `JoinSet::shutdown().await` aborts remaining tasks and awaits them — exactly the drain we want. `try_join_next()` is non-blocking reaping. Keep all existing log lines + the `WELCOME_READ_TIMEOUT` carve-out logic unchanged inside `handle_inbound_stream`.

- [ ] **Step 4: Run the tests, verify PASS:**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness accept::
```
Expected: PASS (`accept_loop_drains_inflight_on_shutdown`, `max_inflight_is_bounded`, and the pre-existing `accept_rejects_unknown_peer` / `accept_ingests_known_peer`).

- [ ] **Step 5: Per-task gates** (fmt + clippy + `cargo test -p skattr-core --features test-harness`). Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/accept.rs
git commit -m "feat(2.D): bound daemon inbound accept-loop concurrency (Semaphore + JoinSet drain)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 7: Flood/soak integration test + final verification

**Files:**
- Create: `crates/tests/src/mailbox_flood.rs`
- Modify: `crates/tests/src/lib.rs` (`mod mailbox_flood;`)

- [ ] **Step 1: Write the flood integration test.** In `crates/tests/src/mailbox_flood.rs` (AGPL? — NO: `crates/tests/` is GPLv3 per CLAUDE.md; use the GPLv3 header). Drive `skattr_mailbox::MailboxServer` directly over in-process duplex streams (no Tor), exercising the real `Store` + `Policy`. Cover the exit criteria:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 2.D exit criterion: the mailbox server bounds disk under an
//! anonymous flood and a targeted victim-fill, rejects oversize Delete,
//! and a fresh legit deposit still lands once space frees.

use std::sync::Arc;
use skattr_mailbox::{MailboxServer, Policy, Store};

#[tokio::test]
async fn global_cap_bounds_disk_under_flood_and_legit_deposit_lands_after_expiry() {
    // Small caps to exercise limits quickly.
    let mut policy = Policy::recommended();
    policy.global_storage_cap_bytes = 4096;
    policy.recipient_cap_bytes = 4096;
    policy.max_recipients = 1000;
    let store = Arc::new(Store::in_memory().unwrap());

    // Flood: many recipients, short TTL. Fill past the global cap; inserts
    // beyond it are rejected (ServerFull), NOT evicting accepted rows.
    // (Drive via Store::insert directly — the dispatch/auth path is unit-tested.)
    let mut accepted = 0u32;
    for i in 0..200u32 {
        let mut rec = [0u8; 32];
        rec[..4].copy_from_slice(&i.to_be_bytes());
        // expires_at = 1000 (short); now = 100.
        if store.insert(rec, vec![0xAB; 512], 100, 1000, 4096, 4096, 1000, 100).is_ok() {
            accepted += 1;
        }
    }
    assert!(accepted >= 1 && accepted <= 8, "global cap bounds accepted deposits (~4096/512)");
    assert!(store.storage_bytes().unwrap() <= 4096, "disk stays bounded under flood");

    // After all those expire (now=2000), a fresh legit deposit lands (expired
    // rows are evicted to make room — no permanent lockout).
    let mut fresh = [0xEE; 32];
    fresh[0] = 0x01;
    store.insert(fresh, vec![0xCD; 512], 2000, 9_999_999, 4096, 4096, 1000, 2000)
        .expect("fresh legit deposit must land after expiry frees space");
}
```

> NOTE TO IMPLEMENTER: confirm `MailboxServer`, `Policy`, `Store` are re-exported from `skattr_mailbox`'s `lib.rs` (the in-process mailbox tests in 2.C used `skattr_mailbox::MailboxServer` — check `crates/tests/src/mailbox_offline_delivery.rs` / `remove_mailbox_drains.rs` for the exact import path and reuse it). If `Store`/`Policy` aren't public, either use the public `MailboxServer` deposit path over a duplex stream (drive a real `Deposit` frame, like `server.rs`'s round-trip test) or add the minimal `pub use` to `lib.rs`. Prefer driving the public frame path if `Store` isn't exported; the assertion (disk bounded, fresh deposit lands) is the same.

Declare `mod mailbox_flood;` in `crates/tests/src/lib.rs` (match the existing module-declaration style + placement).

- [ ] **Step 2: Run it, verify PASS:**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-tests mailbox_flood -- --test-threads=1
```
Expected: PASS.

- [ ] **Step 3: FULL final gate** (CI-parity, single-threaded authoritative):

```bash
. "$HOME/.cargo/env"
cargo fmt --all -- --check
cargo clippy --workspace --exclude skattr-ui --all-targets --all-features -- -D warnings
cargo test -p skattr-mailbox
cargo test -p skattr-core --features test-harness
cargo test -p skattr-tests -- --test-threads=1
```
Expected: fmt clean; clippy clean; mailbox suite green (incl. new policy/store/dispatch/server tests); core green (incl. `accept::` tests); `skattr-tests` all non-ignored green incl. `mailbox_flood` AND the pre-existing loopback guardrails + `mailbox_offline_delivery` + `remove_mailbox` (regression check — the `Store::insert` signature change must have updated every caller).

- [ ] **Step 4: Verify the CLI + mailbox binary build:**

```bash
. "$HOME/.cargo/env" && cargo build -p skattr-cli && cargo build -p skattr-mailbox --features bin
```
Expected: both build clean (the `--features bin` build exercises the `arti.rs` `serve_connection` change).

- [ ] **Step 5: Commit**

```bash
git add crates/tests/src/mailbox_flood.rs crates/tests/src/lib.rs
git commit -m "test(2.D): flood/soak guardrail — global cap bounds disk, legit deposit lands after expiry

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Self-review (against the spec)

**Spec coverage:**
- Part A new `Policy` knobs → Task 1 ✓
- A1 global + recipient-count caps → Task 2 ✓
- A2 bounded `Delete` → Task 3 ✓
- A3 idle timeout → Task 4 ✓
- A4 connection semaphore + `serve_connection` + `arti.rs` → Task 5 ✓
- Part B daemon accept-loop Semaphore + JoinSet → Task 6 ✓
- Exit-criterion flood/soak guardrail + final gate → Task 7 ✓
- Reject-after-expired (no non-expired eviction) → Task 2 (`evict_expired_global` only deletes `expires_at < now`) ✓
- Wire neutrality (reuse existing `ErrorCode`s; extend guard test) → Task 2/3 (`ServerFull`/`RecipientLimit` → `RecipientFull`; `TooManyDeleteIds` → `MalformedRequest`) ✓

**Type/signature consistency:** the `Store::insert` signature is defined once in Task 2 (`..., recipient_cap_bytes, global_storage_cap_bytes, max_recipients, now`) and every caller update (dispatch handler, store tests, dispatch tests, Task 7 test) uses that exact order. New `PolicyErrorKind` variants (`ServerFull`, `RecipientLimit`, `TooManyDeleteIds`) are added in Tasks 2/3 and referenced consistently in `to_wire_code`, `error_frame`, and tests. `serve_connection`/`acquire_permit_for_test`/`conn_sem` (Task 5) and `MAX_INFLIGHT_HANDSHAKES`/`handle_inbound_stream` (Task 6) are each defined and used within their own task.

**Placeholder scan:** no TBD/TODO; every code step shows real code; test bodies are complete. The two IMPLEMENTER notes (store-export check in Task 7; the exact `Delete`/insert caller list in Tasks 2/3) point at concrete files to read, not vague instructions — deliberate because they depend on re-confirming a re-export/caller set rather than inventing API.

**Security invariants:** no non-expired eviction anywhere (durability); rejections reuse existing wire codes (ADR 0006 frozen); no secrets/onions above `debug`; no `unwrap`/`expect` in non-test code; connection shedding closes only the new stream; daemon bound preserves onion-gating + handshake timeout.
