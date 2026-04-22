// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! MLS (RFC 9420) integration.
//!
//! We use OpenMLS with a single locked ciphersuite (see [`ciphersuite`]).
//! Internally the module wraps `openmls::group::MlsGroup` and persists
//! its opaque state blobs through [`crate::storage::groups::MlsGroupRepo`].
//! Group lifecycle is driven by an explicit [`state_machine::GroupState`]
//! so that inconsistent states become unrepresentable rather than silently
//! corrupting MLS internals.

pub(crate) mod ciphersuite;
pub(crate) mod group;
pub(crate) mod key_package;
pub(crate) mod provider;
pub(crate) mod state_machine;

#[cfg(not(feature = "test-harness"))]
pub(crate) use ciphersuite::CIPHERSUITE;
#[cfg(not(feature = "test-harness"))]
pub(crate) use group::{CommitBytes, Group, GroupId, WelcomeBytes};
#[cfg(not(feature = "test-harness"))]
pub(crate) use key_package::KeyPackage;
#[cfg(not(feature = "test-harness"))]
pub(crate) use state_machine::GroupState;

#[cfg(feature = "test-harness")]
pub use ciphersuite::CIPHERSUITE;
#[cfg(feature = "test-harness")]
pub use group::{CommitBytes, Group, GroupId, WelcomeBytes};
#[cfg(feature = "test-harness")]
pub use key_package::KeyPackage;
#[cfg(feature = "test-harness")]
pub use provider::MlsProvider;
#[cfg(feature = "test-harness")]
pub use state_machine::GroupState;
