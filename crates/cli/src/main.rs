// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! `skattr` — the command-line client.
//!
//! Every subcommand is fully implemented. `daemon` runs the daemon in-process
//! via `skattr_core::Daemon::run`; all other subcommands connect to a running
//! daemon over local IPC (Unix socket / Windows named pipe) and issue a single
//! `Command`, rendering the `CommandResult` as text or, where supported, JSON.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use skattr_core::identity::{IdentityKey, Mnemonic, Seed, Vault};

/// `skattr` command-line interface.
#[derive(Debug, Parser)]
#[command(
    name = "skattr",
    version,
    about = "Skattr: metadata-resistant encrypted messaging over Tor.",
    long_about = None,
)]
struct Cli {
    /// Path to config file. Defaults to `~/.config/skattr/config.toml`.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Override the data directory (vault + daemon state). Defaults to
    /// the XDG data dir.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    /// JSON output (for scripting).
    #[arg(long, global = true)]
    json: bool,

    /// Read the vault passphrase from FILE (one passphrase, optional
    /// trailing newline). Overridden by `$SKATTR_PASSPHRASE_FILE`.
    #[arg(long, value_name = "FILE", global = true)]
    passphrase_file: Option<PathBuf>,

    /// Path to the daemon's IPC endpoint. On Unix this is the AF_UNIX
    /// socket file; on Windows this is the daemon's discovery file
    /// (which contains the named-pipe name). Overrides $SKATTR_SOCKET
    /// and the platform default.
    #[arg(long, global = true, value_name = "PATH")]
    socket: Option<PathBuf>,

    /// Subcommand.
    #[command(subcommand)]
    cmd: Command,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
enum Command {
    /// Generate a new identity and seed phrase.
    Init,
    /// Restore an identity from a BIP39 seed phrase.
    Restore {
        /// Space-separated seed phrase (quoted).
        seed: String,
    },
    /// Export a portable backup of identity + storage + HS key to FILE.
    Backup {
        /// Destination archive path.
        file: PathBuf,
    },
    /// Restore identity + storage + HS key from a backup archive.
    RestoreBackup {
        /// BIP39 mnemonic (quoted space-separated words).
        seed: String,
        /// Source archive path.
        file: PathBuf,
    },
    /// Start the daemon (Tor bootstrap + onion publish + accept loop).
    Daemon {
        /// Detach to a background process after startup.
        #[arg(long)]
        detach: bool,
        /// Run a one-shot smoke test (init throwaway vault, boot,
        /// wait for Tor::Ready, exit 0). For CI release smoke; not
        /// for real use.
        #[arg(long)]
        smoke_test: bool,
        /// Smoke-only: timeout for Tor::Ready. Ignored without
        /// `--smoke-test`.
        #[arg(long, value_name = "SECS", default_value_t = 240)]
        smoke_timeout_secs: u64,
    },
    /// Generate a single-use invite link.
    Invite {
        /// Render as a QR code (requires the `qr` feature, enabled by default).
        #[arg(long)]
        qr: bool,
    },
    /// Consume an invite link to add a contact.
    Add {
        /// `skattr://invite/v1#…` URL.
        link: String,
    },
    /// List known contacts.
    Contacts,
    /// Send a text message to a contact.
    Send {
        /// Contact identifier (display name or hex prefix of identity pubkey).
        contact: String,
        /// Message body.
        text: String,
        /// Exit with status 8 if the daemon reports Queued (no ACK
        /// within the inline wait). Without this flag the CLI prints
        /// "queued" and exits 0.
        #[arg(long)]
        fail_on_timeout: bool,
    },
    /// Interactive chat: stream messages from a contact and send lines
    /// typed on stdin.
    Chat {
        /// Contact identifier (prefix or nickname).
        contact: String,
    },
    /// Tail messages. Without --follow: dump most recent N and exit.
    Tail {
        /// Only from this contact (prefix or nickname).
        contact: Option<String>,
        /// Max rows to dump before exiting (only affects non-follow mode).
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Follow: after dumping, stream new MessageReceived events.
        #[arg(long)]
        follow: bool,
    },
    /// Export a contact's full message history.
    Export {
        /// Contact name or hex pubkey prefix.
        contact: String,
        /// Output format: `json` (default) or `text`.
        #[arg(long, default_value = "json")]
        format: String,
        /// Output file path. Refuses to overwrite an existing file.
        #[arg(long)]
        output: std::path::PathBuf,
    },
    /// Delete history rows. Pass exactly one of --before or --keep-last.
    Prune {
        /// Limit to one contact (name or hex prefix).
        #[arg(long)]
        contact: Option<String>,
        /// Delete rows older than this RFC3339 timestamp.
        #[arg(long)]
        before: Option<String>,
        /// Keep only the N newest rows in the contact's group.
        #[arg(long)]
        keep_last: Option<u64>,
    },
    /// Send a file attachment to a contact.
    SendFile {
        /// Contact identifier (display name or hex prefix of identity pubkey).
        contact: String,
        /// Path to the local file to send.
        path: String,
    },
    /// Save a received attachment to a file.
    SaveAttachment {
        /// Attachment id, or any unique prefix of it (see `tail`).
        id: String,
        /// Destination path. Relative paths resolve against the current
        /// directory.
        dest: String,
    },
    /// Remove a contact. A pending/unconnected contact is wiped completely
    /// (so a fresh invite can be added); a connected contact is archived.
    Remove {
        /// Contact identifier (display name or hex prefix of identity pubkey).
        contact: String,
    },
    /// Full-text search over message history.
    Search {
        /// Query — free-form, tokenize-and-AND on the daemon side.
        query: String,
        /// Limit search to one contact (name or hex prefix).
        #[arg(long)]
        contact: Option<String>,
        /// Maximum hits to return.
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Skip this many hits.
        #[arg(long, default_value_t = 0)]
        offset: u32,
        /// Order by id DESC instead of BM25.
        #[arg(long)]
        newest_first: bool,
        /// Emit raw JSON instead of the human-readable form.
        #[arg(long)]
        json: bool,
    },
}

/// Resolve the IPC endpoint path with precedence flag > env > shared default.
fn resolve_socket_path(flag: Option<&std::path::Path>) -> Result<PathBuf> {
    if let Some(p) = flag {
        return Ok(p.to_path_buf());
    }
    if let Some(env) = std::env::var_os("SKATTR_SOCKET") {
        return Ok(PathBuf::from(env));
    }
    Ok(skattr_core::daemon::default_ipc_endpoint()?)
}

/// Connect or print a helpful error and exit with code 3. Returns a
/// live `IpcClient` on success.
async fn connect_or_exit(
    sock_flag: Option<&std::path::Path>,
) -> Result<skattr_core::daemon::IpcClient<skattr_core::daemon::ipc::IpcStream>> {
    let path = resolve_socket_path(sock_flag)?;
    match skattr_core::daemon::IpcClient::connect(&path).await {
        Ok(c) => Ok(c),
        Err(skattr_core::daemon::IpcClientError::DaemonNotRunning) => {
            eprintln!("skattr daemon is not running.");
            eprintln!("Start it with:  skattr daemon");
            std::process::exit(3);
        }
        Err(e) => Err(anyhow::anyhow!("ipc: {e}")),
    }
}

/// Translate a wire `IpcClientError` into a one-line human-readable message
/// plus a stable exit code. Called from every command's error branch.
fn exit_on_ipc_error(err: skattr_core::daemon::IpcClientError) -> ! {
    use skattr_core::daemon::error_kind::DaemonErrorKind;
    use skattr_core::daemon::ipc::wire::IpcError;
    use skattr_core::daemon::IpcClientError;
    match err {
        IpcClientError::Server(IpcError::AuthDenied) => {
            eprintln!("ipc: auth denied (peer-cred mismatch)");
            std::process::exit(4);
        }
        IpcClientError::Server(IpcError::Daemon(k)) => match k {
            DaemonErrorKind::ContactNotFound => {
                eprintln!("contact not found");
                std::process::exit(6);
            }
            DaemonErrorKind::ContactAmbiguous { matches } => {
                eprintln!("contact prefix is ambiguous ({matches} matches)");
                std::process::exit(6);
            }
            DaemonErrorKind::InviteExpired => {
                eprintln!("invite expired");
                std::process::exit(7);
            }
            DaemonErrorKind::InviteConsumed => {
                eprintln!("invite already consumed");
                std::process::exit(7);
            }
            DaemonErrorKind::InviteSignatureInvalid => {
                eprintln!("invite signature invalid");
                std::process::exit(7);
            }
            DaemonErrorKind::GroupCorrupt => {
                eprintln!("mls group state corrupt");
                std::process::exit(1);
            }
            DaemonErrorKind::DeliveryTimeout => {
                eprintln!("delivery timed out");
                std::process::exit(8);
            }
            DaemonErrorKind::TorNotReady => {
                eprintln!("Tor still bootstrapping; retry shortly");
                std::process::exit(1);
            }
            DaemonErrorKind::StorageError => {
                eprintln!("storage error (see daemon logs)");
                std::process::exit(1);
            }
            // Task 24 refines this message and may remap the exit code.
            DaemonErrorKind::SearchSyntax => {
                eprintln!("search query rejected by FTS5 engine");
                std::process::exit(6);
            }
            DaemonErrorKind::InvalidArgument { message } => {
                eprintln!("argument error: {message}");
                std::process::exit(2);
            }
            DaemonErrorKind::Unauthorized => {
                eprintln!("error: authentication failed");
                std::process::exit(1);
            }
        },
        IpcClientError::Server(other) => {
            eprintln!("ipc: server error: {other:?}");
            std::process::exit(1);
        }
        other => {
            eprintln!("ipc: {other}");
            std::process::exit(1);
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    use skattr_core::daemon::logs::{LogSink, RingBufferLayer};
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

    // Build a LogSink that is shared between the tracing layer and the
    // daemon handle. The RingBufferLayer funnels tracing events into the
    // ring buffer; the daemon log-tap task re-emits them onto the event bus.
    let log_sink = LogSink::new();
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "skattr_cli=info,skattr_core=info,warn".into()),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(RingBufferLayer::new(log_sink.clone()))
        .init();

    let cli = Cli::parse();
    let passphrase_file = cli_passphrase_file_or_env(&cli);
    let socket = cli.socket.clone();
    let json = cli.json;
    match cli.cmd {
        Command::Init => init(cli.data_dir.as_deref()).await,
        Command::Restore { seed } => restore(&seed, cli.data_dir.as_deref()).await,
        Command::Backup { file } => backup(&file, cli.data_dir.as_deref()).await,
        Command::RestoreBackup { seed, file } => {
            restore_backup(&seed, &file, cli.data_dir.as_deref()).await
        }
        Command::Daemon {
            detach,
            smoke_test,
            smoke_timeout_secs,
        } => {
            if smoke_test {
                cli_smoke(cli.data_dir.as_deref(), smoke_timeout_secs).await
            } else {
                daemon(detach, cli.data_dir.as_deref(), passphrase_file, log_sink).await
            }
        }
        Command::Invite { qr } => invite(qr, socket.as_deref(), json).await,
        Command::Add { link } => add(&link, socket.as_deref(), json).await,
        Command::Contacts => contacts(socket.as_deref(), json).await,
        Command::Send {
            contact,
            text,
            fail_on_timeout,
        } => send(&contact, &text, fail_on_timeout, socket.as_deref(), json).await,
        Command::SendFile { contact, path } => {
            send_file_cmd(&contact, &path, socket.as_deref()).await
        }
        Command::Tail {
            contact,
            limit,
            follow,
        } => tail(contact.as_deref(), limit, follow, socket.as_deref(), json).await,
        Command::Chat { contact } => chat(&contact, socket.as_deref()).await,
        Command::Search {
            query,
            contact,
            limit,
            offset,
            newest_first,
            json: cmd_json,
        } => {
            search(
                contact.as_deref(),
                query,
                limit,
                offset,
                newest_first,
                cmd_json || json,
                socket.as_deref(),
            )
            .await
        }
        Command::Export {
            contact,
            format,
            output,
        } => export(&contact, format, output, socket.as_deref()).await,
        Command::Prune {
            contact,
            before,
            keep_last,
        } => prune(contact.as_deref(), before, keep_last, socket.as_deref()).await,
        Command::SaveAttachment { id, dest } => {
            save_attachment(&id, &dest, socket.as_deref(), json).await
        }
        Command::Remove { contact } => remove(&contact, socket.as_deref(), json).await,
    }
}

fn effective_data_dir(override_dir: Option<&std::path::Path>) -> Result<PathBuf> {
    if let Some(d) = override_dir {
        return Ok(d.to_path_buf());
    }
    Ok(skattr_core::daemon::data_dir()?)
}

async fn init(data_dir_override: Option<&std::path::Path>) -> Result<()> {
    let data_dir = effective_data_dir(data_dir_override)?;
    std::fs::create_dir_all(&data_dir)?;
    // Carry any pre-existing identity from a legacy location into the
    // canonical data dir before the vault guard (fail-loud: abort rather
    // than silently minting a fresh identity and orphaning the real one).
    skattr_core::daemon::migrate_legacy_into(&data_dir)
        .map_err(|e| anyhow::anyhow!("data migration failed: {e}"))?;
    let vault_path = data_dir.join("identity.vault");

    if vault_path.exists() {
        anyhow::bail!(
            "identity vault already exists at {}; refusing to overwrite",
            vault_path.display()
        );
    }

    let pw1 = read_passphrase("Choose a passphrase: ")?;
    let pw2 = read_passphrase("Confirm passphrase: ")?;
    if *pw1 != *pw2 {
        anyhow::bail!("passphrases do not match");
    }

    let seed = Seed::generate()?;
    let identity = IdentityKey::from_seed(&seed)?;
    let pubkey_hex = identity.public().to_hex();

    Vault::create(&vault_path, identity, pw1.as_str())?;

    let mnemonic = seed.to_mnemonic()?;
    let phrase = mnemonic.words().join(" ");

    println!();
    println!("Identity created.");
    println!("  public key: {pubkey_hex}");
    println!("  vault:      {}", vault_path.display());
    println!();
    println!("RECOVERY SEED PHRASE — write this down, store it offline:");
    println!();
    println!("  {phrase}");
    println!();
    println!("If you lose this phrase AND the vault passphrase, your identity is");
    println!("unrecoverable. We cannot reset it for you.");
    Ok(())
}

/// Source the daemon passphrase can come from.
#[derive(Debug, Clone)]
enum PassphraseSource {
    /// Prompt on `/dev/tty` with echo off.
    InteractiveTty(String),
    /// Read from a file at `path`; trim exactly one trailing newline.
    File(std::path::PathBuf),
}

fn read_passphrase(prompt: &str) -> Result<zeroize::Zeroizing<String>> {
    read_passphrase_from_source(PassphraseSource::InteractiveTty(prompt.to_string()))
}

fn read_passphrase_from_source(source: PassphraseSource) -> Result<zeroize::Zeroizing<String>> {
    match source {
        PassphraseSource::InteractiveTty(prompt) => {
            let raw = rpassword::prompt_password(prompt)
                .map_err(|e| anyhow::anyhow!("read passphrase: {e}"))?;
            Ok(zeroize::Zeroizing::new(raw))
        }
        PassphraseSource::File(path) => read_passphrase_from_file(&path),
    }
}

fn read_passphrase_from_file(path: &std::path::Path) -> Result<zeroize::Zeroizing<String>> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("read passphrase from {}: {e}", path.display()))?;
    // Trim exactly one trailing newline (CRLF or LF), preserve internal newlines.
    let mut pw = zeroize::Zeroizing::new(raw);
    if pw.ends_with('\n') {
        pw.pop();
        if pw.ends_with('\r') {
            pw.pop();
        }
    }
    Ok(pw)
}

