# Recovery Model

## What the guardian changes

- Removes high-token telemetry and context-window failure records that can retrigger recovery loops.
- Replaces inline image bodies and historical attachment references with small structural placeholders.
- Prunes or optionally summarizes oversized tool outputs.
- Folds obsolete pre-compaction history only when a later `compacted` record already exists.
- Lowers stale `threads.tokens_used` and `thread_goals.tokens_used` only for the configured task.

## What it does not guarantee

The recovery counter, normally `100000`, is a safe index value. It is not a measurement that every retained token is useful, nor proof that all prior facts survived. Semantic continuity depends primarily on the quality of the existing compacted summary and the retained active tail.

## Repeated writeback

If the counter briefly rises again, an active app-server may still hold an old in-memory total. Continuous guarding can converge the database back below threshold. Avoid restarting the entire app unless scoped unloading is unavailable and the user accepts disruption to other active tasks.

## Image failures

Image requests may fail because a rollout stores a large `data:image` value or a historical local image reference that gets re-expanded during resume. Token telemetry can look reasonable while the HTTP request body is still too large. Scrubbed image items must be converted to `input_text`; never leave placeholder text in an `input_image.image_url` field because the API validates it as a URL.
