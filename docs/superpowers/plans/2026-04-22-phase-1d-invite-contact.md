# Phase 1.D Invite & Contact Flow Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Alice mints a signed `skattr://invite/v1#...` URL; Bob parses it, verifies the Ed25519 signature, records the bundled KeyPackage in his local `KeyPackageRepo`, and (via `mark_consumed`) can prove single-use enforcement. Alongside: `ContactCard::{sign, verify}` with monotonic version semantics, persisted through `storage::contacts` by a new `contact_cards` table.

**Architecture:** Split every signable object into a `Body` (unsigned fields, serde-derived) and a wrapper that adds the `signature`. Canonical CBOR over the body is the signed form; `ciborium::ser::into_writer` is deterministic for a fixed struct layout. Single-use enforcement reuses Phase 1.C's `KeyPackageRepo.consumed` flag keyed by SHA-256 of the KeyPackage bytes — zero new tables for invites. `ContactCard` persistence lands in a new `contact_cards(contact_id, version)` table via migration 0003, with `put_card` rejecting any version that isn't strictly greater than the latest stored.

**Tech Stack:** Rust 2021, `ciborium` (canonical CBOR), `base32 = "0.5"` (RFC 4648 lowercase, no padding), `base64 = "0.22"` (`URL_SAFE_NO_PAD`), `sha2` (KP hash), `ed25519-dalek` (identity signing), `rusqlite` (migration 0003 + card repo), `qrcode = "0.14"` (SVG render, feature-gated on `qr`), `zeroize` (InvitePsk guard).

**Design spec:** `docs/superpowers/specs/2026-04-22-phase-1d-invite-contact-design.md` — read this first.

---

## Pre-flight

```bash
cd /home/myggiz/development/skattr-phase-1d-invite-contact
. "$HOME/.cargo/env"

cargo build --workspace
cargo test --workspace --release
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

All four must pass before starting Task 1. Worktree is branched from `master` at `8e9f9e7` (Phase 1.C merge); Phase 0 through 1.C state is in place — 153 unit + 10 integration tests passing.

**Cargo isn't on system PATH.** Prefix every shell command with `. "$HOME/.cargo/env" &&`.

---

## File structure

```
crates/core/src/invite/mod.rs              MODIFY: re-export InviteLink + InvitePsk
crates/core/src/invite/link.rs             REWRITE: InviteLinkBody + InviteLink full
crates/core/src/invite/qr.rs               REWRITE: render_svg only; render_png removed

crates/core/src/contact/card.rs            REWRITE: ContactCardBody + ContactCard full
crates/core/src/contact/mod.rs             NO CHANGE
crates/core/src/contact/contact.rs         NO CHANGE
crates/core/src/contact/rotation.rs        NO CHANGE (stub stays; Phase 2)

crates/core/src/storage/contacts.rs        MODIFY: put_card / latest_card; hydrate
                                                   Contact.card in get / list
crates/core/src/storage/migrations/
  0003_contact_cards.sql                   CREATE
crates/core/src/storage/migrations.rs      MODIFY: register 0003; extend expected-
                                                   tables assertion

crates/core/src/lib.rs                     MODIFY: test_exports += InviteLink,
                                                   ContactCard, ContactRepo

crates/tests/src/invite_roundtrip.rs       CREATE: integration test
crates/tests/src/lib.rs                    MODIFY: declare invite_roundtrip module

CHANGELOG.md                               MODIFY: bullet under [Unreleased]
CLAUDE.md                                  MODIFY: Repository-state paragraph
```

No workspace `Cargo.toml` edits. All deps (`base32`, `base64`, `ciborium`, `qrcode`, `sha2`, `serde_bytes`, `zeroize`) already in place.

---

## Task 1: Pre-flight + cleanup + test_exports scaffold

**Goal:** Confirm the worktree is green, delete the `render_png` stub from `qr.rs`, stub-rewrite `invite/link.rs` and `contact/card.rs` with the new body/wrapper shapes (methods `todo!()`), and add a test-harness re-export for the new items.

**Files:**
- Modify: `crates/core/src/invite/qr.rs`
- Modify: `crates/core/src/invite/link.rs`
- Modify: `crates/core/src/invite/mod.rs`
- Modify: `crates/core/src/contact/card.rs`
- Modify: `crates/core/src/lib.rs`

- [ ] **Step 1: Pre-flight**

```bash
cargo build --workspace
cargo test --workspace --release
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

All green. If any fails, STOP and report BLOCKED.

- [ ] **Step 2: Rewrite `invite/link.rs` to the stub shape**

Replace the entire contents of `crates/core/src/invite/link.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Invite link parsing, generation, signing, and verification.
//!
//! Wire layout (fragment-encoded, per design §1.4):
//!
//! ```text
//! skattr://invite/v1#id=<base32(identity_pubkey)>
//!                   &onion=<56-char onion address>
//!                   &kp=<base64url(MLS KeyPackage)>
//!                   &psk=<base64url(32-byte one-time secret)>
//!                   &exp=<unix timestamp>
//!                   &sig=<base64url(Ed25519 signature over canonical CBOR of body)>
//! ```
//!
//! Both `generate` and `from_url` return a validated [`InviteLink`] —
//! the signature is verified before the type is constructed.

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::Result;
use crate::identity::{IdentityKey, PublicKey, Signature};
use crate::storage::KeyPackageRepo;

/// Content that the inviter signs. Deliberately excludes the signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteLinkBody {
    /// Inviter's long-term Ed25519 identity.
    pub identity: PublicKey,
    /// Onion service to dial for first contact.
    pub onion: String,
    /// Single-use MLS KeyPackage (binary, TLS-codec bytes from 1.C).
    #[serde(with = "serde_bytes")]
    pub key_package: Vec<u8>,
    /// 32-byte one-time secret mixed into Noise PSK + first MLS Commit.
    pub psk: [u8; 32],
    /// Unix timestamp (seconds) after which the invite is invalid.
    pub expires_at: i64,
}

/// Parsed + verified invite link.
pub struct InviteLink {
    /// Unsigned body fields. `body.psk` is zeroized after parse; read
    /// the PSK via `self.psk` (the Zeroizing guard).
    pub body: InviteLinkBody,
    /// Ed25519 signature over canonical CBOR of `body`.
    pub signature: Signature,
    /// Zeroizing copy of the PSK.
    pub psk: InvitePsk,
}

/// A 32-byte one-time secret embedded in an invite.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct InvitePsk(pub [u8; 32]);

impl InviteLink {
    /// Build + sign a new invite.
    pub fn generate(
        _inviter: &IdentityKey,
        _onion: String,
        _key_package: Vec<u8>,
        _psk: [u8; 32],
        _ttl_secs: u64,
        _now: i64,
    ) -> Result<Self> {
        todo!("Task 7")
    }

    /// Parse + verify a `skattr://invite/v1#...` URL.
    pub fn from_url(_url: &str, _now: i64) -> Result<Self> {
        todo!("Task 9")
    }

    /// Re-serialize to a URL.
    pub fn to_url(&self) -> Result<String> {
        todo!("Task 8")
    }

    /// SHA-256 of `body.key_package`.
    pub fn kp_hash(&self) -> [u8; 32] {
        todo!("Task 10")
    }

    /// Record this received invite's KP under `direction='theirs'`.
    pub fn record_received(&self, _kp_repo: &KeyPackageRepo<'_>) -> Result<()> {
        todo!("Task 10")
    }

    /// Whether this invite's KP has been marked consumed in the repo.
    pub fn is_consumed(&self, _kp_repo: &KeyPackageRepo<'_>) -> Result<bool> {
        todo!("Task 10")
    }

    /// Flip `consumed=1` for this invite's KP.
    pub fn mark_consumed(&self, _kp_repo: &KeyPackageRepo<'_>) -> Result<()> {
        todo!("Task 10")
    }
}
```

- [ ] **Step 3: Rewrite `invite/mod.rs`**

Replace the contents of `crates/core/src/invite/mod.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Out-of-band contact exchange via signed invite links.
//!
//! The URI scheme is `skattr://invite/v1#<params>`. Parameters are in
//! the URL fragment so that if a link leaks through a web browser or
//! chat tool, none of them end up in Referer headers or access logs.
//!
//! See design §1.4 for the exact field semantics.

pub mod link;

#[cfg(feature = "qr")]
pub mod qr;

pub use link::{InviteLink, InviteLinkBody, InvitePsk};
```

- [ ] **Step 4: Rewrite `invite/qr.rs` (remove `render_png`)**

Replace the contents of `crates/core/src/invite/qr.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! QR code rendering for invite links (feature-gated on `qr`).
//!
//! 1.D ships SVG only. A PNG path can be added later; for the CLI +
//! Tauri consumers we already have, SVG is sufficient.

use crate::error::Result;
use crate::invite::InviteLink;

/// Render an [`InviteLink`] to SVG markup.
///
/// Error correction level is `M` (15% tolerance); invite URLs are short
/// enough that `L` is not worth the resilience tradeoff.
pub fn render_svg(_invite: &InviteLink) -> Result<String> {
    todo!("Task 11")
}
```

- [ ] **Step 5: Rewrite `contact/card.rs` to the stub shape**

Replace the entire contents of `crates/core/src/contact/card.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! `ContactCard`: signed, versioned self-published routing record.
//!
//! When a user rotates their onion address or their mailbox list, they
//! publish a new `ContactCard` with a monotonically higher `version`,
//! signed by their identity key. Peers reject cards whose version is
//! not strictly greater than the last verified version for that
//! identity (monotonic replay resistance) — enforced by
//! [`crate::storage::contacts::ContactRepo::put_card`].

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::identity::{IdentityKey, PublicKey, Signature};

