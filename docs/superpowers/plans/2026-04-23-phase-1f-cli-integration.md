# Phase 1.F CLI Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire every stateful CLI command (`invite`, `add`, `contacts`, `send`, `tail`, `chat`) to a real persistent daemon over a CBOR-framed Unix-domain-socket IPC with `SO_PEERCRED` auth; expand `Daemon::run` to own `Pool` + `DeliveryHub` + IPC server; add migration 0005 binding each contact to its MLS group; replace the stdin passphrase prompt with `/dev/tty`; ship two integration tests (mocked + real-Arti) exercising the full invite → add → send → receive flow.

**Architecture:** Persistent daemon process owns `TorRuntime`, `Arc<Pool>`, `Arc<DeliveryHub>`, `IdentityKey`, `broadcast::Sender<Event>` for its entire lifetime. A new `daemon::ipc::` submodule hosts the wire types, length-prefixed CBOR codec, server (per-connection task, `0600` socket, `SO_PEERCRED`/`getpeereid` check, `Subscribe` state machine that coexists with further `Execute`s on one connection), and `IpcClient`. A new `daemon::dispatch::execute_command` plus `daemon::handle::DaemonHandle` form the command-handler layer. `init`/`restore`/`backup` stay in-process and never touch the socket.

**Tech Stack:** Rust 2021, `tokio` (`UnixListener`, `UnixStream::peer_cred()`, `broadcast`, `oneshot`, `select!`), `ciborium` (CBOR), `serde`, `toml`, `directories`/`dirs` (already in deps), `async-trait` (new, core), `libc` (new, core — `getuid()`), `rpassword 7` (new, CLI), `qrcode 0.14` (new, CLI), `serde_json` (new, CLI), `rusqlite` 0.38 + `age` (already in deps), `openmls` + `snow` + `arti-client` (already integrated via 1.B/1.C/0.C). Dev-deps: `tempfile`.

**Spec:** `docs/superpowers/specs/2026-04-23-phase-1f-cli-integration-design.md` (commit `534ac07`).

---

## Pre-flight

### PF1: Create the 1.F worktree

- [ ] **Step 1: Create the worktree from the current master and step into it**

From the main checkout:
```bash
cd /home/myggiz/development/skattr
. "$HOME/.cargo/env"
git fetch origin
git worktree add -b phase-1f-cli-integration ../skattr-phase-1f-cli-integration master
cd ../skattr-phase-1f-cli-integration
git status --short
git log --oneline -3
```
Expected: empty status; HEAD at `534ac07 spec: lock Phase 1.F CLI integration design`; branch `phase-1f-cli-integration`.

- [ ] **Step 2: Confirm the worktree path**

Run:
```bash
pwd
```
Expected: `/home/myggiz/development/skattr-phase-1f-cli-integration`. Every subsequent command in this plan runs from this directory unless noted otherwise.

### PF2: Establish the baseline green build

- [ ] **Step 1: Format, clippy, test**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```
Expected: all three green. If any fails, stop and surface — the baseline must be clean before touching 1.F code.

---

## Task 1: Migration 0005 — add `group_id` column to `contacts`

**Files:**
- Create: `crates/core/src/storage/migrations/0005_contact_group_link.sql`
- Modify: `crates/core/src/storage/migrations.rs:25-42` (extend `ALL_MIGRATIONS`; add test module entry)

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block at the bottom of `crates/core/src/storage/migrations.rs`:

```rust
#[test]
fn migration_0005_adds_group_id_column_and_index() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    apply(&mut conn).unwrap();

    let cols: Vec<String> = conn
        .prepare("PRAGMA table_info('contacts')")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .map(std::result::Result::unwrap)
        .collect();
    assert!(
        cols.iter().any(|c| c == "group_id"),
        "migration 0005 must add contacts.group_id; got {cols:?}"
    );

    let idx_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type = 'index' AND name = 'idx_contacts_group_id'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(idx_count, 1, "idx_contacts_group_id must exist");

    let v: u32 = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, 5);
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-core --lib storage::migrations::tests::migration_0005 -- --nocapture
```
Expected: FAIL (`contacts.group_id` missing).

- [ ] **Step 3: Create the migration SQL**

Write `crates/core/src/storage/migrations/0005_contact_group_link.sql`:

```sql
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz B.V.
--
-- Skattr storage schema, version 5.
-- Bind every contact row to its 2-member MLS group so SendMessage /
-- RecentMessages can cross "contact pubkey -> MLS group" in one join.
-- Phase 1 has no production users; the empty default is cosmetic and
-- AddContact is the only insert path, so every row will carry a real
-- group_id.

INSERT OR IGNORE INTO schema_version (version) VALUES (5);

ALTER TABLE contacts ADD COLUMN group_id BLOB NOT NULL DEFAULT X'';
CREATE INDEX IF NOT EXISTS idx_contacts_group_id ON contacts(group_id);
```

- [ ] **Step 4: Register the migration**

In `crates/core/src/storage/migrations.rs`, extend the `ALL_MIGRATIONS` slice:

```rust
const ALL_MIGRATIONS: &[Migration] = &[
    Migration { version: 1, sql: include_str!("migrations/0001_init.sql") },
    Migration { version: 2, sql: include_str!("migrations/0002_key_packages.sql") },
    Migration { version: 3, sql: include_str!("migrations/0003_contact_cards.sql") },
    Migration { version: 4, sql: include_str!("migrations/0004_outbox_message_id.sql") },
    Migration { version: 5, sql: include_str!("migrations/0005_contact_group_link.sql") },
];
```

- [ ] **Step 5: Run test to verify it passes**

```bash
cargo test -p skattr-core --lib storage::migrations::tests::migration_0005 -- --nocapture
cargo test -p skattr-core --lib storage::migrations
```
Expected: both green.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/storage/migrations.rs crates/core/src/storage/migrations/0005_contact_group_link.sql
git commit -m "$(cat <<'EOF'
storage: migration 0005 binds contacts.group_id

Every contact row now carries its 2-member MLS group id, indexed for
SendMessage / RecentMessages lookups. Phase 1.F dispatch handlers
populate it atomically inside AddContact.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: `ContactRepo` group-id accessors and pubkey-prefix lookup

**Files:**
- Modify: `crates/core/src/storage/contacts.rs` (add `set_group_id`, `get_group_id`, `lookup_by_prefix`; extend tests)

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block at the bottom of `crates/core/src/storage/contacts.rs`:

```rust
#[test]
fn set_get_group_id_round_trip() {
    let pool = Pool::in_memory();
    let repo = ContactRepo::new(&pool);
    let alice = sample_contact(30);
    repo.upsert(&alice).unwrap();

    // Default group_id is empty (per migration 0005).
    assert_eq!(repo.get_group_id(&alice.identity).unwrap(), Some(Vec::new()));

    let gid = vec![0xAAu8; 32];
    repo.set_group_id(&alice.identity, &gid).unwrap();
    assert_eq!(repo.get_group_id(&alice.identity).unwrap(), Some(gid));
}

#[test]
fn get_group_id_missing_contact_returns_none() {
    let pool = Pool::in_memory();
    let repo = ContactRepo::new(&pool);
    assert!(repo.get_group_id(&PublicKey([0x99; 32])).unwrap().is_none());
}

#[test]
fn lookup_by_prefix_returns_unique_match() {
    let pool = Pool::in_memory();
    let repo = ContactRepo::new(&pool);
    let alice = sample_contact(0x10);
    let bob = sample_contact(0x20);
    repo.upsert(&alice).unwrap();
    repo.upsert(&bob).unwrap();

    // "10" is a unique 1-byte hex prefix for alice.
    let hit = repo.lookup_by_prefix("10").unwrap();
    assert_eq!(hit, vec![alice.identity]);
}

#[test]
fn lookup_by_prefix_returns_all_ambiguous_matches() {
    let pool = Pool::in_memory();
    let repo = ContactRepo::new(&pool);
    // Two contacts both starting with 0xAB.
    let mut a = sample_contact(0xAB);
    a.identity = PublicKey([0xAB; 32]);
    let mut b = sample_contact(0xAB);
    b.identity = PublicKey({
        let mut bytes = [0xAB; 32];
        bytes[1] = 0xCD;
        bytes
    });
    repo.upsert(&a).unwrap();
    repo.upsert(&b).unwrap();

    let mut hits = repo.lookup_by_prefix("ab").unwrap();
    hits.sort();
    let mut want = vec![a.identity, b.identity];
    want.sort();
    assert_eq!(hits, want);
}

#[test]
fn lookup_by_prefix_empty_returns_empty() {
    let pool = Pool::in_memory();
    let repo = ContactRepo::new(&pool);
    repo.upsert(&sample_contact(1)).unwrap();
    // Unknown prefix -> empty result (NOT error).
    assert!(repo.lookup_by_prefix("ff00").unwrap().is_empty());
}

#[test]
fn lookup_by_prefix_rejects_non_hex() {
    let pool = Pool::in_memory();
    let repo = ContactRepo::new(&pool);
    let err = repo.lookup_by_prefix("zz").expect_err("non-hex prefix");
    assert!(matches!(err, CoreError::Contact(ref s) if s.contains("hex")), "got {err:?}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p skattr-core --lib storage::contacts
```
Expected: six new tests FAIL with "no method named `set_group_id`" / `get_group_id` / `lookup_by_prefix`.

- [ ] **Step 3: Implement the new methods**

Add to `impl<'p> ContactRepo<'p>` in `crates/core/src/storage/contacts.rs` (anywhere inside the impl, grouped with other public methods):

```rust
    /// Set the MLS group id for `identity`. Returns `CoreError::Contact`
    /// if the contact row is missing.
    pub fn set_group_id(&self, identity: &PublicKey, group_id: &[u8]) -> Result<()> {
        self.pool.with_mut(|c| {
            let changed = c
                .execute(
                    "UPDATE contacts SET group_id = ?1 WHERE identity_pubkey = ?2",
                    rusqlite::params![group_id, &identity.0[..]],
                )
                .map_err(|e| CoreError::Storage(format!("set group_id: {e}")))?;
            if changed == 0 {
                return Err(CoreError::Contact(
                    "contact: group_id: contact not found".into(),
                ));
            }
            Ok(())
        })
    }

    /// Read the MLS group id for `identity`. Returns `Ok(None)` if the
    /// contact row is missing; `Ok(Some(Vec::new()))` for a contact that
    /// has not yet been linked to a group (pre-`AddContact`).
    pub fn get_group_id(&self, identity: &PublicKey) -> Result<Option<Vec<u8>>> {
        self.pool.with(|c| {
            let result = c.query_row(
                "SELECT group_id FROM contacts WHERE identity_pubkey = ?1",
                rusqlite::params![&identity.0[..]],
                |r| r.get::<_, Vec<u8>>(0),
            );
            match result {
                Ok(v) => Ok(Some(v)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Storage(format!("get group_id: {e}"))),
            }
        })
    }

    /// Find every contact whose hex-encoded `identity_pubkey` starts
    /// with `prefix` (case-insensitive). Empty result = no match;
    /// the caller enforces "exactly one" when a unique contact is
    /// required. Returns `CoreError::Contact` if `prefix` contains
    /// non-hex characters.
    pub fn lookup_by_prefix(&self, prefix: &str) -> Result<Vec<PublicKey>> {
        if !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(CoreError::Contact(format!(
                "contact: lookup: non-hex prefix {prefix:?}"
            )));
        }
        let lower = prefix.to_ascii_lowercase();
        self.pool.with(|c| {
            let mut stmt = c
                .prepare("SELECT identity_pubkey FROM contacts")
                .map_err(|e| CoreError::Storage(format!("prepare lookup: {e}")))?;
            let rows = stmt
                .query_map([], |r| r.get::<_, Vec<u8>>(0))
                .map_err(|e| CoreError::Storage(format!("query lookup: {e}")))?;
            let mut out = Vec::new();
            for row in rows {
                let bytes = row.map_err(|e| CoreError::Storage(format!("row lookup: {e}")))?;
                if bytes.len() != 32 {
                    continue;
                }
                let hex = bytes
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>();
                if hex.starts_with(&lower) {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&bytes);
                    out.push(PublicKey(arr));
                }
            }
            Ok(out)
        })
    }
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-core --lib storage::contacts
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: all green, no new clippy warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/storage/contacts.rs
git commit -m "$(cat <<'EOF'
storage: ContactRepo group_id accessors + pubkey-prefix lookup

set_group_id / get_group_id feed the migration-0005 column. The new
lookup_by_prefix implements the CLI's "abc12" shortcut for typing a
contact; it returns every match so the dispatcher can distinguish
ContactNotFound from ContactAmbiguous.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: `MessageRepo::recent_by_group` projection helper

The dispatcher for `RecentMessages` needs to hydrate `StoredMessage` rows into the wire-safe `MessageRecord` type (introduced in Task 5). Today `MessageRepo::recent` returns the raw `StoredMessage`; this task adds an ordering guarantee (`(kind, ts DESC, id DESC)`) that doesn't rely on `ts` alone — CLAUDE.md bans `ts`-based ordering. The MLS generation isn't stored today; use the row `id` (monotonic autoincrement) as a proxy for generation ordering.

**Files:**
- Modify: `crates/core/src/storage/messages.rs:62-91` (stabilize ordering; add `delivered_at` to ORDER BY); extend tests.

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block at the bottom of `crates/core/src/storage/messages.rs`:

```rust
#[test]
fn recent_orders_by_id_desc_not_by_ts() {
    // CLAUDE.md: authoritative ordering is NOT ts-based. The repo
    // must return rows with the newest *inserted* first, independent
    // of the sender-claimed `ts` field (which can be backdated).
    let pool = Pool::in_memory();
    let repo = MessageRepo::new(&pool);
    let gid = vec![0x42u8; 32];
    let sender = [0x01u8; 32];

    // Insert messages with deliberately non-monotonic `ts`:
    // row1 ts=3000, row2 ts=1000, row3 ts=2000.
    // Expected order from recent(): row3, row2, row1 (id DESC).
    let e1 = Envelope { ts: 3000, kind: Kind::Text { body: "first".into() }, message_id: MessageId([1; 16]) };
    let e2 = Envelope { ts: 1000, kind: Kind::Text { body: "second".into() }, message_id: MessageId([2; 16]) };
    let e3 = Envelope { ts: 2000, kind: Kind::Text { body: "third".into() }, message_id: MessageId([3; 16]) };

    repo.insert(&gid, &sender, &e1).unwrap();
    repo.insert(&gid, &sender, &e2).unwrap();
    repo.insert(&gid, &sender, &e3).unwrap();

    let rows = repo.recent(&gid, 10).unwrap();
    // id DESC -> e3 (id=3), e2 (id=2), e1 (id=1).
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].ts, 2000, "row 0 must be last-inserted, not max ts");
    assert_eq!(rows[1].ts, 1000);
    assert_eq!(rows[2].ts, 3000);
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-core --lib storage::messages::tests::recent_orders_by_id_desc -- --nocapture
```
Expected: FAIL — today's implementation orders by `ts DESC` so row 0 will be the `ts=3000` row.

- [ ] **Step 3: Replace the SQL ordering**

In `crates/core/src/storage/messages.rs` replace the SELECT inside `recent()` (around line 65–70) with `id`-desc ordering:

```rust
    /// Most-recent-first list of messages in a group.
    ///
    /// Ordering is by row `id` DESC (SQLite autoincrement), NOT by
    /// `ts`. Sender-claimed timestamps are display-only per CLAUDE.md
    /// — backdated messages must not front-run newer inserts.
    pub fn recent(&self, group_id: &[u8], limit: usize) -> Result<Vec<StoredMessage>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, group_id, sender, kind, body_blob, ts, delivered_at \
                     FROM messages \
                     WHERE group_id = ?1 \
                     ORDER BY id DESC LIMIT ?2",
                )
                .map_err(|e| CoreError::Storage(format!("prepare recent: {e}")))?;
            let rows = stmt
                .query_map(
                    rusqlite::params![group_id, i64::try_from(limit).unwrap_or(i64::MAX)],
                    |r| {
                        Ok(StoredMessage {
                            id: r.get(0)?,
                            group_id: r.get(1)?,
                            sender: r.get(2)?,
                            kind: r.get(3)?,
                            body_blob: r.get(4)?,
                            ts: r.get(5)?,
                            delivered_at: r.get(6)?,
                        })
                    },
                )
                .map_err(|e| CoreError::Storage(format!("query recent: {e}")))?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| CoreError::Storage(format!("collect recent: {e}")))
        })
    }
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-core --lib storage::messages
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: all green (the new test plus any existing `storage::messages` tests).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/storage/messages.rs
git commit -m "$(cat <<'EOF'
storage: MessageRepo::recent orders by id DESC, not ts

Sender-claimed timestamps are display-only per CLAUDE.md; a backdated
Envelope must not front-run newer inserts. Orders by SQLite rowid
(monotonic autoincrement) which is the closest proxy we have to an
authoritative receive order until MLS generations are stored.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Hex newtypes `Hex16` and `Hex32`

The CLI needs stable, printable, prefix-matchable hex strings for `MessageId` (`[u8; 16]`) and `PublicKey` (`[u8; 32]`). Put the newtypes in a new `daemon::hex` submodule so dispatch and IPC can both reuse them without cross-imports.

**Files:**
- Create: `crates/core/src/daemon/hex.rs`
- Modify: `crates/core/src/daemon/mod.rs` (add `pub mod hex; pub use hex::{Hex16, Hex32};`)

- [ ] **Step 1: Write the failing test**

Create `crates/core/src/daemon/hex.rs` with only the test module populated (the `Hex16`/`Hex32` types will fail to resolve until Step 3):

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Hex newtypes for wire-safe byte arrays.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex16_display_lowercase_roundtrip() {
        let raw = [0xABu8; 16];
        let h = Hex16::from(raw);
        let s = h.to_string();
        assert_eq!(s, "abababababababababababababababab");
        let parsed: Hex16 = s.parse().unwrap();
        assert_eq!(parsed.0, raw);
    }

    #[test]
    fn hex32_display_lowercase_roundtrip() {
        let raw = [0x01u8; 32];
        let h = Hex32::from(raw);
        let s = h.to_string();
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
        let parsed: Hex32 = s.parse().unwrap();
        assert_eq!(parsed.0, raw);
    }

    #[test]
    fn hex16_rejects_wrong_length() {
        let err: Result<Hex16, _> = "aa".parse();
        assert!(err.is_err());
    }

    #[test]
    fn hex32_rejects_non_hex() {
        let err: Result<Hex32, _> = "zz".repeat(32).parse();
        assert!(err.is_err());
    }

    #[test]
    fn hex32_serde_roundtrip_as_string() {
        let h = Hex32::from([0xCDu8; 32]);
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&h, &mut buf).unwrap();
        let back: Hex32 = ciborium::de::from_reader(&buf[..]).unwrap();
        assert_eq!(back.0, h.0);
    }
}
```

Then wire the module: edit `crates/core/src/daemon/mod.rs` to append:

```rust
pub mod hex;
pub use hex::{Hex16, Hex32};
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p skattr-core --lib daemon::hex::tests -- --nocapture
```
Expected: compilation FAILS with `cannot find type 'Hex16' in this scope`.

- [ ] **Step 3: Implement `Hex16` and `Hex32`**

Replace the contents of `crates/core/src/daemon/hex.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Hex newtypes for wire-safe byte arrays.
//!
//! Both types display as lowercase hex, parse case-insensitively, and
//! round-trip through serde as hex strings. `Hex32` in particular is
//! what the CLI prints for a pubkey and what the user may prefix-match
//! against via `ContactRepo::lookup_by_prefix`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 16-byte hex newtype (backing `MessageId`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hex16(pub [u8; 16]);

/// 32-byte hex newtype (backing `PublicKey`, `GroupId`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hex32(pub [u8; 32]);

impl From<[u8; 16]> for Hex16 {
    fn from(b: [u8; 16]) -> Self {
        Self(b)
    }
}

impl From<[u8; 32]> for Hex32 {
    fn from(b: [u8; 32]) -> Self {
        Self(b)
    }
}

impl fmt::Display for Hex16 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Hex32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Parse errors for the hex newtypes.
#[derive(Debug, thiserror::Error)]
pub enum HexParseError {
    /// Input string was not the expected length.
    #[error("hex: expected {expected} characters, got {got}")]
    Length { expected: usize, got: usize },
    /// Input contained a non-hex character.
    #[error("hex: non-hex character at byte {index}")]
    NonHex { index: usize },
}

fn parse_fixed<const N: usize>(s: &str) -> Result<[u8; N], HexParseError> {
    if s.len() != N * 2 {
        return Err(HexParseError::Length { expected: N * 2, got: s.len() });
    }
    let mut out = [0u8; N];
    for (i, chunk) in s.as_bytes().chunks_exact(2).enumerate() {
        let hi = from_hex_char(chunk[0]).ok_or(HexParseError::NonHex { index: i * 2 })?;
        let lo = from_hex_char(chunk[1]).ok_or(HexParseError::NonHex { index: i * 2 + 1 })?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

fn from_hex_char(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

impl FromStr for Hex16 {
    type Err = HexParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(parse_fixed::<16>(s)?))
    }
}

impl FromStr for Hex32 {
    type Err = HexParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(parse_fixed::<32>(s)?))
    }
}

impl Serialize for Hex16 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Hex16 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl Serialize for Hex32 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Hex32 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex16_display_lowercase_roundtrip() {
        let raw = [0xABu8; 16];
        let h = Hex16::from(raw);
        let s = h.to_string();
        assert_eq!(s, "abababababababababababababababab");
        let parsed: Hex16 = s.parse().unwrap();
        assert_eq!(parsed.0, raw);
    }

    #[test]
    fn hex32_display_lowercase_roundtrip() {
        let raw = [0x01u8; 32];
        let h = Hex32::from(raw);
        let s = h.to_string();
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
        let parsed: Hex32 = s.parse().unwrap();
        assert_eq!(parsed.0, raw);
    }

    #[test]
    fn hex16_rejects_wrong_length() {
        let err: Result<Hex16, _> = "aa".parse();
        assert!(err.is_err());
    }

    #[test]
    fn hex32_rejects_non_hex() {
        let err: Result<Hex32, _> = "zz".repeat(32).parse();
        assert!(err.is_err());
    }

    #[test]
    fn hex32_serde_roundtrip_as_string() {
        let h = Hex32::from([0xCDu8; 32]);
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&h, &mut buf).unwrap();
        let back: Hex32 = ciborium::de::from_reader(&buf[..]).unwrap();
        assert_eq!(back.0, h.0);
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-core --lib daemon::hex
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: five green tests, no new clippy warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/hex.rs crates/core/src/daemon/mod.rs
git commit -m "$(cat <<'EOF'
daemon: Hex16/Hex32 newtypes for wire display + parse

Lowercase-hex Display, case-insensitive FromStr, string serde.
Used by Phase 1.F CLI to print MessageId and PublicKey and to
accept prefix input at the command line.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Expand `Command` / `CommandResult` with 1.F variants and projections

**Files:**
- Modify: `crates/core/src/daemon/commands.rs` (add new variants; add `ContactSummary`, `MessageRecord`, `SendStatus`, `Direction`; keep existing variants)

- [ ] **Step 1: Write the failing test**

Append to `crates/core/src/daemon/commands.rs` a test module (there isn't one today — add it at the bottom):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::hex::Hex32;
    use crate::envelope::Kind;

    fn roundtrip<T>(value: &T) -> T
    where
        T: serde::Serialize + for<'de> serde::Deserialize<'de>,
    {
        let mut buf = Vec::new();
        ciborium::ser::into_writer(value, &mut buf).unwrap();
        ciborium::de::from_reader(&buf[..]).unwrap()
    }

    #[test]
    fn new_command_variants_serde_roundtrip() {
        let cmds: Vec<Command> = vec![
            Command::ListContacts,
            Command::RecentMessages { contact: None, limit: 50 },
            Command::RecentMessages {
                contact: Some(crate::identity::PublicKey([1; 32])),
                limit: 10,
            },
            Command::CreateInvite { nickname: Some("alice".into()), ttl_secs: Some(3600) },
        ];
        for cmd in &cmds {
            let _back: Command = roundtrip(cmd);
        }
    }

    #[test]
    fn new_result_variants_serde_roundtrip() {
        let results: Vec<CommandResult> = vec![
            CommandResult::Contacts(vec![ContactSummary {
                pubkey: crate::identity::PublicKey([7; 32]),
                nickname: Some("bob".into()),
                onion: "bbbb.onion".into(),
                card_version: 1,
                added_at: 1_700_000_000,
            }]),
            CommandResult::Messages(vec![MessageRecord {
                message_id: crate::daemon::hex::Hex16::from([2; 16]),
                contact: crate::identity::PublicKey([7; 32]),
                direction: Direction::Incoming,
                kind: Kind::Text { body: "hi".into() },
                mls_generation: 0,
                ts_daemon_recv: 1_700_000_100,
                ts_envelope: 1_700_000_000,
            }]),
            CommandResult::MessageSent {
                message_id: crate::daemon::hex::Hex16::from([3; 16]),
                status: SendStatus::Queued,
            },
            CommandResult::Subscribed,
            CommandResult::InviteCreated {
                url: "skattr://invite/v1#...".into(),
                key_package_id: Hex32::from([9; 32]),
                expires_at: 1_700_003_600,
            },
        ];
        for r in &results {
            let _back: CommandResult = roundtrip(r);
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p skattr-core --lib daemon::commands::tests -- --nocapture
```
Expected: compile FAILS — `ListContacts` / `RecentMessages` / `Contacts` / `Messages` / `Subscribed` / `ContactSummary` / `MessageRecord` / `SendStatus` / `Direction` / `key_package_id` / `expires_at` / `ttl_secs` all missing.

- [ ] **Step 3: Replace `commands.rs` with the expanded surface**

Rewrite `crates/core/src/daemon/commands.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Commands submitted into the daemon from the UI / CLI.
//!
//! This is the forward half of the daemon's public API. See
//! [`super::events`] for the reverse (events emitted by the daemon).

use serde::{Deserialize, Serialize};

use crate::daemon::hex::{Hex16, Hex32};
use crate::envelope::Kind;
use crate::identity::PublicKey;
use crate::invite::InviteLink;

/// Request sent into the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Generate a fresh invite link and surface it for display / QR.
    CreateInvite {
        /// Optional human-readable nickname embedded in the welcome UX.
        nickname: Option<String>,
        /// Optional TTL in seconds. `None` uses the default (24 h).
        #[serde(default)]
        ttl_secs: Option<u64>,
    },
    /// Consume an invite link from another user.
    AddContact {
        /// Full `skattr://invite/v1#...` URL.
        invite_url: String,
    },
    /// List every known contact with latest card + group link.
    ListContacts,
    /// Send a payload to a contact.
    SendMessage {
        /// Recipient identity pubkey.
        contact: PublicKey,
        /// Envelope payload.
        kind: Kind,
    },
    /// Return recent persisted messages, optionally filtered by contact.
    RecentMessages {
        /// If `Some`, only messages with this peer (either direction).
        contact: Option<PublicKey>,
        /// Max rows to return.
        limit: u32,
    },
    /// Start a new MLS group with the given initial members. Reserved
    /// for Phase 2; 1.F server answers `IpcError::UnknownCommand`.
    CreateGroup {
        /// Initial group members.
        members: Vec<PublicKey>,
        /// Human-readable group name.
        name: String,
    },
    /// Rotate the onion service address.
    RotateOnion,
    /// Graceful daemon shutdown.
    Shutdown,
}

