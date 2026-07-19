# Context Guardian

Agent-oriented sidecar for inspecting, recovering, and continuously protecting Codex task contexts. It lives entirely in this subdirectory of `shangTools` and operates on one explicit task ID at a time.

## Why it exists

Long-running tasks can become unusable because of stale token counters, context-window failure loops, inline image bodies, historical image attachments, or oversized tool outputs. Context Guardian repairs the persisted rollout and local indexes without proxying model requests or changing global Codex settings.

The default recovery value is `100000` tokens. This is an index recovery value, not a guarantee that all retained context is semantically useful.

## Components

- `context-guardian`: Rust CLI and watchdog.
- `context-image-gateway`: signed, expiring IPv6 image origin.
- `mcp/server.mjs`: dependency-free stdio MCP server for agents.
- `skill/context-guardian`: installable Codex Agent Skill.
- `scripts/service.sh`: per-task launchd/systemd user service manager.
- `scripts/image-tunnel.sh`: optional SSH-alias-only reverse tunnel manager.
- `relay/`: multi-tenant Relay server and auto-provisioning Rust client.
- `scripts/install.sh`: local release installer.

## Requirements

- A current stable Rust toolchain to build from source.
- Node.js 18+ only when using MCP.
- Codex local state under `$CODEX_HOME` or `${HOME}/.codex`.
- macOS or Linux for managed background services. The CLI itself builds on other Rust targets, but service installation is intentionally unsupported there.

SQLite is bundled into the Rust binary; users do not need the `sqlite3` command.

## Install

```sh
git clone https://github.com/shangshang0/shangTools.git
cd shangTools/context-guardian
./scripts/install.sh
```

## CLI

Inspect without mutation:

```sh
context-guardian --thread-id 019f... --status
```

Run one scoped recovery pass:

```sh
context-guardian --thread-id 019f... --once
```

Run in the foreground as a watchdog:

```sh
context-guardian --thread-id 019f...
```

The rollout path is discovered from `state_5.sqlite`. Override `--rollout`, `--state-db`, or `--goals-db` only for advanced/custom layouts.

## Background service

```sh
./scripts/service.sh install 019f... ./target/release/context-guardian
./scripts/service.sh status 019f... ./target/release/context-guardian
./scripts/service.sh remove 019f... ./target/release/context-guardian
```

This installs a per-user launchd agent on macOS or a systemd user unit on Linux.

## MCP

Build first, then configure an stdio MCP server whose command is:

```sh
node /absolute/path/to/shangTools/context-guardian/mcp/server.mjs
```

Exposed tools:

- `inspect_context`: read-only status inspection.
- `recover_context`: one recovery pass; requires `confirm=true`.
- `guardian_service`: install/remove/status for a per-task daemon; mutations require confirmation.
- `relay_client_service`: install/remove/status for the optional auto-provisioned public Relay client; mutations require confirmation.

Set `CONTEXT_GUARDIAN_BIN` if the binary is not under `target/release/` relative to the MCP service.

## Optional signed image URLs

OpenAI accepts a fully qualified image URL or a Base64 data URL. To keep scrubbed images readable without retaining Base64 in the rollout, Context Guardian can copy image bytes into an isolated cache and replace the data URI with a short-lived signed HTTPS URL.

This requires a public HTTPS domain and a trusted TLS certificate. A naked HTTP or literal-IP URL is intentionally rejected because model-side fetchers commonly block those origins. Network publishing remains disabled unless both `--image-base-url` and `--image-signing-key-file` are explicitly supplied.

Create a signing key and start the IPv6 origin:

```sh
mkdir -p ~/.codex/context-guardian/images
openssl rand 32 > ~/.codex/context-guardian/image-signing.key
chmod 600 ~/.codex/context-guardian/image-signing.key
context-image-gateway \
  --listen '[::1]:8787' \
  --cache-dir ~/.codex/context-guardian/images \
  --signing-key-file ~/.codex/context-guardian/image-signing.key
```

Put Caddy, Nginx, or another trusted HTTPS reverse proxy in front of the gateway and publish a domain such as `https://images.example.com`. Keep the Rust gateway on loopback so it cannot expose unrelated local files.