/// Content the owner signs. Excludes the signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactCardBody {
    /// Long-term identity of the card's owner.
    pub identity: PublicKey,
    /// Current onion service, v3 format (56-char base32).
    pub onion: String,
    /// Mailboxes this user is registered with. Empty in 1.D.
    pub mailboxes: Vec<String>,
    /// Monotonic version. Higher is newer.
    pub version: u64,
    /// Unix timestamp after which this card is considered stale.
    pub expires_at: i64,
}

/// A contact's published routing record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactCard {
    /// Unsigned fields.
    pub body: ContactCardBody,
    /// Ed25519 signature over canonical CBOR of `body`.
    pub signature: Signature,
}

impl ContactCard {
    /// Build and sign a new card.
    pub fn sign(
        _signer: &IdentityKey,
        _onion: String,
        _mailboxes: Vec<String>,
        _version: u64,
        _ttl_secs: u64,
        _now: i64,
    ) -> Result<Self> {
        todo!("Task 5")
    }

    /// Verify the Ed25519 signature + expiry. On success returns the
    /// body's `identity` (the caller typically cross-checks against a
    /// known contact).
    pub fn verify(&self, _now: i64) -> Result<PublicKey> {
        todo!("Task 6")
    }
}
```

- [ ] **Step 6: Extend `lib.rs::test_exports`**

Open `crates/core/src/lib.rs`. Find the `test_exports` module. Add a Phase 1.D section after the Phase 1.C additions:

```rust
    // Phase 1.D additions:
    pub use crate::contact::{Contact, ContactCard, ContactCardBody};
    pub use crate::invite::{InviteLink, InviteLinkBody, InvitePsk};
    pub use crate::storage::ContactRepo;
```

(If `ContactRepo` is already in test_exports from Phase 0.D, drop the duplicate line. Check first.)

- [ ] **Step 7: Verify the crate builds**

```bash
cargo build --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

