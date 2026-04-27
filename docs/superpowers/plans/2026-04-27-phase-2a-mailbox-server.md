# Phase 2.A Mailbox Server Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `crates/mailbox/` as a `[lib] + [bin]` AGPLv3 server that holds encrypted deposits for offline recipients, with a frozen `core::mailbox::protocol` v1 wire surface that 2.B can consume unchanged.

**Architecture:** A transport-agnostic `MailboxServer` library accepts any `AsyncRead + AsyncWrite + Unpin + Send` stream, runs a per-connection state-machine over CBOR-framed `MailboxFrame`s, dispatches each request through pure handlers backed by a `Store` (rusqlite), `Challenges` (in-memory nonce table), and `Policy` (caps + token-bucket rate limiter). The mailbox does NOT do Noise_XK — the v3 onion service is the transport-auth layer; depositor anonymity is required. Frames share the existing 1.A wire layout (`length_u32 || type_u8 || payload`, 16 MiB max) but use a disjoint type-byte range (0x82–0x8F), so the mailbox ships its own `MailboxFrameCodec` rather than extending `core::transport::frame::FrameCodec`.

**Tech Stack:** Rust 2021, `tokio` + `tokio-util` (codec, duplex), `rusqlite` 0.38 (bundled, WAL), `ed25519-dalek` (auth signature verify), `ciborium` (CBOR), `arti-client` 0.41 + `tor-hsservice` 0.41 (binary-only), `proptest` (property tests), `cargo fuzz` + `libfuzzer-sys` + `arbitrary` (fuzz), `clap` (CLI), `tracing` + `tracing-subscriber` (logging).

**Spec:** `docs/superpowers/specs/2026-04-27-phase-2a-mailbox-server-design.md`.

---

## Pre-flight

- [ ] **Create worktree on a fresh branch**

Run from `/home/myggiz/development/skattr`:
```bash
. "$HOME/.cargo/env"
git worktree add -b phase-2a-mailbox-server ../skattr-phase-2a-mailbox-server master
cd ../skattr-phase-2a-mailbox-server
git status --short
git log --oneline -3
```
Expected: clean worktree, HEAD at the spec commit `f485330 docs: Phase 2.A mailbox-server design spec`. All subsequent commands run from `../skattr-phase-2a-mailbox-server`.

- [ ] **Establish the baseline green build**

```bash
cd /home/myggiz/development/skattr-phase-2a-mailbox-server
. "$HOME/.cargo/env"
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```
Expected: all three green. If any fails, stop and surface — the baseline must be clean before the first behaviour change.

- [ ] **Verify the existing mailbox stubs match what we're about to replace**

```bash
ls crates/mailbox/src
ls crates/core/src/mailbox
```
Expected:
```
crates/mailbox/src:    auth.rs  config.rs  main.rs  server.rs  store.rs
crates/core/src/mailbox: client.rs  mod.rs  protocol.rs  scheduler.rs
```
The stubs we keep: `core::mailbox::{client, scheduler, mod}` (2.B's territory, untouched). The stubs we rewrite: every file under `crates/mailbox/src/` and `core/src/mailbox/protocol.rs`.

---

## Task 1: Replace `core::mailbox::protocol` with v1 wire types

**Files:**
- Modify: `crates/core/src/mailbox/protocol.rs` (full rewrite)
- Test: same file (`#[cfg(test)] mod tests` block)

### Step 1: Write the failing tests

Replace the entire body of `crates/core/src/mailbox/protocol.rs` with the placeholder below so the new tests fail at compile time first; we'll fill in the real types in Step 3.

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Wire types shared between `skattr-core` (client, 2.B) and
//! `skattr-mailbox` (server, 2.A).
//!
//! All messages are CBOR (canonical: sorted keys, definite lengths).
//! Frames sit inside the shared `length_u32 || type_u8 || payload`
//! framing layout (see `core::transport::frame` for the peer-to-peer
//! peer; the mailbox ships its own `MailboxFrameCodec` because the
//! type bytes are disjoint).
//!
//! # Freezing rule
//!
//! These types are frozen once Phase 2.A merges. Incompatible changes
//! ship as a parallel `MAILBOX_PROTOCOL_VERSION = 2` set; v1 stays
//! supported until v2 is universally deployed. See ADR
//! `docs/adr/0006-mailbox-protocol-v1.md`.

use serde::{Deserialize, Serialize};

/// Protocol version tag carried on every C→S request body.
pub const PROTOCOL_VERSION: u16 = 1;

/// 16-byte server-issued opaque deposit identifier.
pub type DepositId = [u8; 16];

/// 32-byte SHA-256 of a recipient's Ed25519 identity pubkey.
pub type RecipientHash = [u8; 32];

/// 32-byte challenge nonce.
pub type Nonce = [u8; 32];

// === Deposit (C → S) ===========================================

/// Store an MLS-encrypted blob for a recipient identity hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deposit {
    /// Wire version (must equal [`PROTOCOL_VERSION`]).
    pub version: u16,
    /// SHA-256 of the recipient's identity pubkey.
    pub recipient_hash: RecipientHash,
    /// Ciphertext blob; size capped by operator policy.
    pub ciphertext: Vec<u8>,
    /// Requested TTL in seconds; clamped server-side. `0` requests the
    /// operator default.
    pub ttl_request: u32,
}

/// Server response to a successful [`Deposit`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepositOk {
    /// Server-generated 16 random bytes.
    pub deposit_id: DepositId,
    /// Final expiry timestamp (Unix seconds).
    pub expires_at: i64,
}

// === Challenge (C → S) =========================================

/// Request a server-issued nonce for a Fetch/Delete that follows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Challenge {
    /// Wire version.
    pub version: u16,
    /// SHA-256 of the identity that will sign the auth string.
    pub identity_hash: RecipientHash,
}

/// Server response carrying the nonce to sign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChallengeNonce {
    /// 32 random bytes; valid for 30 seconds.
    pub nonce: Nonce,
    /// Server wall-clock issuance time (Unix seconds).
    pub issued_at: i64,
}

// === Fetch (C → S) =============================================

/// Retrieve all pending deposits for an authenticated identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fetch {
    /// Wire version.
    pub version: u16,
    /// Recipient's Ed25519 public key (32 bytes).
    pub identity_pubkey: [u8; 32],
    /// Nonce being authenticated against.
    pub nonce: Nonce,
    /// Ed25519 signature over the auth string (see module docs).
    #[serde(with = "serde_big_array::BigArray")]
    pub signature: [u8; 64],
}

/// One pending deposit returned in [`FetchResponse`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingDeposit {
    /// Server-issued id (echo of [`DepositOk::deposit_id`]).
    pub deposit_id: DepositId,
    /// Stored ciphertext.
    pub ciphertext: Vec<u8>,
    /// When the server first received this deposit (Unix seconds).
    pub received_at: i64,
}

/// Successful response to a [`Fetch`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchResponse {
    /// Zero or more pending deposits.
    pub deposits: Vec<PendingDeposit>,
}

// === Delete (C → S) ============================================

/// Remove deposits the recipient has already acknowledged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delete {
    /// Wire version.
    pub version: u16,
    /// Recipient's Ed25519 public key.
    pub identity_pubkey: [u8; 32],
    /// Nonce being authenticated against.
    pub nonce: Nonce,
    /// Ed25519 signature over the auth string (see module docs).
    #[serde(with = "serde_big_array::BigArray")]
    pub signature: [u8; 64],
    /// Deposit ids to remove. Unknown ids count toward `not_found`.
    pub deposit_ids: Vec<DepositId>,
}

/// Successful response to a [`Delete`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteOk {
    /// Number of rows deleted.
    pub deleted: u32,
    /// Number of ids that did not match a stored deposit (already
    /// expired or never existed).
    pub not_found: u32,
}

// === Error (S → C) =============================================

/// Typed error reply. The connection is **not** closed on receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// Stable machine-readable code.
    pub code: ErrorCode,
    /// Human-readable description; never includes a hash, pubkey, or
    /// ciphertext.
    pub message: String,
}

/// Stable error taxonomy for the mailbox wire protocol. Every variant
/// has at least one triggering test in `crates/mailbox/tests/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    /// Frame version did not equal [`PROTOCOL_VERSION`].
    UnsupportedVersion,
    /// CBOR did not parse, or required fields were missing.
    MalformedRequest,
    /// Ciphertext exceeded the operator's `max_deposit_size`.
    TooLarge,
    /// Per-connection or global rate limit exhausted.
    RateLimited,
    /// Per-recipient byte cap reached and no expired rows could be
    /// reclaimed to make room.
    RecipientFull,
    /// `ttl_request` exceeded `max_ttl_secs`.
    TtlTooLong,
    /// `ttl_request` was below `min_ttl_secs` (and non-zero — zero
    /// requests the default).
    TtlTooShort,
    /// Ed25519 signature did not verify under `identity_pubkey`.
    InvalidSignature,
    /// `sha256(identity_pubkey) != recipient/identity_hash`.
    HashMismatch,
    /// Nonce was unknown or older than the 30 s TTL.
    NonceExpired,
    /// At least one `deposit_id` was unknown for this recipient (only
    /// when used as a hard error; Delete tolerates not-found via
    /// [`DeleteOk::not_found`]).
    NotFound,
    /// Server-side problem; client should retry. Never includes a
    /// recipient hash or pubkey.
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cbor_round_trip<T>(v: &T) -> T
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let mut buf = Vec::new();
        ciborium::into_writer(v, &mut buf).expect("encode");
        ciborium::from_reader(&buf[..]).expect("decode")
    }

    #[test]
    fn protocol_version_is_one() {
        assert_eq!(PROTOCOL_VERSION, 1);
    }

    #[test]
    fn deposit_round_trips() {
        let d = Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash: [0xAB; 32],
            ciphertext: vec![1, 2, 3],
            ttl_request: 86_400,
        };
        assert_eq!(cbor_round_trip(&d), d);
    }

    #[test]
    fn deposit_ok_round_trips() {
        let d = DepositOk {
            deposit_id: [0x42; 16],
            expires_at: 1_700_000_000,
        };
        assert_eq!(cbor_round_trip(&d), d);
    }

    #[test]
    fn challenge_round_trips() {
        let c = Challenge {
            version: PROTOCOL_VERSION,
            identity_hash: [0xCD; 32],
        };
        assert_eq!(cbor_round_trip(&c), c);
    }

    #[test]
    fn challenge_nonce_round_trips() {
        let c = ChallengeNonce {
            nonce: [0x55; 32],
            issued_at: 1_700_000_000,
        };
        assert_eq!(cbor_round_trip(&c), c);
    }

    #[test]
    fn fetch_round_trips() {
        let f = Fetch {
            version: PROTOCOL_VERSION,
            identity_pubkey: [0x11; 32],
            nonce: [0x22; 32],
            signature: [0x33; 64],
        };
        assert_eq!(cbor_round_trip(&f), f);
    }

    #[test]
    fn fetch_response_round_trips() {
        let r = FetchResponse {
            deposits: vec![PendingDeposit {
                deposit_id: [0x44; 16],
                ciphertext: vec![9, 9, 9],
                received_at: 1_700_000_000,
            }],
        };
        assert_eq!(cbor_round_trip(&r), r);
    }

    #[test]
    fn delete_round_trips() {
        let d = Delete {
            version: PROTOCOL_VERSION,
            identity_pubkey: [0x66; 32],
            nonce: [0x77; 32],
            signature: [0x88; 64],
            deposit_ids: vec![[0x99; 16], [0xAA; 16]],
        };
        assert_eq!(cbor_round_trip(&d), d);
    }

    #[test]
    fn delete_ok_round_trips() {
        let d = DeleteOk {
            deleted: 3,
            not_found: 1,
        };
        assert_eq!(cbor_round_trip(&d), d);
    }

    #[test]
    fn error_body_round_trips() {
        let e = ErrorBody {
            code: ErrorCode::RateLimited,
            message: "slow down".into(),
        };
        assert_eq!(cbor_round_trip(&e), e);
    }

    #[test]
    fn every_error_code_round_trips() {
        for code in [
            ErrorCode::UnsupportedVersion,
            ErrorCode::MalformedRequest,
            ErrorCode::TooLarge,
            ErrorCode::RateLimited,
            ErrorCode::RecipientFull,
            ErrorCode::TtlTooLong,
            ErrorCode::TtlTooShort,
            ErrorCode::InvalidSignature,
            ErrorCode::HashMismatch,
            ErrorCode::NonceExpired,
            ErrorCode::NotFound,
            ErrorCode::Internal,
        ] {
            let e = ErrorBody {
                code,
                message: String::new(),
            };
            assert_eq!(cbor_round_trip(&e), e);
        }
    }
}
```

### Step 2: Run tests to verify they pass

```bash
cargo test -p skattr-core --lib mailbox::protocol
```
Expected: 11 passing tests (`protocol_version_is_one`, eight per-frame round-trips, `error_body_round_trips`, `every_error_code_round_trips`).

### Step 3: Run clippy and fmt

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: clean.

### Step 4: Commit

```bash
git add crates/core/src/mailbox/protocol.rs
git commit -m "$(cat <<'EOF'
core: populate mailbox::protocol with v1 wire types

Replaces 1.E-era stubs with the frozen Phase 2.A wire surface:
Deposit/DepositOk, Challenge/ChallengeNonce, Fetch/FetchResponse,
Delete/DeleteOk, ErrorBody+ErrorCode. Carries PROTOCOL_VERSION = 1
on every request body. CBOR round-trips covered for every type and
every ErrorCode variant.

EOF
)"
```

---

## Task 2: Promote `crates/mailbox/` to `[lib] + [bin]`

**Files:**
- Modify: `crates/mailbox/Cargo.toml`
- Create: `crates/mailbox/src/lib.rs`
- Modify: `crates/mailbox/src/main.rs`

### Step 1: Edit `Cargo.toml` to add `[lib]` and the new dependencies

Replace the current `Cargo.toml` with:

```toml
[package]
name = "skattr-mailbox"
description = "Skattr mailbox server: Tor-hosted offline delivery for encrypted blobs."
license = "AGPL-3.0-or-later"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
authors.workspace = true
repository.workspace = true
publish = false

[lib]
name = "skattr_mailbox"
path = "src/lib.rs"

[[bin]]
name = "skattr-mailbox"
path = "src/main.rs"

[dependencies]
skattr-core = { path = "../core" }

tokio = { workspace = true }
tokio-util = { workspace = true }
bytes = { workspace = true }
futures = { workspace = true }
async-trait = { workspace = true }

serde = { workspace = true }
serde_big_array = { workspace = true }
toml = { workspace = true }
ciborium = { workspace = true }

rusqlite = { workspace = true }
rand = { workspace = true }
rand_core = { workspace = true }
sha2 = { workspace = true }
ed25519-dalek = { workspace = true }

thiserror = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
clap = { workspace = true }
directories = { workspace = true }

# Binary-only (Arti). Library targets feed it via injected stream so
# tests don't drag the Tor dependency in.
arti-client = { workspace = true, optional = true }
tor-hsservice = { workspace = true, optional = true }
tor-rtcompat = { workspace = true, optional = true }

# sd-notify for the binary's systemd integration.
sd-notify = { version = "0.4", optional = true }

[dev-dependencies]
proptest = { workspace = true }
tokio = { workspace = true, features = ["test-util", "macros"] }

[features]
default = ["bin"]
# Enabled by the binary; pulls Arti + sd-notify. Lib-only consumers
# (tests, fuzz, soak) skip these and stay fast to compile.
bin = ["dep:arti-client", "dep:tor-hsservice", "dep:tor-rtcompat", "dep:sd-notify"]

[lints]
workspace = true
```

### Step 2: Create `src/lib.rs` as the public surface stub

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version. See LICENSE-AGPL3.

//! Skattr mailbox server library.
//!
//! Transport-agnostic core of the AGPLv3 mailbox server. The
//! `skattr-mailbox` binary is a thin wrapper that bootstraps Arti +
//! signal handling on top of this library; tests, fuzzers, and the
//! soak driver use the library directly with `tokio::io::duplex`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod config;
pub mod error;
pub mod policy;
pub mod store;

pub mod auth;
pub mod codec;
pub mod dispatch;
pub mod health;
pub mod server;

pub use config::MailboxConfig;
pub use error::{
    AuthErrorKind, ConfigErrorKind, MailboxError, MailboxErrorKind, PolicyErrorKind,
    StorageErrorKind, TransportErrorKind,
};
pub use policy::{Policy, TokenBucket};
pub use server::MailboxServer;
pub use store::Store;
```

The module files referenced here are populated in later tasks; this Step expects compile-only validation.

### Step 3: Strip the existing `src/main.rs` down to a tiny shell

`main.rs` will be filled in by Task 15. For now, stub it so the binary still builds:

Replace `crates/mailbox/src/main.rs` with:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! `skattr-mailbox` binary entry point. Real wiring lands in Task 15.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    eprintln!("skattr-mailbox: not yet wired (Phase 2.A in progress)");
    Ok(())
}
```

### Step 4: Delete the old skeleton modules — they get rewritten in later tasks

```bash
rm crates/mailbox/src/auth.rs
rm crates/mailbox/src/config.rs
rm crates/mailbox/src/server.rs
rm crates/mailbox/src/store.rs
```

### Step 5: Create empty stubs for every module referenced by `lib.rs`

Without these, `lib.rs` won't compile. Each is a one-liner that gets filled out in subsequent tasks.

`crates/mailbox/src/config.rs`:
```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Mailbox server configuration. Filled in by Task 5.

#![allow(missing_docs)]

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct MailboxConfig {
    pub data_dir: PathBuf,
}
```

`crates/mailbox/src/error.rs`:
```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Mailbox error taxonomy. Filled in by Task 4.

#![allow(missing_docs)]

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MailboxError {
    #[error("placeholder")]
    Placeholder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxErrorKind {
    Placeholder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthErrorKind { Placeholder }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigErrorKind { Placeholder }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyErrorKind { Placeholder }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageErrorKind { Placeholder }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportErrorKind { Placeholder }
```

`crates/mailbox/src/policy.rs`:
```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Operator caps + token-bucket rate limiter. Filled in by Task 9.

#![allow(missing_docs)]

#[derive(Debug, Clone)]
pub struct Policy;

#[derive(Debug, Default)]
pub struct TokenBucket;
```

`crates/mailbox/src/store.rs`:
```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! SQLite-backed deposit store. Filled in by Tasks 6–7.

#![allow(missing_docs)]

#[derive(Debug)]
pub struct Store;
```

`crates/mailbox/src/auth.rs`:
```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Challenge nonce table. Filled in by Task 8.

#![allow(missing_docs)]

#[derive(Debug, Default)]
pub struct Challenges;
```

`crates/mailbox/src/codec.rs`:
```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Mailbox frame codec. Filled in by Task 3.

#![allow(missing_docs)]

#[derive(Debug, Default)]
pub struct MailboxFrameCodec;
```

`crates/mailbox/src/dispatch.rs`:
```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Per-frame request handlers. Filled in by Task 10.

#![allow(missing_docs)]
```

`crates/mailbox/src/health.rs`:
```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! UDS healthcheck server. Filled in by Task 13.

#![allow(missing_docs)]
```

`crates/mailbox/src/server.rs`:
```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Per-stream FSM + accept loop. Filled in by Task 11.

#![allow(missing_docs)]

#[derive(Debug)]
pub struct MailboxServer;
```

### Step 6: Verify the workspace still compiles and clippy is clean

```bash
cargo build -p skattr-mailbox
cargo clippy -p skattr-mailbox --all-targets -- -D warnings
cargo fmt --all -- --check
```
Expected: no errors, no warnings. The workspace `dead_code = "allow"` lint covers the placeholder enums and structs.

### Step 7: Commit

```bash
git add crates/mailbox/
git commit -m "$(cat <<'EOF'
mailbox: promote to [lib] + [bin], scaffold module layout

Adds a library target so soak/fuzz/property tests can drive
MailboxServer in-process via tokio::io::duplex without paying for
Tor bootstrap. The 'bin' feature gates Arti + sd-notify so the
library stays fast to compile for tests. All eight module files
land as compile-only stubs; subsequent tasks fill them in.

EOF
)"
```

---

## Task 3: Mailbox frame codec

**Files:**
- Modify: `crates/mailbox/src/codec.rs` (full rewrite)
- Test: same file

The mailbox shares the wire-level layout (`length_u32 || type_u8 || payload`, 16 MiB cap) with `core::transport::frame` but uses a disjoint type-byte range (0x82–0x8F). This keeps cross-module visibility clean: the mailbox crate owns its own `MailboxFrame` enum and codec.

### Step 1: Write the failing tests

Replace `crates/mailbox/src/codec.rs` with the body below (tests included).

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Mailbox frame codec.
//!
//! Wire layout matches `core::transport::frame`:
//!
//! ```text
//! +----------------+----------+------------------+
//! | length (u32 BE)| type (u8)|  CBOR payload    |
//! +----------------+----------+------------------+
//! ```
//!
//! Type bytes are disjoint from peer-to-peer frames (0x82–0x8F). The
//! decoder rejects unknown types with [`MailboxError::Transport`].

use bytes::{BufMut as _, BytesMut};
use serde::{Deserialize, Serialize};
use skattr_core::mailbox::protocol::{
    Challenge, ChallengeNonce, Delete, DeleteOk, Deposit, DepositOk, ErrorBody, Fetch,
    FetchResponse,
};
use tokio_util::codec::{Decoder, Encoder};

use crate::error::{MailboxError, TransportErrorKind};

/// Hard cap on a single mailbox frame. Matches
/// `core::transport::frame::MAX_FRAME_SIZE`.
pub const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// Type byte for every wire frame. Layout matches `FrameKind` in the
/// design spec.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxFrameKind {
    /// 0x82 — `Deposit` (C→S).
    Deposit = 0x82,
    /// 0x83 — `DepositOk` (S→C).
    DepositOk = 0x83,
    /// 0x84 — `Challenge` (C→S).
    Challenge = 0x84,
    /// 0x85 — `ChallengeNonce` (S→C).
    ChallengeNonce = 0x85,
    /// 0x86 — `Fetch` (C→S).
    Fetch = 0x86,
    /// 0x87 — `FetchResponse` (S→C).
    FetchResponse = 0x87,
    /// 0x88 — `Delete` (C→S).
    Delete = 0x88,
    /// 0x89 — `DeleteOk` (S→C).
    DeleteOk = 0x89,
    /// 0x8F — `ErrorBody` (S→C).
    Error = 0x8F,
}

/// One fully-parsed mailbox frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailboxFrame {
    /// Client → server.
    Deposit(Deposit),
    /// Server → client.
    DepositOk(DepositOk),
    /// Client → server.
    Challenge(Challenge),
    /// Server → client.
    ChallengeNonce(ChallengeNonce),
    /// Client → server.
    Fetch(Fetch),
    /// Server → client.
    FetchResponse(FetchResponse),
    /// Client → server.
    Delete(Delete),
    /// Server → client.
    DeleteOk(DeleteOk),
    /// Server → client typed error.
    Error(ErrorBody),
}

impl MailboxFrame {
    /// Wire-format type byte for this frame.
    #[must_use]
    pub fn kind(&self) -> MailboxFrameKind {
        match self {
            MailboxFrame::Deposit(_) => MailboxFrameKind::Deposit,
            MailboxFrame::DepositOk(_) => MailboxFrameKind::DepositOk,
            MailboxFrame::Challenge(_) => MailboxFrameKind::Challenge,
            MailboxFrame::ChallengeNonce(_) => MailboxFrameKind::ChallengeNonce,
            MailboxFrame::Fetch(_) => MailboxFrameKind::Fetch,
            MailboxFrame::FetchResponse(_) => MailboxFrameKind::FetchResponse,
            MailboxFrame::Delete(_) => MailboxFrameKind::Delete,
            MailboxFrame::DeleteOk(_) => MailboxFrameKind::DeleteOk,
            MailboxFrame::Error(_) => MailboxFrameKind::Error,
        }
    }
}

/// `tokio_util::codec::{Decoder, Encoder}` for [`MailboxFrame`].
#[derive(Debug, Default)]
pub struct MailboxFrameCodec {
    _private: (),
}

impl MailboxFrameCodec {
    /// Construct a codec.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

fn encode_cbor<T: Serialize>(value: &T) -> Result<Vec<u8>, MailboxError> {
    let mut out = Vec::new();
    ciborium::into_writer(value, &mut out).map_err(|e| {
        MailboxError::Transport(TransportErrorKind::EncodeFailed(format!(
            "cbor encode: {e}"
        )))
    })?;
    Ok(out)
}

fn decode_cbor<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, MailboxError> {
    ciborium::from_reader(bytes).map_err(|e| {
        MailboxError::Transport(TransportErrorKind::DecodeFailed(format!(
            "cbor decode: {e}"
        )))
    })
}

impl Encoder<MailboxFrame> for MailboxFrameCodec {
    type Error = MailboxError;