fn cli_passphrase_file_or_env(cli: &Cli) -> Option<PathBuf> {
    if let Some(p) = &cli.passphrase_file {
        return Some(p.clone());
    }
    std::env::var_os("SKATTR_PASSPHRASE_FILE").map(PathBuf::from)
}

async fn restore(seed_phrase: &str, data_dir_override: Option<&std::path::Path>) -> Result<()> {
    use anyhow::Context;

    let data_dir = effective_data_dir(data_dir_override)?;
    std::fs::create_dir_all(&data_dir)?;
    // Carry any pre-existing identity from a legacy location into the
    // canonical data dir before the vault guard (fail-loud: abort rather
    // than silently overwriting migrated history with a seed-restore).
    skattr_core::daemon::migrate_legacy_into(&data_dir)
        .map_err(|e| anyhow::anyhow!("data migration failed: {e}"))?;
    let vault_path = data_dir.join("identity.vault");

    if vault_path.exists() {
        anyhow::bail!(
            "identity vault already exists at {}; refusing to overwrite",
            vault_path.display()
        );
    }

    // Parse the phrase through a Zeroizing copy so our local String
    // does not linger. (The clap-owned argv slice is still exposed via
    // /proc/<pid>/cmdline — users should avoid passing secrets on the
    // command line; this is documented in the README.)
    let mnemonic = {
        let owned = zeroize::Zeroizing::new(seed_phrase.to_string());
        Mnemonic::parse(&owned)
    };
    let seed = Seed::from_mnemonic(&mnemonic)
        .context("invalid seed phrase (check word list and checksum)")?;
    let identity = IdentityKey::from_seed(&seed)?;
    let pubkey_hex = identity.public().to_hex();

    let pw1 = read_passphrase("Choose a new vault passphrase: ")?;
    let pw2 = read_passphrase("Confirm passphrase: ")?;
    if *pw1 != *pw2 {
        anyhow::bail!("passphrases do not match");
    }

    Vault::create(&vault_path, identity, pw1.as_str())?;

    println!();
    println!("Identity restored.");
    println!("  public key: {pubkey_hex}");
    println!("  vault:      {}", vault_path.display());
    Ok(())
}

async fn backup(
    out_file: &std::path::Path,
    data_dir_override: Option<&std::path::Path>,
) -> Result<()> {
    use skattr_core::daemon::backup::export_backup;
    use skattr_core::identity::derive::derive_storage_seed;

    let data_dir = effective_data_dir(data_dir_override)?;
    let vault_path = data_dir.join("identity.vault");
    if !vault_path.exists() {
        anyhow::bail!(
            "no identity vault at {}; nothing to back up",
            vault_path.display()
        );
    }

    let pw = read_passphrase("Vault passphrase: ")?;
    let (_vault, identity) = Vault::open(&vault_path, pw.as_str())?;
    let seed = derive_storage_seed(identity)?;

    export_backup(&data_dir, out_file, &seed)?;
    println!("Backup written to {}", out_file.display());
    Ok(())
}

async fn restore_backup(
    seed_phrase: &str,
    archive_file: &std::path::Path,
    data_dir_override: Option<&std::path::Path>,
) -> Result<()> {
    use anyhow::Context;
    use skattr_core::daemon::backup::import_backup;
    use skattr_core::identity::derive::derive_storage_seed;

    let data_dir = effective_data_dir(data_dir_override)?;

    // Parse the mnemonic through a Zeroizing copy.
    let mnemonic = {
        let owned = zeroize::Zeroizing::new(seed_phrase.to_string());
        Mnemonic::parse(&owned)
    };
    let identity_seed = Seed::from_mnemonic(&mnemonic)
        .context("invalid seed phrase (check word list and checksum)")?;
    let identity = IdentityKey::from_seed(&identity_seed)?;
    let storage_seed = derive_storage_seed(identity)?;

    import_backup(archive_file, &data_dir, &storage_seed)?;
    println!("Restored from {}", archive_file.display());
    println!("Data at: {}", data_dir.display());
    println!("Run `skattr daemon` to bring the identity online.");
    Ok(())
}