All three green. The existing tests that constructed `ContactCard` with the old flat-field shape will now fail to compile — this is the point where we discover which files reference the old shape. Expect the `contact::card` existing tests (there may not be any, but if there are, they'll fail). Also expect any other code that pattern-matches on `ContactCard { identity, onion, ... }` to need updates in Tasks 5/6.

For Task 1, we only need the stubs to compile. If production/test code anywhere references the old `ContactCard` or `InviteLink` field shape directly, EITHER:
- Update that code inline (if it's trivial — one call site)
- Comment it out with `// TODO-1.D: update to body/wrapper split` (and the task touching that file will un-comment)

Report any you comment out in your status.

- [ ] **Step 8: Commit**

```bash
git add crates/core/src/invite/ crates/core/src/contact/card.rs crates/core/src/lib.rs
git commit -m "$(cat <<'EOF'
invite/contact: body/wrapper split stubs for Phase 1.D

InviteLink + ContactCard each split into a Body (unsigned content,
serde-derived) + a wrapper that adds the signature. Canonical CBOR
over the body is the signed form for both. Method bodies stay as
todo!() — Tasks 5-10 fill them in.

render_png stub deleted from invite/qr.rs per the Phase 1.D scope
decision (SVG only; PNG deferred to Phase 2).

Adds test_exports entries so integration tests can reach the new
types under the test-harness feature.
EOF
)"
```

---

## Task 2: Migration 0003 + `ContactRepo::put_card` + `latest_card`

**Goal:** Add the `contact_cards` table and implement `put_card` + `latest_card` with monotonic-version enforcement. Tests cover the happy path and stale-version rejection. `Contact.card` hydration in `get`/`list` lands in Task 4 — this task only ships the CRUD primitives.

**Files:**
- Create: `crates/core/src/storage/migrations/0003_contact_cards.sql`
- Modify: `crates/core/src/storage/migrations.rs`
- Modify: `crates/core/src/storage/contacts.rs`

- [ ] **Step 1: Write the migration**

Create `crates/core/src/storage/migrations/0003_contact_cards.sql`:

```sql
-- skattr schema migration 0003: contact cards
--
-- Stores verified ContactCards per contact, keyed by (contact_id, version).
-- Full history is preserved so rotation (Phase 2) can audit previous
-- cards and grace-period overlaps. `latest_card` queries for the
-- highest version; `put_card` rejects any version that isn't strictly
-- greater than the stored max.

CREATE TABLE IF NOT EXISTS contact_cards (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    contact_id INTEGER NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
    version INTEGER NOT NULL,
    card_blob BLOB NOT NULL,
    verified_at INTEGER NOT NULL,
    UNIQUE (contact_id, version)
);

CREATE INDEX IF NOT EXISTS idx_contact_cards_latest
  ON contact_cards(contact_id, version DESC);
```

Note: the Phase 1.C migrations runner uses `INSERT OR REPLACE INTO schema_version` after applying each migration, so we don't include an explicit `UPDATE schema_version` here (the runner owns it).

- [ ] **Step 2: Register the migration**

Open `crates/core/src/storage/migrations.rs`. Find the `ALL_MIGRATIONS` static array (added/extended in Phase 1.C Task 2). Append the 0003 entry:

```rust
const ALL_MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("migrations/0001_init.sql"),
    },
    Migration {
        version: 2,
        sql: include_str!("migrations/0002_key_packages.sql"),
    },
    Migration {
        version: 3,
        sql: include_str!("migrations/0003_contact_cards.sql"),
    },
];
```

Also find the test that asserts expected tables (likely `migration_creates_expected_tables` or similar, in the same file) and add `"contact_cards"` to the list. Keep alphabetical order if that's the style.

- [ ] **Step 3: Write the failing tests**

Open `crates/core/src/storage/contacts.rs`. Find the existing `#[cfg(test)] mod tests { ... }` block (at the end of the file). Append these tests INSIDE the existing `mod tests` block, after `onion_rotation_flow`:

```rust
    use crate::contact::card::{ContactCard, ContactCardBody};
    use crate::identity::Signature;

    fn sample_card(seed: u8, version: u64) -> ContactCard {
        ContactCard {
            body: ContactCardBody {
                identity: PublicKey([seed; 32]),
                onion: format!("onion-{seed}.onion"),
                mailboxes: Vec::new(),
                version,
                expires_at: 1_700_000_000 + i64::from(seed),
            },
            // The signature bytes are irrelevant for storage tests —
            // put_card / latest_card don't re-verify (that's the
            // caller's job via ContactCard::verify before put_card).
            signature: Signature([0u8; 64]),
        }
    }

    #[test]
    fn put_card_then_latest_card_round_trip() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let contact = sample_contact(10);
        repo.upsert(&contact).unwrap();

        let card = sample_card(10, 1);
        repo.put_card(&card).unwrap();

        let got = repo.latest_card(&contact.identity).unwrap().unwrap();
        assert_eq!(got.body.version, 1);
        assert_eq!(got.body.onion, "onion-10.onion");
    }

    #[test]
    fn put_card_rejects_stale_version() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let contact = sample_contact(11);
        repo.upsert(&contact).unwrap();

        repo.put_card(&sample_card(11, 5)).unwrap();
        let err = repo.put_card(&sample_card(11, 5)).expect_err("same version");
        assert!(matches!(err, CoreError::Contact(ref s) if s.contains("stale version")), "got: {err:?}");

        let err = repo.put_card(&sample_card(11, 4)).expect_err("older version");
        assert!(matches!(err, CoreError::Contact(ref s) if s.contains("stale version")), "got: {err:?}");
    }

    #[test]
    fn put_card_rejects_when_contact_absent() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        // No upsert — the contact row isn't there.
        let err = repo.put_card(&sample_card(12, 1)).expect_err("no contact");
        assert!(matches!(err, CoreError::Contact(ref s) if s.contains("contact not found")), "got: {err:?}");
    }

    #[test]
    fn latest_card_missing_contact_returns_none() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        assert!(repo.latest_card(&PublicKey([0x99; 32])).unwrap().is_none());
    }

    #[test]
    fn put_card_accepts_higher_version() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let contact = sample_contact(13);
        repo.upsert(&contact).unwrap();

        repo.put_card(&sample_card(13, 1)).unwrap();
        repo.put_card(&sample_card(13, 2)).unwrap();
        repo.put_card(&sample_card(13, 10)).unwrap();

        let latest = repo.latest_card(&contact.identity).unwrap().unwrap();
        assert_eq!(latest.body.version, 10);
    }

    #[test]
    fn cascade_delete_removes_cards() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let contact = sample_contact(14);
        repo.upsert(&contact).unwrap();
        repo.put_card(&sample_card(14, 1)).unwrap();
        repo.put_card(&sample_card(14, 2)).unwrap();

        repo.remove(&contact.identity).unwrap();

        let count: i64 = pool
            .with(|c| {
                c.query_row("SELECT COUNT(*) FROM contact_cards", [], |r| r.get(0))
                    .map_err(|e| CoreError::Storage(e.to_string()))
            })
            .unwrap();
        assert_eq!(count, 0);
    }
```

- [ ] **Step 4: Run the tests — compile failure expected**

```bash
cargo test -p skattr-core --lib storage::contacts::tests
```

Expected: compile error (`method 'put_card' not found`, etc.). Good.

- [ ] **Step 5: Implement `put_card` and `latest_card`**

Open `crates/core/src/storage/contacts.rs`. Inside `impl<'p> ContactRepo<'p>` (at the end, after `current_onion`), add:

```rust
    /// Insert a freshly-verified `ContactCard`. Rejects with
    /// `CoreError::Contact("contact: card: stale version")` if the
    /// version is not strictly greater than the latest stored card's
    /// version for the same identity. Rejects with
    /// `CoreError::Contact("contact: card: contact not found")` if
    /// the contact row doesn't exist.
    pub fn put_card(&self, card: &ContactCard) -> Result<()> {
        let identity_bytes = card.body.identity.0;
        let version_i: i64 = i64::try_from(card.body.version)
            .map_err(|_| CoreError::Contact("contact: card: version overflows i64".into()))?;

        let mut blob = Vec::new();
        ciborium::ser::into_writer(card, &mut blob)
            .map_err(|e| CoreError::Contact(format!("contact: card: cbor encode: {e}")))?;

        let verified_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        self.pool.transaction(|tx| {
            // Resolve the contact's row id. If missing, fail with the
            // fixed message before touching contact_cards.
            let contact_id: i64 = match tx.query_row(
                "SELECT id FROM contacts WHERE identity_pubkey = ?1",
                rusqlite::params![&identity_bytes[..]],
                |r| r.get(0),
            ) {
                Ok(id) => id,
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    return Err(CoreError::Contact("contact: card: contact not found".into()));
                }
                Err(e) => return Err(CoreError::Contact(format!("contact: card: lookup: {e}"))),
            };

            // Compare against the stored max version; reject if not strictly greater.
            let max_version: Option<i64> = tx
                .query_row(
                    "SELECT MAX(version) FROM contact_cards WHERE contact_id = ?1",
                    rusqlite::params![contact_id],
                    |r| r.get::<_, Option<i64>>(0),
                )
                .map_err(|e| CoreError::Contact(format!("contact: card: max-version lookup: {e}")))?;

            if let Some(max_v) = max_version {
                if version_i <= max_v {
                    return Err(CoreError::Contact("contact: card: stale version".into()));
                }
            }

            tx.execute(
                "INSERT INTO contact_cards (contact_id, version, card_blob, verified_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![contact_id, version_i, &blob, verified_at],
            )
            .map_err(|e| CoreError::Contact(format!("contact: card: insert: {e}")))?;
            Ok(())
        })
    }

    /// Return the highest-version `ContactCard` for `identity`, or
    /// `None` if no card exists (or the contact is unknown).
    pub fn latest_card(&self, identity: &PublicKey) -> Result<Option<ContactCard>> {
        self.pool.with(|c| {
            let result = c.query_row(
                "SELECT cc.card_blob FROM contact_cards cc \
                 JOIN contacts k ON cc.contact_id = k.id \
                 WHERE k.identity_pubkey = ?1 \
                 ORDER BY cc.version DESC LIMIT 1",
                rusqlite::params![&identity.0[..]],
                |r| r.get::<_, Vec<u8>>(0),
            );
            match result {
                Ok(blob) => {
                    let card: ContactCard = ciborium::de::from_reader(&blob[..])
                        .map_err(|e| CoreError::Contact(format!("contact: card: cbor decode: {e}")))?;
                    Ok(Some(card))
                }
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Contact(format!("contact: card: latest: {e}"))),
            }
        })
    }
```

Also add the `ContactCard` import near the top of `crates/core/src/storage/contacts.rs` (alongside the existing `use crate::contact::Contact;`):

```rust
use crate::contact::ContactCard;
```

- [ ] **Step 6: Run the tests — should pass**

```bash
cargo test -p skattr-core --lib storage::contacts::tests
```

Expected: all tests pass (pre-existing 6 tests + 6 new = 12 passing).

- [ ] **Step 7: Verify fmt + clippy**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Both clean. Run `cargo fmt --all` if fmt complains.

- [ ] **Step 8: Commit**

```bash
git add crates/core/src/storage/migrations/0003_contact_cards.sql \
        crates/core/src/storage/migrations.rs \
        crates/core/src/storage/contacts.rs
git commit -m "$(cat <<'EOF'
storage: migration 0003 contact_cards + put_card / latest_card

contact_cards table keyed by (contact_id, version) with cascade-delete
FK + desc-index for 'latest' queries. put_card runs the
version-compare-and-insert inside a transaction, rejecting any
version that isn't strictly greater than the stored max with
"contact: card: stale version". Missing-contact rejected with
"contact: card: contact not found" before touching the cards table.
latest_card joins to contacts to resolve identity → contact_id →
card_blob → ciborium-decoded ContactCard.

Six tests cover round-trip, stale-version rejection at equal and
below, missing-contact rejection, missing-card returns None,
ascending-version acceptance, and cascade-delete cleanup.

Migrations runner extended with the 0003 include_str! entry; the
expected-tables assertion gains 'contact_cards'.
EOF
)"
```

---

## Task 3: Canonical sign/verify helper on `IdentityKey`

**Goal:** Factor the "CBOR-encode body + Ed25519 sign" pattern into a single helper so `ContactCard::sign`, `InviteLink::generate`, and the reverse paths all call the same function. Also exercise a sign-verify round-trip proptest against a generic body type.

**Files:**
- Modify: `crates/core/src/identity/key.rs`

The helper lives on `IdentityKey` because it holds the signing secret. Verification doesn't need an `IdentityKey` — it uses the free function `IdentityKey::verify(pubkey, message, signature)` that already exists.

- [ ] **Step 1: Add the `sign_cbor` helper + test**

Open `crates/core/src/identity/key.rs`. Inside `impl IdentityKey`, after `sign` (around line 100), add:

```rust
    /// Sign the canonical CBOR encoding of `body`. Used by ContactCard
    /// and InviteLink to sign their Body structs. Pairs with
    /// [`Self::verify_cbor`].
    pub fn sign_cbor<T: serde::Serialize>(&self, body: &T) -> Result<Signature> {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(body, &mut bytes)
            .map_err(|e| CoreError::Identity(format!("sign_cbor: {e}")))?;
        Ok(self.sign(&bytes))
    }

    /// Verify a signature over the canonical CBOR encoding of `body`.
    /// Signer identity is `pubkey`. Collapses all failure modes to
    /// the same opaque string (same as [`Self::verify`]).
    pub fn verify_cbor<T: serde::Serialize>(
        pubkey: &PublicKey,
        body: &T,
        signature: &Signature,
    ) -> Result<()> {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(body, &mut bytes)
            .map_err(|_| CoreError::Identity("verification failed".into()))?;
        Self::verify(pubkey, &bytes, signature)
    }
```

Inside the existing `#[cfg(test)] mod tests` block, append:

```rust
    #[test]
    fn sign_cbor_verify_cbor_round_trip() {
        use serde::{Deserialize, Serialize};
        #[derive(Serialize, Deserialize)]
        struct Sample {
            a: u64,
            b: String,
            c: Vec<u8>,
        }

        let id = IdentityKey::generate().unwrap();
        let body = Sample {
            a: 42,
            b: "hello".into(),
            c: vec![1, 2, 3, 4],
        };

        let sig = id.sign_cbor(&body).unwrap();
        IdentityKey::verify_cbor(&id.public(), &body, &sig).expect("valid");

        // Tampered body should fail.
        let tampered = Sample {
            a: 43,
            b: body.b.clone(),
            c: body.c.clone(),
        };
        IdentityKey::verify_cbor(&id.public(), &tampered, &sig).expect_err("tampered body");

        // Wrong signer should fail.
        let other = IdentityKey::generate().unwrap();
        IdentityKey::verify_cbor(&other.public(), &body, &sig).expect_err("wrong signer");
    }
```

- [ ] **Step 2: Run the test**

```bash
cargo test -p skattr-core --lib identity::key::tests::sign_cbor_verify_cbor_round_trip
```

Expected: PASS.

- [ ] **Step 3: Verify fmt + clippy**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Both clean.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/identity/key.rs
git commit -m "$(cat <<'EOF'
identity: sign_cbor / verify_cbor helpers for canonical-body signing

sign_cbor(body) ciborium-encodes the serialize-bound body and signs
the bytes with self's Ed25519 key. verify_cbor(pubkey, body, sig)
is the inverse — re-encodes, verifies. Both ContactCard and
InviteLink will call these instead of inlining the pattern.

Round-trip test covers the happy path + tampered-body rejection +
wrong-signer rejection.
EOF
)"
```

---

## Task 4: `ContactCard::sign`

**Goal:** Build + sign a `ContactCard`. `expires_at = now + ttl_secs`. Signature covers canonical CBOR of the body.

**Files:**
- Modify: `crates/core/src/contact/card.rs`

- [ ] **Step 1: Write the failing test**

Open `crates/core/src/contact/card.rs`. Append at the bottom:

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn sign_fills_body_fields_and_signature_is_64_bytes() {
        let signer = IdentityKey::generate().unwrap();
        let card = ContactCard::sign(
            &signer,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.onion".into(),
            Vec::new(),
            7,
            3600,
            1_000_000,
        )
        .unwrap();

        assert_eq!(card.body.identity, signer.public());
        assert_eq!(card.body.version, 7);
        assert_eq!(card.body.expires_at, 1_000_000 + 3600);
        assert!(card.body.mailboxes.is_empty());
        assert_eq!(card.body.onion.len(), 62); // 56-char onion + ".onion"
        assert_eq!(card.signature.0.len(), 64);
    }
}
```

- [ ] **Step 2: Run the test — expect `todo!()` panic**

```bash
cargo test -p skattr-core --lib contact::card::tests::sign_fills_body_fields_and_signature_is_64_bytes
```

Expected: panic in `todo!("Task 5")`.

- [ ] **Step 3: Implement `sign`**

Replace the `todo!("Task 5")` body of `ContactCard::sign` in `crates/core/src/contact/card.rs` with:

```rust
    pub fn sign(
        signer: &IdentityKey,
        onion: String,
        mailboxes: Vec<String>,
        version: u64,
        ttl_secs: u64,
        now: i64,
    ) -> Result<Self> {
        let expires_at = now
            .checked_add(i64::try_from(ttl_secs).map_err(|_| {
                crate::error::CoreError::Contact("contact: card: ttl overflows i64".into())
            })?)
            .ok_or_else(|| {
                crate::error::CoreError::Contact("contact: card: expires_at overflows i64".into())
            })?;

        let body = ContactCardBody {
            identity: signer.public(),
            onion,
            mailboxes,
            version,
            expires_at,
        };
        let signature = signer.sign_cbor(&body)?;
        Ok(Self { body, signature })
    }
```

Ensure the imports at the top of `crates/core/src/contact/card.rs` include `IdentityKey` and `Signature` (they're already there) plus `PublicKey` (also already there).

- [ ] **Step 4: Run the test**

```bash
cargo test -p skattr-core --lib contact::card::tests::sign_fills_body_fields_and_signature_is_64_bytes
```

Expected: PASS.

- [ ] **Step 5: Verify fmt + clippy**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/contact/card.rs
git commit -m "$(cat <<'EOF'
contact: ContactCard::sign builds body + Ed25519 signs via sign_cbor

sign composes ContactCardBody, checks for i64 overflow in
expires_at = now + ttl_secs, and signs canonical CBOR of the body.
Test verifies the six body fields and the 64-byte signature length.
EOF
)"
```

---

## Task 5: `ContactCard::verify(now)`

**Goal:** Verify the signature and expiry. Return the body's `identity` on success.

**Files:**
- Modify: `crates/core/src/contact/card.rs`

- [ ] **Step 1: Write the failing tests**

Open `crates/core/src/contact/card.rs`. Inside the existing `#[cfg(test)] mod tests` block (after the `sign_fills_body_fields_and_signature_is_64_bytes` test), append:

```rust
    fn fresh_card(version: u64, expires_at: i64, signer: &IdentityKey) -> ContactCard {
        ContactCard::sign(
            signer,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.onion".into(),
            Vec::new(),
            version,
            (expires_at - 1_000_000).max(1) as u64,
            1_000_000,
        )
        .unwrap()
    }

    #[test]
    fn verify_returns_identity_on_valid_card() {
        let signer = IdentityKey::generate().unwrap();
        let card = fresh_card(1, 1_003_600, &signer);
        let got = card.verify(1_000_000).unwrap();
        assert_eq!(got, signer.public());
    }

    #[test]
    fn verify_rejects_expired() {
        let signer = IdentityKey::generate().unwrap();
        let card = fresh_card(1, 1_003_600, &signer);
        let err = card.verify(1_003_601).expect_err("must reject expired");
        match err {
            crate::error::CoreError::Contact(s) => assert!(s.contains("expired"), "got: {s}"),
            other => panic!("expected Contact, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_tampered_signature() {
        let signer = IdentityKey::generate().unwrap();
        let mut card = fresh_card(1, 1_003_600, &signer);
        card.signature.0[0] ^= 0xFF;
        let err = card.verify(1_000_000).expect_err("tampered");
        match err {
            crate::error::CoreError::Contact(s) => {
                assert!(s.contains("signature verification failed"), "got: {s}");
            }
            crate::error::CoreError::Identity(s) => {
                // verify_cbor converts Identity("verification failed") into Contact
                // — but if the impl doesn't translate, accept Identity too and
                // flag it for cleanup.
                assert!(s.contains("verification failed"), "got: {s}");
            }
            other => panic!("expected Contact/Identity, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_wrong_signer() {
        let signer = IdentityKey::generate().unwrap();
        let other = IdentityKey::generate().unwrap();
        let mut card = fresh_card(1, 1_003_600, &signer);
        // Replace the body's identity with the other signer's pubkey —
        // signature was made with `signer`, so verification under
        // `other.public()` must fail.
        card.body.identity = other.public();
        let err = card.verify(1_000_000).expect_err("wrong signer");
        match err {
            crate::error::CoreError::Contact(s) => {
                assert!(s.contains("signature verification failed"), "got: {s}");
            }
            crate::error::CoreError::Identity(s) => {
                assert!(s.contains("verification failed"), "got: {s}");
            }
            other => panic!("expected Contact/Identity, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run tests — expect `todo!()` panic**

```bash
cargo test -p skattr-core --lib contact::card::tests
```

- [ ] **Step 3: Implement `verify`**

Replace the `todo!("Task 6")` body of `ContactCard::verify` in `crates/core/src/contact/card.rs` with:

```rust
    pub fn verify(&self, now: i64) -> Result<PublicKey> {
        if now > self.body.expires_at {
            return Err(crate::error::CoreError::Contact(
                "contact: card: expired".into(),
            ));
        }
        IdentityKey::verify_cbor(&self.body.identity, &self.body, &self.signature).map_err(
            |_| crate::error::CoreError::Contact("contact: card: signature verification failed".into()),
        )?;
        Ok(self.body.identity)
    }
```

- [ ] **Step 4: Run the tests**

```bash
cargo test -p skattr-core --lib contact::card::tests
```

Expected: 5 PASS (1 from Task 4 + 4 new). `verify_rejects_tampered_signature` and `verify_rejects_wrong_signer` both expect `Contact("signature verification failed")` — our impl translates via `.map_err`.

- [ ] **Step 5: Verify fmt + clippy**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/contact/card.rs
git commit -m "$(cat <<'EOF'
contact: ContactCard::verify(now) — signature + expiry check

verify checks now <= body.expires_at, then runs
IdentityKey::verify_cbor(body.identity, body, signature); any
crypto failure is translated into CoreError::Contact("contact: card:
signature verification failed") to keep the error string uniform
regardless of which rejection OpenSSL / dalek raised.

Four tests cover: valid round-trip returns the expected pubkey,
expired rejected, tampered signature rejected, wrong signer (body
identity swapped to a different pubkey) rejected.
EOF
)"
```

---

## Task 6: Hydrate `Contact.card` in `ContactRepo::get` + `list`

**Goal:** Now that `latest_card` exists (Task 2), update `get` and `list` to populate `Contact.card` with the contact's latest card.

**Files:**
- Modify: `crates/core/src/storage/contacts.rs`

- [ ] **Step 1: Write the failing test**

Inside the existing `#[cfg(test)] mod tests { ... }` block in `crates/core/src/storage/contacts.rs`, append:

```rust
    #[test]
    fn get_hydrates_latest_card() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let contact = sample_contact(20);
        repo.upsert(&contact).unwrap();

        repo.put_card(&sample_card(20, 1)).unwrap();
        repo.put_card(&sample_card(20, 2)).unwrap();

        let got = repo.get(&contact.identity).unwrap().unwrap();
        let card = got.card.expect("contact.card must hydrate");
        assert_eq!(card.body.version, 2);
    }

    #[test]
    fn list_hydrates_latest_card() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let c1 = sample_contact(21);
        let c2 = sample_contact(22);
        repo.upsert(&c1).unwrap();
        repo.upsert(&c2).unwrap();

        repo.put_card(&sample_card(21, 5)).unwrap();
        // c2 has no card — get should still succeed with card=None.

        let all = repo.list().unwrap();
        let got_c1 = all.iter().find(|c| c.identity == c1.identity).unwrap();
        let got_c2 = all.iter().find(|c| c.identity == c2.identity).unwrap();
        assert_eq!(got_c1.card.as_ref().unwrap().body.version, 5);
        assert!(got_c2.card.is_none());
    }
```

- [ ] **Step 2: Run the tests**

```bash
cargo test -p skattr-core --lib storage::contacts::tests::get_hydrates_latest_card \
                                 storage::contacts::tests::list_hydrates_latest_card
```

Expected: fail. Currently `get` / `list` always set `card: None`.

- [ ] **Step 3: Update `get` and `list`**

Open `crates/core/src/storage/contacts.rs`. Find the existing `get` method. Change its return-row closure to:

```rust
    pub fn get(&self, identity: &PublicKey) -> Result<Option<Contact>> {
        self.pool.with(|c| {
            let result = c.query_row(
                "SELECT display_name, added_at FROM contacts WHERE identity_pubkey = ?1",
                rusqlite::params![&identity.0[..]],
                |r| {
                    Ok(Contact {
                        identity: *identity,
                        display_name: r.get(0)?,
                        added_at: r.get(1)?,
                        card: None, // hydrate outside the row closure below
                    })
                },
            );
            let mut contact = match result {
                Ok(contact) => contact,
                Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
                Err(e) => return Err(CoreError::Storage(format!("get contact: {e}"))),
            };
            drop(c);
            // Hydrate the latest card. We can't borrow `self.pool.with`
            // twice from inside, so close the first borrow first.
            contact.card = self.latest_card(identity)?;
            Ok(Some(contact))
        })
    }
```

Wait — `self.pool.with` passes a `&Connection` closure, and `self.latest_card` wants to re-borrow `self.pool`. Doing that inside the closure holds the lock. A cleaner approach: do the card lookup *after* the first closure returns.

Replace the whole method with:

```rust
    pub fn get(&self, identity: &PublicKey) -> Result<Option<Contact>> {
        let base = self.pool.with(|c| {
            let result = c.query_row(
                "SELECT display_name, added_at FROM contacts WHERE identity_pubkey = ?1",
                rusqlite::params![&identity.0[..]],
                |r| {
                    Ok(Contact {
                        identity: *identity,
                        display_name: r.get(0)?,
                        added_at: r.get(1)?,
                        card: None,
                    })
                },
            );
            match result {
                Ok(contact) => Ok(Some(contact)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Storage(format!("get contact: {e}"))),
            }
        })?;
        let Some(mut contact) = base else {
            return Ok(None);
        };
        contact.card = self.latest_card(identity)?;
        Ok(Some(contact))
    }
```

Also update `list`:

```rust
    pub(crate) fn list(&self) -> Result<Vec<Contact>> {
        let mut contacts: Vec<Contact> = self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT identity_pubkey, display_name, added_at FROM contacts \
                     ORDER BY display_name IS NULL, display_name COLLATE NOCASE",
                )
                .map_err(|e| CoreError::Storage(format!("prepare list contacts: {e}")))?;
            let rows = stmt
                .query_map([], |r| {
                    let pub_bytes: Vec<u8> = r.get(0)?;
                    let mut arr = [0u8; 32];
                    if pub_bytes.len() == 32 {
                        arr.copy_from_slice(&pub_bytes);
                    }
                    Ok(Contact {
                        identity: PublicKey(arr),
                        display_name: r.get(1)?,
                        added_at: r.get(2)?,
                        card: None,
                    })
                })
                .map_err(|e| CoreError::Storage(format!("query list contacts: {e}")))?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| CoreError::Storage(format!("collect contacts: {e}")))
        })?;

        // Hydrate each contact's latest_card. For small contact lists
        // (our expected scale: dozens), an N+1 query is fine. Phase 2
        // can batch if contacts grow.
        for contact in &mut contacts {
            contact.card = self.latest_card(&contact.identity)?;
        }
        Ok(contacts)
    }
```

- [ ] **Step 4: Run the tests**

```bash
cargo test -p skattr-core --lib storage::contacts::tests
```

Expected: all tests pass, including the two new hydration tests.

- [ ] **Step 5: Verify fmt + clippy**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/storage/contacts.rs
git commit -m "$(cat <<'EOF'
storage: hydrate Contact.card in ContactRepo::get and list

get now runs the base-row lookup, then (outside the connection-
closure scope) calls latest_card to populate Contact.card. Missing
card leaves card = None. list hydrates in a simple N+1 loop —
acceptable for the dozens-of-contacts scale Skattr targets; Phase 2
can batch via JOIN if needed.

Two new tests: get_hydrates_latest_card confirms version 2 (out of
inserted v1+v2) arrives; list_hydrates_latest_card confirms mixed
card-present and card-absent contacts both come back correctly.
EOF
)"
```

---

## Task 7: `InviteLink::generate`

**Goal:** Build + sign an `InviteLink`. Signature covers canonical CBOR of the body. The PSK lives in two places: the body (for serialization/signing) and the `InvitePsk` guard (for zeroizing on drop). Caller's ownership of the PSK bytes is transferred into both.

**Files:**
- Modify: `crates/core/src/invite/link.rs`

- [ ] **Step 1: Write the failing test**

Open `crates/core/src/invite/link.rs`. Append at the bottom:

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn fixed_kp() -> Vec<u8> {
        (0..64u8).collect()
    }

    #[test]
    fn generate_populates_body_and_signature() {
        let inviter = IdentityKey::generate().unwrap();
        let psk = [0xAA; 32];
        let invite = InviteLink::generate(
            &inviter,
            "abc.onion".into(),
            fixed_kp(),
            psk,
            3600,
            1_000_000,
        )
        .unwrap();

        assert_eq!(invite.body.identity, inviter.public());
        assert_eq!(invite.body.onion, "abc.onion");
        assert_eq!(invite.body.key_package, fixed_kp());
        assert_eq!(invite.body.psk, psk);
        assert_eq!(invite.body.expires_at, 1_000_000 + 3600);
        assert_eq!(invite.signature.0.len(), 64);
        assert_eq!(invite.psk.0, psk);
    }

    #[test]
    fn generate_signature_verifies_via_identity() {
        let inviter = IdentityKey::generate().unwrap();
        let invite = InviteLink::generate(
            &inviter,
            "xyz.onion".into(),
            fixed_kp(),
            [0xBB; 32],
            3600,
            1_000_000,
        )
        .unwrap();
        IdentityKey::verify_cbor(&invite.body.identity, &invite.body, &invite.signature)
            .expect("body signature must verify against its embedded identity");
    }
}
```