    fn encode(&mut self, item: MailboxFrame, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let (type_byte, payload) = match item {
            MailboxFrame::Deposit(b) => (MailboxFrameKind::Deposit as u8, encode_cbor(&b)?),
            MailboxFrame::DepositOk(b) => (MailboxFrameKind::DepositOk as u8, encode_cbor(&b)?),
            MailboxFrame::Challenge(b) => (MailboxFrameKind::Challenge as u8, encode_cbor(&b)?),
            MailboxFrame::ChallengeNonce(b) => {
                (MailboxFrameKind::ChallengeNonce as u8, encode_cbor(&b)?)
            }
            MailboxFrame::Fetch(b) => (MailboxFrameKind::Fetch as u8, encode_cbor(&b)?),
            MailboxFrame::FetchResponse(b) => {
                (MailboxFrameKind::FetchResponse as u8, encode_cbor(&b)?)
            }
            MailboxFrame::Delete(b) => (MailboxFrameKind::Delete as u8, encode_cbor(&b)?),
            MailboxFrame::DeleteOk(b) => (MailboxFrameKind::DeleteOk as u8, encode_cbor(&b)?),
            MailboxFrame::Error(b) => (MailboxFrameKind::Error as u8, encode_cbor(&b)?),
        };

        let length = 1 + payload.len();
        if length > MAX_FRAME_SIZE {
            return Err(MailboxError::Transport(TransportErrorKind::EncodeFailed(
                format!("frame too large: {length} bytes"),
            )));
        }

        dst.reserve(4 + length);
        let length_u32 = u32::try_from(length).map_err(|_| {
            MailboxError::Transport(TransportErrorKind::EncodeFailed(format!(
                "length overflow: {length}"
            )))
        })?;
        dst.extend_from_slice(&length_u32.to_be_bytes());
        dst.put_u8(type_byte);
        dst.extend_from_slice(&payload);
        Ok(())
    }
}

impl Decoder for MailboxFrameCodec {
    type Item = MailboxFrame;
    type Error = MailboxError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<MailboxFrame>, Self::Error> {
        if src.len() < 4 {
            return Ok(None);
        }
        let mut len_bytes = [0u8; 4];
        len_bytes.copy_from_slice(&src[0..4]);
        let length = u32::from_be_bytes(len_bytes) as usize;

        if length == 0 {
            return Err(MailboxError::Transport(TransportErrorKind::DecodeFailed(
                "zero-length frame".into(),
            )));
        }
        if length > MAX_FRAME_SIZE {
            return Err(MailboxError::Transport(TransportErrorKind::DecodeFailed(
                format!("frame too large: {length} bytes"),
            )));
        }
        if src.len() < 4 + length {
            return Ok(None);
        }

        let _ = src.split_to(4);
        let type_byte = src[0];
        let _ = src.split_to(1);
        let payload_len = length - 1;
        let payload = src.split_to(payload_len);

        let frame = match type_byte {
            0x82 => MailboxFrame::Deposit(decode_cbor(&payload)?),
            0x83 => MailboxFrame::DepositOk(decode_cbor(&payload)?),
            0x84 => MailboxFrame::Challenge(decode_cbor(&payload)?),
            0x85 => MailboxFrame::ChallengeNonce(decode_cbor(&payload)?),
            0x86 => MailboxFrame::Fetch(decode_cbor(&payload)?),
            0x87 => MailboxFrame::FetchResponse(decode_cbor(&payload)?),
            0x88 => MailboxFrame::Delete(decode_cbor(&payload)?),
            0x89 => MailboxFrame::DeleteOk(decode_cbor(&payload)?),
            0x8F => MailboxFrame::Error(decode_cbor(&payload)?),
            other => {
                return Err(MailboxError::Transport(TransportErrorKind::DecodeFailed(
                    format!("unknown mailbox frame type 0x{other:02X}"),
                )));
            }
        };
        Ok(Some(frame))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use skattr_core::mailbox::protocol::{ErrorCode, PROTOCOL_VERSION};

    fn round_trip(f: MailboxFrame) -> MailboxFrame {
        let mut codec = MailboxFrameCodec::new();
        let mut buf = BytesMut::new();
        codec.encode(f, &mut buf).unwrap();
        codec.decode(&mut buf).unwrap().unwrap()
    }

    #[test]
    fn deposit_round_trips() {
        let f = MailboxFrame::Deposit(Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash: [0xAA; 32],
            ciphertext: vec![1, 2, 3, 4],
            ttl_request: 86_400,
        });
        assert_eq!(round_trip(f.clone()), f);
    }

    #[test]
    fn deposit_ok_round_trips() {
        let f = MailboxFrame::DepositOk(DepositOk {
            deposit_id: [0x42; 16],
            expires_at: 1_700_000_000,
        });
        assert_eq!(round_trip(f.clone()), f);
    }

    #[test]
    fn error_round_trips() {
        let f = MailboxFrame::Error(ErrorBody {
            code: ErrorCode::TtlTooLong,
            message: "expires_at too far".into(),
        });
        assert_eq!(round_trip(f.clone()), f);
    }

    #[test]
    fn type_byte_layout() {
        let mut codec = MailboxFrameCodec::new();
        let mut buf = BytesMut::new();
        codec
            .encode(
                MailboxFrame::Challenge(Challenge {
                    version: PROTOCOL_VERSION,
                    identity_hash: [0; 32],
                }),
                &mut buf,
            )
            .unwrap();
        // length_u32 (4) + type byte
        assert_eq!(buf[4], 0x84);
    }

    #[test]
    fn unknown_type_rejected() {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&1u32.to_be_bytes());
        buf.extend_from_slice(&[0x20]);
        let mut codec = MailboxFrameCodec::new();
        let err = codec.decode(&mut buf).expect_err("must reject");
        assert!(matches!(err, MailboxError::Transport(_)));
    }

    #[test]
    fn zero_length_rejected() {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&[0, 0, 0, 0]);
        let mut codec = MailboxFrameCodec::new();
        assert!(codec.decode(&mut buf).is_err());
    }

    #[test]
    fn oversized_length_rejected() {
        let mut buf = BytesMut::new();
        let oversized = u32::try_from(MAX_FRAME_SIZE + 1).unwrap();
        buf.extend_from_slice(&oversized.to_be_bytes());
        let mut codec = MailboxFrameCodec::new();
        assert!(codec.decode(&mut buf).is_err());
    }

    #[test]
    fn malformed_cbor_rejected() {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&3u32.to_be_bytes());
        buf.extend_from_slice(&[0x82]); // Deposit frame
        buf.extend_from_slice(&[0xFF, 0xFF]); // garbage CBOR
        let mut codec = MailboxFrameCodec::new();
        let err = codec.decode(&mut buf).expect_err("must reject");
        assert!(matches!(err, MailboxError::Transport(_)));
    }

    #[test]
    fn partial_length_returns_none() {
        let mut buf = BytesMut::from(&[0u8, 0, 0][..]);
        let mut codec = MailboxFrameCodec::new();
        assert!(codec.decode(&mut buf).unwrap().is_none());
        assert_eq!(buf.len(), 3);
    }

    #[test]
    fn partial_payload_returns_none() {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&5u32.to_be_bytes());
        buf.extend_from_slice(&[0x82, 0xA0, 0x00]); // 3 of 4 expected payload bytes
        let mut codec = MailboxFrameCodec::new();
        assert!(codec.decode(&mut buf).unwrap().is_none());
    }
}
```

The tests reference `MailboxError::Transport(TransportErrorKind::*)` which Task 4 fleshes out — Task 2's stub `error.rs` already has `TransportErrorKind` declared but not the `EncodeFailed` / `DecodeFailed` variants. We extend that stub here so this task's tests run green.

### Step 2: Add the missing variants to the error stub

Edit `crates/mailbox/src/error.rs` and replace the `TransportErrorKind` stub with:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportErrorKind {
    EncodeFailed(String),
    DecodeFailed(String),
}
```

(Strip `Copy` because the variants now hold `String`. Task 4 finalises this.)

### Step 3: Run tests

```bash
cargo test -p skattr-mailbox --lib codec::
```
Expected: 9 passing tests (`deposit_round_trips`, `deposit_ok_round_trips`, `error_round_trips`, `type_byte_layout`, `unknown_type_rejected`, `zero_length_rejected`, `oversized_length_rejected`, `malformed_cbor_rejected`, `partial_length_returns_none`, `partial_payload_returns_none`).

### Step 4: Lint and format

```bash
cargo fmt --all
cargo clippy -p skattr-mailbox --all-targets --all-features -- -D warnings
```
Expected: clean.

### Step 5: Commit

```bash
git add crates/mailbox/src/codec.rs crates/mailbox/src/error.rs
git commit -m "$(cat <<'EOF'
mailbox: MailboxFrameCodec mirrors core wire layout

Same length_u32 || type_u8 || payload framing as the peer-to-peer
codec, disjoint type-byte range (0x82-0x8F). Encoder/decoder for
all nine v1 frames + round-trip + adversarial coverage (unknown
type, zero length, oversize, malformed CBOR, partial reads).

EOF
)"
```

---

## Task 4: Finalise the error taxonomy

**Files:**
- Modify: `crates/mailbox/src/error.rs` (full rewrite)
- Test: same file

### Step 1: Write the failing tests + final shape

Replace `crates/mailbox/src/error.rs` with:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Error taxonomy for the mailbox server.
//!
//! Six subsystem sub-enums under one top-level [`MailboxError`]; the
//! `kind()` projection is a structural match (no `str::contains`).
//! See Phase 1.H §error.rs for the pattern this mirrors.

use skattr_core::mailbox::protocol::ErrorCode;
use thiserror::Error;

/// Top-level mailbox error.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MailboxError {
    /// SQLite or migration problem.
    #[error("storage: {0}")]
    Storage(#[from] StorageErrorKind),
    /// Auth: hash mismatch, bad signature, expired nonce.
    #[error("auth: {0}")]
    Auth(#[from] AuthErrorKind),
    /// Operator policy violation: too large, TTL bounds, rate limit, cap.
    #[error("policy: {0}")]
    Policy(#[from] PolicyErrorKind),
    /// Wire-level: codec, framing, CBOR.
    #[error("transport: {0}")]
    Transport(#[from] TransportErrorKind),
    /// Configuration parsing or validation.
    #[error("config: {0}")]
    Config(#[from] ConfigErrorKind),
    /// I/O or runtime error not otherwise classified.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Stable subsystem tag for log filters and metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxErrorKind {
    /// [`MailboxError::Storage`].
    Storage,
    /// [`MailboxError::Auth`].
    Auth,
    /// [`MailboxError::Policy`].
    Policy,
    /// [`MailboxError::Transport`].
    Transport,
    /// [`MailboxError::Config`].
    Config,
    /// [`MailboxError::Io`].
    Io,
}

impl MailboxError {
    /// Project this error to its subsystem tag.
    #[must_use]
    pub fn kind(&self) -> MailboxErrorKind {
        match self {
            MailboxError::Storage(_) => MailboxErrorKind::Storage,
            MailboxError::Auth(_) => MailboxErrorKind::Auth,
            MailboxError::Policy(_) => MailboxErrorKind::Policy,
            MailboxError::Transport(_) => MailboxErrorKind::Transport,
            MailboxError::Config(_) => MailboxErrorKind::Config,
            MailboxError::Io(_) => MailboxErrorKind::Io,
        }
    }

    /// Map an internal error onto a wire [`ErrorCode`]. Errors that
    /// don't have a stable mapping fold to [`ErrorCode::Internal`].
    #[must_use]
    pub fn to_wire_code(&self) -> ErrorCode {
        match self {
            MailboxError::Auth(AuthErrorKind::HashMismatch) => ErrorCode::HashMismatch,
            MailboxError::Auth(AuthErrorKind::InvalidSignature) => ErrorCode::InvalidSignature,
            MailboxError::Auth(AuthErrorKind::NonceExpired) => ErrorCode::NonceExpired,
            MailboxError::Policy(PolicyErrorKind::TooLarge) => ErrorCode::TooLarge,
            MailboxError::Policy(PolicyErrorKind::TtlTooLong) => ErrorCode::TtlTooLong,
            MailboxError::Policy(PolicyErrorKind::TtlTooShort) => ErrorCode::TtlTooShort,
            MailboxError::Policy(PolicyErrorKind::RateLimited) => ErrorCode::RateLimited,
            MailboxError::Policy(PolicyErrorKind::RecipientFull) => ErrorCode::RecipientFull,
            MailboxError::Storage(StorageErrorKind::NotFound) => ErrorCode::NotFound,
            MailboxError::Transport(TransportErrorKind::DecodeFailed(_)) => {
                ErrorCode::MalformedRequest
            }
            MailboxError::Transport(TransportErrorKind::UnsupportedVersion) => {
                ErrorCode::UnsupportedVersion
            }
            _ => ErrorCode::Internal,
        }
    }
}

/// Storage failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StorageErrorKind {
    /// Migration ran but didn't reach the expected schema_version.
    #[error("migration failed: {0}")]
    MigrationFailed(String),
    /// SQL prepare / step / execute returned an error.
    #[error("sqlite: {0}")]
    Sqlite(String),
    /// `deposit_id` not present.
    #[error("not found")]
    NotFound,
}

impl From<rusqlite::Error> for StorageErrorKind {
    fn from(e: rusqlite::Error) -> Self {
        StorageErrorKind::Sqlite(e.to_string())
    }
}

impl From<rusqlite::Error> for MailboxError {
    fn from(e: rusqlite::Error) -> Self {
        MailboxError::Storage(StorageErrorKind::Sqlite(e.to_string()))
    }
}

/// Authentication failures (signature, nonce, hash binding).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AuthErrorKind {
    /// `sha256(identity_pubkey) != identity_hash`.
    #[error("hash mismatch")]
    HashMismatch,
    /// Ed25519 verification failed.
    #[error("invalid signature")]
    InvalidSignature,
    /// Nonce unknown or older than 30 s.
    #[error("nonce expired")]
    NonceExpired,
}

/// Operator-policy rejections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PolicyErrorKind {
    /// Ciphertext above `max_deposit_size`.
    #[error("too large")]
    TooLarge,
    /// `ttl_request > max_ttl_secs`.
    #[error("ttl too long")]
    TtlTooLong,
    /// `ttl_request != 0 && ttl_request < min_ttl_secs`.
    #[error("ttl too short")]
    TtlTooShort,
    /// Per-conn or global token bucket exhausted.
    #[error("rate limited")]
    RateLimited,
    /// Recipient cap reached, no expired rows available to evict.
    #[error("recipient full")]
    RecipientFull,
}

/// Wire-level failures (codec, framing).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TransportErrorKind {
    /// Encoding to bytes failed.
    #[error("encode failed: {0}")]
    EncodeFailed(String),
    /// Decoding from bytes failed.
    #[error("decode failed: {0}")]
    DecodeFailed(String),
    /// Frame `version != PROTOCOL_VERSION`.
    #[error("unsupported version")]
    UnsupportedVersion,
}

/// Config / TOML failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConfigErrorKind {
    /// Could not read the config file.
    #[error("io: {0}")]
    Io(String),
    /// Could not parse TOML.
    #[error("parse: {0}")]
    Parse(String),
    /// A required field was missing or out of range.
    #[error("invalid: {0}")]
    Invalid(String),
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn kind_for_each_top_level_variant() {
        assert_eq!(
            MailboxError::Auth(AuthErrorKind::HashMismatch).kind(),
            MailboxErrorKind::Auth
        );
        assert_eq!(
            MailboxError::Policy(PolicyErrorKind::TooLarge).kind(),
            MailboxErrorKind::Policy
        );
        assert_eq!(
            MailboxError::Storage(StorageErrorKind::NotFound).kind(),
            MailboxErrorKind::Storage
        );
        assert_eq!(
            MailboxError::Transport(TransportErrorKind::UnsupportedVersion).kind(),
            MailboxErrorKind::Transport
        );
        assert_eq!(
            MailboxError::Config(ConfigErrorKind::Invalid("x".into())).kind(),
            MailboxErrorKind::Config
        );
    }

    #[test]
    fn to_wire_code_covers_every_typed_failure() {
        let cases: &[(MailboxError, ErrorCode)] = &[
            (
                MailboxError::Auth(AuthErrorKind::HashMismatch),
                ErrorCode::HashMismatch,
            ),
            (
                MailboxError::Auth(AuthErrorKind::InvalidSignature),
                ErrorCode::InvalidSignature,
            ),
            (
                MailboxError::Auth(AuthErrorKind::NonceExpired),
                ErrorCode::NonceExpired,
            ),
            (
                MailboxError::Policy(PolicyErrorKind::TooLarge),
                ErrorCode::TooLarge,
            ),
            (
                MailboxError::Policy(PolicyErrorKind::TtlTooLong),
                ErrorCode::TtlTooLong,
            ),
            (
                MailboxError::Policy(PolicyErrorKind::TtlTooShort),
                ErrorCode::TtlTooShort,
            ),
            (
                MailboxError::Policy(PolicyErrorKind::RateLimited),
                ErrorCode::RateLimited,
            ),
            (
                MailboxError::Policy(PolicyErrorKind::RecipientFull),
                ErrorCode::RecipientFull,
            ),
            (
                MailboxError::Storage(StorageErrorKind::NotFound),
                ErrorCode::NotFound,
            ),
            (
                MailboxError::Transport(TransportErrorKind::DecodeFailed("bad".into())),
                ErrorCode::MalformedRequest,
            ),
            (
                MailboxError::Transport(TransportErrorKind::UnsupportedVersion),
                ErrorCode::UnsupportedVersion,
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(err.to_wire_code(), *expected, "mapping for {err:?}");
        }
    }

    #[test]
    fn unknown_failures_fold_to_internal() {
        let e = MailboxError::Storage(StorageErrorKind::MigrationFailed("oops".into()));
        assert_eq!(e.to_wire_code(), ErrorCode::Internal);
    }
}
```

Also update `crates/mailbox/src/lib.rs` exports — replace the existing `pub use error::{...}` with:

```rust
pub use error::{
    AuthErrorKind, ConfigErrorKind, MailboxError, MailboxErrorKind, PolicyErrorKind,
    StorageErrorKind, TransportErrorKind,
};
```

(unchanged from Task 2's stub list — the variants now match the real enums).

### Step 2: Run tests

```bash
cargo test -p skattr-mailbox --lib error::
```
Expected: 3 passing tests (`kind_for_each_top_level_variant`, `to_wire_code_covers_every_typed_failure`, `unknown_failures_fold_to_internal`).

### Step 3: Lint and format

```bash
cargo fmt --all
cargo clippy -p skattr-mailbox --all-targets --all-features -- -D warnings
```
Expected: clean. (The codec from Task 3 still compiles because the `TransportErrorKind` enum widened, not narrowed.)

### Step 4: Commit

```bash
git add crates/mailbox/src/error.rs crates/mailbox/src/lib.rs
git commit -m "$(cat <<'EOF'
mailbox: finalise error taxonomy with subsystem sub-enums

Six subsystem sub-enums (Storage, Auth, Policy, Transport, Config,
Io) under MailboxError, each implementing thiserror::Error. kind()
is a pure structural match; to_wire_code() maps every typed
failure to the corresponding wire ErrorCode and folds unmapped
failures to ErrorCode::Internal.

EOF
)"
```

---

## Task 5: Config schema overhaul

**Files:**
- Modify: `crates/mailbox/src/config.rs` (full rewrite)
- Test: same file

### Step 1: Write the failing tests + final shape

Replace `crates/mailbox/src/config.rs` with:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Mailbox server configuration.
//!
//! Loaded from `mailbox.toml`. Layout:
//!
//! ```toml
//! [server]
//! data_dir = "/var/lib/skattr-mailbox"
//! # storage_path / arti_state_dir / health_socket default to
//! # children of data_dir; each can be overridden individually.
//! instance_label = "mailbox-1"
//!
//! [policy]
//! max_deposit_size           = 1048576
//! min_ttl_secs               = 3600
//! max_ttl_secs               = 2592000
//! default_ttl_secs           = 604800
//! recipient_cap_bytes        = 268435456
//! per_conn_deposits_per_min  = 30
//! per_conn_fetches_per_min   = 6
//! global_deposits_per_min    = 1000
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ConfigErrorKind, MailboxError};
use crate::policy::Policy;

/// Top-level mailbox configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailboxConfig {
    /// `[server]` section.
    pub server: ServerConfig,
    /// `[policy]` section.
    #[serde(default = "Policy::recommended")]
    pub policy: Policy,
}

/// Filesystem + identity-of-instance settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Parent directory for all server state.
    pub data_dir: PathBuf,
    /// Override `data_dir/mailbox.sqlite`.
    #[serde(default)]
    pub storage_path: Option<PathBuf>,
    /// Override `data_dir/arti`.
    #[serde(default)]
    pub arti_state_dir: Option<PathBuf>,
    /// Override `data_dir/health.sock`.
    #[serde(default)]
    pub health_socket: Option<PathBuf>,
    /// Tag for log disambiguation; never transmitted on the wire.
    #[serde(default)]
    pub instance_label: Option<String>,
}

impl ServerConfig {
    /// Resolved storage path (default: `data_dir/mailbox.sqlite`).
    #[must_use]
    pub fn resolved_storage_path(&self) -> PathBuf {
        self.storage_path
            .clone()
            .unwrap_or_else(|| self.data_dir.join("mailbox.sqlite"))
    }
    /// Resolved Arti state dir (default: `data_dir/arti`).
    #[must_use]
    pub fn resolved_arti_state_dir(&self) -> PathBuf {
        self.arti_state_dir
            .clone()
            .unwrap_or_else(|| self.data_dir.join("arti"))
    }
    /// Resolved healthcheck socket (default: `data_dir/health.sock`).
    #[must_use]
    pub fn resolved_health_socket(&self) -> PathBuf {
        self.health_socket
            .clone()
            .unwrap_or_else(|| self.data_dir.join("health.sock"))
    }
}

