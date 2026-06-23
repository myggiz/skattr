# Phase 3.C — Offline Attachment Transfer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver a multi-chunk file attachment to an offline peer (or one that drops mid-transfer) via the semi-trusted mailbox servers, with cross-session resume — proven end-to-end through the real `run_with_transport` assembly + a real `MailboxServer` over loopback.

**Architecture:** Chunks ride the **frozen ADR 0006 `Deposit` frame** as opaque blobs (no protocol change). The sender records per-chunk deposit state in a new `attachment_deposits` table (migration 0016) and a `chunk_sweep` task deposits them with mailbox failover/backoff. The receiver identifies fetched deposits by `sha256`-matching against its pending manifests (no wire metadata) and writes the same `ChunkStore` + `attachment_chunks` the 3.B direct lane uses, so the two lanes compose idempotently.

**Tech Stack:** Rust 2021, Tokio, `rusqlite` (bundled, WAL), `ciborium` (CBOR), `sha2`, the existing `mailbox::{client,poll}` + `delivery::{hub,mailbox_sweeper}`.

## Global Constraints

- **Spec:** `docs/superpowers/specs/2026-06-22-phase-3c-offline-attachment-transfer-design.md`. **No ADR** — reuses frozen ADR 0006 unchanged. Branch: `phase-3c-offline-attachment-transfer`.
- **Audit rule (defining):** prove behavior through real `Daemon::run` / `run_with_transport` over loopback + a real `MailboxServer` — NOT `test_exports` transport shortcuts. The guardrail tasks enforce this, modeled on `crates/tests/src/mailbox_offline_delivery.rs`.
- **Frozen — do NOT touch:** `crates/core/src/mailbox/{protocol,codec,client,auth}.rs` and `crates/mailbox/` server/`policy.rs`. Chunks reuse `MailboxClient::deposit` as-is.
- **License header on every `.rs` file:** `// SPDX-License-Identifier: GPL-3.0-or-later` then `// Copyright (C) 2026 Myggiz AB`.
- **No `unwrap()`/`expect()` in library code** (`crates/core`) — use `?`/typed errors; test modules use the existing `#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]`. Use `todo!()` never `unimplemented!()`. Secrets zeroize.
- **Constants (3.C):** `MAX_OFFLINE_ATTACHMENT_BYTES = 10 * 1024 * 1024` (10 MiB); `OFFLINE_FALLBACK_STALL_SECS = 90` (deferral before chunk deposits become due — gives the direct 3.B lane a head start); `CHUNK_SWEEP_BATCH = 30` (per-tick deposits per attachment, ≤ `per_conn_deposits_per_min`). Reuse `CHUNK_SIZE = 48 KiB`. Deposit-all + receiver dedups via `received_indices`.
- **Storage pool idioms:** `pool.with(|c| …)` read, `pool.with_mut(|c| …)` write, `pool.transaction(|tx| …)` atomic. Map rusqlite errors to `CoreError::Storage(StorageErrorKind::Other(format!(…)))`.
- **Time units:** the sweeper task uses `crate::daemon::clock::now_unix_millis()` (ms); `attachment_deposits.next_retry_at` is stored in **ms** to match (be consistent — all sweep/`due` comparisons in ms).
- **Refinements vs. the spec (deliberate, behavior-preserving):** (a) `attachment_deposits` adds a **`recipient BLOB`** column (recipient pubkey) — required for `recipient_hash` + mailbox resolution; (b) the spec's `mailbox_id` column is **dropped** — `chunk_sweep` resolves the peer's mailbox list fresh per attachment (like `run_mailbox_fallback`); (c) the offline trigger is **deferred deposit rows** (`SendFile` enqueues with `next_retry_at = now + stall`) rather than an in-memory stall timer — restart-safe and captures the recipient at send time.
- **Gate before any "done" claim (exact CI commands):** `cargo fmt --all --check`; `cargo clippy --workspace --exclude skattr-ui --all-targets --all-features -- -D warnings`; `cargo test --workspace --exclude skattr-ui --features test-harness`. Cargo isn't on PATH — prefix every cargo command with `. "$HOME/.cargo/env" &&`.
- **Reviews go to opus** (mailbox/delivery/storage = protocol/transport-adjacent; per CLAUDE.md model routing).
- **Out of scope:** 3.D UI, chunk batching per deposit (throughput), auto-fail janitor + partial-GC, per-chunk sender feedback, concurrent attachments per peer.

---

## File Structure

- **Create** `crates/core/src/storage/migrations/0016_attachment_deposits.sql` — the new table.
- **Modify** `crates/core/src/storage/migrations.rs` — register version 16.
- **Create** `AttachmentDepositRepo` in `crates/core/src/storage/attachments.rs` — sender deposit state (enqueue/due/mark/reschedule/all_deposited/delete).
- **Modify** `crates/core/src/delivery/peer.rs` — add `InboundDispatch::dispatch_attachment_chunk` (default `false`); extend the `AttachmentComplete` arm to prune deposit rows.
- **Modify** `crates/core/src/daemon/inbound.rs` — `DaemonInbound` gains `chunk_store`/`download_dir` fields + setters; implement `dispatch_attachment_chunk` (hash-match → store → reassemble on completion).
- **Modify** `crates/core/src/mailbox/poll.rs` — `poll_dispatch_once` tries `dispatch_attachment_chunk` before `dispatch_mailbox`.
- **Create** `crates/core/src/delivery/chunk_sweep.rs` — `run_chunk_sweep` (deposit due chunks with failover/backoff, prune on all-deposited).
- **Modify** `crates/core/src/delivery/mod.rs` — `pub(crate) mod chunk_sweep;`.
- **Modify** `crates/core/src/daemon/dispatch.rs` — `send_file` enqueues deferred deposit rows (size-gated).
- **Modify** `crates/core/src/daemon/state.rs` — wire `chunk_store`/`download_dir` into `DaemonInbound`; spawn the `chunk_sweep` task.
- **Create** `crates/tests/src/attachment_offline_delivery.rs` — the two guardrails; register in `crates/tests/src/lib.rs`.

---

## Task 1: Migration 0016 + `AttachmentDepositRepo`

**Files:**
- Create: `crates/core/src/storage/migrations/0016_attachment_deposits.sql`
- Modify: `crates/core/src/storage/migrations.rs`
- Modify: `crates/core/src/storage/attachments.rs`