- [ ] **Step 2: Run — expect `todo!()` panic**

```bash
cargo test -p skattr-core --lib invite::link::tests::generate_populates_body_and_signature
```

- [ ] **Step 3: Implement `generate`**

Replace the `todo!("Task 7")` body of `InviteLink::generate` in `crates/core/src/invite/link.rs` with:

```rust
    pub fn generate(
        inviter: &IdentityKey,
        onion: String,
        key_package: Vec<u8>,
        psk: [u8; 32],
        ttl_secs: u64,
        now: i64,
    ) -> Result<Self> {
        let expires_at = now
            .checked_add(i64::try_from(ttl_secs).map_err(|_| {
                crate::error::CoreError::Invite("invite: ttl overflows i64".into())
            })?)
            .ok_or_else(|| {
                crate::error::CoreError::Invite("invite: expires_at overflows i64".into())
            })?;

        let body = InviteLinkBody {
            identity: inviter.public(),
            onion,
            key_package,
            psk,
            expires_at,
        };
        let signature = inviter
            .sign_cbor(&body)
            .map_err(|e| crate::error::CoreError::Invite(format!("invite: sign: {e}")))?;
        Ok(Self {
            body,
            signature,
            psk: InvitePsk(psk),
        })
    }
```

- [ ] **Step 4: Run the tests**

