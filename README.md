# Context Guardian

[English](README.md) | [简体中文](README.zh-CN.md)

Context Guardian is a Rust sidecar for inspecting, recovering, and continuously protecting Codex task contexts. It also provides an optional signed-image bypass that keeps large Base64 image bodies out of rollout history while preserving GPT vision through short-lived HTTPS URLs.

## What it solves

- Recovers tasks stuck in context-window failure loops.
- Repairs stale per-task token counters without changing global Codex settings.
- Removes oversized inline image/Base64 bodies and tool outputs from persisted rollout JSONL.
- Preserves existing compacted summaries and the active conversation tail.
- Optionally publishes scrubbed images through a signed, expiring URL.
- Supports a default public multi-tenant Relay or a fully self-hosted Docker Relay.

Context Guardian operates on one explicit task/thread ID at a time. It creates backups before high-value rewrites and fails closed when local Codex paths or schemas do not match expectations.

## Architecture

```text
Codex rollout/state
        │
        ▼
context-guardian ── repairs scoped task state
        │ optional signed image publishing
        ▼
local Rust gateway ([::1]:8787)
        │ outbound HTTPS polling
        ▼
public or self-hosted Relay
        │ short-lived signed URL
        ▼
GPT image fetcher
```

Images remain in the local cache. The Relay does not persist image bytes. In the current protocol, however, the Relay operator can observe transient image bytes and traffic metadata; self-host the Relay for sensitive images.

## Requirements

- Current stable Rust toolchain.
- macOS for the install-and-use public Relay background services.
- macOS or Linux for the Guardian CLI and managed Guardian service.
- Node.js 18+ only when using MCP.
- Codex state under `$CODEX_HOME` or `${HOME}/.codex`.

SQLite is bundled into the Rust binary.

## Quick start

```sh
git clone https://github.com/shangshang0/shangTools.git
cd shangTools/context-guardian
./scripts/install.sh
```

On macOS, installation automatically:

1. Builds and installs the Guardian, local image gateway, Relay client, MCP server, and service scripts.
2. Generates an independent 256-bit tenant secret and 128-bit derived tenant ID.
3. Stores identity and image signing material in mode-`0600` files.
4. Starts the loopback-only image gateway and public Relay client.
5. Writes per-user image publishing values to `$CODEX_HOME/context-guardian/image-publishing.env`.

Network image publishing remains opt-in per guarded task. Disable public Relay setup during installation with:

```sh
CONTEXT_GUARDIAN_SKIP_PUBLIC_RELAY=1 ./scripts/install.sh
```

Preview generated launchd configuration without starting services:

```sh
CONTEXT_GUARDIAN_DRY_RUN=1 ./scripts/install.sh
```

## Guardian CLI

Inspect without mutation:

```sh
context-guardian --thread-id 019f... --status
```

Run one scoped recovery pass:

```sh
context-guardian --thread-id 019f... --once
```

Run continuously in the foreground:

```sh
context-guardian --thread-id 019f...
```

The rollout path is discovered from `state_5.sqlite`. Override `--rollout`, `--state-db`, or `--goals-db` only for custom layouts.

## Managed Guardian service

```sh
./scripts/service.sh install 019f... ./target/release/context-guardian
./scripts/service.sh status 019f... ./target/release/context-guardian
./scripts/service.sh remove 019f... ./target/release/context-guardian
```

On macOS, `service.sh install` automatically reads the active user's mode-`0600` image publishing configuration and injects the four image arguments. It never reads another user's HOME or identity.

## Image publishing modes

### Default public Relay

The default macOS installation uses the project-operated HTTPS Relay. No SSH account, inbound home-network port, or manual client key is required.

Each client:

- creates its own secret locally;
- derives its tenant ID from that secret;
- computes a lightweight registration proof;
- authenticates polling and responses independently;
- receives uniform `404` responses for invalid credentials and path scans.

Signed image URLs expire after 900 seconds by default. If publishing fails, Guardian falls back to a protocol-valid text placeholder.