**Interfaces:**
- Produces: `AttachmentDepositRepo<'p>` with `new(&Pool)`, and methods:
  - `enqueue_all(&self, attachment_id: &[u8;16], recipient: &[u8;32], total_chunks: u32, first_due_at_ms: i64) -> Result<()>`
  - `due(&self, now_ms: i64, limit: usize) -> Result<Vec<DepositDue>>` where `pub struct DepositDue { pub attachment_id: [u8;16], pub chunk_index: u32, pub recipient: [u8;32], pub attempts: u32 }`
  - `mark_deposited(&self, attachment_id: &[u8;16], chunk_index: u32) -> Result<()>`
  - `reschedule(&self, attachment_id: &[u8;16], chunk_index: u32, attempts: u32, next_retry_at_ms: i64) -> Result<()>`
  - `all_deposited(&self, attachment_id: &[u8;16]) -> Result<bool>`
  - `delete_for_attachment(&self, attachment_id: &[u8;16]) -> Result<()>`

- [ ] **Step 1: Write the migration SQL.** Create `crates/core/src/storage/migrations/0016_attachment_deposits.sql`:

```sql
-- Phase 3.C: sender-side per-chunk mailbox-deposit state for offline transfer.
-- Small rows, no payload (chunk bytes live in ChunkStore). next_retry_at is in
-- milliseconds, matching the sweep clock (now_unix_millis).
CREATE TABLE IF NOT EXISTS attachment_deposits (
    attachment_id BLOB NOT NULL,
    chunk_index   INTEGER NOT NULL,
    recipient     BLOB NOT NULL,            -- recipient identity pubkey (32 bytes)
    attempts      INTEGER NOT NULL DEFAULT 0,
    next_retry_at INTEGER NOT NULL,         -- ms since epoch; due when <= now
    status        TEXT NOT NULL CHECK (status IN ('pending','deposited')) DEFAULT 'pending',
    PRIMARY KEY (attachment_id, chunk_index)
);

CREATE INDEX IF NOT EXISTS idx_attachment_deposits_due
    ON attachment_deposits (status, next_retry_at);
```

- [ ] **Step 2: Register the migration.** In `crates/core/src/storage/migrations.rs`, add to the `ALL_MIGRATIONS` array after the `version: 15` entry:

```rust
    Migration {
        version: 16,
        sql: include_str!("migrations/0016_attachment_deposits.sql"),
    },
```

- [ ] **Step 3: Write failing repo tests.** Append to the `#[cfg(test)] mod tests` in `crates/core/src/storage/attachments.rs` (create the module with the test allow if absent):

```rust
    #[test]
    fn enqueue_due_mark_and_all_deposited() {
        let pool = Pool::in_memory();
        let repo = AttachmentDepositRepo::new(&pool);
        let aid = [0xAB; 16];
        let recip = [0xCD; 32];
        // Enqueue 3 chunks due at t=1000ms.
        repo.enqueue_all(&aid, &recip, 3, 1000).unwrap();
        // Not due before 1000.
        assert!(repo.due(999, 10).unwrap().is_empty());
        // Due at/after 1000.
        let due = repo.due(1000, 10).unwrap();
        assert_eq!(due.len(), 3);
        assert_eq!(due[0].recipient, recip);
        assert!(!repo.all_deposited(&aid).unwrap());
        // Mark all deposited.
        for d in &due {
            repo.mark_deposited(&aid, d.chunk_index).unwrap();
        }
        assert!(repo.all_deposited(&aid).unwrap());
        // Deposited rows are no longer due.
        assert!(repo.due(2000, 10).unwrap().is_empty());
    }

    #[test]
    fn reschedule_defers_and_bumps_attempts() {
        let pool = Pool::in_memory();
        let repo = AttachmentDepositRepo::new(&pool);
        let aid = [0x11; 16];
        repo.enqueue_all(&aid, &[0; 32], 1, 0).unwrap();
        repo.reschedule(&aid, 0, 1, 5000).unwrap();
        assert!(repo.due(4999, 10).unwrap().is_empty());
        let due = repo.due(5000, 10).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].attempts, 1);
    }

    #[test]
    fn delete_for_attachment_clears_rows() {
        let pool = Pool::in_memory();
        let repo = AttachmentDepositRepo::new(&pool);
        let aid = [0x22; 16];
        repo.enqueue_all(&aid, &[0; 32], 2, 0).unwrap();
        repo.delete_for_attachment(&aid).unwrap();
        assert!(repo.due(10_000, 10).unwrap().is_empty());
        assert!(repo.all_deposited(&aid).unwrap()); // vacuously true: no pending rows
    }
```

- [ ] **Step 4: Run tests to verify they fail.**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib storage::attachments 2>&1 | tail -20`
Expected: FAIL — `cannot find type AttachmentDepositRepo`.

- [ ] **Step 5: Implement `AttachmentDepositRepo`.** Add to `crates/core/src/storage/attachments.rs` (after `AttachmentRepo`):

```rust
/// One due chunk-deposit row (Phase 3.C offline sender state).
pub struct DepositDue {
    pub attachment_id: [u8; 16],
    pub chunk_index: u32,
    pub recipient: [u8; 32],
    pub attempts: u32,
}

/// Sender-side per-chunk mailbox-deposit state. Rows carry no payload; the
/// chunk bytes are read from the `ChunkStore` at deposit time.
pub struct AttachmentDepositRepo<'p> {
    pool: &'p Pool,
}

