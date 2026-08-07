# ADR 0004: Passphrase Unicode normalization

- **Status:** Accepted
- **Date:** 2026-04-17

## Context

`derive_aead_key` in `crates/core/src/identity/vault.rs` feeds the
user's passphrase into Argon2id as `passphrase.as_bytes()`. Non-ASCII
passphrases can be represented in multiple equivalent Unicode forms
(NFC, NFD, NFKC, NFKD); the same visible string may yield different
byte sequences depending on the input method, OS, and copy-paste
source. Without a normalization step, a user who creates a vault on
macOS (which historically preferred NFD for HFS+ filenames) and
re-opens it on Linux (NFC) would fail authentication even when typing
the "same" passphrase.

## Decision

**Passphrase bytes are used verbatim with no normalization.** Callers
are responsible for feeding stable byte sequences. The CLI surface
documents this constraint prominently (README and on-screen prompt
guidance in Phase 2).

Rationale:

- Unicode normalization is subtle and adds attack surface — every
  additional step is a potential source of inconsistency.
- In the Phase 0.B/Phase 1 timeline, the CLI is the only surface;
  users typing at a terminal generally get consistent output from
  their input method.
- If we ever add mobile or web clients, those surfaces ship their
  own NFC-normalizing input and the caller can do the conversion.
- Fixing this later is a breaking change only for users who
  actually rely on non-ASCII passphrases — a small cohort. We
  accept the risk and document the contract.

## Consequences

- **Good:** No crypto-adjacent normalization code to audit.
- **Bad — UNMITIGATED as of v0.1.x.** Non-ASCII passphrases can lock users out
  if their input method changes. An earlier revision of this ADR stated the
  mitigation as fact ("the Phase 2 UI will ship an NFC normalization pass on the
  passphrase-entry textbox"). **That pass was never implemented** — there is no
  normalization anywhere in `crates/ui/src-svelte/src`, and the vault uses
  `passphrase.as_bytes()` verbatim (`identity/vault.rs`). The risk is therefore
  live, and the "ASCII passphrases recommended" guidance below is currently the
  only protection. Corrected 2026-08-07; shipping the normalization pass (or
  formally accepting the risk) is tracked as outstanding work.
- Docstring on `derive_aead_key` and the restore/init CLI surfaces
  will state: "ASCII passphrases recommended; non-ASCII entries are
  used verbatim and will not round-trip across OSes with different
  default Unicode forms."

## Alternatives considered

- **NFC normalization in `derive_aead_key`:** rejected — adds an
  `unicode-normalization` dependency and shifts the contract. Revisit
  if user reports make this painful.
- **NFKC (compatibility normalization):** rejected — stronger than NFC,
  would silently collapse visually-distinct characters (e.g. ligatures)
  which is surprising.
- **Stronger input validation (ASCII-only enforcement):** rejected —
  excludes users with non-Latin keyboards from ever setting an
  ergonomic passphrase.
