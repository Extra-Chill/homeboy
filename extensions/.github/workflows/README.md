# Homeboy Extensions reusable workflows

`actions-run-once.yml` is a reusable idempotency gate for workflows that consume an evidence packet exactly once. Call it in `check` mode before the expensive job and in `mark` mode after consuming the evidence.

Use a stable `idempotency_key` derived from the source repository, revision, and workload. Pair the caller with a concurrency group derived from the same key so parallel events cannot both pass the check before the marker is saved. The workflow returns `should_run` to gate downstream jobs.

Only mark successful consumption if failures should be retryable. Mark after any attempted consumption if each immutable evidence packet should get exactly one attempt.