```bash
cargo test -p skattr-core --lib invite::link::tests
```

Expected: 2 PASS.

- [ ] **Step 5: Verify fmt + clippy**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/invite/link.rs
git commit -m "$(cat <<'EOF'
invite: InviteLink::generate builds body + signs via sign_cbor

generate composes InviteLinkBody, computes expires_at = now + ttl_secs
with overflow guards, signs canonical CBOR of the body, and wraps
the PSK in a zeroizing InvitePsk copy alongside the body.psk field
(the body copy is used at serialization time and zeroed in from_url
later; generate keeps both copies identical).

Two tests cover field population + signature-over-embedded-identity
verification.
EOF
)"
```

---

## Task 8: `InviteLink::to_url`

**Goal:** Serialize an `InviteLink` to `skattr://invite/v1#...`. Base32 for `id`, base64url-no-pad for `kp`/`psk`/`sig`, decimal for `exp`, raw onion. Fixed field order.

**Files:**
- Modify: `crates/core/src/invite/link.rs`

- [ ] **Step 1: Write the failing test**

Append inside `mod tests` in `crates/core/src/invite/link.rs`:

```rust
    #[test]
    fn to_url_has_expected_prefix_and_all_six_params() {
        let inviter = IdentityKey::generate().unwrap();
        let invite = InviteLink::generate(
            &inviter,
            "abc.onion".into(),
            fixed_kp(),
            [0xAA; 32],
            3600,
            1_000_000,
        )
        .unwrap();

        let url = invite.to_url().unwrap();
        assert!(url.starts_with("skattr://invite/v1#"));
        // All six fields present, in fixed order.
        let fragment = url.strip_prefix("skattr://invite/v1#").unwrap();
        let keys: Vec<&str> = fragment.split('&').map(|p| p.split('=').next().unwrap()).collect();
        assert_eq!(keys, &["id", "onion", "kp", "psk", "exp", "sig"]);
    }

    #[test]
    fn to_url_is_deterministic() {
        let inviter = IdentityKey::generate().unwrap();
        let invite = InviteLink::generate(
            &inviter,
            "a.onion".into(),
            fixed_kp(),
            [0xAA; 32],
            3600,
            1_000_000,
        )
        .unwrap();
        assert_eq!(invite.to_url().unwrap(), invite.to_url().unwrap());
    }
```

- [ ] **Step 2: Run — expect `todo!()` panic**

```bash
cargo test -p skattr-core --lib invite::link::tests::to_url_has_expected_prefix_and_all_six_params
```

- [ ] **Step 3: Implement `to_url`**

Add two module-level helpers near the top of `crates/core/src/invite/link.rs` (after the `use` lines, before `InviteLinkBody`):

```rust
use base32::Alphabet;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;

const URL_PREFIX: &str = "skattr://invite/v1#";

fn encode_b32(bytes: &[u8]) -> String {
    base32::encode(Alphabet::Rfc4648Lower { padding: false }, bytes)
}

fn decode_b32(s: &str) -> Option<Vec<u8>> {
    base32::decode(Alphabet::Rfc4648Lower { padding: false }, s)
}

fn encode_b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn decode_b64url(s: &str) -> Option<Vec<u8>> {
    // Tolerate accidental padding: strip trailing '=' before decoding.
    let trimmed = s.trim_end_matches('=');
    URL_SAFE_NO_PAD.decode(trimmed.as_bytes()).ok()
}
```

Then replace the `todo!("Task 8")` body of `InviteLink::to_url`:

```rust
    pub fn to_url(&self) -> Result<String> {
        let id = encode_b32(&self.body.identity.0);
        let kp = encode_b64url(&self.body.key_package);
        let psk = encode_b64url(&self.body.psk);
        let sig = encode_b64url(&self.signature.0);
        Ok(format!(
            "{prefix}id={id}&onion={onion}&kp={kp}&psk={psk}&exp={exp}&sig={sig}",
            prefix = URL_PREFIX,
            id = id,
            onion = self.body.onion,
            kp = kp,
            psk = psk,
            exp = self.body.expires_at,
            sig = sig,
        ))
    }
```

- [ ] **Step 4: Run the tests**

```bash
cargo test -p skattr-core --lib invite::link::tests
```

Expected: 4 PASS (2 from Task 7 + 2 new).

- [ ] **Step 5: Verify fmt + clippy**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/invite/link.rs
git commit -m "$(cat <<'EOF'
invite: InviteLink::to_url emits skattr://invite/v1#... with fixed
field order

id=base32-lower-nopad, kp/psk/sig=base64url-nopad, onion as-is,
exp as decimal i64. Field order is fixed (id/onion/kp/psk/exp/sig)
so to_url is deterministic and to_url → from_url → to_url is
idempotent. Two tests cover the URL shape and determinism.