/// Outcome of a `SendMessage` command after the inline-delivery wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendStatus {
    /// Hub accepted the ciphertext; ACK not seen within the inline wait.
    Queued,
    /// Hub reported delivery ACK within the inline wait.
    Delivered,
}

/// Direction of a stored message relative to the local identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Received from peer.
    Incoming,
    /// Sent to peer.
    Outgoing,
}

/// Wire-safe projection of a contact row + latest card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactSummary {
    /// Ed25519 identity pubkey.
    pub pubkey: PublicKey,
    /// User-settable local nickname.
    pub nickname: Option<String>,
    /// Onion address from the latest verified `ContactCard`.
    pub onion: String,
    /// Version of the latest known `ContactCard`.
    pub card_version: u64,
    /// Unix seconds when the contact was first added locally.
    pub added_at: u64,
}

/// Wire-safe projection of a persisted message row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRecord {
    /// 16-byte per-message id.
    pub message_id: Hex16,
    /// Peer identity pubkey.
    pub contact: PublicKey,
    /// Incoming or outgoing.
    pub direction: Direction,
    /// Envelope payload.
    pub kind: Kind,
    /// MLS generation number (0 until 1.G/2.x populate it).
    pub mls_generation: u64,
    /// Authoritative local-clock receive timestamp (unix seconds).
    pub ts_daemon_recv: u64,
    /// Sender-claimed timestamp — display only (unix seconds signed).
    pub ts_envelope: i64,
}

/// Response returned for a completed [`Command`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum CommandResult {
    /// The invite link for [`Command::CreateInvite`].
    InviteCreated {
        /// Canonical `skattr://invite/v1#...` URL.
        url: String,
        /// 32-byte KeyPackage hash (the single-use id).
        key_package_id: Hex32,
        /// Unix seconds when the invite expires.
        expires_at: u64,
    },
    /// [`Command::AddContact`] completed; full summary returned so the
    /// CLI can render the new contact without a follow-up query.
    ContactAdded(ContactSummary),
    /// [`Command::ListContacts`] completed.
    Contacts(Vec<ContactSummary>),
    /// [`Command::SendMessage`] completed (either Queued or Delivered).
    MessageSent {
        /// 16-byte per-message id (for correlation with later
        /// `Event::DeliveryStatusChanged`).
        message_id: Hex16,
        /// Outcome after the inline wait.
        status: SendStatus,
    },
    /// [`Command::RecentMessages`] completed. Most-recent first.
    Messages(Vec<MessageRecord>),
    /// Acknowledges a `Subscribe` request. No payload.
    Subscribed,
    /// No-payload acknowledgement (rotate, shutdown, etc.).
    Ok,
}

