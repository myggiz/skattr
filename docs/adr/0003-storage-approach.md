# ADR 0003: Storage approach

- **Status:** Accepted
- **Date:** 2026-04-16

## Context

Skattr persists contacts, MLS state, message history with full-text
search, an outbox, and mailbox registrations. The store lives on the
user's device and must:

- Survive crashes (writes are atomic).
- Be encrypted at rest with a key derived from the identity secret.
- Support FTS over message bodies.
- Migrate forward cleanly across versions without losing messages.

## Decision

**`rusqlite` with bundled SQLite, plus app-level file encryption via
`age`.**

- `rusqlite` with the `bundled` feature statically links SQLite so we
  don't depend on whichever SQLite version the host happens to have.
- WAL mode (`journal_mode=WAL`, `synchronous=NORMAL`) for fast,
  crash-safe writes.
- FTS5 virtual table over `messages.body_blob` (text-kind only),
  maintained by triggers.
- Encryption: at startup we open `skattr.sqlite.age`, decrypt to a
  temporary path (or into memory for small stores), and work on the
  decrypted file. On clean shutdown we re-encrypt. The `age` key is
  derived from the identity seed via `HKDF("skattr-storage-v1")`.
- Migrations: a `schema_version` table plus `include_str!`-embedded
  `NNNN_*.sql` files, applied in lexicographic order.

## Consequences

- **Good:** SQLite is by far the most battle-tested embedded store.
  WAL mode is well understood. FTS5 gives us search without adding
  another dependency.
- **Good:** `age` is audited, small, and has well-defined wire
  compatibility — easier to reason about than SQLCipher's custom
  codec.
- **Bad:** decrypt-to-temp means a cleartext DB briefly exists on disk
  during a session. This is a trade-off we accept for Phase 0–3; a
  future phase may reconsider using SQLCipher for a fully
  always-encrypted page format. Document this in the threat model.
- **Bad:** we own the migration tooling, unlike using `refinery` or
  `diesel`. Acceptable given the tiny set of migrations we expect to
  accumulate.

## Alternatives considered

- **SQLCipher:** proven, page-level encryption. Rejected because the
  codec is bespoke, pulls a large C patch onto SQLite, and
  complicates reproducible builds.
- **`sled` / pure-Rust KV:** rejected. No FTS, less mature than
  SQLite, no WAL equivalent. Message search is a first-class feature.
- **`sqlx`:** async SQLite bindings. Rejected for `core` — `rusqlite`'s
  blocking API is a better fit for the single-writer thread model, and
  we don't need the compile-time query validation for a tiny query
  set.
