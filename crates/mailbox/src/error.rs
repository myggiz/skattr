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
pub enum AuthErrorKind {
    Placeholder,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigErrorKind {
    Placeholder,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyErrorKind {
    Placeholder,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageErrorKind {
    Placeholder,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportErrorKind {
    Placeholder,
}