Adds module-level encode_b32 / decode_b32 / encode_b64url /
decode_b64url helpers that Task 9's from_url will reuse. decode_b64url
tolerates trailing '=' padding so users can paste URLs that picked
up URL-normalization tails.
EOF
)"
```

---

## Task 9: `InviteLink::from_url(url, now)`

**Goal:** Parse + verify a URL. Checks: scheme prefix, all six required fields, each field decodes cleanly, signature verifies against body, `now <= expires_at`. On success, zeroize `body.psk` after copying into `InvitePsk`.

**Files:**
- Modify: `crates/core/src/invite/link.rs`

- [ ] **Step 1: Write the failing tests**

Append inside `mod tests` in `crates/core/src/invite/link.rs`:

```rust
    #[test]
    fn from_url_round_trip_valid() {
        let inviter = IdentityKey::generate().unwrap();
        let invite = InviteLink::generate(
            &inviter,
            "xyz.onion".into(),
            fixed_kp(),
            [0xCC; 32],
            3600,
            1_000_000,
        )
        .unwrap();
        let url = invite.to_url().unwrap();

        let parsed = InviteLink::from_url(&url, 1_000_500).unwrap();
        assert_eq!(parsed.body.identity, invite.body.identity);
        assert_eq!(parsed.body.onion, invite.body.onion);
        assert_eq!(parsed.body.key_package, invite.body.key_package);
        assert_eq!(parsed.body.expires_at, invite.body.expires_at);
        // PSK moved into the Zeroizing guard; body.psk cleared.
        assert_eq!(parsed.psk.0, [0xCC; 32]);
        assert_eq!(parsed.body.psk, [0u8; 32]);
    }

    #[test]
    fn from_url_rejects_unsupported_scheme() {
        let err = InviteLink::from_url("https://example.com/?id=x", 0).expect_err("bad scheme");
        match err {
            crate::error::CoreError::Invite(s) => {
                assert!(s.contains("unsupported scheme"), "got: {s}");
            }
            other => panic!("expected Invite, got {other:?}"),
        }
    }

    #[test]
    fn from_url_rejects_missing_field() {
        let inviter = IdentityKey::generate().unwrap();
        let url = InviteLink::generate(
            &inviter,
            "a.onion".into(),
            fixed_kp(),
            [0xDD; 32],
            3600,
            1_000_000,
        )
        .unwrap()
        .to_url()
        .unwrap();

        // Drop the `&kp=...` segment entirely.
        let fragment = url.strip_prefix("skattr://invite/v1#").unwrap();
        let trimmed: String = fragment
            .split('&')
            .filter(|p| !p.starts_with("kp="))
            .collect::<Vec<_>>()
            .join("&");
        let bad = format!("skattr://invite/v1#{trimmed}");

        let err = InviteLink::from_url(&bad, 1_000_000).expect_err("missing kp");
        match err {
            crate::error::CoreError::Invite(s) => {
                assert!(s.contains("missing field kp"), "got: {s}");
            }
            other => panic!("expected Invite, got {other:?}"),
        }
    }

    #[test]
    fn from_url_rejects_tampered_signature() {
        let inviter = IdentityKey::generate().unwrap();
        let url = InviteLink::generate(
            &inviter,
            "a.onion".into(),
            fixed_kp(),
            [0xEE; 32],
            3600,
            1_000_000,
        )
        .unwrap()
        .to_url()
        .unwrap();
        // Flip one character in the sig segment — any corruption makes
        // the signature invalid. The sig= is the last field, so we can
        // flip a char in the middle of the trailer.
        let mut bytes = url.into_bytes();
        let sig_start = twittle_sig_offset(&bytes);
        bytes[sig_start] ^= 0x01;
        // Ensure we didn't just produce an out-of-alphabet char.
        if !bytes[sig_start].is_ascii_alphanumeric()
            && bytes[sig_start] != b'-'
            && bytes[sig_start] != b'_'
        {
            bytes[sig_start] = b'A';
        }
        let tampered = String::from_utf8(bytes).unwrap();

        let err = InviteLink::from_url(&tampered, 1_000_000).expect_err("tampered");
        match err {
            crate::error::CoreError::Invite(s) => {
                assert!(
                    s.contains("signature verification failed") || s.contains("malformed"),
                    "got: {s}"
                );
            }
            other => panic!("expected Invite, got {other:?}"),
        }
    }

    /// Find the byte offset of the first character inside `sig=` value.
    fn twittle_sig_offset(bytes: &[u8]) -> usize {
        let needle = b"&sig=";
        let haystack = bytes;
        for i in 0..haystack.len().saturating_sub(needle.len()) {
            if &haystack[i..i + needle.len()] == needle {
                return i + needle.len();
            }
        }
        panic!("no &sig= in URL")
    }

    #[test]
    fn from_url_rejects_expired() {
        let inviter = IdentityKey::generate().unwrap();
        let url = InviteLink::generate(
            &inviter,
            "a.onion".into(),
            fixed_kp(),
            [0xFF; 32],
            3600,
            1_000_000,
        )
        .unwrap()
        .to_url()
        .unwrap();

        let err = InviteLink::from_url(&url, 1_003_601).expect_err("expired");
        match err {
            crate::error::CoreError::Invite(s) => assert!(s.contains("expired"), "got: {s}"),
            other => panic!("expected Invite, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run — expect failures**

```bash
cargo test -p skattr-core --lib invite::link::tests
```

Expected: new tests fail (existing `todo!` on `from_url`).

- [ ] **Step 3: Implement `from_url`**

Replace the `todo!("Task 9")` body of `InviteLink::from_url` in `crates/core/src/invite/link.rs` with:

```rust
    pub fn from_url(url: &str, now: i64) -> Result<Self> {
        use zeroize::Zeroize as _;

        let fragment = url
            .strip_prefix(URL_PREFIX)
            .ok_or_else(|| crate::error::CoreError::Invite("invite: unsupported scheme".into()))?;

        // Parse key=value pairs. Duplicates are resolved as last-wins;
        // we only care about the six canonical fields.
        let mut id_str: Option<&str> = None;
        let mut onion: Option<&str> = None;
        let mut kp_str: Option<&str> = None;
        let mut psk_str: Option<&str> = None;
        let mut exp_str: Option<&str> = None;
        let mut sig_str: Option<&str> = None;
        for pair in fragment.split('&') {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next().unwrap_or("");
            let value = parts.next().unwrap_or("");
            match key {
                "id" => id_str = Some(value),
                "onion" => onion = Some(value),
                "kp" => kp_str = Some(value),
                "psk" => psk_str = Some(value),
                "exp" => exp_str = Some(value),
                "sig" => sig_str = Some(value),
                _ => {} // ignore unknown fields for forward-compat
            }
        }

        let id_str = id_str
            .ok_or_else(|| crate::error::CoreError::Invite("invite: missing field id".into()))?;
        let onion_str = onion
            .ok_or_else(|| crate::error::CoreError::Invite("invite: missing field onion".into()))?;
        let kp_str = kp_str
            .ok_or_else(|| crate::error::CoreError::Invite("invite: missing field kp".into()))?;
        let psk_str = psk_str
            .ok_or_else(|| crate::error::CoreError::Invite("invite: missing field psk".into()))?;
        let exp_str = exp_str
            .ok_or_else(|| crate::error::CoreError::Invite("invite: missing field exp".into()))?;
        let sig_str = sig_str
            .ok_or_else(|| crate::error::CoreError::Invite("invite: missing field sig".into()))?;

        let id_bytes = decode_b32(&id_str.to_ascii_lowercase())
            .ok_or_else(|| crate::error::CoreError::Invite("invite: malformed id".into()))?;
        if id_bytes.len() != 32 {
            return Err(crate::error::CoreError::Invite("invite: malformed id".into()));
        }
        let mut identity_bytes = [0u8; 32];
        identity_bytes.copy_from_slice(&id_bytes);
        let identity = PublicKey(identity_bytes);

        let key_package = decode_b64url(kp_str)
            .ok_or_else(|| crate::error::CoreError::Invite("invite: malformed kp".into()))?;

        let psk_bytes = decode_b64url(psk_str)
            .ok_or_else(|| crate::error::CoreError::Invite("invite: malformed psk".into()))?;
        if psk_bytes.len() != 32 {
            return Err(crate::error::CoreError::Invite("invite: malformed psk".into()));
        }
        let mut psk = [0u8; 32];
        psk.copy_from_slice(&psk_bytes);

        let expires_at: i64 = exp_str
            .parse()
            .map_err(|_| crate::error::CoreError::Invite("invite: malformed exp".into()))?;

        let sig_bytes = decode_b64url(sig_str)
            .ok_or_else(|| crate::error::CoreError::Invite("invite: malformed sig".into()))?;
        if sig_bytes.len() != 64 {
            return Err(crate::error::CoreError::Invite("invite: malformed sig".into()));
        }
        let mut sig_arr = [0u8; 64];
        sig_arr.copy_from_slice(&sig_bytes);
        let signature = Signature(sig_arr);

        let body = InviteLinkBody {
            identity,
            onion: onion_str.to_string(),
            key_package,
            psk,
            expires_at,
        };

        // Verify signature.
        IdentityKey::verify_cbor(&body.identity, &body, &signature).map_err(|_| {
            crate::error::CoreError::Invite("invite: signature verification failed".into())
        })?;

        // Expiry check.
        if now > body.expires_at {
            return Err(crate::error::CoreError::Invite("invite: expired".into()));
        }

        // Move PSK into guard, zero body copy.
        let guard = InvitePsk(body.psk);
        let mut body = body;
        body.psk.zeroize();
        Ok(Self {
            body,
            signature,
            psk: guard,
        })
    }
```

- [ ] **Step 4: Run the tests**

```bash
cargo test -p skattr-core --lib invite::link::tests
```

Expected: 9 PASS (4 from Tasks 7-8 + 5 new).

- [ ] **Step 5: Verify fmt + clippy**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Common clippy complaints:
- `parts.next().unwrap_or("")` — `unwrap_or_default()` might be preferred; switch if clippy asks.
- Unused import of `zeroize::Zeroize` — if already imported via `use zeroize::{Zeroize, ZeroizeOnDrop};` at the top of the file (from the InvitePsk derive), the `use zeroize::Zeroize as _;` in the function body may duplicate. Remove whichever clippy complains about.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/invite/link.rs
git commit -m "$(cat <<'EOF'
invite: InviteLink::from_url(url, now) — parse + verify + expiry

Strips the skattr://invite/v1# prefix, pulls the six key=value
fragment params (unknown keys ignored for forward-compat), decodes
each via base32-lower or base64url-nopad (tolerating trailing '='
padding), reconstructs InviteLinkBody, runs IdentityKey::verify_cbor
against body.identity, checks now <= body.expires_at, moves the PSK
into an InvitePsk guard and zeroizes body.psk.

Error taxonomy:
- wrong scheme → "invite: unsupported scheme"
- missing field → "invite: missing field {name}"
- decode failure → "invite: malformed {field}"
- sig verify failure → "invite: signature verification failed"
- now > expires_at → "invite: expired"

Five tests: round-trip valid, unsupported scheme, missing field,
tampered signature, expired.
EOF
)"
```

---

## Task 10: Single-use helpers — `kp_hash` / `record_received` / `is_consumed` / `mark_consumed`

**Goal:** Wire the invite into the 1.C `KeyPackageRepo`. `kp_hash` is SHA-256 of the KP bytes. `record_received` inserts with `direction='theirs'`, idempotent. `is_consumed` reads the flag. `mark_consumed` flips the flag (idempotent), errors if the hash isn't recorded.

**Files:**
- Modify: `crates/core/src/invite/link.rs`

- [ ] **Step 1: Write the failing tests**

Append inside `mod tests` in `crates/core/src/invite/link.rs`:

```rust
    use crate::storage::{KeyPackageRepo, Pool};

    fn make_invite() -> InviteLink {
        let inviter = IdentityKey::generate().unwrap();
        InviteLink::generate(
            &inviter,
            "a.onion".into(),
            fixed_kp(),
            [0xAA; 32],
            3600,
            1_000_000,
        )
        .unwrap()
    }

    #[test]
    fn kp_hash_matches_sha256_of_key_package() {
        let invite = make_invite();
        let hash = invite.kp_hash();

        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&invite.body.key_package);
        let expected: [u8; 32] = h.finalize().into();
        assert_eq!(hash, expected);
    }

    #[test]
    fn record_received_then_is_consumed_false_then_mark_then_true() {
        let pool = Pool::in_memory();
        let kp_repo = KeyPackageRepo::new(&pool);
        let invite = make_invite();

        invite.record_received(&kp_repo).unwrap();
        assert!(!invite.is_consumed(&kp_repo).unwrap());

        invite.mark_consumed(&kp_repo).unwrap();
        assert!(invite.is_consumed(&kp_repo).unwrap());
    }

    #[test]
    fn record_received_is_idempotent() {
        let pool = Pool::in_memory();
        let kp_repo = KeyPackageRepo::new(&pool);
        let invite = make_invite();

        invite.record_received(&kp_repo).unwrap();
        invite.record_received(&kp_repo).unwrap();

        let hash = invite.kp_hash();
        let row = kp_repo.get(&hash).unwrap().unwrap();
        assert_eq!(row.0, invite.body.key_package);
    }

    #[test]
    fn mark_consumed_on_unrecorded_errors() {
        let pool = Pool::in_memory();
        let kp_repo = KeyPackageRepo::new(&pool);
        let invite = make_invite();
        let err = invite.mark_consumed(&kp_repo).expect_err("unrecorded");
        match err {
            crate::error::CoreError::Invite(s) => {
                assert!(s.contains("unknown"), "got: {s}");
            }
            other => panic!("expected Invite, got {other:?}"),
        }
    }

    #[test]
    fn is_consumed_returns_false_for_unrecorded() {
        let pool = Pool::in_memory();
        let kp_repo = KeyPackageRepo::new(&pool);
        let invite = make_invite();
        assert!(!invite.is_consumed(&kp_repo).unwrap());
    }
```

- [ ] **Step 2: Run — expect `todo!()` panics**

```bash
cargo test -p skattr-core --lib invite::link::tests
```

- [ ] **Step 3: Implement the four methods**

Replace the four `todo!("Task 10")` bodies in `crates/core/src/invite/link.rs` with:

```rust
    pub fn kp_hash(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&self.body.key_package);
        h.finalize().into()
    }

    pub fn record_received(&self, kp_repo: &KeyPackageRepo<'_>) -> Result<()> {
        let hash = self.kp_hash();
        // Idempotent: if already present, no-op.
        if kp_repo.get(&hash)?.is_some() {
            return Ok(());
        }
        kp_repo.insert(&hash, &self.body.key_package, "theirs")
    }

    pub fn is_consumed(&self, kp_repo: &KeyPackageRepo<'_>) -> Result<bool> {
        let hash = self.kp_hash();
        match kp_repo.get(&hash)? {
            Some((_, consumed)) => Ok(consumed),
            None => Ok(false),
        }
    }

    pub fn mark_consumed(&self, kp_repo: &KeyPackageRepo<'_>) -> Result<()> {
        let hash = self.kp_hash();
        if kp_repo.get(&hash)?.is_none() {
            return Err(crate::error::CoreError::Invite(
                "invite: unknown: not recorded".into(),
            ));
        }
        kp_repo.mark_consumed(&hash)
    }
