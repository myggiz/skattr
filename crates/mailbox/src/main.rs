// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version. See LICENSE-AGPL3.

//! Skattr mailbox server binary.
//!
//! Serves the DEPOSIT / CHALLENGE / FETCH / DELETE wire protocol over
//! a v3 Tor onion service. Semi-trusted infrastructure: sees per-
//! identity-hash polling patterns but no message plaintexts.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;

mod auth;
mod config;
mod server;
mod store;

use config::MailboxConfig;

/// Command-line arguments.
#[derive(Debug, Parser)]
#[command(version, about = "Skattr mailbox server", long_about = None)]
struct Args {
    /// Path to `mailbox.toml`.
    #[arg(short, long)]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "skattr_mailbox=info,warn".into()),
        )
        .init();

    let args = Args::parse();
    let config = match args.config.as_deref() {
        Some(path) => MailboxConfig::load(path)
            .with_context(|| format!("load config from {}", path.display()))?,
        None => MailboxConfig::defaults().context("build default config")?,
    };

    tracing::info!(?config.storage_path, "starting mailbox");
    server::run(config).await?;
    Ok(())
}