### Self-hosted Docker Relay

The Relay server is fully open source. Deploy it on a trusted public server with your own domain:

```sh
cd relay
cp .env.example .env
# Set CONTEXT_RELAY_DOMAIN and CONTEXT_RELAY_ACME_EMAIL.
docker compose up -d --build
```

Caddy obtains and renews HTTPS certificates automatically on ports 80/443. Point clients to it during installation:

```sh
CONTEXT_RELAY_URL=https://relay.example.com ./scripts/install.sh
```

See [relay/README.md](relay/README.md) for the complete server deployment and security model.

### SSH alias fallback

Single-user/self-hosted deployments can use a restricted SSH reverse tunnel instead of the multi-tenant Relay:

```sh
./scripts/image-tunnel.sh install image-relay 5003 28787
```

`image-relay` must be a plain alias from `~/.ssh/config`. The script rejects raw usernames, hostnames, IP literals, and passwords. Restrict the authorized key with `no-agent-forwarding,no-X11-forwarding,no-pty,permitlisten="0.0.0.0:5003"`.

## MCP

Configure the stdio server command:

```sh
node /absolute/path/to/shangTools/context-guardian/mcp/server.mjs
```

Tools:

- `inspect_context`: read-only task inspection.
- `recover_context`: one scoped recovery; requires `confirm=true`.
- `guardian_service`: install/remove/status for a per-task service; mutations require confirmation.
- `relay_client_service`: install/remove/status for the optional Relay client; mutations require confirmation.

The MCP validates task IDs and image parameters, and kills child processes whose output exceeds 1 MiB.

## Agent Skill

The bundled Skill is under `skill/context-guardian`. It guides agents through scoped inspection, recovery, continuous guarding, and safe image publishing. Validate it with:

```sh
python3 /path/to/skill-creator/scripts/quick_validate.py skill/context-guardian
```

## Security model

- Exact single-task scope; rollout paths must contain the supplied task ID.
- Backups before rollout or database rewrites.
- Loopback-only Rust image gateway.
- Content-addressed image filenames and HMAC-SHA256 expiry signatures.
- Independent client identities stored as mode-`0600` files.
- Secret-derived tenant IDs, registration proof of work, constant-time authentication.
- Bounded request bodies, queues, inflight requests, memory, CPU, PIDs, and logs.
- Uniform `404` responses for invalid tenants, credentials, signatures, and scans.
- Relay Docker container is non-root, read-only, capability-free, and has no host mounts.
- HTTPS container keeps only `NET_BIND_SERVICE` for ports 80/443.

Read [SECURITY.md](SECURITY.md) before operating a public Relay. Report vulnerabilities privately through GitHub Security Advisories.

## Proxy support

The Relay client supports standard proxy environment variables, including SOCKS:

```sh
HTTP_PROXY=http://127.0.0.1:8080 \
HTTPS_PROXY=http://127.0.0.1:8080 \
ALL_PROXY=socks5h://127.0.0.1:1080 \
./scripts/install.sh
```

Local gateway requests always bypass proxy environment variables.

## Limitations

- Recovery cannot recreate details already lost from a poor compaction summary.
- A live Codex app-server may briefly rewrite stale counters; daemon mode converges them again.
- Codex local schemas may evolve; unknown layouts fail closed.
- The public Relay does not persist images, but its operator can observe transient bytes and traffic metadata.
- Managed Relay client setup is currently macOS-only; the Rust client itself is portable.

## Development and release checks

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo audit --file Cargo.lock

cargo fmt --check --manifest-path relay/Cargo.toml
cargo clippy --manifest-path relay/Cargo.toml --all-targets --all-features -- -D warnings
cargo test --manifest-path relay/Cargo.toml
cargo audit --file relay/Cargo.lock

shellcheck -x scripts/*.sh skill/context-guardian/scripts/*.sh
node --check mcp/server.mjs
docker compose -f relay/compose.yaml config
```

## License

MIT
