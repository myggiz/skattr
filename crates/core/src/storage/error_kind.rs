// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Typed storage-layer error kinds. Replaces free-form `String` payloads
//! so `CoreError::kind()` can project via a structural match instead of
//! `str::contains`.

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StorageErrorKind {
    /// FTS5 MATCH parse/syntax error. The inner string is the raw
    /// sqlite message for logs; the projected `DaemonErrorKind` is
    /// `SearchSyntax`.
    #[error("fts5 syntax error: {0}")]
    FtsSyntax(String),

    /// `(group_id, envelope_id)` UNIQUE violation. Send path maps this
    /// to `SendStatus::Delivered` (idempotent retry); receive path
    /// never sees it thanks to the `seen_messages` pre-check.
    #[error("duplicate message")]
    DuplicateMessage,

    /// Everything else — catch-all escape hatch during the Phase 1.H
    /// refactor. Prefer adding a typed variant over populating this.
    #[error("storage: {0}")]
    Other(String),
}