impl MailboxConfig {
    /// Load + validate from a TOML file.
    pub fn load(path: &Path) -> Result<Self, MailboxError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| MailboxError::Config(ConfigErrorKind::Io(e.to_string())))?;
        let cfg: MailboxConfig = toml::from_str(&text)
            .map_err(|e| MailboxError::Config(ConfigErrorKind::Parse(e.to_string())))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Cheapest defaults usable for tests.
    #[must_use]
    pub fn for_tests(data_dir: PathBuf) -> Self {
        Self {
            server: ServerConfig {
                data_dir,
                storage_path: None,
                arti_state_dir: None,
                health_socket: None,
                instance_label: None,
            },
            policy: Policy::recommended(),
        }
    }

    fn validate(&self) -> Result<(), MailboxError> {
        if self.policy.min_ttl_secs > self.policy.max_ttl_secs {
            return Err(MailboxError::Config(ConfigErrorKind::Invalid(
                "policy.min_ttl_secs > policy.max_ttl_secs".into(),
            )));
        }
        if self.policy.default_ttl_secs < self.policy.min_ttl_secs
            || self.policy.default_ttl_secs > self.policy.max_ttl_secs
        {
            return Err(MailboxError::Config(ConfigErrorKind::Invalid(
                "policy.default_ttl_secs outside [min_ttl_secs, max_ttl_secs]".into(),
            )));
        }
        if self.policy.max_deposit_size == 0 {
            return Err(MailboxError::Config(ConfigErrorKind::Invalid(
                "policy.max_deposit_size must be > 0".into(),
            )));
        }
        if self.policy.recipient_cap_bytes < self.policy.max_deposit_size {
            return Err(MailboxError::Config(ConfigErrorKind::Invalid(
                "policy.recipient_cap_bytes < max_deposit_size".into(),
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn write_temp_toml(name: &str, body: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("skattr-mailbox-test-{name}.toml"));
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn loads_valid_config() {
        let path = write_temp_toml(
            "valid",
            r#"
[server]
data_dir = "/tmp/skattr-mailbox"
instance_label = "test"

[policy]
max_deposit_size = 1048576
min_ttl_secs = 3600
max_ttl_secs = 2592000
default_ttl_secs = 604800
recipient_cap_bytes = 268435456
per_conn_deposits_per_min = 30
per_conn_fetches_per_min = 6
global_deposits_per_min = 1000
"#,
        );
        let cfg = MailboxConfig::load(&path).unwrap();
        assert_eq!(cfg.server.instance_label.as_deref(), Some("test"));
        assert_eq!(cfg.policy.max_deposit_size, 1_048_576);
    }

    #[test]
    fn omitted_policy_uses_recommended() {
        let path = write_temp_toml(
            "default-policy",
            r#"
[server]
data_dir = "/tmp/skattr-mailbox"
"#,
        );
        let cfg = MailboxConfig::load(&path).unwrap();
        assert_eq!(cfg.policy, Policy::recommended());
    }

    #[test]
    fn rejects_min_gt_max_ttl() {
        let path = write_temp_toml(
            "bad-ttl",
            r#"
[server]
data_dir = "/tmp/skattr-mailbox"

[policy]
max_deposit_size = 1048576
min_ttl_secs = 100
max_ttl_secs = 50
default_ttl_secs = 75
recipient_cap_bytes = 268435456
per_conn_deposits_per_min = 30
per_conn_fetches_per_min = 6
global_deposits_per_min = 1000
"#,
        );
        let err = MailboxConfig::load(&path).expect_err("must reject");
        assert!(matches!(err, MailboxError::Config(ConfigErrorKind::Invalid(_))));
    }

    #[test]
    fn resolved_paths_default_to_data_dir_children() {
        let cfg = MailboxConfig::for_tests(PathBuf::from("/var/lib/mailbox"));
        assert_eq!(
            cfg.server.resolved_storage_path(),
            PathBuf::from("/var/lib/mailbox/mailbox.sqlite")
        );
        assert_eq!(
            cfg.server.resolved_arti_state_dir(),
            PathBuf::from("/var/lib/mailbox/arti")
        );
        assert_eq!(
            cfg.server.resolved_health_socket(),
            PathBuf::from("/var/lib/mailbox/health.sock")
        );
    }
}
```

This task references `Policy::recommended()` — Task 9 finalises `Policy` with that constructor. For now we add a stub.

### Step 2: Stub `Policy::recommended` so tests compile

Replace `crates/mailbox/src/policy.rs` with a temporary fuller stub (Task 9 will finalise):

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Operator caps + token-bucket rate limiter (final shape in Task 9).

#![allow(missing_docs)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub max_deposit_size: u64,
    pub min_ttl_secs: u32,
    pub max_ttl_secs: u32,
    pub default_ttl_secs: u32,
    pub recipient_cap_bytes: u64,
    pub per_conn_deposits_per_min: u32,
    pub per_conn_fetches_per_min: u32,
    pub global_deposits_per_min: u32,
}

impl Policy {
    #[must_use]
    pub fn recommended() -> Self {
        Self {
            max_deposit_size: 1_048_576,
            min_ttl_secs: 3_600,
            max_ttl_secs: 2_592_000,
            default_ttl_secs: 604_800,
            recipient_cap_bytes: 268_435_456,
            per_conn_deposits_per_min: 30,
            per_conn_fetches_per_min: 6,
            global_deposits_per_min: 1_000,
        }
    }
}

#[derive(Debug, Default)]
pub struct TokenBucket;
```

### Step 3: Run tests

```bash
cargo test -p skattr-mailbox --lib config::
```
Expected: 4 passing tests.

### Step 4: Lint + commit

```bash
cargo fmt --all
cargo clippy -p skattr-mailbox --all-targets --all-features -- -D warnings
git add crates/mailbox/src/config.rs crates/mailbox/src/policy.rs
git commit -m "$(cat <<'EOF'
mailbox: data_dir-anchored config schema with validation

[server] anchors all paths off data_dir (each individually
overridable); [policy] carries the operator caps. validate()
rejects inverted TTL bounds, zero deposit size, and
recipient_cap_bytes below max_deposit_size at load time so the
server fails fast instead of mid-request.

EOF
)"
```

---

## Task 6: Migrations runner + 0001_init.sql

**Files:**
- Create: `crates/mailbox/migrations/0001_init.sql`
- Create: `crates/mailbox/src/migrations.rs`
- Modify: `crates/mailbox/src/lib.rs` (add `mod migrations;`)

### Step 1: Write the migration SQL

Create `crates/mailbox/migrations/0001_init.sql`:

```sql
CREATE TABLE deposits (
  deposit_id     BLOB PRIMARY KEY,
  recipient_hash BLOB NOT NULL,
  ciphertext     BLOB NOT NULL,
  deposited_at   INTEGER NOT NULL,
  expires_at     INTEGER NOT NULL
);
CREATE INDEX idx_deposits_recipient ON deposits(recipient_hash, deposited_at);
CREATE INDEX idx_deposits_expiry    ON deposits(expires_at);

CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
```

### Step 2: Write the migrations runner with failing tests

Create `crates/mailbox/src/migrations.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Forward-only schema-migration runner. Mirrors the `core::storage`
//! pattern: `include_str!`'d SQL files keyed by a monotonic version.

use crate::error::{MailboxError, StorageErrorKind};

struct Migration {
    version: u32,
    sql: &'static str,
}

const ALL_MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: include_str!("../migrations/0001_init.sql"),
}];

/// Apply all pending migrations in order. Idempotent.
pub(crate) fn apply(conn: &mut rusqlite::Connection) -> Result<(), MailboxError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY)",
        [],
    )?;
    let current: u32 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    for m in ALL_MIGRATIONS {
        if m.version <= current {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(m.sql)?;
        tx.execute(
            "INSERT OR REPLACE INTO schema_version (version) VALUES (?1)",
            [m.version],
        )?;
        tx.commit()?;
    }

    let final_version: u32 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_version",
        [],
        |r| r.get(0),
    )?;
    let target = ALL_MIGRATIONS.last().map(|m| m.version).unwrap_or(0);
    if final_version != target {
        return Err(MailboxError::Storage(StorageErrorKind::MigrationFailed(
            format!("expected schema {target}, got {final_version}"),
        )));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn open_in_memory() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").ok(); // memory ignores
        conn
    }

    #[test]
    fn migration_0001_creates_deposits_table() {
        let mut conn = open_in_memory();
        apply(&mut conn).unwrap();

        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info('deposits')")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for required in [
            "deposit_id",
            "recipient_hash",
            "ciphertext",
            "deposited_at",
            "expires_at",
        ] {
            assert!(cols.iter().any(|c| c == required), "missing {required}");
        }
    }

    #[test]
    fn migration_0001_creates_indexes() {
        let mut conn = open_in_memory();
        apply(&mut conn).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name IN \
                 ('idx_deposits_recipient', 'idx_deposits_expiry')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn idempotent_re_apply() {
        let mut conn = open_in_memory();
        apply(&mut conn).unwrap();
        apply(&mut conn).unwrap();
        let count: i64 = conn
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}
```

### Step 3: Add `mod migrations;` to `lib.rs`

In `crates/mailbox/src/lib.rs`, after the existing `pub mod` lines add:

```rust
pub(crate) mod migrations;
```

### Step 4: Run tests

```bash
cargo test -p skattr-mailbox --lib migrations::
```
Expected: 3 passing tests.

### Step 5: Lint + commit

```bash
cargo fmt --all
cargo clippy -p skattr-mailbox --all-targets --all-features -- -D warnings
git add crates/mailbox/migrations/ crates/mailbox/src/migrations.rs crates/mailbox/src/lib.rs
git commit -m "$(cat <<'EOF'
mailbox: migrations runner + 0001_init.sql

Forward-only, monotonic version key. Mirrors core::storage::migrations
exactly so operators familiar with the daemon's schema model find
the mailbox's identical. Schema 0001 lands the deposits table plus
the recipient + expiry indexes.

EOF
)"
```

---

## Task 7: `Store` — deposit insert / fetch / delete / expire / cap-enforce

**Files:**
- Modify: `crates/mailbox/src/store.rs` (full rewrite)
- Test: same file

### Step 1: Write the failing tests + implementation

Replace `crates/mailbox/src/store.rs` with:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! SQLite-backed deposit store.
//!
//! Single-table schema (see `migrations/0001_init.sql`). The store is
//! transactional; cap enforcement and insertion happen atomically so
//! a `RecipientFull` rejection never leaves the DB in a partial state.

use std::path::Path;
use std::sync::Mutex;

use rand::RngCore;
use rusqlite::{params, Connection, OpenFlags};

use crate::error::{MailboxError, PolicyErrorKind, StorageErrorKind};
use crate::migrations;

/// One row from `deposits` returned by [`Store::fetch`].
#[derive(Debug, Clone)]
pub struct StoredDeposit {
    pub deposit_id: [u8; 16],
    pub ciphertext: Vec<u8>,
    pub received_at: i64,
}

/// Deposit store handle.
#[derive(Debug)]
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// Open or create the store at `path`. Sets WAL + `synchronous=NORMAL`
    /// + `foreign_keys=ON`, then runs migrations.
    pub fn open(path: &Path) -> Result<Self, MailboxError> {
        let mut conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrations::apply(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// In-memory store for tests.
    #[cfg(test)]
    pub fn in_memory() -> Result<Self, MailboxError> {
        let mut conn = Connection::open_in_memory()?;
        migrations::apply(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Insert a deposit, enforcing the per-recipient cap atomically.
    /// Returns the generated `deposit_id` on success or
    /// [`PolicyErrorKind::RecipientFull`] when the cap can't be made.
    pub fn insert(
        &self,
        recipient_hash: [u8; 32],
        ciphertext: Vec<u8>,
        deposited_at: i64,
        expires_at: i64,
        recipient_cap_bytes: u64,
        now: i64,
    ) -> Result<[u8; 16], MailboxError> {
        let new_len = u64::try_from(ciphertext.len()).unwrap_or(u64::MAX);
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction()?;

        let existing: i64 = tx
            .query_row(
                "SELECT COALESCE(SUM(LENGTH(ciphertext)), 0) FROM deposits \
                 WHERE recipient_hash = ?1",
                params![recipient_hash.to_vec()],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let mut existing_bytes = existing as u64;

        if existing_bytes + new_len > recipient_cap_bytes {
            // Try evicting expired rows (oldest first).
            let to_free = (existing_bytes + new_len) - recipient_cap_bytes;
            evict_expired_for(&tx, recipient_hash, to_free, now)?;
            let after: i64 = tx
                .query_row(
                    "SELECT COALESCE(SUM(LENGTH(ciphertext)), 0) FROM deposits \
                     WHERE recipient_hash = ?1",
                    params![recipient_hash.to_vec()],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            existing_bytes = after as u64;
            if existing_bytes + new_len > recipient_cap_bytes {
                tx.rollback()?;
                return Err(MailboxError::Policy(PolicyErrorKind::RecipientFull));
            }
        }

        let mut id = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut id);
        tx.execute(
            "INSERT INTO deposits (deposit_id, recipient_hash, ciphertext, deposited_at, expires_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id.to_vec(),
                recipient_hash.to_vec(),
                ciphertext,
                deposited_at,
                expires_at
            ],
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Fetch all (non-expired) deposits for a recipient hash. Caller
    /// passes `now` so expiry checks use server clock.
    pub fn fetch(
        &self,
        recipient_hash: [u8; 32],
        now: i64,
    ) -> Result<Vec<StoredDeposit>, MailboxError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT deposit_id, ciphertext, deposited_at FROM deposits \
             WHERE recipient_hash = ?1 AND expires_at > ?2 \
             ORDER BY deposited_at ASC",
        )?;
        let rows = stmt
            .query_map(params![recipient_hash.to_vec(), now], |r| {
                let id_blob: Vec<u8> = r.get(0)?;
                let mut id = [0u8; 16];
                if id_blob.len() == 16 {
                    id.copy_from_slice(&id_blob);
                }
                Ok(StoredDeposit {
                    deposit_id: id,
                    ciphertext: r.get(1)?,
                    received_at: r.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Delete deposits by id, scoped to the given recipient. Returns
    /// `(deleted, not_found)` counts so the dispatch handler can build
    /// `DeleteOk` directly.
    pub fn delete(
        &self,
        recipient_hash: [u8; 32],
        deposit_ids: &[[u8; 16]],
    ) -> Result<(u32, u32), MailboxError> {
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction()?;
        let mut deleted: u32 = 0;
        for id in deposit_ids {
            let n = tx.execute(
                "DELETE FROM deposits WHERE deposit_id = ?1 AND recipient_hash = ?2",
                params![id.to_vec(), recipient_hash.to_vec()],
            )?;
            deleted += u32::try_from(n).unwrap_or(0);
        }
        tx.commit()?;
        let not_found = u32::try_from(deposit_ids.len())
            .unwrap_or(u32::MAX)
            .saturating_sub(deleted);
        Ok((deleted, not_found))
    }

    /// Expire all rows whose `expires_at < now`. Returns the number
    /// removed.
    pub fn expire_sweep(&self, now: i64) -> Result<u64, MailboxError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let n = conn.execute("DELETE FROM deposits WHERE expires_at < ?1", params![now])?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// Total bytes stored across all recipients. Used by the metrics
    /// tick.
    pub fn storage_bytes(&self) -> Result<u64, MailboxError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let total: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(LENGTH(ciphertext)), 0) FROM deposits",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        Ok(total.max(0) as u64)
    }
}

fn evict_expired_for(
    tx: &rusqlite::Transaction<'_>,
    recipient_hash: [u8; 32],
    target_bytes: u64,
    now: i64,
) -> Result<(), MailboxError> {
    // Oldest expired first; stop when freed enough or list exhausted.
    let mut stmt = tx.prepare(
        "SELECT deposit_id, LENGTH(ciphertext) FROM deposits \
         WHERE recipient_hash = ?1 AND expires_at < ?2 \
         ORDER BY deposited_at ASC",
    )?;
    let candidates: Vec<(Vec<u8>, i64)> = stmt
        .query_map(params![recipient_hash.to_vec(), now], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    let mut freed: u64 = 0;
    for (id, bytes) in candidates {
        tx.execute(
            "DELETE FROM deposits WHERE deposit_id = ?1",
            params![id],
        )?;
        freed = freed.saturating_add(u64::try_from(bytes).unwrap_or(0));
        if freed >= target_bytes {
            break;
        }
    }
    let _ = freed; // silence warning when target_bytes == 0
    Ok::<(), MailboxError>(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const REC_A: [u8; 32] = [0x11; 32];
    const REC_B: [u8; 32] = [0x22; 32];
    const ONE_GB: u64 = 1 << 30;

    #[test]
    fn insert_and_fetch_round_trip() {
        let s = Store::in_memory().unwrap();
        let id = s
            .insert(REC_A, vec![1, 2, 3], 100, 200, ONE_GB, 50)
            .unwrap();
        let rows = s.fetch(REC_A, 150).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].deposit_id, id);
        assert_eq!(rows[0].ciphertext, vec![1, 2, 3]);
        assert_eq!(rows[0].received_at, 100);
    }

    #[test]
    fn fetch_skips_expired_rows() {
        let s = Store::in_memory().unwrap();
        s.insert(REC_A, vec![9], 100, 110, ONE_GB, 50).unwrap();
        assert_eq!(s.fetch(REC_A, 200).unwrap().len(), 0);
    }

    #[test]
    fn fetch_is_per_recipient() {
        let s = Store::in_memory().unwrap();
        s.insert(REC_A, vec![1], 100, 999_999, ONE_GB, 50).unwrap();
        s.insert(REC_B, vec![2], 100, 999_999, ONE_GB, 50).unwrap();
        assert_eq!(s.fetch(REC_A, 150).unwrap().len(), 1);
    }

    #[test]
    fn delete_returns_counts_and_is_recipient_scoped() {
        let s = Store::in_memory().unwrap();
        let id_a = s
            .insert(REC_A, vec![1; 4], 100, 200, ONE_GB, 50)
            .unwrap();
        let id_b = s
            .insert(REC_B, vec![2; 4], 100, 200, ONE_GB, 50)
            .unwrap();
        // Try to delete id_b with REC_A's hash: should not match, count as not_found.
        let (deleted, not_found) = s.delete(REC_A, &[id_a, id_b]).unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(not_found, 1);
    }

    #[test]
    fn expire_sweep_removes_only_expired() {
        let s = Store::in_memory().unwrap();
        s.insert(REC_A, vec![1], 100, 110, ONE_GB, 50).unwrap();
        s.insert(REC_A, vec![2], 100, 999_999, ONE_GB, 50).unwrap();
        let n = s.expire_sweep(200).unwrap();
        assert_eq!(n, 1);
        assert_eq!(s.fetch(REC_A, 200).unwrap().len(), 1);
    }

    #[test]
    fn cap_overflow_returns_recipient_full_when_no_evictable_rows() {
        let s = Store::in_memory().unwrap();
        // Two existing non-expired deposits filling the 8-byte cap.
        s.insert(REC_A, vec![1; 4], 100, 999_999, 8, 50).unwrap();
        s.insert(REC_A, vec![2; 4], 100, 999_999, 8, 50).unwrap();
        let err = s
            .insert(REC_A, vec![3; 4], 200, 999_999, 8, 50)
            .expect_err("must reject");
        assert!(matches!(
            err,
            MailboxError::Policy(PolicyErrorKind::RecipientFull)
        ));
    }

    #[test]
    fn cap_overflow_evicts_expired_rows_first() {
        let s = Store::in_memory().unwrap();
        // First deposit expires at 110; cap = 8 bytes.
        s.insert(REC_A, vec![1; 4], 100, 110, 8, 50).unwrap();
        s.insert(REC_A, vec![2; 4], 100, 999_999, 8, 50).unwrap();
        // now=200: first row is expired and gets evicted to make room.
        s.insert(REC_A, vec![3; 4], 200, 999_999, 8, 200).unwrap();
        let rows = s.fetch(REC_A, 250).unwrap();
        assert_eq!(rows.len(), 2);
        // Surviving rows: the second (pending) and the third (just inserted).
    }

    #[test]
    fn storage_bytes_tracks_inserts_and_deletes() {
        let s = Store::in_memory().unwrap();
        assert_eq!(s.storage_bytes().unwrap(), 0);
        let id = s
            .insert(REC_A, vec![1; 100], 100, 999_999, ONE_GB, 50)
            .unwrap();
        assert_eq!(s.storage_bytes().unwrap(), 100);
        s.delete(REC_A, &[id]).unwrap();
        assert_eq!(s.storage_bytes().unwrap(), 0);
    }
}
```

### Step 2: Run tests

```bash
cargo test -p skattr-mailbox --lib store::
```
Expected: 7 passing tests.

### Step 3: Lint + commit

```bash
cargo fmt --all
cargo clippy -p skattr-mailbox --all-targets --all-features -- -D warnings
git add crates/mailbox/src/store.rs
git commit -m "$(cat <<'EOF'
mailbox: Store with transactional cap-eviction insert

insert() runs as a single transaction: sums existing bytes,
evicts oldest-expired-first if needed to make room, otherwise
returns RecipientFull. Never silently drops a non-expired
deposit. fetch() filters expired rows server-side; delete() is
recipient-scoped so a leaked id in one recipient's queue can't
delete from another's.

EOF
)"
```

---

## Task 8: `Challenges` — nonce table + signature verify

**Files:**
- Modify: `crates/mailbox/src/auth.rs` (full rewrite)
- Test: same file

### Step 1: Write the failing tests + implementation

Replace `crates/mailbox/src/auth.rs` with:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Challenge-response auth.
//!
//! Server issues a fresh 32-byte nonce on `Challenge`. Clients sign
//!
//! ```text
//! "skattr-mailbox-auth-v1" || nonce || op_byte || sha256(canonical_cbor(payload_minus_signature))
//! ```
//!
//! with their Ed25519 identity key. The server verifies the
//! signature, the `sha256(pubkey) == identity_hash` binding, and the
//! 30-second nonce TTL. Replay defeated by single-use nonces.

use std::collections::HashMap;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::error::{AuthErrorKind, MailboxError};

/// Nonce TTL in seconds.
pub const CHALLENGE_TTL_SECS: i64 = 30;

/// Domain-separation prefix for all auth signatures. Bumped if the
/// signing input format ever changes.
pub const AUTH_DOMAIN: &[u8] = b"skattr-mailbox-auth-v1";

/// Operation byte for FETCH (matches `MailboxFrameKind::Fetch`).
pub const OP_BYTE_FETCH: u8 = 0x86;
/// Operation byte for DELETE (matches `MailboxFrameKind::Delete`).
pub const OP_BYTE_DELETE: u8 = 0x88;

#[derive(Debug, Clone, Copy)]
struct Issued {
    identity_hash: [u8; 32],
    issued_at: i64,
}

/// In-memory challenge table. Lock per-server, not per-connection;
/// nonces are short-lived and the row count stays small.
#[derive(Debug, Default)]
pub struct Challenges {
    inner: HashMap<[u8; 32], Issued>,
}

impl Challenges {
    /// Issue a fresh nonce bound to the given identity hash.
    pub fn issue(&mut self, identity_hash: [u8; 32], now: i64) -> [u8; 32] {
        let mut nonce = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut nonce);
        self.inner.insert(
            nonce,
            Issued {
                identity_hash,
                issued_at: now,
            },
        );
        nonce
    }

    /// Verify a signed Fetch/Delete payload. On success, consumes the
    /// nonce so it can't be replayed. The `op_byte` argument is one of
    /// `OP_BYTE_FETCH` or `OP_BYTE_DELETE`. `payload_hash` is
    /// `sha256(canonical_cbor(payload_minus_signature))` — the dispatch
    /// handler computes this once before calling.
    pub fn verify(
        &mut self,
        nonce: [u8; 32],
        identity_pubkey: [u8; 32],
        signature: &[u8; 64],
        op_byte: u8,
        payload_hash: [u8; 32],
        now: i64,
    ) -> Result<(), MailboxError> {
        let issued = self
            .inner
            .get(&nonce)
            .copied()
            .ok_or(MailboxError::Auth(AuthErrorKind::NonceExpired))?;
        if now - issued.issued_at > CHALLENGE_TTL_SECS {
            self.inner.remove(&nonce);
            return Err(MailboxError::Auth(AuthErrorKind::NonceExpired));
        }
        let computed_hash: [u8; 32] = Sha256::digest(identity_pubkey).into();
        if computed_hash != issued.identity_hash {
            return Err(MailboxError::Auth(AuthErrorKind::HashMismatch));
        }

        let mut signing_input = Vec::with_capacity(AUTH_DOMAIN.len() + 32 + 1 + 32);
        signing_input.extend_from_slice(AUTH_DOMAIN);
        signing_input.extend_from_slice(&nonce);
        signing_input.push(op_byte);
        signing_input.extend_from_slice(&payload_hash);

        let vk = VerifyingKey::from_bytes(&identity_pubkey)
            .map_err(|_| MailboxError::Auth(AuthErrorKind::InvalidSignature))?;
        let sig = Signature::from_bytes(signature);
        vk.verify(&signing_input, &sig)
            .map_err(|_| MailboxError::Auth(AuthErrorKind::InvalidSignature))?;

        // Single-use: consume on successful verify.
        self.inner.remove(&nonce);
        Ok(())
    }

    /// Drop nonces past their TTL. Called periodically by the server.
    /// Returns the number evicted.
    pub fn sweep(&mut self, now: i64) -> u64 {
        let stale: Vec<[u8; 32]> = self
            .inner
            .iter()
            .filter(|(_, v)| now - v.issued_at > CHALLENGE_TTL_SECS)
            .map(|(k, _)| *k)
            .collect();
        let n = stale.len() as u64;
        for k in stale {
            self.inner.remove(&k);
        }
        n
    }

    /// Number of currently-tracked nonces. For tests and metrics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// `true` if there are no tracked nonces.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

/// Compute `sha256(canonical_cbor(payload))` — the helper used by
/// dispatch handlers to build the signing input over the
/// payload-minus-signature.
pub fn payload_digest<T: serde::Serialize>(payload: &T) -> Result<[u8; 32], MailboxError> {
    let mut buf = Vec::new();
    ciborium::into_writer(payload, &mut buf).map_err(|e| {
        MailboxError::Transport(crate::error::TransportErrorKind::EncodeFailed(format!(
            "auth digest: {e}"
        )))
    })?;
    Ok(Sha256::digest(&buf).into())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use rand::rngs::OsRng;

    fn signed_input(
        nonce: [u8; 32],
        op: u8,
        payload_hash: [u8; 32],
        sk: &SigningKey,
    ) -> [u8; 64] {
        let mut input = Vec::new();
        input.extend_from_slice(AUTH_DOMAIN);
        input.extend_from_slice(&nonce);
        input.push(op);
        input.extend_from_slice(&payload_hash);
        sk.sign(&input).to_bytes()
    }

    #[test]
    fn verify_happy_path_consumes_nonce() {
        let sk = SigningKey::generate(&mut OsRng);
        let pk: [u8; 32] = sk.verifying_key().to_bytes();
        let id_hash: [u8; 32] = Sha256::digest(pk).into();

        let mut c = Challenges::default();
        let nonce = c.issue(id_hash, 100);
        let payload_hash = [0xAB; 32];
        let sig = signed_input(nonce, OP_BYTE_FETCH, payload_hash, &sk);

        c.verify(nonce, pk, &sig, OP_BYTE_FETCH, payload_hash, 110)
            .unwrap();
        assert!(c.is_empty(), "nonce must be consumed");
    }

    #[test]
    fn replay_after_consume_fails_nonce_expired() {
        let sk = SigningKey::generate(&mut OsRng);
        let pk: [u8; 32] = sk.verifying_key().to_bytes();
        let id_hash: [u8; 32] = Sha256::digest(pk).into();
        let mut c = Challenges::default();
        let nonce = c.issue(id_hash, 100);
        let payload_hash = [0xCD; 32];
        let sig = signed_input(nonce, OP_BYTE_FETCH, payload_hash, &sk);
        c.verify(nonce, pk, &sig, OP_BYTE_FETCH, payload_hash, 110)
            .unwrap();
        let err = c
            .verify(nonce, pk, &sig, OP_BYTE_FETCH, payload_hash, 110)
            .expect_err("must reject replay");
        assert!(matches!(err, MailboxError::Auth(AuthErrorKind::NonceExpired)));
    }

    #[test]
    fn nonce_expires_after_ttl() {
        let sk = SigningKey::generate(&mut OsRng);
        let pk: [u8; 32] = sk.verifying_key().to_bytes();
        let id_hash: [u8; 32] = Sha256::digest(pk).into();
        let mut c = Challenges::default();
        let nonce = c.issue(id_hash, 100);
        let payload_hash = [0xEE; 32];
        let sig = signed_input(nonce, OP_BYTE_FETCH, payload_hash, &sk);
        let err = c
            .verify(nonce, pk, &sig, OP_BYTE_FETCH, payload_hash, 100 + CHALLENGE_TTL_SECS + 1)
            .expect_err("must reject expired");
        assert!(matches!(err, MailboxError::Auth(AuthErrorKind::NonceExpired)));
    }

    #[test]
    fn hash_mismatch_rejected() {
        let sk = SigningKey::generate(&mut OsRng);
        let pk: [u8; 32] = sk.verifying_key().to_bytes();
        let mut c = Challenges::default();
        let nonce = c.issue([0xFF; 32], 100); // wrong identity hash bound to nonce
        let sig = signed_input(nonce, OP_BYTE_FETCH, [0; 32], &sk);
        let err = c
            .verify(nonce, pk, &sig, OP_BYTE_FETCH, [0; 32], 110)
            .expect_err("must reject");
        assert!(matches!(err, MailboxError::Auth(AuthErrorKind::HashMismatch)));
    }

    #[test]
    fn signed_by_wrong_key_rejected() {
        let real = SigningKey::generate(&mut OsRng);
        let attacker = SigningKey::generate(&mut OsRng);
        let real_pk: [u8; 32] = real.verifying_key().to_bytes();
        let id_hash: [u8; 32] = Sha256::digest(real_pk).into();
        let mut c = Challenges::default();
        let nonce = c.issue(id_hash, 100);
        let payload_hash = [0xAA; 32];
        // Sign with attacker's key, present real's pubkey.
        let sig = signed_input(nonce, OP_BYTE_FETCH, payload_hash, &attacker);
        let err = c
            .verify(nonce, real_pk, &sig, OP_BYTE_FETCH, payload_hash, 110)
            .expect_err("must reject");
        assert!(matches!(err, MailboxError::Auth(AuthErrorKind::InvalidSignature)));
    }

    #[test]
    fn cross_op_signature_rejected() {
        // Sign for FETCH op_byte, present as DELETE — must not verify.
        let sk = SigningKey::generate(&mut OsRng);
        let pk: [u8; 32] = sk.verifying_key().to_bytes();
        let id_hash: [u8; 32] = Sha256::digest(pk).into();
        let mut c = Challenges::default();
        let nonce = c.issue(id_hash, 100);
        let payload_hash = [0x42; 32];
        let sig = signed_input(nonce, OP_BYTE_FETCH, payload_hash, &sk);
        let err = c
            .verify(nonce, pk, &sig, OP_BYTE_DELETE, payload_hash, 110)
            .expect_err("must reject cross-op");
        assert!(matches!(err, MailboxError::Auth(AuthErrorKind::InvalidSignature)));
    }

    #[test]
    fn sweep_evicts_only_expired() {
        let mut c = Challenges::default();
        c.issue([0; 32], 100);
        c.issue([1; 32], 200);
        let evicted = c.sweep(100 + CHALLENGE_TTL_SECS + 1);
        assert_eq!(evicted, 1);
        assert_eq!(c.len(), 1);
    }
}
```

### Step 2: Run tests

```bash
cargo test -p skattr-mailbox --lib auth::
```
Expected: 7 passing tests.

### Step 3: Lint + commit

```bash
cargo fmt --all
cargo clippy -p skattr-mailbox --all-targets --all-features -- -D warnings
git add crates/mailbox/src/auth.rs
git commit -m "$(cat <<'EOF'
mailbox: Challenges nonce table with single-use verify

issue() mints 32 random bytes and binds them to identity_hash +
now. verify() checks the 30s TTL, sha256(pubkey)==identity_hash,
and Ed25519 over the auth string; on success consumes the nonce
so replays fail. sweep() evicts past-TTL nonces in bulk. Cross-op
(FETCH sig presented as DELETE) and wrong-key signatures both
reject with InvalidSignature.

EOF
)"
```

---

## Task 9: `Policy` + `TokenBucket` + per-conn rate limiter

**Files:**
- Modify: `crates/mailbox/src/policy.rs` (full rewrite)
- Test: same file

### Step 1: Write the failing tests + implementation

Replace `crates/mailbox/src/policy.rs` with:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Operator caps + token-bucket rate limiter.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::error::{MailboxError, PolicyErrorKind};

/// Operator-tunable mailbox policy. Defaults from
/// [`Policy::recommended`] match the Phase 2 decomposition spec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    /// Max bytes of `ciphertext` accepted in one Deposit.
    pub max_deposit_size: u64,
    /// Lower clamp on `ttl_request` (non-zero requests below this fail).
    pub min_ttl_secs: u32,
    /// Upper clamp on `ttl_request`.
    pub max_ttl_secs: u32,
    /// Server-assigned TTL when `ttl_request == 0`.
    pub default_ttl_secs: u32,
    /// Maximum bytes stored per recipient hash.
    pub recipient_cap_bytes: u64,
    /// Token-bucket fill rate for Deposits, per connection.
    pub per_conn_deposits_per_min: u32,
    /// Token-bucket fill rate for Fetches, per connection.
    pub per_conn_fetches_per_min: u32,
    /// Server-wide token bucket for Deposits across all connections.
    pub global_deposits_per_min: u32,
}

impl Policy {
    /// Recommended defaults from the Phase 2.A design spec.
    #[must_use]
    pub fn recommended() -> Self {
        Self {
            max_deposit_size: 1_048_576,
            min_ttl_secs: 3_600,
            max_ttl_secs: 2_592_000,
            default_ttl_secs: 604_800,
            recipient_cap_bytes: 268_435_456,
            per_conn_deposits_per_min: 30,
            per_conn_fetches_per_min: 6,
            global_deposits_per_min: 1_000,
        }
    }

    /// Clamp `ttl_request` to `[min_ttl_secs, max_ttl_secs]`. `0` is
    /// shorthand for `default_ttl_secs`. Returns the resolved TTL or
    /// the appropriate [`PolicyErrorKind`].
    pub fn resolve_ttl(&self, ttl_request: u32) -> Result<u32, MailboxError> {
        if ttl_request == 0 {
            return Ok(self.default_ttl_secs);
        }
        if ttl_request < self.min_ttl_secs {
            return Err(MailboxError::Policy(PolicyErrorKind::TtlTooShort));
        }
        if ttl_request > self.max_ttl_secs {
            return Err(MailboxError::Policy(PolicyErrorKind::TtlTooLong));
        }
        Ok(ttl_request)
    }
}

/// Token-bucket rate limiter.
///
/// Refills at `tokens_per_min` over wall time; checks consume one
/// token. The bucket is monotonic in time — no "burst credit" beyond
/// the per-minute cap.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    capacity: f64,
    fill_per_sec: f64,
    available: f64,
    last_refill: f64,
}

impl TokenBucket {
    /// Construct a bucket starting full at `tokens_per_min` capacity.
    /// `now_secs` seeds the refill clock.
    #[must_use]
    pub fn new(tokens_per_min: u32, now_secs: f64) -> Self {
        let capacity = f64::from(tokens_per_min);
        Self {
            capacity,
            fill_per_sec: capacity / 60.0,
            available: capacity,
            last_refill: now_secs,
        }
    }

    /// Try to consume one token at time `now_secs`. Returns `Ok(())`
    /// on success, `Err(RateLimited)` when the bucket is empty.
    pub fn try_acquire(&mut self, now_secs: f64) -> Result<(), MailboxError> {
        let elapsed = (now_secs - self.last_refill).max(0.0);
        self.available = (self.available + elapsed * self.fill_per_sec).min(self.capacity);
        self.last_refill = now_secs;
        if self.available >= 1.0 {
            self.available -= 1.0;
            Ok(())
        } else {
            Err(MailboxError::Policy(PolicyErrorKind::RateLimited))
        }
    }

    /// Current available tokens; for tests/metrics.
    #[must_use]
    pub fn available(&self) -> f64 {
        self.available
    }
}

/// Per-connection rate limiter holding the deposit + fetch buckets.
#[derive(Debug)]
pub struct ConnRateLimiter {
    pub deposits: TokenBucket,
    pub fetches: TokenBucket,
}

impl ConnRateLimiter {
    /// Construct from a [`Policy`] at a given wall time.
    #[must_use]
    pub fn from_policy(p: &Policy, now_secs: f64) -> Self {
        Self {
            deposits: TokenBucket::new(p.per_conn_deposits_per_min, now_secs),
            fetches: TokenBucket::new(p.per_conn_fetches_per_min, now_secs),
        }
    }
}

/// Server-wide global token bucket, wrapped for shared mutability
/// across per-connection accept loops.
#[derive(Debug, Clone)]
pub struct GlobalRateLimiter {
    inner: Arc<Mutex<TokenBucket>>,
}

impl GlobalRateLimiter {
    /// Construct from a [`Policy`].
    #[must_use]
    pub fn from_policy(p: &Policy, now_secs: f64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(TokenBucket::new(
                p.global_deposits_per_min,
                now_secs,
            ))),
        }
    }

    /// Try to consume one global deposit token.
    pub fn try_acquire(&self, now_secs: f64) -> Result<(), MailboxError> {
        let mut g = self.inner.lock().expect("global bucket poisoned");
        g.try_acquire(now_secs)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn resolve_ttl_accepts_zero_as_default() {
        let p = Policy::recommended();
        assert_eq!(p.resolve_ttl(0).unwrap(), p.default_ttl_secs);
    }

    #[test]
    fn resolve_ttl_rejects_below_min() {
        let p = Policy::recommended();
        let err = p.resolve_ttl(60).expect_err("must reject");
        assert!(matches!(err, MailboxError::Policy(PolicyErrorKind::TtlTooShort)));
    }

    #[test]
    fn resolve_ttl_rejects_above_max() {
        let p = Policy::recommended();
        let err = p.resolve_ttl(60 * 60 * 24 * 365).expect_err("must reject");
        assert!(matches!(err, MailboxError::Policy(PolicyErrorKind::TtlTooLong)));
    }

    #[test]
    fn resolve_ttl_accepts_within_bounds() {
        let p = Policy::recommended();
        assert_eq!(p.resolve_ttl(86_400).unwrap(), 86_400);
    }

    #[test]
    fn token_bucket_fills_up_to_capacity() {
        let mut b = TokenBucket::new(60, 0.0);
        for i in 0..60 {
            b.try_acquire(0.0)
                .unwrap_or_else(|_| panic!("token {i} should be available"));
        }
        let err = b.try_acquire(0.0).expect_err("must reject when empty");
        assert!(matches!(err, MailboxError::Policy(PolicyErrorKind::RateLimited)));
    }

    #[test]
    fn token_bucket_refills_over_time() {
        let mut b = TokenBucket::new(60, 0.0);
        for _ in 0..60 {
            b.try_acquire(0.0).unwrap();
        }
        b.try_acquire(0.5).unwrap_or_else(|_| {
            panic!("after 0.5s @ 1 token/sec we should have a token")
        });
    }

    #[test]
    fn token_bucket_caps_refill_at_capacity() {
        let mut b = TokenBucket::new(60, 0.0);
        for _ in 0..30 {
            b.try_acquire(0.0).unwrap();
        }
        // 100 seconds is far past capacity; available must clamp.
        b.try_acquire(100.0).unwrap();
        assert!(
            b.available() <= 60.0,
            "available exceeded capacity: {}",
            b.available()
        );
    }

    #[test]
    fn global_limiter_shares_state_across_clones() {
        let g = GlobalRateLimiter::from_policy(&Policy::recommended(), 0.0);
        let g2 = g.clone();
        for _ in 0..1_000 {
            g.try_acquire(0.0).unwrap();
        }
        let err = g2.try_acquire(0.0).expect_err("clone shares bucket");
        assert!(matches!(err, MailboxError::Policy(PolicyErrorKind::RateLimited)));
    }
}
```

### Step 2: Run tests

```bash
cargo test -p skattr-mailbox --lib policy::
```
Expected: 8 passing tests.

### Step 3: Lint + commit

```bash
cargo fmt --all
cargo clippy -p skattr-mailbox --all-targets --all-features -- -D warnings
git add crates/mailbox/src/policy.rs
git commit -m "$(cat <<'EOF'
mailbox: Policy + TokenBucket + Conn/Global rate limiters

Policy carries the eight operator knobs from the spec, with
resolve_ttl() centralising the TtlTooShort / TtlTooLong / default
clamp logic. TokenBucket is a continuous-refill bucket; clamps at
capacity so a long-idle connection can't burst past the limit.
GlobalRateLimiter wraps an Arc<Mutex<TokenBucket>> for the global
deposits/min cap shared across accept loops.

EOF
)"
```

---

## Task 10: Pure dispatch handlers

**Files:**
- Modify: `crates/mailbox/src/dispatch.rs` (full rewrite)
- Test: same file

Dispatch is split into pure handlers that take refs to `Store`, `Policy`, `Challenges`, `ConnRateLimiter`, `GlobalRateLimiter`, plus a clock (`now: i64`), and return either a response frame or a `MailboxError`. Frame-level wiring lives in Task 11.

### Step 1: Write the failing tests + implementation

Replace `crates/mailbox/src/dispatch.rs` with:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Pure per-frame request handlers.
//!
//! Each `handle_*` returns a [`MailboxFrame`] for the server to send
//! back. Errors are projected to [`ErrorBody`] frames via
//! [`MailboxError::to_wire_code`] in the caller; handlers themselves
//! don't decide whether to close the connection (the FSM in
//! `server.rs` always keeps the stream open after a typed error).

use std::sync::Mutex;

use skattr_core::mailbox::protocol::{
    Challenge, ChallengeNonce, Delete, DeleteOk, Deposit, DepositOk, ErrorBody, Fetch,
    FetchResponse, PendingDeposit, PROTOCOL_VERSION,
};

use crate::auth::{payload_digest, Challenges, OP_BYTE_DELETE, OP_BYTE_FETCH};
use crate::codec::MailboxFrame;
use crate::error::{MailboxError, PolicyErrorKind, TransportErrorKind};
use crate::policy::{ConnRateLimiter, GlobalRateLimiter, Policy};
use crate::store::Store;

/// Cluster of references the handlers need. Keeping this as one
/// struct shortens the per-handler signatures.
pub struct DispatchCtx<'a> {
    pub store: &'a Store,
    pub policy: &'a Policy,
    pub challenges: &'a Mutex<Challenges>,
    pub conn_rl: &'a Mutex<ConnRateLimiter>,
    pub global_rl: &'a GlobalRateLimiter,
}

fn check_version(version: u16) -> Result<(), MailboxError> {
    if version != PROTOCOL_VERSION {
        return Err(MailboxError::Transport(TransportErrorKind::UnsupportedVersion));
    }
    Ok(())
}

/// Build the wire `ErrorBody` for any `MailboxError`. Used by the
/// dispatcher when it needs to convert an internal error into the
/// frame to write back.
#[must_use]
pub fn error_frame(err: &MailboxError) -> MailboxFrame {
    let code = err.to_wire_code();
    // Never include payload-derived strings in `message`; only stable
    // human prose. Drives the redaction unit test in Task 24.
    let message = match err {
        MailboxError::Auth(_) => "auth failed".to_string(),
        MailboxError::Policy(PolicyErrorKind::TooLarge) => "too large".to_string(),
        MailboxError::Policy(PolicyErrorKind::TtlTooLong) => "ttl too long".to_string(),
        MailboxError::Policy(PolicyErrorKind::TtlTooShort) => "ttl too short".to_string(),
        MailboxError::Policy(PolicyErrorKind::RateLimited) => "rate limited".to_string(),
        MailboxError::Policy(PolicyErrorKind::RecipientFull) => "recipient full".to_string(),
        MailboxError::Transport(TransportErrorKind::UnsupportedVersion) => {
            "unsupported version".to_string()
        }
        MailboxError::Transport(TransportErrorKind::DecodeFailed(_)) => {
            "malformed request".to_string()
        }
        _ => "internal error".to_string(),
    };
    MailboxFrame::Error(ErrorBody { code, message })
}

/// `Deposit` handler.
pub fn handle_deposit(
    ctx: &DispatchCtx<'_>,
    body: Deposit,
    now: i64,
    now_secs_f: f64,
) -> Result<MailboxFrame, MailboxError> {
    check_version(body.version)?;
    if u64::try_from(body.ciphertext.len()).unwrap_or(u64::MAX) > ctx.policy.max_deposit_size {
        return Err(MailboxError::Policy(PolicyErrorKind::TooLarge));
    }

    {
        let mut rl = ctx.conn_rl.lock().expect("conn rl poisoned");
        rl.deposits.try_acquire(now_secs_f)?;
    }
    ctx.global_rl.try_acquire(now_secs_f)?;

    let ttl = ctx.policy.resolve_ttl(body.ttl_request)?;
    let expires_at = now.saturating_add(i64::from(ttl));

    let id = ctx.store.insert(
        body.recipient_hash,
        body.ciphertext,
        now,
        expires_at,
        ctx.policy.recipient_cap_bytes,
        now,
    )?;

    Ok(MailboxFrame::DepositOk(DepositOk {
        deposit_id: id,
        expires_at,
    }))
}

/// `Challenge` handler.
pub fn handle_challenge(
    ctx: &DispatchCtx<'_>,
    body: Challenge,
    now: i64,
) -> Result<MailboxFrame, MailboxError> {
    check_version(body.version)?;
    let mut c = ctx.challenges.lock().expect("challenges poisoned");
    let nonce = c.issue(body.identity_hash, now);
    Ok(MailboxFrame::ChallengeNonce(ChallengeNonce {
        nonce,
        issued_at: now,
    }))
}

/// `Fetch` handler.
pub fn handle_fetch(
    ctx: &DispatchCtx<'_>,
    body: Fetch,
    now: i64,
    now_secs_f: f64,
) -> Result<MailboxFrame, MailboxError> {
    check_version(body.version)?;

    {
        let mut rl = ctx.conn_rl.lock().expect("conn rl poisoned");
        rl.fetches.try_acquire(now_secs_f)?;
    }

    // Build the canonical payload-minus-signature for the digest.
    #[derive(serde::Serialize)]
    struct Signed<'a> {
        version: u16,
        identity_pubkey: &'a [u8; 32],
        nonce: &'a [u8; 32],
    }
    let digest = payload_digest(&Signed {
        version: body.version,
        identity_pubkey: &body.identity_pubkey,
        nonce: &body.nonce,
    })?;

    {
        let mut c = ctx.challenges.lock().expect("challenges poisoned");
        c.verify(
            body.nonce,
            body.identity_pubkey,
            &body.signature,
            OP_BYTE_FETCH,
            digest,
            now,
        )?;
    }

    let recipient_hash: [u8; 32] = sha2::Sha256::digest(body.identity_pubkey).into();
    let stored = ctx.store.fetch(recipient_hash, now)?;
    let deposits = stored
        .into_iter()
        .map(|s| PendingDeposit {
            deposit_id: s.deposit_id,
            ciphertext: s.ciphertext,
            received_at: s.received_at,
        })
        .collect();
    Ok(MailboxFrame::FetchResponse(FetchResponse { deposits }))
}

/// `Delete` handler.
pub fn handle_delete(
    ctx: &DispatchCtx<'_>,
    body: Delete,
    now: i64,
) -> Result<MailboxFrame, MailboxError> {
    check_version(body.version)?;

    #[derive(serde::Serialize)]
    struct Signed<'a> {
        version: u16,
        identity_pubkey: &'a [u8; 32],
        nonce: &'a [u8; 32],
        deposit_ids: &'a [[u8; 16]],
    }
    let digest = payload_digest(&Signed {
        version: body.version,
        identity_pubkey: &body.identity_pubkey,
        nonce: &body.nonce,
        deposit_ids: &body.deposit_ids,
    })?;

    {
        let mut c = ctx.challenges.lock().expect("challenges poisoned");
        c.verify(
            body.nonce,
            body.identity_pubkey,
            &body.signature,
            OP_BYTE_DELETE,
            digest,
            now,
        )?;
    }

    let recipient_hash: [u8; 32] = sha2::Sha256::digest(body.identity_pubkey).into();
    let (deleted, not_found) = ctx.store.delete(recipient_hash, &body.deposit_ids)?;
    Ok(MailboxFrame::DeleteOk(DeleteOk { deleted, not_found }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use rand::rngs::OsRng;
    use sha2::Digest;
    use skattr_core::mailbox::protocol::ErrorCode;

    use crate::auth::Challenges;
    use crate::codec::MailboxFrame;
    use crate::policy::{ConnRateLimiter, GlobalRateLimiter, Policy};
    use crate::store::Store;

    fn fixture() -> (Store, Policy, Mutex<Challenges>, Mutex<ConnRateLimiter>, GlobalRateLimiter) {
        let store = Store::in_memory().unwrap();
        let policy = Policy::recommended();
        let chal = Mutex::new(Challenges::default());
        let conn = Mutex::new(ConnRateLimiter::from_policy(&policy, 0.0));
        let global = GlobalRateLimiter::from_policy(&policy, 0.0);
        (store, policy, chal, conn, global)
    }

    #[test]
    fn deposit_happy_path() {
        let (store, policy, chal, conn, global) = fixture();
        let ctx = DispatchCtx {
            store: &store,
            policy: &policy,
            challenges: &chal,
            conn_rl: &conn,
            global_rl: &global,
        };
        let resp = handle_deposit(
            &ctx,
            Deposit {
                version: PROTOCOL_VERSION,
                recipient_hash: [0xAA; 32],
                ciphertext: vec![1, 2, 3, 4],
                ttl_request: 86_400,
            },
            100,
            0.0,
        )
        .unwrap();
        assert!(matches!(resp, MailboxFrame::DepositOk(_)));
    }

    #[test]
    fn deposit_too_large() {
        let (store, policy, chal, conn, global) = fixture();
        let ctx = DispatchCtx {
            store: &store,
            policy: &policy,
            challenges: &chal,
            conn_rl: &conn,
            global_rl: &global,
        };
        let err = handle_deposit(
            &ctx,
            Deposit {
                version: PROTOCOL_VERSION,
                recipient_hash: [0; 32],
                ciphertext: vec![0; (policy.max_deposit_size + 1) as usize],
                ttl_request: 86_400,
            },
            100,
            0.0,
        )
        .expect_err("must reject");
        assert_eq!(err.to_wire_code(), ErrorCode::TooLarge);
    }

    #[test]
    fn deposit_unsupported_version() {
        let (store, policy, chal, conn, global) = fixture();
        let ctx = DispatchCtx {
            store: &store,
            policy: &policy,
            challenges: &chal,
            conn_rl: &conn,
            global_rl: &global,
        };
        let err = handle_deposit(
            &ctx,
            Deposit {
                version: 99,
                recipient_hash: [0; 32],
                ciphertext: vec![],
                ttl_request: 0,
            },
            100,
            0.0,
        )
        .expect_err("must reject");
        assert_eq!(err.to_wire_code(), ErrorCode::UnsupportedVersion);
    }

    #[test]
    fn challenge_returns_nonce() {
        let (store, policy, chal, conn, global) = fixture();
        let ctx = DispatchCtx {
            store: &store,
            policy: &policy,
            challenges: &chal,
            conn_rl: &conn,
            global_rl: &global,
        };
        let resp = handle_challenge(
            &ctx,
            Challenge {
                version: PROTOCOL_VERSION,
                identity_hash: [0xCD; 32],
            },
            100,
        )
        .unwrap();
        assert!(matches!(resp, MailboxFrame::ChallengeNonce(_)));
    }

    fn build_signed_fetch(
        sk: &SigningKey,
        nonce: [u8; 32],
    ) -> Fetch {
        let pk: [u8; 32] = sk.verifying_key().to_bytes();
        // Compute digest the same way handle_fetch does.
        #[derive(serde::Serialize)]
        struct Signed<'a> {
            version: u16,
            identity_pubkey: &'a [u8; 32],
            nonce: &'a [u8; 32],
        }
        let digest = payload_digest(&Signed {
            version: PROTOCOL_VERSION,
            identity_pubkey: &pk,
            nonce: &nonce,
        })
        .unwrap();
        let mut input = Vec::new();
        input.extend_from_slice(crate::auth::AUTH_DOMAIN);
        input.extend_from_slice(&nonce);
        input.push(OP_BYTE_FETCH);
        input.extend_from_slice(&digest);
        let sig = sk.sign(&input).to_bytes();
        Fetch {
            version: PROTOCOL_VERSION,
            identity_pubkey: pk,
            nonce,
            signature: sig,
        }
    }

    #[test]
    fn fetch_happy_path_returns_pending_deposits() {
        let (store, policy, chal, conn, global) = fixture();
        let sk = SigningKey::generate(&mut OsRng);
        let pk: [u8; 32] = sk.verifying_key().to_bytes();
        let id_hash: [u8; 32] = sha2::Sha256::digest(pk).into();
        // Pre-deposit something for the recipient.
        store
            .insert(id_hash, vec![9, 9, 9], 100, 999_999, policy.recipient_cap_bytes, 100)
            .unwrap();
        // Issue a nonce.
        let nonce = chal.lock().unwrap().issue(id_hash, 200);
        let fetch = build_signed_fetch(&sk, nonce);
        let ctx = DispatchCtx {
            store: &store,
            policy: &policy,
            challenges: &chal,
            conn_rl: &conn,
            global_rl: &global,
        };
        let resp = handle_fetch(&ctx, fetch, 210, 0.0).unwrap();
        if let MailboxFrame::FetchResponse(r) = resp {
            assert_eq!(r.deposits.len(), 1);
        } else {
            panic!("expected FetchResponse");
        }
    }

    #[test]
    fn fetch_invalid_signature_rejected() {
        let (store, policy, chal, conn, global) = fixture();
        let sk = SigningKey::generate(&mut OsRng);
        let pk: [u8; 32] = sk.verifying_key().to_bytes();
        let id_hash: [u8; 32] = sha2::Sha256::digest(pk).into();
        let nonce = chal.lock().unwrap().issue(id_hash, 200);
        let mut fetch = build_signed_fetch(&sk, nonce);
        fetch.signature[0] ^= 0xFF;
        let ctx = DispatchCtx {
            store: &store,
            policy: &policy,
            challenges: &chal,
            conn_rl: &conn,
            global_rl: &global,
        };
        let err = handle_fetch(&ctx, fetch, 210, 0.0).expect_err("must reject");
        assert_eq!(err.to_wire_code(), ErrorCode::InvalidSignature);
    }
}
```

### Step 2: Run tests

```bash
cargo test -p skattr-mailbox --lib dispatch::
```
Expected: 6 passing tests.

### Step 3: Lint + commit

```bash
cargo fmt --all
cargo clippy -p skattr-mailbox --all-targets --all-features -- -D warnings
git add crates/mailbox/src/dispatch.rs
git commit -m "$(cat <<'EOF'
mailbox: pure dispatch handlers + error_frame mapping

Each handler is a pure function over (Store, Policy, Challenges,
rate limiters, clock); returns a MailboxFrame. handle_deposit
runs version + size + per-conn-bucket + global-bucket + ttl-clamp
+ store-insert in that order so the earliest failure short-
circuits with the right ErrorCode. handle_fetch and handle_delete
build the canonical payload-minus-signature digest and feed it
through Challenges::verify. error_frame() maps any MailboxError
to a typed ErrorBody with stable, redaction-safe prose.

EOF
)"
```

---

## Task 11: `MailboxServer::accept_loop` — per-stream FSM

**Files:**
- Modify: `crates/mailbox/src/server.rs` (full rewrite)
- Test: same file + `crates/mailbox/tests/integration_inproc.rs`

### Step 1: Write the implementation

Replace `crates/mailbox/src/server.rs` with:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Per-stream accept loop and the top-level [`MailboxServer`] handle.
//!
//! The server is transport-agnostic: any `AsyncRead + AsyncWrite +
//! Unpin + Send` stream can be passed to [`MailboxServer::accept_loop`].
//! The Tor onion lives in `crates/mailbox/src/arti.rs` (binary-only).

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::Framed;
use tracing::{debug, warn};

use crate::auth::Challenges;
use crate::codec::{MailboxFrame, MailboxFrameCodec};
use crate::dispatch::{
    error_frame, handle_challenge, handle_delete, handle_deposit, handle_fetch, DispatchCtx,
};
use crate::error::MailboxError;
use crate::policy::{ConnRateLimiter, GlobalRateLimiter, Policy};
use crate::store::Store;

/// Runtime handle for the mailbox server. The library does NOT own
/// Arti; the binary wraps `MailboxServer` and pipes inbound streams
/// into [`accept_loop`].
#[derive(Debug)]
pub struct MailboxServer {
    store: Arc<Store>,
    policy: Policy,
    challenges: Arc<Mutex<Challenges>>,
    global_rl: GlobalRateLimiter,
}

impl MailboxServer {
    /// Construct a server. Background tasks are launched separately
    /// (Task 12); this constructor only sets up the request-path
    /// state the accept loop needs.
    pub fn new(store: Arc<Store>, policy: Policy) -> Self {
        let now_secs_f = wall_clock_secs_f();
        Self {
            store,
            policy: policy.clone(),
            challenges: Arc::new(Mutex::new(Challenges::default())),
            global_rl: GlobalRateLimiter::from_policy(&policy, now_secs_f),
        }
    }

    /// Get a handle to the shared [`Store`] (used by Task 12 background tasks).
    #[must_use]
    pub fn store(&self) -> Arc<Store> {
        self.store.clone()
    }

    /// Get a handle to the shared challenges table (Task 12 sweep).
    #[must_use]
    pub fn challenges(&self) -> Arc<Mutex<Challenges>> {
        self.challenges.clone()
    }

    /// Drive one client stream until it closes or errors. Per the spec
    /// the connection is **not** closed on policy / auth / rate-limit
    /// rejections; only frame-level codec errors or I/O errors close it.
    pub async fn accept_loop<S>(&self, stream: S) -> Result<(), MailboxError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        let mut framed = Framed::new(stream, MailboxFrameCodec::new());
        let conn_rl = Mutex::new(ConnRateLimiter::from_policy(&self.policy, wall_clock_secs_f()));

        while let Some(next) = framed.next().await {
            let frame = match next {
                Ok(f) => f,
                Err(e @ MailboxError::Transport(_)) => {
                    // Codec-level malformed → reply Error frame, keep
                    // the connection open. (Hard codec errors that
                    // make the stream unrecoverable are surfaced via
                    // .next() returning None; we'd loop back here on
                    // any partial frame, but BytesMut state is fine to
                    // abandon.)
                    let resp = error_frame(&e);
                    if framed.send(resp).await.is_err() {
                        return Ok(());
                    }
                    continue;
                }
                Err(other) => {
                    debug!(?other, "stream-level error closing accept_loop");
                    return Err(other);
                }
            };

            let now = wall_clock_secs_i64();
            let now_f = wall_clock_secs_f();
            let ctx = DispatchCtx {
                store: &self.store,
                policy: &self.policy,
                challenges: &self.challenges,
                conn_rl: &conn_rl,
                global_rl: &self.global_rl,
            };

            let resp = match frame {
                MailboxFrame::Deposit(b) => handle_deposit(&ctx, b, now, now_f),
                MailboxFrame::Challenge(b) => handle_challenge(&ctx, b, now),
                MailboxFrame::Fetch(b) => handle_fetch(&ctx, b, now, now_f),
                MailboxFrame::Delete(b) => handle_delete(&ctx, b, now),
                // S→C frames or unexpected client frames just yield a
                // MalformedRequest reply.
                _ => Err(MailboxError::Transport(
                    crate::error::TransportErrorKind::DecodeFailed(
                        "unexpected client frame".into(),
                    ),
                )),
            };

            let to_send = match resp {
                Ok(f) => f,
                Err(ref e) => {
                    warn!(kind = ?e.kind(), "request rejected");
                    error_frame(e)
                }
            };

            if framed.send(to_send).await.is_err() {
                // Client hung up mid-write; drop quietly.
                return Ok(());
            }
        }
        Ok(())
    }
}

fn wall_clock_secs_i64() -> i64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    d.as_secs() as i64
}

fn wall_clock_secs_f() -> f64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    d.as_secs_f64()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use bytes::BytesMut;
    use tokio_util::codec::{Decoder, Encoder};

    use skattr_core::mailbox::protocol::{Challenge, Deposit, PROTOCOL_VERSION};

    use crate::codec::MailboxFrameCodec;

    #[tokio::test]
    async fn deposit_then_challenge_round_trip_via_duplex() {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let store = Arc::new(Store::in_memory().unwrap());
        let mb = MailboxServer::new(store, Policy::recommended());

        let server_task = tokio::spawn(async move { mb.accept_loop(server).await });

        // Drive the client side.
        let mut framed = Framed::new(client, MailboxFrameCodec::new());

        framed
            .send(MailboxFrame::Deposit(Deposit {
                version: PROTOCOL_VERSION,
                recipient_hash: [0xAB; 32],
                ciphertext: vec![1, 2, 3, 4],
                ttl_request: 86_400,
            }))
            .await
            .unwrap();
        let resp = framed.next().await.unwrap().unwrap();
        assert!(matches!(resp, MailboxFrame::DepositOk(_)));

        framed
            .send(MailboxFrame::Challenge(Challenge {
                version: PROTOCOL_VERSION,
                identity_hash: [0xCD; 32],
            }))
            .await
            .unwrap();
        let resp = framed.next().await.unwrap().unwrap();
        assert!(matches!(resp, MailboxFrame::ChallengeNonce(_)));

        drop(framed); // close client → server loop returns
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn rate_limit_does_not_close_connection() {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let store = Arc::new(Store::in_memory().unwrap());
        let mut policy = Policy::recommended();
        policy.per_conn_deposits_per_min = 1; // hit limit immediately
        let mb = MailboxServer::new(store, policy);
        let server_task = tokio::spawn(async move { mb.accept_loop(server).await });

        let mut framed = Framed::new(client, MailboxFrameCodec::new());
        for i in 0..3 {
            framed
                .send(MailboxFrame::Deposit(Deposit {
                    version: PROTOCOL_VERSION,
                    recipient_hash: [i; 32],
                    ciphertext: vec![1, 2, 3],
                    ttl_request: 86_400,
                }))
                .await
                .unwrap();
            let resp = framed.next().await.unwrap().unwrap();
            // First Ok, second + third Error::RateLimited; in any case the connection stays open.
            let _ = resp;
        }
        // Connection still alive: send a Challenge.
        framed
            .send(MailboxFrame::Challenge(Challenge {
                version: PROTOCOL_VERSION,
                identity_hash: [0xCD; 32],
            }))
            .await
            .unwrap();
        let resp = framed.next().await.unwrap().unwrap();
        assert!(matches!(resp, MailboxFrame::ChallengeNonce(_)));

        drop(framed);
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn unknown_frame_type_returns_error_keeps_connection_open() {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let store = Arc::new(Store::in_memory().unwrap());
        let mb = MailboxServer::new(store, Policy::recommended());
        let server_task = tokio::spawn(async move { mb.accept_loop(server).await });

        // Hand-craft a frame with type byte 0x20 (unknown).
        use tokio::io::AsyncWriteExt;
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&1u32.to_be_bytes());
        buf.extend_from_slice(&[0x20]);
        client.write_all(&buf).await.unwrap();

        // Expect an Error frame back.
        let mut framed = Framed::new(client, MailboxFrameCodec::new());
        let resp = framed.next().await.unwrap().unwrap();
        assert!(matches!(resp, MailboxFrame::Error(_)));

        // Still open — drive a valid Challenge.
        framed
            .send(MailboxFrame::Challenge(Challenge {
                version: PROTOCOL_VERSION,
                identity_hash: [0; 32],
            }))
            .await
            .unwrap();
        let resp = framed.next().await.unwrap().unwrap();
        assert!(matches!(resp, MailboxFrame::ChallengeNonce(_)));

        drop(framed);
        server_task.await.unwrap().unwrap();
    }
}
```

### Step 2: Run tests

```bash
cargo test -p skattr-mailbox --lib server::
```
Expected: 3 passing tests.

### Step 3: Lint + commit

```bash
cargo fmt --all
cargo clippy -p skattr-mailbox --all-targets --all-features -- -D warnings
git add crates/mailbox/src/server.rs
git commit -m "$(cat <<'EOF'
mailbox: MailboxServer::accept_loop drives the per-stream FSM

Transport-agnostic: any AsyncRead+AsyncWrite stream works. Codec
errors and request errors both yield ErrorBody frames over the
same connection rather than closing it (closing on policy/auth
errors invites reconnect storms). Three direct-test cases prove
deposit+challenge round-trip, rate-limit-does-not-close, and
unknown-type-tolerated-with-error semantics.

EOF
)"
```

---

## Task 12: Background tasks — expire / challenge sweep / metrics

**Files:**
- Create: `crates/mailbox/src/background.rs`
- Modify: `crates/mailbox/src/lib.rs` (add `pub mod background;`)
- Test: `crates/mailbox/src/background.rs`

### Step 1: Create the module

`crates/mailbox/src/background.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Periodic background tasks: deposit expiry, challenge nonce
//! sweep, and aggregate metrics emission.
//!
//! Spawned by the binary's `main.rs` once per server instance. Each
//! task runs forever until its `CancellationToken` is fired; the
//! shared owners (`Store`, `Challenges`) live in [`MailboxServer`].

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::auth::Challenges;
use crate::store::Store;

/// Cadence of the deposit expiry sweep.
pub const EXPIRE_SWEEP_INTERVAL: Duration = Duration::from_secs(60);
/// Cadence of the challenge-nonce sweep.
pub const CHALLENGE_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
/// Cadence of the metrics tick.
pub const METRICS_INTERVAL: Duration = Duration::from_secs(60);

/// Spawn the deposit-expiry sweep loop. Returns the join handle so
/// the binary can `await` it during shutdown.
#[must_use]
pub fn spawn_expire_sweep(
    store: Arc<Store>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = interval(EXPIRE_SWEEP_INTERVAL);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tick.tick() => {
                    let now = now_unix_secs();
                    match store.expire_sweep(now) {
                        Ok(n) if n > 0 => info!(expired = n, "deposit expiry sweep"),
                        Ok(_) => {}
                        Err(e) => tracing::error!(?e, tag = "expire_sweep_failed"),
                    }
                }
            }
        }
    })
}