/// Invoke the same smoke entry point the UI uses, from the CLI.
///
/// `data_dir_override`: if `Some(path)`, use that data_dir (caller is
/// responsible for ensuring it's empty). If `None`, allocate a fresh
/// tempdir under `$HOME/.cache/skattr-smoke-test/` so Arti's
/// fs-mistrust accepts the parent chain (avoiding world-writable
/// `/tmp`).
async fn cli_smoke(data_dir_override: Option<&std::path::Path>, timeout_secs: u64) -> Result<()> {
    use skattr_core::daemon::smoke::{run_smoke, SmokeConfig};

    let data_dir = match data_dir_override {
        Some(d) => d.to_path_buf(),
        None => {
            // No override -> create a fresh tempdir anchored at
            // ~/.cache/ (NOT /tmp, because Arti's fs-mistrust rejects
            // world-writable parent dirs). The dev-only escape hatch
            // never runs over real user state.
            let cache_root = std::env::var_os("HOME")
                .map(|h| {
                    std::path::PathBuf::from(h)
                        .join(".cache")
                        .join("skattr-smoke-test")
                })
                .ok_or_else(|| anyhow::anyhow!("$HOME not set"))?;
            std::fs::create_dir_all(&cache_root)?;
            let tmpdir = tempfile::Builder::new()
                .prefix("cli-smoke-")
                .tempdir_in(&cache_root)?;
            tmpdir.keep()
        }
    };
    let cfg = SmokeConfig {
        data_dir,
        tor_ready_timeout: std::time::Duration::from_secs(timeout_secs),
        ..Default::default()
    };
    match run_smoke(cfg).await {
        Ok(report) => {
            println!(
                "smoke OK: onion={} duration={:?}",
                report.onion, report.duration
            );
            Ok(())
        }
        Err(e) => {
            eprintln!("smoke FAIL: {e}");
            std::process::exit(1);
        }
    }
}

async fn daemon(
    detach: bool,
    data_dir_override: Option<&std::path::Path>,
    passphrase_file: Option<PathBuf>,
    log_sink: skattr_core::daemon::logs::LogSink,
) -> Result<()> {
    use skattr_core::daemon::config::resolve_config_path;
    use skattr_core::daemon::{Config, Daemon};

    if detach {
        anyhow::bail!(
            "--detach is not implemented; run the daemon in the foreground \n             (or under your service manager) instead"
        );
    }

    let mut config = Config::defaults()?;
    if let Some(override_dir) = data_dir_override {
        config.data_dir = override_dir.to_path_buf();
    }
    std::fs::create_dir_all(&config.data_dir)
        .map_err(|e| anyhow::anyhow!("create data dir {}: {e}", config.data_dir.display()))?;
    // Carry any pre-existing identity from a legacy location into the
    // canonical data dir before the daemon opens it (fail-loud: abort rather
    // than silently onboarding anew).
    skattr_core::daemon::migrate_legacy_into(&config.data_dir)
        .map_err(|e| anyhow::anyhow!("data migration failed: {e}"))?;
    let vault_path = config.data_dir.join("identity.vault");

    if !vault_path.exists() {
        anyhow::bail!(
            "no identity vault at {}; run `skattr init` first",
            vault_path.display()
        );
    }

    let pw = match passphrase_file {
        Some(path) => read_passphrase_from_source(PassphraseSource::File(path))?,
        None => read_passphrase("Vault passphrase: ")?,
    };

    println!("Bootstrapping Tor\u{2026}");
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let shutdown_fut = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    // Resolve config-file path so SetConfig can persist changes atomically.
    let config_path = resolve_config_path(None);

    // Move the Zeroizing<String> passphrase and config by value into the
    // spawned task — they drop (and wipe) when Daemon::run_with_sink returns.
    let data_dir_owned = config.data_dir.clone();
    let config_owned = config.clone();
    let daemon_fut = tokio::spawn(async move {
        Daemon::run_with_sink(
            &data_dir_owned,
            &pw,
            config_owned,
            config_path,
            ready_tx,
            shutdown_fut,
            Some(log_sink),
        )
        .await
    });

    // Wait for the daemon to signal readiness.
    let ready = ready_rx
        .await
        .map_err(|_| anyhow::anyhow!("daemon exited before becoming ready"))?;
    println!();
    println!("Listening on: {}:1", ready.onion);
    println!("IPC socket:   {}", ready.ipc_socket.display());
    println!("Ctrl-C to shut down.");

    // Block until the daemon future returns (SIGINT + graceful shutdown).
    daemon_fut
        .await
        .map_err(|e| anyhow::anyhow!("daemon join: {e}"))??;

    println!();
    println!("Shutdown complete.");
    Ok(())
}

async fn invite(qr: bool, sock_flag: Option<&std::path::Path>, json: bool) -> Result<()> {
    use skattr_core::daemon::{Command as CoreCommand, CommandResult};

    let mut client = connect_or_exit(sock_flag).await?;
    let result = match client
        .execute(CoreCommand::CreateInvite {
            nickname: None,
            ttl_secs: None,
        })
        .await
    {
        Ok(r) => r,
        Err(e) => exit_on_ipc_error(e),
    };

    let (url, kpi, expires_at) = match result {
        CommandResult::InviteCreated {
            url,
            key_package_id,
            expires_at,
        } => (url, key_package_id, expires_at),
        other => anyhow::bail!("unexpected result: {other:?}"),
    };

    if json {
        let obj = serde_json::json!({
            "url": url,
            "key_package_id": kpi.to_string(),
            "expires_at": expires_at,
        });
        println!("{obj}");
    } else {
        println!("{url}");
        println!("(expires at unix {expires_at}, key package {kpi})");
        if qr {
            println!();
            println!("{}", render_invite_qr(&url));
        }
    }
    Ok(())
}

fn render_invite_qr(url: &str) -> String {
    use qrcode::render::unicode;
    use qrcode::QrCode;

    match QrCode::new(url.as_bytes()) {
        Ok(code) => code
            .render::<unicode::Dense1x2>()
            .quiet_zone(false)
            .dark_color(unicode::Dense1x2::Light)
            .light_color(unicode::Dense1x2::Dark)
            .build(),
        Err(_) => String::new(),
    }
}

async fn add(link: &str, sock_flag: Option<&std::path::Path>, json: bool) -> Result<()> {
    use skattr_core::daemon::{Command as CoreCommand, CommandResult};

    let mut client = connect_or_exit(sock_flag).await?;
    let result = match client
        .execute(CoreCommand::AddContact {
            invite_url: link.to_string(),
        })
        .await
    {
        Ok(r) => r,
        Err(e) => exit_on_ipc_error(e),
    };

    let summary = match result {
        CommandResult::ContactAdded(s) => s,
        other => anyhow::bail!("unexpected result: {other:?}"),
    };

    if json {
        println!("{}", serde_json::to_string(&summary)?);
    } else {
        let hex: String = summary
            .pubkey
            .0
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        println!("Added contact:");
        println!("  pubkey:  {}", hex);
        println!("  onion:   {}", summary.onion);
        println!("  added:   {}", summary.added_at);
    }
    Ok(())
}

async fn contacts(sock_flag: Option<&std::path::Path>, json: bool) -> Result<()> {
    use skattr_core::daemon::{Command as CoreCommand, CommandResult};

    let mut client = connect_or_exit(sock_flag).await?;
    let result = match client.execute(CoreCommand::ListContacts).await {
        Ok(r) => r,
        Err(e) => exit_on_ipc_error(e),
    };

    let rows = match result {
        CommandResult::Contacts(rows) => rows,
        other => anyhow::bail!("unexpected result: {other:?}"),
    };

    if json {
        println!("{}", serde_json::to_string(&rows)?);
    } else {
        print!("{}", render_contacts_human(&rows));
    }
    Ok(())
}

fn render_contacts_human(rows: &[skattr_core::daemon::commands::ContactSummary]) -> String {
    use std::fmt::Write;
    if rows.is_empty() {
        return "No contacts.\n".to_string();
    }
    let mut out = String::new();
    for row in rows {
        let short: String = row
            .pubkey
            .0
            .iter()
            .take(4)
            .map(|b| format!("{b:02x}"))
            .collect();
        let name = row.nickname.as_deref().unwrap_or("(unnamed)");
        let _ = writeln!(
            out,
            "{short}  {name:<20}  {onion}  added={added}",
            onion = row.onion,
            added = row.added_at
        );
    }
    out
}

async fn send(
    contact_prefix: &str,
    text: &str,
    fail_on_timeout: bool,
    sock_flag: Option<&std::path::Path>,
    json: bool,
) -> Result<()> {
    use skattr_core::daemon::commands::SendStatus;
    use skattr_core::daemon::{Command as CoreCommand, CommandResult};
    use skattr_core::envelope::Kind;

    let mut client = connect_or_exit(sock_flag).await?;

    // Resolve prefix via ListContacts (server-side).
    let rows_result = match client.execute(CoreCommand::ListContacts).await {
        Ok(r) => r,
        Err(e) => exit_on_ipc_error(e),
    };
    let rows = match rows_result {
        CommandResult::Contacts(rows) => rows,
        other => anyhow::bail!("unexpected result: {other:?}"),
    };
    let pubkey = match resolve_contact(&rows, contact_prefix) {
        Ok(pk) => pk,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(6);
        }
    };

    // The IPC server closes a non-subscribed connection after one Execute
    // (server/mod.rs: `is_terminal = subscribed.is_none() …`), so the resolve
    // `ListContacts` above already closed `client`. Reconnect for the action
    // Execute — one OS-level connection per Execute (see cli_real_tor's exec()).
    let mut client = connect_or_exit(sock_flag).await?;
    let result = match client
        .execute(CoreCommand::SendMessage {
            contact: pubkey,
            kind: Kind::Text {
                body: text.to_string(),
            },
        })
        .await
    {
        Ok(r) => r,
        Err(e) => exit_on_ipc_error(e),
    };

    let (msg_id, status) = match result {
        CommandResult::MessageSent {
            message_id, status, ..
        } => (message_id, status),
        other => anyhow::bail!("unexpected result: {other:?}"),
    };

    if json {
        let obj = serde_json::json!({
            "message_id": msg_id.to_string(),
            "status": match status {
                SendStatus::Queued => "queued",
                SendStatus::Delivered => "delivered",
            },
        });
        println!("{obj}");
    } else {
        println!(
            "{msg_id}  {state}",
            state = match status {
                SendStatus::Queued => "queued",
                SendStatus::Delivered => "delivered",
            }
        );
    }

    if fail_on_timeout && matches!(status, SendStatus::Queued) {
        std::process::exit(8);
    }
    Ok(())
}

