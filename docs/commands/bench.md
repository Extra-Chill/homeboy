# Bench Command

Run performance benchmarks for a Homeboy component and surface regression
deltas against a stored baseline.

## Synopsis

```bash
homeboy bench <component> [options] [-- <runner-args>]
```

## Description

The `bench` command invokes the extension's bench runner, which measures
one or more scenarios over N iterations and emits a structured JSON
results file. Homeboy parses the results, compares p95 latency against
a saved baseline, and returns a structured report plus an exit code
suitable for CI gates.

`bench` is a sibling of `test`, `lint`, and `build` under homeboy's
extension capability model. The runner contract, manifest shape, and
baseline primitive (`homeboy.json` → `baselines.bench`) are shared with
the other capabilities.

## Arguments

- `<component>`: Component to benchmark. Auto-detected from the current
  working directory if omitted. The component must have a linked
  extension that declares a `bench` capability.

## Options

- `--iterations <N>`: Iterations per scenario (default `10`). Forwarded
  to the runner via `$HOMEBOY_BENCH_ITERATIONS`. Extensions may clamp.
- `--baseline`: Save the current run as the new baseline under
  `homeboy.json` → `baselines.bench`.
- `--ignore-baseline`: Run without comparing to any saved baseline.
- `--ratchet`: When scenarios improve (p95 got faster), auto-update the
  saved baseline so the improvement "sticks". Ignored when the run
  regresses.
- `--regression-threshold <PERCENT>`: p95 regression tolerance (default
  `5.0`). A scenario regresses when its current p95_ms exceeds
  `baseline.p95_ms * (1 + threshold/100)`.
- `--setting <key=value>`: Override component settings (may be repeated).
- `--path <PATH>`: Override the component's `local_path` for this run.
- `--json-summary`: Include a compact machine-readable summary in the
  JSON output envelope (for CI wrappers).

Arguments after `--` are passed verbatim to the extension's bench runner
script (e.g., `--filter=scenario_id` for selective execution).

## Examples

```bash
# Benchmark a component with defaults (10 iterations, 5% regression threshold)
homeboy bench my-component

# 50 iterations, stricter 2% regression threshold
homeboy bench my-component --iterations 50 --regression-threshold 2.0

# Save a new baseline
homeboy bench my-component --baseline

# Run with auto-ratchet on improvement
homeboy bench my-component --ratchet

# Select a single scenario via passthrough args
homeboy bench my-component -- --filter=hot_path
```

## Baseline Ratchet Semantics

The bench baseline is a list of per-scenario snapshots stored in
`homeboy.json` under the `baselines.bench` key. Each snapshot records
`{ id, p95_ms, p50_ms, mean_ms }` plus the iteration count at capture
time.

On every run without `--baseline` or `--ignore-baseline`:

1. Each current scenario is matched against the baseline by `id`.
2. A scenario regresses when current `p95_ms > baseline.p95_ms * (1 + threshold/100)`.
   Default threshold: 5%.
3. A scenario improves when current `p95_ms < baseline.p95_ms`.
4. Scenarios present in one run but not the other are flagged as
   `new_scenario_ids` / `removed_scenario_ids`. Neither state triggers
   a regression by itself — they're informational.
5. If any scenario regressed, the command exits `1` regardless of the
   runner's own exit code.
6. If any scenario improved and `--ratchet` is set, the baseline is
   overwritten with the current snapshot.

p95 was chosen as the regression signal over mean (too sensitive to
one-off GC pauses) and p99 (too insensitive to real regressions).

## Runner Contract

The extension's bench script must:

1. Read `$HOMEBOY_BENCH_ITERATIONS` to determine iteration count.
2. Write its JSON output to `$HOMEBOY_BENCH_RESULTS_FILE`.
3. Exit with a non-zero status only on runner-level failure (script
   error, workload crash) — regressions are homeboy's domain.

### JSON output schema

```json
{
  "component_id": "string",
  "iterations": 10,
  "scenarios": [
    {
      "id": "scenario_slug",
      "file": "tests/bench/some-workload.ext",
      "iterations": 10,
      "metrics": {
        "mean_ms": 120.3,
        "p50_ms": 118.0,
        "p95_ms": 145.0,
        "p99_ms": 160.0,
        "min_ms": 110.0,
        "max_ms": 172.0
      },
      "memory": { "peak_bytes": 41943040 }
    }
  ]
}
```

- Top-level keys are strict — unknown top-level fields are rejected to
  keep the contract honest.
- Scenario-level unknown keys are **tolerated**, so extensions can emit
  additional metadata (tags, environment info, warmup counts) without
  breaking parsing.
- `memory` is optional. Extensions that can't measure peak memory omit it.
- `file` is optional but recommended for diagnostics.

### Environment variables injected

Bench scripts receive the standard runner contract plus bench-specific
variables:

- `HOMEBOY_BENCH_RESULTS_FILE` — where to write JSON output.
- `HOMEBOY_BENCH_ITERATIONS` — iteration count to use.
- `HOMEBOY_RUN_DIR` — per-run directory (shared with test/lint/build).
- `HOMEBOY_EXTENSION_ID`, `HOMEBOY_COMPONENT_ID`, `HOMEBOY_COMPONENT_PATH`,
  and the usual execution-context vars.
- `HOMEBOY_SETTINGS_JSON` — component settings as JSON.

## Component Requirements

For a component to be benchmarkable, it must have:

- A linked extension whose manifest declares a `bench` capability.
- A bench-runner script provided by the extension.

Extension manifest:

```json
{
  "bench": {
    "extension_script": "scripts/bench/bench-runner.sh"
  }
}
```

## Exit Codes

- `0` — All scenarios passed, no regressions detected (or no baseline
  exists yet).
- `1` — At least one scenario regressed beyond the threshold, or the
  runner itself failed.
- Other non-zero — Runner exit code passthrough (extension-specific).

## Related

- [test](./test.md) — Test sibling capability; bench mirrors its flag
  conventions for `--baseline`, `--ignore-baseline`, and `--ratchet`.
- [lint](./lint.md) — Lint sibling capability.
- [build](./build.md) — Build sibling capability.