impl<'p> AttachmentDepositRepo<'p> {
    pub fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Enqueue all chunks `0..total_chunks` for `attachment_id`, due at
    /// `first_due_at_ms`. Idempotent on the (attachment_id, chunk_index) PK.
    pub fn enqueue_all(
        &self,
        attachment_id: &[u8; 16],
        recipient: &[u8; 32],
        total_chunks: u32,
        first_due_at_ms: i64,
    ) -> Result<()> {
        self.pool.transaction(|tx| {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO attachment_deposits \
                 (attachment_id, chunk_index, recipient, attempts, next_retry_at, status) \
                 VALUES (?1, ?2, ?3, 0, ?4, 'pending')",
            )?;
            for i in 0..total_chunks {
                stmt.execute(rusqlite::params![
                    &attachment_id[..],
                    i,
                    &recipient[..],
                    first_due_at_ms
                ])?;
            }
            Ok(())
        })
    }

    /// Pending rows whose `next_retry_at <= now_ms`, oldest first.
    pub fn due(&self, now_ms: i64, limit: usize) -> Result<Vec<DepositDue>> {
        self.pool.with(|c| {
            let mut stmt = c.prepare(
                "SELECT attachment_id, chunk_index, recipient, attempts \
                 FROM attachment_deposits \
                 WHERE status = 'pending' AND next_retry_at <= ?1 \
                 ORDER BY next_retry_at ASC LIMIT ?2",
            )?;
            let rows = stmt.query_map(rusqlite::params![now_ms, limit as i64], |r| {
                let aid: Vec<u8> = r.get(0)?;
                let recip: Vec<u8> = r.get(2)?;
                Ok((aid, r.get::<_, i64>(1)?, recip, r.get::<_, i64>(3)?))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (aid, idx, recip, attempts) = row?;
                let mut attachment_id = [0u8; 16];
                let mut recipient = [0u8; 32];
                if aid.len() == 16 && recip.len() == 32 {
                    attachment_id.copy_from_slice(&aid);
                    recipient.copy_from_slice(&recip);
                    out.push(DepositDue {
                        attachment_id,
                        chunk_index: idx as u32,
                        recipient,
                        attempts: attempts as u32,
                    });
                }
            }
            Ok(out)
        })
    }

    pub fn mark_deposited(&self, attachment_id: &[u8; 16], chunk_index: u32) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "UPDATE attachment_deposits SET status = 'deposited' \
                 WHERE attachment_id = ?1 AND chunk_index = ?2",
                rusqlite::params![&attachment_id[..], chunk_index],
            )?;
            Ok(())
        })
    }

    pub fn reschedule(
        &self,
        attachment_id: &[u8; 16],
        chunk_index: u32,
        attempts: u32,
        next_retry_at_ms: i64,
    ) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "UPDATE attachment_deposits SET attempts = ?3, next_retry_at = ?4 \
                 WHERE attachment_id = ?1 AND chunk_index = ?2",
                rusqlite::params![&attachment_id[..], chunk_index, attempts, next_retry_at_ms],
            )?;
            Ok(())
        })
    }

    /// True if no `pending` rows remain for the attachment (all deposited, or
    /// none were ever enqueued).
    pub fn all_deposited(&self, attachment_id: &[u8; 16]) -> Result<bool> {
        self.pool.with(|c| {
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM attachment_deposits \
                 WHERE attachment_id = ?1 AND status = 'pending'",
                rusqlite::params![&attachment_id[..]],
                |r| r.get(0),
            )?;
            Ok(n == 0)
        })
    }

    pub fn delete_for_attachment(&self, attachment_id: &[u8; 16]) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "DELETE FROM attachment_deposits WHERE attachment_id = ?1",
                rusqlite::params![&attachment_id[..]],
            )?;
            Ok(())
        })
    }
}
```

Wrap each `?`-bubbled rusqlite error site that the compiler complains about with `.map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("attachment_deposits: {e}"))))` to match the file's existing error mapping (the `prepare`/`execute`/`query_row` calls — follow the pattern already used by `AttachmentRepo` methods in this file).

- [ ] **Step 6: Run tests to verify they pass.**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib storage::attachments 2>&1 | tail -20`
Expected: PASS (all three new tests + existing attachment tests).

- [ ] **Step 7: Verify the migration applies on a fresh DB.**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib storage::migrations 2>&1 | tail -10`
Expected: PASS (migration runner applies 0016 cleanly; if there's a "latest version" assertion, bump it to 16).

- [ ] **Step 8: Commit.**

```bash
git add crates/core/src/storage/migrations/0016_attachment_deposits.sql crates/core/src/storage/migrations.rs crates/core/src/storage/attachments.rs
git commit -m "feat(3.C): migration 0016 + AttachmentDepositRepo"
```

---

## Task 2: Receiver — `dispatch_attachment_chunk` + poll wiring

**Files:**
- Modify: `crates/core/src/delivery/peer.rs` (trait method only)
- Modify: `crates/core/src/daemon/inbound.rs`
- Modify: `crates/core/src/mailbox/poll.rs`

**Interfaces:**
- Consumes: `AttachmentRepo` (`get`, `received_indices`, `mark_received`, `set_status`), `ChunkStore` (`put`, `get_chunk`), `StoreSource`, `reassemble`, `AttachmentManifest::from_cbor`, `chunk_transfer::{sanitize_filename, unique_download_path}`.
- Produces: `InboundDispatch::dispatch_attachment_chunk(&self, ciphertext: &[u8]) -> bool` (default `false`); `DaemonInbound::{set_chunk_store, set_download_dir}`.

- [ ] **Step 1: Add the trait method (default `false`).** In `crates/core/src/delivery/peer.rs`, inside `trait InboundDispatch`, after `dispatch_mailbox`:

```rust
    /// Try to interpret an inbound mailbox deposit as an attachment chunk
    /// (Phase 3.C offline lane): match `sha256(ciphertext)` against the chunk
    /// hashes of pending `direction='in'` manifests; on a match, store + mark
    /// received (and reassemble on completion). Returns `true` if the deposit
    /// was a (recognized) chunk and should be deleted from the mailbox server
    /// — including the dedup case where the chunk was already held. Returns
    /// `false` if it is not a chunk (caller falls through to MLS dispatch).
    /// Default `false` so non-attachment impls are unaffected.
    fn dispatch_attachment_chunk(&self, _ciphertext: &[u8]) -> bool {
        false
    }
```

- [ ] **Step 2: Build to confirm the trait still compiles** (default keeps existing impls valid).

Run: `. "$HOME/.cargo/env" && cargo build -p skattr-core 2>&1 | tail -10`
Expected: compiles.

- [ ] **Step 3: Add `chunk_store`/`download_dir` to `DaemonInbound`.** In `crates/core/src/daemon/inbound.rs`, add fields to `struct DaemonInbound`:

```rust
    pub chunk_store: std::sync::RwLock<Option<std::sync::Arc<crate::attachment::store::ChunkStore>>>,
    pub download_dir: std::sync::RwLock<Option<std::path::PathBuf>>,
```

In `DaemonInbound::new`, initialize them in the constructed `Self`:

```rust
            chunk_store: std::sync::RwLock::new(None),
            download_dir: std::sync::RwLock::new(None),
```

Add setters (near `set_identity`):

```rust
    pub(crate) fn set_chunk_store(&self, store: std::sync::Arc<crate::attachment::store::ChunkStore>) {
        if let Ok(mut g) = self.chunk_store.write() {
            *g = Some(store);
        }
    }

    pub(crate) fn set_download_dir(&self, dir: std::path::PathBuf) {
        if let Ok(mut g) = self.download_dir.write() {
            *g = Some(dir);
        }
    }
```

- [ ] **Step 4: Write the failing dispatch test.** Append to the `#[cfg(test)] mod tests` in `inbound.rs` (it already has the test allow):

