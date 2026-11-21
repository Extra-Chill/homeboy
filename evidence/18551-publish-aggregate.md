# publish.aggregate CPU

## Problem and trace

The import-loop sample in #18551 attributed 170 Homeboy CPU-s over eight
`publish.aggregate` spans. `persist_aggregate` times `record_run_aggregate`.
That path reads the record, runs retention and scratch registration/finalization,
fetches remote candidates if necessary, applies the aggregate transition, commits
its record/events/intents, rereads the record, projects terminal artifacts, commits
the projection annotation, and updates the Cook candidate index.

The guarded record writer builds progress events, serializes the aggregate into
observation metadata, merges existing metadata, compares the record snapshot for
revision changes, and writes the row/events/intents with a revision fence. That
write already has an atomic store API; replacing it with independent or broadly
batched writes would change concurrency and failure semantics. Artifact projection
checks declared size/digest and verifies local or downloaded bytes before publishing
artifact records. Those integrity checks are not redundant proof and remain intact.
Projection and annotation do rewrite metadata, but removing those commits changes
recovery boundaries; this change instead removes work inside their shared codec.

## Root cause and change

Metadata compaction serialized the entire remaining metadata after *each* payload
was removed, and serialized ancestor/child evidence repeatedly while descending
the largest-child path. For aggregates with many large evidence entries this is
quadratic repeated traversal/allocation. Resolution additionally cloned each whole
subtree before recursively replacing it with another clone.

Count compact JSON bytes without allocating a serialized copy; carry the selected
child's measured size into descent; update total length by the exact subtree size
delta after externalization. Serialize the final row only once. Resolution now
constructs each output container once. Payload selection (including last-child tie
breaking), threshold, compact JSON bytes, SHA-256, fsyncs, integrity verification,
transactions, and lifecycle write boundaries are unchanged. No new cache or layer.
`gate.acceptance` polling is outside this change.

## Measurement

Manual ignored timing fixture: `publish_aggregate_large_evidence_timing` in
`agent_task_lifecycle/tests/task_state.rs`. It builds 24 attempts, each with 1,200
changed-file paths and 16 lines of diff evidence per file, and publishes three
fresh terminal runs. It times precisely `record_run_aggregate_in_store`, the body
of `publish.aggregate`, excluding submission and compilation. Debug build, same
workspace and fixture on both versions, Linux; not a production latency forecast.

| | Before | After |
|---|---|---|
| First wall-ms run | 62971, 63154, 62523 | 18253, 18081, 18054 |
| Median wall-ms | 62971 | 18081 |
| Direct test-process user CPU-s, three publications | 228.08 | 54.33 |
| Direct test-process system CPU-s | 0.28 | 0.14 |
| Direct test-process wall-s | 228.57 | 54.54 |

The separate direct baseline CPU run had wall-ms 62031, 103879, 62511 (one slow
iteration); the first baseline wall sample above is included rather than hiding
that variation. Direct process CPU excludes compilation and includes fixture
construction/submission, whose costs are unchanged. Median publish wall fell 71%;
total measured direct-process CPU fell 76%.

All six persisted aggregate projections in the first baseline and optimized run
had identical SHA-256:
`d09824fb6652fe5dfe83d3c9af5af2a023cf1d671e0c7967514f900abf8981ec`.
This covers payload selection/digest/length in the stored projection. The fixture
is a manual benchmark, not a flaky wall-clock regression assertion.

## Verification

The timing fixture succeeds on both versions. Existing metadata-payload tests
cover round-trip resolution, integrity rejection, compaction, retention, and locks;
these pass after the change. Full deterministic gates are left to Homeboy.

Refs #18551
