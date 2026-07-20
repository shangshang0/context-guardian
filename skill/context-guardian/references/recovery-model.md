# Recovery Model

## What the guardian changes

- Removes high-token telemetry and context-window failure records that can retrigger recovery loops.
- Replaces inline image bodies and historical attachment references with small structural placeholders.
- Prunes or optionally summarizes oversized tool outputs.
- Folds obsolete pre-compaction history only when a later `compacted` record already exists.
- Lowers stale `threads.tokens_used` and `thread_goals.tokens_used` only for the configured task.
- In preview mode, validates message envelopes after unknown failures and repairs only lossless structural transformations.

## What it does not guarantee

The recovery counter, normally `100000`, is a safe index value. It is not a measurement that every retained token is useful, nor proof that all prior facts survived. Semantic continuity depends primarily on the quality of the existing compacted summary and the retained active tail.

## Message format failures

Compaction or third-party processing may stringify `replacement_history`, flatten typed content arrays, swap `input_text` and `output_text`, or leave function arguments as objects instead of JSON strings. Preview diagnosis compares field paths and value types without recording content. If every difference has a lossless normalization, Guardian backs up the rollout, repairs the records, and removes the triggering unknown failure event. Any ambiguous role, missing text, or unknown content type fails closed.

The optional live probe establishes that the installed Codex CLI can complete a minimal request with the current user's provider/auth/proxy settings. It is an availability oracle, not a packet capture, and cannot prove that missing semantic content is recoverable.

## Repeated writeback

If the counter briefly rises again, an active app-server may still hold an old in-memory total. Continuous guarding can converge the database back below threshold. Avoid restarting the entire app unless scoped unloading is unavailable and the user accepts disruption to other active tasks.

## Image failures

Image requests may fail because a rollout stores a large `data:image` value or a historical local image reference that gets re-expanded during resume. Token telemetry can look reasonable while the HTTP request body is still too large. Scrubbed image items must be converted to `input_text`; never leave placeholder text in an `input_image.image_url` field because the API validates it as a URL.