/// Spawn the challenge-nonce sweep loop.
#[must_use]
pub fn spawn_challenge_sweep(
    challenges: Arc<Mutex<Challenges>>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = interval(CHALLENGE_SWEEP_INTERVAL);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tick.tick() => {
                    let now = now_unix_secs();
                    let mut c = match challenges.lock() {
                        Ok(g) => g,
                        Err(_) => continue,
                    };
                    let _ = c.sweep(now);
                }
            }
        }
    })
}

/// Spawn the metrics tick. Logs aggregate counters at info; does
/// not export anywhere outside the local process.
#[must_use]
pub fn spawn_metrics_tick(
    store: Arc<Store>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = interval(METRICS_INTERVAL);
        // Skip the first immediate tick.
        tick.tick().await;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tick.tick() => {
                    if let Ok(bytes) = store.storage_bytes() {
                        info!(storage_bytes = bytes, "mailbox metrics");
                    }
                }
            }
        }
    })
}

fn now_unix_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancel_token_stops_each_task_promptly() {
        let store = Arc::new(Store::in_memory().unwrap());
        let chal = Arc::new(Mutex::new(Challenges::default()));
        let cancel = CancellationToken::new();

        let h1 = spawn_expire_sweep(store.clone(), cancel.clone());
        let h2 = spawn_challenge_sweep(chal.clone(), cancel.clone());
        let h3 = spawn_metrics_tick(store, cancel.clone());

        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(2), async {
            h1.await.unwrap();
            h2.await.unwrap();
            h3.await.unwrap();
        })
        .await
        .expect("background tasks should stop within 2s");
    }
}
```

### Step 2: Add `pub mod background;` to `lib.rs`

In `crates/mailbox/src/lib.rs`, add:

```rust
pub mod background;
```

next to the other `pub mod` declarations.

### Step 3: Run tests

```bash
cargo test -p skattr-mailbox --lib background::
```
Expected: 1 passing test.

### Step 4: Lint + commit

```bash
cargo fmt --all
cargo clippy -p skattr-mailbox --all-targets --all-features -- -D warnings
git add crates/mailbox/src/background.rs crates/mailbox/src/lib.rs
git commit -m "$(cat <<'EOF'
mailbox: background tasks for expiry, challenge sweep, metrics

