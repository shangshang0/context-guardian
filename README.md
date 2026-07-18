# Context Guardian

Agent-oriented sidecar for inspecting, recovering, and continuously protecting Codex task contexts. It lives entirely in this subdirectory of `shangTools` and operates on one explicit task ID at a time.

## Why it exists

Long-running tasks can become unusable because of stale token counters, context-window failure loops, inline image bodies, historical image attachments, or oversized tool outputs. Context Guardian repairs the persisted rollout and local indexes without proxying model requests or changing global Codex settings.

The default recovery value is `100000` tokens. This is an index recovery value, not a guarantee that all retained context is semantically useful.

## Components

- `context-guardian`: Rust CLI and watchdog.
- `mcp/server.mjs`: dependency-free stdio MCP server for agents.
- `skill/context-guardian`: installable Codex Agent Skill.
- `scripts/service.sh`: per-task launchd/systemd user service manager.
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

Set `CONTEXT_GUARDIAN_BIN` if the binary is not under `target/release/` relative to the MCP service.

## Recovery behavior

- Strict single-thread scope and rollout-path validation.
- Structural placeholders for inline and historical images.
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
node --check mcp/server.mjs
sh -n scripts/install.sh
sh -n scripts/service.sh
python3 /path/to/skill-creator/scripts/quick_validate.py skill/context-guardian
```

## License

MIT
