# Capture Evidence

Homeboy commands should leave durable evidence that humans, CI, scheduled jobs, and agents can inspect later. Prefer structured output and persisted run artifacts over copied terminal logs.

## Use This When

- A reviewer needs proof beyond “it passed on my machine.”
- A coding agent needs machine-readable results.
- A benchmark, trace, fuzz run, or matrix job creates artifacts that need to survive the shell session.
- You need to compare candidate behavior with a baseline.

## 1. Start With Command JSON

Most Homeboy commands support the global `--output <path>` flag. Use it for the first layer of durable evidence:

```bash
homeboy --output homeboy-results/review.json \
  review my-component --changed-since origin/main

homeboy --output homeboy-results/bench.json \
  bench my-component --baseline

homeboy --output homeboy-results/triage.json \
  triage workspace --mine --failing-checks
```

Use a real path. Bare format names such as `json` are rejected.

## 2. Use Persisted Runs For Artifact-Heavy Work

When a workflow produces observation records, files, screenshots, bundles, or comparisons, inspect it through `homeboy runs`:

```bash
homeboy runs list
homeboy runs show <run-id>
homeboy runs artifacts <run-id>
homeboy runs evidence <run-id>
```

Use `show` for the run summary, `artifacts` for generated files, and `evidence` for reviewer-facing references.

## 3. Capture Benchmark Evidence

Use benchmarks when performance is part of the acceptance criteria:

```bash
homeboy bench my-component --baseline
homeboy bench my-component --ratchet
homeboy bench my-component --output homeboy-results/bench.json
```

For comparisons across rigs or histories, keep the run id and inspect it through `homeboy runs` instead of copying stdout.

## 4. Capture Trace Evidence

Use traces for black-box behavior and scenario timing:

```bash
homeboy trace my-component list
homeboy trace my-component checkout-flow --baseline
homeboy trace my-component checkout-flow --runs 5 --aggregate spans
```

Trace output is most useful when paired with persisted artifacts or a Markdown report that can be attached to a PR or issue.

## 5. Capture Fuzz Evidence

Use fuzzing when you need workload coverage, replayable failures, or campaign artifacts:

```bash
homeboy fuzz list my-component
homeboy fuzz run my-component --workload <workload-id>
homeboy runs show <run-id>
```

Keep fuzz evidence tied to the workload id, run id, and any failure artifact bundle.

## 6. Publish Reviewer-Safe Evidence

Reviewer-facing evidence should point to a reachable artifact, PR comment, issue, release asset, or exported run bundle. Local paths and localhost URLs are operator notes, not durable review evidence.

For runner, static HTML, and matrix workflows, publish stdout and generated files into persisted artifacts before sharing evidence. See [Artifact loop for runner and matrix workflows](../operations/artifact-loop-runner-matrix.md).

## Reference

- [bench command](../commands/bench.md)
- [trace command](../commands/trace.md)
- [fuzz command](../commands/fuzz.md)
- [runs command](../commands/runs.md)
- [JSON output contract](../architecture/output-system.md)
- [Structured evidence concept](../concepts/structured-evidence.md)