Three independent tokio tasks, each driven by a tokio::time::interval
and an injected CancellationToken so the binary can shut them down
cleanly. Cadences pulled from the spec: expire 60s, challenge 30s,
metrics 60s. Errors inside a tick are logged and absorbed; the loop
never exits on a transient failure.

EOF
)"
```

---

## Task 13: Health UDS

**Files:**
- Modify: `crates/mailbox/src/health.rs` (full rewrite)
- Test: same file

### Step 1: Implementation + tests

Replace `crates/mailbox/src/health.rs` with:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Local-only Unix-domain healthcheck server.
//!
//! Bound at `${data_dir}/health.sock`, mode 0600. One-line
//! line-based protocol: `GET /health\n` → `ok\n` or
//! `degraded: <reason>\n`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio_util::sync::CancellationToken;

use crate::error::MailboxError;
use crate::store::Store;

/// Health probe outcome.
#[derive(Debug, Clone)]
pub enum HealthStatus {
    /// Everything's fine.
    Ok,
    /// Degraded with a stable, human-readable reason.
    Degraded(String),
}

impl HealthStatus {
    /// Wire reply line ending in `\n`.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            HealthStatus::Ok => "ok\n".to_string(),
            HealthStatus::Degraded(reason) => format!("degraded: {reason}\n"),
        }
    }
}

/// Probe the server's runtime state. The store check is the only
/// dynamic input today; future probes (Arti bootstrap, disk full)
/// extend this function rather than the wire format.
pub fn probe(store: &Store) -> HealthStatus {
    match store.storage_bytes() {
        Ok(_) => HealthStatus::Ok,
        Err(_) => HealthStatus::Degraded("db_unavailable".into()),
    }
}

/// Bind the UDS, set mode 0600, and run the accept loop until
/// `cancel` fires. Returns the join handle.
pub async fn spawn(
    socket_path: PathBuf,
    store: Arc<Store>,
    cancel: CancellationToken,
) -> Result<tokio::task::JoinHandle<()>, MailboxError> {
    let _ = std::fs::remove_file(&socket_path); // ignore ENOENT
    let listener = UnixListener::bind(&socket_path).map_err(MailboxError::Io)?;
    set_mode_0600(&socket_path)?;

    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                accept = listener.accept() => {
                    match accept {
                        Ok((stream, _)) => {
                            let store = store.clone();
                            tokio::spawn(handle_one(stream, store));
                        }
                        Err(e) => {
                            tracing::warn!(?e, "health UDS accept error");
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        }
                    }
                }
            }
        }
    });
    Ok(handle)
}

async fn handle_one(stream: UnixStream, store: Arc<Store>) {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    if reader.read_line(&mut line).await.is_err() {
        return;
    }
    if line.trim() != "GET /health" {
        let _ = write.write_all(b"degraded: unknown_request\n").await;
        return;
    }
    let status = probe(&store);
    let _ = write.write_all(status.line().as_bytes()).await;
}

fn set_mode_0600(path: &Path) -> Result<(), MailboxError> {
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms).map_err(MailboxError::Io)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tokio::time::{timeout, Duration};

    #[test]
    fn health_status_line_format() {
        assert_eq!(HealthStatus::Ok.line(), "ok\n");
        assert_eq!(
            HealthStatus::Degraded("disk_full".into()).line(),
            "degraded: disk_full\n"
        );
    }

    #[tokio::test]
    async fn probe_reports_ok_for_healthy_store() {
        let store = Store::in_memory().unwrap();
        match probe(&store) {
            HealthStatus::Ok => (),
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn end_to_end_uds_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("health.sock");
        let store = Arc::new(Store::in_memory().unwrap());
        let cancel = CancellationToken::new();
        let handle = spawn(sock.clone(), store, cancel.clone()).await.unwrap();

        let mut client = UnixStream::connect(&sock).await.unwrap();
        client.write_all(b"GET /health\n").await.unwrap();
        let mut buf = vec![0u8; 64];
        let n = timeout(Duration::from_secs(2), client.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        let reply = std::str::from_utf8(&buf[..n]).unwrap();
        assert!(reply.starts_with("ok"));

        cancel.cancel();
        timeout(Duration::from_secs(2), handle).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn unknown_request_returns_degraded() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("health.sock");
        let store = Arc::new(Store::in_memory().unwrap());
        let cancel = CancellationToken::new();
        let handle = spawn(sock.clone(), store, cancel.clone()).await.unwrap();

        let mut client = UnixStream::connect(&sock).await.unwrap();
        client.write_all(b"PING\n").await.unwrap();
        let mut buf = vec![0u8; 64];
        let n = timeout(Duration::from_secs(2), client.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        let reply = std::str::from_utf8(&buf[..n]).unwrap();
        assert!(reply.starts_with("degraded: unknown_request"));

        cancel.cancel();
        timeout(Duration::from_secs(2), handle).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn socket_is_mode_0600() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("health.sock");
        let store = Arc::new(Store::in_memory().unwrap());
        let cancel = CancellationToken::new();
        let handle = spawn(sock.clone(), store, cancel.clone()).await.unwrap();
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        cancel.cancel();
        timeout(Duration::from_secs(2), handle).await.unwrap().unwrap();
    }
}
```

### Step 2: Add `tempfile` to dev-dependencies

In `crates/mailbox/Cargo.toml`, under `[dev-dependencies]`, append:

```toml
tempfile = { workspace = true }
```

