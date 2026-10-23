# Cook status regression run directories (#16322)

These reduced, portable run directories model controller aggregates inspected for:

- `agent-task-0bd49fe2-2a15-47c3-ab54-10ea4b06ca4d-attempt-2-473d3ef9`: timeout classification, first provider diagnostic and recorded timeout duration.
- `agent-task-599ec50a-8071-43f2-bb98-e1742b08bd8f-attempt-3-ddd8c7a8`: harvest diagnostic without failure_classification; absolute workspace paths shortened.
- `agent-task-254ba5c0-17d3-4ad7-90ba-e2b7bbc50e3f-attempt-1-d16be950`: execution_failed plus missing Bedrock credential diagnostic.
- `agent-task-1806e43a-7e32-417a-8de7-59b59c560e7a-attempt-1-dda76f6c`: successful provider aggregate whose publication metadata says not_attempted. The record models the issue's contradictory controller verdict: failed terminal progress beside an applied candidate and controller review_ready receipt. The test also exercises draft_published and stale verification_pending promotion snapshots.

Provider artifacts, usage and unrelated metadata are omitted. The record envelopes are portable test reconstructions, not copies of the controller database. Tests import each directory into the lifecycle store so status reads the authoritative atomic record/aggregate pair, not a free-standing JSON cache.

Lab receipt atomicity and resolved-route credential admission are tracked in https://github.com/Extra-Chill/homeboy/issues/16326.
