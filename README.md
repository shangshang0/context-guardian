# Context Guardian

[English](README.md) | [简体中文](README.zh-CN.md)

Context Guardian is a Rust sidecar for inspecting, recovering, and continuously protecting Codex task contexts. It also provides an optional signed-image bypass that keeps large Base64 image bodies out of rollout history while preserving GPT vision through short-lived HTTPS URLs.

## What it solves

- Recovers tasks stuck in context-window failure loops.
- Repairs stale per-task token counters without changing global Codex settings.
- Removes oversized inline image/Base64 bodies and tool outputs from persisted rollout JSONL.
- Preserves existing compacted summaries and the active conversation tail.
- Preview: diagnoses and safely repairs message-envelope damage after unknown task failures.
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
git clone https://github.com/shangshang0/context-guardian.git
cd context-guardian
./scripts/install.sh
```

On macOS, installation automatically:

1. Builds and installs the Guardian, passive loopback capture sidecar, local image gateway, Relay client, MCP server, and service scripts.
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

## Message format recovery preview

Enable structural validation and safe automatic repair after an unknown task error:

```sh
context-guardian --thread-id 019f... --once \
  --enable-message-format-preview
```

Also issue one minimal live request through the current user's Codex CLI, authentication, model, provider, and proxy environment:

```sh
context-guardian --thread-id 019f... --once \
  --enable-message-format-preview \
  --enable-message-format-live-probe
```

The preview validates compacted `replacement_history`, message roles/content blocks, function arguments, and tool outputs. It normalizes only lossless cases such as stringified history, string content that should be a typed array, role-mismatched `input_text`/`output_text`, or structured tool arguments that should be JSON strings. If any difference cannot be repaired without guessing, the rollout is left unchanged.

The live probe is ephemeral, uses an empty temporary working directory, cannot write to the workspace, and is asked not to call tools. Its output is discarded. It confirms that the current user environment can produce a healthy request; it does not MITM TLS, capture authorization headers, or store raw request/message bodies. A probe consumes one minimal model request and must succeed before automatic repair when live probing is enabled.

Before an applied repair, Guardian backs up the rollout and removes the unknown failure record that would otherwise retrigger the broken turn. Schema-only reports are written mode `0600` under `$CODEX_HOME/context-guardian/message-format-reports`; reports contain field paths and value types, never message text or credentials.

### Exact passive request capture

For an exact wire-format comparison, start the opt-in sidecar before the failure occurs:

```sh
./scripts/passive-capture-service.sh install

context-guardian --thread-id 019f... --once \
  --enable-message-format-preview \
  --enable-message-format-passive-capture
```

The sidecar passively listens on `lo0`, TCP port `15721` by default. It never edits `~/.codex/config.toml`, Provider, Base URL, environment, Codex process state, or routing. In a common CC Switch setup, the relevant first hop is `Codex -> plaintext HTTP 127.0.0.1:15721 -> CC Switch`; this makes the exact Codex request observable without TLS interception.

Capture windows are bounded by time and size, and the sidecar retains at most 100 reports by default. The temporary PCAP is mode `0600`, is processed locally, and is deleted immediately after parsing. Persisted mode-`0600` reports contain only exact JSON paths/types, allowlisted `role`/`type` enums, timestamps, sizes, and SHA-256 hashes. Authorization and other header values, request bodies, response bodies, message scalar values, and raw identifiers are never written to reports. HTTP/1.1 reassembly supports `Content-Length`, chunked transfer coding, and gzip content coding.

On an unknown failure, Guardian correlates the closest failed request with a prior successful request by timestamp, hashed identifiers, and hashed target. Passive-capture gating is fail closed: automatic repair is allowed only when the rollout repair is independently lossless and every relevant wire-schema difference is a known lossless transformation. Missing evidence, no successful baseline, or an ambiguous difference leaves the rollout unchanged.

The three diagnostic levels are distinct:

- Rollout inference validates the local persisted message envelope.
- Passive loopback capture records the exact plaintext request Codex sent to the local provider bridge.
- Upstream TLS inspection can show a request transformed by CC Switch only when that process already exported TLS session secrets during the handshake. Past TLS sessions cannot be decrypted afterward. Guardian does not restart, inject into, or change CC Switch/Codex to obtain keys, and the current preview does not claim upstream visibility when keys are unavailable.

The macOS service requires the current user to have BPF access (`tcpdump -D` must succeed). On other platforms, run `context-guardian-passive-capture --watch` with the minimum packet-capture capability required by the OS. Remove the macOS sidecar with `./scripts/passive-capture-service.sh remove`; schema-only reports are retained.

Enable the preview for a newly installed managed service:

```sh
CONTEXT_GUARDIAN_MESSAGE_FORMAT_PREVIEW=1 \
CONTEXT_GUARDIAN_MESSAGE_FORMAT_LIVE_PROBE=1 \
./scripts/service.sh install 019f... ./target/release/context-guardian
```

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

Signed image URLs expire after 900 seconds by default. Guardian stores a published image as a protocol-valid `input_text` reference, not a remote `input_image`: current Codex CLI releases reject remote image URLs while rebuilding historical context. Direct API clients or agents may fetch the signed URL explicitly before it expires. If publishing fails, Guardian falls back to a text-only placeholder.

### Self-hosted Docker Relay

The Relay server is fully open source. Deploy it on a trusted public server with your own domain:

```sh
cd relay
cp .env.example .env
# Set CONTEXT_RELAY_DOMAIN and CONTEXT_RELAY_ACME_EMAIL.
docker compose up -d --build
```

Caddy uses port 80 for automatic certificate issuance and exposes HTTPS on ports 5003/5004. Point clients to it during installation:

```sh
CONTEXT_RELAY_URL=https://relay.example.com:5003 ./scripts/install.sh
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
node /absolute/path/to/context-guardian/mcp/server.mjs
```

Tools:

- `inspect_context`: read-only task inspection.
- `recover_context`: one scoped recovery; requires `confirm=true`.
- `guardian_service`: install/remove/status for a per-task service; mutations require confirmation.
- `relay_client_service`: install/remove/status for the optional Relay client; mutations require confirmation.

The MCP validates task IDs and image parameters, and kills child processes whose output exceeds 1 MiB.

`recover_context` also accepts the preview fields `message_format_preview`, `message_format_live_probe`, `message_format_passive_capture`, probe settings, and passive-capture report/window settings. `guardian_service` accepts the three preview booleans during installation. `passive_capture_service` manages the macOS sidecar separately. Live probing and passive-capture gating both require message-format preview.

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
- Message-format preview cannot reconstruct missing semantic content; it repairs only structural transformations that are lossless.
- Passive capture sees only traffic that occurs while the sidecar is running; it cannot recover a past plaintext request or decrypt a past TLS session.
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