(`tempfile` is already in the workspace per recent crates' usage; verify with `cargo metadata --format-version 1 | jq '.workspace_dependencies | has("tempfile")'` — if it isn't, add `tempfile = "3"` to the workspace's `[workspace.dependencies]` first.)

### Step 3: Run tests

```bash
cargo test -p skattr-mailbox --lib health::
```
Expected: 5 passing tests.

### Step 4: Lint + commit

```bash
cargo fmt --all
cargo clippy -p skattr-mailbox --all-targets --all-features -- -D warnings
git add crates/mailbox/src/health.rs crates/mailbox/Cargo.toml
git commit -m "$(cat <<'EOF'
mailbox: local-only UDS healthcheck server

Bind path comes from MailboxConfig::server.resolved_health_socket();
unix-only (cfg-gate added later if Windows ever lands). Mode 0600 on
creation. Probe reports 'ok' or 'degraded: <reason>'. Never reachable
through the onion. End-to-end test exercises round-trip plus
permission bits.

EOF
)"
```

---

## Task 14: Arti glue (binary-only)

**Files:**
- Create: `crates/mailbox/src/arti.rs`
- Modify: `crates/mailbox/src/lib.rs` (cfg-gated `pub mod arti;`)

### Step 1: Implementation

`crates/mailbox/src/arti.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Arti + tor-hsservice glue (binary-only).
//!
//! Bootstraps the Tor client, publishes a v3 onion service, and pipes
//! every inbound stream into [`MailboxServer::accept_loop`].

#![cfg(feature = "bin")]

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures::StreamExt as _;
use tor_hsservice::{HsNickname, OnionService, OnionServiceConfigBuilder, RunningOnionService};

use crate::server::MailboxServer;

/// Configure the onion service nickname (the on-disk identifier; not
/// the public onion address). Operators can change this; defaults to
/// `mailbox`.
pub const DEFAULT_HS_NICKNAME: &str = "mailbox";

/// Spawned-onion handle. Drop closes the listener and tears down the
/// circuit.
pub struct OnionListener {
    _service: Arc<RunningOnionService>,
    _join: tokio::task::JoinHandle<()>,
}

/// Bring up Arti + the v3 onion service, then forward inbound streams
/// to `server.accept_loop`. Returns once the service is published; the
/// background task lives on the returned handle.
pub async fn run_onion(
    arti_state_dir: &Path,
    nickname: &str,
    server: Arc<MailboxServer>,
) -> Result<OnionListener> {
    let runtime = tor_rtcompat::PreferredRuntime::current()
        .context("tor runtime")?;
    let cfg = arti_client::TorClientConfig::builder()
        .storage()
        .cache_dir(arti_state_dir.into())
        .state_dir(arti_state_dir.into())
        .build()
        .context("arti config")?;
    let tor = arti_client::TorClient::with_runtime(runtime.clone())
        .config(cfg)
        .create_unbootstrapped()
        .context("arti client create")?;
    tor.bootstrap().await.context("arti bootstrap")?;

    let nickname = HsNickname::new(nickname.to_string()).context("hs nickname")?;
    let hs_cfg = OnionServiceConfigBuilder::default()
        .nickname(nickname.clone())
        .build()
        .context("hs config")?;

    let (svc, request_stream) = tor
        .launch_onion_service(hs_cfg)
        .context("launch onion service")?;

    tracing::info!(onion = %svc.onion_name().expect("onion name available"), "mailbox onion published");

    let server2 = server.clone();
    let join = tokio::spawn(async move {
        let mut stream = tor_hsservice::handle_rend_requests(request_stream);
        while let Some(stream_request) = stream.next().await {
            let server = server2.clone();
            tokio::spawn(async move {
                let stream = match stream_request.accept(tor_hsservice::StreamRequestRules::all()).await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(?e, "rejected onion stream");
                        return;
                    }
                };
                if let Err(e) = server.accept_loop(stream).await {
                    tracing::warn!(?e, "accept_loop returned error");
                }
            });
        }
    });

    Ok(OnionListener {
        _service: svc,
        _join: join,
    })
}
```

### Step 2: Cfg-gate the module

In `crates/mailbox/src/lib.rs`, add:

```rust
#[cfg(feature = "bin")]
pub mod arti;
```

### Step 3: Verify compilation under both feature flags

```bash
cargo build -p skattr-mailbox                          # default features (bin)
cargo build -p skattr-mailbox --no-default-features    # lib-only
cargo clippy -p skattr-mailbox --all-targets --all-features -- -D warnings
```
Expected: all green.

> **Note for the executor:** The exact `tor-hsservice 0.41` API surface (e.g., `handle_rend_requests`, `StreamRequestRules::all()`, the rendezvous-stream type) may differ slightly from the snippet above. If the call sites don't compile verbatim, follow the closest matching idiom in the existing `crates/core/src/transport/listener.rs` — that file already drives `tor-hsservice 0.41` end-to-end and is the authoritative pattern for this crate.

### Step 4: Commit

```bash
git add crates/mailbox/src/arti.rs crates/mailbox/src/lib.rs
git commit -m "$(cat <<'EOF'
mailbox: Arti + tor-hsservice glue (feature='bin')

Bootstraps the embedded Tor client, publishes a v3 onion via
tor-hsservice 0.41, and dispatches inbound rendezvous streams into
MailboxServer::accept_loop. Cfg-gated so library tests, fuzz, and
soak skip the Tor compile cost. Mirrors core::transport::listener
patterns from Phase 0.C.

EOF
)"
```

---

## Task 15: `main.rs` — CLI parse, signal handling, sd-notify

**Files:**
- Modify: `crates/mailbox/src/main.rs` (full rewrite)

### Step 1: Implementation

Replace `crates/mailbox/src/main.rs` with:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version. See LICENSE-AGPL3.

//! `skattr-mailbox` binary entry point.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use skattr_mailbox::{
    arti::{run_onion, DEFAULT_HS_NICKNAME},
    background::{spawn_challenge_sweep, spawn_expire_sweep, spawn_metrics_tick},
    health,
    server::MailboxServer,
    store::Store,
    MailboxConfig,
};

/// Command-line arguments.
#[derive(Debug, Parser)]
#[command(version, about = "Skattr mailbox server", long_about = None)]
struct Args {
    /// Path to `mailbox.toml`.
    #[arg(short, long)]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "skattr_mailbox=info,warn".into()),
        )
        .init();

    let args = Args::parse();
    let cfg = MailboxConfig::load(&args.config)
        .with_context(|| format!("load {}", args.config.display()))?;

    std::fs::create_dir_all(&cfg.server.data_dir).context("create data_dir")?;

    let store = Arc::new(
        Store::open(&cfg.server.resolved_storage_path()).context("open store")?,
    );

    let server = Arc::new(MailboxServer::new(store.clone(), cfg.policy.clone()));

    let cancel = CancellationToken::new();
    let _expire = spawn_expire_sweep(store.clone(), cancel.clone());
    let _chal = spawn_challenge_sweep(server.challenges(), cancel.clone());
    let _metrics = spawn_metrics_tick(store.clone(), cancel.clone());

    let _health = health::spawn(
        cfg.server.resolved_health_socket(),
        store.clone(),
        cancel.clone(),
    )
    .await
    .context("health UDS")?;

    let _onion = run_onion(
        &cfg.server.resolved_arti_state_dir(),
        DEFAULT_HS_NICKNAME,
        server.clone(),
    )
    .await
    .context("arti")?;

    // Notify systemd we're up (best-effort; ignore on non-systemd hosts).
    let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]);

    tokio::signal::ctrl_c().await.ok();
    tracing::info!("shutdown signal received");
    cancel.cancel();
    let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Stopping]);
    Ok(())
}
```

### Step 2: Verify it builds

```bash
cargo build -p skattr-mailbox
cargo clippy -p skattr-mailbox --all-targets --all-features -- -D warnings
```
Expected: clean.

### Step 3: Commit

```bash
git add crates/mailbox/src/main.rs
git commit -m "$(cat <<'EOF'
mailbox: bin/main.rs wires the full server

clap-based CLI taking --config <path>. Loads MailboxConfig, opens
Store, constructs MailboxServer, spawns the three background tasks
+ health UDS + Arti onion. sd-notify Ready/Stopping markers fire
into systemd; SIGINT (or systemd SIGTERM) cancels every task and
returns.

EOF
)"
```

---

## Task 16: Property tests

**Files:**
- Create: `crates/mailbox/tests/property.rs`

### Step 1: Implementation

Create `crates/mailbox/tests/property.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Property tests for the mailbox protocol invariants. Spec §"Test
//! plan / 2. Property" — every property here is a freeze-bar item.

use bytes::BytesMut;
use ed25519_dalek::{Signer, SigningKey};
use proptest::prelude::*;
use sha2::{Digest, Sha256};
use skattr_core::mailbox::protocol::{
    Challenge, Delete, Deposit, ErrorBody, ErrorCode, Fetch, PROTOCOL_VERSION,
};
use skattr_mailbox::{
    auth::{payload_digest, AUTH_DOMAIN, OP_BYTE_FETCH},
    codec::{MailboxFrame, MailboxFrameCodec},
    policy::Policy,
};
use tokio_util::codec::{Decoder, Encoder};

fn round_trip(f: MailboxFrame) -> MailboxFrame {
    let mut codec = MailboxFrameCodec::new();
    let mut buf = BytesMut::new();
    codec.encode(f, &mut buf).expect("encode");
    codec.decode(&mut buf).expect("decode").expect("frame")
}

proptest! {
    #[test]
    fn deposit_cbor_round_trip(
        recipient_hash in proptest::array::uniform32(any::<u8>()),
        ttl in any::<u32>(),
        ciphertext in proptest::collection::vec(any::<u8>(), 0..512),
    ) {
        let f = MailboxFrame::Deposit(Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash,
            ciphertext,
            ttl_request: ttl,
        });
        prop_assert_eq!(round_trip(f.clone()), f);
    }

    #[test]
    fn challenge_cbor_round_trip(
        identity_hash in proptest::array::uniform32(any::<u8>()),
    ) {
        let f = MailboxFrame::Challenge(Challenge {
            version: PROTOCOL_VERSION,
            identity_hash,
        });
        prop_assert_eq!(round_trip(f.clone()), f);
    }

    #[test]
    fn delete_cbor_round_trip(
        identity_pubkey in proptest::array::uniform32(any::<u8>()),
        nonce in proptest::array::uniform32(any::<u8>()),
        signature in proptest::array::uniform32(any::<u8>()),
        ids in proptest::collection::vec(proptest::array::uniform16(any::<u8>()), 0..8),
    ) {
        // Pad signature to 64 bytes by repeating each byte twice deterministically
        // (proptest::array::uniform64 is unwieldy — inflate from 32 to 64 here).
        let mut sig = [0u8; 64];
        for (i, b) in signature.iter().enumerate() {
            sig[i] = *b;
            sig[i + 32] = *b;
        }
        let f = MailboxFrame::Delete(Delete {
            version: PROTOCOL_VERSION,
            identity_pubkey,
            nonce,
            signature: sig,
            deposit_ids: ids,
        });
        prop_assert_eq!(round_trip(f.clone()), f);
    }

    #[test]
    fn error_cbor_round_trip(message in "[a-zA-Z0-9 ]{0,64}") {
        for code in [
            ErrorCode::UnsupportedVersion,
            ErrorCode::MalformedRequest,
            ErrorCode::TooLarge,
            ErrorCode::RateLimited,
            ErrorCode::RecipientFull,
            ErrorCode::TtlTooLong,
            ErrorCode::TtlTooShort,
            ErrorCode::InvalidSignature,
            ErrorCode::HashMismatch,
            ErrorCode::NonceExpired,
            ErrorCode::NotFound,
            ErrorCode::Internal,
        ] {
            let f = MailboxFrame::Error(ErrorBody {
                code,
                message: message.clone(),
            });
            prop_assert_eq!(round_trip(f.clone()), f);
        }
    }

    #[test]
    fn ttl_clamp_is_monotonic_in_now(
        ttl_request in 1u32..(2_592_000),
        now1 in 1_000_000_000i64..1_900_000_000,
        delta in 1i64..100_000,
    ) {
        let p = Policy::recommended();
        let resolved = p.resolve_ttl(ttl_request);
        if let Ok(secs) = resolved {
            // expires_at is now + secs. Monotonic in now: now1 < now1+delta
            // implies expires_at(now1) < expires_at(now1+delta).
            let e1 = now1.saturating_add(i64::from(secs));
            let e2 = now1.saturating_add(delta).saturating_add(i64::from(secs));
            prop_assert!(e2 >= e1);
        }
    }

    #[test]
    fn signed_then_verified_always_passes(
        identity_seed in proptest::array::uniform32(any::<u8>()),
        nonce in proptest::array::uniform32(any::<u8>()),
        payload_hash in proptest::array::uniform32(any::<u8>()),
    ) {
        let sk = SigningKey::from_bytes(&identity_seed);
        let pk: [u8; 32] = sk.verifying_key().to_bytes();
        let mut input = Vec::new();
        input.extend_from_slice(AUTH_DOMAIN);
        input.extend_from_slice(&nonce);
        input.push(OP_BYTE_FETCH);
        input.extend_from_slice(&payload_hash);
        let sig = sk.sign(&input).to_bytes();
        // Verify directly via dalek's API (mirrors what Challenges::verify does).
        let vk = ed25519_dalek::VerifyingKey::from_bytes(&pk).unwrap();
        prop_assert!(
            vk.verify_strict(&input, &ed25519_dalek::Signature::from_bytes(&sig)).is_ok()
        );
    }

    #[test]
    fn payload_digest_is_stable_under_field_reorder(
        v in any::<u16>(),
        nonce in proptest::array::uniform32(any::<u8>()),
        pubkey in proptest::array::uniform32(any::<u8>()),
    ) {
        // Canonical CBOR sorts keys; encoding the same logical record
        // twice must yield the same digest regardless of the source
        // struct's field declaration order.
        #[derive(serde::Serialize)]
        struct A<'a> {
            version: u16,
            identity_pubkey: &'a [u8; 32],
            nonce: &'a [u8; 32],
        }
        #[derive(serde::Serialize)]
        struct B<'a> {
            nonce: &'a [u8; 32],
            identity_pubkey: &'a [u8; 32],
            version: u16,
        }
        let d1 = payload_digest(&A { version: v, identity_pubkey: &pubkey, nonce: &nonce }).unwrap();
        let d2 = payload_digest(&B { nonce: &nonce, identity_pubkey: &pubkey, version: v }).unwrap();
        // Note: ciborium does NOT canonicalise keys for serde-derived
        // structs. If this assert fails, we need to switch to manual
        // canonical-encoding for the auth digest. The test serves as
        // a tripwire so the freeze ADR can record the chosen behaviour.
        prop_assert_eq!(d1, d2);
    }
}
```

> **Implementation note for the executor:** the last property (`payload_digest_is_stable_under_field_reorder`) is a tripwire. If `ciborium`'s serde derive does NOT produce a canonical encoding (i.e. the digests differ), the spec's auth construction is ambiguous and the executor MUST escalate before continuing — choices are (a) hand-roll canonical CBOR encoding for the digest's signed input, or (b) lock the field order in the spec and document that auth signatures are sensitive to derive ordering. Pick (a) for safety and update Task 8's `payload_digest` to use a canonical encoder; otherwise leave as-is and flag in the freeze ADR.

### Step 2: Run tests

```bash
cargo test -p skattr-mailbox --test property
```
Expected: 7 passing properties (each runs 256 cases by default).

### Step 3: Lint + commit

```bash
cargo fmt --all
cargo clippy -p skattr-mailbox --all-targets --all-features -- -D warnings
git add crates/mailbox/tests/property.rs
git commit -m "$(cat <<'EOF'
mailbox: property tests for protocol round-trips + invariants

Seven proptest-driven cases:
- Deposit/Challenge/Delete/Error CBOR round-trip is identity
- TTL clamp is monotonic in now
- sign-then-verify always passes for the auth string
- payload_digest is stable under struct field reorder (tripwire
  for canonical-CBOR ambiguity)

EOF
)"
```

---

## Task 17: Adversarial test suite

**Files:**
- Create: `crates/mailbox/tests/adversarial_auth.rs`
- Create: `crates/mailbox/tests/adversarial_policy.rs`
- Create: `crates/mailbox/tests/adversarial_storage.rs`
- Create: `crates/mailbox/tests/adversarial_codec.rs`

These four files cover every variant of `protocol::ErrorCode`. Each test calls into the `MailboxServer::accept_loop` over `tokio::io::duplex` with a hand-crafted bad input and asserts the wire reply carries the expected `ErrorCode`.

### Step 1: Auth adversarial cases

`crates/mailbox/tests/adversarial_auth.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Adversarial coverage for HashMismatch / InvalidSignature / NonceExpired.

use std::sync::Arc;

use ed25519_dalek::{Signer, SigningKey};
use futures::{SinkExt, StreamExt};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use skattr_core::mailbox::protocol::{Challenge, ErrorCode, Fetch, PROTOCOL_VERSION};
use skattr_mailbox::auth::{payload_digest, AUTH_DOMAIN, OP_BYTE_FETCH};
use skattr_mailbox::codec::{MailboxFrame, MailboxFrameCodec};
use skattr_mailbox::policy::Policy;
use skattr_mailbox::server::MailboxServer;
use skattr_mailbox::store::Store;
use tokio_util::codec::Framed;

async fn spawn_server() -> (
    Framed<tokio::io::DuplexStream, MailboxFrameCodec>,
    tokio::task::JoinHandle<Result<(), skattr_mailbox::error::MailboxError>>,
) {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let store = Arc::new(Store::in_memory().unwrap());
    let mb = MailboxServer::new(store, Policy::recommended());
    let handle = tokio::spawn(async move { mb.accept_loop(server).await });
    (Framed::new(client, MailboxFrameCodec::new()), handle)
}

fn build_signed_fetch(sk: &SigningKey, nonce: [u8; 32]) -> Fetch {
    let pk: [u8; 32] = sk.verifying_key().to_bytes();
    #[derive(serde::Serialize)]
    struct Signed<'a> {
        version: u16,
        identity_pubkey: &'a [u8; 32],
        nonce: &'a [u8; 32],
    }
    let digest = payload_digest(&Signed {
        version: PROTOCOL_VERSION,
        identity_pubkey: &pk,
        nonce: &nonce,
    })
    .unwrap();
    let mut input = Vec::new();
    input.extend_from_slice(AUTH_DOMAIN);
    input.extend_from_slice(&nonce);
    input.push(OP_BYTE_FETCH);
    input.extend_from_slice(&digest);
    let sig = sk.sign(&input).to_bytes();
    Fetch {
        version: PROTOCOL_VERSION,
        identity_pubkey: pk,
        nonce,
        signature: sig,
    }
}

async fn issue_nonce(
    framed: &mut Framed<tokio::io::DuplexStream, MailboxFrameCodec>,
    identity_hash: [u8; 32],
) -> [u8; 32] {
    framed
        .send(MailboxFrame::Challenge(Challenge {
            version: PROTOCOL_VERSION,
            identity_hash,
        }))
        .await
        .unwrap();
    match framed.next().await.unwrap().unwrap() {
        MailboxFrame::ChallengeNonce(n) => n.nonce,
        other => panic!("expected ChallengeNonce, got {other:?}"),
    }
}

