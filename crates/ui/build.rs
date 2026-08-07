// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Cargo build script — invokes `tauri_build` to bake the Tauri 2
//! context (icons, capabilities, frontend dist path) at compile time.

fn main() {
    tauri_build::build();
}
