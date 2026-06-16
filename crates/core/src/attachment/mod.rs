// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Attachment core (Phase 3.A): manifest format, chunker, reassembler,
//! metadata stripping. Pure/local — no transport. The manifest rides inside
//! MLS via `envelope::kinds::Kind::File`.

pub(crate) mod chunker;
pub(crate) mod error_kind;
pub(crate) mod manifest;

pub(crate) use error_kind::AttachmentErrorKind;
pub(crate) use manifest::{AttachmentManifest, ChunkRef};

/// Plaintext bytes per chunk (256 KiB). Sits under the mailbox 1 MiB
/// `max_deposit_size` (with AEAD + framing headroom) so a 3.C offline chunk
/// fits one `Deposit`.
pub(crate) const CHUNK_SIZE: usize = 262_144;

/// Maximum total plaintext attachment size (100 MiB), rejected up front.
pub(crate) const MAX_ATTACHMENT_BYTES: u64 = 100 * 1024 * 1024;

/// Current manifest version. An unknown version is rejected on decode.
pub(crate) const MANIFEST_VERSION: u8 = 1;