async fn send_file_cmd(
    contact_prefix: &str,
    path: &str,
    sock_flag: Option<&std::path::Path>,
) -> Result<()> {
    use skattr_core::daemon::{Command as CoreCommand, CommandResult};

    let mut client = connect_or_exit(sock_flag).await?;

    // Resolve prefix via ListContacts (same idiom as `send`).
    let rows_result = match client.execute(CoreCommand::ListContacts).await {
        Ok(r) => r,
        Err(e) => exit_on_ipc_error(e),
    };
    let rows = match rows_result {
        CommandResult::Contacts(rows) => rows,
        other => anyhow::bail!("unexpected result: {other:?}"),
    };
    let pubkey = match resolve_contact(&rows, contact_prefix) {
        Ok(pk) => pk,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(6);
        }
    };

    // Reconnect: the resolve `ListContacts` closed the one-shot connection.
    let mut client = connect_or_exit(sock_flag).await?;
    let result = match client
        .execute(CoreCommand::SendFile {
            contact: pubkey,
            path: path.to_string(),
        })
        .await
    {
        Ok(r) => r,
        Err(e) => exit_on_ipc_error(e),
    };
    match result {
        CommandResult::FileQueued {
            message_id,
            attachment_id,
            total_chunks,
        } => {
            println!(
                "{message_id}  file queued  attachment={attachment_id}  chunks={total_chunks}"
            );
        }
        other => anyhow::bail!("unexpected result: {other:?}"),
    }
    Ok(())
}

fn resolve_contact(
    rows: &[skattr_core::daemon::commands::ContactSummary],
    prefix: &str,
) -> Result<skattr_core::identity::PublicKey> {
    let lower = prefix.to_ascii_lowercase();
    let mut matches: Vec<&skattr_core::daemon::commands::ContactSummary> = rows
        .iter()
        .filter(|r| {
            let hex: String = r.pubkey.0.iter().map(|b| format!("{b:02x}")).collect();
            hex.starts_with(&lower)
                || r.nickname
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(prefix))
        })
        .collect();
    match matches.len() {
        1 => Ok(matches.remove(0).pubkey),
        0 => anyhow::bail!("no contact matches {prefix:?}"),
        n => anyhow::bail!("ambiguous: {n} contacts match {prefix:?}"),
    }
}

/// Resolve a unique attachment-id prefix against the file rows in `rows`.
///
/// Mirrors `resolve_contact`: lowercased `starts_with` matching, and an error
/// naming the count when a prefix is ambiguous. Returns the full id and the
/// decoded manifest, so the caller can report the filename and size without
/// decoding twice.
fn resolve_attachment_id(
    rows: &[skattr_core::daemon::commands::MessageRecord],
    prefix: &str,
) -> Result<([u8; 16], skattr_core::AttachmentManifest)> {
    use skattr_core::envelope::Kind;
    let lower = prefix.to_ascii_lowercase();
    let mut matches: Vec<([u8; 16], skattr_core::AttachmentManifest)> = Vec::new();
    for row in rows {
        let Kind::File { manifest } = &row.kind else {
            continue;
        };
        let Ok(m) = skattr_core::AttachmentManifest::from_cbor(manifest) else {
            continue;
        };
        let hex: String = m.attachment_id.iter().map(|b| format!("{b:02x}")).collect();
        if hex.starts_with(&lower) && !matches.iter().any(|(id, _)| *id == m.attachment_id) {
            matches.push((m.attachment_id, m));
        }
    }
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => anyhow::bail!("no attachment matches {prefix:?}"),
        n => anyhow::bail!("ambiguous: {n} attachments match {prefix:?}"),
    }
}

/// Save a received attachment to `dest`.
///
/// Resolve-then-act: one connection to list recent messages (to resolve the id
/// prefix), a second to save. The IPC connection is single-request (#116), so
/// reusing the first would broken-pipe.
async fn save_attachment(
    id_prefix: &str,
    dest: &str,
    sock_flag: Option<&std::path::Path>,
    json: bool,
) -> Result<()> {
    use skattr_core::daemon::{Command as CoreCommand, CommandResult};

    // 1. Resolve the prefix against recent messages.
    let mut client = connect_or_exit(sock_flag).await?;
    let rows = match client
        .execute(CoreCommand::RecentMessages {
            contact: None,
            limit: 500,
            before_id: None,
            paged: false,
        })
        .await
    {
        Ok(CommandResult::Messages(rows)) => rows,
        Ok(other) => anyhow::bail!("unexpected reply: {other:?}"),
        Err(e) => exit_on_ipc_error(e),
    };
    let (attachment_id, manifest) = resolve_attachment_id(&rows, id_prefix)?;

    // 2. Absolutize the destination. The daemon's working directory is not
    //    ours, so a relative path would otherwise resolve somewhere the user
    //    did not mean. Validation is #54's job, not this command's.
    let dest_path = std::path::Path::new(dest);
    let abs = if dest_path.is_absolute() {
        dest_path.to_path_buf()
    } else {
        std::env::current_dir()?.join(dest_path)
    };

    // 3. Save on a fresh connection.
    let mut client = connect_or_exit(sock_flag).await?;
    match client
        .execute(CoreCommand::SaveAttachment {
            attachment_id: skattr_core::daemon::hex::Hex16::from(attachment_id),
            dest_path: abs.to_string_lossy().into_owned(),
        })
        .await
    {
        Ok(CommandResult::Ok) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "saved": true,
                        "path": abs.to_string_lossy(),
                        "filename": manifest.filename,
                        "size": manifest.total_size,
                    })
                );
            } else {
                println!(
                    "saved {} -> {}",
                    format_size(manifest.total_size),
                    abs.display()
                );
            }
            Ok(())
        }
        Ok(other) => anyhow::bail!("unexpected reply: {other:?}"),
        // Verified: `decrypt_attachment_to` (dispatch.rs) returns
        // `IpcError::Daemon(DaemonErrorKind::InvalidArgument{..})` when the row
        // is missing OR when `direction != "in" || status != "complete"`.
        // Since the id came from a real file row we just listed, the live cause
        // is effectively always "not complete yet". Report it as a clean
        // diagnostic with a non-zero exit so a script can branch on it, rather
        // than an error dump (#118 acceptance).
        Err(skattr_core::daemon::IpcClientError::Server(
            skattr_core::daemon::ipc::wire::IpcError::Daemon(
                skattr_core::daemon::error_kind::DaemonErrorKind::InvalidArgument { .. },
            ),
        )) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "saved": false, "reason": "unavailable" })
                );
            } else {
                eprintln!("not available yet (transfer incomplete)");
            }
            std::process::exit(1);
        }
        // Anything else is a genuine transport/daemon failure.
        Err(e) => exit_on_ipc_error(e),
    }
}

async fn tail(
    contact_prefix: Option<&str>,
    limit: u32,
    follow: bool,
    sock_flag: Option<&std::path::Path>,
    json: bool,
) -> Result<()> {
    use skattr_core::daemon::{Command as CoreCommand, CommandResult};

    if follow {
        return tail_follow(contact_prefix, limit, sock_flag).await;
    }

    let mut client = connect_or_exit(sock_flag).await?;
    let target = resolve_optional_contact(sock_flag, contact_prefix).await?;

    let result = match client
        .execute(CoreCommand::RecentMessages {
            contact: target,
            limit,
            before_id: None,
            paged: false,
        })
        .await
    {
        Ok(r) => r,
        Err(e) => exit_on_ipc_error(e),
    };

    let rows = match result {
        CommandResult::Messages(rows) => rows,
        other => anyhow::bail!("unexpected result: {other:?}"),
    };

    if json {
        println!("{}", serde_json::to_string(&rows)?);
    } else {
        let avail = probe_availability(&rows, sock_flag).await;
        print!("{}", render_messages_human(&rows, &avail));
    }
    Ok(())
}

async fn resolve_optional_contact(
    sock_flag: Option<&std::path::Path>,
    prefix: Option<&str>,
) -> Result<Option<skattr_core::identity::PublicKey>> {
    use skattr_core::daemon::{Command as CoreCommand, CommandResult};
    let Some(prefix) = prefix else {
        return Ok(None);
    };
    // Own connection: the IPC server closes a non-subscribed connection after
    // one Execute (server/mod.rs: `is_terminal = subscribed.is_none() …`), so
    // resolving on the caller's client would close it before the caller's own
    // action Execute. Resolve on a throwaway connection instead.
    let mut client = connect_or_exit(sock_flag).await?;
    let rows_result = match client.execute(CoreCommand::ListContacts).await {
        Ok(r) => r,
        Err(e) => exit_on_ipc_error(e),
    };
    let rows = match rows_result {
        CommandResult::Contacts(rows) => rows,
        other => anyhow::bail!("unexpected result: {other:?}"),
    };
    match resolve_contact(&rows, prefix) {
        Ok(pk) => Ok(Some(pk)),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(6);
        }
    }
}