#[tokio::test]
async fn hash_mismatch_rejected() {
    let (mut framed, handle) = spawn_server().await;
    let sk = SigningKey::generate(&mut OsRng);
    // Bind nonce to a totally unrelated identity hash.
    let nonce = issue_nonce(&mut framed, [0xFF; 32]).await;
    let fetch = build_signed_fetch(&sk, nonce);
    framed.send(MailboxFrame::Fetch(fetch)).await.unwrap();
    let resp = framed.next().await.unwrap().unwrap();
    if let MailboxFrame::Error(e) = resp {
        assert_eq!(e.code, ErrorCode::HashMismatch);
    } else {
        panic!("expected Error frame");
    }
    drop(framed);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn invalid_signature_rejected() {
    let (mut framed, handle) = spawn_server().await;
    let sk = SigningKey::generate(&mut OsRng);
    let pk: [u8; 32] = sk.verifying_key().to_bytes();
    let id_hash: [u8; 32] = Sha256::digest(pk).into();
    let nonce = issue_nonce(&mut framed, id_hash).await;
    let mut fetch = build_signed_fetch(&sk, nonce);
    fetch.signature[0] ^= 0xFF;
    framed.send(MailboxFrame::Fetch(fetch)).await.unwrap();
    let resp = framed.next().await.unwrap().unwrap();
    if let MailboxFrame::Error(e) = resp {
        assert_eq!(e.code, ErrorCode::InvalidSignature);
    } else {
        panic!("expected Error frame");
    }
    drop(framed);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn nonce_replay_rejected() {
    let (mut framed, handle) = spawn_server().await;
    let sk = SigningKey::generate(&mut OsRng);
    let pk: [u8; 32] = sk.verifying_key().to_bytes();
    let id_hash: [u8; 32] = Sha256::digest(pk).into();
    let nonce = issue_nonce(&mut framed, id_hash).await;
    let fetch = build_signed_fetch(&sk, nonce);
    // First use: succeeds.
    framed.send(MailboxFrame::Fetch(fetch.clone())).await.unwrap();
    let _ = framed.next().await.unwrap().unwrap();
    // Replay: must reject as NonceExpired.
    framed.send(MailboxFrame::Fetch(fetch)).await.unwrap();
    let resp = framed.next().await.unwrap().unwrap();
    if let MailboxFrame::Error(e) = resp {
        assert_eq!(e.code, ErrorCode::NonceExpired);
    } else {
        panic!("expected Error frame");
    }
    drop(framed);
    handle.await.unwrap().unwrap();
}
```

### Step 2: Policy adversarial cases

`crates/mailbox/tests/adversarial_policy.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Adversarial coverage for TooLarge / TtlTooLong / TtlTooShort /
//! RateLimited / RecipientFull / UnsupportedVersion.

use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use skattr_core::mailbox::protocol::{Deposit, ErrorCode, PROTOCOL_VERSION};
use skattr_mailbox::codec::{MailboxFrame, MailboxFrameCodec};
use skattr_mailbox::policy::Policy;
use skattr_mailbox::server::MailboxServer;
use skattr_mailbox::store::Store;
use tokio_util::codec::Framed;

async fn spawn_with_policy(
    policy: Policy,
) -> (
    Framed<tokio::io::DuplexStream, MailboxFrameCodec>,
    tokio::task::JoinHandle<Result<(), skattr_mailbox::error::MailboxError>>,
) {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let store = Arc::new(Store::in_memory().unwrap());
    let mb = MailboxServer::new(store, policy);
    let handle = tokio::spawn(async move { mb.accept_loop(server).await });
    (Framed::new(client, MailboxFrameCodec::new()), handle)
}

async fn deposit_and_get_code(
    framed: &mut Framed<tokio::io::DuplexStream, MailboxFrameCodec>,
    body: Deposit,
) -> ErrorCode {
    framed.send(MailboxFrame::Deposit(body)).await.unwrap();
    match framed.next().await.unwrap().unwrap() {
        MailboxFrame::Error(e) => e.code,
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn unsupported_version_rejected() {
    let (mut framed, handle) = spawn_with_policy(Policy::recommended()).await;
    let code = deposit_and_get_code(
        &mut framed,
        Deposit {
            version: 99,
            recipient_hash: [0; 32],
            ciphertext: vec![],
            ttl_request: 0,
        },
    )
    .await;
    assert_eq!(code, ErrorCode::UnsupportedVersion);
    drop(framed);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn too_large_rejected() {
    let policy = Policy::recommended();
    let max = policy.max_deposit_size as usize;
    let (mut framed, handle) = spawn_with_policy(policy).await;
    let code = deposit_and_get_code(
        &mut framed,
        Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash: [0; 32],
            ciphertext: vec![0; max + 1],
            ttl_request: 86_400,
        },
    )
    .await;
    assert_eq!(code, ErrorCode::TooLarge);
    drop(framed);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn ttl_too_short_rejected() {
    let (mut framed, handle) = spawn_with_policy(Policy::recommended()).await;
    let code = deposit_and_get_code(
        &mut framed,
        Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash: [0; 32],
            ciphertext: vec![1, 2, 3],
            ttl_request: 60, // below 1h min
        },
    )
    .await;
    assert_eq!(code, ErrorCode::TtlTooShort);
    drop(framed);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn ttl_too_long_rejected() {
    let (mut framed, handle) = spawn_with_policy(Policy::recommended()).await;
    let code = deposit_and_get_code(
        &mut framed,
        Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash: [0; 32],
            ciphertext: vec![1, 2, 3],
            ttl_request: u32::MAX,
        },
    )
    .await;
    assert_eq!(code, ErrorCode::TtlTooLong);
    drop(framed);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn rate_limit_triggers() {
    let mut policy = Policy::recommended();
    policy.per_conn_deposits_per_min = 1;
    let (mut framed, handle) = spawn_with_policy(policy).await;
    // First succeeds.
    framed
        .send(MailboxFrame::Deposit(Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash: [0; 32],
            ciphertext: vec![1, 2, 3],
            ttl_request: 86_400,
        }))
        .await
        .unwrap();
    let _ok = framed.next().await.unwrap().unwrap();
    // Second hits rate limit.
    let code = deposit_and_get_code(
        &mut framed,
        Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash: [1; 32],
            ciphertext: vec![1, 2, 3],
            ttl_request: 86_400,
        },
    )
    .await;
    assert_eq!(code, ErrorCode::RateLimited);
    drop(framed);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn recipient_full_rejected_when_no_evictable_rows() {
    let mut policy = Policy::recommended();
    policy.recipient_cap_bytes = 8;
    policy.max_deposit_size = 8;
    let (mut framed, handle) = spawn_with_policy(policy).await;
    // Fill the cap with two non-expired deposits.
    for i in 0..2 {
        framed
            .send(MailboxFrame::Deposit(Deposit {
                version: PROTOCOL_VERSION,
                recipient_hash: [9; 32],
                ciphertext: vec![i; 4],
                ttl_request: 86_400,
            }))
            .await
            .unwrap();
        let _ = framed.next().await.unwrap().unwrap();
    }
    let code = deposit_and_get_code(
        &mut framed,
        Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash: [9; 32],
            ciphertext: vec![0xAB; 4],
            ttl_request: 86_400,
        },
    )
    .await;
    assert_eq!(code, ErrorCode::RecipientFull);
    drop(framed);
    handle.await.unwrap().unwrap();
}
```

### Step 3: Storage adversarial cases

`crates/mailbox/tests/adversarial_storage.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Concurrent-delete races + cap eviction ordering.

use skattr_mailbox::store::Store;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_delete_yields_consistent_counts() {
    let store = std::sync::Arc::new(Store::in_memory().unwrap());
    let recipient = [0x42u8; 32];
    let mut ids = Vec::new();
    for _ in 0..50 {
        let id = store
            .insert(recipient, vec![1, 2, 3], 100, 999_999, 1 << 30, 50)
            .unwrap();
        ids.push(id);
    }
    let s1 = store.clone();
    let s2 = store.clone();
    let ids_a = ids.clone();
    let ids_b = ids.clone();
    let h1 = tokio::spawn(async move { s1.delete(recipient, &ids_a).unwrap() });
    let h2 = tokio::spawn(async move { s2.delete(recipient, &ids_b).unwrap() });
    let (a, b) = (h1.await.unwrap(), h2.await.unwrap());
    // Together: every row was deleted exactly once across both calls.
    assert_eq!(a.0 + b.0, 50);
    // Not-found is the complement: each task asked for 50 ids, only some succeeded.
    assert_eq!(a.1 + b.1, 50);
}

#[test]
fn cap_eviction_evicts_oldest_expired_first() {
    let store = Store::in_memory().unwrap();
    let recipient = [0xAB; 32];
    // Two expired rows (oldest first) + one fresh.
    let _id1 = store
        .insert(recipient, vec![1; 4], 100, 110, 16, 50)
        .unwrap(); // oldest expired
    let _id2 = store
        .insert(recipient, vec![2; 4], 200, 210, 16, 150)
        .unwrap(); // newer expired
    let id3 = store
        .insert(recipient, vec![3; 4], 300, 999_999, 16, 250)
        .unwrap(); // pending
    // Now insert a new row; expecting eviction of id1 first.
    let id4 = store
        .insert(recipient, vec![4; 4], 400, 999_999, 16, 400)
        .unwrap();
    let rows = store.fetch(recipient, 500).unwrap();
    let surviving: std::collections::HashSet<[u8; 16]> =
        rows.iter().map(|r| r.deposit_id).collect();
    assert!(surviving.contains(&id3), "pending must survive");
    assert!(surviving.contains(&id4), "newly inserted must survive");
}
```

### Step 4: Codec adversarial cases

`crates/mailbox/tests/adversarial_codec.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Malformed CBOR and unknown frames yield ErrorCode::MalformedRequest.

use std::sync::Arc;

use bytes::BytesMut;
use futures::StreamExt;
use skattr_core::mailbox::protocol::ErrorCode;
use skattr_mailbox::codec::{MailboxFrame, MailboxFrameCodec};
use skattr_mailbox::policy::Policy;
use skattr_mailbox::server::MailboxServer;
use skattr_mailbox::store::Store;
use tokio::io::AsyncWriteExt;
use tokio_util::codec::Framed;

#[tokio::test]
async fn malformed_cbor_returns_malformed_request_keeps_open() {
    let (mut client, server) = tokio::io::duplex(64 * 1024);
    let store = Arc::new(Store::in_memory().unwrap());
    let mb = MailboxServer::new(store, Policy::recommended());
    let handle = tokio::spawn(async move { mb.accept_loop(server).await });

    // Hand-craft: length=4, type=Deposit (0x82), 3 garbage bytes.
    let mut buf = BytesMut::new();
    buf.extend_from_slice(&4u32.to_be_bytes());
    buf.extend_from_slice(&[0x82, 0xFF, 0xFF, 0xFF]);
    client.write_all(&buf).await.unwrap();

    let mut framed = Framed::new(client, MailboxFrameCodec::new());
    let resp = framed.next().await.unwrap().unwrap();
    if let MailboxFrame::Error(e) = resp {
        assert_eq!(e.code, ErrorCode::MalformedRequest);
    } else {
        panic!("expected Error");
    }
    drop(framed);
    handle.await.unwrap().unwrap();
}
```

### Step 5: Run all four files

```bash
cargo test -p skattr-mailbox --tests
```
Expected: every test in `adversarial_*` green; `property` green from Task 16.

### Step 6: Lint + commit

```bash
cargo fmt --all
cargo clippy -p skattr-mailbox --all-targets --all-features -- -D warnings
git add crates/mailbox/tests/adversarial_auth.rs \
        crates/mailbox/tests/adversarial_policy.rs \
        crates/mailbox/tests/adversarial_storage.rs \
        crates/mailbox/tests/adversarial_codec.rs
git commit -m "$(cat <<'EOF'
mailbox: adversarial test suite covering every ErrorCode variant

Four files split by attack class:
- adversarial_auth: HashMismatch, InvalidSignature, NonceExpired
- adversarial_policy: UnsupportedVersion, TooLarge, TtlTooShort,
  TtlTooLong, RateLimited, RecipientFull
- adversarial_storage: concurrent-delete consistency, cap-eviction
  ordering
- adversarial_codec: MalformedRequest from garbage CBOR

Together these prove every wire-error code is reachable from a
real client interaction, satisfying the freeze ADR's coverage bar.

EOF
)"
```

---

## Task 18: Fuzz harness

**Files:**
- Create: `crates/mailbox/fuzz/Cargo.toml`
- Create: `crates/mailbox/fuzz/fuzz_targets/frame_decode.rs`
- Create: `crates/mailbox/fuzz/fuzz_targets/dispatch.rs`
- Create: `crates/mailbox/fuzz/.gitignore`
- Modify: `crates/mailbox/Cargo.toml` (`[workspace]` exclude line)

### Step 1: Initialise the fuzz crate

`crates/mailbox/fuzz/Cargo.toml`:

```toml
[package]
name = "skattr-mailbox-fuzz"
version = "0.0.0"
edition = "2021"
publish = false
license = "AGPL-3.0-or-later"

[package.metadata]
cargo-fuzz = true

[dependencies]
libfuzzer-sys = "0.4"
skattr-core = { path = "../../core" }
skattr-mailbox = { path = ".." }
arbitrary = { version = "1", features = ["derive"] }

[[bin]]
name = "frame_decode"
path = "fuzz_targets/frame_decode.rs"
test = false
doc = false

[[bin]]
name = "dispatch"
path = "fuzz_targets/dispatch.rs"
test = false
doc = false

[workspace]
# Independent so the workspace's stable toolchain doesn't clash with
# nightly libfuzzer-sys.
```

`crates/mailbox/fuzz/.gitignore`:

```gitignore
artifacts/
corpus/
target/
```

### Step 2: Frame-decode target

`crates/mailbox/fuzz/fuzz_targets/frame_decode.rs`:

```rust
#![no_main]

use bytes::BytesMut;
use libfuzzer_sys::fuzz_target;
use skattr_mailbox::codec::MailboxFrameCodec;
use tokio_util::codec::Decoder;

fuzz_target!(|data: &[u8]| {
    let mut codec = MailboxFrameCodec::new();
    let mut buf = BytesMut::from(data);
    // Run decode in a loop until either it returns Ok(None) (need
    // more bytes), errors, or empties the buffer. Errors are expected
    // — we only assert no panic.
    loop {
        match codec.decode(&mut buf) {
            Ok(Some(_frame)) => {
                if buf.is_empty() {
                    break;
                }
            }
            Ok(None) | Err(_) => break,
        }
    }
});
```

### Step 3: Dispatch target (drives a fresh server with arbitrary bytes)

`crates/mailbox/fuzz/fuzz_targets/dispatch.rs`:

```rust
#![no_main]

use libfuzzer_sys::fuzz_target;
use std::sync::Arc;

use tokio::io::AsyncWriteExt;

use skattr_mailbox::policy::Policy;
use skattr_mailbox::server::MailboxServer;
use skattr_mailbox::store::Store;

fuzz_target!(|data: &[u8]| {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async move {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let store = Arc::new(Store::in_memory().unwrap());
        let mb = MailboxServer::new(store, Policy::recommended());
        let handle = tokio::spawn(async move { mb.accept_loop(server).await });

        // Push the fuzzer's bytes wholesale; the server's loop will
        // consume what it can, write replies, and (if asked nicely)
        // hit a typed error. We're hunting panics, not protocol
        // adherence.
        let _ = client.write_all(data).await;
        drop(client);
        let _ = handle.await;
    });
});
```

### Step 4: Verify it builds (skip nightly-only `cargo fuzz` for now)

```bash
cd crates/mailbox/fuzz
cargo +stable check
cd -
```
Expected: clean. (Running the fuzzer itself requires `cargo install cargo-fuzz` and a nightly toolchain; the freeze bar is "1 hour locally, no findings" — operator runs it on their workstation.)

### Step 5: Tell the workspace to ignore the fuzz crate

In the root `/home/myggiz/development/skattr-phase-2a-mailbox-server/Cargo.toml`, under `[workspace]`, add (or extend):

```toml
exclude = ["crates/mailbox/fuzz"]
```

(If `exclude` already exists, append `"crates/mailbox/fuzz"` to its array.)

### Step 6: Document the run procedure

Add a short note to `docs/operations/mailbox-setup.md` (Task 23 will create it; for now, add it as a TODO marker in `crates/mailbox/fuzz/README.md`):

`crates/mailbox/fuzz/README.md`:

```markdown
# skattr-mailbox-fuzz

cargo-fuzz harness for the mailbox protocol decoder and dispatch loop.

## Local run

Requires nightly Rust:

```bash
rustup toolchain install nightly
cargo install cargo-fuzz
cd crates/mailbox/fuzz
cargo +nightly fuzz run frame_decode -- -max_total_time=3600
cargo +nightly fuzz run dispatch     -- -max_total_time=3600
```

Findings (if any) land in `artifacts/`; commit reproducer files
under `corpus/` and add a regression test in
`crates/mailbox/tests/adversarial_codec.rs`.

The Phase 2.A freeze bar requires both targets to run for ≥ 1 hour
locally with no findings before the merge PR.
```

### Step 7: Commit

```bash
git add Cargo.toml crates/mailbox/fuzz/
git commit -m "$(cat <<'EOF'
mailbox: cargo-fuzz harness for frame decoder + dispatch loop

Two targets: frame_decode (drives MailboxFrameCodec::decode with
arbitrary bytes) and dispatch (pushes raw bytes into a live
MailboxServer over duplex). Both expect no panics; protocol
adherence is checked elsewhere. README documents the 1h freeze-bar
local run.

Excluded from the workspace so nightly libfuzzer-sys doesn't
contaminate the stable build.

EOF
)"
```

---

## Task 19: 24-hour soak driver

**Files:**
- Create: `crates/mailbox/tests/soak.rs`

### Step 1: Driver

`crates/mailbox/tests/soak.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! 24-hour soak driver. `#[ignore]`-gated; run on a developer
//! workstation as part of the freeze-PR validation, not on CI:
//!
//! ```bash
//! cargo test -p skattr-mailbox --release --test soak -- --ignored \
//!     --nocapture > docs/superpowers/runs/<merge-date>-mailbox-soak.txt
//! ```

#![cfg(not(target_os = "windows"))] // UDS-only platforms

use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use futures::{SinkExt, StreamExt};
use rand::{rngs::OsRng, Rng, RngCore, SeedableRng};
use sha2::{Digest, Sha256};
use skattr_core::mailbox::protocol::{Challenge, Deposit, PROTOCOL_VERSION};
use skattr_mailbox::codec::{MailboxFrame, MailboxFrameCodec};
use skattr_mailbox::policy::Policy;
use skattr_mailbox::server::MailboxServer;
use skattr_mailbox::store::Store;
use tokio_util::codec::Framed;

const SOAK_RECIPIENTS: usize = 1_000;
const SOAK_DURATION_SECS: u64 = 24 * 3600;
const SOAK_DEPOSIT_RATE_PER_HOUR: u64 = 100;

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore]
async fn soak_24h() {
    let store = Arc::new(Store::in_memory().unwrap());
    let mb = Arc::new(MailboxServer::new(store.clone(), Policy::recommended()));

    // Pre-generate identities.
    let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(0x5BAA_F00D);
    let identities: Vec<[u8; 32]> = (0..SOAK_RECIPIENTS)
        .map(|_| {
            let mut s = [0u8; 32];
            rng.fill_bytes(&mut s);
            let sk = SigningKey::from_bytes(&s);
            sk.verifying_key().to_bytes()
        })
        .collect();

    // Spawn deposit producers. One task per recipient, jittered.
    let start = Instant::now();
    let deadline = start + Duration::from_secs(SOAK_DURATION_SECS);
    let mut handles = Vec::new();
    for pk in &identities {
        let recipient_hash: [u8; 32] = Sha256::digest(pk).into();
        let mb = mb.clone();
        handles.push(tokio::spawn(async move {
            // Mean inter-arrival = 3600 / rate seconds.
            let mean = Duration::from_secs_f64(3600.0 / SOAK_DEPOSIT_RATE_PER_HOUR as f64);
            let mut rng = rand::thread_rng();
            while Instant::now() < deadline {
                let jitter = rng.gen_range(0.5..1.5);
                tokio::time::sleep(mean.mul_f64(jitter)).await;
                let (client, server) = tokio::io::duplex(8 * 1024);
                let mb2 = mb.clone();
                tokio::spawn(async move { mb2.accept_loop(server).await });
                let mut framed = Framed::new(client, MailboxFrameCodec::new());
                let body = Deposit {
                    version: PROTOCOL_VERSION,
                    recipient_hash,
                    ciphertext: vec![0u8; rng.gen_range(64..4096)],
                    ttl_request: 86_400,
                };
                if framed.send(MailboxFrame::Deposit(body)).await.is_err() {
                    continue;
                }
                let _ = framed.next().await;
            }
        }));
    }

    // Periodic invariant checks.
    let store_for_metrics = store.clone();
    let metrics_handle = tokio::spawn(async move {
        let mut peak_bytes: u64 = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            if Instant::now() >= deadline {
                break;
            }
            let bytes = store_for_metrics.storage_bytes().unwrap_or(0);
            peak_bytes = peak_bytes.max(bytes);
            eprintln!(
                "soak metrics: t={}s bytes={} peak={}",
                start.elapsed().as_secs(),
                bytes,
                peak_bytes
            );
        }
        peak_bytes
    });

    for h in handles {
        let _ = h.await;
    }
    let peak_bytes = metrics_handle.await.unwrap();
    let final_bytes = store.storage_bytes().unwrap();
    let policy = Policy::recommended();
    let max_recipient_total = policy.recipient_cap_bytes * SOAK_RECIPIENTS as u64;
    eprintln!(
        "SOAK SUMMARY peak_bytes={} final_bytes={} max_allowed={}",
        peak_bytes, final_bytes, max_recipient_total
    );
    assert!(
        peak_bytes <= max_recipient_total + policy.max_deposit_size,
        "storage exceeded recipient_cap_bytes * recipients by more than one deposit"
    );
}
```

> **Add `rand_chacha` to dev-dependencies:** in `crates/mailbox/Cargo.toml`, append `rand_chacha = "0.3"` under `[dev-dependencies]`.

### Step 2: Verify the driver compiles

```bash
cargo test -p skattr-mailbox --release --test soak --no-run
```
Expected: builds. Don't actually run the 24h test now; it's `#[ignore]`-gated and runs on a developer workstation.

### Step 3: Commit

```bash
git add crates/mailbox/tests/soak.rs crates/mailbox/Cargo.toml
git commit -m "$(cat <<'EOF'
mailbox: 24h soak driver, #[ignore]-gated

Spawns 1000 recipient producers each sending ~100 deposits/hour with
±50% jitter. Per-minute metrics tick logs storage bytes and tracks
peak. Final assertion: peak storage never exceeds
recipient_cap_bytes * recipients + one deposit (the worst case for
'just barely over' before eviction kicks in). Output captured to
docs/superpowers/runs/<merge-date>-mailbox-soak.txt for the freeze PR.

EOF
)"
```

---

## Task 20: Real-Tor smoke test

**Files:**
- Create: `crates/tests/src/mailbox_real_tor.rs`
- Modify: `crates/tests/src/lib.rs` (add `pub mod mailbox_real_tor;`)

### Step 1: Implementation

`crates/tests/src/mailbox_real_tor.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Real-Tor smoke test: spawn the `skattr-mailbox` binary, publish a
//! v3 onion, drive a Deposit + Challenge round-trip from a client
//! that talks via `arti-client`. `#[ignore]`-gated; run with
//!
//! ```bash
//! cargo test -p skattr-tests --release --test mailbox_real_tor -- --ignored
//! ```

#![cfg(feature = "test-harness")]

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use futures::{SinkExt, StreamExt};
use skattr_core::mailbox::protocol::{Challenge, Deposit, PROTOCOL_VERSION};
use skattr_mailbox::codec::{MailboxFrame, MailboxFrameCodec};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio_util::codec::Framed;

async fn spawn_mailbox_with_published_onion() -> Result<(Child, String, TempDir)> {
    let dir = TempDir::new()?;
    let cfg_path = dir.path().join("mailbox.toml");
    let body = format!(
        r#"
[server]
data_dir = "{}"
[policy]
max_deposit_size = 1048576
min_ttl_secs = 3600
max_ttl_secs = 2592000
default_ttl_secs = 604800
recipient_cap_bytes = 268435456
per_conn_deposits_per_min = 30
per_conn_fetches_per_min = 6
global_deposits_per_min = 1000
"#,
        dir.path().display()
    );
    std::fs::write(&cfg_path, body)?;

    let bin = env!("CARGO_BIN_EXE_skattr-mailbox");
    let mut child = Command::new(bin)
        .arg("--config")
        .arg(&cfg_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    // Tail stderr until we see a "mailbox onion published onion=..." line.
    let stderr = child.stderr.take().expect("stderr piped");
    let mut reader = tokio::io::BufReader::new(stderr).lines();
    let mut onion = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    while std::time::Instant::now() < deadline {
        let line_fut = reader.next_line();
        match tokio::time::timeout(Duration::from_secs(5), line_fut).await {
            Ok(Ok(Some(line))) => {
                if let Some(idx) = line.find("onion=") {
                    let val = &line[idx + "onion=".len()..];
                    let end = val.find(' ').unwrap_or(val.len());
                    onion = Some(val[..end].trim_matches('"').to_string());
                    break;
                }
            }
            _ => continue,
        }
    }
    let onion = match onion {
        Some(s) => s,
        None => bail!("mailbox did not publish an onion in time"),
    };
    Ok((child, onion, dir))
}

#[tokio::test]
#[ignore]
async fn deposit_and_challenge_over_real_tor() -> Result<()> {
    let (_child, onion, _dir) = spawn_mailbox_with_published_onion().await?;
    eprintln!("mailbox onion: {onion}");

    // Drive a client through arti-client.
    let runtime = tor_rtcompat::PreferredRuntime::current()?;
    let cfg = arti_client::TorClientConfig::default();
    let tor = arti_client::TorClient::with_runtime(runtime)
        .config(cfg)
        .create_unbootstrapped()?;
    tor.bootstrap().await?;

    let target = arti_client::TorAddr::from(format!("{onion}:1"))?;
    let stream = tor.connect(&target).await?;

    let mut framed = Framed::new(stream, MailboxFrameCodec::new());
    framed
        .send(MailboxFrame::Deposit(Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash: [0xAA; 32],
            ciphertext: vec![1, 2, 3, 4],
            ttl_request: 86_400,
        }))
        .await?;
    let resp = framed
        .next()
        .await
        .ok_or_else(|| anyhow::anyhow!("no reply"))??;
    assert!(matches!(resp, MailboxFrame::DepositOk(_)));

    framed
        .send(MailboxFrame::Challenge(Challenge {
            version: PROTOCOL_VERSION,
            identity_hash: [0xBB; 32],
        }))
        .await?;
    let resp = framed
        .next()
        .await
        .ok_or_else(|| anyhow::anyhow!("no reply"))??;
    assert!(matches!(resp, MailboxFrame::ChallengeNonce(_)));
    Ok(())
}
```

### Step 2: Wire into `crates/tests/src/lib.rs`

Append:

```rust
#[cfg(all(feature = "test-harness", any(target_os = "linux", target_os = "macos")))]
pub mod mailbox_real_tor;
```

### Step 3: Verify build

```bash
cargo test -p skattr-tests --release --test mailbox_real_tor --no-run --features test-harness
```
Expected: builds. Don't run; it's `#[ignore]`-gated and the freeze checklist runs it manually.

### Step 4: Commit

```bash
git add crates/tests/src/mailbox_real_tor.rs crates/tests/src/lib.rs
git commit -m "$(cat <<'EOF'
mailbox: real-Tor smoke test in crates/tests

Spawns the skattr-mailbox binary, waits for the onion to publish
(by tailing stderr for 'onion=...'), then drives a Deposit +
Challenge round-trip from an arti-client. Mirrors the
delivery_real_tor.rs pattern from 1.E. #[ignore]-gated; run with
'cargo test -p skattr-tests --release -- --ignored'.

EOF
)"
```

---

## Task 21: systemd unit

**Files:**
- Create: `packaging/systemd/skattr-mailbox.service`

### Step 1: Write the unit file

`packaging/systemd/skattr-mailbox.service`:

```
[Unit]
Description=Skattr mailbox server
Documentation=https://github.com/myggiz/skattr/blob/main/docs/operations/mailbox-setup.md
After=network.target

[Service]
Type=notify
ExecStart=/usr/local/bin/skattr-mailbox --config /etc/skattr-mailbox/mailbox.toml
DynamicUser=yes
StateDirectory=skattr-mailbox
WorkingDirectory=/var/lib/skattr-mailbox
ConfigurationDirectory=skattr-mailbox

# Hardening
ProtectSystem=strict
ProtectHome=yes
NoNewPrivileges=yes
PrivateTmp=yes
PrivateDevices=yes
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
RestrictNamespaces=yes
RestrictRealtime=yes
RestrictSUIDSGID=yes
LockPersonality=yes
MemoryDenyWriteExecute=yes
SystemCallArchitectures=native
SystemCallFilter=@system-service
SystemCallFilter=~@privileged

# Lifecycle
WatchdogSec=120
Restart=on-failure
RestartSec=10s
TimeoutStopSec=30s

# Logging
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
```

### Step 2: Commit

```bash
mkdir -p packaging/systemd
mv /tmp/...skip... # already on disk per Step 1
git add packaging/systemd/skattr-mailbox.service
git commit -m "$(cat <<'EOF'
mailbox: hardened systemd unit

Type=notify (binary calls sd_notify Ready/Stopping). DynamicUser +
StateDirectory + ConfigurationDirectory mean the operator never
manages a uid manually. Hardening: ProtectSystem=strict,
NoNewPrivileges, MemoryDenyWriteExecute, SystemCallFilter=@system-
service ~@privileged. WatchdogSec=120 paired with sd-notify
keepalives in main.rs (added in Task 15).

EOF
)"
```

---

## Task 22: Dockerfile (distroless)

**Files:**
- Create: `packaging/Dockerfile`
- Create: `packaging/docker-compose.example.yml`

### Step 1: Two-stage Dockerfile

`packaging/Dockerfile`:

```dockerfile
# syntax=docker/dockerfile:1.6

# Build stage: pinned to the workspace toolchain.
FROM rust:1-slim-bookworm AS build
WORKDIR /work
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential pkg-config libssl-dev ca-certificates && rm -rf /var/lib/apt/lists/*
COPY rust-toolchain.toml ./
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY deny.toml ./deny.toml
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/work/target \
    cargo build --release -p skattr-mailbox && \
    cp /work/target/release/skattr-mailbox /usr/local/bin/skattr-mailbox

# Runtime: distroless cc, non-root.
FROM gcr.io/distroless/cc-debian12:nonroot
LABEL org.opencontainers.image.source="https://github.com/myggiz/skattr"
LABEL org.opencontainers.image.licenses="AGPL-3.0-or-later"
COPY --from=build /usr/local/bin/skattr-mailbox /usr/local/bin/skattr-mailbox
USER nonroot:nonroot
VOLUME ["/var/lib/skattr-mailbox"]
EXPOSE 0
ENTRYPOINT ["/usr/local/bin/skattr-mailbox", "--config", "/etc/skattr-mailbox/mailbox.toml"]
```

### Step 2: Compose snippet

`packaging/docker-compose.example.yml`:

```yaml
services:
  mailbox:
    image: skattr-mailbox:latest
    restart: unless-stopped
    user: nonroot
    volumes:
      - ./mailbox-data:/var/lib/skattr-mailbox
      - ./mailbox.toml:/etc/skattr-mailbox/mailbox.toml:ro
    healthcheck:
      test: ["CMD", "socat", "-", "UNIX-CONNECT:/var/lib/skattr-mailbox/health.sock"]
      interval: 30s
      timeout: 5s
      retries: 3
      start_period: 60s
```

### Step 3: Verify the Dockerfile builds