```rust
    #[test]
    fn dispatch_attachment_chunk_matches_stores_and_dedups() {
        use crate::attachment::{chunker::chunk_plaintext, store::ChunkStore};
        use crate::storage::attachments::AttachmentRepo;

        let dir = tempfile::tempdir().unwrap();
        let pool = std::sync::Arc::new(crate::storage::Pool::in_memory());
        // Apply migrations (attachments tables) — Pool::in_memory runs them.
        let (events_tx, _rx) = tokio::sync::broadcast::channel(8);
        let inbound = DaemonInbound::new(pool.clone(), events_tx);
        let store = std::sync::Arc::new(ChunkStore::new(dir.path()));
        inbound.set_chunk_store(store.clone());
        inbound.set_download_dir(dir.path().join("downloads"));

        // Build a 2-chunk attachment.
        let payload = vec![7u8; crate::attachment::CHUNK_SIZE + 100];
        let (manifest, cts) = chunk_plaintext(&payload, "f.bin", "application/octet-stream").unwrap();
        // Persist the manifest as a pending 'in' attachment.
        AttachmentRepo::new(&pool)
            .insert(&manifest.attachment_id, "in", &manifest.to_cbor().unwrap(), cts.len() as i64, 0)
            .unwrap();

        // First chunk: recognized + stored.
        assert!(inbound.dispatch_attachment_chunk(&cts[0]));
        // Same chunk again: still recognized (dedup) → true (server-delete) but no double-count.
        assert!(inbound.dispatch_attachment_chunk(&cts[0]));
        assert_eq!(
            AttachmentRepo::new(&pool).received_indices(&manifest.attachment_id).unwrap(),
            vec![0]
        );
        // A non-chunk blob: not recognized.
        assert!(!inbound.dispatch_attachment_chunk(b"not a chunk"));
    }
```

- [ ] **Step 5: Run to verify it fails.**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib daemon::inbound::tests::dispatch_attachment_chunk 2>&1 | tail -15`
Expected: FAIL — method returns the default `false`.

- [ ] **Step 6: Implement `dispatch_attachment_chunk` on `DaemonInbound`.** In the `impl InboundDispatch for DaemonInbound` block add:

```rust
    fn dispatch_attachment_chunk(&self, ciphertext: &[u8]) -> bool {
        use sha2::{Digest, Sha256};
        let store = match self.chunk_store.read() {
            Ok(g) => match g.as_ref() {
                Some(s) => s.clone(),
                None => return false,
            },
            Err(_) => return false,
        };
        let hash: [u8; 32] = Sha256::digest(ciphertext).into();

        // Find which pending 'in' attachment + index this hash belongs to.
        let repo = crate::storage::attachments::AttachmentRepo::new(&self.pool);
        let pending = match repo.list_pending_in() {
            Ok(v) => v,
            Err(_) => return false,
        };
        for (attachment_id, manifest_bytes) in pending {
            let manifest =
                match crate::attachment::manifest::AttachmentManifest::from_cbor(&manifest_bytes) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
            let Some(chunk_ref) = manifest.chunks.iter().find(|c| c.ciphertext_hash == hash) else {
                continue;
            };
            let index = chunk_ref.index;
            // Dedup: already held → report handled (delete from server) without re-storing.
            let already = repo.received_indices(&attachment_id).unwrap_or_default();
            if already.contains(&index) {
                return true;
            }
            if store.put(&attachment_id, index, ciphertext).is_err() {
                return false; // could not store; leave on server for retry
            }
            if repo.mark_received(&attachment_id, index).is_err() {
                return false;
            }
            // Completion?
            let now = repo
                .received_indices(&attachment_id)
                .map(|v| v.len() as i64)
                .unwrap_or(0);
            if now >= manifest.chunks.len() as i64 {
                self.finalize_offline(&attachment_id, &manifest, &store);
            } else if let Some(d) = self.events_tx_progress() {
                // throttled progress (every 8th) — emit_progress is a thin helper
                let _ = d;
            }
            return true;
        }
        false
    }
```

Then add the helper that reassembles + emits (mirrors 3.B's `finalize_rx`, but here in `DaemonInbound`):

```rust
    fn finalize_offline(
        &self,
        attachment_id: &[u8; 16],
        manifest: &crate::attachment::manifest::AttachmentManifest,
        store: &crate::attachment::store::ChunkStore,
    ) {
        let dir = match self.download_dir.read() {
            Ok(g) => match g.as_ref() {
                Some(d) => d.clone(),
                None => return,
            },
            Err(_) => return,
        };
        let _ = std::fs::create_dir_all(&dir);
        let source = crate::attachment::store::StoreSource::new(store, *attachment_id);
        let safe = crate::delivery::chunk_transfer::sanitize_filename(&manifest.filename);
        let out_path = crate::delivery::chunk_transfer::unique_download_path(&dir, &safe);
        let repo = crate::storage::attachments::AttachmentRepo::new(&self.pool);
        match crate::attachment::reassembler::reassemble(manifest, &source, &out_path) {
            Ok(()) => {
                let _ = repo.set_status(attachment_id, "complete");
                let _ = store.remove(attachment_id);
                let _ = self.events_tx.send(Event::AttachmentReceived {
                    contact: PublicKey([0u8; 32]), // see Step 7 note
                    attachment_id: crate::daemon::hex::Hex16::from(*attachment_id),
                    filename: safe,
                    mime: manifest.mime.clone(),
                    size: manifest.total_size,
                    path: out_path.to_string_lossy().to_string(),
                });
            }
            Err(e) => tracing::warn!(err = %e, "inbound: offline reassembly failed"),
        }
    }
```

Remove the `events_tx_progress()` placeholder call from Step 6's body (it was a sketch) — emit throttled `AttachmentProgress` directly instead:

```rust
            if now >= manifest.chunks.len() as i64 {
                self.finalize_offline(&attachment_id, &manifest, &store);
            } else if now % 8 == 0 {
                let _ = self.events_tx.send(Event::AttachmentProgress {
                    attachment_id: crate::daemon::hex::Hex16::from(attachment_id),
                    received: now as u32,
                    total: manifest.chunks.len() as u32,
                });
            }