/// Availability of inbound attachments, keyed by `attachment_id`.
///
/// Built by the caller (which does the IPC) and passed into rendering, so the
/// render functions stay pure and unit-testable.
type AvailMap = std::collections::HashMap<[u8; 16], bool>;

/// Probe availability for every **inbound** file row in `rows`.
///
/// Inbound only: `attachment_available_cmd` answers true iff the row is
/// `direction='in', status='complete'`, so probing an outgoing row would print
/// `incomplete` beside a file the user sent themselves.
///
/// One probe per connection — the daemon's IPC connection is single-request
/// (#116), and in `--follow` the caller's connection is already subscribed, so
/// a probe there *must* be separate or it would hang.
///
/// Best-effort: a probe that fails is simply omitted from the map, and the row
/// renders without an availability field. Visibility must not be all-or-nothing.
async fn probe_availability(
    rows: &[skattr_core::daemon::commands::MessageRecord],
    sock_flag: Option<&std::path::Path>,
) -> AvailMap {
    use skattr_core::daemon::commands::Direction;
    use skattr_core::daemon::{Command as CoreCommand, CommandResult};
    use skattr_core::envelope::Kind;

    let mut out = AvailMap::new();
    for row in rows {
        if row.direction != Direction::Incoming {
            continue;
        }
        let Kind::File { manifest } = &row.kind else {
            continue;
        };
        let Ok(m) = skattr_core::AttachmentManifest::from_cbor(manifest) else {
            continue;
        };
        if out.contains_key(&m.attachment_id) {
            continue; // same attachment referenced twice in one listing
        }
        // Deliberately NOT `connect_or_exit`: it prints and exits the process
        // when the daemon is down, which is wrong for a best-effort probe.
        let Ok(path) = resolve_socket_path(sock_flag) else {
            continue;
        };
        let Ok(mut client) = skattr_core::daemon::IpcClient::connect(&path).await else {
            continue;
        };
        let res = client
            .execute(CoreCommand::AttachmentAvailable {
                attachment_id: skattr_core::daemon::hex::Hex16::from(m.attachment_id),
            })
            .await;
        if let Ok(CommandResult::AttachmentAvailability { available }) = res {
            out.insert(m.attachment_id, available);
        }
    }
    out
}

/// Human-readable byte size: `0 B`, `512 B`, `2.0 KiB`, `2.4 MiB`.
fn format_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.1} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.1} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// Render a `Kind::File` body: filename, size, short id, and (when known)
/// availability.
///
/// `Kind::File` carries only the CBOR manifest — there is no filename field on
/// the record — so decoding is the only way to say anything useful about it.
/// A manifest that will not decode renders as a marker rather than aborting the
/// listing: one bad row must not blind the whole tail (#118).
fn render_file_kind(manifest: &[u8], availability: Option<bool>) -> String {
    let Ok(m) = skattr_core::AttachmentManifest::from_cbor(manifest) else {
        return "📎 (unreadable manifest)".to_string();
    };
    let id: String = m
        .attachment_id
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    let state = match availability {
        Some(true) => "  available",
        Some(false) => "  incomplete",
        None => "",
    };
    format!(
        "📎 {name}  {size}  id={id}{state}",
        name = m.filename,
        size = format_size(m.total_size),
    )
}

/// Look up availability for a file row, keyed by the manifest's attachment id.
/// `None` when the manifest will not decode or the id was never probed
/// (outgoing rows, or a probe that failed).
fn availability_for(manifest: &[u8], avail: &AvailMap) -> Option<bool> {
    let m = skattr_core::AttachmentManifest::from_cbor(manifest).ok()?;
    avail.get(&m.attachment_id).copied()
}

fn render_message_record_human(
    row: &skattr_core::daemon::commands::MessageRecord,
    avail: &AvailMap,
) -> String {
    use skattr_core::daemon::commands::Direction;
    use skattr_core::envelope::Kind;

    let arrow = match row.direction {
        Direction::Incoming => "<-",
        Direction::Outgoing => "->",
    };
    let body = match &row.kind {
        Kind::Text { body } => body.clone(),
        Kind::File { manifest } => render_file_kind(manifest, availability_for(manifest, avail)),
        other => format!("({other:?})"),
    };
    let contact_short: String = row
        .contact
        .0
        .iter()
        .take(4)
        .map(|b| format!("{b:02x}"))
        .collect();
    format!(
        "[{ts}] {arrow} {contact_short} {body}",
        ts = row.ts_daemon_recv
    )
}

fn render_messages_human(
    rows: &[skattr_core::daemon::commands::MessageRecord],
    avail: &AvailMap,
) -> String {
    use std::fmt::Write;

    if rows.is_empty() {
        return "No messages.\n".to_string();
    }

    let mut out = String::new();
    // Render oldest-first on stdout (`recent` returns newest-first).
    for row in rows.iter().rev() {
        let _ = writeln!(out, "{}", render_message_record_human(row, avail));
    }
    out
}

async fn tail_follow(
    contact_prefix: Option<&str>,
    limit: u32,
    sock_flag: Option<&std::path::Path>,
) -> Result<()> {
    use skattr_core::daemon::events::Event;
    use skattr_core::daemon::ipc::wire::EventFilter;
    use skattr_core::daemon::{Command as CoreCommand, CommandResult, IpcClientError};

    let mut client = connect_or_exit(sock_flag).await?;
    let target = resolve_optional_contact(sock_flag, contact_prefix).await?;

    // 1. Dump recent.
    let recent = match client
        .execute(CoreCommand::RecentMessages {
            contact: target,
            limit,
            before_id: None,
            paged: false,
        })
        .await
    {
        Ok(r) => r,
        Err(e) => exit_on_ipc_error(e),
    };
    if let CommandResult::Messages(rows) = recent {
        let avail = probe_availability(&rows, sock_flag).await;
        print!("{}", render_messages_human(&rows, &avail));
    }

    // 2. Subscribe. Reconnect first: RecentMessages above closed the one-shot
    // connection. A subscribed connection is kept open (Execute is no longer
    // terminal), so this fresh one persists for the event stream below.
    let mut client = connect_or_exit(sock_flag).await?;
    let filter = EventFilter::Messages { contact: target };
    match client.subscribe(filter).await {
        Ok(()) => {}
        Err(e) => exit_on_ipc_error(e),
    }

    // 3. Stream events until Ctrl-C / EOF.
    loop {
        let ev = match client.next_event().await {
            Ok(ev) => ev,
            Err(IpcClientError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(IpcClientError::UnexpectedFrame(_)) => break,
            Err(other) => exit_on_ipc_error(other),
        };
        match ev {
            Event::MessageReceived { contact: _, record } => {
                let avail = probe_availability(std::slice::from_ref(&record), sock_flag).await;
                println!("{}", render_message_record_human(&record, &avail));
            }
            Event::DeliveryStatusChanged { message, status } => {
                let id_hex: String = message.0.iter().map(|b| format!("{b:02x}")).collect();
                println!("... {id_hex} {status:?}");
            }
            Event::ContactUpdated(pk) => {
                let short: String = pk.0.iter().take(4).map(|b| format!("{b:02x}")).collect();
                println!("contact updated: {short}");
            }
            Event::ContactRemoved(pk) => {
                let short: String = pk.0.iter().take(4).map(|b| format!("{b:02x}")).collect();
                println!("contact removed: {short}");
            }
            Event::TorStatusChanged(s) => {
                eprintln!("tor: {s:?}");
            }
            Event::MailboxStatusChanged { mailbox_id, status } => {
                eprintln!("mailbox {mailbox_id}: {status:?}");
            }
            Event::ContactCardReceived { contact, version } => {
                let short: String = contact
                    .0
                    .iter()
                    .take(4)
                    .map(|b| format!("{b:02x}"))
                    .collect();
                eprintln!("contact card updated: {short} v{version}");
            }
            Event::LogRecord(record) => {
                // Log records are only emitted when explicitly subscribed
                // via EventFilter::Logs (Settings → Advanced → Logs).
                eprintln!(
                    "{} [{:?}] {}: {}",
                    record.ts_unix_ms, record.level, record.target, record.message
                );
            }
            // 3.B attachment events — full CLI integration in Task 7.
            Event::AttachmentReceived { .. }
            | Event::AttachmentProgress { .. }
            | Event::AttachmentFailed { .. } => {}
        }
    }
    Ok(())
}

async fn chat(contact_prefix: &str, sock_flag: Option<&std::path::Path>) -> Result<()> {
    use skattr_core::daemon::events::Event;
    use skattr_core::daemon::ipc::wire::EventFilter;
    use skattr_core::daemon::{Command as CoreCommand, CommandResult, IpcClientError};
    use skattr_core::envelope::Kind;
    use tokio::io::{AsyncBufReadExt, BufReader};

    let mut client = connect_or_exit(sock_flag).await?;

    // Resolve contact via ListContacts + resolve_contact.
    let rows_result = match client.execute(CoreCommand::ListContacts).await {
        Ok(r) => r,
        Err(e) => exit_on_ipc_error(e),
    };
    let rows = match rows_result {
        CommandResult::Contacts(rows) => rows,
        other => anyhow::bail!("unexpected result: {other:?}"),
    };
    let pubkey = match resolve_contact(&rows, contact_prefix) {
        Ok(pk) => pk,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(6);
        }
    };

    // Reconnect: the resolve `ListContacts` closed the one-shot connection. A
    // subscribed connection is kept open (Execute is no longer terminal), so
    // this fresh one persists for the event stream + the interleaved
    // SendMessage Executes in the loop below.
    let mut client = connect_or_exit(sock_flag).await?;
    // Subscribe for incoming messages from this peer.
    match client.subscribe(EventFilter::Contact(pubkey)).await {
        Ok(()) => {}
        Err(e) => exit_on_ipc_error(e),
    }

    eprintln!("chat: connected. Type a line and press Enter; Ctrl-D to exit.");

    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut line = String::new();

    loop {
        tokio::select! {
            // Incoming event from the daemon.
            ev = client.next_event() => {
                match ev {
                    Ok(Event::MessageReceived { contact: _, record }) => {
                        let body = match record.kind {
                            Kind::Text { body } => body,
                            other => format!("({other:?})"),
                        };
                        println!("<- {body}");
                    }
                    Ok(Event::DeliveryStatusChanged { message: _, status }) => {
                        eprintln!("... {status:?}");
                    }
                    Ok(_) => {}
                    Err(IpcClientError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    Err(IpcClientError::UnexpectedFrame(_)) => break,
                    Err(other) => exit_on_ipc_error(other),
                }
            }
            // User typed a line.
            n = stdin.read_line(&mut line) => {
                let n = n?;
                if n == 0 {
                    // EOF on stdin.
                    break;
                }
                let trimmed = line.trim_end_matches(['\n', '\r']).to_string();
                line.clear();
                if trimmed.is_empty() {
                    continue;
                }
                // Send on a SEPARATE one-shot connection — the subscribed
                // `client` above is for the inbound event stream only.
                // `IpcClient::execute` on a subscribed connection does not
                // interleave cleanly (it would block on the event stream), so
                // each send gets its own fresh connection.
                let mut send_client = connect_or_exit(sock_flag).await?;
                let res = send_client
                    .execute(CoreCommand::SendMessage {
                        contact: pubkey,
                        kind: Kind::Text { body: trimmed },
                    })
                    .await;
                match res {
                    Ok(CommandResult::MessageSent { status, .. }) => {
                        eprintln!(".. {status:?}");
                    }
                    Ok(other) => eprintln!("unexpected: {other:?}"),
                    Err(e) => exit_on_ipc_error(e),
                }
            }
        }
    }
    Ok(())
}

fn render_search_human(hits: &[skattr_core::daemon::commands::SearchHitRecord]) -> String {
    let mut out = String::new();
    for h in hits {
        let id_prefix: String = h
            .record
            .message_id
            .0
            .iter()
            .take(3)
            .map(|b| format!("{b:02x}"))
            .collect();
        out.push_str(&format!(
            "[ts_recv={ts}] (id={id} epoch={epoch}) {snippet}\n",
            ts = h.record.ts_daemon_recv,
            id = id_prefix,
            epoch = h.record.mls_generation,
            snippet = h.snippet,
        ));
    }
    out
}

async fn search(
    contact: Option<&str>,
    query: String,
    limit: u32,
    offset: u32,
    newest_first: bool,
    as_json: bool,
    sock_flag: Option<&std::path::Path>,
) -> Result<()> {
    use skattr_core::daemon::commands::CommandResult;
    use skattr_core::daemon::Command as CoreCommand;

    let mut client = connect_or_exit(sock_flag).await?;

    // Resolve optional contact prefix to a PublicKey via ListContacts.
    let resolved_pk = if let Some(prefix) = contact {
        let rows_result = match client.execute(CoreCommand::ListContacts).await {
            Ok(r) => r,
            Err(e) => exit_on_ipc_error(e),
        };
        let rows = match rows_result {
            CommandResult::Contacts(rows) => rows,
            other => anyhow::bail!("unexpected daemon response: {other:?}"),
        };
        match resolve_contact(&rows, prefix) {
            Ok(pk) => Some(pk),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(6);
            }
        }
    } else {
        None
    };

    // Reconnect: a contact-filtered search resolved via `ListContacts`, which
    // closed the one-shot connection. Harmless when no filter was given.
    let mut client = connect_or_exit(sock_flag).await?;
    let resp = match client
        .execute(CoreCommand::SearchMessages {
            query,
            contact: resolved_pk,
            limit,
            offset,
            newest_first,
        })
        .await
    {
        Ok(r) => r,
        Err(e) => exit_on_ipc_error(e),
    };

    match resp {
        CommandResult::SearchResults(hits) => {
            if as_json {
                println!("{}", serde_json::to_string_pretty(&hits)?);
            } else {
                print!("{}", render_search_human(&hits));
            }
            Ok(())
        }
        other => anyhow::bail!("unexpected daemon response: {other:?}"),
    }
}

