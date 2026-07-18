# Security

Context Guardian intentionally mutates local Codex state only after an exact thread ID is supplied. Rollout paths must contain that ID, and database filenames must match the expected Codex indexes.

Before recovery, close or pause sensitive tasks when practical. The daemon can handle stale counter writeback, but simultaneous writers always increase operational risk.

Do not enable external summarization unless you trust the configured endpoint with the tool output being summarized.

Report vulnerabilities privately through GitHub Security Advisories for `shangshang0/shangTools`.
