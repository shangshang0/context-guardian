# Security

Context Guardian intentionally mutates local Codex state only after an exact thread ID is supplied. Rollout paths must contain that ID, and database filenames must match the expected Codex indexes.

Before recovery, close or pause sensitive tasks when practical. The daemon can handle stale counter writeback, but simultaneous writers always increase operational risk.

Do not enable external summarization unless you trust the configured endpoint with the tool output being summarized.

Image URL publishing is opt-in per guarded task. The public Relay isolates tenants with independent client-generated secrets and does not persist image bytes, but the Relay operator can observe transient bytes and traffic metadata. Self-host the Relay for sensitive images.

Relay identities and image signing keys must remain local mode-`0600` files. Do not copy them between users. Report suspected credential exposure promptly and delete the local identity to rotate it after the old tenant is removed or the Relay state is reset.

Report vulnerabilities privately through GitHub Security Advisories for `shangshang0/context-guardian`.