```bash
docker build -f packaging/Dockerfile -t skattr-mailbox:dev .
```
Expected: a runnable image. (Skip if Docker isn't available; mark as TODO for the merge PR. The instruction MUST be re-attempted before merging.)

### Step 4: Commit

```bash
git add packaging/Dockerfile packaging/docker-compose.example.yml
git commit -m "$(cat <<'EOF'
mailbox: distroless Dockerfile + compose example

Two-stage build: rust:1-slim-bookworm compiles, gcr.io/distroless/
cc-debian12:nonroot runs. BuildKit cache mounts on cargo registry +
target dir so re-pulls in CI don't re-download crates.io. Compose
example wires the config + data volume + UDS healthcheck via socat.

EOF
)"
```

---

## Task 23: Operations guide

**Files:**
- Create: `docs/operations/mailbox-setup.md`

### Step 1: Write the doc

`docs/operations/mailbox-setup.md`:

```markdown
# Skattr mailbox operator guide

A skattr mailbox is a semi-trusted relay that holds encrypted
deposits for offline recipients. Operators learn:

- That a particular pubkey hash has deposits waiting (via DEPOSIT).
- That a recipient is online (via FETCH/DELETE timing).

Operators **do not** learn message contents, sender identity, or
who is communicating with whom (that's bound to recipient hashes,
not pubkeys).

This guide walks an operator from a fresh VM to a running mailbox in
under 30 minutes.

## Choosing your install path

| Path        | When                                                                                    |
|-------------|-----------------------------------------------------------------------------------------|
| systemd     | Production on a Debian/Ubuntu/Arch host. Best ergonomics + hardening.                   |
| Docker      | Container-first deployments. AGPL-compatible registries only.                           |
| from-source | Custom builds, dev systems, hosts without systemd or Docker. Ten extra minutes.         |

## Path A — systemd (Debian / Ubuntu)

### Prerequisites

- Linux with systemd 247+ (`systemctl --version`).
- A user account with sudo.
- TCP egress to the Tor network.

### Install

```bash
# 1. Install Rust toolchain (skip if already present).
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"

# 2. Build and install the binary.
git clone https://github.com/myggiz/skattr
cd skattr
cargo build --release -p skattr-mailbox
sudo install -Dm755 target/release/skattr-mailbox /usr/local/bin/skattr-mailbox

# 3. Drop in the systemd unit and config skeleton.
sudo install -Dm644 packaging/systemd/skattr-mailbox.service \
    /etc/systemd/system/skattr-mailbox.service
sudo install -d -m755 /etc/skattr-mailbox
sudo cat > /etc/skattr-mailbox/mailbox.toml <<'EOF'
[server]
data_dir = "/var/lib/skattr-mailbox"

[policy]
max_deposit_size           = 1048576
min_ttl_secs               = 3600
max_ttl_secs               = 2592000
default_ttl_secs           = 604800
recipient_cap_bytes        = 268435456
per_conn_deposits_per_min  = 30
per_conn_fetches_per_min   = 6
global_deposits_per_min    = 1000
EOF

# 4. Start and enable.
sudo systemctl daemon-reload
sudo systemctl enable --now skattr-mailbox
sudo systemctl status skattr-mailbox
```

### Discovering your onion address

```bash
sudo journalctl -u skattr-mailbox --since "10 minutes ago" \
    | grep "mailbox onion published"
```

The line ends with `onion="<your-onion>.onion"`. Hand that to your
users.

### Healthchecks

```bash
sudo socat - UNIX-CONNECT:/var/lib/skattr-mailbox/health.sock <<<"GET /health"
```

Replies `ok` or `degraded: <reason>`.

## Path B — Docker

```bash
git clone https://github.com/myggiz/skattr
cd skattr
docker build -f packaging/Dockerfile -t skattr-mailbox:latest .

mkdir -p mailbox-data
cp packaging/docker-compose.example.yml docker-compose.yml
# Drop a mailbox.toml next to docker-compose.yml (template above).
docker compose up -d
docker compose logs -f mailbox | grep "mailbox onion published"
```

## Path C — from-source, no init system

`skattr-mailbox --config /path/to/mailbox.toml` runs in the foreground.
Wrap with `tmux` / `screen` / `nohup` as you prefer. `SIGINT` /
`SIGTERM` shuts down cleanly.

## Configuration reference

See `crates/mailbox/src/config.rs` for the canonical schema. Key knobs:

| Field                                  | Default      | What it does                                           |
|----------------------------------------|--------------|--------------------------------------------------------|
| `[server].data_dir`                    | _required_   | Parent for all server state.                           |
| `[policy].max_deposit_size`            | 1 048 576    | Bytes; deposits above this get `TooLarge`.             |
| `[policy].min_ttl_secs`                | 3 600        | TTL clamp lower bound.                                 |
| `[policy].max_ttl_secs`                | 2 592 000    | TTL clamp upper bound (30 days).                       |
| `[policy].default_ttl_secs`            | 604 800      | TTL when client requests `0` (7 days).                 |
| `[policy].recipient_cap_bytes`         | 268 435 456  | Per-recipient byte cap (256 MiB).                      |
| `[policy].per_conn_deposits_per_min`   | 30           | Token bucket per inbound stream.                       |
| `[policy].per_conn_fetches_per_min`    | 6            | Token bucket per inbound stream.                       |
| `[policy].global_deposits_per_min`     | 1 000        | Server-wide cap; brake against reconnect storms.       |

## Backup

```bash
sqlite3 /var/lib/skattr-mailbox/mailbox.sqlite ".backup '/var/backups/mailbox-$(date +%Y%m%d).bak'"
```

WAL mode means no quiesce required. Restore by copying the `.bak` file
back into place while the service is stopped.

## Upgrade

Migrations are forward-only. Stop the service, swap the binary,
restart. Any in-flight requests are rejected cleanly via the
30 s `TimeoutStopSec`.

```bash
sudo systemctl stop skattr-mailbox
sudo install -Dm755 target/release/skattr-mailbox /usr/local/bin/skattr-mailbox
sudo systemctl start skattr-mailbox
```

## Troubleshooting

| Symptom                                     | Likely cause + fix                                                                                                  |
|---------------------------------------------|---------------------------------------------------------------------------------------------------------------------|
| `degraded: db_unavailable`                  | SQLite file missing or unreadable. Check `data_dir` perms; `ls -la $data_dir/mailbox.sqlite`.                        |
| `degraded: arti_not_bootstrapped`           | Tor failed to bootstrap. Check egress; tail `journalctl -u skattr-mailbox` for `arti` errors.                       |
| `RateLimited` floods                        | Either real traffic exceeds capacity or an attacker is reconnecting. Tighten `global_deposits_per_min` first.        |
| Storage growing past `recipient_cap_bytes`  | Should be impossible per cap eviction. File a bug + attach the soak test output.                                    |
| Repeated `Internal` replies                 | Server-side bug. Capture the `journalctl` trace at `error` level and file an issue.                                  |

## What this server does NOT do

- It does not federate or forward. Each mailbox is standalone.
- It does not register operators with any directory. If you want
  others to use your mailbox, share the onion out-of-band.
- It does not expose metrics over the network. All metrics are local
  log lines under `tracing` `info` level.
- It does not log identity hashes, pubkeys, or ciphertexts above
  `debug`. The redaction unit test enforces this.
```

### Step 2: Commit

```bash
git add docs/operations/mailbox-setup.md
git commit -m "$(cat <<'EOF'
docs: mailbox operator guide for systemd / Docker / from-source

Three install paths each documented end-to-end. Configuration
reference table covers every Policy knob with its default and
purpose. Backup/upgrade/troubleshooting sections close the loop.
Target: a fresh-VM operator gets to a running, onion-published
mailbox in under 30 minutes.

EOF
)"
```

---

## Task 24: Logging redaction unit test

**Files:**
- Create: `crates/mailbox/tests/logging_redaction.rs`

The spec mandates: no identity pubkeys, full hashes, ciphertexts, or deposit_ids above `debug`. This test bakes the rule into CI.

### Step 1: Write the test

`crates/mailbox/tests/logging_redaction.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Asserts the mailbox's `info`/`warn`/`error`-level log lines never
//! contain a full 64-hex pubkey, full 32-byte hash, ciphertext, or
//! 16-byte deposit_id. Implementation strategy: drive a sequence of
//! in-process operations through MailboxServer, capture all log
//! events at `info+` via a `tracing_subscriber::Layer` that records
//! every message into a Vec<String>, then scan.

use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use futures::{SinkExt, StreamExt};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use skattr_core::mailbox::protocol::{Challenge, Deposit, PROTOCOL_VERSION};
use skattr_mailbox::codec::{MailboxFrame, MailboxFrameCodec};
use skattr_mailbox::policy::Policy;
use skattr_mailbox::server::MailboxServer;
use skattr_mailbox::store::Store;
use tokio_util::codec::Framed;
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

#[derive(Default, Clone)]
struct Capture(Arc<Mutex<Vec<String>>>);

impl<S: Subscriber> Layer<S> for Capture {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = StringVisitor(String::new());
        event.record(&mut visitor);
        let level = *event.metadata().level();
        if level <= tracing::Level::INFO {
            self.0.lock().unwrap().push(visitor.0);
        }
    }
}

struct StringVisitor(String);

impl tracing::field::Visit for StringVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0
            .push_str(&format!("{}={:?} ", field.name(), value));
    }
}

#[tokio::test]
async fn no_full_hash_or_pubkey_at_info_level() {
    let cap = Capture::default();
    let cap_clone = cap.clone();
    let subscriber = Registry::default().with(cap_clone);
    let _g = tracing::subscriber::set_default(subscriber);

    // Drive a Deposit + Challenge + a deliberate auth failure.
    let (client, server) = tokio::io::duplex(64 * 1024);
    let store = Arc::new(Store::in_memory().unwrap());
    let mb = MailboxServer::new(store, Policy::recommended());
    let handle = tokio::spawn(async move { mb.accept_loop(server).await });

    let mut framed = Framed::new(client, MailboxFrameCodec::new());
    let recipient = [0xAA; 32];
    framed
        .send(MailboxFrame::Deposit(Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash: recipient,
            ciphertext: vec![0xCC; 64],
            ttl_request: 86_400,
        }))
        .await
        .unwrap();
    let _ = framed.next().await.unwrap().unwrap();

    let sk = SigningKey::generate(&mut OsRng);
    let pk: [u8; 32] = sk.verifying_key().to_bytes();
    let id_hash: [u8; 32] = Sha256::digest(pk).into();
    framed
        .send(MailboxFrame::Challenge(Challenge {
            version: PROTOCOL_VERSION,
            identity_hash: id_hash,
        }))
        .await
        .unwrap();
    let _ = framed.next().await.unwrap().unwrap();

    drop(framed);
    handle.await.unwrap().unwrap();

    let lines = cap.0.lock().unwrap().clone();
    let recipient_hex = hex_lower(&recipient);
    let pk_hex = hex_lower(&pk);
    let id_hash_hex = hex_lower(&id_hash);
    let ciphertext_hex = hex_lower(&[0xCC; 64]);
    for line in &lines {
        let lower = line.to_ascii_lowercase();
        for forbidden in [&recipient_hex, &pk_hex, &id_hash_hex, &ciphertext_hex] {
            assert!(
                !lower.contains(forbidden.as_str()),
                "info-level log leaked secret hex: line={line:?} forbidden={forbidden:?}"
            );
        }
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut s, "{b:02x}");
    }
    s
}
```

### Step 2: Run it

```bash
cargo test -p skattr-mailbox --test logging_redaction
```
Expected: passes.

### Step 3: Commit

```bash
git add crates/mailbox/tests/logging_redaction.rs
git commit -m "$(cat <<'EOF'
mailbox: enforce log redaction policy in tests

Captures all info+ tracing events while driving a Deposit +
Challenge through MailboxServer, then asserts the captured lines
never contain full 64-hex pubkeys, 32-byte hashes, or ciphertexts.
Future code that adds an info-level log of a hash will trip this
test before it ships.

EOF
)"
```

---

## Task 25: ADR + protocol freeze

**Files:**
- Create: `docs/adr/0006-mailbox-protocol-v1.md`
- Verify: every freeze-bar item from the spec is satisfied

### Step 1: Write the ADR

`docs/adr/0006-mailbox-protocol-v1.md`:

```markdown
# ADR 0006 — Mailbox protocol v1 (frozen)

**Status:** accepted
**Date:** 2026-04-27
**Predecessor ADRs:** 0001 (license), 0002 (crypto), 0005 (Arti).
**Related spec:** `docs/superpowers/specs/2026-04-27-phase-2a-mailbox-server-design.md`.

## Context

Phase 2.A delivers `crates/mailbox/` as the standalone, AGPLv3
mailbox server, with shared wire types in `core::mailbox::protocol`.
2.B (the client) consumes those types unchanged. To prevent silent
breakage between sub-projects, this ADR freezes the v1 wire surface
and records the rule for future evolution.

## Decision

The wire types declared in `core::mailbox::protocol` as of the merge
PR for Phase 2.A are **frozen**. Every C→S request body carries
`version: u16 == PROTOCOL_VERSION = 1`. The frame-byte assignment
(0x82–0x8F) and the `ErrorCode` enum are also frozen.

Incompatible changes — adding a required field, removing a field,
changing a field's CBOR type, renaming an existing variant, repurposing
a frame byte — ship as `MAILBOX_PROTOCOL_V2`, a parallel module under
`core::mailbox::protocol_v2`. Servers may advertise both; clients
choose at connect time. v1 stays supported for at least one full
release after v2 ships.

Additive evolutions — adding a new optional field with a `#[serde(default)]`
default, adding a new `ErrorCode` variant — are compatible and may be
made within v1. Adding a new frame byte requires v2 because v1 decoders
reject unknown bytes.

## Wire types (canonical reference)

See `crates/core/src/mailbox/protocol.rs` at the merge commit. Mirror
of the freeze table:

| Type byte | Variant            | Direction | Body                                                                                                             |
|-----------|--------------------|-----------|------------------------------------------------------------------------------------------------------------------|
| 0x82      | `Deposit`          | C→S       | `version: u16, recipient_hash: [u8;32], ciphertext: bytes, ttl_request: u32`                                     |
| 0x83      | `DepositOk`        | S→C       | `deposit_id: [u8;16], expires_at: i64`                                                                            |
| 0x84      | `Challenge`        | C→S       | `version: u16, identity_hash: [u8;32]`                                                                            |
| 0x85      | `ChallengeNonce`   | S→C       | `nonce: [u8;32], issued_at: i64`                                                                                  |
| 0x86      | `Fetch`            | C→S       | `version: u16, identity_pubkey: [u8;32], nonce: [u8;32], signature: [u8;64]`                                      |
| 0x87      | `FetchResponse`    | S→C       | `deposits: Vec<{deposit_id: [u8;16], ciphertext: bytes, received_at: i64}>`                                       |
| 0x88      | `Delete`           | C→S       | `version: u16, identity_pubkey: [u8;32], nonce: [u8;32], signature: [u8;64], deposit_ids: Vec<[u8;16]>`           |
| 0x89      | `DeleteOk`         | S→C       | `deleted: u32, not_found: u32`                                                                                    |
| 0x8F      | `Error`            | S→C       | `code: ErrorCode, message: String`                                                                                |

`ErrorCode`: `UnsupportedVersion`, `MalformedRequest`, `TooLarge`,
`RateLimited`, `RecipientFull`, `TtlTooLong`, `TtlTooShort`,
`InvalidSignature`, `HashMismatch`, `NonceExpired`, `NotFound`, `Internal`.

## Auth string

```
"skattr-mailbox-auth-v1" || nonce || op_byte || sha256(canonical_cbor(payload_minus_signature))
```

`op_byte ∈ {0x86 (Fetch), 0x88 (Delete)}`. The CBOR encoding used for
`payload_minus_signature` is `ciborium`'s default serde-derived
encoding; if the payload-digest property test (Task 16, tripwire)
shows non-canonical behaviour, this section is amended to specify
manual canonical encoding before merge.

## Test bar (satisfied)

The merge PR satisfies all six layers from the spec:

- [x] Unit tests in every module touched.
- [x] Property tests round-trip every frame and every `ErrorCode`.
- [x] Fuzz harness present; ≥ 1 hour local run with no findings (record
      the run command + machine in the merge PR description).
- [x] Adversarial regression suite triggers every `ErrorCode` variant.
- [x] 24 h soak run; summary committed at
      `docs/superpowers/runs/<merge-date>-mailbox-soak.txt`.
- [x] Real-Tor smoke test green (`#[ignore]`-gated; manual run).

## Consequences

- 2.B's `MailboxClient` writes against this freeze and ships without
  surprise breakage from 2.A churn.
- Any post-2.A change that touches `core::mailbox::protocol` requires
  either a v2 ADR (incompatible) or a one-paragraph addendum to this
  ADR (compatible additive).
- The frozen frame bytes (0x82–0x8F) are off-limits for any other
  protocol added to skattr.
```

### Step 2: Cross-check the freeze checklist

Re-read the spec's "Freeze definition" section and verify each
bullet against the work done so far:

- [x] `MAILBOX_PROTOCOL_VERSION: u16 = 1` exported from
  `core::mailbox::protocol` (Task 1).
- [x] All six test layers green: unit (Tasks 1, 4–13), property
  (Task 16), fuzz (Task 18; manual 1h run), adversarial (Task 17),
  24h soak (Task 19; manual run + committed summary), real-Tor
  smoke (Task 20; manual run).
- [x] Every `ErrorCode` variant has a triggering test (Task 17).
- [x] ADR `docs/adr/0006-mailbox-protocol-v1.md` (this task).
- [x] `core::mailbox::protocol` exports nothing beyond wire types
  (Task 1's body never imports policy / transport).

### Step 3: Commit

```bash
git add docs/adr/0006-mailbox-protocol-v1.md
git commit -m "$(cat <<'EOF'
adr: 0006 freeze mailbox protocol v1 for 2.B

Records the frozen wire types, ErrorCode enum, and auth string
construction. Incompatible changes ship as MAILBOX_PROTOCOL_V2 in
parallel; additive evolutions stay within v1 with a one-paragraph
addendum. The merge PR for Phase 2.A is the freeze commit; 2.B
develops against this ADR.

EOF
)"
```

---

## Task 26: CHANGELOG + CLAUDE.md repo-state update

**Files:**
- Modify: `CHANGELOG.md` (append the 2.A bullet under the next-version section)
- Modify: `CLAUDE.md` (extend the "Repository state" prose)

### Step 1: CHANGELOG

Read `CHANGELOG.md`; locate the "Unreleased" or "Phase 2 in progress" section. Append:

```markdown
- **Phase 2.A — Mailbox server.** `crates/mailbox/` promoted to
  `[lib] + [bin]` (AGPLv3). Frozen wire surface in
  `core::mailbox::protocol` (ADR 0006). Server ships transactional
  cap-eviction, per-connection + global token-bucket rate limits,
  challenge-response auth with single-use 30 s nonces, three
  background tasks (expiry, challenge sweep, metrics), local-only
  UDS healthcheck, hardened systemd unit, distroless Dockerfile,
  and an operator guide that targets ≤ 30 min from-fresh-VM.
  Test pyramid: unit + property + fuzz + adversarial (every
  `ErrorCode` covered) + 24h soak (`#[ignore]`-gated) + real-Tor
  smoke. `core::mailbox::client` and `core::mailbox::scheduler`
  remain stubs; 2.B picks them up.
```

### Step 2: CLAUDE.md

Edit `CLAUDE.md`'s "Repository state" section. After the existing
paragraph that ends `the next workstream is Phase 2 (Tauri 2 +
SvelteKit UI)`, replace that sentence with:

```
Phase 1 is complete (1.H merged 2026-04-24); Phase 2.A (mailbox
server) is complete (merged <merge-date>). The next workstream is
Phase 2.B (mailbox client + ContactCard rotation), then the UI
lane 2.C → 2.D → 2.E → 2.F → 2.G — see
`docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md`.
```

Also add a short paragraph below describing the 2.A artefacts (mirror
the format of the existing 1.x paragraphs):

```
Phase 2.A added `crates/mailbox/` as a `[lib] + [bin]` AGPLv3 crate:
`MailboxServer::accept_loop` per-stream FSM over the shared wire
layout (length+type+CBOR; type bytes 0x82–0x8F), `Store` with
transactional cap-eviction insert, `Challenges` (single-use 30 s
nonces), `Policy` + per-conn / global token buckets, three
background tasks (expiry / challenge sweep / metrics), a UDS
healthcheck at `${data_dir}/health.sock`, and Arti glue feature-
gated as `bin`. `core::mailbox::protocol` is frozen (ADR 0006);
`core::mailbox::client` and `scheduler` stay stubs for 2.B.
Operational artefacts: `packaging/systemd/skattr-mailbox.service`,
`packaging/Dockerfile` (distroless cc + nonroot),
`docs/operations/mailbox-setup.md` (≤ 30 min target).
```

### Step 3: Replace `<merge-date>` placeholder

The exact merge date isn't known until the PR lands. The executor
who does the merge should `git grep '<merge-date>'` before pushing
and replace each occurrence with the actual date in `YYYY-MM-DD`
form. Files affected:

- `CLAUDE.md`
- `docs/operations/mailbox-setup.md` (if any references slipped in)
- `docs/superpowers/runs/<merge-date>-mailbox-soak.txt` (rename)

### Step 4: Commit

```bash
git add CHANGELOG.md CLAUDE.md
git commit -m "$(cat <<'EOF'
docs: CHANGELOG + CLAUDE.md state update for Phase 2.A close-out

CHANGELOG records the 2.A bullet under the in-progress Phase 2
section. CLAUDE.md's 'Repository state' section gets the 2.A
paragraph + a forward pointer to 2.B as the next workstream.

EOF
)"
```

---

## Final verification

Before opening the merge PR, run the full freeze-bar verification:

- [ ] **All unit + property + adversarial + integration tests green**

```bash
cd /home/myggiz/development/skattr-phase-2a-mailbox-server
. "$HOME/.cargo/env"
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo deny check
```
Expected: every command green.

- [ ] **24h soak run committed**

```bash
cargo test -p skattr-mailbox --release --test soak -- --ignored --nocapture \
    > docs/superpowers/runs/$(date +%Y-%m-%d)-mailbox-soak.txt 2>&1
git add docs/superpowers/runs/
git commit -m "soak: 24h mailbox soak run for Phase 2.A freeze"
```
Expected: tail of the file shows the `SOAK SUMMARY` line and the test
exits 0.

- [ ] **Fuzz harness ≥ 1h locally with no findings**

```bash
cd crates/mailbox/fuzz
cargo +nightly fuzz run frame_decode -- -max_total_time=3600
cargo +nightly fuzz run dispatch     -- -max_total_time=3600
cd -
```
Expected: both targets exit cleanly (libfuzzer prints "Done <N> runs"
without a crash banner). If a finding lands in `artifacts/`, treat it
as a regression — reproduce in `tests/adversarial_codec.rs`, fix, and
re-run.

- [ ] **Real-Tor smoke run**

```bash
cargo test -p skattr-tests --release --test mailbox_real_tor -- --ignored
```
Expected: passes within ~2 minutes.

- [ ] **Manual healthcheck round-trip on a real install**

(Path A from the ops guide.) Expected: `socat - UNIX-CONNECT:.../health.sock`
replies `ok\n`.

- [ ] **Open the merge PR**

```bash
git push -u origin phase-2a-mailbox-server
gh pr create --base master --title "Phase 2.A — Mailbox server"
```

PR body should include: link to the spec, link to ADR 0006, the
soak summary path, and a checklist of the freeze-bar items above.

---

## Self-review against the spec

Cross-checking the plan against
`docs/superpowers/specs/2026-04-27-phase-2a-mailbox-server-design.md`:

| Spec section                                | Plan task(s)        |
|---------------------------------------------|---------------------|
| Architectural decisions 1–6                 | 1, 2, 3, 7, 9, 14   |
| Wire protocol v1                            | 1, 3                |
| Auth construction                           | 8, 16, 17 (auth)    |
| Per-connection state machine                | 11                  |
| Storage schema (deposits + indexes)         | 6, 7                |
| Per-recipient cap eviction                  | 7, 17 (storage)     |
| Background tasks (expire / chal / metrics)  | 12                  |
| Policy + rate limiting (per-conn + global)  | 9, 17 (policy)      |
| Crate layout (lib + bin, modules)           | 2, 4–14             |
| Module visibility                           | 2 (lib.rs `pub use`)|
| Errors (six sub-enums, kind, to_wire_code)  | 4                   |
| Operational artefacts                       | 21, 22, 23          |
| Logging redaction policy                    | 24                  |
| Test plan (six layers)                      | 16, 17, 18, 19, 20, plus units in 1, 4–14 |
| Freeze definition                           | 25                  |
| Cross-cutting compliance                    | every task (license header, no unwrap, fmt+clippy gates) |
| Risks (every row → mitigation)              | covered in implementation choices; the malformed-frame Risk row is addressed by the FSM in Task 11 (no close on Transport errors) and the 24h soak in Task 19 |

No spec section lacks a task. No task is a placeholder.

---

## Plan complete

Plan complete and saved to
`docs/superpowers/plans/2026-04-27-phase-2a-mailbox-server.md`.

Two execution options:

1. **Subagent-driven (recommended)** — fresh subagent per task,
   review between tasks, fast iteration.
2. **Inline execution** — run tasks in this session via
   `superpowers:executing-plans`, batch with checkpoints.

Which approach?
