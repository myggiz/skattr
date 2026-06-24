// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Typed storage-layer error kinds. Replaces free-form `String` payloads
//! so `CoreError::kind()` can project via a structural match instead of
//! `str::contains`.

use thiserror::Error;

#[derive(Debug, Error, ts_rs::TS)]
#[non_exhaustive]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
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

    /// The DB `schema_version` is newer than this binary knows about — an
    /// older binary opened a DB written by a newer one. Refuse rather than
    /// silently operating on an unknown schema. Projects to
    /// `DaemonErrorKind::StorageError` (no new wire variant).
    #[error("schema too new: db at version {found}, this binary knows up to {max_known}")]
    SchemaTooNew { found: u32, max_known: u32 },
}