```

- [ ] **Step 4: Run the tests**

```bash
cargo test -p skattr-core --lib invite::link::tests
```

Expected: 14 PASS (9 from Tasks 7-9 + 5 new).

- [ ] **Step 5: Verify fmt + clippy**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/invite/link.rs
git commit -m "$(cat <<'EOF'
invite: single-use tracking via KeyPackageRepo.consumed

Four methods wire invite replay prevention:
- kp_hash: SHA-256 of body.key_package (storage key)
- record_received: inserts with direction='theirs', idempotent via
  get-before-insert
- is_consumed: reads consumed flag; returns false for unrecorded KPs
  (caller decides to also check is_recorded if that distinction matters)
- mark_consumed: errors if unrecorded ("invite: unknown: not
  recorded") to surface a caller-side bug (the caller must record
  before marking)

Five tests cover the full lifecycle.
EOF
)"
```

---

## Task 11: QR SVG rendering

**Goal:** `invite::qr::render_svg` produces SVG markup for an `InviteLink`'s URL. Feature-gated on `qr`.

**Files:**
- Modify: `crates/core/src/invite/qr.rs`

- [ ] **Step 1: Write the failing test**

Open `crates/core/src/invite/qr.rs`. Append:

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::identity::IdentityKey;

    #[test]
    fn render_svg_produces_non_empty_svg_document() {
        let inviter = IdentityKey::generate().unwrap();
        let invite = InviteLink::generate(
            &inviter,
            "a.onion".into(),
            (0..64u8).collect(),
            [0x11; 32],
            3600,
            1_000_000,
        )
        .unwrap();
        let svg = render_svg(&invite).unwrap();
        assert!(svg.starts_with("<svg") || svg.contains("<svg"), "got: {svg}");
        assert!(svg.len() > 100, "svg should be non-trivial");
    }
}
```

- [ ] **Step 2: Run — expect `todo!()` panic**

```bash
cargo test -p skattr-core --lib --features qr invite::qr::tests::render_svg_produces_non_empty_svg_document
```

- [ ] **Step 3: Implement `render_svg`**

Replace the `todo!("Task 11")` body in `crates/core/src/invite/qr.rs` with:

```rust
use qrcode::render::svg;
use qrcode::{EcLevel, QrCode};

use crate::error::CoreError;

pub fn render_svg(invite: &InviteLink) -> Result<String> {
    let url = invite
        .to_url()
        .map_err(|e| CoreError::Invite(format!("invite: qr: url: {e}")))?;
    let code = QrCode::with_error_correction_level(url.as_bytes(), EcLevel::M)
        .map_err(|e| CoreError::Invite(format!("invite: qr: encode: {e}")))?;
    Ok(code
        .render::<svg::Color<'_>>()
        .min_dimensions(200, 200)
        .build())
}
```

Note the `use` statements: they need to land at the top of `invite/qr.rs` with the existing `use crate::error::Result;` line. Consolidate into a single block if that's the file's style.

If `svg::Color` takes a lifetime parameter (it does in qrcode 0.14), the `<'_>` anonymous lifetime annotation is required.

- [ ] **Step 4: Run the test**

```bash
cargo test -p skattr-core --lib --features qr invite::qr::tests::render_svg_produces_non_empty_svg_document
```

Expected: PASS.

- [ ] **Step 5: Also run the full suite with `qr` feature enabled**

```bash
cargo test -p skattr-core --lib --features qr
```

Expected: everything still passes. `qr` is in the default feature set per `crates/core/Cargo.toml`, so this also covers the `cargo test --features test-harness` path.

- [ ] **Step 6: Verify fmt + clippy**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/invite/qr.rs
git commit -m "$(cat <<'EOF'
invite: render_svg produces SVG markup for an InviteLink's URL

Uses qrcode 0.14's QrCode::with_error_correction_level(..., EcLevel::M)
— 15% recovery, adequate for short invite URLs. min_dimensions
clamps the output to at least 200x200 so small invites still scan
reasonably on high-DPI screens.

Test asserts the output contains <svg and is non-trivial in length.
Feature-gated on 'qr' per the existing invite::qr module config.
EOF
)"
```

---

## Task 12: Integration test — `crates/tests/src/invite_roundtrip.rs`

**Goal:** Alice mints invite → Bob parses, records, queries consumed, marks consumed, replays, all works end-to-end with separate in-memory pools. Feature-gated on `test-harness`.

**Files:**
- Create: `crates/tests/src/invite_roundtrip.rs`
- Modify: `crates/tests/src/lib.rs`

- [ ] **Step 1: Declare the module in `crates/tests/src/lib.rs`**

Open `crates/tests/src/lib.rs`. Find the existing `#[cfg(test)] mod arti_echo;` and `#[cfg(test)] mod mls_pair;` lines. Append:

```rust
#[cfg(test)]
mod invite_roundtrip;
```

- [ ] **Step 2: Create the integration test file**

