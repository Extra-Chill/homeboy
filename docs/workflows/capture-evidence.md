# Capture Evidence

Homeboy commands should leave durable evidence that humans, CI, scheduled jobs, and agents can inspect later. Prefer structured output and persisted run artifacts over copied terminal logs.

## Command JSON

Write command-specific JSON with `--output`:

```bash
homeboy review --changed-since origin/main --output homeboy-results/review.json
homeboy bench my-project --output homeboy-results/bench.json
homeboy triage workspace --output homeboy-results/triage.json
```

## Persisted Runs

Use `homeboy runs` when a workflow produces persisted observations or artifacts:

```bash
homeboy runs list
homeboy runs show <run-id>
homeboy runs artifacts <run-id>
homeboy runs evidence <run-id>
```

## Benchmarks, Traces, And Fuzzing

```bash
homeboy bench my-project --baseline
homeboy trace my-project checkout-flow --baseline
homeboy fuzz run my-project --workload <workload-id>
```

For runner, static HTML, and matrix workflows, publish stdout and generated files into persisted artifacts before sharing evidence. See [Artifact loop for runner and matrix workflows](../operations/artifact-loop-runner-matrix.md).

## Reference

- [bench command](../commands/bench.md)
- [trace command](../commands/trace.md)
- [fuzz command](../commands/fuzz.md)
- [runs command](../commands/runs.md)
- [JSON output contract](../architecture/output-system.md)
