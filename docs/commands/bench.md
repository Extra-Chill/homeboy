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
results file. Homeboy parses the results, compares declared numeric
metrics against a saved baseline, and returns a structured report plus
an exit code suitable for CI gates.

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
- `--ratchet`: When scenarios improve, auto-update the saved baseline so
  the improvement "sticks". Ignored when the run regresses.
- `--regression-threshold <PERCENT>`: Legacy p95 regression tolerance
  (default `5.0`) used when the runner does not declare `metric_policies`.
  A p95 scenario regresses when its current `p95_ms` exceeds
  `baseline.p95_ms * (1 + threshold/100)`.
- `--setting <key=value>`: Override component settings (may be repeated).
- `--path <PATH>`: Override the component's `local_path` for this run.
- `--json-summary`: Include a compact machine-readable summary in the
  JSON output envelope (for CI wrappers).
- `--shared-state <DIR>`: Mount a stable storage directory across
  iterations (and across parallel runner instances when combined with
  `--concurrency`). Exposed to the runner as
  `$HOMEBOY_BENCH_SHARED_STATE`. Created if it doesn't exist; never
  cleaned up by homeboy. See "Shared State and Concurrency" below.
- `--concurrency <N>`: Number of parallel runner instances to spawn
  (default `1`). When `> 1`, `--shared-state` is required. Each instance
  receives a distinct `$HOMEBOY_BENCH_INSTANCE_ID` (`0..N-1`) plus
  `$HOMEBOY_BENCH_CONCURRENCY=N`.
- `--rig <RIG_ID[,RIG_ID...]>`: Pin the run to one or more rigs. Single
  rig pins the rig and stores its baseline under a rig-scoped key.
  Multiple rigs (comma-separated) run the same component + workload +
  iteration count against each rig in sequence and emit a cross-rig
  comparison envelope. See "Cross-rig comparison" below.

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

# Concurrent-writer stress test: 4 parallel instances against a shared
# on-disk state directory. All four runners see the same SQLite +
# markdown files, surfacing lock contention and write loss.
homeboy bench my-component \
    --shared-state /tmp/bench-shared \
    --concurrency 4

# Crash-recovery / durability test: single instance, persistent state.
# Workload kills mid-stream on iteration N; iteration N+1 boots fresh
# against the same on-disk state and audits integrity.
homeboy bench my-component --shared-state /tmp/bench-durability

# Pin to a single rig — preflight + rig-scoped baseline
homeboy bench studio --rig studio-trunk

# Cross-rig comparison: same workload, two rigs, side-by-side report.
# First rig (`studio-trunk`) is the reference; the diff table expresses
# every other rig's metrics as percent deltas vs the reference.
homeboy bench studio --rig studio-trunk,studio-combined-fixes --iterations 10

# Three-rig comparison to isolate one PR's contribution.
homeboy bench studio \
    --rig trunk,combined-fixes,combined-fixes-without-3120 \
    --iterations 20
