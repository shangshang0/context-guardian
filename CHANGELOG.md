# Changelog

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