fn chrono_or_naive_iso(ts: u64) -> String {
    use time::format_description::well_known::Rfc3339;
    use time::OffsetDateTime;
    let secs = i64::try_from(ts).unwrap_or(0);
    OffsetDateTime::from_unix_timestamp(secs)
        .ok()
        .and_then(|odt| odt.format(&Rfc3339).ok())
        .unwrap_or_else(|| format!("{ts}"))
}

fn render_export_text_line(
    rec: &skattr_core::daemon::commands::MessageRecord,
    avail: &AvailMap,
) -> String {
    use skattr_core::daemon::commands::Direction;
    let body = match &rec.kind {
        skattr_core::envelope::Kind::Text { body } => body.clone(),
        skattr_core::envelope::Kind::File { manifest } => {
            render_file_kind(manifest, availability_for(manifest, avail))
        }
        other => format!("<{other:?}>"),
    };
    let from = match rec.direction {
        Direction::Incoming => "peer",
        Direction::Outgoing => "self",
    };
    let ts = chrono_or_naive_iso(rec.ts_daemon_recv);
    format!("[{ts}] {from}: {body}\n")
}

async fn export(
    contact_prefix: &str,
    format: String,
    output: std::path::PathBuf,
    sock_flag: Option<&std::path::Path>,
) -> Result<()> {
    use skattr_core::daemon::commands::CommandResult;
    use skattr_core::daemon::Command as CoreCommand;
    use std::io::Write;

    // Validate format.
    if format != "json" && format != "text" {
        anyhow::bail!("unsupported --format {format:?}; use `json` or `text`");
    }

    let mut client = connect_or_exit(sock_flag).await?;

    // Resolve contact via ListContacts + resolve_contact.
    let rows_result = match client.execute(CoreCommand::ListContacts).await {
        Ok(r) => r,
        Err(e) => exit_on_ipc_error(e),
    };
    let rows = match rows_result {
        CommandResult::Contacts(rows) => rows,
        other => anyhow::bail!("unexpected daemon response: {other:?}"),
    };
    let pk = match resolve_contact(&rows, contact_prefix) {
        Ok(pk) => pk,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(6);
        }
    };

    // Open output with O_CREAT|O_EXCL to refuse clobbering.
    // On Unix, also set permissions to 0o600 (owner read+write only).
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&output)
            .map_err(|e| anyhow::anyhow!("open {}: {e}", output.display()))?
    };
    #[cfg(not(unix))]
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)
        .map_err(|e| anyhow::anyhow!("open {}: {e}", output.display()))?;

    if format == "json" {
        file.write_all(b"[\n")?;
    }
    let mut first_record = true;
    let mut after_id: Option<i64> = None;
    loop {
        // Reconnect each iteration: the IPC server closes a non-subscribed
        // connection after one Execute, so each paged ExportHistory (and the
        // prior resolve `ListContacts`) needs its own connection.
        let mut client = connect_or_exit(sock_flag).await?;
        let resp = match client
            .execute(CoreCommand::ExportHistory {
                contact: pk,
                after_id,
                limit: 1000,
            })
            .await
        {
            Ok(r) => r,
            Err(e) => exit_on_ipc_error(e),
        };
        let (records, next) = match resp {
            CommandResult::ExportPage {
                records,
                next_after_id,
            } => (records, next_after_id),
            other => anyhow::bail!("unexpected response: {other:?}"),
        };
        let avail = if format == "json" {
            AvailMap::new()
        } else {
            probe_availability(&records, sock_flag).await
        };
        for r in &records {
            if format == "json" {
                if !first_record {
                    file.write_all(b",\n")?;
                }
                first_record = false;
                serde_json::to_writer(&mut file, r)?;
            } else {
                file.write_all(render_export_text_line(r, &avail).as_bytes())?;
            }
        }
        if next.is_none() {
            break;
        }
        after_id = next;
    }
    if format == "json" {
        file.write_all(b"\n]\n")?;
    }
    file.sync_all()?;
    Ok(())
}

fn parse_rfc3339_to_unix(s: &str) -> anyhow::Result<i64> {
    use time::format_description::well_known::Rfc3339;
    use time::OffsetDateTime;
    let odt = OffsetDateTime::parse(s, &Rfc3339)
        .map_err(|e| anyhow::anyhow!("invalid RFC3339 timestamp: {e}"))?;
    Ok(odt.unix_timestamp())
}

async fn prune(
    contact: Option<&str>,
    before: Option<String>,
    keep_last: Option<u64>,
    sock_flag: Option<&std::path::Path>,
) -> Result<()> {
    use skattr_core::daemon::commands::CommandResult;
    use skattr_core::daemon::Command as CoreCommand;

    if before.is_some() == keep_last.is_some() {
        anyhow::bail!("exactly one of --before or --keep-last is required");
    }

    let mut client = connect_or_exit(sock_flag).await?;

    // Resolve optional contact.
    let resolved_pk = if let Some(prefix) = contact {
        let rows_result = match client.execute(CoreCommand::ListContacts).await {
            Ok(r) => r,
            Err(e) => exit_on_ipc_error(e),
        };
        let rows = match rows_result {
            CommandResult::Contacts(rows) => rows,
            other => anyhow::bail!("unexpected daemon response: {other:?}"),
        };
        match resolve_contact(&rows, prefix) {
            Ok(pk) => Some(pk),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(6);
            }
        }
    } else {
        None
    };

    let before_ts_recv = before.map(|s| parse_rfc3339_to_unix(&s)).transpose()?;

    // Reconnect: a contact-filtered prune resolved via `ListContacts`, closing
    // the one-shot connection. Harmless when no filter was given.
    let mut client = connect_or_exit(sock_flag).await?;
    let resp = match client
        .execute(CoreCommand::PruneHistory {
            contact: resolved_pk,
            before_ts_recv,
            keep_last,
        })
        .await
    {
        Ok(r) => r,
        Err(e) => exit_on_ipc_error(e),
    };
    match resp {
        CommandResult::Pruned { rows_deleted } => {
            println!("Deleted {rows_deleted} messages.");
            Ok(())
        }
        other => anyhow::bail!("unexpected daemon response: {other:?}"),
    }
}