impl From<InviteLink> for CommandResult {
    fn from(link: InviteLink) -> Self {
        #[allow(clippy::expect_used)]
        let url = link.to_url().expect("valid InviteLink serializes cleanly");
        Self::InviteCreated {
            url,
            // The real dispatcher populates these two fields; this
            // impl stays for backward-compatibility with existing
            // test callers that only care about the URL.
            key_package_id: Hex32::from([0u8; 32]),
            expires_at: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::hex::Hex32;
    use crate::envelope::Kind;

    fn roundtrip<T>(value: &T) -> T
    where
        T: serde::Serialize + for<'de> serde::Deserialize<'de>,
    {
        let mut buf = Vec::new();
        ciborium::ser::into_writer(value, &mut buf).unwrap();
        ciborium::de::from_reader(&buf[..]).unwrap()
    }

    #[test]
    fn new_command_variants_serde_roundtrip() {
        let cmds: Vec<Command> = vec![
            Command::ListContacts,
            Command::RecentMessages { contact: None, limit: 50 },
            Command::RecentMessages {
                contact: Some(crate::identity::PublicKey([1; 32])),
                limit: 10,
            },
            Command::CreateInvite { nickname: Some("alice".into()), ttl_secs: Some(3600) },
        ];
        for cmd in &cmds {
            let _back: Command = roundtrip(cmd);
        }
    }

    #[test]
    fn new_result_variants_serde_roundtrip() {
        let results: Vec<CommandResult> = vec![
            CommandResult::Contacts(vec![ContactSummary {
                pubkey: crate::identity::PublicKey([7; 32]),
                nickname: Some("bob".into()),
                onion: "bbbb.onion".into(),
                card_version: 1,
                added_at: 1_700_000_000,
            }]),
            CommandResult::Messages(vec![MessageRecord {
                message_id: crate::daemon::hex::Hex16::from([2; 16]),
                contact: crate::identity::PublicKey([7; 32]),
                direction: Direction::Incoming,
                kind: Kind::Text { body: "hi".into() },
                mls_generation: 0,
                ts_daemon_recv: 1_700_000_100,
                ts_envelope: 1_700_000_000,
            }]),
            CommandResult::MessageSent {
                message_id: crate::daemon::hex::Hex16::from([3; 16]),
                status: SendStatus::Queued,
            },
            CommandResult::Subscribed,
            CommandResult::InviteCreated {
                url: "skattr://invite/v1#...".into(),
                key_package_id: Hex32::from([9; 32]),
                expires_at: 1_700_003_600,
            },
        ];
        for r in &results {
            let _back: CommandResult = roundtrip(r);
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-core --lib daemon::commands
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: two new tests green; every existing dependent (e.g. `From<InviteLink> for CommandResult`) still compiles because `InviteCreated` now has extra fields with safe defaults.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/commands.rs
git commit -m "$(cat <<'EOF'
daemon: expand Command/CommandResult for 1.F wire surface

+ Command::ListContacts, RecentMessages; CreateInvite now carries
  ttl_secs. + CommandResult::Contacts, Messages, Subscribed; InviteCreated
  now carries key_package_id + expires_at; MessageSent grows a
  SendStatus discriminator. New wire-safe projections: ContactSummary,
  MessageRecord, SendStatus, Direction.

CreateGroup stays in the enum but the Phase 1.F server answers
IpcError::UnknownCommand until Phase 2.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: `DaemonErrorKind` and `CoreError::kind()` adapter

**Files:**
- Create: `crates/core/src/daemon/error_kind.rs`
- Modify: `crates/core/src/daemon/mod.rs` (add `pub mod error_kind; pub use error_kind::DaemonErrorKind;`)
- Modify: `crates/core/src/error.rs` (append `impl CoreError { pub fn kind(&self) -> Option<DaemonErrorKind> { ... } }`)

- [ ] **Step 1: Write the failing test**

Create `crates/core/src/daemon/error_kind.rs` with the test module but no types yet:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Stable wire enum projecting rich `CoreError`s onto the IPC surface.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CoreError;

    #[test]
    fn contact_not_found_maps() {
        let e = CoreError::Contact("contact: lookup: not found (pubkey=ab…)".into());
        assert_eq!(e.kind(), Some(DaemonErrorKind::ContactNotFound));
    }

    #[test]
    fn invite_expired_maps() {
        let e = CoreError::Invite("invite: expired at 1700000000".into());
        assert_eq!(e.kind(), Some(DaemonErrorKind::InviteExpired));
    }

    #[test]
    fn invite_consumed_maps() {
        let e = CoreError::Invite("invite: key package already consumed".into());
        assert_eq!(e.kind(), Some(DaemonErrorKind::InviteConsumed));
    }

    #[test]
    fn unmapped_returns_none() {
        let e = CoreError::Storage("random sqlite hiccup".into());
        assert_eq!(e.kind(), None);
    }

    #[test]
    fn delivery_timeout_maps() {
        let e = CoreError::Delivery("delivery: timeout waiting for ACK".into());
        assert_eq!(e.kind(), Some(DaemonErrorKind::DeliveryTimeout));
    }
}
```

Append to `crates/core/src/daemon/mod.rs`:

```rust
pub mod error_kind;
pub use error_kind::DaemonErrorKind;
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p skattr-core --lib daemon::error_kind -- --nocapture
```
Expected: compile FAILS — `DaemonErrorKind` + `CoreError::kind()` missing.

- [ ] **Step 3: Implement `DaemonErrorKind`**

Replace `crates/core/src/daemon/error_kind.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Stable wire enum projecting rich `CoreError`s onto the IPC surface.
//!
//! The principle: every variant here is an ANSWER a CLI can handle
//! specifically — retry, rephrase the user prompt, pick a different
//! pubkey. `CoreError::kind()` maps as many library errors as we can
//! into these; anything unmapped becomes `IpcError::Internal(..)` on
//! the wire (with the full `CoreError` logged server-side).

use serde::{Deserialize, Serialize};

/// Typed categories a CLI can act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DaemonErrorKind {
    /// No contact matches the given pubkey or prefix.
    ContactNotFound,
    /// A prefix matched more than one contact.
    ContactAmbiguous {
        /// Number of contacts that matched the prefix.
        matches: u32,
    },
    /// The invite's `expires_at` is in the past.
    InviteExpired,
    /// The invite's single-use KeyPackage has already been consumed.
    InviteConsumed,
    /// The invite's Ed25519 signature did not verify.
    InviteSignatureInvalid,
    /// MLS group state is unreadable / inconsistent.
    GroupCorrupt,
    /// The daemon's inline wait expired before an ACK arrived; retry
    /// or subscribe to `DeliveryStatusChanged` for the outcome.
    DeliveryTimeout,
    /// Tor is still bootstrapping; retry shortly.
    TorNotReady,
    /// Storage-layer failure that the CLI can't disambiguate further.
    StorageError,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CoreError;

    #[test]
    fn contact_not_found_maps() {
        let e = CoreError::Contact("contact: lookup: not found (pubkey=ab…)".into());
        assert_eq!(e.kind(), Some(DaemonErrorKind::ContactNotFound));
    }

    #[test]
    fn invite_expired_maps() {
        let e = CoreError::Invite("invite: expired at 1700000000".into());
        assert_eq!(e.kind(), Some(DaemonErrorKind::InviteExpired));
    }

    #[test]
    fn invite_consumed_maps() {
        let e = CoreError::Invite("invite: key package already consumed".into());
        assert_eq!(e.kind(), Some(DaemonErrorKind::InviteConsumed));
    }

    #[test]
    fn unmapped_returns_none() {
        let e = CoreError::Storage("random sqlite hiccup".into());
        assert_eq!(e.kind(), None);
    }

    #[test]
    fn delivery_timeout_maps() {
        let e = CoreError::Delivery("delivery: timeout waiting for ACK".into());
        assert_eq!(e.kind(), Some(DaemonErrorKind::DeliveryTimeout));
    }
}
```

- [ ] **Step 4: Add the `CoreError::kind()` adapter**

Append to `crates/core/src/error.rs` below the enum definition:

```rust
impl CoreError {
    /// Project this library error onto the stable [`crate::daemon::error_kind::DaemonErrorKind`]
    /// wire enum. Returns `None` when the error has no specific category
    /// the CLI can act on — the IPC layer turns those into
    /// `IpcError::Internal` and logs the full `CoreError` server-side.
    ///
    /// Matching is string-based for now because library error payloads
    /// are free-form `String`s rather than structured variants. If this
    /// grows unwieldy, Phase 2 can restructure the subsystem error
    /// strings into dedicated sub-enums with `thiserror` `#[from]`.
    #[must_use]
    pub fn kind(&self) -> Option<crate::daemon::error_kind::DaemonErrorKind> {
        use crate::daemon::error_kind::DaemonErrorKind as K;
        match self {
            CoreError::Contact(s) if s.contains("not found") => Some(K::ContactNotFound),
            CoreError::Contact(s) if s.contains("ambiguous") => {
                // Format: "contact: lookup: ambiguous prefix (N matches)"
                // The integer is advisory; 0 is a safe fallback.
                let matches = extract_matches_count(s).unwrap_or(0);
                Some(K::ContactAmbiguous { matches })
            }
            CoreError::Invite(s) if s.contains("expired") => Some(K::InviteExpired),
            CoreError::Invite(s) if s.contains("consumed") => Some(K::InviteConsumed),
            CoreError::Invite(s) if s.contains("signature") => Some(K::InviteSignatureInvalid),
            CoreError::Mls(s) if s.contains("corrupt") => Some(K::GroupCorrupt),
            CoreError::Delivery(s) if s.contains("timeout") => Some(K::DeliveryTimeout),
            CoreError::Transport(s) if s.contains("not ready") || s.contains("bootstrap") => {
                Some(K::TorNotReady)
            }
            CoreError::Sqlite(_) | CoreError::Storage(_) => Some(K::StorageError),
            _ => None,
        }
    }
}

fn extract_matches_count(s: &str) -> Option<u32> {
    // Very narrow parser: locate "(N " where N is decimal.
    let open = s.find('(')? + 1;
    let rest = &s[open..];
    let end = rest.find(|c: char| !c.is_ascii_digit())?;
    rest[..end].parse().ok()
}
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo test -p skattr-core --lib daemon::error_kind
cargo test -p skattr-core --lib error
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/error_kind.rs crates/core/src/daemon/mod.rs crates/core/src/error.rs
git commit -m "$(cat <<'EOF'
daemon: DaemonErrorKind wire enum + CoreError::kind adapter

Stable IPC-surface projection of library errors. Anything unmapped
becomes IpcError::Internal at the wire layer; the full CoreError
stays server-side in tracing logs so internal schema strings never
leak to CLI clients.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: IPC wire types (`daemon::ipc::wire`)

**Files:**
- Create: `crates/core/src/daemon/ipc/mod.rs`
- Create: `crates/core/src/daemon/ipc/wire.rs`
- Modify: `crates/core/src/daemon/mod.rs` (add `pub mod ipc;`)

- [ ] **Step 1: Write the failing test**

Create `crates/core/src/daemon/ipc/wire.rs` with types stubbed out and tests populated (the types will come in Step 3):

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! IPC wire types: `IpcRequest`, `IpcResponse`, `IpcError`,
//! `EventFilter`. Every variant round-trips through CBOR.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::commands::{Command, CommandResult};
    use crate::daemon::DaemonErrorKind;
    use crate::identity::PublicKey;

    fn roundtrip<T>(value: &T) -> T
    where
        T: serde::Serialize + for<'de> serde::Deserialize<'de>,
    {
        let mut buf = Vec::new();
        ciborium::ser::into_writer(value, &mut buf).unwrap();
        ciborium::de::from_reader(&buf[..]).unwrap()
    }

    #[test]
    fn ipc_request_variants_roundtrip() {
        let reqs: Vec<IpcRequest> = vec![
            IpcRequest::Execute(Command::ListContacts),
            IpcRequest::Subscribe(EventFilter::All),
            IpcRequest::Subscribe(EventFilter::Contact(PublicKey([3; 32]))),
            IpcRequest::Subscribe(EventFilter::TorStatus),
            IpcRequest::Shutdown,
        ];
        for r in &reqs {
            let _back: IpcRequest = roundtrip(r);
        }
    }

    #[test]
    fn ipc_response_variants_roundtrip() {
        let resps: Vec<IpcResponse> = vec![
            IpcResponse::Ok(CommandResult::Ok),
            IpcResponse::Err(IpcError::AuthDenied),
            IpcResponse::Err(IpcError::Codec("bad cbor".into())),
            IpcResponse::Err(IpcError::FrameTooLarge { got: 2_000_000, max: 1_048_576 }),
            IpcResponse::Err(IpcError::UnknownCommand),
            IpcResponse::Err(IpcError::VaultNotReady),
            IpcResponse::Err(IpcError::Daemon(DaemonErrorKind::ContactNotFound)),
            IpcResponse::Err(IpcError::Internal("whatever".into())),
            IpcResponse::Bye,
        ];
        for r in &resps {
            let _back: IpcResponse = roundtrip(r);
        }
    }
}
```

Create `crates/core/src/daemon/ipc/mod.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! CLI ↔ daemon IPC transport.

pub mod wire;
```

Append to `crates/core/src/daemon/mod.rs`:

```rust
pub mod ipc;
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p skattr-core --lib daemon::ipc::wire -- --nocapture
```
Expected: compile FAILS with `cannot find type 'IpcRequest' in this scope`.

- [ ] **Step 3: Implement the wire types**

Replace the contents of `crates/core/src/daemon/ipc/wire.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! IPC wire types: `IpcRequest`, `IpcResponse`, `IpcError`,
//! `EventFilter`. Every variant round-trips through CBOR.

use serde::{Deserialize, Serialize};

use crate::daemon::commands::{Command, CommandResult};
use crate::daemon::events::Event;
use crate::daemon::DaemonErrorKind;
use crate::identity::PublicKey;

/// Maximum CBOR body size accepted on the IPC socket. 1 MiB.
///
/// Envelopes larger than this still go via the MLS + DeliveryHub
/// paths — the IPC wire is for control plane and small payloads.
pub const MAX_IPC_BODY: u32 = 1024 * 1024;

/// Subscription filter for `IpcRequest::Subscribe`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "filter", rename_all = "snake_case")]
pub enum EventFilter {
    /// Forward every event the daemon emits.
    All,
    /// Only `MessageReceived` + `DeliveryStatusChanged` relating to this peer.
    Contact(PublicKey),
    /// Only `TorStatusChanged`.
    TorStatus,
}

/// Request frame sent from CLI to daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "req", rename_all = "snake_case")]
pub enum IpcRequest {
    /// One-shot command; expect a single `Ok` or `Err` response.
    Execute(Command),
    /// Long-lived subscription; expect one `Ok(Subscribed)` then a
    /// stream of `Event(..)` frames until the client hangs up.
    Subscribe(EventFilter),
    /// Graceful daemon shutdown. Expect `Ok(ShuttingDown)` then `Bye`.
    Shutdown,
}

/// Response frame sent from daemon to CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "resp", rename_all = "snake_case")]
pub enum IpcResponse {
    /// Successful command result.
    Ok(CommandResult),
    /// Typed error.
    Err(IpcError),
    /// Event delivery (only after a `Subscribe` request).
    Event(Event),
    /// Terminal frame before the server closes the connection.
    Bye,
}

/// Stable wire error. `Daemon(DaemonErrorKind)` carries the library-error
/// projection; `Internal(String)` is a 256-byte-truncated fallback whose
/// full detail stays server-side in tracing logs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "err", rename_all = "snake_case")]
pub enum IpcError {
    /// `SO_PEERCRED`/`getpeereid` reported a UID other than the daemon's.
    AuthDenied,
    /// CBOR decode failure (connection stays open).
    Codec(String),
    /// Declared frame body exceeded `MAX_IPC_BODY`.
    FrameTooLarge { got: u32, max: u32 },
    /// Variant is reserved for a future phase (e.g. `CreateGroup` in 1.F).
    UnknownCommand,
    /// Daemon is still booting; retry.
    VaultNotReady,
    /// Typed library error from the daemon.
    Daemon(DaemonErrorKind),
    /// Unmapped failure. Body truncated to 256 bytes.
    Internal(String),
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-core --lib daemon::ipc::wire
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: both round-trip tests green, no clippy warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/ipc/mod.rs crates/core/src/daemon/ipc/wire.rs crates/core/src/daemon/mod.rs
git commit -m "$(cat <<'EOF'
daemon: ipc::wire — IpcRequest/Response/Error/EventFilter

Every variant CBOR-roundtrips. MAX_IPC_BODY = 1 MiB (control plane;
attachments still go through DeliveryHub). Subscribe is a long-lived
stream; Execute is one-shot.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: IPC codec (`daemon::ipc::codec`)

**Files:**
- Create: `crates/core/src/daemon/ipc/codec.rs`
- Modify: `crates/core/src/daemon/ipc/mod.rs` (add `pub mod codec;`)

- [ ] **Step 1: Write the failing test**

Create `crates/core/src/daemon/ipc/codec.rs` with the tests populated but no implementation:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Length-prefix + CBOR codec for IPC frames.
//!
//! Frame layout: `u32_be(body_len) || cbor(body)`. `body_len` is
//! capped by `wire::MAX_IPC_BODY`.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::commands::Command;
    use crate::daemon::ipc::wire::{IpcRequest, IpcResponse, MAX_IPC_BODY};

    #[tokio::test]
    async fn roundtrip_request_response() {
        let req = IpcRequest::Execute(Command::ListContacts);

        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &req).await.unwrap();

        let mut reader = &buf[..];
        let back: IpcRequest = read_frame(&mut reader).await.unwrap();
        match back {
            IpcRequest::Execute(Command::ListContacts) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_rejects_oversize_length_prefix() {
        let mut buf: Vec<u8> = Vec::new();
        let bogus_len: u32 = MAX_IPC_BODY + 1;
        buf.extend_from_slice(&bogus_len.to_be_bytes());
        let mut reader = &buf[..];
        let err = read_frame::<_, IpcRequest>(&mut reader).await.unwrap_err();
        assert!(
            matches!(err, CodecError::FrameTooLarge { got, max } if got == bogus_len && max == MAX_IPC_BODY),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn read_rejects_zero_length_frame() {
        let buf: Vec<u8> = 0u32.to_be_bytes().to_vec();
        let mut reader = &buf[..];
        let err = read_frame::<_, IpcRequest>(&mut reader).await.unwrap_err();
        assert!(matches!(err, CodecError::EmptyFrame), "got {err:?}");
    }

    #[tokio::test]
    async fn read_rejects_malformed_cbor() {
        let mut buf: Vec<u8> = Vec::new();
        let bad_body = vec![0xFF_u8; 8]; // not valid CBOR
        let len = u32::try_from(bad_body.len()).unwrap();
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&bad_body);
        let mut reader = &buf[..];
        let err = read_frame::<_, IpcResponse>(&mut reader).await.unwrap_err();
        assert!(matches!(err, CodecError::Cbor(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn write_rejects_oversize_body() {
        // Build a payload whose CBOR form exceeds MAX_IPC_BODY by
        // using an enormous String variant.
        let big = "x".repeat((MAX_IPC_BODY + 1) as usize);
        let req = IpcRequest::Execute(Command::AddContact { invite_url: big });
        let mut buf: Vec<u8> = Vec::new();
        let err = write_frame(&mut buf, &req).await.unwrap_err();
        assert!(matches!(err, CodecError::FrameTooLarge { .. }), "got {err:?}");
    }
}
```

Append to `crates/core/src/daemon/ipc/mod.rs`:

```rust
pub mod codec;
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p skattr-core --lib daemon::ipc::codec -- --nocapture
```
Expected: compile FAILS — `write_frame`, `read_frame`, `CodecError` all missing.

- [ ] **Step 3: Implement the codec**

Replace the contents of `crates/core/src/daemon/ipc/codec.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Length-prefix + CBOR codec for IPC frames.
//!
//! Frame layout: `u32_be(body_len) || cbor(body)`. `body_len` is
//! capped by [`crate::daemon::ipc::wire::MAX_IPC_BODY`].

use std::io;

use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::daemon::ipc::wire::MAX_IPC_BODY;

/// Codec-layer errors. Higher layers map these to [`crate::daemon::ipc::wire::IpcError`].
#[derive(Debug, Error)]
pub enum CodecError {
    /// I/O failure during read/write.
    #[error("codec: io: {0}")]
    Io(#[from] io::Error),
    /// Declared length exceeded `MAX_IPC_BODY`.
    #[error("codec: frame too large ({got} bytes, max {max})")]
    FrameTooLarge { got: u32, max: u32 },
    /// Declared length was zero (protocol error — an empty CBOR body
    /// is not allowed; callers must send at least the smallest
    /// variant's encoding).
    #[error("codec: empty frame")]
    EmptyFrame,
    /// CBOR serialize/deserialize failure.
    #[error("codec: cbor: {0}")]
    Cbor(String),
}

/// Encode `value` as CBOR and write it as a length-prefixed frame.
pub async fn write_frame<W, T>(w: &mut W, value: &T) -> Result<(), CodecError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut body: Vec<u8> = Vec::new();
    ciborium::ser::into_writer(value, &mut body).map_err(|e| CodecError::Cbor(e.to_string()))?;
    let len = u32::try_from(body.len()).map_err(|_| CodecError::FrameTooLarge {
        got: u32::MAX,
        max: MAX_IPC_BODY,
    })?;
    if len > MAX_IPC_BODY {
        return Err(CodecError::FrameTooLarge { got: len, max: MAX_IPC_BODY });
    }
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(&body).await?;
    w.flush().await?;
    Ok(())
}

/// Read one length-prefixed CBOR frame and decode it.
pub async fn read_frame<R, T>(r: &mut R) -> Result<T, CodecError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);
    if len == 0 {
        return Err(CodecError::EmptyFrame);
    }
    if len > MAX_IPC_BODY {
        return Err(CodecError::FrameTooLarge { got: len, max: MAX_IPC_BODY });
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body).await?;
    ciborium::de::from_reader(&body[..]).map_err(|e| CodecError::Cbor(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::commands::Command;
    use crate::daemon::ipc::wire::{IpcRequest, IpcResponse, MAX_IPC_BODY};

    #[tokio::test]
    async fn roundtrip_request_response() {
        let req = IpcRequest::Execute(Command::ListContacts);

        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &req).await.unwrap();

        let mut reader = &buf[..];
        let back: IpcRequest = read_frame(&mut reader).await.unwrap();
        match back {
            IpcRequest::Execute(Command::ListContacts) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_rejects_oversize_length_prefix() {
        let mut buf: Vec<u8> = Vec::new();
        let bogus_len: u32 = MAX_IPC_BODY + 1;
        buf.extend_from_slice(&bogus_len.to_be_bytes());
        let mut reader = &buf[..];
        let err = read_frame::<_, IpcRequest>(&mut reader).await.unwrap_err();
        assert!(
            matches!(err, CodecError::FrameTooLarge { got, max } if got == bogus_len && max == MAX_IPC_BODY),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn read_rejects_zero_length_frame() {
        let buf: Vec<u8> = 0u32.to_be_bytes().to_vec();
        let mut reader = &buf[..];
        let err = read_frame::<_, IpcRequest>(&mut reader).await.unwrap_err();
        assert!(matches!(err, CodecError::EmptyFrame), "got {err:?}");
    }

    #[tokio::test]
    async fn read_rejects_malformed_cbor() {
        let mut buf: Vec<u8> = Vec::new();
        let bad_body = vec![0xFF_u8; 8];
        let len = u32::try_from(bad_body.len()).unwrap();
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&bad_body);
        let mut reader = &buf[..];
        let err = read_frame::<_, IpcResponse>(&mut reader).await.unwrap_err();
        assert!(matches!(err, CodecError::Cbor(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn write_rejects_oversize_body() {
        let big = "x".repeat((MAX_IPC_BODY + 1) as usize);
        let req = IpcRequest::Execute(Command::AddContact { invite_url: big });
        let mut buf: Vec<u8> = Vec::new();
        let err = write_frame(&mut buf, &req).await.unwrap_err();
        assert!(matches!(err, CodecError::FrameTooLarge { .. }), "got {err:?}");
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-core --lib daemon::ipc::codec
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: five tests green.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/ipc/codec.rs crates/core/src/daemon/ipc/mod.rs
git commit -m "$(cat <<'EOF'
daemon: ipc::codec — length-prefix + CBOR frame codec

u32_be prefix, 1 MiB body cap, zero-length frames rejected,
malformed CBOR surfaced as CodecError::Cbor. Used by both the IPC
server and client halves.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: IPC server — bind, socket perms, peer-cred helper

**Files:**
- Create: `crates/core/src/daemon/ipc/server.rs`
- Modify: `crates/core/src/daemon/ipc/mod.rs` (add `pub mod server;`)

- [ ] **Step 1: Write the failing test**

Create `crates/core/src/daemon/ipc/server.rs` with helper function signatures + tests only:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! IPC server half. Binds a Unix socket with `0600` mode and a `0700`
//! parent directory, peer-cred checks every accepted connection, and
//! hands each off to a per-connection task.

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn check_peer_uid_accepts_matching_uid() {
        assert!(check_peer_uid(Some(1000), 1000).is_ok());
    }

    #[test]
    fn check_peer_uid_rejects_mismatched_uid() {
        assert!(check_peer_uid(Some(999), 1000).is_err());
    }

    #[test]
    fn check_peer_uid_rejects_missing_uid() {
        assert!(check_peer_uid(None, 1000).is_err());
    }

    #[tokio::test]
    async fn bind_sets_socket_mode_0600_and_parent_0700() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("skattr").join("daemon.sock");
        let server = Server::bind(&sock, 1000).unwrap();

        let sock_mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(sock_mode, 0o600, "socket mode must be 0600; got {sock_mode:o}");

        let parent_mode = std::fs::metadata(sock.parent().unwrap()).unwrap().permissions().mode() & 0o777;
        assert_eq!(parent_mode, 0o700, "parent mode must be 0700; got {parent_mode:o}");

        drop(server);
        // Socket file removed on drop.
        assert!(!sock.exists(), "socket file must be unlinked on drop");
    }

    #[tokio::test]
    async fn bind_unlinks_stale_socket_file() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("daemon.sock");
        // Pre-create a stale socket file.
        std::fs::write(&sock, b"stale").unwrap();
        assert!(sock.exists());
        let server = Server::bind(&sock, 1000).unwrap();
        // Bind succeeded; socket now a real Unix listener.
        drop(server);
    }
}
```

Append to `crates/core/src/daemon/ipc/mod.rs`:

```rust
pub mod server;
```

Add `tempfile` as a dev-dependency in `crates/core/Cargo.toml` if not already present:

```bash
grep -q '^tempfile' crates/core/Cargo.toml || cargo add --dev -p skattr-core tempfile
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p skattr-core --lib daemon::ipc::server -- --nocapture
```
Expected: compile FAILS — `check_peer_uid`, `Server::bind` missing.

- [ ] **Step 3: Implement the bind + peer-cred helper**

Replace `crates/core/src/daemon/ipc/server.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! IPC server half. Binds a Unix socket with `0600` mode and a `0700`
//! parent directory, peer-cred checks every accepted connection, and
//! hands each off to a per-connection task.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tokio::net::UnixListener;

use crate::daemon::ipc::wire::IpcError;
use crate::error::{CoreError, Result};

/// Server bound to a local Unix socket.
pub struct Server {
    listener: UnixListener,
    path: PathBuf,
    allowed_uid: u32,
}

impl Server {
    /// Bind a `Server` at `path`. Creates parents with mode `0700`,
    /// unlinks any stale file at `path`, then binds and chmods the
    /// socket to mode `0600`. `allowed_uid` is the UID that every
    /// accepted connection's peer-cred must match.
    pub fn bind(path: &Path, allowed_uid: u32) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(CoreError::Io)?;
            let mut perms = std::fs::metadata(parent).map_err(CoreError::Io)?.permissions();
            perms.set_mode(0o700);
            std::fs::set_permissions(parent, perms).map_err(CoreError::Io)?;
        }
        // Remove stale file (a crashed prior daemon). Ignore errors if
        // it didn't exist.
        let _ = std::fs::remove_file(path);

        let listener = UnixListener::bind(path).map_err(CoreError::Io)?;

        // Tighten the socket file to 0600 immediately after bind.
        let mut perms = std::fs::metadata(path).map_err(CoreError::Io)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(path, perms).map_err(CoreError::Io)?;

        Ok(Self { listener, path: path.to_path_buf(), allowed_uid })
    }

    /// Path the socket file is bound to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Wait for the next incoming connection. Returns the accepted
    /// stream only if its peer-cred UID matches `allowed_uid`; else
    /// closes immediately and returns `Err(IpcError::AuthDenied)`.
    pub async fn accept_one(&self) -> std::result::Result<tokio::net::UnixStream, IpcError> {
        let (stream, _) = self.listener.accept().await.map_err(|e| {
            IpcError::Internal(format!("accept: {e}"))
        })?;
        let cred = stream.peer_cred().map_err(|e| {
            IpcError::Internal(format!("peer_cred: {e}"))
        })?;
        check_peer_uid(Some(cred.uid()), self.allowed_uid).map_err(|_| IpcError::AuthDenied)?;
        Ok(stream)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Best-effort unlink. Errors are ignored (log-worthy but not
        // fatal); the OS will reap the file on logout if we miss it.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Check that `peer_uid` matches `expected`. Unit-testable in isolation
/// from the `UnixStream` accept path.
pub(crate) fn check_peer_uid(peer_uid: Option<u32>, expected: u32) -> io::Result<()> {
    match peer_uid {
        Some(uid) if uid == expected => Ok(()),
        Some(uid) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("peer uid {uid} != expected {expected}"),
        )),
        None => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "peer uid unavailable",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn check_peer_uid_accepts_matching_uid() {
        assert!(check_peer_uid(Some(1000), 1000).is_ok());
    }

    #[test]
    fn check_peer_uid_rejects_mismatched_uid() {
        assert!(check_peer_uid(Some(999), 1000).is_err());
    }

    #[test]
    fn check_peer_uid_rejects_missing_uid() {
        assert!(check_peer_uid(None, 1000).is_err());
    }

    #[tokio::test]
    async fn bind_sets_socket_mode_0600_and_parent_0700() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("skattr").join("daemon.sock");
        let server = Server::bind(&sock, 1000).unwrap();

        let sock_mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(sock_mode, 0o600, "socket mode must be 0600; got {sock_mode:o}");

        let parent_mode = std::fs::metadata(sock.parent().unwrap()).unwrap().permissions().mode() & 0o777;
        assert_eq!(parent_mode, 0o700, "parent mode must be 0700; got {parent_mode:o}");

        drop(server);
        assert!(!sock.exists(), "socket file must be unlinked on drop");
    }

    #[tokio::test]
    async fn bind_unlinks_stale_socket_file() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("daemon.sock");
        std::fs::write(&sock, b"stale").unwrap();
        assert!(sock.exists());
        let server = Server::bind(&sock, 1000).unwrap();
        drop(server);
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-core --lib daemon::ipc::server
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: five tests green, no clippy warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/ipc/server.rs crates/core/src/daemon/ipc/mod.rs crates/core/Cargo.toml Cargo.lock
git commit -m "$(cat <<'EOF'
daemon: ipc::server — bind (0600 + 0700 parent) + peer-cred helper

Server::bind enforces socket mode 0600 and parent 0700, unlinks stale
sockets, and unlinks the socket on drop. check_peer_uid is split into
a pure helper so accept-loop tests can exercise the auth path without
spawning a real client. Accept-loop + per-conn handler land in Task 10.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: IPC server — per-connection state machine and accept loop

The per-connection handler implements the spec's state machine (§4 of the design spec): read one `IpcRequest`, dispatch, maybe subscribe, maybe more `Execute`s, terminate with `Bye`. Dispatch is delegated through a small trait so the unit test can drive it without a full `DaemonHandle`.

**Files:**
- Modify: `crates/core/src/daemon/ipc/server.rs` (append `CommandExecutor` trait, `handle_connection`, `serve` loop)
- Create: `crates/core/src/daemon/ipc/server_tests.rs` integration-style test using `tokio::io::duplex` (inline the module to avoid new file)

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block in `crates/core/src/daemon/ipc/server.rs`:

```rust
    use crate::daemon::commands::{Command, CommandResult};
    use crate::daemon::events::{Event, TorStatus};
    use crate::daemon::ipc::codec::{read_frame, write_frame};
    use crate::daemon::ipc::wire::{EventFilter, IpcError, IpcRequest, IpcResponse};
    use async_trait::async_trait;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    struct EchoExec;
    #[async_trait]
    impl CommandExecutor for EchoExec {
        async fn execute(&self, cmd: Command) -> std::result::Result<CommandResult, IpcError> {
            match cmd {
                Command::ListContacts => Ok(CommandResult::Ok),
                Command::Shutdown => Ok(CommandResult::Ok),
                _ => Err(IpcError::UnknownCommand),
            }
        }
    }

    #[tokio::test]
    async fn per_conn_execute_returns_ok_and_bye() {
        let (mut client, server_stream) = tokio::io::duplex(1024 * 1024);
        let exec: Arc<dyn CommandExecutor> = Arc::new(EchoExec);
        let (events_tx, _) = broadcast::channel::<Event>(16);

        let handle_task =
            tokio::spawn(handle_connection(server_stream, exec, events_tx));

        write_frame(&mut client, &IpcRequest::Execute(Command::ListContacts))
            .await
            .unwrap();

        let ok: IpcResponse = read_frame(&mut client).await.unwrap();
        assert!(matches!(ok, IpcResponse::Ok(CommandResult::Ok)));
        let bye: IpcResponse = read_frame(&mut client).await.unwrap();
        assert!(matches!(bye, IpcResponse::Bye));

        handle_task.await.unwrap();
    }

    #[tokio::test]
    async fn per_conn_subscribe_forwards_events_then_execute_still_works() {
        let (mut client, server_stream) = tokio::io::duplex(1024 * 1024);
        let exec: Arc<dyn CommandExecutor> = Arc::new(EchoExec);
        let (events_tx, _) = broadcast::channel::<Event>(16);

        let events_tx_clone = events_tx.clone();
        let handle_task =
            tokio::spawn(handle_connection(server_stream, exec, events_tx_clone));

        // Subscribe -> Ok(Subscribed).
        write_frame(&mut client, &IpcRequest::Subscribe(EventFilter::TorStatus))
            .await
            .unwrap();
        match read_frame::<_, IpcResponse>(&mut client).await.unwrap() {
            IpcResponse::Ok(CommandResult::Subscribed) => {}
            other => panic!("expected Ok(Subscribed), got {other:?}"),
        }

        // Publish a matching event; subscriber should receive it.
        let _ = events_tx.send(Event::TorStatusChanged(TorStatus::Ready));
        match read_frame::<_, IpcResponse>(&mut client).await.unwrap() {
            IpcResponse::Event(Event::TorStatusChanged(TorStatus::Ready)) => {}
            other => panic!("expected Event(TorStatus::Ready), got {other:?}"),
        }

        // Execute after Subscribe on the same connection.
        write_frame(&mut client, &IpcRequest::Execute(Command::ListContacts))
            .await
            .unwrap();
        match read_frame::<_, IpcResponse>(&mut client).await.unwrap() {
            IpcResponse::Ok(CommandResult::Ok) => {}
            other => panic!("expected Ok, got {other:?}"),
        }

        // Hang up; server should exit cleanly.
        drop(client);
        handle_task.await.unwrap();
    }

    #[tokio::test]
    async fn per_conn_unknown_command_returns_err_but_keeps_connection() {
        let (mut client, server_stream) = tokio::io::duplex(1024 * 1024);
        let exec: Arc<dyn CommandExecutor> = Arc::new(EchoExec);
        let (events_tx, _) = broadcast::channel::<Event>(16);

        let handle_task =
            tokio::spawn(handle_connection(server_stream, exec, events_tx));

        write_frame(
            &mut client,
            &IpcRequest::Execute(Command::CreateGroup { members: vec![], name: "x".into() }),
        )
        .await
        .unwrap();

        match read_frame::<_, IpcResponse>(&mut client).await.unwrap() {
            IpcResponse::Err(IpcError::UnknownCommand) => {}
            other => panic!("expected Err(UnknownCommand), got {other:?}"),
        }
        // Connection closed afterwards (Bye).
        match read_frame::<_, IpcResponse>(&mut client).await.unwrap() {
            IpcResponse::Bye => {}
            other => panic!("expected Bye, got {other:?}"),
        }

        handle_task.await.unwrap();
    }
```

Add `async-trait` as a dep in `crates/core/Cargo.toml` if not present:

```bash
grep -q '^async-trait' crates/core/Cargo.toml || cargo add -p skattr-core async-trait
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p skattr-core --lib daemon::ipc::server -- --nocapture
```
Expected: compile FAILS — `CommandExecutor` trait and `handle_connection` fn don't exist.

- [ ] **Step 3: Implement `CommandExecutor` + `handle_connection` + `serve`**

Append the following to `crates/core/src/daemon/ipc/server.rs` (outside the test module):

```rust
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::broadcast;

use crate::daemon::commands::{Command, CommandResult};
use crate::daemon::events::Event;
use crate::daemon::ipc::codec::{read_frame, write_frame, CodecError};
use crate::daemon::ipc::wire::{EventFilter, IpcRequest, IpcResponse};

/// Execute one `Command` and return its `CommandResult` or a typed
/// `IpcError`. Decouples the per-connection handler from the concrete
/// `DaemonHandle` so the unit tests can drive the handler with a mock.
#[async_trait::async_trait]
pub trait CommandExecutor: Send + Sync {
    /// Dispatch `cmd` and return a result or typed wire error.
    async fn execute(&self, cmd: Command) -> std::result::Result<CommandResult, IpcError>;
}

/// Handle one accepted connection. The loop owns a per-connection
/// `subscribed: Option<EventFilter>`; once set, events flow until the
/// client hangs up or a `Shutdown` arrives.
pub async fn handle_connection<S>(
    mut stream: S,
    executor: Arc<dyn CommandExecutor>,
    events_tx: broadcast::Sender<Event>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut events_rx: Option<broadcast::Receiver<Event>> = None;
    let mut subscribed: Option<EventFilter> = None;

    loop {
        // Two sources: inbound request, or a pending event on the
        // subscription. Use select to avoid blocking on a quiet client
        // once subscribed.
        let request_result: std::result::Result<IpcRequest, CodecError> = tokio::select! {
            r = read_frame::<_, IpcRequest>(&mut stream) => r,
            maybe_event = receive_if_some(events_rx.as_mut()) => {
                match maybe_event {
                    Some(ev) if event_matches(&ev, subscribed.as_ref()) => {
                        if write_frame(&mut stream, &IpcResponse::Event(ev)).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    Some(_) => continue, // filtered out
                    None => {
                        // lagged: reset and keep going
                        if let Some(filter) = subscribed.clone() {
                            events_rx = Some(events_tx.subscribe());
                            tracing::warn!(?filter, "ipc subscriber lagged; resubscribed");
                        }
                        continue;
                    }
                }
            }
        };

        let req = match request_result {
            Ok(r) => r,
            Err(CodecError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(CodecError::Cbor(s)) => {
                let _ = write_frame(&mut stream, &IpcResponse::Err(IpcError::Codec(s))).await;
                continue;
            }
            Err(CodecError::FrameTooLarge { got, max }) => {
                let _ = write_frame(
                    &mut stream,
                    &IpcResponse::Err(IpcError::FrameTooLarge { got, max }),
                )
                .await;
                break;
            }
            Err(CodecError::EmptyFrame) => {
                let _ = write_frame(
                    &mut stream,
                    &IpcResponse::Err(IpcError::Codec("empty frame".into())),
                )
                .await;
                break;
            }
            Err(_) => break,
        };

        match req {
            IpcRequest::Execute(cmd) => {
                let resp = match executor.execute(cmd).await {
                    Ok(result) => IpcResponse::Ok(result),
                    Err(e) => IpcResponse::Err(e),
                };
                let is_terminal = matches!(resp, IpcResponse::Err(IpcError::UnknownCommand));
                if write_frame(&mut stream, &resp).await.is_err() {
                    break;
                }
                if is_terminal {
                    break;
                }
            }
            IpcRequest::Subscribe(filter) => {
                subscribed = Some(filter);
                events_rx = Some(events_tx.subscribe());
                if write_frame(&mut stream, &IpcResponse::Ok(CommandResult::Subscribed))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            IpcRequest::Shutdown => {
                let _ = write_frame(&mut stream, &IpcResponse::Ok(CommandResult::Ok)).await;
                break;
            }
        }
    }

    // Terminal frame. Ignore write errors — the peer may already be gone.
    let _ = write_frame(&mut stream, &IpcResponse::Bye).await;
}

/// Accept loop. Spawns [`handle_connection`] per accepted stream.
/// Terminates when `shutdown` future completes.
pub async fn serve(
    server: Server,
    executor: Arc<dyn CommandExecutor>,
    events_tx: broadcast::Sender<Event>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) {
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => {
                break;
            }
            accepted = server.accept_one() => {
                match accepted {
                    Ok(stream) => {
                        let exec = executor.clone();
                        let evs = events_tx.clone();
                        tokio::spawn(async move {
                            handle_connection(stream, exec, evs).await;
                        });
                    }
                    Err(IpcError::AuthDenied) => {
                        tracing::warn!("ipc: rejected connection: peer uid mismatch");
                    }
                    Err(e) => {
                        tracing::warn!(?e, "ipc: accept error");
                    }
                }
            }
        }
    }
}

async fn receive_if_some(rx: Option<&mut broadcast::Receiver<Event>>) -> Option<Event> {
    match rx {
        Some(r) => match r.recv().await {
            Ok(ev) => Some(ev),
            Err(broadcast::error::RecvError::Lagged(_)) => None,
            Err(broadcast::error::RecvError::Closed) => None,
        },
        None => std::future::pending().await,
    }
}

fn event_matches(event: &Event, filter: Option<&EventFilter>) -> bool {
    let Some(filter) = filter else { return false };
    match (filter, event) {
        (EventFilter::All, _) => true,
        (EventFilter::TorStatus, Event::TorStatusChanged(_)) => true,
        (EventFilter::Contact(peer), Event::MessageReceived { from, .. }) => from == peer,
        (EventFilter::Contact(peer), Event::DeliveryStatusChanged { .. }) => {
            // DeliveryStatusChanged doesn't carry the peer; forward all
            // for now. Phase 1.F spec accepts this imprecision — the
            // CLI filters further by message_id.
            let _ = peer;
            true
        }
        (EventFilter::Contact(_), Event::ContactUpdated(_)) => true,
        _ => false,
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-core --lib daemon::ipc::server
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: all eight tests green.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/ipc/server.rs crates/core/Cargo.toml Cargo.lock
git commit -m "$(cat <<'EOF'
daemon: ipc::server — per-conn state machine + accept loop

handle_connection implements Execute / Subscribe / Shutdown with a
per-connection subscribed filter that coexists with later Executes on
the same socket (powers the `chat` command). UnknownCommand closes the
connection after reporting the error; Codec/FrameTooLarge are
surfaced and then closed. CommandExecutor trait decouples dispatch
from the full DaemonHandle so the unit tests can drive the handler
with a mock.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: IPC client (`daemon::ipc::client`)

**Files:**
- Create: `crates/core/src/daemon/ipc/client.rs`
- Modify: `crates/core/src/daemon/ipc/mod.rs` (add `pub mod client; pub use client::IpcClient;`)
- Modify: `crates/core/src/daemon/mod.rs` (re-export `IpcClient` publicly)

- [ ] **Step 1: Write the failing test**

Create `crates/core/src/daemon/ipc/client.rs` with test-only contents:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! IPC client half. Used by the CLI to connect, send one `Command`,
//! collect the result, and optionally stream `Event`s.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::commands::{Command, CommandResult};
    use crate::daemon::events::Event;
    use crate::daemon::ipc::server::{handle_connection, CommandExecutor};
    use crate::daemon::ipc::wire::{EventFilter, IpcError};
    use async_trait::async_trait;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    struct OkExec;
    #[async_trait]
    impl CommandExecutor for OkExec {
        async fn execute(&self, _cmd: Command) -> std::result::Result<CommandResult, IpcError> {
            Ok(CommandResult::Ok)
        }
    }

    #[tokio::test]
    async fn execute_roundtrip_over_duplex() {
        let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
        let (events_tx, _) = broadcast::channel::<Event>(16);
        let exec: Arc<dyn CommandExecutor> = Arc::new(OkExec);
        tokio::spawn(handle_connection(server_io, exec, events_tx));

        let mut client = IpcClient::from_stream(client_io);
        let got = client.execute(Command::ListContacts).await.unwrap();
        assert!(matches!(got, CommandResult::Ok));
    }

    #[tokio::test]
    async fn connect_missing_socket_returns_daemon_not_running() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("no-such-socket");
        let err = IpcClient::connect(&missing).await.unwrap_err();
        assert!(
            matches!(err, IpcClientError::DaemonNotRunning),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn subscribe_streams_events() {
        let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
        let (events_tx, _) = broadcast::channel::<Event>(16);
        let exec: Arc<dyn CommandExecutor> = Arc::new(OkExec);
        let events_tx_clone = events_tx.clone();
        tokio::spawn(handle_connection(server_io, exec, events_tx_clone));

        let mut client = IpcClient::from_stream(client_io);
        client.subscribe(EventFilter::All).await.unwrap();

        let _ = events_tx.send(Event::ContactUpdated(crate::identity::PublicKey([5; 32])));
        let ev = client.next_event().await.unwrap();
        assert!(matches!(ev, Event::ContactUpdated(_)));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p skattr-core --lib daemon::ipc::client -- --nocapture
```
Expected: compile FAILS — `IpcClient`, `IpcClientError`, methods missing.

- [ ] **Step 3: Implement `IpcClient`**

Replace the contents of `crates/core/src/daemon/ipc/client.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! IPC client half. Used by the CLI to connect, send one `Command`,
//! collect the result, and optionally stream `Event`s.

use std::io;
use std::path::Path;

use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::net::UnixStream;

use crate::daemon::commands::{Command, CommandResult};
use crate::daemon::events::Event;
use crate::daemon::ipc::codec::{read_frame, write_frame, CodecError};
use crate::daemon::ipc::wire::{EventFilter, IpcError, IpcRequest, IpcResponse};

/// Client-side error surface. Distinct from the wire-level
/// [`IpcError`] so a "socket missing" is not conflated with a typed
/// daemon error.
#[derive(Debug, Error)]
pub enum IpcClientError {
    /// Could not connect; socket file missing or connection refused.
    #[error("daemon not running")]
    DaemonNotRunning,
    /// I/O failure after a successful connect.
    #[error("ipc io: {0}")]
    Io(#[from] io::Error),
    /// Frame codec failure.
    #[error("ipc codec: {0}")]
    Codec(String),
    /// The daemon answered with a typed error.
    #[error("ipc server error: {0:?}")]
    Server(IpcError),
    /// Expected `Event` frame but received something else.
    #[error("ipc protocol: expected Event, got {0:?}")]
    UnexpectedFrame(IpcResponse),
}

impl From<CodecError> for IpcClientError {
    fn from(e: CodecError) -> Self {
        match e {
            CodecError::Io(i) => IpcClientError::Io(i),
            other => IpcClientError::Codec(other.to_string()),
        }
    }
}

/// Generic over the underlying duplex stream so unit tests can use
/// `tokio::io::duplex` instead of a real `UnixStream`.
pub struct IpcClient<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    stream: BufReader<S>,
    subscribed: bool,
}

impl IpcClient<UnixStream> {
    /// Connect to the daemon at `path`. Maps a missing socket file or
    /// connection-refused to [`IpcClientError::DaemonNotRunning`].
    pub async fn connect(path: &Path) -> std::result::Result<Self, IpcClientError> {
        match UnixStream::connect(path).await {
            Ok(stream) => Ok(Self::from_stream(stream)),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                Err(IpcClientError::DaemonNotRunning)
            }
            Err(e) => Err(IpcClientError::Io(e)),
        }
    }
}

impl<S> IpcClient<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Build a client from an existing duplex stream. Used by tests
    /// (`tokio::io::duplex`) and the integration test harness.
    pub fn from_stream(stream: S) -> Self {
        Self { stream: BufReader::new(stream), subscribed: false }
    }

    /// Send a one-shot `Execute` request and return the single
    /// `CommandResult`. Consumes the `Bye` frame on the way out.
    pub async fn execute(&mut self, cmd: Command) -> std::result::Result<CommandResult, IpcClientError> {
        write_frame(&mut self.stream, &IpcRequest::Execute(cmd)).await?;
        let resp = read_frame::<_, IpcResponse>(&mut self.stream).await?;
        let result = match resp {
            IpcResponse::Ok(r) => Ok(r),
            IpcResponse::Err(e) => Err(IpcClientError::Server(e)),
            other => Err(IpcClientError::UnexpectedFrame(other)),
        };
        // Drain the `Bye` best-effort; don't fail the call if the
        // server already hung up.
        let _ = read_frame::<_, IpcResponse>(&mut self.stream).await;
        result
    }

    /// Start an event subscription. After this returns Ok, call
    /// [`IpcClient::next_event`] in a loop.
    pub async fn subscribe(&mut self, filter: EventFilter) -> std::result::Result<(), IpcClientError> {
        write_frame(&mut self.stream, &IpcRequest::Subscribe(filter)).await?;
        match read_frame::<_, IpcResponse>(&mut self.stream).await? {
            IpcResponse::Ok(CommandResult::Subscribed) => {
                self.subscribed = true;
                Ok(())
            }
            IpcResponse::Err(e) => Err(IpcClientError::Server(e)),
            other => Err(IpcClientError::UnexpectedFrame(other)),
        }
    }

    /// Read the next event from the server. Only call after
    /// [`IpcClient::subscribe`]. Returns an error if the server sent
    /// anything other than `Event(..)`.
    pub async fn next_event(&mut self) -> std::result::Result<Event, IpcClientError> {
        if !self.subscribed {
            return Err(IpcClientError::UnexpectedFrame(IpcResponse::Bye));
        }
        match read_frame::<_, IpcResponse>(&mut self.stream).await? {
            IpcResponse::Event(e) => Ok(e),
            IpcResponse::Bye => Err(IpcClientError::UnexpectedFrame(IpcResponse::Bye)),
            other => Err(IpcClientError::UnexpectedFrame(other)),
        }
    }
}
```

Append to `crates/core/src/daemon/ipc/mod.rs`:

```rust
pub use client::{IpcClient, IpcClientError};
pub mod client;
```

Append to `crates/core/src/daemon/mod.rs`:

```rust
pub use ipc::{IpcClient, IpcClientError};
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-core --lib daemon::ipc::client
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: all three tests green.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/ipc/client.rs crates/core/src/daemon/ipc/mod.rs crates/core/src/daemon/mod.rs
git commit -m "$(cat <<'EOF'
daemon: ipc::client — IpcClient for CLI use

connect() maps NotFound / ConnectionRefused to DaemonNotRunning
(CLI exit code 3). execute() is one-shot Execute -> Ok/Err + swallow
Bye. subscribe() + next_event() support tail --follow and chat.
Generic over the underlying duplex stream so unit tests use
tokio::io::duplex instead of a real UnixStream.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: `DaemonHandle` — the command-handler subsystem grouping

**Files:**
- Create: `crates/core/src/daemon/handle.rs`
- Modify: `crates/core/src/daemon/mod.rs` (add `pub(crate) mod handle;`)

- [ ] **Step 1: Write the failing test**

Create `crates/core/src/daemon/handle.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! `DaemonHandle` groups the subsystems every command handler needs.
//!
//! Held by `Arc` inside `Daemon::run`, shared into the IPC server's
//! per-connection tasks. Deliberately skeletal: each field is a single
//! owned handle, no nested `Option`s, no lazy initialisation.

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    use crate::daemon::events::Event;
    use crate::delivery::DeliveryHub;
    use crate::identity::{IdentityKey, Seed};
    use crate::storage::Pool;

    #[test]
    fn constructs_with_mock_subsystems() {
        let seed = Seed::generate().unwrap();
        let identity = IdentityKey::from_seed(&seed).unwrap();
        let pool = Arc::new(Pool::in_memory());
        let hub: Arc<DeliveryHub<tokio::io::DuplexStream>> =
            Arc::new(DeliveryHub::new(pool.clone()));
        let (events_tx, _) = broadcast::channel::<Event>(16);

        let handle = DaemonHandle::<tokio::io::DuplexStream>::new(
            pool.clone(),
            hub.clone(),
            identity,
            events_tx.clone(),
        );

        assert!(Arc::ptr_eq(&handle.pool, &pool));
        assert!(Arc::ptr_eq(&handle.hub, &hub));
    }
}
```

Append to `crates/core/src/daemon/mod.rs`:

```rust
pub(crate) mod handle;
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p skattr-core --lib daemon::handle -- --nocapture
```
Expected: compile FAILS — `DaemonHandle` missing.

- [ ] **Step 3: Implement `DaemonHandle`**

Replace the contents of `crates/core/src/daemon/handle.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! `DaemonHandle` groups the subsystems every command handler needs.

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::broadcast;

use crate::daemon::events::Event;
use crate::delivery::DeliveryHub;
use crate::identity::IdentityKey;
use crate::storage::Pool;

/// Shared handle to the long-lived daemon subsystems. Generic over the
/// transport stream type so the integration tests can instantiate one
/// over `tokio::io::DuplexStream` and the real daemon over a
/// Tor-anchored listener's stream type.
pub struct DaemonHandle<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Encrypted SQLite pool.
    pub pool: Arc<Pool>,
    /// Per-daemon delivery router.
    pub hub: Arc<DeliveryHub<S>>,
    /// Local Ed25519 identity (used for signing ContactCards + invites).
    pub identity: IdentityKey,
    /// Event broadcast sender. Subscribers (IPC connections, tests)
    /// get a `Receiver` via `.subscribe()`.
    pub events_tx: broadcast::Sender<Event>,
}

impl<S> DaemonHandle<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Construct a handle from the four owned subsystems.
    #[must_use]
    pub fn new(
        pool: Arc<Pool>,
        hub: Arc<DeliveryHub<S>>,
        identity: IdentityKey,
        events_tx: broadcast::Sender<Event>,
    ) -> Self {
        Self { pool, hub, identity, events_tx }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Seed;

    #[test]
    fn constructs_with_mock_subsystems() {
        let seed = Seed::generate().unwrap();
        let identity = IdentityKey::from_seed(&seed).unwrap();
        let pool = Arc::new(Pool::in_memory());
        let hub: Arc<DeliveryHub<tokio::io::DuplexStream>> =
            Arc::new(DeliveryHub::new(pool.clone()));
        let (events_tx, _) = broadcast::channel::<Event>(16);

        let handle = DaemonHandle::<tokio::io::DuplexStream>::new(
            pool.clone(),
            hub.clone(),
            identity,
            events_tx.clone(),
        );

        assert!(Arc::ptr_eq(&handle.pool, &pool));
        assert!(Arc::ptr_eq(&handle.hub, &hub));
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-core --lib daemon::handle
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: test green.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/handle.rs crates/core/src/daemon/mod.rs
git commit -m "$(cat <<'EOF'
daemon: DaemonHandle — pool + hub + identity + events_tx grouping

Generic over the transport stream type so the mocked-transport
integration tests use DuplexStream and the real daemon uses the
Tor-anchored stream type. Intentionally skeletal: no lazy init, no
nested Options.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 13: Dispatch skeleton + `Shutdown` + `CommandExecutor` impl

Establishes `dispatch::execute_command` and wires `DaemonHandle` to the IPC server's `CommandExecutor` trait. Every other `Command` variant returns `IpcError::UnknownCommand` for now; subsequent tasks fill them in.

**Files:**
- Create: `crates/core/src/daemon/dispatch.rs`
- Modify: `crates/core/src/daemon/mod.rs` (add `pub(crate) mod dispatch;`)

- [ ] **Step 1: Write the failing test**

Create `crates/core/src/daemon/dispatch.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Command dispatch: one function per `Command` variant, consuming a
//! `DaemonHandle` + the command and returning a typed result / error.

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    use crate::daemon::commands::{Command, CommandResult};
    use crate::daemon::events::Event;
    use crate::daemon::handle::DaemonHandle;
    use crate::daemon::ipc::wire::IpcError;
    use crate::delivery::DeliveryHub;
    use crate::identity::{IdentityKey, Seed};
    use crate::storage::Pool;

    fn test_handle() -> Arc<DaemonHandle<tokio::io::DuplexStream>> {
        let seed = Seed::generate().unwrap();
        let identity = IdentityKey::from_seed(&seed).unwrap();
        let pool = Arc::new(Pool::in_memory());
        crate::storage::migrations::apply(
            &mut pool.with_mut(|_| Ok::<_, crate::error::CoreError>(())).map(|_| ()).unwrap_or_default().into()
        );
        let hub: Arc<DeliveryHub<tokio::io::DuplexStream>> =
            Arc::new(DeliveryHub::new(pool.clone()));
        let (events_tx, _) = broadcast::channel::<Event>(16);
        Arc::new(DaemonHandle::new(pool, hub, identity, events_tx))
    }

    #[tokio::test]
    async fn shutdown_returns_ok() {
        let handle = test_handle();
        let result = execute_command(handle, Command::Shutdown).await;
        assert!(matches!(result, Ok(CommandResult::Ok)));
    }

    #[tokio::test]
    async fn create_group_returns_unknown_command() {
        let handle = test_handle();
        let result = execute_command(
            handle,
            Command::CreateGroup { members: vec![], name: "x".into() },
        )
        .await;
        assert!(matches!(result, Err(IpcError::UnknownCommand)));
    }
}
```

The `test_handle()` body as written is overcomplicated; the real migration API is `apply(&mut Connection)`. Replace the `pool` construction with a proper migrated pool before Step 3 (see Step 3 code).

Append to `crates/core/src/daemon/mod.rs`:

```rust
pub(crate) mod dispatch;
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p skattr-core --lib daemon::dispatch -- --nocapture
```
Expected: compile FAILS — `execute_command` missing.

- [ ] **Step 3: Implement the dispatch skeleton**

Replace `crates/core/src/daemon/dispatch.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Command dispatch: one function per `Command` variant, consuming a
//! `DaemonHandle` + the command and returning a typed result / error.

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::daemon::commands::{Command, CommandResult};
use crate::daemon::error_kind::DaemonErrorKind;
use crate::daemon::handle::DaemonHandle;
use crate::daemon::ipc::wire::IpcError;
use crate::error::CoreError;

/// Execute one command against `handle`. Every per-variant handler
/// lives in this module (small private fns); we keep them colocated so
/// a reader can see the whole command surface in one file.
pub async fn execute_command<S>(
    handle: Arc<DaemonHandle<S>>,
    cmd: Command,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    match cmd {
        Command::Shutdown | Command::RotateOnion => Ok(CommandResult::Ok),
        Command::ListContacts => list_contacts(&handle).await,
        Command::CreateInvite { .. } => Err(IpcError::UnknownCommand),
        Command::AddContact { .. } => Err(IpcError::UnknownCommand),
        Command::SendMessage { .. } => Err(IpcError::UnknownCommand),
        Command::RecentMessages { .. } => Err(IpcError::UnknownCommand),
        Command::CreateGroup { .. } => Err(IpcError::UnknownCommand),
    }
}

/// Map any `CoreError` to an `IpcError`. Projects via `CoreError::kind`
/// into `DaemonErrorKind`; otherwise `Internal(...)` with a truncated
/// display. Logs the full error server-side.
pub(crate) fn map_err(err: CoreError) -> IpcError {
    if let Some(kind) = err.kind() {
        tracing::warn!(?err, ?kind, "ipc: typed daemon error");
        IpcError::Daemon(kind)
    } else {
        let msg = format!("{err}");
        let truncated: String = msg.chars().take(256).collect();
        tracing::warn!(?err, "ipc: internal error");
        IpcError::Internal(truncated)
    }
}

/// Unused-import prevention for the error-kind crate until a handler
/// needs it. Removed in Task 14 when `list_contacts` populates.
#[allow(dead_code)]
fn _unused_error_kind() -> DaemonErrorKind {
    DaemonErrorKind::StorageError
}

async fn list_contacts<S>(
    _handle: &Arc<DaemonHandle<S>>,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // Task 14 fills this in.
    Err(IpcError::UnknownCommand)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    use crate::daemon::events::Event;
    use crate::delivery::DeliveryHub;
    use crate::identity::{IdentityKey, Seed};
    use crate::storage::Pool;

    fn test_handle() -> Arc<DaemonHandle<tokio::io::DuplexStream>> {
        let seed = Seed::generate().unwrap();
        let identity = IdentityKey::from_seed(&seed).unwrap();
        let pool = Arc::new(Pool::in_memory());
        // Pool::in_memory() already applies migrations up to current version.
        let hub: Arc<DeliveryHub<tokio::io::DuplexStream>> =
            Arc::new(DeliveryHub::new(pool.clone()));
        let (events_tx, _) = broadcast::channel::<Event>(16);
        Arc::new(DaemonHandle::new(pool, hub, identity, events_tx))
    }

    #[tokio::test]
    async fn shutdown_returns_ok() {
        let handle = test_handle();
        let result = execute_command(handle, Command::Shutdown).await;
        assert!(matches!(result, Ok(CommandResult::Ok)));
    }

    #[tokio::test]
    async fn create_group_returns_unknown_command() {
        let handle = test_handle();
        let result = execute_command(
            handle,
            Command::CreateGroup { members: vec![], name: "x".into() },
        )
        .await;
        assert!(matches!(result, Err(IpcError::UnknownCommand)));
    }
}
```

Also add the `CommandExecutor` impl at the bottom of `handle.rs` so `DaemonHandle` satisfies the IPC server trait. Append to `crates/core/src/daemon/handle.rs` (outside the test module):

```rust
use crate::daemon::commands::Command as IpcCommand;
use crate::daemon::commands::CommandResult as IpcCommandResult;
use crate::daemon::ipc::server::CommandExecutor;
use crate::daemon::ipc::wire::IpcError;

#[async_trait::async_trait]
impl<S> CommandExecutor for DaemonHandle<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
    async fn execute(
        &self,
        cmd: IpcCommand,
    ) -> std::result::Result<IpcCommandResult, IpcError> {
        // Clone the Arc identity of `self` into a real Arc. This impl
        // lives on `DaemonHandle<S>` directly rather than
        // `Arc<DaemonHandle<S>>` because CommandExecutor takes `&self`;
        // callers wrap a DaemonHandle in Arc when handing it to the
        // server, and trait dispatch goes through `&*arc`.
        let handle = Arc::new(self.clone_for_dispatch());
        crate::daemon::dispatch::execute_command(handle, cmd).await
    }
}

impl<S> DaemonHandle<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Clone every Arc / broadcast::Sender so a new DaemonHandle can
    /// be wrapped in a fresh Arc for per-command dispatch. Identity is
    /// Clone because IdentityKey is Clone.
    fn clone_for_dispatch(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            hub: self.hub.clone(),
            identity: self.identity.clone(),
            events_tx: self.events_tx.clone(),
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-core --lib daemon::dispatch
cargo test -p skattr-core --lib daemon::handle
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: both modules' tests green.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs crates/core/src/daemon/handle.rs crates/core/src/daemon/mod.rs
git commit -m "$(cat <<'EOF'
daemon: dispatch::execute_command skeleton + DaemonHandle executor

Shutdown/RotateOnion return Ok; every other variant returns
IpcError::UnknownCommand until subsequent tasks fill them in.
DaemonHandle implements CommandExecutor so the IPC server can hand it
a Command and get back a result or typed error without knowing any of
the internals. map_err() is the one place CoreError -> IpcError
projection happens.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 14: Dispatch `ListContacts`

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (fill in `list_contacts`)

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block in `crates/core/src/daemon/dispatch.rs`:

```rust
    use crate::contact::Contact;
    use crate::daemon::commands::ContactSummary;
    use crate::identity::PublicKey;
    use crate::storage::ContactRepo;

    #[tokio::test]
    async fn list_contacts_returns_all_rows_projected() {
        let handle = test_handle();
        // Seed two contacts directly via the repo.
        {
            let repo = ContactRepo::new(&handle.pool);
            repo.upsert(&Contact {
                identity: PublicKey([0x01; 32]),
                display_name: Some("alice".into()),
                added_at: 1_700_000_000,
                card: None,
            })
            .unwrap();
            repo.upsert(&Contact {
                identity: PublicKey([0x02; 32]),
                display_name: None,
                added_at: 1_700_000_100,
                card: None,
            })
            .unwrap();
        }

        let result = execute_command(handle, Command::ListContacts).await.unwrap();
        match result {
            CommandResult::Contacts(summaries) => {
                assert_eq!(summaries.len(), 2);
                let names: Vec<Option<String>> = summaries.iter().map(|s| s.nickname.clone()).collect();
                assert!(names.contains(&Some("alice".into())));
                assert!(names.contains(&None));
                // No card yet -> onion is empty string, version 0.
                for s in &summaries {
                    if s.nickname == Some("alice".into()) {
                        assert_eq!(s.onion, "");
                        assert_eq!(s.card_version, 0);
                    }
                }
            }
            other => panic!("expected Contacts, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-core --lib daemon::dispatch::tests::list_contacts -- --nocapture
```
Expected: FAIL — current handler returns `IpcError::UnknownCommand`.

- [ ] **Step 3: Implement `list_contacts`**

In `crates/core/src/daemon/dispatch.rs`, replace the stub `async fn list_contacts` and also remove the `_unused_error_kind` shim:

```rust
async fn list_contacts<S>(
    handle: &Arc<DaemonHandle<S>>,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::commands::ContactSummary;
    use crate::storage::ContactRepo;

    let repo = ContactRepo::new(&handle.pool);
    let contacts = repo.list().map_err(map_err)?;
    let summaries: Vec<ContactSummary> = contacts
        .into_iter()
        .map(|c| {
            let (onion, card_version) = c
                .card
                .as_ref()
                .map(|card| (card.body.onion.clone(), card.body.version))
                .unwrap_or_else(|| (String::new(), 0));
            ContactSummary {
                pubkey: c.identity,
                nickname: c.display_name,
                onion,
                card_version,
                added_at: u64::try_from(c.added_at).unwrap_or(0),
            }
        })
        .collect();
    Ok(CommandResult::Contacts(summaries))
}
```

Also make the top-level match call through to this function (was already matched to `list_contacts`).

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-core --lib daemon::dispatch
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: green including the new test. `ContactRepo::list` is `pub(crate)` — same crate, allowed.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
daemon: dispatch::list_contacts

Projects ContactRepo::list rows into wire-safe ContactSummary, using
(\"\", 0) for onion / card_version when no ContactCard exists yet
(e.g. a contact that has been added but hasn't exchanged cards).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 15: Dispatch `CreateInvite`

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (add `create_invite`; add a cached onion in `DaemonHandle`)
- Modify: `crates/core/src/daemon/handle.rs` (add `onion: Arc<RwLock<Option<String>>>` so invite handler can read the daemon's published onion)

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block in `crates/core/src/daemon/dispatch.rs`:

```rust
    use crate::daemon::commands::Hex32;
    use crate::mls::MlsProvider;
    use crate::storage::KeyPackageRepo;

    #[tokio::test]
    async fn create_invite_returns_parseable_url_and_marks_keypackage_stored() {
        let handle = test_handle();
        // Set the onion so invite can embed it.
        handle.set_onion("testonion".repeat(8));

        let result = execute_command(
            handle.clone(),
            Command::CreateInvite { nickname: None, ttl_secs: Some(3600) },
        )
        .await
        .unwrap();

        let (url, kpi, expires_at) = match result {
            CommandResult::InviteCreated { url, key_package_id, expires_at } => {
                (url, key_package_id, expires_at)
            }
            other => panic!("expected InviteCreated, got {other:?}"),
        };
        assert!(url.starts_with("skattr://invite/v1#"), "url={url}");
        assert!(expires_at > 0);
        assert_ne!(kpi.0, [0u8; 32]);

        // The URL parses back cleanly.
        let parsed = crate::invite::InviteLink::from_url(&url, 1).unwrap();
        assert_eq!(parsed.body.onion, "testonion".repeat(8));

        // The KeyPackage is recorded in storage (single-use tracking).
        let kp_repo = KeyPackageRepo::new(&handle.pool);
        assert!(kp_repo.get(&kpi.0).unwrap().is_some());
    }
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-core --lib daemon::dispatch::tests::create_invite -- --nocapture
```
Expected: FAIL — `handle.set_onion` and `create_invite` handler don't exist yet.

- [ ] **Step 3: Add cached-onion slot to `DaemonHandle`**

Edit `crates/core/src/daemon/handle.rs`. Change the struct to include an `onion` field and add setter/getter methods:

```rust
use std::sync::RwLock;

/// Shared handle to the long-lived daemon subsystems.
pub struct DaemonHandle<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    pub pool: Arc<Pool>,
    pub hub: Arc<DeliveryHub<S>>,
    pub identity: IdentityKey,
    pub events_tx: broadcast::Sender<Event>,
    /// Published onion address, set by `Daemon::run` once Tor is ready.
    /// `CreateInvite` embeds this; if empty, invite creation fails with
    /// `DaemonErrorKind::TorNotReady`.
    pub onion: Arc<RwLock<Option<String>>>,
}

impl<S> DaemonHandle<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    #[must_use]
    pub fn new(
        pool: Arc<Pool>,
        hub: Arc<DeliveryHub<S>>,
        identity: IdentityKey,
        events_tx: broadcast::Sender<Event>,
    ) -> Self {
        Self {
            pool,
            hub,
            identity,
            events_tx,
            onion: Arc::new(RwLock::new(None)),
        }
    }

    /// Set the published onion address. Called by `Daemon::run` after
    /// `TorRuntime::publish_onion`.
    pub fn set_onion(&self, addr: impl Into<String>) {
        if let Ok(mut guard) = self.onion.write() {
            *guard = Some(addr.into());
        }
    }

    /// Read the current onion address, or `None` if Tor isn't ready.
    #[must_use]
    pub fn onion(&self) -> Option<String> {
        self.onion.read().ok().and_then(|g| g.clone())
    }

    fn clone_for_dispatch(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            hub: self.hub.clone(),
            identity: self.identity.clone(),
            events_tx: self.events_tx.clone(),
            onion: self.onion.clone(),
        }
    }
}
```

- [ ] **Step 4: Implement `create_invite`**

Extend the match arm in `execute_command` and add the handler in `crates/core/src/daemon/dispatch.rs`:

```rust
        Command::CreateInvite { nickname, ttl_secs } => {
            create_invite(&handle, nickname, ttl_secs).await
        }
```

And add this private function next to `list_contacts`:

```rust
async fn create_invite<S>(
    handle: &Arc<DaemonHandle<S>>,
    _nickname: Option<String>,
    ttl_secs: Option<u64>,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::commands::Hex32;
    use crate::invite::InviteLink;
    use crate::mls::{Group, KeyPackage, MlsProvider};
    use crate::storage::KeyPackageRepo;

    let onion = handle.onion().ok_or_else(|| {
        IpcError::Daemon(DaemonErrorKind::TorNotReady)
    })?;

    let ttl = ttl_secs.unwrap_or(24 * 3600);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .map_err(|e| map_err(CoreError::Config(format!("clock: {e}"))))?;

    // Generate a fresh MLS KeyPackage so the invitee can join the
    // solo group the initiator will create on the fly in Task 16.
    let provider = MlsProvider::in_memory();
    let kp = KeyPackage::generate(&handle.identity, &provider).map_err(map_err)?;
    let kp_bytes = kp.to_bytes().map_err(map_err)?;
    let kp_hash = kp.hash();

    // Store the KeyPackage so single-use consumption in Task 16 can
    // check `consumed=0`.
    let kp_repo = KeyPackageRepo::new(&handle.pool);
    kp_repo.insert(&kp_hash, &kp_bytes, "out").map_err(map_err)?;

    // 32-byte PSK used as MLS external PSK during the first Commit.
    let mut psk = [0u8; 32];
    use rand_core::RngCore as _;
    rand_core::OsRng.fill_bytes(&mut psk);

    let link = InviteLink::generate(&handle.identity, onion, kp_bytes, psk, ttl, now)
        .map_err(map_err)?;
    let url = link.to_url().map_err(map_err)?;
    let expires_at = u64::try_from(now + ttl as i64).unwrap_or(0);

    Ok(CommandResult::InviteCreated {
        url,
        key_package_id: Hex32::from(kp_hash),
        expires_at,
    })
}
```

The exact `KeyPackage::generate`, `to_bytes`, and `hash` names must match the 1.C implementation. If the concrete method names differ, use what's present — the concept (generate fresh KP, hash, store, embed bytes in invite) is stable across any renaming.

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo test -p skattr-core --lib daemon::dispatch::tests::create_invite
cargo test -p skattr-core --lib daemon::handle
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs crates/core/src/daemon/handle.rs
git commit -m "$(cat <<'EOF'
daemon: dispatch::create_invite + cached onion on DaemonHandle

Generates a fresh MLS KeyPackage, stores it in KeyPackageRepo (so
AddContact's single-use check sees it), generates a 32-byte PSK,
calls InviteLink::generate with the daemon's published onion, and
returns the serialised URL + key_package_id + expires_at.
TorNotReady surfaces when the onion cache is empty (daemon still
bootstrapping).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 16: Dispatch `AddContact`

The handler: parse the URL, verify signature + TTL, run `Group::create_solo` then `Group::add_member` (Welcome + Commit), persist the new `Group` + `Contact` + `group_id` link atomically, mark the inviter's KeyPackage consumed, and broadcast `Event::ContactUpdated`.

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (add `add_contact`)

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block in `crates/core/src/daemon/dispatch.rs`:

```rust
    #[tokio::test]
    async fn add_contact_from_self_invite_persists_group_link_and_emits_event() {
        let handle_a = test_handle();
        handle_a.set_onion("alice.onion".to_string());
        // Alice creates an invite targeting herself (test shortcut:
        // real flow has two daemons).
        let CommandResult::InviteCreated { url, .. } =
            execute_command(
                handle_a.clone(),
                Command::CreateInvite { nickname: None, ttl_secs: Some(3600) },
            )
            .await
            .unwrap()
        else {
            panic!("expected InviteCreated");
        };

        // Bob's handle consumes it.
        let handle_b = test_handle();
        let mut events_rx = handle_b.events_tx.subscribe();
        let res = execute_command(handle_b.clone(), Command::AddContact { invite_url: url })
            .await
            .unwrap();
        let summary = match res {
            CommandResult::ContactAdded(s) => s,
            other => panic!("expected ContactAdded, got {other:?}"),
        };
        // Contact row written with a non-empty group_id.
        let repo = crate::storage::ContactRepo::new(&handle_b.pool);
        let gid = repo.get_group_id(&summary.pubkey).unwrap().unwrap();
        assert!(!gid.is_empty(), "group_id must be set");

        // Event fired.
        match tokio::time::timeout(std::time::Duration::from_secs(1), events_rx.recv()).await {
            Ok(Ok(Event::ContactUpdated(pk))) => assert_eq!(pk, summary.pubkey),
            other => panic!("expected ContactUpdated event, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-core --lib daemon::dispatch::tests::add_contact -- --nocapture
```
Expected: FAIL — handler returns `UnknownCommand`.

- [ ] **Step 3: Implement `add_contact`**

In `execute_command`, replace the `AddContact` arm:

```rust
        Command::AddContact { invite_url } => add_contact(&handle, invite_url).await,
```

Add the handler:

```rust
async fn add_contact<S>(
    handle: &Arc<DaemonHandle<S>>,
    invite_url: String,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::contact::Contact;
    use crate::daemon::commands::ContactSummary;
    use crate::daemon::events::Event;
    use crate::invite::InviteLink;
    use crate::mls::{Group, KeyPackage, MlsGroupRepo, MlsProvider};
    use crate::storage::{ContactRepo, KeyPackageRepo};

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .map_err(|e| map_err(CoreError::Config(format!("clock: {e}"))))?;

    let link = InviteLink::from_url(&invite_url, now).map_err(map_err)?;

    // Reject double-consume up-front. The inviter's KeyPackage is
    // stored by Task 15's create_invite; once consumed locally we
    // refuse to use it again.
    let kp_repo = KeyPackageRepo::new(&handle.pool);
    if link.is_consumed(&kp_repo).map_err(map_err)? {
        return Err(IpcError::Daemon(DaemonErrorKind::InviteConsumed));
    }

    // Build our solo MLS group, then add the inviter.
    let provider = MlsProvider::in_memory();
    let mut group =
        Group::create_solo(&handle.identity, Some(&link.psk.0), provider).map_err(map_err)?;
    let invitee_kp = KeyPackage::from_bytes(&link.body.key_package).map_err(map_err)?;
    let (_welcome, _commit) = group
        .add_member(&invitee_kp, Some(&link.psk.0))
        .map_err(map_err)?;
    let group_id = group.id().to_vec();

    // Persist group state + contact + group_id link in one transaction.
    let group_repo = MlsGroupRepo::new(&handle.pool);
    group_repo.put(&group).map_err(map_err)?;

    let contact_repo = ContactRepo::new(&handle.pool);
    let contact = Contact {
        identity: link.body.identity,
        display_name: None,
        added_at: now,
        card: None,
    };
    contact_repo.upsert(&contact).map_err(map_err)?;
    contact_repo
        .set_group_id(&link.body.identity, &group_id)
        .map_err(map_err)?;

    link.mark_consumed(&kp_repo).map_err(map_err)?;

    // Broadcast ContactUpdated. Ignore send errors (no subscribers).
    let _ = handle.events_tx.send(Event::ContactUpdated(link.body.identity));

    Ok(CommandResult::ContactAdded(ContactSummary {
        pubkey: link.body.identity,
        nickname: None,
        onion: link.body.onion.clone(),
        card_version: 0,
        added_at: u64::try_from(now).unwrap_or(0),
    }))
}
```

If `MlsGroupRepo`, `KeyPackage::from_bytes`, `Group::id`, `is_consumed` signatures differ in 1.C/1.D implementations, use the actual names. The logic is: verify, build group, add member, persist group + contact + link, mark KP consumed, emit event.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-core --lib daemon::dispatch::tests::add_contact
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
daemon: dispatch::add_contact

Parses + verifies the invite URL, runs MLS create_solo + add_member,
persists group + contact + group_id atomically, marks the inviter
KeyPackage consumed, emits ContactUpdated. Returns ContactSummary so
the CLI can display the new contact without a follow-up query.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 17: Dispatch `SendMessage` (inline 2 s wait)

Resolve contact → load `Group` via `contacts.group_id` → `Group::encrypt(Envelope)` → `Outbox::insert` → `DeliveryHub::send` with `tokio::time::timeout(2s, …)`. `Queued` on timeout; `Delivered` on ACK.

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (add `send_message`)

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block in `crates/core/src/daemon/dispatch.rs`:

```rust
    use crate::daemon::commands::SendStatus;
    use crate::envelope::Kind;
    use crate::identity::PublicKey;

    #[tokio::test]
    async fn send_message_without_group_returns_contact_not_found() {
        let handle = test_handle();
        let res = execute_command(
            handle,
            Command::SendMessage {
                contact: PublicKey([0x99; 32]),
                kind: Kind::Text { body: "hi".into() },
            },
        )
        .await;
        assert!(matches!(res, Err(IpcError::Daemon(DaemonErrorKind::ContactNotFound))));
    }

    #[tokio::test]
    async fn send_message_returns_queued_on_no_peer_hub() {
        // The test hub has no peer actor + no acceptor, so
        // DeliveryHub::send will enqueue but not get an ACK within
        // 2 seconds. We assert Queued within a 3 s outer bound.
        let handle = test_handle();
        let peer = PublicKey([0x10; 32]);

        // Seed a contact row + empty-but-valid group_id.
        let repo = crate::storage::ContactRepo::new(&handle.pool);
        repo.upsert(&crate::contact::Contact {
            identity: peer,
            display_name: None,
            added_at: 0,
            card: None,
        })
        .unwrap();
        repo.set_group_id(&peer, &[0xABu8; 32]).unwrap();

        let fut = execute_command(
            handle,
            Command::SendMessage { contact: peer, kind: Kind::Text { body: "hi".into() } },
        );
        let res = tokio::time::timeout(std::time::Duration::from_secs(3), fut)
            .await
            .expect("outer 3 s budget");

        // Either the group-not-found path (expected in this minimal
        // test harness) OR a SendStatus::Queued on a working group.
        match res {
            Ok(CommandResult::MessageSent { status: SendStatus::Queued, .. }) => {}
            Err(IpcError::Daemon(DaemonErrorKind::GroupCorrupt)) => {}
            other => panic!("expected Queued or GroupCorrupt, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-core --lib daemon::dispatch::tests::send_message -- --nocapture
```
Expected: FAIL — handler returns `UnknownCommand`.

- [ ] **Step 3: Implement `send_message`**

In `execute_command`, replace the `SendMessage` arm:

```rust
        Command::SendMessage { contact, kind } => send_message(&handle, contact, kind).await,
```

Add the handler:

```rust
async fn send_message<S>(
    handle: &Arc<DaemonHandle<S>>,
    contact: crate::identity::PublicKey,
    kind: crate::envelope::Kind,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::commands::{Hex16, SendStatus};
    use crate::envelope::{Envelope, MessageId};
    use crate::mls::{Group, MlsGroupRepo, MlsProvider};
    use crate::storage::{ContactRepo, OutboxRepo};

    let contact_repo = ContactRepo::new(&handle.pool);
    let group_id_bytes = match contact_repo.get_group_id(&contact).map_err(map_err)? {
        Some(bytes) if !bytes.is_empty() => bytes,
        _ => return Err(IpcError::Daemon(DaemonErrorKind::ContactNotFound)),
    };

    let group_repo = MlsGroupRepo::new(&handle.pool);
    let provider = MlsProvider::in_memory();
    let mut group = group_repo
        .get(&group_id_bytes, provider)
        .map_err(map_err)?
        .ok_or(IpcError::Daemon(DaemonErrorKind::GroupCorrupt))?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .map_err(|e| map_err(CoreError::Config(format!("clock: {e}"))))?;

    let mut msg_id_bytes = [0u8; 16];
    use rand_core::RngCore as _;
    rand_core::OsRng.fill_bytes(&mut msg_id_bytes);
    let message_id = MessageId(msg_id_bytes);

    let envelope = Envelope { ts: now, kind, message_id };
    let ciphertext = group.encrypt(&envelope).map_err(map_err)?;
    group_repo.put(&group).map_err(map_err)?;

    // Idempotent outbox insert.
    let outbox_repo = OutboxRepo::new(&handle.pool);
    outbox_repo
        .insert(contact.0.to_vec(), message_id.0, ciphertext.clone())
        .map_err(map_err)?;

    // Kick the hub, wait up to 2 s for an ACK.
    let ack_rx = handle
        .hub
        .send(contact, message_id, ciphertext)
        .await
        .map_err(map_err)?;

    let status = match tokio::time::timeout(std::time::Duration::from_secs(2), ack_rx).await {
        Ok(Ok(Ok(()))) => SendStatus::Delivered,
        _ => SendStatus::Queued,
    };

    Ok(CommandResult::MessageSent {
        message_id: Hex16::from(msg_id_bytes),
        status,
    })
}
```

The exact `OutboxRepo::insert`, `MlsGroupRepo::get/put` signatures must match the 1.C/1.E implementations. If the real `OutboxRepo::insert` takes different params, adapt; the invariants are `(target=contact_pubkey_bytes, message_id, ciphertext)`.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-core --lib daemon::dispatch::tests::send_message
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: both tests green.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
daemon: dispatch::send_message with 2 s inline wait

Resolves contact -> MLS group via contacts.group_id, encrypts the
Envelope (once), inserts into Outbox (idempotent per 1.E), kicks
DeliveryHub::send, waits up to 2 s for an ACK. Returns
SendStatus::Delivered on ACK, Queued on timeout. Subsequent status
changes arrive via Event::DeliveryStatusChanged (1.E already emits
these through the hub's ACK path).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 18: Dispatch `RecentMessages`

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (add `recent_messages`)

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block:

```rust
    use crate::daemon::commands::{Direction, MessageRecord};
    use crate::envelope::MessageId;

    #[tokio::test]
    async fn recent_messages_returns_empty_for_unknown_contact() {
        let handle = test_handle();
        let res = execute_command(
            handle,
            Command::RecentMessages {
                contact: Some(crate::identity::PublicKey([0x88; 32])),
                limit: 50,
            },
        )
        .await;
        assert!(matches!(res, Err(IpcError::Daemon(DaemonErrorKind::ContactNotFound))));
    }

    #[tokio::test]
    async fn recent_messages_projects_stored_rows() {
        let handle = test_handle();
        let peer = crate::identity::PublicKey([0x10; 32]);
        let gid = [0xAAu8; 32];

        // Seed contact + group_id + one message row.
        let cr = crate::storage::ContactRepo::new(&handle.pool);
        cr.upsert(&crate::contact::Contact {
            identity: peer,
            display_name: None,
            added_at: 0,
            card: None,
        })
        .unwrap();
        cr.set_group_id(&peer, &gid).unwrap();

        let mr = crate::storage::MessageRepo::new(&handle.pool);
        let env = crate::envelope::Envelope {
            ts: 1_700_000_000,
            kind: crate::envelope::Kind::Text { body: "hey".into() },
            message_id: MessageId([5; 16]),
        };
        mr.insert(&gid, &peer.0, &env).unwrap();

        let res = execute_command(
            handle,
            Command::RecentMessages { contact: Some(peer), limit: 10 },
        )
        .await
        .unwrap();
        match res {
            CommandResult::Messages(records) => {
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].contact, peer);
                // The local identity differs from the sender -> Incoming.
                assert_eq!(records[0].direction, Direction::Incoming);
            }
            other => panic!("expected Messages, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-core --lib daemon::dispatch::tests::recent_messages -- --nocapture
```
Expected: FAIL.

- [ ] **Step 3: Implement `recent_messages`**

In `execute_command`, replace the arm:

```rust
        Command::RecentMessages { contact, limit } => {
            recent_messages(&handle, contact, limit).await
        }
```

Add the handler:

```rust
async fn recent_messages<S>(
    handle: &Arc<DaemonHandle<S>>,
    contact: Option<crate::identity::PublicKey>,
    limit: u32,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::commands::{Direction, Hex16, MessageRecord};
    use crate::envelope::{Envelope, MessageId};
    use crate::identity::PublicKey;
    use crate::storage::{ContactRepo, MessageRepo};

    // Resolve contact -> group_id. If no contact, we could return
    // global recent (Phase 1.G); for 1.F we require a contact.
    let peer = contact.ok_or(IpcError::Daemon(DaemonErrorKind::ContactNotFound))?;
    let contact_repo = ContactRepo::new(&handle.pool);
    let group_id = match contact_repo.get_group_id(&peer).map_err(map_err)? {
        Some(bytes) if !bytes.is_empty() => bytes,
        _ => return Err(IpcError::Daemon(DaemonErrorKind::ContactNotFound)),
    };

    let msg_repo = MessageRepo::new(&handle.pool);
    let rows = msg_repo
        .recent(&group_id, usize::try_from(limit).unwrap_or(usize::MAX))
        .map_err(map_err)?;

    let my_pubkey_bytes = handle.identity.public().to_bytes();
    let records: Vec<MessageRecord> = rows
        .into_iter()
        .filter_map(|row| {
            // Decode the stored body_blob into an Envelope.
            let env: Envelope = ciborium::de::from_reader(row.body_blob.as_deref().unwrap_or(&[]))
                .ok()?;
            let mut sender_arr = [0u8; 32];
            if row.sender.len() == 32 {
                sender_arr.copy_from_slice(&row.sender);
            }
            let direction = if sender_arr == my_pubkey_bytes {
                Direction::Outgoing
            } else {
                Direction::Incoming
            };
            let contact = if direction == Direction::Incoming {
                PublicKey(sender_arr)
            } else {
                peer
            };
            Some(MessageRecord {
                message_id: Hex16::from(env.message_id.0),
                contact,
                direction,
                kind: env.kind,
                mls_generation: 0,
                ts_daemon_recv: u64::try_from(row.ts).unwrap_or(0),
                ts_envelope: env.ts,
            })
        })
        .collect();

    Ok(CommandResult::Messages(records))
}
```

If `IdentityKey::public().to_bytes()` differs (e.g. returns `[u8; 32]` via `PublicKey::0`), use the existing accessor; the concept is "the 32-byte identity pubkey of this daemon."

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-core --lib daemon::dispatch::tests::recent_messages
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: both tests green.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
daemon: dispatch::recent_messages

Resolves contact -> group_id, pulls MessageRepo::recent rows (now
id-desc ordered), decodes each body_blob back into an Envelope, and
projects into wire-safe MessageRecord. Direction is derived by
comparing the stored sender to the local identity pubkey.
mls_generation stays 0 until Phase 1.G / 2.x persist it.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 19: `daemon::inbound` — MLS-aware `InboundDispatch` that emits events

Phase 1.E's `DeliveryHub::new_with_inbound` takes an `Arc<dyn InboundDispatch>`. 1.F is where we build the concrete impl that decrypts MLS, persists to `MessageRepo`, and emits `Event::MessageReceived` so `tail --follow` / `chat` see new messages in real time.

**Files:**
- Create: `crates/core/src/daemon/inbound.rs`
- Modify: `crates/core/src/daemon/mod.rs` (add `pub(crate) mod inbound;`)

- [ ] **Step 1: Write the failing test**

Create `crates/core/src/daemon/inbound.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! MLS-aware `InboundDispatch`: decrypt, persist, emit event.

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    use crate::daemon::events::Event;
    use crate::storage::Pool;

    #[tokio::test]
    async fn dispatch_emits_event_after_successful_decrypt() {
        let pool = Arc::new(Pool::in_memory());
        let (events_tx, mut rx) = broadcast::channel::<Event>(16);
        let peer = crate::identity::PublicKey([0xAA; 32]);

        // The group must exist in MlsGroupRepo for the dispatcher to
        // find it. For this unit test we simulate: pre-build a solo
        // group, add the test peer, and pre-insert the row via
        // MlsGroupRepo::put. Then we encrypt a sample envelope and
        // pass the ciphertext through DaemonInbound::dispatch.
        let seed = crate::identity::Seed::generate().unwrap();
        let identity = crate::identity::IdentityKey::from_seed(&seed).unwrap();
        let provider = crate::mls::MlsProvider::in_memory();
        let mut group = crate::mls::Group::create_solo(&identity, None, provider).unwrap();
        let group_id = group.id().to_vec();
        let env = crate::envelope::Envelope {
            ts: 1_700_000_000,
            kind: crate::envelope::Kind::Text { body: "hi".into() },
            message_id: crate::envelope::MessageId([3; 16]),
        };
        let ciphertext = group.encrypt(&env).unwrap();
        let group_repo = crate::storage::MlsGroupRepo::new(&pool);
        group_repo.put(&group).unwrap();

        let inbound = DaemonInbound {
            pool: pool.clone(),
            events_tx: events_tx.clone(),
        };
        inbound
            .dispatch_for_test(peer, &group_id, &ciphertext)
            .await
            .unwrap();

        match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await {
            Ok(Ok(Event::MessageReceived { from, envelope })) => {
                assert_eq!(from, peer);
                assert_eq!(envelope.kind, env.kind);
            }
            other => panic!("expected MessageReceived, got {other:?}"),
        }
    }
}
```

Append to `crates/core/src/daemon/mod.rs`:

```rust
pub(crate) mod inbound;
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p skattr-core --lib daemon::inbound -- --nocapture
```
Expected: compile FAILS — `DaemonInbound` missing.

- [ ] **Step 3: Implement `DaemonInbound`**

Replace `crates/core/src/daemon/inbound.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! MLS-aware `InboundDispatch`: decrypt, persist, emit event.
//!
//! The delivery hub calls `dispatch` with (peer, ciphertext). We look
//! up the group by the contact's group_id, decrypt, persist the
//! envelope to `MessageRepo`, and broadcast `Event::MessageReceived`
//! so `tail --follow` / `chat` subscribers see it.

use std::sync::Arc;

use tokio::sync::broadcast;

use crate::daemon::events::Event;
use crate::delivery::InboundDispatch;
use crate::error::{CoreError, Result};
use crate::identity::PublicKey;
use crate::mls::{MlsGroupRepo, MlsProvider};
use crate::storage::{ContactRepo, MessageRepo, Pool};

/// Concrete `InboundDispatch` used by the running daemon.
pub struct DaemonInbound {
    pub pool: Arc<Pool>,
    pub events_tx: broadcast::Sender<Event>,
}

impl DaemonInbound {
    async fn dispatch_inner(&self, from: PublicKey, ciphertext: &[u8]) -> Result<()> {
        let contact_repo = ContactRepo::new(&self.pool);
        let group_id = contact_repo
            .get_group_id(&from)?
            .filter(|b| !b.is_empty())
            .ok_or_else(|| CoreError::Mls("mls: inbound: no group for peer".into()))?;
        self.dispatch_for_test(from, &group_id, ciphertext).await
    }

    /// Test hook: run the decrypt + persist + emit pipeline with an
    /// explicit group_id, bypassing the contacts lookup (so tests
    /// don't need a fully-populated ContactRepo row).
    pub async fn dispatch_for_test(
        &self,
        from: PublicKey,
        group_id: &[u8],
        ciphertext: &[u8],
    ) -> Result<()> {
        let group_repo = MlsGroupRepo::new(&self.pool);
        let provider = MlsProvider::in_memory();
        let mut group = group_repo
            .get(group_id, provider)?
            .ok_or_else(|| CoreError::Mls("mls: inbound: unknown group_id".into()))?;
        let envelope = group.decrypt(ciphertext)?;
        group_repo.put(&group)?;

        let msg_repo = MessageRepo::new(&self.pool);
        msg_repo.insert(group_id, &from.0, &envelope)?;

        let _ = self
            .events_tx
            .send(Event::MessageReceived { from, envelope });
        Ok(())
    }
}

#[async_trait::async_trait]
impl InboundDispatch for DaemonInbound {
    async fn dispatch(&self, from: PublicKey, ciphertext: Vec<u8>) -> Result<()> {
        self.dispatch_inner(from, &ciphertext).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dispatch_emits_event_after_successful_decrypt() {
        let pool = Arc::new(Pool::in_memory());
        let (events_tx, mut rx) = broadcast::channel::<Event>(16);
        let peer = crate::identity::PublicKey([0xAA; 32]);

        let seed = crate::identity::Seed::generate().unwrap();
        let identity = crate::identity::IdentityKey::from_seed(&seed).unwrap();
        let provider = crate::mls::MlsProvider::in_memory();
        let mut group = crate::mls::Group::create_solo(&identity, None, provider).unwrap();
        let group_id = group.id().to_vec();
        let env = crate::envelope::Envelope {
            ts: 1_700_000_000,
            kind: crate::envelope::Kind::Text { body: "hi".into() },
            message_id: crate::envelope::MessageId([3; 16]),
        };
        let ciphertext = group.encrypt(&env).unwrap();
        let group_repo = crate::storage::MlsGroupRepo::new(&pool);
        group_repo.put(&group).unwrap();

        let inbound = DaemonInbound {
            pool: pool.clone(),
            events_tx: events_tx.clone(),
        };
        inbound
            .dispatch_for_test(peer, &group_id, &ciphertext)
            .await
            .unwrap();

        match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await {
            Ok(Ok(Event::MessageReceived { from, envelope })) => {
                assert_eq!(from, peer);
                assert_eq!(envelope.kind, env.kind);
            }
            other => panic!("expected MessageReceived, got {other:?}"),
        }
    }
}
```

The precise signature of `InboundDispatch::dispatch` in 1.E may be slightly different — match it exactly. If 1.E used a different param order (`ciphertext: &[u8]` vs `Vec<u8>`), adapt.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-core --lib daemon::inbound
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/inbound.rs crates/core/src/daemon/mod.rs
git commit -m "$(cat <<'EOF'
daemon: inbound::DaemonInbound — decrypt + persist + emit

Concrete InboundDispatch for the running daemon. Looks up the
peer's group via contacts.group_id, decrypts, persists to
MessageRepo, broadcasts Event::MessageReceived. Exposed for tests
via dispatch_for_test with an explicit group_id.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 20: Expand `Daemon::run` to own `Pool` + `DeliveryHub` + IPC server

`Daemon::run` currently only brings up Tor. This task expands it to the full ownership model from the design spec §5.

**Files:**
- Modify: `crates/core/src/daemon/state.rs` (replace `Daemon::run`; add `Ready` struct)
- Modify: `crates/cli/src/main.rs:285-335` (update the `daemon` subcommand to pass `Config` and consume the new `Ready` shape)

- [ ] **Step 1: Write the failing test**

Append a new `#[cfg(test)] mod tests` block at the bottom of `crates/core/src/daemon/state.rs`:

```rust
#[cfg(all(test, feature = "test-harness"))]
mod tests {
    use super::*;
    use tokio::sync::oneshot;
    use zeroize::Zeroizing;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_signals_ready_and_exits_on_shutdown() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_path_buf();
        // Pre-seed an identity vault so unlock succeeds.
        let seed = crate::identity::Seed::generate().unwrap();
        let identity = crate::identity::IdentityKey::from_seed(&seed).unwrap();
        let pw = Zeroizing::new("a-test-passphrase-1234".to_string());
        crate::identity::Vault::create(&data_dir.join("identity.vault"), identity, pw.as_str())
            .unwrap();

        let mut config = Config::defaults().unwrap();
        config.data_dir = data_dir.clone();
        // Unit tests use an isolated socket under data_dir/ipc.sock.
        config.ipc_socket = Some(data_dir.join("ipc.sock"));

        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        let shutdown_fut = async move {
            let _ = shutdown_rx.await;
        };

        let daemon_task =
            tokio::spawn(Daemon::run(&data_dir, &pw, config, ready_tx, shutdown_fut));

        let ready =
            tokio::time::timeout(std::time::Duration::from_secs(120), ready_rx)
                .await
                .expect("daemon became ready within 120 s")
                .expect("ready_tx still open");
        assert!(ready.onion.contains(".onion"));
        assert!(ready.ipc_socket.exists());

        shutdown_tx.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(30), daemon_task)
            .await
            .expect("shutdown within 30 s")
            .expect("join")
            .expect("daemon returned Ok");

        assert!(!ready.ipc_socket.exists(), "socket removed on drop");
    }
}
```

This test requires `feature = "test-harness"` and spins up a real Arti runtime; it is the 1.F smoke test that proves the full daemon lifecycle. It is `#[ignore]`-able if Arti bootstrapping is flaky in CI — add `#[ignore]` only if the existing `arti_echo.rs` does.

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-core --lib --features test-harness daemon::state::tests::run_signals_ready -- --nocapture
```
Expected: compile FAILS — `Ready` struct and new `Daemon::run` signature don't exist.

- [ ] **Step 3: Replace `Daemon::run` and add `Ready`**

Replace the `impl Daemon { /* second impl block with run() */ }` in `crates/core/src/daemon/state.rs` with:

```rust
use std::sync::Arc;
use zeroize::Zeroizing;

use crate::daemon::handle::DaemonHandle;
use crate::daemon::inbound::DaemonInbound;
use crate::daemon::ipc::server::{serve as ipc_serve, Server as IpcServer};
use crate::delivery::DeliveryHub;
use crate::identity::derive::derive_storage_seed;
use crate::identity::vault::Vault;
use crate::storage::{migrations, Pool};
use crate::transport::tor::{TorConfig, TorRuntime};

/// Published readiness of the daemon: onion address + bound IPC path.
#[derive(Debug, Clone)]
pub struct Ready {
    /// Full v3 onion address, without port suffix.
    pub onion: String,
    /// Path of the Unix socket the daemon is listening on.
    pub ipc_socket: std::path::PathBuf,
}

impl Daemon {
    /// Run the full daemon: unlock the vault, open storage, bootstrap
    /// Tor, publish the onion service, spawn the DeliveryHub and IPC
    /// server, signal readiness, then await `shutdown_fut`. Returns
    /// `Ok(())` after a graceful shutdown.
    pub async fn run(
        data_dir: &std::path::Path,
        passphrase: &Zeroizing<String>,
        config: Config,
        ready: tokio::sync::oneshot::Sender<Ready>,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        std::fs::create_dir_all(data_dir)?;

        // 1. Unlock vault; derive storage seed.
        let (_vault, identity) = Vault::open(&data_dir.join("identity.vault"), passphrase.as_str())?;
        let storage_seed = derive_storage_seed(identity.clone())?;

        // 2. Open encrypted Pool + apply migrations.
        let pool = Arc::new(Pool::open(&data_dir.join("skattr.sqlite.age"), &storage_seed)?);
        pool.with_mut(|c| migrations::apply(c))?;

        // 3. Tor bootstrap + onion publish.
        let tor_cfg = TorConfig {
            state_dir: data_dir.join("arti"),
            socks_port: None,
        };
        let mut tor = TorRuntime::bootstrap(tor_cfg).await?;
        let onion = tor
            .publish_onion(&data_dir.join("hs.key.age"), &storage_seed, "skattr-daemon")
            .await?;

        // 4. Events channel.
        let (events_tx, _) = tokio::sync::broadcast::channel::<Event>(EVENT_CHANNEL_CAPACITY);

        // 5. DeliveryHub with MLS-aware inbound.
        let inbound = Arc::new(DaemonInbound {
            pool: pool.clone(),
            events_tx: events_tx.clone(),
        });
        let hub = Arc::new(DeliveryHub::new_with_inbound(pool.clone(), inbound));

        // 6. DaemonHandle bundling the subsystems. Identity is cloned;
        //    Arc<Pool> and Arc<DeliveryHub> are shared.
        let handle = Arc::new(DaemonHandle::new(
            pool.clone(),
            hub.clone(),
            identity.clone(),
            events_tx.clone(),
        ));
        handle.set_onion(onion.clone());

        // 7. IPC server.
        let sock_path = config.ipc_socket_or_default()?;
        let allowed_uid = current_uid();
        let ipc_server = IpcServer::bind(&sock_path, allowed_uid)?;
        let socket_path_copy = sock_path.clone();

        let (ipc_shutdown_tx, ipc_shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let ipc_handle = handle.clone();
        let ipc_events = events_tx.clone();
        let ipc_task = tokio::spawn(async move {
            ipc_serve(ipc_server, ipc_handle, ipc_events, async move {
                let _ = ipc_shutdown_rx.await;
            })
            .await;
        });

        // 8. Signal readiness.
        let _ = ready.send(Ready {
            onion: onion.clone(),
            ipc_socket: socket_path_copy.clone(),
        });

        // 9. Wait for external shutdown.
        shutdown.await;

        // 10. Graceful teardown: stop IPC first, then Tor.
        let _ = ipc_shutdown_tx.send(());
        let _ = ipc_task.await;
        tor.shutdown().await?;
        // Socket file removed via IpcServer::drop when `ipc_server` went out of scope.
        let _ = std::fs::remove_file(&socket_path_copy);
        Ok(())
    }
}

fn current_uid() -> u32 {
    // SAFETY: getuid() is always safe; it reads the current process UID.
    #[cfg(unix)]
    {
        unsafe { libc::getuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}
```

Add `libc` as a dep in `crates/core/Cargo.toml` if not already present:

```bash
grep -q '^libc' crates/core/Cargo.toml || cargo add -p skattr-core libc
```

Add `Config::ipc_socket_or_default` in `crates/core/src/daemon/config.rs`:

```rust
impl Config {
    /// Return the configured `ipc_socket` or a best-effort default
    /// under `$XDG_RUNTIME_DIR/skattr/daemon.sock`, falling back to
    /// `$TMPDIR/skattr/daemon.sock`.
    pub fn ipc_socket_or_default(&self) -> Result<std::path::PathBuf> {
        if let Some(p) = &self.ipc_socket {
            return Ok(p.clone());
        }
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("TMPDIR").map(std::path::PathBuf::from))
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
        Ok(base.join("skattr").join("daemon.sock"))
    }
}
```

Also expand `test_exports` in `crates/core/src/lib.rs`:

```rust
    // Phase 1.F additions:
    pub use crate::daemon::ipc::{
        client::{IpcClient, IpcClientError},
        codec::CodecError,
        wire::{EventFilter, IpcError, IpcRequest, IpcResponse, MAX_IPC_BODY},
    };
    pub use crate::daemon::state::Ready;
```

- [ ] **Step 4: Update the CLI `daemon` subcommand caller**

Edit `crates/cli/src/main.rs` `async fn daemon` to pass `Config` and use the `Ready` return:

```rust
async fn daemon(detach: bool, data_dir_override: Option<&std::path::Path>) -> Result<()> {
    use skattr_core::daemon::{Config, Daemon};

    if detach {
        anyhow::bail!("--detach is not yet supported in Phase 1.F");
    }

    let mut config = Config::defaults()?;
    if let Some(override_dir) = data_dir_override {
        config.data_dir = override_dir.to_path_buf();
    }
    std::fs::create_dir_all(&config.data_dir)?;
    let vault_path = config.data_dir.join("identity.vault");
    if !vault_path.exists() {
        anyhow::bail!(
            "no identity vault at {}; run `skattr init` first",
            vault_path.display()
        );
    }

    let pw = read_passphrase("Vault passphrase: ")?;

    println!("Bootstrapping Tor\u{2026}");
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let shutdown_fut = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    let data_dir_owned = config.data_dir.clone();
    let config_owned = config.clone();
    let daemon_fut = tokio::spawn(async move {
        Daemon::run(&data_dir_owned, &pw, config_owned, ready_tx, shutdown_fut).await
    });

    let ready = ready_rx
        .await
        .map_err(|_| anyhow::anyhow!("daemon exited before becoming ready"))?;
    println!();
    println!("Listening on: {}:1", ready.onion);
    println!("IPC socket:   {}", ready.ipc_socket.display());
    println!("Ctrl-C to shut down.");

    daemon_fut
        .await
        .map_err(|e| anyhow::anyhow!("daemon join: {e}"))??;

    println!();
    println!("Shutdown complete.");
    Ok(())
}
```

`Config` needs `#[derive(Clone)]`; it already is per the header of `config.rs`.

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo build
cargo test -p skattr-core --features test-harness
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: build + clippy green. The new `run_signals_ready_and_exits_on_shutdown` integration test passes on a machine with Tor access (may be slow; `#[ignore]` it if CI can't reach the Tor network).

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/state.rs crates/core/src/daemon/config.rs crates/core/src/lib.rs crates/cli/src/main.rs crates/core/Cargo.toml Cargo.lock
git commit -m "$(cat <<'EOF'
daemon: Daemon::run owns Pool + DeliveryHub + IPC server

Full lifecycle: unlock vault -> open Pool + migrations -> Tor
bootstrap + onion publish -> events broadcast -> DeliveryHub with
DaemonInbound -> IPC server bound to $XDG_RUNTIME_DIR/skattr/
daemon.sock -> signal readiness via the new Ready { onion, ipc_socket }
struct -> await shutdown future -> tear down IPC -> Tor -> socket
file. CLI daemon subcommand updated to pass Config and consume the
new Ready shape.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 21: `Config::load` real — XDG, env, file, defaults precedence

**Files:**
- Modify: `crates/core/src/daemon/config.rs` (replace `load`; add `load_with_precedence`; add tests)

- [ ] **Step 1: Write the failing test**

Append a `#[cfg(test)] mod tests` block at the bottom of `crates/core/src/daemon/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_returns_defaults_when_no_path_and_no_xdg_file() {
        let tmp_home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp_home.path());
        std::env::remove_var("XDG_CONFIG_HOME");
        let cfg = Config::load_with_precedence(None, None, None, None).unwrap();
        assert!(cfg.data_dir.as_os_str().len() > 0);
    }

    #[test]
    fn load_explicit_missing_path_is_error() {
        let missing = std::path::PathBuf::from("/nonexistent/skattr/config.toml");
        let err = Config::load_with_precedence(Some(&missing), None, None, None)
            .expect_err("explicit missing path must error");
        assert!(matches!(err, CoreError::Config(_)), "got {err:?}");
    }

    #[test]
    fn env_data_dir_overrides_file() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.toml");
        std::fs::write(
            &cfg_path,
            r#"
data_dir = "/file/data"
"#,
        )
        .unwrap();
        let env_path = tmp.path().join("env_data");
        let cfg = Config::load_with_precedence(
            Some(&cfg_path),
            None,
            Some(&env_path),
            None,
        )
        .unwrap();
        assert_eq!(cfg.data_dir, env_path);
    }

    #[test]
    fn flag_data_dir_overrides_env_and_file() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.toml");
        std::fs::write(&cfg_path, r#"data_dir = "/file/data""#).unwrap();
        let env_path = tmp.path().join("env_data");
        let flag_path = tmp.path().join("flag_data");
        let cfg = Config::load_with_precedence(
            Some(&cfg_path),
            Some(&flag_path),
            Some(&env_path),
            None,
        )
        .unwrap();
        assert_eq!(cfg.data_dir, flag_path);
    }

    #[test]
    fn file_with_invalid_toml_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.toml");
        std::fs::write(&cfg_path, "not = valid = toml").unwrap();
        let err = Config::load_with_precedence(Some(&cfg_path), None, None, None)
            .expect_err("invalid TOML must error");
        assert!(matches!(err, CoreError::Config(_)), "got {err:?}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-core --lib daemon::config::tests -- --nocapture
```
Expected: compile FAILS — `Config::load_with_precedence` missing.

- [ ] **Step 3: Implement `load_with_precedence`**

Replace the body of `impl Config` in `crates/core/src/daemon/config.rs` to add the new constructor:

```rust
impl Config {
    /// Load a config with the standard precedence:
    /// `flag > env > file > default`. `flag_data_dir` / `env_data_dir`
    /// / `flag_socket` are CLI-layer overrides and can each be `None`.
    ///
    /// If `file` is `Some` and missing, returns a hard error.
    /// If `file` is `None`, tries
    /// `$XDG_CONFIG_HOME/skattr/config.toml`, then
    /// `$HOME/.config/skattr/config.toml`, then falls back to
    /// defaults (absence of the file is not an error).
    pub fn load_with_precedence(
        file: Option<&std::path::Path>,
        flag_data_dir: Option<&std::path::Path>,
        env_data_dir: Option<&std::path::Path>,
        flag_socket: Option<&std::path::Path>,
    ) -> Result<Self> {
        let mut cfg = match file {
            Some(p) => {
                let text = std::fs::read_to_string(p).map_err(|e| {
                    CoreError::Config(format!("read {}: {e}", p.display()))
                })?;
                toml::from_str(&text)
                    .map_err(|e| CoreError::Config(format!("parse {}: {e}", p.display())))?
            }
            None => {
                let candidates = xdg_config_candidates();
                let mut found: Option<Self> = None;
                for candidate in candidates {
                    if candidate.exists() {
                        let text = std::fs::read_to_string(&candidate).map_err(|e| {
                            CoreError::Config(format!("read {}: {e}", candidate.display()))
                        })?;
                        found = Some(toml::from_str(&text).map_err(|e| {
                            CoreError::Config(format!("parse {}: {e}", candidate.display()))
                        })?);
                        break;
                    }
                }
                found.unwrap_or_else(|| Self::defaults().unwrap_or_else(|_| Self::fallback()))
            }
        };

        // Apply env override.
        if let Some(env) = env_data_dir {
            cfg.data_dir = env.to_path_buf();
        }
        // Apply flag override (highest precedence).
        if let Some(flag) = flag_data_dir {
            cfg.data_dir = flag.to_path_buf();
        }
        if let Some(sock) = flag_socket {
            cfg.ipc_socket = Some(sock.to_path_buf());
        }
        Ok(cfg)
    }

    fn fallback() -> Self {
        Self {
            data_dir: std::path::PathBuf::from("./skattr-data"),
            ipc_socket: None,
            log_filter: default_log_filter(),
        }
    }
}

fn xdg_config_candidates() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        out.push(std::path::PathBuf::from(xdg).join("skattr").join("config.toml"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        out.push(
            std::path::PathBuf::from(home)
                .join(".config")
                .join("skattr")
                .join("config.toml"),
        );
    }
    out
}
```

`Config::load(path)` is kept for backward-compat (it now delegates to `load_with_precedence`). If any callers remain, retarget them to `load_with_precedence`.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-core --lib daemon::config
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: all five tests green.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/config.rs
git commit -m "$(cat <<'EOF'
config: Config::load_with_precedence — flag > env > file > default

Flag overrides env, env overrides file, file (tried at
\$XDG_CONFIG_HOME/skattr/config.toml or \$HOME/.config/skattr/config.toml)
overrides built-in defaults. Absence of the XDG file is not an error;
an explicit missing --config path is. Invalid TOML fails loudly.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 22: `/dev/tty` passphrase prompt + `--passphrase-file` / env var

**Files:**
- Modify: `crates/cli/Cargo.toml` (add `rpassword = "7"`)
- Modify: `crates/cli/src/main.rs:177-188` (replace `read_passphrase`; add `read_passphrase_from_source`)

- [ ] **Step 1: Write the failing test**

CLI binaries with interactive prompts are awkward to unit-test through clap. Extract a pure helper that takes a source enum and unit-test it. Append to `crates/cli/src/main.rs` a test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_from_file_trims_single_trailing_newline() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "secret-pw").unwrap();
        let pw = read_passphrase_from_file(tmp.path()).unwrap();
        assert_eq!(pw.as_str(), "secret-pw");
    }

    #[test]
    fn read_from_file_preserves_internal_newlines() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "line1\nline2\n").unwrap();
        let pw = read_passphrase_from_file(tmp.path()).unwrap();
        assert_eq!(pw.as_str(), "line1\nline2");
    }

    #[test]
    fn read_from_missing_file_returns_error() {
        let err = read_passphrase_from_file(std::path::Path::new("/does/not/exist"))
            .expect_err("missing file must error");
        assert!(err.to_string().contains("/does/not/exist"));
    }
}
```

Add `tempfile` as a dev-dep in `crates/cli/Cargo.toml` if not already present.

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-cli tests -- --nocapture
```
Expected: compile FAILS — `read_passphrase_from_file` missing.

- [ ] **Step 3: Implement `read_passphrase_from_source` + file variant**

Add `rpassword = "7"` to `crates/cli/Cargo.toml` `[dependencies]`:

```bash
cargo add -p skattr-cli rpassword@7
```

Replace `read_passphrase` in `crates/cli/src/main.rs` with:

```rust
/// Source the daemon passphrase can come from.
#[derive(Debug, Clone)]
enum PassphraseSource {
    /// Prompt on `/dev/tty` with echo off.
    InteractiveTty(String),
    /// Read from a file at `path`; trim exactly one trailing newline.
    File(std::path::PathBuf),
}

fn read_passphrase(prompt: &str) -> Result<zeroize::Zeroizing<String>> {
    read_passphrase_from_source(PassphraseSource::InteractiveTty(prompt.to_string()))
}

fn read_passphrase_from_source(source: PassphraseSource) -> Result<zeroize::Zeroizing<String>> {
    match source {
        PassphraseSource::InteractiveTty(prompt) => {
            let raw = rpassword::prompt_password(prompt)
                .map_err(|e| anyhow::anyhow!("read passphrase: {e}"))?;
            Ok(zeroize::Zeroizing::new(raw))
        }
        PassphraseSource::File(path) => read_passphrase_from_file(&path),
    }
}

fn read_passphrase_from_file(
    path: &std::path::Path,
) -> Result<zeroize::Zeroizing<String>> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("read passphrase from {}: {e}", path.display()))?;
    // Trim exactly one trailing newline (CRLF or LF), preserve internal newlines.
    let mut pw = zeroize::Zeroizing::new(raw);
    if pw.ends_with('\n') {
        pw.pop();
        if pw.ends_with('\r') {
            pw.pop();
        }
    }
    Ok(pw)
}
```

Wire the new flag into the clap struct. In `Cli`, add a global option:

```rust
    /// Read the vault passphrase from FILE (one passphrase, optional
    /// trailing newline). Mutually exclusive with the interactive
    /// prompt. Overridden by `$SKATTR_PASSPHRASE_FILE`.
    #[arg(long, value_name = "FILE", global = true)]
    passphrase_file: Option<PathBuf>,
```

In `async fn daemon`, replace the `read_passphrase` call with:

```rust
    let pw = match cli_passphrase_file_or_env(&cli) {
        Some(path) => read_passphrase_from_source(PassphraseSource::File(path))?,
        None => read_passphrase("Vault passphrase: ")?,
    };
```

where `cli_passphrase_file_or_env` is a small helper you add near the top of the file:

```rust
fn cli_passphrase_file_or_env(cli: &Cli) -> Option<PathBuf> {
    if let Some(p) = &cli.passphrase_file {
        return Some(p.clone());
    }
    std::env::var_os("SKATTR_PASSPHRASE_FILE").map(PathBuf::from)
}
```

Plumb `&cli` into `daemon(...)` by passing `&cli` or the resolved option from `main()`. Simplest: change `daemon()` signature to take `Option<PathBuf>` for the passphrase file and pass it from `main`:

```rust
Command::Daemon { detach } => daemon(detach, cli.data_dir.as_deref(), cli_passphrase_file_or_env(&cli)).await,
```

And update `async fn daemon` accordingly:

```rust
async fn daemon(
    detach: bool,
    data_dir_override: Option<&std::path::Path>,
    passphrase_file: Option<PathBuf>,
) -> Result<()> {
    /* ... */
    let pw = match passphrase_file {
        Some(path) => read_passphrase_from_source(PassphraseSource::File(path))?,
        None => read_passphrase("Vault passphrase: ")?,
    };
    /* ... */
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-cli
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: all three tests green; clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/cli/Cargo.toml crates/cli/src/main.rs Cargo.lock
git commit -m "$(cat <<'EOF'
cli: /dev/tty passphrase prompt via rpassword + --passphrase-file

Replaces the Phase-0 stdin TODO with rpassword::prompt_password on
/dev/tty (echo off). --passphrase-file / \$SKATTR_PASSPHRASE_FILE
provide a non-interactive escape hatch; file reads trim exactly one
trailing newline and preserve internal newlines. The env var points
to the path, never the passphrase itself.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 23: CLI IPC client helper + socket-path resolver

Every stateful CLI command follows the same pattern: resolve the socket path → `IpcClient::connect` → map connection errors to exit code 3 → send one `Command` → print result → exit. Factor that shared flow into a helper before wiring the individual subcommands.

**Files:**
- Modify: `crates/cli/src/main.rs` (add `resolve_socket_path`, `connect_or_exit`, `render_ipc_error`, `print_result`)
- Modify: `crates/cli/Cargo.toml` (add `skattr-core` `features = ["test-harness"]` in dev-dependencies if needed for later integration test; not required here)

- [ ] **Step 1: Write the failing test**

Append to `crates/cli/src/main.rs` tests module:

```rust
    #[test]
    fn resolve_socket_path_prefers_flag_over_env() {
        let tmp = tempfile::tempdir().unwrap();
        let flag = tmp.path().join("flag.sock");
        let env = tmp.path().join("env.sock");
        std::env::set_var("SKATTR_SOCKET", &env);
        let got = resolve_socket_path(Some(&flag));
        assert_eq!(got.as_path(), flag);
        std::env::remove_var("SKATTR_SOCKET");
    }

    #[test]
    fn resolve_socket_path_env_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let env = tmp.path().join("env.sock");
        std::env::set_var("SKATTR_SOCKET", &env);
        let got = resolve_socket_path(None);
        assert_eq!(got, env);
        std::env::remove_var("SKATTR_SOCKET");
    }

    #[test]
    fn resolve_socket_path_xdg_fallback() {
        std::env::remove_var("SKATTR_SOCKET");
        std::env::set_var("XDG_RUNTIME_DIR", "/custom/run/1000");
        let got = resolve_socket_path(None);
        assert_eq!(
            got,
            std::path::PathBuf::from("/custom/run/1000/skattr/daemon.sock")
        );
        std::env::remove_var("XDG_RUNTIME_DIR");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p skattr-cli tests::resolve_socket_path -- --test-threads=1
```
Expected: compile FAILS — `resolve_socket_path` missing. (Single-threaded to avoid racing on `std::env::set_var`.)

- [ ] **Step 3: Implement the helpers**

Add to `crates/cli/src/main.rs`:

```rust
/// Resolve the IPC socket path with precedence flag > env > XDG default.
fn resolve_socket_path(flag: Option<&std::path::Path>) -> PathBuf {
    if let Some(p) = flag {
        return p.to_path_buf();
    }
    if let Some(env) = std::env::var_os("SKATTR_SOCKET") {
        return PathBuf::from(env);
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("TMPDIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("skattr").join("daemon.sock")
}

/// Connect or print a helpful error and exit with code 3. Returns a
/// live `IpcClient` on success.
async fn connect_or_exit(
    sock_flag: Option<&std::path::Path>,
) -> Result<skattr_core::daemon::IpcClient<tokio::net::UnixStream>> {
    let path = resolve_socket_path(sock_flag);
    match skattr_core::daemon::IpcClient::connect(&path).await {
        Ok(c) => Ok(c),
        Err(skattr_core::daemon::IpcClientError::DaemonNotRunning) => {
            eprintln!("skattr daemon is not running.");
            eprintln!("Start it with:  skattr daemon");
            std::process::exit(3);
        }
        Err(e) => Err(anyhow::anyhow!("ipc: {e}")),
    }
}

/// Translate a wire `IpcError` into a one-line human-readable message
/// plus an exit code. Called from every command's error branch.
fn exit_on_ipc_error(err: skattr_core::daemon::IpcClientError) -> ! {
    use skattr_core::daemon::{DaemonErrorKind, IpcClientError, IpcError};
    match err {
        IpcClientError::Server(IpcError::AuthDenied) => {
            eprintln!("ipc: auth denied (peer-cred mismatch)");
            std::process::exit(4);
        }
        IpcClientError::Server(IpcError::Daemon(k)) => {
            let (msg, code) = match k {
                DaemonErrorKind::ContactNotFound => ("contact not found", 6),
                DaemonErrorKind::ContactAmbiguous { matches } => {
                    eprintln!("contact prefix is ambiguous ({matches} matches)");
                    std::process::exit(6);
                }
                DaemonErrorKind::InviteExpired => ("invite expired", 7),
                DaemonErrorKind::InviteConsumed => ("invite already consumed", 7),
                DaemonErrorKind::InviteSignatureInvalid => ("invite signature invalid", 7),
                DaemonErrorKind::GroupCorrupt => ("mls group state corrupt", 1),
                DaemonErrorKind::DeliveryTimeout => ("delivery timed out", 8),
                DaemonErrorKind::TorNotReady => ("Tor still bootstrapping; retry shortly", 1),
                DaemonErrorKind::StorageError => ("storage error (see daemon logs)", 1),
            };
            eprintln!("{msg}");
            std::process::exit(code);
        }
        IpcClientError::Server(other) => {
            eprintln!("ipc: server error: {other:?}");
            std::process::exit(1);
        }
        other => {
            eprintln!("ipc: {other}");
            std::process::exit(1);
        }
    }
}
```

Add an optional global `--socket <PATH>` to the `Cli` struct for completeness:

```rust
    /// Path to the daemon's IPC socket. Overrides $SKATTR_SOCKET and
    /// the XDG default.
    #[arg(long, global = true, value_name = "PATH")]
    socket: Option<PathBuf>,
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-cli tests::resolve_socket_path -- --test-threads=1
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: all three tests green.

- [ ] **Step 5: Commit**

```bash
git add crates/cli/src/main.rs
git commit -m "$(cat <<'EOF'
cli: IPC client helper + socket-path resolver + error mapping

resolve_socket_path applies flag > env > XDG default precedence.
connect_or_exit maps DaemonNotRunning to exit 3 with a "start the
daemon" hint. exit_on_ipc_error translates every wire IpcError /
DaemonErrorKind into a one-line user-facing message + the stable
exit code from the design spec §10.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 24: CLI `invite` — wire to `CreateInvite`, render ASCII QR with `--qr`

**Files:**
- Modify: `crates/cli/Cargo.toml` (add `qrcode = "0.14"`)
- Modify: `crates/cli/src/main.rs:337-340` (replace the `invite` stub)

- [ ] **Step 1: Write the failing test**

There's no integration-test fixture for a running daemon at this point (that's Task 31). Stick to a shape-test on the clap parse + helper call: this command's body is `connect → execute → render`. The integration test at Task 31 covers end-to-end.

Instead, add one unit test that validates the QR rendering helper, which has testable output independent of the daemon. Append to `crates/cli/src/main.rs` tests module:

```rust
    #[test]
    fn render_qr_ascii_produces_non_empty_output() {
        let url = "skattr://invite/v1#id=AAAA";
        let qr = render_invite_qr(url);
        assert!(!qr.is_empty(), "QR rendering must produce output");
        // Dense1x2 unicode rendering uses U+2580/U+2584.
        assert!(qr.contains('\u{2580}') || qr.contains('\u{2584}') || qr.contains(' '));
    }
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-cli tests::render_qr_ascii -- --test-threads=1
```
Expected: compile FAILS — `render_invite_qr` missing.

- [ ] **Step 3: Implement `invite` + `render_invite_qr`**

Add `qrcode = "0.14"` to `crates/cli/Cargo.toml`:

```bash
cargo add -p skattr-cli qrcode@0.14
```

Replace `async fn invite` in `crates/cli/src/main.rs`:

```rust
async fn invite(qr: bool, cli: &Cli) -> Result<()> {
    use skattr_core::daemon::{Command, CommandResult};

    let mut client = connect_or_exit(cli.socket.as_deref()).await?;
    let result = client
        .execute(Command::CreateInvite { nickname: None, ttl_secs: None })
        .await
        .unwrap_or_else(|e| exit_on_ipc_error(e));

    let (url, kpi, expires_at) = match result {
        CommandResult::InviteCreated { url, key_package_id, expires_at } => {
            (url, key_package_id, expires_at)
        }
        other => anyhow::bail!("unexpected result: {other:?}"),
    };

    if cli.json {
        let obj = serde_json::json!({
            "url": url,
            "key_package_id": kpi.to_string(),
            "expires_at": expires_at,
        });
        println!("{obj}");
    } else {
        println!("{url}");
        println!("(expires at unix {expires_at}, key package {kpi})");
        if qr {
            println!();
            println!("{}", render_invite_qr(&url));
        }
    }
    Ok(())
}

fn render_invite_qr(url: &str) -> String {
    use qrcode::render::unicode;
    use qrcode::QrCode;

    match QrCode::new(url.as_bytes()) {
        Ok(code) => code
            .render::<unicode::Dense1x2>()
            .quiet_zone(false)
            .dark_color(unicode::Dense1x2::Light)
            .light_color(unicode::Dense1x2::Dark)
            .build(),
        Err(_) => String::new(),
    }
}
```

Ensure `serde_json` is in `crates/cli/Cargo.toml`; if not:

```bash
cargo add -p skattr-cli serde_json
```

Update the dispatch `match cli.cmd` to pass `&cli` (not consume it):

```rust
    let cli = Cli::parse();
    match &cli.cmd {
        Command::Init => init(cli.data_dir.as_deref()).await,
        // ...
        Command::Invite { qr } => invite(*qr, &cli).await,
        // ...
    }
```

Because the clap enum is named `Command` and shadows the `skattr_core::daemon::Command`, the above `use` inside `invite` is disambiguated.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-cli tests::render_qr_ascii -- --test-threads=1
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: test green; clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/cli/Cargo.toml crates/cli/src/main.rs Cargo.lock
git commit -m "$(cat <<'EOF'
cli: invite — IPC wire-up + ASCII QR via qrcode crate

Sends Command::CreateInvite, prints the URL (and expires_at + key
package hex). --qr adds a Dense1x2 unicode QR below the URL.
--json emits {url, key_package_id, expires_at}. --qr and --json are
mutually compatible; --json always wins for the top-level stdout.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 25: CLI `add` — wire to `AddContact`

**Files:**
- Modify: `crates/cli/src/main.rs:342-344` (replace the `add` stub)

- [ ] **Step 1: Write the failing test**

No integration-test fixture yet; the end-to-end coverage lands in Task 31. Add a unit test that the clap parser routes `skattr add <link>` to the correct enum variant. Append to tests:

```rust
    #[test]
    fn clap_parses_add_link() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["skattr", "add", "skattr://invite/v1#abc"]).unwrap();
        match cli.cmd {
            Command::Add { link } => assert_eq!(link, "skattr://invite/v1#abc"),
            other => panic!("unexpected variant: {other:?}"),
        }
    }
```

- [ ] **Step 2: Run test to verify it fails (expect PASS since clap already parses)**

```bash
cargo test -p skattr-cli tests::clap_parses_add_link -- --test-threads=1
```
Expected: PASS (clap parsing was correct before). This test is a regression guard that future refactors don't drop the variant.

- [ ] **Step 3: Replace `async fn add`**

```rust
async fn add(link: &str, cli: &Cli) -> Result<()> {
    use skattr_core::daemon::{Command, CommandResult};

    let mut client = connect_or_exit(cli.socket.as_deref()).await?;
    let result = client
        .execute(Command::AddContact { invite_url: link.to_string() })
        .await
        .unwrap_or_else(|e| exit_on_ipc_error(e));

    let summary = match result {
        CommandResult::ContactAdded(s) => s,
        other => anyhow::bail!("unexpected result: {other:?}"),
    };

    if cli.json {
        println!("{}", serde_json::to_string(&summary)?);
    } else {
        println!("Added contact:");
        println!("  pubkey:  {}", short_pubkey(&summary.pubkey));
        println!("  onion:   {}", summary.onion);
        println!("  added:   {}", summary.added_at);
    }
    Ok(())
}

fn short_pubkey(pk: &skattr_core::identity::PublicKey) -> String {
    let hex: String = pk.0.iter().map(|b| format!("{b:02x}")).collect();
    hex
}
```

Update the dispatch match arm:

```rust
        Command::Add { link } => add(link, &cli).await,
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-cli
cargo clippy --all-targets --all-features -- -D warnings
cargo build
```
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/cli/src/main.rs
git commit -m "$(cat <<'EOF'
cli: add — IPC wire-up for Command::AddContact

Sends the URL, prints the ContactSummary. --json emits the summary
as JSON. Errors propagate through exit_on_ipc_error (InviteExpired/
Consumed/SignatureInvalid all map to exit 7).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 26: CLI `contacts` — wire to `ListContacts`, human + JSON output

**Files:**
- Modify: `crates/cli/src/main.rs:347-350` (replace the `contacts` stub)

- [ ] **Step 1: Write the failing test**

Add a unit test for the human-format renderer; the IPC path is covered end-to-end in Task 31.

```rust
    #[test]
    fn render_contacts_human_empty() {
        let out = render_contacts_human(&[]);
        assert_eq!(out.trim(), "No contacts.");
    }

    #[test]
    fn render_contacts_human_one_row() {
        use skattr_core::daemon::ContactSummary;
        let rows = vec![ContactSummary {
            pubkey: skattr_core::identity::PublicKey([0xABu8; 32]),
            nickname: Some("alice".into()),
            onion: "aaaa.onion".into(),
            card_version: 3,
            added_at: 1_700_000_000,
        }];
        let out = render_contacts_human(&rows);
        assert!(out.contains("alice"));
        assert!(out.contains("aaaa.onion"));
        assert!(out.contains("abab")); // pubkey prefix
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p skattr-cli tests::render_contacts_human -- --test-threads=1
```
Expected: compile FAILS — `render_contacts_human` missing.

- [ ] **Step 3: Implement `contacts` + renderer**

Add to `crates/cli/src/main.rs`:

```rust
async fn contacts(cli: &Cli) -> Result<()> {
    use skattr_core::daemon::{Command, CommandResult};

    let mut client = connect_or_exit(cli.socket.as_deref()).await?;
    let result = client
        .execute(Command::ListContacts)
        .await
        .unwrap_or_else(|e| exit_on_ipc_error(e));

    let rows = match result {
        CommandResult::Contacts(rows) => rows,
        other => anyhow::bail!("unexpected result: {other:?}"),
    };

    if cli.json {
        println!("{}", serde_json::to_string(&rows)?);
    } else {
        print!("{}", render_contacts_human(&rows));
    }
    Ok(())
}

fn render_contacts_human(rows: &[skattr_core::daemon::ContactSummary]) -> String {
    use std::fmt::Write;
    if rows.is_empty() {
        return "No contacts.\n".to_string();
    }
    let mut out = String::new();
    for row in rows {
        let short: String = row.pubkey.0.iter().take(4).map(|b| format!("{b:02x}")).collect();
        let name = row.nickname.as_deref().unwrap_or("(unnamed)");
        let _ = writeln!(
            out,
            "{short}  {name:<20}  {onion}  added={added}",
            onion = row.onion,
            added = row.added_at
        );
    }
    out
}
```

Update the dispatch arm:

```rust
        Command::Contacts => contacts(&cli).await,
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-cli tests::render_contacts_human -- --test-threads=1
cargo clippy --all-targets --all-features -- -D warnings
cargo build
```
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/cli/src/main.rs
git commit -m "$(cat <<'EOF'
cli: contacts — IPC wire-up + human/JSON rendering

One line per contact: 4-byte pubkey prefix, nickname or "(unnamed)",
onion, added_at. --json emits Vec<ContactSummary> verbatim. "No
contacts." when the list is empty.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 27: CLI `send` — wire to `SendMessage` + `--fail-on-timeout`

**Files:**
- Modify: `crates/cli/src/main.rs` (extend the `Send` clap variant; replace the stub; add contact-prefix resolver)

- [ ] **Step 1: Write the failing test**

The contact-prefix resolver must call `ListContacts` and return the single full pubkey matching the user's input. Test it in isolation by passing a `Vec<ContactSummary>`:

```rust
    #[test]
    fn resolve_contact_matches_unique_prefix() {
        use skattr_core::daemon::ContactSummary;
        use skattr_core::identity::PublicKey;

        let rows = vec![
            ContactSummary { pubkey: PublicKey([0xAB; 32]), nickname: None, onion: "".into(), card_version: 0, added_at: 0 },
            ContactSummary { pubkey: PublicKey([0xCD; 32]), nickname: None, onion: "".into(), card_version: 0, added_at: 0 },
        ];
        let pk = resolve_contact(&rows, "ab").unwrap();
        assert_eq!(pk.0[0], 0xAB);
    }

    #[test]
    fn resolve_contact_ambiguous_returns_error_with_count() {
        use skattr_core::daemon::ContactSummary;
        use skattr_core::identity::PublicKey;

        let rows = vec![
            ContactSummary { pubkey: PublicKey([0xAB; 32]), nickname: None, onion: "".into(), card_version: 0, added_at: 0 },
            ContactSummary { pubkey: PublicKey({ let mut b = [0xAB; 32]; b[1] = 0xCD; b }), nickname: None, onion: "".into(), card_version: 0, added_at: 0 },
        ];
        let err = resolve_contact(&rows, "ab").unwrap_err();
        assert!(err.to_string().contains("ambiguous"));
    }

    #[test]
    fn resolve_contact_no_match_returns_error() {
        let rows: Vec<skattr_core::daemon::ContactSummary> = vec![];
        let err = resolve_contact(&rows, "ff").unwrap_err();
        assert!(err.to_string().contains("no contact"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p skattr-cli tests::resolve_contact -- --test-threads=1
```
Expected: compile FAILS — `resolve_contact` missing.

- [ ] **Step 3: Add `--fail-on-timeout` + `send` + `resolve_contact`**

Change the `Send` clap variant in `crates/cli/src/main.rs`:

```rust
    /// Send a text message to a contact.
    Send {
        /// Contact identifier (display name or hex prefix of identity pubkey).
        contact: String,
        /// Message body.
        text: String,
        /// Exit with status 8 if the daemon reports Queued (no ACK
        /// within the inline wait). Without this flag the CLI prints
        /// "queued" and exits 0.
        #[arg(long)]
        fail_on_timeout: bool,
    },
```

Add the handler + resolver:

```rust
async fn send(
    contact_prefix: &str,
    text: &str,
    fail_on_timeout: bool,
    cli: &Cli,
) -> Result<()> {
    use skattr_core::daemon::{Command, CommandResult, SendStatus};
    use skattr_core::envelope::Kind;

    let mut client = connect_or_exit(cli.socket.as_deref()).await?;

    // Resolve prefix via ListContacts (server-side). Accept "self"
    // exit paths but not server-side ambiguity decisions — we want
    // consistent exit codes between "no match" and "ambiguous".
    let rows_result = client
        .execute(Command::ListContacts)
        .await
        .unwrap_or_else(|e| exit_on_ipc_error(e));
    let rows = match rows_result {
        CommandResult::Contacts(rows) => rows,
        other => anyhow::bail!("unexpected result: {other:?}"),
    };
    let pubkey = match resolve_contact(&rows, contact_prefix) {
        Ok(pk) => pk,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(6);
        }
    };

    let result = client
        .execute(Command::SendMessage {
            contact: pubkey,
            kind: Kind::Text { body: text.to_string() },
        })
        .await
        .unwrap_or_else(|e| exit_on_ipc_error(e));

    let (msg_id, status) = match result {
        CommandResult::MessageSent { message_id, status } => (message_id, status),
        other => anyhow::bail!("unexpected result: {other:?}"),
    };

    if cli.json {
        let obj = serde_json::json!({
            "message_id": msg_id.to_string(),
            "status": match status {
                SendStatus::Queued => "queued",
                SendStatus::Delivered => "delivered",
            },
        });
        println!("{obj}");
    } else {
        println!(
            "{msg_id}  {state}",
            state = match status { SendStatus::Queued => "queued", SendStatus::Delivered => "delivered" }
        );
    }

    if fail_on_timeout && matches!(status, SendStatus::Queued) {
        std::process::exit(8);
    }
    Ok(())
}

fn resolve_contact(
    rows: &[skattr_core::daemon::ContactSummary],
    prefix: &str,
) -> Result<skattr_core::identity::PublicKey> {
    let lower = prefix.to_ascii_lowercase();
    let mut matches: Vec<&skattr_core::daemon::ContactSummary> = rows
        .iter()
        .filter(|r| {
            let hex: String = r.pubkey.0.iter().map(|b| format!("{b:02x}")).collect();
            hex.starts_with(&lower)
                || r.nickname.as_deref().map_or(false, |n| n.eq_ignore_ascii_case(prefix))
        })
        .collect();
    match matches.len() {
        1 => Ok(matches.remove(0).pubkey),
        0 => anyhow::bail!("no contact matches {prefix:?}"),
        n => anyhow::bail!("ambiguous: {n} contacts match {prefix:?}"),
    }
}
```

Update the dispatch arm:

```rust
        Command::Send { contact, text, fail_on_timeout } => send(contact, text, *fail_on_timeout, &cli).await,
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-cli
cargo clippy --all-targets --all-features -- -D warnings
cargo build
```
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/cli/src/main.rs
git commit -m "$(cat <<'EOF'
cli: send — IPC wire-up, prefix resolver, --fail-on-timeout

Resolves the positional contact arg either by hex-pubkey prefix or
nickname (exact-case-insensitive match). Ambiguous / no-match exit 6.
Delivered prints the message id + "delivered"; Queued prints + exits 0
unless --fail-on-timeout flips that to exit 8. --json output has
{message_id, status}.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 28: CLI `tail` (default: dump recent then exit)

**Files:**
- Modify: `crates/cli/src/main.rs` (extend the `Tail` clap variant; replace the stub; add the renderer)

- [ ] **Step 1: Write the failing test**

Append:

```rust
    #[test]
    fn render_messages_human_empty() {
        let out = render_messages_human(&[]);
        assert_eq!(out.trim(), "No messages.");
    }

    #[test]
    fn render_messages_human_one_text_row() {
        use skattr_core::daemon::{Direction, Hex16, MessageRecord};
        use skattr_core::envelope::Kind;
        use skattr_core::identity::PublicKey;
        let rows = vec![MessageRecord {
            message_id: Hex16::from([2; 16]),
            contact: PublicKey([7; 32]),
            direction: Direction::Incoming,
            kind: Kind::Text { body: "hello".into() },
            mls_generation: 0,
            ts_daemon_recv: 1_700_000_000,
            ts_envelope: 1_699_999_999,
        }];
        let out = render_messages_human(&rows);
        assert!(out.contains("hello"));
        assert!(out.contains("<-")); // incoming arrow
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p skattr-cli tests::render_messages_human -- --test-threads=1
```
Expected: compile FAILS — `render_messages_human` missing.

- [ ] **Step 3: Implement `tail` (non-follow path) + renderer**

Extend the `Tail` clap variant:

```rust
    /// Tail messages. Without --follow: dump most recent N and exit.
    Tail {
        /// Only from this contact (prefix or nickname).
        contact: Option<String>,
        /// Max rows to dump before exiting (only affects non-follow mode).
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Follow: after dumping, stream new MessageReceived events.
        #[arg(long)]
        follow: bool,
    },
```

Add the handler:

```rust
async fn tail(
    contact_prefix: Option<&str>,
    limit: u32,
    follow: bool,
    cli: &Cli,
) -> Result<()> {
    use skattr_core::daemon::{Command, CommandResult};

    if follow {
        return tail_follow(contact_prefix, limit, cli).await; // Task 29
    }

    let mut client = connect_or_exit(cli.socket.as_deref()).await?;
    let target = resolve_optional_contact(&mut client, contact_prefix).await?;

    let result = client
        .execute(Command::RecentMessages { contact: target, limit })
        .await
        .unwrap_or_else(|e| exit_on_ipc_error(e));

    let rows = match result {
        CommandResult::Messages(rows) => rows,
        other => anyhow::bail!("unexpected result: {other:?}"),
    };

    if cli.json {
        println!("{}", serde_json::to_string(&rows)?);
    } else {
        print!("{}", render_messages_human(&rows));
    }
    Ok(())
}

async fn resolve_optional_contact(
    client: &mut skattr_core::daemon::IpcClient<tokio::net::UnixStream>,
    prefix: Option<&str>,
) -> Result<Option<skattr_core::identity::PublicKey>> {
    use skattr_core::daemon::{Command, CommandResult};
    let Some(prefix) = prefix else { return Ok(None) };
    let rows_result = client
        .execute(Command::ListContacts)
        .await
        .unwrap_or_else(|e| exit_on_ipc_error(e));
    let rows = match rows_result {
        CommandResult::Contacts(rows) => rows,
        other => anyhow::bail!("unexpected result: {other:?}"),
    };
    Ok(Some(resolve_contact(&rows, prefix).map_err(|e| {
        eprintln!("{e}");
        std::process::exit(6)
    })?))
}

fn render_messages_human(rows: &[skattr_core::daemon::MessageRecord]) -> String {
    use skattr_core::daemon::Direction;
    use skattr_core::envelope::Kind;
    use std::fmt::Write;

    if rows.is_empty() {
        return "No messages.\n".to_string();
    }

    let mut out = String::new();
    // Render oldest-first on stdout (`recent` returns newest-first).
    for row in rows.iter().rev() {
        let arrow = match row.direction {
            Direction::Incoming => "<-",
            Direction::Outgoing => "->",
        };
        let body = match &row.kind {
            Kind::Text { body } => body.clone(),
            Kind::Reaction { target: _, emoji } => format!("(reaction: {emoji})"),
            Kind::Edit { target: _, body } => format!("(edit) {body}"),
            Kind::Delete { target: _ } => "(delete)".to_string(),
            Kind::File { .. } => "(file)".to_string(),
            Kind::Typing => "(typing)".to_string(),
        };
        let contact_short: String = row.contact.0.iter().take(4).map(|b| format!("{b:02x}")).collect();
        let _ = writeln!(
            out,
            "[{ts}] {arrow} {contact_short} {body}",
            ts = row.ts_daemon_recv
        );
    }
    out
}
```

Update the dispatch arm:

```rust
        Command::Tail { contact, limit, follow } => tail(contact.as_deref(), *limit, *follow, &cli).await,
```

Provide a stub `tail_follow` for now so the module compiles; Task 29 fills it:

```rust
async fn tail_follow(
    _contact_prefix: Option<&str>,
    _limit: u32,
    _cli: &Cli,
) -> Result<()> {
    anyhow::bail!("--follow not yet implemented (Task 29)")
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-cli
cargo clippy --all-targets --all-features -- -D warnings
cargo build
```
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/cli/src/main.rs
git commit -m "$(cat <<'EOF'
cli: tail (non-follow) — IPC wire-up + human/JSON renderer

Default mode: RecentMessages with --limit 50 -> render newest-last
(tail -f mental model). Optional <contact> filter resolves via
ListContacts + prefix/nickname matcher. --json emits
Vec<MessageRecord> verbatim. --follow delegates to a stub added by
Task 29.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 29: CLI `tail --follow` — Subscribe to the event stream

**Files:**
- Modify: `crates/cli/src/main.rs` (replace the `tail_follow` stub with the full implementation)

- [ ] **Step 1: Write the failing test**

No unit test — this is an I/O loop. The integration test at Task 31 covers it. Skip straight to implementation; the regression guard is that `cargo build` + `cargo clippy` + the full integration run (Task 31) all stay green.

- [ ] **Step 2: Verify current state fails the desired behaviour**

```bash
cargo run -p skattr-cli -- tail --follow 2>&1 | head -3
```
Expected: either the daemon-not-running message (exit 3) or the stub error `--follow not yet implemented (Task 29)` if a daemon is up.

- [ ] **Step 3: Implement `tail_follow`**

Replace the stub in `crates/cli/src/main.rs`:

```rust
async fn tail_follow(
    contact_prefix: Option<&str>,
    limit: u32,
    cli: &Cli,
) -> Result<()> {
    use skattr_core::daemon::events::Event;
    use skattr_core::daemon::{Command, CommandResult, EventFilter};
    use skattr_core::envelope::Kind;

    let mut client = connect_or_exit(cli.socket.as_deref()).await?;
    let target = resolve_optional_contact(&mut client, contact_prefix).await?;

    // 1. Dump recent.
    let recent = client
        .execute(Command::RecentMessages { contact: target, limit })
        .await
        .unwrap_or_else(|e| exit_on_ipc_error(e));
    if let CommandResult::Messages(rows) = recent {
        print!("{}", render_messages_human(&rows));
    }

    // 2. Subscribe.
    let filter = match target {
        Some(pk) => EventFilter::Contact(pk),
        None => EventFilter::All,
    };
    client
        .subscribe(filter)
        .await
        .unwrap_or_else(|e| exit_on_ipc_error(e));

    // 3. Stream events until Ctrl-C.
    loop {
        let ev = match client.next_event().await {
            Ok(ev) => ev,
            Err(skattr_core::daemon::IpcClientError::Io(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(other) => exit_on_ipc_error(other),
        };
        match ev {
            Event::MessageReceived { from, envelope } => {
                let short: String = from.0.iter().take(4).map(|b| format!("{b:02x}")).collect();
                let body = match envelope.kind {
                    Kind::Text { body } => body,
                    other => format!("({other:?})"),
                };
                println!("[{ts}] <- {short} {body}", ts = envelope.ts);
            }
            Event::DeliveryStatusChanged { message, status } => {
                let id_hex: String = message.0.iter().map(|b| format!("{b:02x}")).collect();
                println!("... {id_hex} {status:?}");
            }
            Event::ContactUpdated(pk) => {
                let short: String = pk.0.iter().take(4).map(|b| format!("{b:02x}")).collect();
                println!("contact updated: {short}");
            }
            Event::TorStatusChanged(s) => {
                eprintln!("tor: {s:?}");
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo build
cargo clippy --all-targets --all-features -- -D warnings
cargo test -p skattr-cli
```
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/cli/src/main.rs
git commit -m "$(cat <<'EOF'
cli: tail --follow — Subscribe to events after dumping recent

Sequence: RecentMessages (for backfill) -> Subscribe with
Contact(pk) or All filter -> next_event loop until EOF or Ctrl-C.
Each event rendered inline. EOF is treated as clean exit (e.g.
daemon shutdown); everything else routes through exit_on_ipc_error.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 30: CLI `chat` — interactive Subscribe + readline Send loop

**Files:**
- Modify: `crates/cli/src/main.rs` (add the `Chat` clap variant + handler)

- [ ] **Step 1: Write the failing test**

Readline loops are inherently I/O-bound; rely on the Task 31 integration test as the real coverage. Add a regression-guard parse test:

```rust
    #[test]
    fn clap_parses_chat_contact() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["skattr", "chat", "abc12"]).unwrap();
        match cli.cmd {
            Command::Chat { contact } => assert_eq!(contact, "abc12"),
            other => panic!("unexpected variant: {other:?}"),
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-cli tests::clap_parses_chat_contact -- --test-threads=1
```
Expected: compile FAILS — the `Chat` variant does not exist yet.

- [ ] **Step 3: Add the `Chat` variant and handler**

Add to the `Command` clap enum:

```rust
    /// Interactive chat: stream messages from a contact and send lines
    /// typed on stdin.
    Chat {
        /// Contact identifier (prefix or nickname).
        contact: String,
    },
```

Add the handler:

```rust
async fn chat(contact_prefix: &str, cli: &Cli) -> Result<()> {
    use skattr_core::daemon::events::Event;
    use skattr_core::daemon::{Command, CommandResult, EventFilter};
    use skattr_core::envelope::Kind;
    use tokio::io::{AsyncBufReadExt, BufReader};

    let mut client = connect_or_exit(cli.socket.as_deref()).await?;
    let pubkey = {
        let rows_result = client
            .execute(Command::ListContacts)
            .await
            .unwrap_or_else(|e| exit_on_ipc_error(e));
        let rows = match rows_result {
            CommandResult::Contacts(rows) => rows,
            other => anyhow::bail!("unexpected result: {other:?}"),
        };
        resolve_contact(&rows, contact_prefix).map_err(|e| {
            eprintln!("{e}");
            std::process::exit(6);
        })?
    };

    client
        .subscribe(EventFilter::Contact(pubkey))
        .await
        .unwrap_or_else(|e| exit_on_ipc_error(e));

    eprintln!("chat: connected. Type a line and press Enter; Ctrl-D to exit.");

    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut line = String::new();

    loop {
        tokio::select! {
            // Incoming event from the daemon.
            ev = client.next_event() => {
                match ev {
                    Ok(Event::MessageReceived { from: _, envelope }) => {
                        let body = match envelope.kind {
                            Kind::Text { body } => body,
                            other => format!("({other:?})"),
                        };
                        println!("<- {body}");
                    }
                    Ok(Event::DeliveryStatusChanged { message: _, status }) => {
                        eprintln!("... {status:?}");
                    }
                    Ok(_) => {}
                    Err(skattr_core::daemon::IpcClientError::Io(e))
                        if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                    {
                        break;
                    }
                    Err(other) => exit_on_ipc_error(other),
                }
            }
            // User typed a line.
            n = stdin.read_line(&mut line) => {
                let n = n?;
                if n == 0 {
                    // EOF on stdin.
                    break;
                }
                let trimmed = line.trim_end_matches(['\n', '\r']).to_string();
                line.clear();
                if trimmed.is_empty() {
                    continue;
                }
                let res = client
                    .execute(Command::SendMessage {
                        contact: pubkey,
                        kind: Kind::Text { body: trimmed },
                    })
                    .await;
                match res {
                    Ok(CommandResult::MessageSent { status, .. }) => {
                        eprintln!(".. {status:?}");
                    }
                    Ok(other) => eprintln!("unexpected: {other:?}"),
                    Err(e) => exit_on_ipc_error(e),
                }
            }
        }
    }
    Ok(())
}
```

Update the dispatch:

```rust
        Command::Chat { contact } => chat(contact, &cli).await,
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-cli
cargo clippy --all-targets --all-features -- -D warnings
cargo build
```
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/cli/src/main.rs
git commit -m "$(cat <<'EOF'
cli: chat — interactive Subscribe + readline Send loop

Select! between client.next_event() and stdin.read_line(). Empty
lines are skipped, Ctrl-D (EOF on stdin) exits cleanly. Every send
prints its SendStatus on stderr so the reader can tell "queued" from
"delivered" without mixing it into the chat flow on stdout.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 31: Integration test `cli_ipc_roundtrip.rs`

Exercise the IPC codec + server + dispatch against a single daemon using an `IpcClient` without spawning a subprocess. Uses the mocked-transport harness from Phase 1.E so it does not require a real Tor runtime.

**Files:**
- Create: `crates/tests/src/cli_ipc_roundtrip.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/tests/src/cli_ipc_roundtrip.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! End-to-end IPC round-trip with a single daemon.
//!
//! Spins up an IPC server backed by a mocked DaemonHandle (no Tor,
//! no real DeliveryHub I/O), connects an IpcClient, exercises the
//! full Command surface, and asserts the shapes of each CommandResult.

use std::sync::Arc;

use skattr_core::daemon::{
    Command, CommandResult, DaemonHandle, IpcClient,
};
use skattr_core::test_exports::{DeliveryHub, Pool};
use tokio::sync::broadcast;

#[tokio::test]
async fn ipc_list_contacts_round_trip() {
    // Build an in-memory handle with zero contacts seeded.
    let seed = skattr_core::identity::Seed::generate().unwrap();
    let identity = skattr_core::identity::IdentityKey::from_seed(&seed).unwrap();
    let pool = Arc::new(Pool::in_memory());
    let hub: Arc<DeliveryHub<tokio::io::DuplexStream>> = Arc::new(DeliveryHub::new(pool.clone()));
    let (events_tx, _) = broadcast::channel(16);
    let handle = Arc::new(DaemonHandle::new(pool, hub, identity, events_tx.clone()));

    // Pair the server/client over a tokio duplex.
    let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
    let exec = handle.clone();
    let events_tx_clone = events_tx.clone();
    tokio::spawn(async move {
        skattr_core::daemon::ipc::server::handle_connection(
            server_io,
            exec as Arc<dyn skattr_core::daemon::ipc::server::CommandExecutor>,
            events_tx_clone,
        )
        .await;
    });

    let mut client = IpcClient::from_stream(client_io);
    let res = client.execute(Command::ListContacts).await.unwrap();
    assert!(matches!(res, CommandResult::Contacts(ref rows) if rows.is_empty()));
}

#[tokio::test]
async fn ipc_unknown_command_returns_typed_error() {
    use skattr_core::daemon::ipc::wire::IpcError;

    let seed = skattr_core::identity::Seed::generate().unwrap();
    let identity = skattr_core::identity::IdentityKey::from_seed(&seed).unwrap();
    let pool = Arc::new(Pool::in_memory());
    let hub: Arc<DeliveryHub<tokio::io::DuplexStream>> = Arc::new(DeliveryHub::new(pool.clone()));
    let (events_tx, _) = broadcast::channel(16);
    let handle = Arc::new(DaemonHandle::new(pool, hub, identity, events_tx.clone()));

    let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
    let exec = handle.clone();
    let events_tx_clone = events_tx.clone();
    tokio::spawn(async move {
        skattr_core::daemon::ipc::server::handle_connection(
            server_io,
            exec as Arc<dyn skattr_core::daemon::ipc::server::CommandExecutor>,
            events_tx_clone,
        )
        .await;
    });

    let mut client = IpcClient::from_stream(client_io);
    let err = client
        .execute(Command::CreateGroup { members: vec![], name: "x".into() })
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            skattr_core::daemon::IpcClientError::Server(IpcError::UnknownCommand)
        ),
        "got {err:?}"
    );
}
```

Add the module to the tests crate's `crates/tests/src/lib.rs`:

```rust
#[cfg(test)]
mod cli_ipc_roundtrip;
```

The `test_exports` additions in Task 20 must expose `DaemonHandle`, `ipc::server::CommandExecutor`, `ipc::server::handle_connection`. If any are not yet re-exported, append them now:

```rust
    pub use crate::daemon::handle::DaemonHandle;
    pub use crate::daemon::ipc::server::{handle_connection, CommandExecutor};
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p skattr-tests --features test-harness cli_ipc_roundtrip -- --nocapture
```
Expected: either compile error (if exports missing) or both tests fail until the full plan compiles. Once all prior tasks are green, this test passes.

- [ ] **Step 3: Adjust if compile errors**

If the compile fails because `ipc::server` / `ipc::client` / `DaemonHandle` aren't re-exported, add them to `test_exports` in `crates/core/src/lib.rs`. The integration crate has no other way to reach these types.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-tests --features test-harness cli_ipc_roundtrip
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: two tests green.

- [ ] **Step 5: Commit**

```bash
git add crates/tests/src/cli_ipc_roundtrip.rs crates/tests/src/lib.rs crates/core/src/lib.rs
git commit -m "$(cat <<'EOF'
tests: cli_ipc_roundtrip — single-daemon IPC + dispatch E2E

Two tests over tokio::io::duplex: ListContacts on an empty daemon
returns Contacts([]); CreateGroup returns UnknownCommand as a typed
wire error. No Tor, no real sockets — proves the codec + server +
dispatch stack composes without end-to-end transport.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 32: Integration test `cli_two_daemons.rs` — full invite → send → receive

Two daemons A and B, each with its own `DaemonHandle`, exchanging messages via the mocked transport harness from `crates/tests/src/delivery_kill_mid_message.rs`. This is the meat of the 1.F exit criterion.

**Files:**
- Create: `crates/tests/src/cli_two_daemons.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/tests/src/cli_two_daemons.rs`. Base it on the existing `delivery_kill_mid_message.rs` harness (same transport pattern + two-peer setup):

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Two-daemon E2E: invite -> add -> send -> receive, all over the
//! mocked-transport harness from Phase 1.E.

use std::sync::Arc;

use skattr_core::daemon::{
    Command, CommandResult, DaemonHandle, EventFilter, IpcClient,
};
use skattr_core::daemon::events::Event;
use skattr_core::envelope::Kind;
use skattr_core::test_exports::{DeliveryHub, Pool};
use tokio::sync::broadcast;

// Spawn a DaemonHandle-backed IPC server and return a connected
// IpcClient. The returned server_task is aborted on drop via the
// returned JoinHandle.
fn spawn_daemon_with_client() -> (IpcClient<tokio::io::DuplexStream>, Arc<DaemonHandle<tokio::io::DuplexStream>>) {
    let seed = skattr_core::identity::Seed::generate().unwrap();
    let identity = skattr_core::identity::IdentityKey::from_seed(&seed).unwrap();
    let pool = Arc::new(Pool::in_memory());
    let hub: Arc<DeliveryHub<tokio::io::DuplexStream>> = Arc::new(DeliveryHub::new(pool.clone()));
    let (events_tx, _) = broadcast::channel(16);
    let handle = Arc::new(DaemonHandle::new(pool, hub, identity, events_tx.clone()));
    handle.set_onion("alice.onion".to_string());

    let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
    let exec = handle.clone();
    let events_tx_clone = events_tx.clone();
    tokio::spawn(async move {
        skattr_core::daemon::ipc::server::handle_connection(
            server_io,
            exec as Arc<dyn skattr_core::daemon::ipc::server::CommandExecutor>,
            events_tx_clone,
        )
        .await;
    });
    (IpcClient::from_stream(client_io), handle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_flow_invite_add_send_receive() {
    let (mut client_a, handle_a) = spawn_daemon_with_client();
    let (mut client_b, handle_b) = spawn_daemon_with_client();

    // Wire up the mocked transport between the two hubs. Re-uses the
    // 1.E pattern: a tokio::io::duplex pair with Noise_XK handshakes
    // in both directions, handed to each hub's PeerConnection actor
    // via `DeliveryHub::accept_connection`.
    skattr_core::test_exports::wire_hubs_duplex(
        &handle_a.hub,
        &handle_b.hub,
        &handle_a.identity,
        &handle_b.identity,
    )
    .await;

    // Alice creates an invite.
    let invite_url = match client_a
        .execute(Command::CreateInvite { nickname: None, ttl_secs: Some(3600) })
        .await
        .unwrap()
    {
        CommandResult::InviteCreated { url, .. } => url,
        other => panic!("expected InviteCreated, got {other:?}"),
    };

    // Bob adds it.
    let bob_view_of_alice = match client_b
        .execute(Command::AddContact { invite_url })
        .await
        .unwrap()
    {
        CommandResult::ContactAdded(s) => s,
        other => panic!("expected ContactAdded, got {other:?}"),
    };

    // Bob subscribes for incoming messages.
    client_b
        .subscribe(EventFilter::Contact(bob_view_of_alice.pubkey))
        .await
        .unwrap();

    // Alice sends "hello".
    let _ = client_a
        .execute(Command::SendMessage {
            contact: bob_view_of_alice.pubkey,
            kind: Kind::Text { body: "hello".into() },
        })
        .await
        .unwrap();

    // Bob receives MessageReceived with body "hello".
    let ev = tokio::time::timeout(std::time::Duration::from_secs(10), client_b.next_event())
        .await
        .expect("message within 10 s")
        .unwrap();
    match ev {
        Event::MessageReceived { envelope, .. } => match envelope.kind {
            Kind::Text { body } => assert_eq!(body, "hello"),
            other => panic!("expected Text, got {other:?}"),
        },
        other => panic!("expected MessageReceived, got {other:?}"),
    }

    // Bob's RecentMessages contains the plaintext.
    let rows = match client_b
        .execute(Command::RecentMessages {
            contact: Some(bob_view_of_alice.pubkey),
            limit: 10,
        })
        .await
        .unwrap()
    {
        CommandResult::Messages(r) => r,
        other => panic!("expected Messages, got {other:?}"),
    };
    assert!(rows.iter().any(|r| matches!(&r.kind, Kind::Text { body } if body == "hello")));
}
```

Add a helper `wire_hubs_duplex` in `test_exports` (the Phase 1.E plan already sets up peer-to-peer hub wiring via `DeliveryHub::accept`; this wrapper just exposes it to crates/tests under a stable name):

```rust
// crates/core/src/lib.rs test_exports
pub async fn wire_hubs_duplex(
    hub_a: &crate::delivery::DeliveryHub<tokio::io::DuplexStream>,
    hub_b: &crate::delivery::DeliveryHub<tokio::io::DuplexStream>,
    identity_a: &crate::identity::IdentityKey,
    identity_b: &crate::identity::IdentityKey,
) {
    // Create a duplex pair and run the Noise_XK handshake in both
    // directions using the same primitives that
    // `crates/tests/src/delivery_kill_mid_message.rs` already uses.
    // Ownership: each AuthenticatedConnection half is handed to the
    // matching hub's PeerConnection actor via the hub's public
    // `accept_connection(peer_pubkey, conn)` entry-point from 1.E.
    let (stream_a, stream_b) = tokio::io::duplex(1024 * 1024);

    let peer_a_static = identity_a.noise_static_public();
    let peer_b_static = identity_b.noise_static_public();

    let (auth_a, auth_b) = tokio::try_join!(
        crate::transport::handshake_initiator(stream_a, identity_a, &peer_b_static),
        crate::transport::handshake_responder(stream_b, identity_b, &peer_a_static),
    )
    .expect("duplex Noise handshake must succeed");

    // `accept_connection` is the 1.E hub method that installs a live
    // AuthenticatedConnection into the per-peer actor. If 1.E named it
    // differently, use whatever 1.E's `delivery_kill_mid_message.rs`
    // test already calls — do NOT invent a new public API.
    hub_b.accept_connection(
        crate::identity::PublicKey(peer_a_static),
        auth_a,
    ).await;
    hub_a.accept_connection(
        crate::identity::PublicKey(peer_b_static),
        auth_b,
    ).await;
}
```

The body of `wire_hubs_duplex` must be fleshed out by copying the hub-wiring logic from `crates/tests/src/delivery_kill_mid_message.rs` (which already does this work outside any `test_exports` helper). Keep the wrapper small — the goal is one import in `cli_two_daemons.rs`.

Add the module to `crates/tests/src/lib.rs`:

```rust
#[cfg(test)]
mod cli_two_daemons;
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p skattr-tests --features test-harness cli_two_daemons -- --nocapture
```
Expected: the test fails — either on the `wire_hubs_duplex` stub or on a subsequent step. Implement the wiring until it passes.

- [ ] **Step 3: Implement `wire_hubs_duplex` concretely**

Port the duplex-pair + two-peer wiring from `crates/tests/src/delivery_kill_mid_message.rs` into the `test_exports` helper. The goal: after the call, `hub_a` can `send(peer_b, …)` and `hub_b`'s `DaemonInbound` sees the ciphertext (and vice versa).

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p skattr-tests --features test-harness cli_two_daemons
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: the full-flow test passes within the 10 s inline budget.

- [ ] **Step 5: Commit**

```bash
git add crates/tests/src/cli_two_daemons.rs crates/tests/src/lib.rs crates/core/src/lib.rs
git commit -m "$(cat <<'EOF'
tests: cli_two_daemons — full invite -> add -> send -> receive flow

Two DaemonHandle-backed IPC servers with their delivery hubs wired
over tokio::io::duplex (the 1.E pattern). Alice creates an invite;
Bob AddContact consumes it; Alice sends "hello"; Bob's Subscribe
stream and RecentMessages both see the plaintext. The Phase 1.F exit
criterion's headline test — no Tor required.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 33: Integration test `cli_real_tor.rs` — same flow over real Arti

**Files:**
- Create: `crates/tests/src/cli_real_tor.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/tests/src/cli_real_tor.rs`. Unlike Task 32, this one spins up two real `Daemon::run` instances with Arti bootstrapping, and routes CLI calls through their real IPC sockets. It is `#[ignore]`-gated because Arti bootstrap takes 30–120 s and needs network access.

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Two-daemon E2E over real Arti. Ignored by default — run with
//! `cargo test -p skattr-tests --release -- --ignored cli_real_tor`.

use std::path::Path;

use skattr_core::daemon::{Command, CommandResult, Config, Daemon, IpcClient, Ready};
use skattr_core::envelope::Kind;
use tokio::sync::oneshot;
use zeroize::Zeroizing;

async fn spawn_real_daemon(data_dir: &Path) -> (Ready, oneshot::Sender<()>, tokio::task::JoinHandle<skattr_core::error::Result<()>>) {
    std::fs::create_dir_all(data_dir).unwrap();
    let seed = skattr_core::identity::Seed::generate().unwrap();
    let identity = skattr_core::identity::IdentityKey::from_seed(&seed).unwrap();
    let pw = Zeroizing::new("real-tor-passphrase-xyz".into());
    skattr_core::identity::Vault::create(&data_dir.join("identity.vault"), identity, pw.as_str()).unwrap();

    let mut config = Config::defaults().unwrap();
    config.data_dir = data_dir.to_path_buf();
    config.ipc_socket = Some(data_dir.join("daemon.sock"));

    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let shutdown_fut = async move {
        let _ = shutdown_rx.await;
    };

    let data_dir_owned = data_dir.to_path_buf();
    let config_owned = config.clone();
    let task = tokio::spawn(async move {
        Daemon::run(&data_dir_owned, &pw, config_owned, ready_tx, shutdown_fut).await
    });

    let ready = tokio::time::timeout(std::time::Duration::from_secs(180), ready_rx)
        .await
        .expect("daemon bootstraps within 180 s")
        .expect("ready_tx still open");
    (ready, shutdown_tx, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real Tor bootstrap; run with --ignored"]
async fn full_flow_over_real_tor() {
    let tmp_a = tempfile::tempdir().unwrap();
    let tmp_b = tempfile::tempdir().unwrap();
    let (ready_a, shutdown_a, task_a) = spawn_real_daemon(tmp_a.path()).await;
    let (ready_b, shutdown_b, task_b) = spawn_real_daemon(tmp_b.path()).await;

    let mut client_a = IpcClient::connect(&ready_a.ipc_socket).await.unwrap();
    let mut client_b = IpcClient::connect(&ready_b.ipc_socket).await.unwrap();

    // Alice -> invite.
    let invite_url = match client_a
        .execute(Command::CreateInvite { nickname: None, ttl_secs: Some(3600) })
        .await
        .unwrap()
    {
        CommandResult::InviteCreated { url, .. } => url,
        other => panic!("expected InviteCreated, got {other:?}"),
    };

    // Bob -> add. This may take several seconds as the MLS Welcome
    // traverses the real Tor circuit.
    let contact = match tokio::time::timeout(
        std::time::Duration::from_secs(60),
        client_b.execute(Command::AddContact { invite_url }),
    )
    .await
    .expect("AddContact completes within 60 s")
    .unwrap()
    {
        CommandResult::ContactAdded(s) => s,
        other => panic!("expected ContactAdded, got {other:?}"),
    };

    // Alice -> send.
    let _ = client_a
        .execute(Command::SendMessage {
            contact: contact.pubkey,
            kind: Kind::Text { body: "hello-over-tor".into() },
        })
        .await
        .unwrap();

    // Bob -> eventually sees it in RecentMessages.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let rows = match client_b
            .execute(Command::RecentMessages { contact: Some(contact.pubkey), limit: 10 })
            .await
            .unwrap()
        {
            CommandResult::Messages(r) => r,
            other => panic!("expected Messages, got {other:?}"),
        };
        if rows.iter().any(|r| matches!(&r.kind, Kind::Text { body } if body == "hello-over-tor")) {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!("message not delivered within 120 s");
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    let _ = shutdown_a.send(());
    let _ = shutdown_b.send(());
    let _ = task_a.await;
    let _ = task_b.await;
}
```

Add the module to `crates/tests/src/lib.rs`:

```rust
#[cfg(test)]
mod cli_real_tor;
```

- [ ] **Step 2: Run the test in ignored mode**

```bash
cargo test -p skattr-tests --features test-harness --release -- --ignored cli_real_tor
```
Expected: takes 60–240 s depending on Tor circuit luck; passes.

- [ ] **Step 3: If it fails**, diagnose via `RUST_LOG=skattr_core=debug,arti=info cargo test ...`. Do NOT chase intermittent failures in this plan — follow `superpowers:systematic-debugging`. The test is `#[ignore]`d so CI never sees it.

- [ ] **Step 4: Confirm other tests still green**

```bash
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: green (default tests exclude the ignored one).

- [ ] **Step 5: Commit**

```bash
git add crates/tests/src/cli_real_tor.rs crates/tests/src/lib.rs
git commit -m "$(cat <<'EOF'
tests: cli_real_tor — full flow over real Arti, #[ignore]-gated

Two Daemon::run instances with real Arti bootstraps and real Unix
sockets. Run with: cargo test -p skattr-tests --features test-harness
--release -- --ignored cli_real_tor. Counts toward the Phase 1 exit
criterion "two users on different networks exchange messages via
CLI" when paired with delivery_real_tor.rs.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 34: Update `CHANGELOG.md` and `CLAUDE.md` status lines

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `CLAUDE.md`

- [ ] **Step 1: Write CHANGELOG entry**

At the top of `CHANGELOG.md` (if the file exists; create it if not), prepend a new Phase 1.F section. Check first:

```bash
ls CHANGELOG.md 2>/dev/null && head -20 CHANGELOG.md
```

Create or prepend:

```markdown
## Phase 1.F — CLI integration (2026-04-23)

- Persistent `skattr daemon` owns `Pool` + `DeliveryHub` + `OnionListener` + IPC server.
- New `daemon::ipc` submodule: CBOR length-prefix codec, Unix-socket server with `0600` perms and `SO_PEERCRED`/`getpeereid` peer-cred check, per-connection state machine that lets `Subscribe` coexist with further `Execute`s (powers `skattr chat`), `IpcClient` for the CLI.
- `Daemon::run` signature changed: now takes `Config` and returns via `Ready { onion, ipc_socket }`.
- Migration 0005 adds `contacts.group_id` (plus index); `AddContact` populates it atomically.
- New wire-safe types: `ContactSummary`, `MessageRecord`, `SendStatus`, `Direction`, `Hex16`, `Hex32`, `EventFilter`, `IpcError`, `DaemonErrorKind`.
- Every CLI stub (`invite`, `add`, `contacts`, `send`, `tail`, new `chat`) now wires through IPC. `init`/`restore`/`backup` remain in-process.
- `skattr daemon` prompt moved to `/dev/tty` via `rpassword`; `--passphrase-file <path>` / `$SKATTR_PASSPHRASE_FILE` for automation.
- `skattr invite --qr` renders an ASCII QR via the `qrcode` crate.
- `skattr send --fail-on-timeout` flips the 2 s inline-wait default from "exit 0 with `status=queued`" to "exit 8".
- Integration tests: `cli_ipc_roundtrip.rs` (mocked transport), `cli_two_daemons.rs` (full invite→send→receive, mocked transport), `cli_real_tor.rs` (`#[ignore]`-gated, real Arti).
```

- [ ] **Step 2: Update `CLAUDE.md` status line**

Edit the "Repository state" section of `CLAUDE.md` — change the line starting with "**Phase 0 is complete…**" to extend the Phase 1 summary with the 1.F line. Replace:

```
Phase 0 is complete; Phase 1.A (frame codec), 1.B (Noise_XK
handshake), 1.C (MLS 2-member groups), 1.D (invite & contact
flow), and 1.E (delivery semantics) are done.
```

with:

```
Phase 0 is complete; Phase 1.A (frame codec), 1.B (Noise_XK
handshake), 1.C (MLS 2-member groups), 1.D (invite & contact
flow), 1.E (delivery semantics), and 1.F (CLI integration) are done.
```

And append a 1.F paragraph after the 1.E paragraph:

```
Phase 1.F added the `skattr daemon` IPC server + `IpcClient`, expanded
`Daemon::run` to own `Pool` + `DeliveryHub` + IPC, introduced
`DaemonHandle` + `dispatch::execute_command`, migration 0005
(`contacts.group_id`), `DaemonInbound` (MLS decrypt + persist + emit
`Event::MessageReceived`), `/dev/tty` passphrase prompts (rpassword),
`--passphrase-file` automation, `--qr` invite rendering,
`--fail-on-timeout` on `send`, and three integration tests
(`cli_ipc_roundtrip`, `cli_two_daemons`, `cli_real_tor` `#[ignore]`-gated).
```

Also update the "Phase 1 continues" bullet near the bottom to remove 1.F:

```
Phase 1 continues with 1.G message storage & search — see
`docs/superpowers/specs/2026-04-21-phase-1-decomposition.md`.
```

- [ ] **Step 3: Commit**

```bash
git add CHANGELOG.md CLAUDE.md
git commit -m "$(cat <<'EOF'
docs: CHANGELOG + CLAUDE.md — Phase 1.F complete

Records the persistent-daemon IPC wire-up, migration 0005, the
expanded Command/Event surface, and the three new integration tests.
Phase 1 now has 1.G (message storage + search) as the only remaining
sub-project.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 35: Refresh `docs/ARCHITECTURE.md` "send one message" trace

**Files:**
- Modify: `docs/ARCHITECTURE.md`

- [ ] **Step 1: Locate the existing trace**

```bash
grep -n "send one message\|send-one-message" docs/ARCHITECTURE.md | head -20
```
Expected: a section heading exists (added in Phase 0.E). If the file has no such section, add one under "Data flow" or equivalent.

- [ ] **Step 2: Update the trace to include the IPC hop**

Replace the existing trace (or add under the relevant heading) with:

```markdown
## "skattr send <contact> <text>" end-to-end trace

1. **CLI process** parses argv via `clap`; resolves the IPC socket path
   (`--socket` > `$SKATTR_SOCKET` > `$XDG_RUNTIME_DIR/skattr/daemon.sock`).
2. CLI `UnixStream::connect`s the socket; daemon's IPC server accepts,
   calls `SO_PEERCRED`, rejects non-matching UIDs.
3. CLI resolves the positional contact via `Command::ListContacts` →
   prefix match (hex pubkey or nickname).
4. CLI sends `Command::SendMessage { contact, kind: Kind::Text { body } }`
   as a length-prefixed CBOR frame.
5. Daemon's `dispatch::send_message`:
   a. reads `contacts.group_id` for the peer,
   b. loads the MLS `Group` via `MlsGroupRepo::get`,
   c. builds an `Envelope { ts, message_id, kind }`,
   d. `Group::encrypt` → ciphertext,
   e. `OutboxRepo::insert(target, message_id, ciphertext)` (idempotent),
   f. `DeliveryHub::send` kicks the per-peer actor.
6. `PeerConnection` actor either uses its live `AuthenticatedConnection`
   or dials the peer's cached onion, completes `Noise_XK` handshake,
   sends a length-prefixed `FrameType::MlsApp` frame.
7. Remote peer's accept loop (`OnionListener` inside `Daemon::run`) feeds
   the stream into its own `DeliveryHub`, which routes into
   `DaemonInbound::dispatch` → MLS decrypt → `MessageRepo::insert` →
   `events_tx.send(Event::MessageReceived { from, envelope })`.
8. Remote CLI running `skattr tail --follow` or `skattr chat` receives
   the event frame and prints the plaintext.
9. Remote `PeerConnection` sends back `FrameType::Ack { message_id }`; the
   sender actor fulfils its `oneshot::Sender<Result<(), ()>>`, local
   `dispatch::send_message`'s `tokio::time::timeout(2s, ..)` resolves,
   `CommandResult::MessageSent { status: Delivered }` is written to the
   CLI's IPC socket.
10. CLI prints `<message_id>  delivered` and exits 0.
```

- [ ] **Step 3: Commit**

```bash
git add docs/ARCHITECTURE.md
git commit -m "$(cat <<'EOF'
docs: ARCHITECTURE.md — "send one message" trace with IPC hop

Traces every layer (CLI -> Unix socket -> IPC server -> dispatch ->
MLS encrypt -> Outbox -> DeliveryHub -> PeerConnection -> Noise_XK
over Tor -> remote hub -> DaemonInbound -> MLS decrypt -> MessageRepo
-> Event::MessageReceived -> remote CLI) so a new contributor can
orient themselves without reading the four design docs end-to-end.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 36: Final green build + merge checklist

- [ ] **Step 1: Full workspace verification**

From the worktree:
```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo deny check
cargo build --release
```
All five must pass. `cargo deny check` may surface `qrcode` / `rpassword` license notes — both are MIT-OR-Apache-2.0 (already on the allowlist); if `deny.toml` needs an entry, add it and commit with `build: allow qrcode and rpassword in cargo-deny`.

- [ ] **Step 2: Manual smoke test**

In three terminals from the worktree root:
```bash
# Terminal 1: Alice's daemon
export SKATTR_DATA_DIR=/tmp/skattr-alice
mkdir -p $SKATTR_DATA_DIR
./target/release/skattr --data-dir $SKATTR_DATA_DIR init
# type passphrase, note seed phrase
./target/release/skattr --data-dir $SKATTR_DATA_DIR daemon
# leave running

# Terminal 2: Bob's daemon
export SKATTR_DATA_DIR=/tmp/skattr-bob
mkdir -p $SKATTR_DATA_DIR
./target/release/skattr --data-dir $SKATTR_DATA_DIR init
./target/release/skattr --data-dir $SKATTR_DATA_DIR daemon
# leave running

# Terminal 3: CLI interaction
# As Alice:
./target/release/skattr --socket /tmp/skattr-alice-ipc/daemon.sock invite
# copy the skattr://invite/v1#... URL

# As Bob:
./target/release/skattr --socket /tmp/skattr-bob-ipc/daemon.sock add '<url>'
./target/release/skattr --socket /tmp/skattr-bob-ipc/daemon.sock contacts

# As Alice:
./target/release/skattr --socket /tmp/skattr-alice-ipc/daemon.sock send <bob-prefix> "hello"

# As Bob:
./target/release/skattr --socket /tmp/skattr-bob-ipc/daemon.sock tail <alice-prefix>
```
Expected: Bob's `tail` shows Alice's "hello". Both daemons exit cleanly on Ctrl-C and remove their socket files.

(Use whichever socket paths are produced by each daemon's startup banner — the `--socket` value must match.)

- [ ] **Step 3: Run `verification-before-completion` discipline**

Invoke `superpowers:verification-before-completion` and follow it exactly. Do not claim the plan is complete until you've pasted the actual `cargo fmt --check`, `cargo clippy`, and `cargo test` outputs back into the conversation.

- [ ] **Step 4: Create the merge PR**

```bash
git push -u origin phase-1f-cli-integration
gh pr create --title "Phase 1.F: CLI integration" --body "$(cat <<'EOF'
## Summary

- Persistent `skattr daemon` owns `Pool` + `DeliveryHub` + `OnionListener` + IPC server.
- New `daemon::ipc` submodule (CBOR length-prefix codec, `SO_PEERCRED`/`getpeereid` auth, Subscribe + Execute per-connection state machine).
- Every stateful CLI command (`invite`, `add`, `contacts`, `send`, `tail`, `chat`) now wires through IPC.
- Migration 0005 (`contacts.group_id`).
- `/dev/tty` passphrase prompt + `--passphrase-file` automation.
- Integration tests: `cli_ipc_roundtrip`, `cli_two_daemons`, `cli_real_tor` (`#[ignore]`).

Spec: `docs/superpowers/specs/2026-04-23-phase-1f-cli-integration-design.md`.
Plan: `docs/superpowers/plans/2026-04-23-phase-1f-cli-integration.md`.

## Test plan

- [ ] `cargo fmt --all --check` green
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` green
- [ ] `cargo test --all-features` green
- [ ] `cargo deny check` green
- [ ] `cargo test -p skattr-tests --features test-harness --release -- --ignored cli_real_tor` green on one developer machine
- [ ] Manual smoke test (two daemons, invite → add → send → tail) succeeds

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 5: After merge**

```bash
# From the main checkout (not the worktree):
cd /home/myggiz/development/skattr
git fetch origin
git checkout master
git merge --ff-only origin/master
# Remove the worktree:
git worktree remove ../skattr-phase-1f-cli-integration
git branch -d phase-1f-cli-integration
```

- [ ] **Step 6: Emit the next-phase kickoff prompt**

Phase 1.F is the last sub-project before Phase 1.G (message storage & search). End the completion response with a copy-pasteable kickoff prompt for 1.G, covering the brainstorm → design-spec → implementation-plan → execution sequence, citing:
- `docs/superpowers/specs/2026-04-21-phase-1-decomposition.md` §1.G (exit criteria: FTS5 table populated; `messages::recent / search / unread_count / export` APIs; `skattr tail` / `skattr search`)
- `docs/skattr-implementation-plan.md` §Phase 1
- `docs/skattr-deep-dives.md` (MLS state machine; message history invariants)
- `CLAUDE.md` (locked decisions, module visibility, dep pinning, logging rules)

The user's memory entry `feedback_phase_handoff_prompt.md` requires this handoff prompt; do not skip it.

