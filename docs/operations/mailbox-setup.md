# Skattr mailbox operator guide

A skattr mailbox is a semi-trusted relay that holds encrypted
deposits for offline recipients. Operators learn:

- That a particular pubkey hash has deposits waiting (via DEPOSIT).
- That a recipient is online (via FETCH/DELETE timing).

Operators **do not** learn message contents, sender identity, or
who is communicating with whom (that's bound to recipient hashes,
not pubkeys).

This guide walks an operator from a fresh VM to a running mailbox in
under 30 minutes.

## Choosing your install path

| Path        | When                                                                                    |
|-------------|-----------------------------------------------------------------------------------------|
| systemd     | Production on a Debian/Ubuntu/Arch host. Best ergonomics + hardening.                   |
| Docker      | Container-first deployments. AGPL-compatible registries only.                           |
| from-source | Custom builds, dev systems, hosts without systemd or Docker. Ten extra minutes.         |

## Path A — systemd (Debian / Ubuntu)

### Prerequisites

- Linux with systemd 247+ (`systemctl --version`).
- A user account with sudo.
- TCP egress to the Tor network.

### Install

```bash
# 1. Install Rust toolchain (skip if already present).
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"

# 2. Build and install the binary.
git clone https://github.com/myggiz/skattr
cd skattr
cargo build --release -p skattr-mailbox
sudo install -Dm755 target/release/skattr-mailbox /usr/local/bin/skattr-mailbox

# 3. Drop in the systemd unit and config skeleton.
sudo install -Dm644 packaging/systemd/skattr-mailbox.service \
    /etc/systemd/system/skattr-mailbox.service
sudo install -d -m755 /etc/skattr-mailbox
sudo tee /etc/skattr-mailbox/mailbox.toml > /dev/null <<'EOF'
[server]
data_dir = "/var/lib/skattr-mailbox"

[policy]
max_deposit_size           = 1048576
min_ttl_secs               = 3600
max_ttl_secs               = 2592000
default_ttl_secs           = 604800
recipient_cap_bytes        = 268435456
per_conn_deposits_per_min  = 30
per_conn_fetches_per_min   = 6
global_deposits_per_min    = 1000
EOF

# 4. Start and enable.
sudo systemctl daemon-reload
sudo systemctl enable --now skattr-mailbox
sudo systemctl status skattr-mailbox
```

### Discovering your onion address

```bash
sudo journalctl -u skattr-mailbox --since "10 minutes ago" \
    | grep "mailbox onion published"
```

The line ends with `onion=<your-onion>.onion`. Hand that to your users.

### Healthchecks

```bash
sudo socat - UNIX-CONNECT:/var/lib/skattr-mailbox/health.sock <<<"GET /health"
```

Replies `ok` or `degraded: <reason>`.

## Path B — Docker

```bash
git clone https://github.com/myggiz/skattr
cd skattr
docker build -f packaging/Dockerfile -t skattr-mailbox:latest .

mkdir -p mailbox-data
cp packaging/docker-compose.example.yml docker-compose.yml
# Drop a mailbox.toml next to docker-compose.yml (template above).
docker compose up -d
docker compose logs -f mailbox | grep "mailbox onion published"
```

## Path C — from-source, no init system

`skattr-mailbox --config /path/to/mailbox.toml` runs in the foreground.
Wrap with `tmux` / `screen` / `nohup` as you prefer. `SIGINT` /
`SIGTERM` shuts down cleanly.

## Configuration reference

See `crates/mailbox/src/config.rs` for the canonical schema. Key knobs:

| Field                                  | Default      | What it does                                           |
|----------------------------------------|--------------|--------------------------------------------------------|
| `[server].data_dir`                    | _required_   | Parent for all server state.                           |
| `[policy].max_deposit_size`            | 1 048 576    | Bytes; deposits above this get `TooLarge`.             |
| `[policy].min_ttl_secs`                | 3 600        | TTL clamp lower bound.                                 |
| `[policy].max_ttl_secs`                | 2 592 000    | TTL clamp upper bound (30 days).                       |
| `[policy].default_ttl_secs`            | 604 800      | TTL when client requests `0` (7 days).                 |
| `[policy].recipient_cap_bytes`         | 268 435 456  | Per-recipient byte cap (256 MiB).                      |
| `[policy].per_conn_deposits_per_min`   | 30           | Token bucket per inbound stream.                       |
| `[policy].per_conn_fetches_per_min`    | 6            | Token bucket per inbound stream.                       |
| `[policy].global_deposits_per_min`     | 1 000        | Server-wide cap; brake against reconnect storms.       |

## Backup

```bash
sqlite3 /var/lib/skattr-mailbox/mailbox.sqlite ".backup '/var/backups/mailbox-$(date +%Y%m%d).bak'"
```

WAL mode means no quiesce required. Restore by copying the `.bak` file
back into place while the service is stopped.

## Upgrade

Migrations are forward-only. Stop the service, swap the binary, restart.
Any in-flight requests are rejected cleanly via the 30 s `TimeoutStopSec`.

```bash
sudo systemctl stop skattr-mailbox
sudo install -Dm755 target/release/skattr-mailbox /usr/local/bin/skattr-mailbox
sudo systemctl start skattr-mailbox
```

## Troubleshooting

| Symptom                                     | Likely cause + fix                                                                                                  |
|---------------------------------------------|---------------------------------------------------------------------------------------------------------------------|
| `degraded: db_unavailable`                  | SQLite file missing or unreadable. Check `data_dir` perms; `ls -la $data_dir/mailbox.sqlite`.                        |
| `degraded: arti_not_bootstrapped`           | Tor failed to bootstrap. Check egress; tail `journalctl -u skattr-mailbox` for `arti` errors.                       |
| `RateLimited` floods                        | Either real traffic exceeds capacity or an attacker is reconnecting. Tighten `global_deposits_per_min` first.        |
| Storage growing past `recipient_cap_bytes`  | Should be impossible per cap eviction. File a bug + attach the soak test output.                                    |
| Repeated `Internal` replies                 | Server-side bug. Capture the `journalctl` trace at `error` level and file an issue.                                  |

## What this server does NOT do

- It does not federate or forward. Each mailbox is standalone.
- It does not register operators with any directory. If you want
  others to use your mailbox, share the onion out-of-band.
- It does not expose metrics over the network. All metrics are local
  log lines under `tracing` `info` level.
- It does not log identity hashes, pubkeys, or ciphertexts above
  `debug`. The redaction unit test enforces this.