```

- [ ] **Step 7: Resolve the `contact` for `AttachmentReceived`.** The offline path has no peer in hand at chunk time. Add a `peer_for_attachment` lookup: the manifest arrived as a `Kind::File` message whose sender is recorded in the messages table; simplest is to store the sender alongside the manifest. Add a `recipient`/`peer` lookup helper OR — to avoid schema churn — resolve via the existing message row: add `AttachmentRepo::peer_for_attachment(&self, attachment_id) -> Result<Option<PublicKey>>` that joins the manifest's message. If that join isn't readily available, add a nullable `peer BLOB` column to the `attachments` table in migration 0016 (same migration) and have the 3.B `Kind::File` ingest (`inbound.rs` `dispatch_for_group`) write `from` into it; then `finalize_offline` reads it. Implement the column approach (deterministic): in `0016_attachment_deposits.sql` add `ALTER TABLE attachments ADD COLUMN peer BLOB;`, set it in the manifest-ingest `repo.insert(...)` path (extend `AttachmentRepo::insert` to take an optional `peer`), and read it in `finalize_offline` to fill `contact`. Replace the `PublicKey([0u8;32])` placeholder accordingly. (Mirror is fine for the unit test — assert on `attachment_id`/path, not `contact`.)

- [ ] **Step 8: Add `AttachmentRepo::list_pending_in`.** In `attachments.rs`:

```rust
    /// `(attachment_id, manifest_bytes)` for every `direction='in'`,
    /// `status='pending'` attachment — the offline receiver's match set.
    pub fn list_pending_in(&self) -> Result<Vec<([u8; 16], Vec<u8>)>> {
        self.pool.with(|c| {
            let mut stmt = c.prepare(
                "SELECT attachment_id, manifest FROM attachments \
                 WHERE direction = 'in' AND status = 'pending'",
            )?;
            let rows = stmt.query_map([], |r| {
                let aid: Vec<u8> = r.get(0)?;
                let m: Vec<u8> = r.get(1)?;
                Ok((aid, m))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (aid, m) = row?;
                if aid.len() == 16 {
                    let mut id = [0u8; 16];
                    id.copy_from_slice(&aid);
                    out.push((id, m));
                }
            }
            Ok(out)
        })
    }
```

- [ ] **Step 9: Wire into `poll_dispatch_once`.** In `crates/core/src/mailbox/poll.rs`, change the per-deposit loop:

```rust
    for dep in &resp.deposits {
        if inbound.dispatch_attachment_chunk(&dep.ciphertext)
            || inbound.dispatch_mailbox(&dep.ciphertext).is_some()
        {
            dispatched.push(dep.deposit_id);
        }
    }
```

- [ ] **Step 10: Run tests + build.**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib daemon::inbound 2>&1 | tail -15 && cargo build -p skattr-core 2>&1 | tail -8`
Expected: the new dispatch test PASSES; crate builds.

- [ ] **Step 11: Commit.**

```bash
git add crates/core/src/delivery/peer.rs crates/core/src/daemon/inbound.rs crates/core/src/mailbox/poll.rs crates/core/src/storage/attachments.rs crates/core/src/storage/migrations/0016_attachment_deposits.sql
git commit -m "feat(3.C): receiver dispatch_attachment_chunk (hash-match) + poll wiring"
```

---

## Task 3: `delivery::chunk_sweep` — deposit due chunks

**Files:**
- Create: `crates/core/src/delivery/chunk_sweep.rs`
- Modify: `crates/core/src/delivery/mod.rs`

**Interfaces:**
- Consumes: `MailboxFallbackShared` (`factory`, `events`), `AttachmentDepositRepo` (`due`, `mark_deposited`, `reschedule`, `all_deposited`, `delete_for_attachment`), `MailboxRepo::list_for_contact`, `ChunkStore::get_chunk`, `MailboxClient::deposit`.
- Produces: `pub(crate) async fn run_chunk_sweep(pool: &Pool, shared: &MailboxFallbackShared, chunk_store: &ChunkStore, now_ms: i64, batch: usize)`.

- [ ] **Step 1: Create the module.** Write `crates/core/src/delivery/chunk_sweep.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 3.C: deposit due attachment chunks into recipients' mailboxes.
//!
//! Sibling to `delivery::mailbox_sweeper`. Reads due `attachment_deposits`
//! rows, resolves the recipient's mailboxes (reusing the message-fallback
//! resolution), deposits the chunk ciphertext (from `ChunkStore`) via the
//! frozen `MailboxClient::deposit`, and marks the row `deposited`. On
//! all-deposited, prunes the rows + staged chunks. Mailbox unreachable / full
//! reschedules the row with backoff (the next sweep retries / failovers).

use sha2::{Digest, Sha256};

use crate::attachment::store::ChunkStore;
use crate::delivery::hub::MailboxFallbackShared;
use crate::storage::attachments::{AttachmentDepositRepo, AttachmentRepo};
use crate::storage::mailboxes::MailboxRepo;
use crate::storage::Pool;

/// Per-row backoff schedule (ms): index by min(attempts, len-1).
const BACKOFF_MS: &[i64] = &[15_000, 60_000, 300_000, 900_000];

pub(crate) async fn run_chunk_sweep(
    pool: &Pool,
    shared: &MailboxFallbackShared,
    chunk_store: &ChunkStore,
    now_ms: i64,
    batch: usize,
) {
    let deposit_repo = AttachmentDepositRepo::new(pool);
    let due = match deposit_repo.due(now_ms, batch) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(target: "skattr::delivery::chunk_sweep", error = %e, "due() failed");
            return;
        }
    };
    for row in due {
        let recipient = crate::identity::PublicKey(row.recipient);
        // Resolve the recipient's advertised mailboxes (reuse message resolution).
        let onions = match MailboxRepo::new(pool).list_for_contact(&recipient) {
            Ok(v) if !v.is_empty() => v,
            _ => {
                // No mailbox known → back off; maybe a card update lands later.
                reschedule(&deposit_repo, &row, now_ms);
                continue;
            }
        };
        let chunk = match chunk_store.get_chunk(&row.attachment_id, row.chunk_index) {
            Ok(c) => c,
            Err(_) => {
                // Staged chunk missing (pruned?) — drop the row to avoid a hot loop.
                let _ = deposit_repo.mark_deposited(&row.attachment_id, row.chunk_index);
                continue;
            }
        };
        let recipient_hash: [u8; 32] = Sha256::digest(recipient.0).into();

        // Walk mailboxes with failover; deposit on first reachable.
        let mut deposited = false;
        let n = onions.len();
        let primary = (row.chunk_index as usize) % n; // spread across mailboxes
        for offset in 0..n {
            let onion = &onions[(primary + offset) % n];
            let mut client = match shared.factory.connect(onion).await {
                Ok(c) => c,
                Err(_) => continue,
            };
            match client.deposit(recipient_hash, chunk.clone(), 0).await {
                Ok(_ok) => {
                    let _ = deposit_repo.mark_deposited(&row.attachment_id, row.chunk_index);
                    deposited = true;
                    break;
                }
                Err(_) => continue, // RecipientFull / ServerFull / protocol — try next
            }
        }
        if !deposited {
            reschedule(&deposit_repo, &row, now_ms);
            continue;
        }

        // If that was the last pending chunk, prune + clean staging + finalize 'out'.
        if deposit_repo.all_deposited(&row.attachment_id).unwrap_or(false) {
            let _ = deposit_repo.delete_for_attachment(&row.attachment_id);
            let _ = chunk_store.remove(&row.attachment_id);
            let _ = AttachmentRepo::new(pool).set_status(&row.attachment_id, "complete");
        }
    }
}

fn reschedule(
    repo: &AttachmentDepositRepo<'_>,
    row: &crate::storage::attachments::DepositDue,
    now_ms: i64,
) {
    let attempts = row.attempts.saturating_add(1);
    let idx = (attempts as usize).min(BACKOFF_MS.len()) - 1;
    let next = now_ms + BACKOFF_MS[idx];
    let _ = repo.reschedule(&row.attachment_id, row.chunk_index, attempts, next);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    // A unit test drives run_chunk_sweep with a stub MailboxConnectFactory
    // (the 2.C test pattern) + a seeded ChunkStore + a contact card listing one
    // mailbox onion, and asserts the due row becomes 'deposited'. See Step 2.
}
```

- [ ] **Step 2: Add a stub-factory unit test.** Fill the `mod tests` using the same `MailboxConnectFactory` stub shape as `peer.rs`'s `sustained_failure_triggers_mailbox_fallback` test (an inline duplex mailbox server that replies `DepositOk`). The test: seed a contact + card listing `"mb1.onion"`, stage one chunk in a temp `ChunkStore`, `enqueue_all(aid, recipient, 1, 0)`, run `run_chunk_sweep(now=1)`, assert `all_deposited(aid)` is true and the row is gone (pruned). Copy the `deposit_ok_server` + `StubFactory` from `peer.rs:957-987` verbatim into this module (they are test-local). Build the `MailboxFallbackShared` as that test does.

- [ ] **Step 3: Register the module.** In `crates/core/src/delivery/mod.rs`:

```rust
pub(crate) mod chunk_sweep;
```

- [ ] **Step 4: Build + run the unit test.**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib delivery::chunk_sweep 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/core/src/delivery/chunk_sweep.rs crates/core/src/delivery/mod.rs
git commit -m "feat(3.C): chunk_sweep deposits due chunks with failover/backoff"
```

---

## Task 4: Sender — `SendFile` enqueues deferred deposits + `AttachmentComplete` prune

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs`
- Modify: `crates/core/src/delivery/peer.rs`
- Modify: `crates/core/src/attachment/mod.rs` (add the offline-cap + stall constants)

**Interfaces:**
- Consumes: `AttachmentDepositRepo::{enqueue_all, delete_for_attachment}`.
- Produces: `MAX_OFFLINE_ATTACHMENT_BYTES`, `OFFLINE_FALLBACK_STALL_SECS` constants.

- [ ] **Step 1: Add the constants.** In `crates/core/src/attachment/mod.rs`:

```rust
/// Files at/under this size may use the offline (mailbox) lane; larger files
/// are direct-only (3.B) — if the peer is offline they wait for both online.
pub(crate) const MAX_OFFLINE_ATTACHMENT_BYTES: u64 = 10 * 1024 * 1024;
/// How long after send the offline deposit rows become due — a head start for
/// the direct 3.B lane to complete first (and be pruned).
pub(crate) const OFFLINE_FALLBACK_STALL_SECS: i64 = 90;
```

- [ ] **Step 2: Enqueue deferred deposits in `send_file`.** In `crates/core/src/daemon/dispatch.rs`, in `send_file`, AFTER the `send_message(...)` for the `Kind::File` manifest and BEFORE building `CommandResult::FileQueued`, add:

```rust
    // Phase 3.C: if the file is within the offline cap, eagerly enqueue deferred
    // chunk-deposit rows. They become due after OFFLINE_FALLBACK_STALL_SECS, by
    // which time a successful direct (3.B) transfer will have pruned them (on
    // AttachmentComplete). If the peer is/was offline, chunk_sweep deposits them.
    if manifest.total_size <= crate::attachment::MAX_OFFLINE_ATTACHMENT_BYTES {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let due_at = now_ms + crate::attachment::OFFLINE_FALLBACK_STALL_SECS * 1000;
        let dep_repo = crate::storage::attachments::AttachmentDepositRepo::new(&handle.pool);
        if let Err(e) = dep_repo.enqueue_all(&manifest.attachment_id, &contact.0, total_chunks, due_at) {
            tracing::warn!(err = %e, "send_file: enqueue offline deposits failed");
        }
    }
```

(`manifest`, `contact`, `total_chunks`, `handle.pool` are all in scope in `send_file`. `total_chunks` is the `u32` already computed; if it's an `i64` there, cast `as u32`.)

- [ ] **Step 3: Prune deposits when direct completes.** In `crates/core/src/delivery/peer.rs`, in the `Ok(Some(Frame::AttachmentComplete { attachment_id }))` arm (the sender side — it already `set_status("complete")` + `store.remove`), add a deposit-row prune so a direct success cancels any pending offline deposits:

```rust
                        // 3.C: a direct completion cancels the offline lane.
                        let _ = crate::storage::attachments::AttachmentDepositRepo::new(&pool)
                            .delete_for_attachment(&attachment_id);
```

- [ ] **Step 4: Build + run existing dispatch/peer tests.**

Run: `. "$HOME/.cargo/env" && cargo build -p skattr-core 2>&1 | tail -8 && cargo test -p skattr-core --lib delivery::peer daemon::dispatch 2>&1 | tail -8`
Expected: compiles; existing tests still pass.

- [ ] **Step 5: Commit.**

```bash
git add crates/core/src/daemon/dispatch.rs crates/core/src/delivery/peer.rs crates/core/src/attachment/mod.rs
git commit -m "feat(3.C): SendFile enqueues deferred offline deposits; direct-complete prunes"
```

---

## Task 5: Wire `chunk_store`/`download_dir` into `DaemonInbound` + spawn `chunk_sweep`

**Files:**
- Modify: `crates/core/src/daemon/state.rs`

**Interfaces:**
- Consumes: `DaemonInbound::{set_chunk_store, set_download_dir}`, `run_chunk_sweep`, `MailboxFallbackShared`, `ChunkStore::new`.

- [ ] **Step 1: Give `DaemonInbound` the ChunkStore + download dir.** In `run_with_transport` (`state.rs`), where `inbound` is built (the `DaemonInbound::new(...)` + `set_identity`/`set_group_locks` block), after those calls add:

```rust
    let chunk_store = std::sync::Arc::new(crate::attachment::store::ChunkStore::new(data_dir));
    inbound_impl.set_chunk_store(chunk_store.clone());
    inbound_impl.set_download_dir(config.resolved_download_dir());
```

(Match the exact local name — the explore shows `inbound_impl` is built then wrapped in `Arc`; call the setters on `inbound_impl` before the `Arc::new`, OR on the `Arc<DaemonInbound>` if the setters take `&self` — they do (`&self`), so calling on the `Arc` deref works too. Keep `chunk_store` in scope for the sweep spawn below.)

- [ ] **Step 2: Spawn the chunk-sweep task.** Immediately after the `mailbox_sweeper_task` spawn block (around state.rs:381), add a sibling:

```rust
    let chunk_sweep_pool = pool.clone();
    let chunk_sweep_shared = fallback_shared.clone();
    let chunk_sweep_store = chunk_store.clone();
    let chunk_sweep_task = tokio::spawn(async move {
        const SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(15);
        let mut t = tokio::time::interval(SWEEP_EVERY);
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            t.tick().await;
            let now = crate::daemon::clock::now_unix_millis();
            crate::delivery::chunk_sweep::run_chunk_sweep(
                &chunk_sweep_pool,
                &chunk_sweep_shared,
                &chunk_sweep_store,
                now,
                crate::attachment::CHUNK_SWEEP_BATCH,
            )
            .await;
        }
    });
```

- [ ] **Step 3: Abort the task on shutdown.** Wherever `mailbox_sweeper_task` is `.abort()`ed / joined in the teardown path, add `chunk_sweep_task.abort();` alongside it (grep `mailbox_sweeper_task` in `state.rs` and mirror every reference).

- [ ] **Step 4: Add `CHUNK_SWEEP_BATCH`.** In `crates/core/src/attachment/mod.rs`:

```rust
/// Max chunk deposits attempted per sweep tick (≤ per_conn_deposits_per_min).
pub(crate) const CHUNK_SWEEP_BATCH: usize = 30;
```

- [ ] **Step 5: Build the workspace.**

Run: `. "$HOME/.cargo/env" && cargo build --workspace --exclude skattr-ui 2>&1 | tail -12`
Expected: compiles. Fix any borrow/scope issues the compiler flags (e.g. `data_dir` is `&Path` — `ChunkStore::new(data_dir)` is fine; `config` is in scope per the explore).

- [ ] **Step 6: Run core + delivery tests.**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib 2>&1 | tail -6`
Expected: PASS.

- [ ] **Step 7: Commit.**

```bash
git add crates/core/src/daemon/state.rs crates/core/src/attachment/mod.rs
git commit -m "feat(3.C): wire ChunkStore/download_dir into DaemonInbound; spawn chunk_sweep"
```

---

## Task 6: Guardrail — offline multi-chunk transfer via mailbox

**Files:**
- Create: `crates/tests/src/attachment_offline_delivery.rs`
- Modify: `crates/tests/src/lib.rs`

**Interfaces:**
- Consumes: the 2.C mailbox-over-loopback harness (`mailbox_offline_delivery.rs` helpers / `crate::mailbox_harness`), `loopback_harness`, `run_loopback`/`LoopbackNet` from `skattr_core::test_exports`, the 3.B `attachment_transfer_direct.rs` payload/event patterns.

> Drive two real daemons through `run_with_transport` with a **real `MailboxServer`** wired via the `MailboxConnectFactory`, and make Bob non-directly-dialable so the offline lane is forced — exactly as `mailbox_offline_delivery.rs` does for messages, but asserting `Event::AttachmentReceived` + byte-identity.

- [ ] **Step 1: Study the two templates, then write the test.** Read `crates/tests/src/mailbox_offline_delivery.rs` IN FULL (how it spawns the in-process mailbox + wires the factory + makes a peer unreachable) and `crates/tests/src/attachment_transfer_direct.rs` (the `jpeg_with_exif` payload, `EventFilter::All` subscription, the byte-identical + EXIF-absent assertions, `wait_for_attachment`). Create `crates/tests/src/attachment_offline_delivery.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 3.C guardrail: an offline peer receives a multi-chunk file via the
//! mailbox, byte-identical, metadata stripped — through real run_with_transport
//! + a real MailboxServer.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// ... mirror the imports of mailbox_offline_delivery.rs + attachment_transfer_direct.rs ...

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn offline_attachment_via_mailbox() {
    // 1. Spawn Alice + Bob via run_loopback, each with a real mailbox wired into
    //    their MailboxConnectFactory (copy the mailbox-server spawn + factory
    //    wiring verbatim from mailbox_offline_delivery.rs).
    // 2. Establish contact (invite -> add) so the MLS group exists and Bob's
    //    ContactCard (with his mailbox onion) is in Alice's store.
    // 3. Make Bob NOT directly dialable (the same trick mailbox_offline_delivery
    //    uses — e.g. Bob's onion absent from the loopback dial registry / a
    //    factory that returns Unreachable for direct), so SendFile's direct lane
    //    cannot deliver and the offline deposits fire.
    // 4. Subscribe Bob to EventFilter::All.
    // 5. Alice writes a ~300 KiB JPEG-with-EXIF (>= 6 chunks at 48 KiB) and
    //    SendFile()s it. The manifest deposits to Bob's mailbox; chunk_sweep
    //    deposits the chunks (drive time forward / poll so the 90s stall + sweep
    //    ticks elapse — use tokio::time or a shorter test-only stall if the
    //    harness exposes one; otherwise pump the sweep by advancing time).
    // 6. Await Event::AttachmentReceived for the attachment_id on Bob.
    // 7. Assert: file at Bob's download_dir is byte-identical to the stripped
    //    source, and the EXIF APP1 marker [0xFF,0xE1] is absent.
}
```

> **Timing note (resolve during implementation):** the 90 s stall + 15 s sweep cadence are real-time. For a deterministic test, either (a) use `tokio::time::pause()` + `advance()` to fast-forward past the stall and sweep ticks (preferred — `mailbox_offline_delivery.rs` may already poll deterministically via a one-shot tick helper), or (b) add a test-only override for `OFFLINE_FALLBACK_STALL_SECS` / the sweep interval (e.g. read an env var or a `#[cfg(test)]` shorter constant) so the test runs in seconds. Pick the approach that matches how the 2.C test drives its sweep; do NOT leave the test sleeping 90+ seconds. If neither is clean, STOP and report `NEEDS_CONTEXT`.

- [ ] **Step 2: Register the module.** In `crates/tests/src/lib.rs`, alongside the other `#[cfg(test)] mod …;`:

```rust
#[cfg(test)]
mod attachment_offline_delivery;
```

- [ ] **Step 3: Run the guardrail.**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-tests offline_attachment_via_mailbox -- --nocapture 2>&1 | tail -40`
Expected: PASS (Bob receives the file via mailbox, byte-identical, EXIF gone).

- [ ] **Step 4: Commit.**

```bash
git add crates/tests/src/attachment_offline_delivery.rs crates/tests/src/lib.rs
git commit -m "test(3.C): offline multi-chunk attachment via mailbox guardrail"
```

---

## Task 7: Guardrail — cross-session resume (receiver restart)

**Files:**
- Modify: `crates/tests/src/attachment_offline_delivery.rs`

**Interfaces:**
- Consumes: the Task 6 setup; restarts the receiver daemon mid-transfer.

- [ ] **Step 1: Write the resume test.** Append `offline_attachment_cross_session_resume`. Reuse Task 6's setup (factor the spawn+contact+mailbox wiring into a shared helper in the test file). Flow: start the offline transfer; let **some but not all** chunks be fetched + persisted by Bob (assert `received_indices` is partial via a peek, or wait for an `AttachmentProgress`); then **shut down Bob's daemon and restart it** with the same `data_dir` (so `attachment_chunks` + the persisted manifest survive); on restart, Bob polls again, hash-matches the remaining chunks (still on the mailbox server because Bob only deleted the ones it stored), and completes. Assert `Event::AttachmentReceived` after restart and byte-identity.

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn offline_attachment_cross_session_resume() {
    // ... shared setup (offline lane forced) ...
    // start transfer; wait until Bob has fetched SOME chunks (partial received_indices)
    // shutdown Bob; restart Bob on the SAME data_dir
    // assert the transfer completes after restart, byte-identical
}
```

> **Determinism:** same timing approach as Task 6 (paused time or a test-only stall). To force a clean mid-transfer point, gate Bob's restart on the first `AttachmentProgress` event (or on `received_indices().len()` reaching ~half via a daemon command/peek). If a clean daemon restart on the same `data_dir` isn't supported by the harness, STOP and report `NEEDS_CONTEXT` — do not fake the restart.

- [ ] **Step 2: Run both guardrails.**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-tests attachment_offline 2>&1 | tail -20`
Expected: both PASS.

- [ ] **Step 3: Commit.**

```bash
git add crates/tests/src/attachment_offline_delivery.rs
git commit -m "test(3.C): offline cross-session resume guardrail (receiver restart)"
```

---

## Task 8: Full gate + docs

**Files:**
- Modify: `CLAUDE.md`, `PICKUP.md`

- [ ] **Step 1: Run the full CI gate.**

Run:
```bash
. "$HOME/.cargo/env" && \
cargo fmt --all --check && \
cargo clippy --workspace --exclude skattr-ui --all-targets --all-features -- -D warnings && \
cargo test --workspace --exclude skattr-ui --features test-harness 2>&1 | tail -40
```
Expected: fmt clean; zero clippy warnings; all tests PASS (the two new guardrails included; real-Tor tests remain `--ignored`). If `cargo fmt --all --check` flags anything, run `cargo fmt --all` and commit it.

- [ ] **Step 2: Update docs.** In `CLAUDE.md`: change the Phase 3 header to "3.A, 3.B, 3.C done; 3.D next", add a 3.C entry (Deposit-reuse + hash-match, `attachment_deposits` migration 0016, `chunk_sweep`, 10 MiB offline cap, the two guardrails), and bump the migrations landmark to `0016`. In `PICKUP.md`: set the next workstream to **3.D (Tauri UI)**.

- [ ] **Step 3: Commit.**

```bash
git add CLAUDE.md PICKUP.md
git commit -m "docs(3.C): mark Phase 3.C done, 3.D next; migrations through 0016"
```

- [ ] **Step 4: Finish the branch.** Use `superpowers:finishing-a-development-branch` to merge into local `master` (repo keeps `master` local-only).

---

## Self-Review

**Spec coverage** (each spec section → task):
- §2.1 Deposit-reuse + hash-match → Tasks 2 (`dispatch_attachment_chunk`), 3 (deposit). §2.2 dedicated `attachment_deposits` → Task 1. §2.3 deposit-all + receiver dedups → Task 2 (dedup) + Task 4 (enqueue_all). §2.4 10 MiB cap → Task 4. §2.5 stay-pending → no janitor added (respected by omission).
- §4 wire/identification → Tasks 2 (hash-match, leave-unmatched via the `||` fall-through in poll), 3 (Deposit). §5 sender path → Tasks 1, 3, 4. §6 receiver path → Task 2. §7 cross-session resume → durable by construction (Task 1 rows + 3.A `attachment_chunks`), proven in Task 7. §8 caps/failure/cleanup → Task 3 (failover/backoff, prune) + Task 4 (direct-complete prune). §9 guardrails → Tasks 6, 7 + unit tests in Tasks 1–3. §10 deferrals → respected (no janitor/batching/per-chunk-feedback).

**Placeholder scan:** Task 2 Step 6 ships a sketch (`events_tx_progress()`) that Step 6's tail + Step 7 correct (the throttled-progress emit + the `contact` resolution); these are flagged two-step refinements with the corrected code given, not unresolved TODOs. Task 6/7 leave the *timing mechanism* (paused-time vs test-only stall) to be chosen against the real 2.C harness, with an explicit `NEEDS_CONTEXT` escape — because the exact sweep-driving helper in `mailbox_offline_delivery.rs` wasn't quoted in full. No other gaps.

**Type consistency:** `AttachmentDepositRepo` method names + `DepositDue` fields are used identically in Tasks 1, 3, 4. `dispatch_attachment_chunk(&self, &[u8]) -> bool` matches between the trait (Task 2 Step 1), the impl (Step 6), and the poll call site (Step 9). `run_chunk_sweep(pool, shared, chunk_store, now_ms, batch)` matches between Task 3 (def) and Task 5 (spawn). `now_ms` is consistently ms (clock `now_unix_millis`, `next_retry_at` in ms). `recipient`/`recipient_hash = sha256(pubkey)` consistent between Task 1 (column), Task 3 (deposit), Task 4 (enqueue with `contact.0`).
