# Context Guardian

[English](README.md) | [简体中文](README.zh-CN.md)

Context Guardian is a Rust sidecar for inspecting, recovering, and continuously protecting Codex task contexts. It also provides an optional signed-image bypass that keeps large Base64 image bodies out of rollout history while preserving GPT vision through short-lived HTTPS URLs. The preview blind TLS mode keeps image plaintext hidden from an honest-but-curious public Relay.

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
v1: GPT HTTPS -> Relay terminates HTTPS -> outbound client polling -> local HTTP gateway
v2: GPT HTTPS -> Relay reads SNI -> opaque TLS over authenticated WSS -> local TLS gateway
```

Images remain in the local cache and neither mode persists image bytes at the Relay. In compatible v1, the Relay operator can observe transient image bytes and traffic metadata. In preview v2, TLS terminates only at the local gateway, so the Relay sees SNI, IP addresses, timing, and ciphertext sizes but not the signed URL, HTTP headers, or image plaintext.

## Requirements

- Current stable Rust toolchain.
- macOS for the install-and-use public Relay background services.
- macOS or Linux for the Guardian CLI and managed Guardian service.
- Node.js 18+ only when using MCP.
- Codex state under `$CODEX_HOME` or `${HOME}/.codex`.
- Preview v2 additionally requires public TCP `443`, wildcard DNS for the blind suffix, and either a matching certificate/key or `acme.sh` plus OpenSSL for local TLS-ALPN issuance.

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

## API-assisted tool-output compression

Guardian can use a trusted OpenAI-compatible API to summarize oversized historical tool outputs instead of replacing them with a generic pruning notice. This is opt-in because the selected endpoint receives the original tool output:

```sh
context-guardian --thread-id 019f... --once \
  --enable-cc-switch-summary
```

The defaults target the local CC Switch endpoint and model:

```sh
context-guardian --thread-id 019f... --once \
  --enable-cc-switch-summary \
  --cc-switch-url http://127.0.0.1:15721/v1/chat/completions \
  --cc-switch-model feature/gpt-5.6-sol \
  --cc-switch-chunk-target-tokens 120000 \
  --large-tool-output-bytes 160000
```

Only `function_call_output` records at or above the size threshold are sent. Inline image outputs use the separate image-cleanup path and are not summarized. Large text is split and reduced for at most four rounds while asking the model to preserve paths, commands, errors, test results, and decisions. Guardian backs up the rollout before replacement. If the API request fails or returns an invalid response, recovery continues with the ordinary pruning notice rather than leaving the oversized body in context.

Use only an endpoint and model you trust with the original output. The endpoint must implement `POST /v1/chat/completions`; each request has a 20-second timeout. This feature compresses oversized tool results—it does not regenerate a missing Codex compaction summary or reconstruct information already lost from history.

For a managed Guardian, set the equivalent MCP `guardian_service` fields or install with environment variables:

```sh
CONTEXT_GUARDIAN_CC_SWITCH_SUMMARY=1 \
CONTEXT_GUARDIAN_CC_SWITCH_URL=http://127.0.0.1:15721/v1/chat/completions \
CONTEXT_GUARDIAN_CC_SWITCH_MODEL=feature/gpt-5.6-sol \
./scripts/service.sh install 019f... ./target/release/context-guardian
```

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

### Preview blind TLS Relay (v2)

v2 keeps v1 available and changes only the image transport. The public listener on port `443` parses the ClientHello SNI for an exact `<tenant_id>.<blind_suffix>` hostname, then forwards the untouched inner TLS stream through authenticated WSS control slots on the existing Relay HTTPS port. TLS and signed-URL validation happen at `127.0.0.1:8788`.

After installing the binaries, configure local TLS with either an existing certificate pair:

```sh
CONTEXT_RELAY_BLIND_CERT_FILE=/absolute/path/fullchain.pem \
CONTEXT_RELAY_BLIND_KEY_FILE=/absolute/path/private-key.pem \
./scripts/setup-blind-relay.sh install \
  https://relay.example.com:5003 relay.example.com
```

Or let local `acme.sh` obtain the exact tenant certificate through a temporary blind tunnel on `127.0.0.1:8789`:

```sh
./scripts/setup-blind-relay.sh install \
  https://relay.example.com:5003 relay.example.com admin@example.com
```

Renew automatic certificates by rerunning the same command with `renew`. This is intentionally an explicit renewal because ACME validation needs the temporary tunnel. Check services with `status`. `remove` retains certificates, identity, signing key, and cache; pass a Relay URL to `remove` only when v1 should be restored.

The strongest arrangement uses a client-owned DNS suffix and certificate on a dedicated/self-hosted Relay deployment, so the Relay operator cannot issue an alternate certificate. With a shared operator-owned suffix, the private key generated by `acme.sh` remains local and protects against passive or honest-but-curious operation, but the domain owner could actively issue another valid certificate and MITM a future connection. v2 does not hide traffic metadata.

### Self-hosted Docker Relay

The Relay server is fully open source. Deploy it on a trusted public server with your own domain:

```sh
cd relay
cp .env.example .env
# Set CONTEXT_RELAY_DOMAIN and CONTEXT_RELAY_ACME_EMAIL.
# Set CONTEXT_RELAY_BLIND_SUFFIX to the wildcard DNS suffix to enable v2.
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
- `passive_capture_service`: install/remove/status for the optional macOS packet-capture sidecar.
- `relay_client_service`: install/remove/status for the optional Relay client; mutations require confirmation.
- `blind_relay_service`: install/renew/remove/status for preview v2; accepts either a local certificate pair or an ACME email.

The MCP validates task IDs, image parameters, and CC Switch endpoint/model settings, and kills child processes whose output exceeds 1 MiB.

`recover_context` and `guardian_service` expose `cc_switch_summary`, `cc_switch_url`, `cc_switch_model`, `cc_switch_chunk_target_tokens`, and the large-output threshold. `recover_context` also accepts the preview fields `message_format_preview`, `message_format_live_probe`, `message_format_passive_capture`, probe settings, and passive-capture report/window settings. `guardian_service` accepts the three preview booleans during installation. `passive_capture_service` manages the macOS sidecar separately. Live probing and passive-capture gating both require message-format preview.

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
- Blind v2 routes only exact 32-hex tenant SNI names, limits waiting slots/connections/ClientHello size/lifetime, and forwards TLS records without terminating them.

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
- Compatible v1 exposes transient image bytes and traffic metadata to the Relay operator.
- Preview v2 hides URL/header/image plaintext from a passive Relay, but not SNI, IP, timing, or ciphertext size; a shared-domain owner can still mount an active certificate-substitution attack.
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