async fn remove(
    contact_prefix: &str,
    sock_flag: Option<&std::path::Path>,
    _json: bool,
) -> Result<()> {
    use skattr_core::daemon::{Command as CoreCommand, CommandResult};

    let mut client = connect_or_exit(sock_flag).await?;

    let rows = match client.execute(CoreCommand::ListContacts).await {
        Ok(CommandResult::Contacts(rows)) => rows,
        Ok(other) => anyhow::bail!("unexpected result: {other:?}"),
        Err(e) => exit_on_ipc_error(e),
    };
    let pubkey = match resolve_contact(&rows, contact_prefix) {
        Ok(pk) => pk,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(6);
        }
    };

    // Reconnect: the resolve `ListContacts` closed the one-shot connection.
    let mut client = connect_or_exit(sock_flag).await?;
    let result = match client
        .execute(CoreCommand::RemoveContact { contact: pubkey })
        .await
    {
        Ok(r) => r,
        Err(e) => exit_on_ipc_error(e),
    };
    match result {
        CommandResult::ContactRemoved { hard: true } => {
            println!("removed {contact_prefix} (local state wiped)")
        }
        CommandResult::ContactRemoved { hard: false } => println!("archived {contact_prefix}"),
        other => anyhow::bail!("unexpected result: {other:?}"),
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_from_file_trims_single_trailing_newline() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "secret-pw").unwrap();
        let pw = read_passphrase_from_file(tmp.path()).unwrap();
        assert_eq!(pw.as_str(), "secret-pw");
    }

    #[test]
    fn read_from_file_preserves_internal_newlines() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "line1\nline2\n").unwrap();
        let pw = read_passphrase_from_file(tmp.path()).unwrap();
        assert_eq!(pw.as_str(), "line1\nline2");
    }

    #[test]
    fn read_from_missing_file_returns_error() {
        let err = read_passphrase_from_file(std::path::Path::new("/does/not/exist"))
            .expect_err("missing file must error");
        assert!(err.to_string().contains("/does/not/exist"));
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial(resolve_socket_path_env)]
    fn resolve_socket_path_prefers_flag_over_env() {
        let tmp = tempfile::tempdir().unwrap();
        let flag = tmp.path().join("flag.sock");
        let env = tmp.path().join("env.sock");
        std::env::set_var("SKATTR_SOCKET", &env);
        let got = resolve_socket_path(Some(&flag)).unwrap();
        assert_eq!(got.as_path(), flag);
        std::env::remove_var("SKATTR_SOCKET");
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial(resolve_socket_path_env)]
    fn resolve_socket_path_env_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let env = tmp.path().join("env.sock");
        std::env::set_var("SKATTR_SOCKET", &env);
        // Prevent a stale XDG_RUNTIME_DIR from a sibling test poisoning
        // the env-before-xdg precedence check.
        std::env::remove_var("XDG_RUNTIME_DIR");
        let got = resolve_socket_path(None).unwrap();
        assert_eq!(got, env);
        std::env::remove_var("SKATTR_SOCKET");
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial(resolve_socket_path_env)]
    fn resolve_socket_path_xdg_fallback() {
        std::env::remove_var("SKATTR_SOCKET");
        std::env::set_var("XDG_RUNTIME_DIR", "/custom/run/1000");
        let got = resolve_socket_path(None).unwrap();
        assert_eq!(
            got,
            std::path::PathBuf::from("/custom/run/1000/skattr")
                .join(skattr_core::daemon::ipc::ENDPOINT_FILENAME)
        );
        std::env::remove_var("XDG_RUNTIME_DIR");
    }

    #[test]
    fn clap_parses_add_link() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["skattr", "add", "skattr://invite/v1#abc"]).unwrap();
        match cli.cmd {
            Command::Add { link } => assert_eq!(link, "skattr://invite/v1#abc"),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn render_contacts_human_empty() {
        let out = render_contacts_human(&[]);
        assert_eq!(out.trim(), "No contacts.");
    }

    #[test]
    fn render_contacts_human_one_row() {
        use skattr_core::daemon::commands::ContactSummary;
        let rows = vec![ContactSummary {
            pubkey: skattr_core::identity::PublicKey([0xABu8; 32]),
            nickname: Some("alice".into()),
            onion: "aaaa.onion".into(),
            card_version: 3,
            added_at: 1_700_000_000,
            unread_count: 0,
            last_message_preview: None,
            last_ts_recv: None,
            group_state: None,
            last_read_row_id: None,
            muted: false,
            peer_mailboxes: Vec::new(),
            welcome_failed: false,
        }];
        let out = render_contacts_human(&rows);
        assert!(out.contains("alice"));
        assert!(out.contains("aaaa.onion"));
        assert!(out.contains("abab")); // pubkey prefix
    }

    #[test]
    fn resolve_contact_matches_unique_prefix() {
        use skattr_core::daemon::commands::ContactSummary;
        use skattr_core::identity::PublicKey;

        let rows = vec![
            ContactSummary {
                pubkey: PublicKey([0xAB; 32]),
                nickname: None,
                onion: "".into(),
                card_version: 0,
                added_at: 0,
                unread_count: 0,
                last_message_preview: None,
                last_ts_recv: None,
                group_state: None,
                last_read_row_id: None,
                muted: false,
                peer_mailboxes: Vec::new(),
                welcome_failed: false,
            },
            ContactSummary {
                pubkey: PublicKey([0xCD; 32]),
                nickname: None,
                onion: "".into(),
                card_version: 0,
                added_at: 0,
                unread_count: 0,
                last_message_preview: None,
                last_ts_recv: None,
                group_state: None,
                last_read_row_id: None,
                muted: false,
                peer_mailboxes: Vec::new(),
                welcome_failed: false,
            },
        ];
        let pk = resolve_contact(&rows, "ab").unwrap();
        assert_eq!(pk.0[0], 0xAB);
    }

    #[test]
    fn resolve_contact_ambiguous_returns_error_with_count() {
        use skattr_core::daemon::commands::ContactSummary;
        use skattr_core::identity::PublicKey;

        let rows = vec![
            ContactSummary {
                pubkey: PublicKey([0xAB; 32]),
                nickname: None,
                onion: "".into(),
                card_version: 0,
                added_at: 0,
                unread_count: 0,
                last_message_preview: None,
                last_ts_recv: None,
                group_state: None,
                last_read_row_id: None,
                muted: false,
                peer_mailboxes: Vec::new(),
                welcome_failed: false,
            },
            ContactSummary {
                pubkey: PublicKey({
                    let mut b = [0xAB; 32];
                    b[1] = 0xCD;
                    b
                }),
                nickname: None,
                onion: "".into(),
                card_version: 0,
                added_at: 0,
                unread_count: 0,
                last_message_preview: None,
                last_ts_recv: None,
                group_state: None,
                last_read_row_id: None,
                muted: false,
                peer_mailboxes: Vec::new(),
                welcome_failed: false,
            },
        ];
        let err = resolve_contact(&rows, "ab").unwrap_err();
        assert!(err.to_string().contains("ambiguous"));
    }

    #[test]
    fn resolve_contact_no_match_returns_error() {
        let rows: Vec<skattr_core::daemon::commands::ContactSummary> = vec![];
        let err = resolve_contact(&rows, "ff").unwrap_err();
        assert!(err.to_string().contains("no contact"));
    }

    #[test]
    fn render_messages_human_empty() {
        let out = render_messages_human(&[], &AvailMap::new());
        assert_eq!(out.trim(), "No messages.");
    }

    #[test]
    fn render_messages_human_one_text_row() {
        use skattr_core::daemon::commands::{Direction, MessageRecord};
        use skattr_core::daemon::hex::Hex16;
        use skattr_core::envelope::Kind;
        use skattr_core::identity::PublicKey;
        let rows = vec![MessageRecord {
            row_id: 0, // row_id irrelevant in this test
            message_id: Hex16::from([2; 16]),
            contact: PublicKey([7; 32]),
            direction: Direction::Incoming,
            kind: Kind::Text {
                body: "hello".into(),
            },
            mls_generation: 0,
            ts_daemon_recv: 1_700_000_000,
            ts_envelope: 1_699_999_999,
        }];
        let out = render_messages_human(&rows, &AvailMap::new());
        assert!(out.contains("hello"));
        assert!(out.contains("<-")); // incoming arrow
    }

    const DISTINCT_ID: [u8; 16] = [
        0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
        0x00,
    ];

    fn test_manifest_bytes(name: &str, size: u64, id: [u8; 16]) -> Vec<u8> {
        use skattr_core::AttachmentManifest;
        let m = AttachmentManifest {
            manifest_version: 1,
            attachment_id: id,
            filename: name.to_string(),
            mime: "application/octet-stream".into(),
            total_size: size,
            chunk_size: 49152,
            file_key: [0u8; 32],
            chunks: vec![],
        };
        m.to_cbor().unwrap()
    }

    #[test]
    fn format_size_renders_human_units() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(2048), "2.0 KiB");
        assert_eq!(format_size(2_516_582), "2.4 MiB");
    }

    #[test]
    fn render_file_kind_shows_name_size_and_id() {
        let bytes = test_manifest_bytes("logs.tar.gz", 2_516_582, DISTINCT_ID);
        let out = render_file_kind(&bytes, Some(true));
        assert!(out.contains("logs.tar.gz"), "got {out}");
        assert!(out.contains("2.4 MiB"), "got {out}");
        assert!(out.contains("id=1122334455667788"), "got {out}");
        // Must stop at 8 bytes: the 9th byte must not follow the id.
        assert!(
            !out.contains("112233445566778899"),
            "id not truncated: {out}"
        );
        // And certainly not the whole 32-char id.
        assert!(
            !out.contains("112233445566778899aabbccddeeff00"),
            "id not truncated: {out}"
        );
        assert!(out.contains("available"), "got {out}");
    }

    #[test]
    fn render_file_kind_marks_incomplete_when_unavailable() {
        let bytes = test_manifest_bytes("huge.bin", 40, [0x4f; 16]);
        let out = render_file_kind(&bytes, Some(false));
        assert!(out.contains("incomplete"), "got {out}");
        assert!(
            !out.contains("available"),
            "must not claim available: {out}"
        );
    }

    #[test]
    fn render_file_kind_omits_state_when_availability_unknown() {
        // Outgoing rows and failed probes pass None — the row still renders,
        // just without an availability field.
        let bytes = test_manifest_bytes("sent.bin", 10, [0x11; 16]);
        let out = render_file_kind(&bytes, None);
        assert!(out.contains("sent.bin"), "got {out}");
        assert!(!out.contains("available"), "got {out}");
        assert!(!out.contains("incomplete"), "got {out}");
    }

    #[test]
    fn render_file_kind_survives_an_undecodable_manifest() {
        // A corrupt or future-version manifest must not abort a tail.
        let out = render_file_kind(&[0xff, 0x00, 0x13], None);
        assert!(out.contains("unreadable manifest"), "got {out}");
    }

    #[test]
    fn render_messages_human_decodes_a_file_row() {
        use skattr_core::daemon::commands::{Direction, MessageRecord};
        use skattr_core::daemon::hex::Hex16;
        use skattr_core::envelope::Kind;
        use skattr_core::identity::PublicKey;
        let bytes = test_manifest_bytes("photo.jpg", 318_000, [0xAB; 16]);
        let rows = vec![MessageRecord {
            row_id: 0,
            message_id: Hex16::from([2; 16]),
            contact: PublicKey([7; 32]),
            direction: Direction::Incoming,
            kind: Kind::File { manifest: bytes },
            mls_generation: 0,
            ts_daemon_recv: 1_700_000_000,
            ts_envelope: 1_699_999_999,
        }];
        let mut avail = AvailMap::new();
        avail.insert([0xAB; 16], true);
        let out = render_messages_human(&rows, &avail);
        assert!(out.contains("photo.jpg"), "got {out}");
        assert!(out.contains("available"), "got {out}");
        // The old Debug dump must be gone.
        assert!(!out.contains("File {"), "still Debug-dumping: {out}");
    }

    #[test]
    fn clap_parses_chat_contact() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["skattr", "chat", "abc12"]).unwrap();
        match cli.cmd {
            Command::Chat { contact } => assert_eq!(contact, "abc12"),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn render_qr_ascii_produces_non_empty_output() {
        let url = "skattr://invite/v1#id=AAAA";
        let qr = render_invite_qr(url);
        assert!(!qr.is_empty(), "QR rendering must produce output");
        // Dense1x2 unicode rendering uses U+2580/U+2584 + space.
        assert!(qr.contains('\u{2580}') || qr.contains('\u{2584}') || qr.contains(' '));
    }

    #[test]
    fn render_export_text_line_contains_body_and_direction_label() {
        use skattr_core::daemon::commands::{Direction, MessageRecord};
        use skattr_core::daemon::hex::Hex16;
        use skattr_core::envelope::Kind;
        use skattr_core::identity::PublicKey;

        let rec = MessageRecord {
            row_id: 0, // row_id irrelevant in this test
            message_id: Hex16::from([0xCC; 16]),
            contact: PublicKey([0x42; 32]),
            direction: Direction::Incoming,
            kind: Kind::Text { body: "hi".into() },
            mls_generation: 1,
            ts_daemon_recv: 1_700_000_000,
            ts_envelope: 1_700_000_000,
        };
        let line = render_export_text_line(&rec, &AvailMap::new());
        assert!(line.starts_with('['));
        assert!(line.contains("peer"));
        assert!(line.contains("hi"));
        // Must include the RFC3339 date part for ts_daemon_recv = 1_700_000_000 (2023-11-14T22:13:20Z).
        assert!(line.contains("2023-11-14"));
    }

    #[test]
    fn parse_rfc3339_to_unix_seconds() {
        let secs = parse_rfc3339_to_unix("2026-01-01T00:00:00Z").unwrap();
        assert_eq!(secs, 1_767_225_600);
    }

    #[test]
    fn parse_rfc3339_rejects_garbage() {
        assert!(parse_rfc3339_to_unix("not a date").is_err());
    }

    #[test]
    fn render_search_results_human_includes_snippet_and_id() {
        use skattr_core::daemon::commands::{Direction, MessageRecord, SearchHitRecord};
        use skattr_core::daemon::hex::Hex16;
        use skattr_core::envelope::Kind;
        use skattr_core::identity::PublicKey;

        let rec = MessageRecord {
            row_id: 0, // row_id irrelevant in this test
            message_id: Hex16::from([0xAB; 16]),
            contact: PublicKey([0x42; 32]),
            direction: Direction::Incoming,
            kind: Kind::Text {
                body: "the merge conflict".into(),
            },
            mls_generation: 7,
            ts_daemon_recv: 1_700_000_000,
            ts_envelope: 1_700_000_000,
        };
        let hit = SearchHitRecord {
            record: rec,
            bm25: 0.5,
            snippet: "...the merge conflict...".to_string(),
        };
        let out = render_search_human(&[hit]);
        assert!(out.contains("merge conflict"));
        assert!(out.contains("epoch=7"));
        assert!(out.contains("ababab")); // first chars of message id
    }

    #[test]
    fn remove_subcommand_parses() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["skattr", "remove", "alice"]).unwrap();
        assert!(matches!(cli.cmd, Command::Remove { contact } if contact == "alice"));
    }

    #[test]
    fn render_event_message_received_matches_one_shot_format() {
        use skattr_core::daemon::commands::{Direction, MessageRecord};
        use skattr_core::daemon::hex::Hex16;
        use skattr_core::envelope::Kind;
        use skattr_core::identity::PublicKey;

        let rec = MessageRecord {
            row_id: 0, // row_id irrelevant in this test
            message_id: Hex16::from([0xDD; 16]),
            contact: PublicKey([0x42; 32]),
            direction: Direction::Incoming,
            kind: Kind::Text {
                body: "live update".into(),
            },
            mls_generation: 11,
            ts_daemon_recv: 1_700_000_900,
            ts_envelope: 1_700_000_900,
        };
        // Per-row formatter must match exactly what a one-shot dump of a single row produces.
        let line = render_message_record_human(&rec, &AvailMap::new());
        let one_shot = render_messages_human(std::slice::from_ref(&rec), &AvailMap::new());
        assert_eq!(one_shot.trim_end(), line.trim_end());
        assert!(line.contains("live update"));
    }

    fn file_row(id: [u8; 16], name: &str) -> skattr_core::daemon::commands::MessageRecord {
        use skattr_core::daemon::commands::{Direction, MessageRecord};
        use skattr_core::daemon::hex::Hex16;
        use skattr_core::envelope::Kind;
        use skattr_core::identity::PublicKey;
        MessageRecord {
            row_id: 0,
            message_id: Hex16::from([2; 16]),
            contact: PublicKey([7; 32]),
            direction: Direction::Incoming,
            kind: Kind::File {
                manifest: test_manifest_bytes(name, 10, id),
            },
            mls_generation: 0,
            ts_daemon_recv: 1_700_000_000,
            ts_envelope: 1_699_999_999,
        }
    }

    #[test]
    fn resolve_attachment_id_matches_a_unique_prefix() {
        let rows = vec![file_row([0xAB; 16], "a.bin"), file_row([0xCD; 16], "b.bin")];
        let (id, m) = resolve_attachment_id(&rows, "abab").unwrap();
        assert_eq!(id, [0xAB; 16]);
        assert_eq!(m.filename, "a.bin");
    }

    #[test]
    fn resolve_attachment_id_is_case_insensitive() {
        let rows = vec![file_row([0xAB; 16], "a.bin")];
        assert!(resolve_attachment_id(&rows, "ABAB").is_ok());
    }

    #[test]
    fn resolve_attachment_id_ambiguous_reports_the_count() {
        // Two ids sharing the queried prefix.
        let mut a = [0u8; 16];
        a[0] = 0xAB;
        let mut b = [0u8; 16];
        b[0] = 0xAB;
        b[15] = 0x01;
        let rows = vec![file_row(a, "a.bin"), file_row(b, "b.bin")];
        let err = resolve_attachment_id(&rows, "ab").unwrap_err().to_string();
        assert!(err.contains('2'), "should report the count: {err}");
        assert!(err.to_lowercase().contains("ambiguous"), "got {err}");
    }

    #[test]
    fn resolve_attachment_id_no_match_errors() {
        let rows = vec![file_row([0xAB; 16], "a.bin")];
        let err = resolve_attachment_id(&rows, "ffff")
            .unwrap_err()
            .to_string();
        assert!(err.contains("ffff"), "should quote the prefix: {err}");
    }

    #[test]
    fn resolve_attachment_id_ignores_text_rows() {
        use skattr_core::daemon::commands::{Direction, MessageRecord};
        use skattr_core::daemon::hex::Hex16;
        use skattr_core::envelope::Kind;
        use skattr_core::identity::PublicKey;
        let rows = vec![MessageRecord {
            row_id: 0,
            message_id: Hex16::from([2; 16]),
            contact: PublicKey([7; 32]),
            direction: Direction::Incoming,
            kind: Kind::Text {
                body: "abab".into(),
            },
            mls_generation: 0,
            ts_daemon_recv: 1_700_000_000,
            ts_envelope: 1_699_999_999,
        }];
        assert!(resolve_attachment_id(&rows, "abab").is_err());
    }
}
