---
name: context-guardian
description: Inspect, recover, and continuously protect oversized or corrupted Codex task contexts. Use when a Codex task reports context-window errors, token counters grow implausibly large, image/tool outputs inflate rollout JSONL, stale counters are repeatedly written back, or the user asks to guard/watch/protect a task or thread ID.
---

# Context Guardian

Protect one explicit Codex task at a time through the bundled Rust CLI or MCP server. Treat recovery as a state repair operation, not semantic summarization.

## Workflow

1. Identify the exact task/thread ID. Never infer a write target from a title alone.
2. Run `inspect_context` through MCP or `context-guardian --thread-id ID --status`.
3. Explain what triggered recovery: high token counter, context error, inline image, attachment reference, or oversized tool output.
4. Before mutation, confirm the rollout path contains the exact thread ID and that `state_5.sqlite` belongs to the active `CODEX_HOME`.
5. Run one recovery pass. Use MCP `recover_context` with `confirm=true`, or the CLI with `--once`.
6. Verify the resulting rollout parses as JSONL, current task tail remains, counters are below the trigger, and a second recovery pass reports zero changes.
7. Install continuous guarding only when requested or when a live app-server repeatedly restores stale counters. Use `guardian_service` with explicit confirmation.

## Safety Rules

- Do not operate on multiple thread IDs in one command.
- Do not lower thresholds merely to make counters look smaller.
- Do not claim that the recovery token value equals effective semantic context.
- Preserve existing `compacted` summaries and the active tail. If no reliable compacted summary exists, warn that old details may not be recoverable.
- Expect high-value rewrites to create backups under `$CODEX_HOME/context-guardian/backups`.
- External CC Switch summarization is opt-in. Do not enable it unless the endpoint and model are trusted by the user.
- Stop if rollout discovery fails, the path does not contain the thread ID, JSONL is incomplete, or database filenames are unexpected.

## Commands

```sh
context-guardian --thread-id THREAD_ID --status
context-guardian --thread-id THREAD_ID --once
context-guardian --thread-id THREAD_ID
```

The daemon mode watches continuously. Prefer MCP tools when available because they validate parameters and require explicit mutation confirmation.

Read [references/recovery-model.md](references/recovery-model.md) before diagnosing data loss, repeated counter writeback, or image-related failures.
