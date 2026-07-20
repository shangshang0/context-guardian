# Changelog

## Unreleased

- Restore full documentation and MCP/managed-service controls for opt-in CC Switch map-reduce compression of oversized tool outputs.
- Add an opt-in message-format recovery preview that diagnoses unknown task failures, validates compacted/request message envelopes, writes privacy-preserving schema diffs, and applies only lossless repairs after backup.
- Add an optional ephemeral live Codex probe using the current user environment; when enabled, safe automatic repair requires a successful probe and never captures raw TLS requests, credentials, or message bodies.
- Add an opt-in passive loopback capture sidecar for exact Codex-to-local-provider request schemas without changing Provider, Base URL, configuration, process state, or routing. Raw bounded PCAPs are mode `0600` and deleted after schema-only extraction.
- Correlate failed wire requests with prior successful baselines using timestamps and hashed identifiers, and fail closed unless every relevant schema delta is a known lossless transformation.
- Store published historical images as lightweight `input_text` references so Codex CLI can resume guarded tasks without rejecting remote `input_image` URLs.
- Retain the short-lived signed URL for explicit agent or direct API retrieval without restoring Base64 data to rollout history.

## 0.4.1

- Harden tenant isolation with secret-derived IDs, constant-time authentication, registration proof of work, bounded bodies/queues, and cross-tenant inflight protection.
- Add atomic identity creation and migration, automatic re-registration after Relay restart, proxy-safe loopback access, and MCP child-output limits.
- Make launchd generation safe for nonstandard user paths and automatically inject each user's mode-`0600` image publishing configuration into managed guardians.
- Open-source a self-hosted Docker Relay with automatic HTTPS, no image persistence, no host mounts, pinned runtime images, and documented security boundaries.
- Validate Skill metadata, MCP contracts, Shell scripts, Docker Compose, Rust Clippy/tests, RustSec advisories, and two-tenant end-to-end GPT image recognition.
- Rewrite the project and self-hosted Relay documentation with matched English and Simplified Chinese guides, clearer quick starts, deployment choices, threat boundaries, and troubleshooting prerequisites.

## 0.4.0

- Add an install-and-use multi-tenant Rust Relay client and server for signed image URLs without SSH accounts or inbound home-network ports.
- Generate independent 128-bit tenant IDs and 256-bit tenant secrets automatically; store client identity with mode `0600` and only secret hashes on the Relay.
- Isolate tenant queues and credentials, return uniform `404` responses for cross-tenant access and scans, and enforce global/per-tenant limits.
- Add hardened Docker deployment, macOS background services, optional proxy support, MCP management, and an SSH-alias-only self-hosted fallback.
- Keep network image publishing opt-in for each guardian. The first Relay protocol does not persist images but Relay operators can observe transient bytes; self-host for sensitive images.

## 0.3.0

- Add optional signed, expiring HTTPS image URLs backed by an IPv6 Rust gateway.
- Add content-addressed image caching and automatic safe fallback to `input_text`.
- Require an HTTPS base URL; bare IPv6 HTTP URLs are not written into model requests.

## 0.2.1

- Convert scrubbed image output objects from `input_image` to protocol-valid `input_text` records.
- Migrate legacy placeholder strings previously written into `image_url`.
- Treat invalid `image_url` task failures as recoverable guardian events.
- Retain a five-second SQLite busy timeout during live app-server contention.

## 0.2.0

- Initial public release with Rust CLI, MCP server, Agent Skill, and per-task service management.