Create `crates/tests/src/invite_roundtrip.rs` with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Integration test: Alice mints invite → Bob parses + verifies +
//! records + marks consumed. No Tor, no Noise, no MLS — just the
//! invite layer exercised against separate in-memory pools.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use skattr_core::identity::IdentityKey;
use skattr_core::test_exports::{InviteLink, KeyPackageRepo, Pool};

#[test]
fn alice_mints_invite_bob_parses_records_and_consumes() {
    let alice_id = IdentityKey::generate().unwrap();
    // For 1.D the integration test uses opaque KP bytes — it doesn't
    // need to generate a real MLS KeyPackage. 1.F integrates the full
    // flow end-to-end with a real KP.
    let kp_bytes: Vec<u8> = (0..128u8).collect();
    let psk = [0x5A; 32];

    let invite = InviteLink::generate(&alice_id, "abc.onion".into(), kp_bytes.clone(), psk, 3600, 1_000_000)
        .unwrap();
    let url = invite.to_url().unwrap();

    // Bob's side — separate pool.
    let bob_pool = Pool::in_memory();
    let bob_kp_repo = KeyPackageRepo::new(&bob_pool);

    let parsed = InviteLink::from_url(&url, 1_000_010).unwrap();
    assert_eq!(parsed.body.identity, alice_id.public());
    assert_eq!(parsed.body.key_package, kp_bytes);

    parsed.record_received(&bob_kp_repo).unwrap();
    assert!(!parsed.is_consumed(&bob_kp_repo).unwrap());

    parsed.mark_consumed(&bob_kp_repo).unwrap();
    assert!(parsed.is_consumed(&bob_kp_repo).unwrap());

    // Replay attempt — parsing is stateless, so parse still succeeds.
    let reparsed = InviteLink::from_url(&url, 1_000_020).unwrap();
    // record_received is idempotent — no error on second call.
    reparsed.record_received(&bob_kp_repo).unwrap();
    // is_consumed still true — Bob's local state remembers.
    assert!(reparsed.is_consumed(&bob_kp_repo).unwrap());

    // Expiry: past TTL (was 3600 seconds from 1_000_000).
    let err = InviteLink::from_url(&url, 1_003_601).expect_err("expired");
    match err {
        skattr_core::error::CoreError::Invite(s) => assert!(s.contains("expired"), "got: {s}"),
        other => panic!("expected Invite, got {other:?}"),
    }
}
```

Note: `Pool` must be in `test_exports`. It was added in Phase 1.C Task 12 when the `test_*` wrappers were simplified. Verify via `grep -n "pub.*Pool" /home/myggiz/development/skattr-phase-1d-invite-contact/crates/core/src/lib.rs`. If not present, add it. (See Task 1's test_exports extension — may already be there.)

- [ ] **Step 3: Run the integration test**

```bash
cargo test -p skattr-tests invite_roundtrip
```

Expected: `alice_mints_invite_bob_parses_records_and_consumes` PASS.

- [ ] **Step 4: Run the full workspace test suite**

```bash
cargo test --workspace --all-features --release
```

Expected: all tests pass.

- [ ] **Step 5: Verify fmt + clippy**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/tests/src/invite_roundtrip.rs crates/tests/src/lib.rs
git commit -m "$(cat <<'EOF'
tests: integration test for Alice-mints-invite → Bob-consumes flow

crates/tests/src/invite_roundtrip.rs runs the full 1.D invite flow
end-to-end:
- Alice generates an IdentityKey + opaque KP bytes, mints an invite,
  serializes to skattr://invite/v1# URL.
- Bob (separate in-memory Pool + KeyPackageRepo) parses the URL,
  verifies the signature, asserts body fields match what Alice sent,
  records the KP, checks is_consumed is false, marks consumed,
  checks is_consumed is true.
- Replay: parses again, record_received is idempotent, is_consumed
  stays true.
- Expiry: parsing at now > expires_at fails with "expired".

Opaque KP bytes are used here — 1.F integration wires a real MLS
KeyPackage from 1.C.
EOF
)"
```

---

## Task 13: CHANGELOG + CLAUDE.md + final verification

**Goal:** Document what 1.D shipped and refresh the repository-state paragraph. Full-matrix verification.

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add CHANGELOG bullet**

Open `CHANGELOG.md`. Under `## [Unreleased]` → `### Added`, immediately after the Phase 1.C bullet, add:

```markdown
- **Phase 1.D Invite & contact flow:** `invite::InviteLink` mints signed `skattr://invite/v1#id=&onion=&kp=&psk=&exp=&sig=` URLs with a fixed field order; `from_url(url, now)` parses, verifies Ed25519 over canonical CBOR of the unsigned body, checks expiry, and moves the 32-byte PSK into a `Zeroizing` guard (body copy zeroized). Single-use replay prevention reuses 1.C's `KeyPackageRepo.consumed` flag keyed by SHA-256 of the KP bytes (`record_received` / `is_consumed` / `mark_consumed`). `contact::ContactCard::{sign, verify(now)}` with canonical-CBOR body signing + expiry check; monotonic version persistence via a new `contact_cards` table (migration 0003) with `put_card` rejecting stale or equal versions and `latest_card` returning the top version. `ContactRepo::get` / `list` now hydrate `Contact.card`. `IdentityKey::sign_cbor` / `verify_cbor` helpers factor the body-signing pattern for both invite and card. QR SVG rendering via `qrcode` (feature `qr`); `render_png` removed per scope. Coverage: 14 `invite::link` unit tests, 5 `contact::card` tests, 12 `storage::contacts` tests (6 new), 1 QR test, 1 integration test `crates/tests/src/invite_roundtrip.rs`.
```

- [ ] **Step 2: Refresh CLAUDE.md Repository-state paragraph**

Open `CLAUDE.md`. Find the "Phase 0 is complete; Phase 1.A ... and 1.C ... are done" paragraph (it mentions `MlsProvider` and `KeyPackage`). Replace it with:

```markdown
**Phase 0 is complete; Phase 1.A (frame codec), 1.B (Noise_XK
handshake), 1.C (MLS 2-member groups), and 1.D (invite & contact
flow) are done.** Phase 0 shipped all five workstreams (0.A scaffold,
0.B identity & crypto, 0.C Arti integration, 0.D storage layer, 0.E
documentation baseline). Phase 1.A added `transport::frame::FrameCodec`.
Phase 1.B added `transport::noise::handshake_{initiator,responder}`
+ the stateful `AuthenticatedConnection<S>` wrapper, plus the
Ed25519 → X25519 bridge on `IdentityKey`. Phase 1.C added `mls::Group`
(2-member only), `MlsProvider` checkpoint-snapshot persistence,
`KeyPackage` newtype + `KeyPackageRepo`, and migration 0002. Phase
1.D added `invite::InviteLink` (skattr://invite/v1# URL with
fragment-only params, canonical-CBOR Ed25519 signature, Zeroizing
PSK guard, single-use tracking via `KeyPackageRepo.consumed`),
`contact::ContactCard::{sign, verify}` with monotonic-version
persistence in a new `contact_cards` table (migration 0003), and
`IdentityKey::{sign_cbor, verify_cbor}` helpers.
```

Update the "Phase 1 continues with" paragraph to remove 1.D:

```markdown
Phase 1 continues with 1.E delivery semantics, 1.F CLI integration,
1.G message storage & search — see
`docs/superpowers/specs/2026-04-21-phase-1-decomposition.md` for the
full Phase 1 split. The bootstrap prompt remains authoritative for
file layout, module boundaries, type signatures, and visibility rules
— match it exactly.
```

- [ ] **Step 3: Full check matrix**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features --release
```

All three green. Test counts should show the new 1.D tests passing alongside all prior tests.

- [ ] **Step 4: Sanity-check integration suite still compiles**

```bash
cargo test -p skattr-tests --release --no-run
```

Expected: clean compile. Nothing in 1.D should have broken `arti_echo` or `mls_pair`.

- [ ] **Step 5: Commit**

```bash
git add CHANGELOG.md CLAUDE.md
git commit -m "$(cat <<'EOF'
docs: CHANGELOG + CLAUDE.md — Phase 1.D invite & contact flow done

CHANGELOG captures the 1.D scope: InviteLink mint + parse + single-
use tracking via KeyPackageRepo, ContactCard sign/verify with
monotonic versions persisted in the new contact_cards table,
IdentityKey::sign_cbor / verify_cbor helpers, QR SVG rendering.
CLAUDE.md Repository-state paragraph now reflects 1.A + 1.B + 1.C
+ 1.D complete and points 1.E-1.G at the decomposition doc.
EOF
)"
```

---

## Exit verification

After Task 13, the worktree satisfies every item in the design spec's **Exit criteria** section:

1. All unit tests in `invite::link`, `invite::qr`, `contact::card`, `storage::contacts` pass — Tasks 2 / 4 / 5 / 6 / 7 / 8 / 9 / 10 / 11.
2. Integration test in `crates/tests/src/invite_roundtrip.rs` passes under `--features test-harness` — Task 12.
3. Full check matrix green — Task 13 Step 3.
4. `generate → to_url → from_url` round-trips with signature verification — Tasks 7 / 8 / 9 tests.
5. Expired invite rejected — Task 9 tests; Task 12 replay scenario.
6. Single-use: `is_consumed` reflects `mark_consumed`; idempotent — Tasks 10 / 12.
7. `ContactCard::sign → verify` round-trips; stale version rejected at storage — Tasks 4 / 5 / 2.
8. Migration 0003 applies cleanly; `contact_cards` in expected-tables assertion — Task 2.
9. CHANGELOG + CLAUDE.md refreshed — Task 13.
10. No PNG QR, no onion rotation, no CLI wiring, no mailbox-list semantics — all explicitly out of scope.

After confirming all boxes, `subagent-driven-development` merges `phase-1d-invite-contact` → `master` with `--no-ff` and removes the worktree.
