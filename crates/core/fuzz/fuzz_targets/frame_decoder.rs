// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Fuzz target: `FrameCodec::decode` must never panic on arbitrary bytes.

#![no_main]

use libfuzzer_sys::fuzz_target;
use skattr_core::test_exports::FrameCodec;
use tokio_util::codec::Decoder;

fuzz_target!(|data: &[u8]| {
    let mut codec = FrameCodec::new();
    let mut buf = bytes::BytesMut::from(data);
    // Drain whatever frames parse out of the buffer. Errors are fine;
    // panics or aborts are bugs.
    loop {
        match codec.decode(&mut buf) {
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(_) => break,
        }
    }
});
