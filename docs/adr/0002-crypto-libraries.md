# ADR 0002: Cryptographic library choices

- **Status:** Accepted
- **Date:** 2026-04-16

## Context

Skattr needs Ed25519 (identity), X25519 (MLS DHKEM, Noise key
exchange), ChaCha20-Poly1305 (MLS AEAD, Noise cipher), SHA-256, HKDF,
Argon2id, BIP39, and an MLS implementation. Every choice here is a
dependency audit question, and switching crates after shipping is
expensive.

## Decision

We adopt the **RustCrypto** ecosystem plus **OpenMLS** and **snow**.

| Primitive | Crate | Notes |
|-----------|-------|-------|
| Ed25519 | `ed25519-dalek` v2 | `zeroize` feature on. The de-facto Rust Ed25519 crate. |
| X25519 | `x25519-dalek` v2 | `static_secrets` + `zeroize` features. |
| ChaCha20-Poly1305 | `chacha20poly1305` | RustCrypto; constant-time by construction. |
| SHA-256 | `sha2` | RustCrypto. |
| HKDF | `hkdf` | RustCrypto; HKDF-SHA256. |
| Argon2id | `argon2` | RustCrypto; parameters `m=64 MiB, t=3, p=4`. |
| BIP39 | `bip39` v2 | Standard, English wordlist only. |
| MLS | `openmls` | The mainline Rust MLS implementation. |
| Noise | `snow` | Pattern: `Noise_XK_25519_ChaChaPoly_BLAKE2s` — the only pattern used in production (`psk = None` at every call site). A `Noise_XKpsk3_25519_ChaChaPoly_BLAKE2s` constant also exists in `transport/noise.rs` for an invite-PSK handshake, but **no production path selects it**; the invite PSK is applied at the MLS layer instead (ADR 0009). |

All are pure Rust. No system OpenSSL dependency. No custom
cryptographic primitives.

## Consequences

- **Good:** auditable Rust source end-to-end; no `unsafe` leaking
  into primitives from a C dependency.
- **Good:** uniform ownership model (`Zeroize`, `ZeroizeOnDrop`) and
  consistent error types across primitives.
- **Bad:** BLAKE2s for Noise and SHA-256 for MLS means two hash
  implementations in the binary. Acceptable — swapping Noise to SHA-256
  would diverge from the standard Signal-style transport pattern.
- **Bad:** OpenMLS API moves faster than most crates; we expect to pin
  and bump deliberately at each phase boundary.

## Alternatives considered

- **`ring`:** mature but closed-ish governance, no MLS support, mixed
  Rust/assembly. Rejected for the same reason we avoided OpenSSL.
- **`openssl` / `boring`:** ruled out — introduces a large C surface
  and adds build complexity across Linux / macOS / Windows.
- **mls-rs:** AWS Labs, also mainline. Considered; OpenMLS chosen for
  longer presence in the Rust ecosystem. Revisit at Phase 3 if
  OpenMLS's upstream API churn bites.
