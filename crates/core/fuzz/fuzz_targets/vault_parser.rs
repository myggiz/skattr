// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Fuzz harness for the on-disk vault parser.
//!
//! The invariant we care about: Vault::open must never panic on any
//! input. It is allowed to return an error.

#![no_main]

use libfuzzer_sys::fuzz_target;
use std::io::Write;

fuzz_target!(|data: &[u8]| {
    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    tmp.as_file()
        .write_all(data)
        .expect("write tempfile");
    let path = tmp.path();
    // We don't care whether open succeeds — only that it never panics.
    let _ = skattr_core::identity::Vault::open(path, "passphrase");
});