```

## Shared State and Concurrency

Two workload classes need state shared across runtime instances or
surviving a kill:

- **Concurrent writers** — N parallel processes writing against the
  same site, surfacing lock contention and write loss under load.
- **Crash recovery** — Start a write stream, kill mid-stream, boot a
  fresh runtime against the same on-disk state, audit integrity.

Both fit cleanly under `--shared-state <DIR>`:

| Mode | `--concurrency` | `--shared-state` | Behaviour |
|---|---|---|---|
| Cold-iteration (default) | `1` | unset | Per-iteration cold boot, no shared state. The original bench design. |
| Persistent single | `1` | `<DIR>` | Single runtime, but state in `<DIR>` survives across iterations. Crash-recovery workloads. |
| Concurrent | `> 1` | `<DIR>` (required) | N parallel runners, all pointed at `<DIR>`. Lock-contention workloads. |
| Concurrent without state | `> 1` | unset | **Rejected** — N parallel cold-boots without shared state are N independent runs. The validation error points you at `--shared-state`. |

Per-instance scenarios are merged with `:i<n>` suffixed IDs in the
aggregated output (`shared_counter:i0`, `shared_counter:i1`, …) so each
instance's measurements stay distinguishable. The baseline ratchet works
unchanged — a regression in instance 2 surfaces as a regression on
`<id>:i2`, not as silent averaging across instances.

### Runner contract additions

When shared-state and concurrency flags are set, three additional env
vars flow into the runner:

- `HOMEBOY_BENCH_SHARED_STATE` — absolute path to the shared directory
  (or empty string when not set). Workloads that opt into shared state
  read or write files under this path.
- `HOMEBOY_BENCH_INSTANCE_ID` — `0..N-1` for parallel runs, `0` for
  single-instance.
- `HOMEBOY_BENCH_CONCURRENCY` — `N` for parallel runs, `1` for
  single-instance.

Per-instance results are written to `bench-results-i<n>.json` under the
run dir; homeboy core merges them into the unified `BenchResults`
envelope before applying baseline comparison.

## Cross-rig comparison

`--rig <a>,<b>[,<c>...]` runs the same component + workload + iteration
count against each rig in sequence and emits a single comparison
envelope. Useful for "is my fix actually faster than trunk?" — same
question, two rigs differing only in component commit state.

### How it runs

For each rig, in input order:

1. Load the rig spec and run `rig check`. Failure aborts the entire
   comparison — comparing against an unhealthy rig would produce
   garbage numbers.
2. Snapshot rig state (each component's git SHA + branch) into the
   per-rig output entry.
3. Run bench against the resolved component with the rig pinned.

After every rig finishes, results are aggregated into a
`BenchComparisonOutput` envelope with `comparison: "cross_rig"`. The
**first rig in the list is the reference**: per-metric percent deltas
in the `diff` table express each subsequent rig as `(current -
reference) / reference * 100`.

### What's intentionally not done

- **No baseline writes.** `--baseline` and `--ratchet` are rejected on
  cross-rig invocations. Baselines are per-rig; writing one from a
  comparison would silently bless one rig over the others. Run `homeboy
  bench --rig <id> --baseline` once per rig to ratchet individually.
- **No statistical-significance gating.** Two rigs with overlapping
  `p95_ms` distributions still produce a numeric delta. Treat single-digit
  percent moves with skepticism. Confidence intervals are a v2 question.
- **No matrix × rig composition.** `--matrix` and multi-`--rig` together
  is not yet supported; pick one axis per invocation. Single-rig +
  matrix continues to work.

### Output shape (cross-rig)

```json
{
  "comparison": "cross_rig",
  "passed": true,
  "component": "studio",
  "exit_code": 0,
  "iterations": 10,
  "rigs": [
    {
      "rig_id": "studio-trunk",
      "passed": true,
      "status": "passed",
      "exit_code": 0,
      "results": { ... },
      "rig_state": { "rig_id": "studio-trunk", "captured_at": "...", "components": { ... } }
    },
    {
      "rig_id": "studio-combined-fixes",
      "passed": true,
      "status": "passed",
      "exit_code": 0,
      "results": { ... },
      "rig_state": { ... }
    }
  ],
  "diff": {
    "by_scenario": {
      "agent_boot": {
        "p95_ms": {
          "studio-combined-fixes": {
            "reference": 31200.0,
            "current": 19400.0,
            "delta_percent": -37.82
          }
        }
      }
    }
  },
  "hints": [ ... ]
}
```

The reference rig is omitted from the inner `diff.by_scenario.<id>.<metric>`
map — its delta against itself would always be zero. A scenario or
metric missing from a non-reference rig is silently skipped (no
synthetic zeros).

### Exit code

`exit_code` is `0` only when every rig passed. The first non-zero rig
exit code wins. `passed` is `true` only when every rig passed.

## Baseline Ratchet Semantics

The bench baseline is a list of per-scenario snapshots stored in
`homeboy.json` under the `baselines.bench` key. Each snapshot records
`{ id, metrics }` plus the iteration count at capture time.

On every run without `--baseline` or `--ignore-baseline`:

1. Each current scenario is matched against the baseline by `id`.
2. If the runner declares `metric_policies`, only those metrics are
   compared. Each policy declares whether lower or higher values are
   better and optional percent/absolute tolerances.
3. If a policy declares `variance_aware: true`, Homeboy compares the
   metric's raw sample distributions instead of only the summary value.
   The summary value still appears under `metrics.<name>` for reports;
   the per-iteration samples live under `metrics.distributions.<name>`.
4. If the runner omits `metric_policies`, Homeboy keeps the historical
   default: compare `p95_ms` as lower-is-better with the CLI threshold.
5. A scenario improves when any compared metric moves in the better
   direction.
6. Scenarios present in one run but not the other are flagged as
   `new_scenario_ids` / `removed_scenario_ids`. Neither state triggers
   a regression by itself — they're informational.
7. If any scenario regressed, the command exits `1` regardless of the
   runner's own exit code.
8. If any scenario improved and `--ratchet` is set, the baseline is
   overwritten with the current snapshot.

p95 remains the default for legacy latency benchmarks because it is less
sensitive than mean to one-off GC pauses but more sensitive than p99 to
genuine regressions. Runners that care about non-latency signals should
declare `metric_policies` instead.

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
  "metric_policies": {
    "error_rate": {
      "direction": "lower_is_better",
      "regression_threshold_absolute": 0.01
    },
    "requests_per_second": {
      "direction": "higher_is_better",
      "regression_threshold_percent": 5.0
    },
    "agent_loop_ms": {
      "direction": "lower_is_better",
      "regression_threshold_percent": 10.0,
      "variance_aware": true,
      "min_iterations_for_variance": 20,
      "regression_test": "mann_whitney_u"
    }
  },
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
        "max_ms": 172.0,
        "error_rate": 0.0,
        "requests_per_second": 180.5,
        "status_500_count": 0,
        "agent_loop_ms": 1200.0,
        "distributions": {
          "agent_loop_ms": [1100.0, 1200.0, 1300.0]
        }
      },
      "memory": { "peak_bytes": 41943040 }
    }
  ]
}
```

- Top-level keys are strict — unknown top-level fields are rejected to
  keep the contract honest.
- `metrics` is an arbitrary map of numeric values. Homeboy core does not
  attach domain meaning to metric names.
- `metric_policies` is optional. If omitted, Homeboy compares `p95_ms`
  using the legacy lower-is-better latency policy.
- Policy `direction` accepts `lower_is_better` / `lower` and
  `higher_is_better` / `higher`.
- Policy thresholds are optional. `regression_threshold_percent` compares
  relative movement; `regression_threshold_absolute` compares raw numeric
  movement. If both are present, a metric must exceed both tolerances to
  regress.
- Policy `variance_aware: true` requires a matching
  `metrics.distributions.<metric>` array on every scenario that emits the
  metric. If `min_iterations_for_variance` is set and the sample array is
  smaller, parsing fails before baseline comparison.
- Policy `regression_test` accepts `point_delta`, `mann_whitney_u`, and
  `kolmogorov_smirnov`. `point_delta` is the legacy summary-value check.
  Variance-aware metrics default to `mann_whitney_u` when the field is
  omitted. Mann-Whitney uses a one-sided 95% normal approximation;
  Kolmogorov-Smirnov uses the standard 5% two-sample critical value.
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
