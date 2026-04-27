# Bench Command

Run performance benchmarks for a Homeboy component and surface regression
deltas against a stored baseline.

## Synopsis

```bash
homeboy bench <component> [options] [-- <runner-args>]
homeboy bench list <component> [options] [-- <runner-args>]
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
- `--shared-state <DIR>`: Directory shared across iterations and concurrent
  runner instances. Forwarded to workloads via
  `$HOMEBOY_BENCH_SHARED_STATE`.
- `--concurrency <N>`: Number of parallel bench runner instances to spawn
  (default `1`). Values greater than `1` require `--shared-state`.
- `--setting <key=value>`: Override component settings (may be repeated).
- `--setting-json <key=json>`: Override component settings with typed JSON
  values for arrays, objects, numbers, booleans, or null.
- `--path <PATH>`: Override the component's `local_path` for this run.
- `--json-summary`: Include a compact machine-readable summary in the
  JSON output envelope (for CI wrappers).
- `--rig <RIG_ID[,RIG_ID...]>`: Pin the run to one or more rigs. Single
  rig pins the rig and stores its baseline under a rig-scoped key. If
  that rig declares `bench.components`, the command fans out across those
  components under one rig-state snapshot. Multiple rigs (comma-separated)
  run the same component + workload + iteration count against each rig in
  sequence and emit a cross-rig comparison envelope. See "Cross-rig
  comparison" below.
- `--ignore-default-baseline`: Skip automatic single-rig expansion when
  the rig declares `bench.default_baseline_rig`.

Arguments after `--` are passed verbatim to the extension's bench runner
script (e.g., `--filter=scenario_id` for selective execution).

## Scenario Discovery

`homeboy bench list <component>` asks the extension runner for its scenario
inventory without executing any workload code. The runner receives
`$HOMEBOY_BENCH_LIST_ONLY=1` and writes the normal `BenchResults` envelope
with `iterations: 0`, empty per-scenario `metrics`, and optional discovery
metadata such as `file`, `source`, `default_iterations`, and `tags`.

This is the safe first step for agent-driven or CI-driven perf work: inspect
what can be measured before deciding which full bench run is worth paying for.

## Examples

```bash
# Benchmark a component with defaults (10 iterations, 5% regression threshold)
homeboy bench my-component

# List declared scenarios without executing them
homeboy bench list my-component

# 50 iterations, stricter 2% regression threshold
homeboy bench my-component --iterations 50 --regression-threshold 2.0

# Save a new baseline
homeboy bench my-component --baseline

# Run with auto-ratchet on improvement
homeboy bench my-component --ratchet

# Select a single scenario via passthrough args
homeboy bench my-component -- --filter=hot_path

# Share warm state across invocations and run four instances in parallel
homeboy bench my-component --shared-state /tmp/homeboy-bench --concurrency 4

# Pin to a single rig — preflight + rig-scoped baseline
homeboy bench studio --rig studio-trunk

# Pin to one rig and run every component declared in bench.components
homeboy bench --rig mdi-substrates --shared-state /tmp/mdi-bench

# Cross-rig comparison: same workload, two rigs, side-by-side report.
# First rig (`studio-trunk`) is the reference; the diff table expresses
# every other rig's metrics as percent deltas vs the reference.
homeboy bench studio --rig studio-trunk,studio-combined-fixes --iterations 10

# Three-rig comparison to isolate one PR's contribution.
homeboy bench studio \
    --rig trunk,combined-fixes,combined-fixes-without-3120 \
    --iterations 20
```

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
  percent moves with skepticism.

### Rig bench defaults

Rig specs can reduce repeated CLI arguments for common main-vs-branch
bench workflows:

```jsonc
{
  "bench": {
    "default_component": "studio",
    "components": ["studio", "playground"],
    "default_baseline_rig": "studio-trunk"
  },
  "bench_workloads": {
    "wordpress": ["${package.root}/bench/studio-admin.php"]
  }
}
```

- `bench.default_component` lets `homeboy bench --rig <id>` omit the
  positional component. With multiple rigs, every rig must agree on the
  default unless the component is provided explicitly.
- `bench.components` lets `homeboy bench --rig <id>` fan out across a list
  of components from one rig spec. Scenarios are merged into the standard
  single-run envelope with `:c<component>` suffixes (for example
  `cold-boot:cstudio`). When `--shared-state <dir>` is provided, each
  component gets its own `<dir>/<component>` subdirectory.
- `bench.default_baseline_rig` upgrades `homeboy bench --rig <candidate>`
  into `homeboy bench --rig <baseline>,<candidate>` unless the invocation
  already lists multiple rigs, writes a baseline (`--baseline` / `--ratchet`),
  passes `--ignore-default-baseline`, or the candidate rig declares a
  multi-component `bench.components` matrix.
- `bench_workloads` supplies rig-owned workload files keyed by extension ID.
  Paths support `~`, `${env.NAME}`, `${components.<id>.path}`, and
  `${package.root}` expansion. `${package.root}` resolves to the installed
  rig package root, so portable rig packages can keep sibling `bench/` files
  without hardcoded machine paths.

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
- Scenario `id` values must be unique within one bench results envelope.
  Workload-discovering runners should derive ids from paths relative to
  the bench root (for example, `reads/heavy.php` → `reads-heavy`) instead
  of file basenames alone.
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
