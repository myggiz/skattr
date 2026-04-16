// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! MLS (RFC 9420) integration.
//!
//! We use OpenMLS with a single, locked ciphersuite (see [`ciphersuite`]).
//! Internally the module wraps `openmls::MlsGroup` and persists its
//! opaque state blobs through [`crate::storage::groups`]. Group lifecycle
//! is driven by an explicit [`state_machine::GroupState`] so that
//! inconsistent states become unrepresentable rather than silently
//! silently corrupting MLS internals.

pub(crate) mod ciphersuite;
pub(crate) mod commit;
pub(crate) mod group;
pub(crate) mod keystore;
pub(crate) mod state_machine;
pub(crate) mod welcome;

pub(crate) use ciphersuite::CIPHERSUITE;
pub(crate) use group::Group;
pub(crate) use state_machine::GroupState;
