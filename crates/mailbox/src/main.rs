// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version. See LICENSE-AGPL3.

//! `skattr-mailbox` binary entry point.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use skattr_mailbox::{
    arti::{run_onion, DEFAULT_HS_NICKNAME},
    background::{spawn_challenge_sweep, spawn_expire_sweep, spawn_metrics_tick},
    server::MailboxServer,
    store::Store,
    MailboxConfig,
};
#[cfg(unix)]
use skattr_mailbox::health;

/// Command-line arguments.
#[derive(Debug, Parser)]
#[command(version, about = "Skattr mailbox server", long_about = None)]
struct Args {
    /// Path to `mailbox.toml`.
    #[arg(short, long)]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "skattr_mailbox=info,warn".into()),
        )
        // Daemons log to stderr, not stdout — keeps stdout free for
        // structured output that consumers can pipe, and matches how
        // systemd's `StandardError=journal` expects diagnostic events.
        .with_writer(std::io::stderr)
        // Disable ANSI colour codes — this is a server, not a TTY
        // application; journald and the real-Tor smoke test parse
        // plain text. Operators tailing logs locally can pipe through
        // `bat -l log` if they want highlighting.
        .with_ansi(false)
        .init();

    let args = Args::parse();
    let cfg = MailboxConfig::load(&args.config)
        .with_context(|| format!("load {}", args.config.display()))?;

    std::fs::create_dir_all(&cfg.server.data_dir).context("create data_dir")?;

    let store = Arc::new(Store::open(&cfg.server.resolved_storage_path()).context("open store")?);

    let server = Arc::new(MailboxServer::new(store.clone(), cfg.policy.clone()));

    let cancel = CancellationToken::new();
    let _expire = spawn_expire_sweep(store.clone(), cancel.clone());
    let _chal = spawn_challenge_sweep(server.challenges(), cancel.clone());
    let _metrics = spawn_metrics_tick(store.clone(), cancel.clone());

    #[cfg(unix)]
    let _health = health::spawn(
        cfg.server.resolved_health_socket(),
        store.clone(),
        cancel.clone(),
    )
    .await
    .context("health UDS")?;

    let _onion = run_onion(
        &cfg.server.resolved_arti_state_dir(),
        DEFAULT_HS_NICKNAME,
        server.clone(),
    )
    .await
    .context("arti")?;

    // Notify systemd we're up (best-effort; ignore on non-systemd hosts).
    // sd-notify uses Unix domain sockets; only available on Unix platforms.
    #[cfg(unix)]
    let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]);

    tokio::signal::ctrl_c().await.ok();
    tracing::info!("shutdown signal received");
    cancel.cancel();
    #[cfg(unix)]
    let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Stopping]);
    Ok(())
}
