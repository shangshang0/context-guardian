# Changelog

## 0.2.1

- Convert scrubbed image output objects from `input_image` to protocol-valid `input_text` records.
- Migrate legacy placeholder strings previously written into `image_url`.
- Treat invalid `image_url` task failures as recoverable guardian events.
- Retain a five-second SQLite busy timeout during live app-server contention.

## 0.2.0

- Initial public release with Rust CLI, MCP server, Agent Skill, and per-task service management.