If the local network cannot accept public connections, use a server that you control as a TCP-only SSH relay. Define its address and user under a plain alias in `~/.ssh/config`, install a restricted public key on that server, and never pass an IP address, username, password, or private-key content to the tunnel manager:

```sh
./scripts/image-tunnel.sh install image-relay 5003 28787
./scripts/image-tunnel.sh status image-relay 5003 28787
```

The relay listens on TCP `5003` and forwards encrypted traffic to the local HTTPS proxy on `127.0.0.1:28787`. It does not receive filesystem access or image-cache paths. Restrict the authorized key with `no-agent-forwarding,no-X11-forwarding,no-pty,permitlisten="0.0.0.0:5003"`, allow only that TCP port in the server firewall/security group, and rotate any password that was ever shared in plaintext.

### Install-and-use public Relay mode

For users who do not operate an SSH server, the bundled Rust Relay client generates a random 128-bit tenant ID and 256-bit tenant secret on first start. The identity is saved only in a local mode-`0600` file. The Relay stores only the secret hash and never creates an image directory.

```sh
./scripts/install.sh
```

On macOS the installer enables the public Relay automatically. Set `CONTEXT_GUARDIAN_SKIP_PUBLIC_RELAY=1` to install binaries without network image support, or set `CONTEXT_RELAY_URL` to a self-hosted Relay. The Rust guardian itself remains opt-in: pass the four values written to `~/.codex/context-guardian/image-publishing.env` only to guarded tasks that should preserve image URLs.

No SSH account, inbound port, or manually created key is required. Each public image URL contains its tenant ID and the existing short-lived image signature. A different tenant secret cannot poll or submit another tenant's requests; invalid tenants, bad credentials, and scanned image paths all return the same `404` response.

The installer initializes the identity before starting the daemon. Client logs never print the tenant secret and normal daemon startup does not print the tenant ID.
Standard `HTTP_PROXY`, `HTTPS_PROXY`, and `ALL_PROXY` environment variables are supported, including SOCKS proxies, for networks that filter nonstandard HTTPS ports.

The minimal Relay protocol carries image bytes through server memory and does not persist them. Operators can deploy `relay/compose.yaml` behind a trusted HTTPS reverse proxy. The container is non-root, read-only, capability-free, resource-limited, and mounts only a small tenant-hash volume. Sensitive deployments should self-host because the Relay operator can still observe transient image bytes in this first protocol version.

Enable publishing for one guardian:

```sh
context-guardian --thread-id 019f... \
  --image-base-url https://images.example.com \
  --image-signing-key-file ~/.codex/context-guardian/image-signing.key \
  --image-cache-dir ~/.codex/context-guardian/images \
  --image-url-ttl-seconds 900
```

If publishing fails, Guardian safely falls back to an `input_text` placeholder.

## Recovery behavior

- Strict single-thread scope and rollout-path validation.
- Structural placeholders for inline and historical images.
- Image tool outputs are downgraded to protocol-valid `input_text` items; legacy placeholder values incorrectly stored in `image_url` are migrated automatically.
- Pruning of oversized tool output; optional trusted CC Switch summarization.
- Preservation of existing compacted summaries and active history tails.
- Targeted SQLite counter repair with a five-second busy timeout.
- Backups before image, large-output, history-folding, or normalized-token rewrites.

## Limitations

- This tool protects persisted state; it cannot reconstruct details already lost from a poor compaction summary.
- A live app-server can briefly rewrite stale counters. Daemon mode continuously converges them.
- Codex local schemas may evolve. Scope validation intentionally fails closed when expected files or fields are absent.

## Development

```sh
cargo fmt --check
cargo test
cargo build --release
cargo test --manifest-path relay/Cargo.toml
node --check mcp/server.mjs
sh -n scripts/install.sh
sh -n scripts/service.sh
sh -n scripts/image-tunnel.sh
sh -n scripts/relay-client.sh
sh -n scripts/setup-public-relay.sh
python3 /path/to/skill-creator/scripts/quick_validate.py skill/context-guardian
```

## License

MIT
