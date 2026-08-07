// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Convenience re-exports for downstream consumers.
//!
//! Downstream crates should prefer `use skattr_core::prelude::*;` over
//! deep imports. The contents of this module are considered part of the
//! public API.

pub use crate::contact::Contact;
pub use crate::daemon::{Command, Daemon, Event};
pub use crate::envelope::{Envelope, Kind};
pub use crate::error::{CoreError, Result};
pub use crate::identity::{IdentityKey, Mnemonic, PublicKey, Seed, Signature};
pub use crate::invite::InviteLink;
