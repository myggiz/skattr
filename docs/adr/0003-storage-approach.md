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

---

## Amendment (2026-08-07): the at-rest lifecycle as actually shipped

This ADR's original *Decision* text predates Phase 2.B (T1-2) and the v0.1.12
scrypt fix. Recorded here so the ADR matches the code.

### Decrypt target is a fixed path, not a temp file

The original text said "decrypt to a temporary path (or into memory for small
stores)". Neither shipped. `Pool::open` always decrypts `skattr.sqlite.age` to
the fixed working path `<data_dir>/skattr.sqlite` (`storage/pool.rs`); there is
no in-memory variant for persistent pools. **A plaintext DB therefore exists on
disk for the lifetime of the daemon** — at-rest encryption protects data
*between* runs, not during operation. This is disclosed in `SECURITY.md`.

### Full lifecycle (Phase 2.B)

- `Pool::open` writes a `skattr.sqlite.open` **sentinel** while the plaintext DB
  is live.
- **Crash-residue recovery:** if a plaintext DB is present at open (i.e. the
  previous run died without closing), it is checkpointed and re-encrypted to
  `.age` on boot, so a current ciphertext always exists.
- `Pool::close(&self)` is idempotent and guarded: `wal_checkpoint(TRUNCATE)` →
  drop connection → encrypt → remove the plaintext DB, its `-wal`/`-shm`
  sidecars, and the sentinel. `Drop` is the backstop for abnormal exits.
- A second, independent at-rest layer exists for backups: `export_backup` /
  `snapshot_encrypted` (`storage/backup.rs`) under its own domain-separated key
  (`INFO_BACKUP_V1`).

### We deliberately override age's scrypt work-factor calibration

**Decision:** encrypt with a fixed scrypt work factor `AGE_WORK_FACTOR = 12`
(`N = 2^12`) and decrypt with a fixed ceiling `AGE_MAX_WORK_FACTOR = 22`, applied
identically in `storage/pool.rs`, `storage/backup.rs`, and
`transport/hs_key.rs`.

**Why:** `age::Encryptor::with_user_passphrase` auto-calibrates the work factor
to the *encrypting* machine's speed, and `scrypt::Identity::new` rejects on
decrypt when the file's factor exceeds a ceiling recomputed on the *reading*
machine. A file written on a fast/idle box was refused on a slower/loaded one
("Excessive work parameter… ~64 seconds"), locking the user out of their own HS
key, DB, and backups. Because every passphrase here is a full-entropy 256-bit
HKDF-derived key (hex-encoded), scrypt's low-entropy-passphrase stretching is
security-irrelevant — the calibration bought no security and created a real
availability failure.

**Consequences:** at-rest files are portable across machines of any speed; the
generous decrypt ceiling still reads legacy files written with device-calibrated
factors (observed 19–20 in the wild). This is a deliberate deviation from age's
defaults and must be preserved when touching any of the three encrypt/decrypt
sites. See the v0.1.12 fix (issue #121, PR #122).
