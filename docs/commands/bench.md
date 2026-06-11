# Bench Command

Run performance benchmarks for a Homeboy component and surface regression
deltas against a stored baseline.

## Synopsis

```bash
homeboy bench <component> [options] [-- <runner-args>]
homeboy bench matrix [<component>] --setting-matrix <key=value[,value...]> [options] [-- <runner-args>]
homeboy bench list <component> [options] [-- <runner-args>]
homeboy bench history <component> [--scenario <id>] [--rig <id>] [--limit 20]
homeboy bench distribution <component> --field <metadata.path> [--scenario <id>] [--rig <id>] [--status <status>] [--limit 20]
homeboy bench compare --from-run <run-id> --to-run <run-id> [--metric <name>]
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
  (default `1`). Values greater than `1` require `--shared-state`. With
  `--matrix`, this controls generic scheduler task concurrency instead.
- `--setting <key=value>`: Override component settings (may be repeated).
- `--setting-json <key=json>`: Override component settings with typed JSON
  values for arrays, objects, numbers, booleans, or null.
- `--path <PATH>`: Override the component's `local_path` for this run.
- `--json-summary`: Include a compact machine-readable summary in the
  JSON output envelope (for CI wrappers).
- `--report side-by-side`: Select the combined side-by-side comparison
  report for a multi-rig bench envelope. The report is emitted under
  `reports.side_by_side` and includes each rig's status, elapsed time,
  key metrics, artifact paths/URLs, and failure reason.
- `--rig <RIG_ID[,RIG_ID...]>`: Pin the run to one or more rigs. Single
  rig pins the rig and stores its baseline under a rig-scoped key. If
  that rig declares `bench.components`, the command fans out across those
  components under one rig-state snapshot. Multiple rigs (comma-separated)
  run the same component + workload + iteration count against each rig in
  sequence and emit a cross-rig comparison envelope. See "Cross-rig
  comparison" below.
- `--rig-concurrency <N>`: For multi-rig comparisons, run up to `N` rigs
  concurrently. Default `1` preserves sequential CI behavior. Values greater
  than `1` are opt-in and preserve output ordering by the selected rig order.
- `--matrix <axis=value[,value...]>`: Add an agent-task matrix axis. Repeat for
  multiple axes. When present, `homeboy bench` builds a generic
  `homeboy/agent-task-matrix-plan/v1`, dispatches cells through the generic
  scheduler, and returns both scheduler and matrix aggregate metadata.
- `--setting-matrix <key=value[,value...]>`: For `homeboy bench matrix`, add a
  local settings axis. The command expands the Cartesian product, runs one
  normal child bench per cell, and aggregates child statuses, run IDs, and
  numeric metric samples.
- `--runner-pool <BACKEND>`: Executor backend string for matrix cells. Core keeps
  this as data so concrete backends such as Codebox remain extension-owned.
  Omit to use the local no-op executor for plan/scheduler smoke checks.
- `--max-tasks <N>` / `--max-queue-depth <N>`: Scheduler backpressure limits for
  matrix cells.
- `--expect-artifact <NAME>`: Record an expected artifact name on each matrix
  task request. Repeat to declare multiple expected artifacts.
- `--scenario <SCENARIO_ID>`: Run or list only the exact scenario id. May
  be repeated. Homeboy validates selected ids against discovery before
  execution and forwards the comma-separated selector to runners via
  `$HOMEBOY_BENCH_SCENARIOS`.
- `--ci-profile <ID>`: Run using env and passthrough args from a single
  extension-declared CI profile whose only job declares `command: "bench"`.
  This keeps bench parity on the normal Homeboy bench workflow instead of
  parsing arbitrary provider YAML into runnable commands.
- `--ignore-default-baseline`: Skip automatic single-rig expansion when
  the rig declares `bench.default_baseline_rig`.

Arguments after `--` are passed verbatim to the extension's bench runner
script.

## Matrix Fan-Out

For local benchmark scale sweeps, `homeboy bench matrix` is the narrow reusable
runner over existing bench execution. It supports zero or one rig, rejects
baseline writes, and varies string settings via `--setting-matrix`:

```bash
homeboy bench matrix \
  --rig gutenberg-rtc \
  --scenario gutenberg-rtc-protocol-load \
  --setting-matrix clients=10,100,500,1000 rounds=3 batch_size=1,10,25,100 \
  --runs 3
```

The JSON output uses the `settings_matrix` bench variant and includes:

- `cells`: one entry per expanded settings combination with status, exit code,
  extracted child run ID, hints, and numeric metric samples.
- `summary`: cell totals and collected child run IDs.
- `follow_ups`: intentionally deferred pieces such as matrix-level observation
  records, typed JSON axes, and cross-rig matrix aggregation.

## Agent-Task Matrix Fan-Out

Matrix fan-out is the operator-facing path over Homeboy's generic agent-task
substrate. It is intentionally executor-neutral: the CLI turns product inputs
into `AgentTaskRequest` cells, the scheduler owns concurrency/backpressure, and
backend-specific execution remains outside Homeboy core.

```bash
homeboy bench studio-web \
  --rig studio-web-evals \
  --matrix model=gpt-5.5,kimi,claude \
  --matrix prompt=site-a,site-b,site-c \
  --runner-pool codebox \
  --concurrency 8 \
  --max-queue-depth 16 \
  --expect-artifact bench-results \
  --report side-by-side
```

The JSON output uses the `matrix_fanout` bench variant and includes:

- `scheduler`: `homeboy/agent-task-aggregate/v1` scheduler totals, events,
  queue state, outcomes, and backpressure.
- `matrix`: `homeboy/agent-task-matrix-aggregate/v1` cells with axes, status,
  artifacts, evidence refs, diagnostics, and metadata.
- `report`: compact product metadata for the selected report format plus
  succeeded, blocked, failed, cancelled-cell, and timed-out-cell totals.

When `--ci-profile <ID>` is used, args declared on that profile's bench job
are forwarded before explicit CLI passthrough arguments, and job env is passed
to the bench runner. Profiles with zero jobs, multiple jobs, or a non-`bench`
job are rejected for command-native bench reproduction; use `homeboy ci run
--profile <ID>` when you need to execute a multi-job CI profile directly.

`homeboy bench` is resource-policy aware. If the current machine is already warm
or hot according to `homeboy doctor resources`, Homeboy prints a stderr warning
before running because the extra load can skew benchmark results. Use global
`--force-hot` when running under load is intentional. When a default Lab runner is
available for portable bench runs, `--force-hot` is not a local bypass by itself;
add `--allow-local-hot` only when controller-machine execution is intentional:

```bash
homeboy --force-hot --allow-local-hot bench my-component
```

## Memory Timeline Evidence

Each bench runner invocation samples the runner process tree while the workload
is active. Successful runs add memory metrics to `results.metric_groups.memory`
and each scenario's metrics:

- `peak_rss_mb`: peak sampled RSS for the runner process tree.
- `peak_child_count`: number of descendant processes at the peak RSS sample.
- `memory_sample_count`: number of retained timeline samples.

Homeboy also writes `bench-memory-timeline.json` and
`bench-memory-timeline.csv` in the run directory. Concurrent bench instances use
`bench-memory-timeline-i<N>.json` and `.csv`. Observation runs record these as
`bench_memory_timeline` artifacts, including failure runs that exit after samples
were captured but before benchmark JSON is complete.

## Observation History

Every `homeboy bench` run is persisted to the local observation store. The
normal bench output includes hints with the persisted run ID, the observation DB
path, and follow-up commands such as:

```bash
homeboy runs show <run-id>
homeboy runs list --kind bench --component <component> --rig <rig>
```

Omit `--rig <rig>` from the list command for unpinned bench runs.

`homeboy bench history <component>` lists persisted benchmark runs from the local observation store. `--scenario` filters to runs whose stored metadata includes the scenario, and `--rig` narrows to rig-pinned runs.

`homeboy bench distribution <component> --field <metadata.path>` summarizes repeated categorical values from persisted benchmark run metadata. It is generic over metadata shape: scalar string, number, and boolean values are counted directly, and arrays are flattened. Use `--scenario`, `--rig`, `--status`, and `--limit` to narrow the persisted run window before aggregation.

`homeboy bench compare --from-run <baseline-run-id> --to-run <candidate-run-id>` compares numeric metrics recorded in two persisted benchmark runs. It is the first useful baseline-vs-candidate comparison slice: it captures both run IDs, component state, shared bench context, selected metric deltas, and a Markdown table under `reports.markdown` in the JSON payload.

Bench runners may attach generic tracker provenance to the top-level results
envelope or to individual scenarios/workloads. Homeboy persists that metadata in
the run record, keeps it in JSON artifacts/exports, and renders a `Provenance`
section with clickable links in compare Markdown when present:

```json
{
  "provenance": {
    "labels": ["source: zendesk", "privacy: internal"],
    "links": [
      {
        "url": "https://wordpress.org/support/topic/checkout-is-very-slow/",
        "label": "WordPress.org support thread",
        "source": "wordpress.org"
      }
    ]
  }
}
```

Use `labels` for non-public or freeform context that should stay as text, such
as `source: zendesk`, `scenario: shortcode checkout place-order latency`, or
`privacy: internal`. Use `links` for reviewer-clickable references.

The command is read-only and exits successfully for a valid comparison even when the numbers regress. Repeat `--metric <name>` to keep the comparison focused; omit it to compare all shared numeric scenario metrics.

Homeboy rejects comparisons when the stored shared benchmark context differs for settings, selected scenarios, workload fingerprints, iteration count, run count, or concurrency. This keeps manually-created baseline/candidate runs from being compared when they were not actually measuring the same scenario contract.

This slice compares two existing persisted run IDs. A future execution mode should wrap two `homeboy bench` invocations in one command, using the same rig/scenario/settings and separate component paths or refs, then feed the resulting persisted run IDs through this comparison layer.

## Scenario Discovery

`homeboy bench list <component>` asks the extension runner for its scenario
inventory without executing any workload code. The runner receives
`$HOMEBOY_BENCH_LIST_ONLY=1` and writes the normal `BenchResults` envelope
with `iterations: 0`, empty per-scenario `metrics`, and optional discovery
metadata such as `file`, `source`, `default_iterations`, and `tags`.

This is the safe first step for agent-driven or CI-driven perf work: inspect
what can be measured before deciding which full bench run is worth paying for.

## Semantic Gates

Bench runners can attach scenario-level correctness gates to non-timing
metrics. Gates are evaluated after metrics are parsed and aggregated. Any
failed gate marks the scenario and run failed, even when timing metrics
improve, and the failure details are emitted before baseline comparison data
in the JSON output.

Supported operators are `eq`, `gte`, and `lte`:

```json
{
  "id": "studio-agent-loop",
  "iterations": 10,
  "metrics": {
    "p95_ms": 1200.0,
    "assistant_message_count": 1,
    "identifies_studio_rate": 1.0
  },
  "gates": [
    { "metric": "assistant_message_count", "op": "gte", "value": 1 },
    { "metric": "identifies_studio_rate", "op": "eq", "value": 1.0 }
  ]
}
```

Evaluated gates add scenario-level `gate_results`. Failed gates set the
scenario's `passed` field to `false` and add top-level `gate_failures` plus
`budget_findings` to the bench output.

Bench output also exposes a top-level `gate_results` array using the shared
`homeboy/gate-result/v1` schema consumed by cook loops and PR finalization.
IDs include the scenario id and metric name, for example
`bench.gate.studio-agent-loop.success_rate`, so multiple scenarios can gate on
the same metric without collisions. Failed normalized gates are marked
`retryable: true` and include agent feedback plus metric evidence for the next
candidate iteration.

## Budget Findings

Benchmark and profile workloads can emit top-level `budget_findings` for fixed
threshold failures that should report and gate consistently across extensions.
The common shape is:

```json
{
  "category": "budget",
  "code": "rest.max_response_bytes",
  "severity": "error",
  "file": null,
  "context_label": "profile:wordpress-rest",
  "message": "REST response exceeded 250 KB budget",
  "actual": 4378195,
  "expected": 250000,
  "unit": "bytes",
  "subject": "/wp-json/datamachine/v1/pipelines?per_page=100",
  "passed": false
}
```

`severity: "error"` or `passed: false` fails the bench run. Lower severities
are report-only. `code` is the stable grouping key, `subject` identifies the
endpoint/resource/phase being measured, and `actual` / `expected` / `unit` are
rendered by `homeboy report failure-digest`.

## Metric Policy Presets

Bench runners can declare generic `metric_policy_presets` when they want common
performance policy semantics without hand-writing full `metric_policies` or
gates. Homeboy expands presets during result parsing into the existing policy,
gate, and `budget_findings` paths.

```json
{
  "metric_policy_presets": {
    "agent_loop_ms": {
      "preset": "latency_regression",
      "regression_threshold_percent": 7.5,
      "phase": "warm"
    },
    "peak_rss_bytes": {
      "preset": "memory_regression"
    },
    "rest_response_bytes": {
      "preset": "absolute_budget",
      "max": 250000
    }
  }
}
```

Supported presets are `latency_regression`, `memory_regression`,
`cold_warm_delta`, `flake_noise_threshold`, and `absolute_budget`. Regression
presets expand to `metric_policies`; absolute budgets expand to normal gates so
failures render through the existing budget finding and failure digest surfaces.

Rigs can also declare gates for scenario metrics when the workload output is
owned elsewhere. The keys under `metric_gates` are exact scenario ids:

```json
{
  "bench": {
    "default_component": "studio",
    "metric_gates": {
      "wordpress-is-dead": {
        "native_block_quality_pass": { "equals": 1 },
        "tool_error_count": { "equals": 0 },
        "success_rate": { "equals": 1 }
      }
    }
  }
}
```

Rig-declared metric gates are merged with workload-emitted `gates` before
Homeboy evaluates scenario status. Supported rig operators are `equals`, `gte`,
and `lte`.

## Invocation Isolation

Every bench child process receives generic invocation-scoped environment
variables, independent of runner implementation:

- `HOMEBOY_INVOCATION_CONTEXT_JSON`: structured JSON contract for the same
  invocation context. Prefer this for new extension helpers so they consume one
  audited Homeboy-owned shape instead of reconstructing context from individual
  env names.
- `HOMEBOY_INVOCATION_ID`: stable identifier for this child workload invocation.
- `HOMEBOY_INVOCATION_STATE_DIR`: private state directory for files that should
  outlive one internal iteration but remain scoped to this invocation.
- `HOMEBOY_INVOCATION_ARTIFACT_DIR`: private artifact directory for logs,
  screenshots, traces, and downloaded/build artifacts.
- `HOMEBOY_INVOCATION_TMP_DIR`: private temporary directory for project copies,
  browser profiles, wasm caches, and other scratch state.

The structured context currently has this substrate-agnostic shape:

```json
{
  "id": "inv-0123456789",
  "state_dir": "/tmp/hb/0123456789",
  "artifact_dir": "/tmp/hb/0123456789.a",
  "tmp_dir": "/tmp/hb/0123456789.t",
  "port_range": { "base": 20000, "max": 20007 },
  "named_leases": ["playground-browser-profile"]
}
```

`port_range` is omitted when the workload did not request ports.
`named_leases` is omitted when no named leases were requested. The legacy
`HOMEBOY_INVOCATION_*` env vars remain part of the contract for existing
extensions and scripts.

### Runtime path contract

Invocation directories are placed under a short, platform-aware root so any
workload can put a UNIX domain socket or other path-length-sensitive primitive
under `HOMEBOY_INVOCATION_STATE_DIR` without bespoke defense.

**Layout.** The invocation owns three sibling directories under the runtime
root:

- `<root>/<short-id>`     → `HOMEBOY_INVOCATION_STATE_DIR` (the leaf the
  workload owns; downstream sockets bind directly here).
- `<root>/<short-id>.a`   → `HOMEBOY_INVOCATION_ARTIFACT_DIR`
- `<root>/<short-id>.t`   → `HOMEBOY_INVOCATION_TMP_DIR`

There is no `s/a/t` subdir layer underneath the short id. The invocation is
1:1 with a single workload run, so an extra workload-id segment under
STATE_DIR would burn `sockaddr_un` budget for no isolation gain. Workloads
that need internal subdirs under STATE_DIR can still create them, but they
own the path-length budget at that point.

**Root selection.** In priority order:

- `HOMEBOY_INVOCATION_RUNTIME_DIR` env override (tests and unusual host
  configurations).
- `/tmp/hb` on every Unix host when `/tmp` is a writable directory. macOS
  apps that respect `$TMPDIR` get per-user isolation under
  `/var/folders/<14>/T/...` (~50 bytes), which leaves no realistic
  `sockaddr_un` budget. Anchoring to `/tmp` saves ~35 bytes of headroom and
  is writable on every standard macOS / Linux configuration.
- Linux fallback: `$XDG_RUNTIME_DIR/hb` when set and `/tmp` is unusable.
- macOS fallback: `$TMPDIR/hb` when `/tmp` is unusable.
- Generic fallback: `~/.cache/homeboy/inv` (or `$XDG_CACHE_HOME/homeboy/inv`).

Path components use a short opaque id (~10 hex chars) instead of a full
UUID v4. The full UUID is retained inside the on-disk invocation lease for
traceability, but is never embedded in path components.

**Path budget contract.** Homeboy guarantees that
`HOMEBOY_INVOCATION_STATE_DIR`, `HOMEBOY_INVOCATION_ARTIFACT_DIR`, and
`HOMEBOY_INVOCATION_TMP_DIR` leave at least:

- **48 bytes** of headroom under the macOS `sockaddr_un` `sun_path`
  capacity (104 bytes), and
- **32 bytes** of headroom under the Linux capacity (108 bytes).

Workloads can append a realistic workload-relative socket name like
`<workload-id>/daemon/daemon.sock` (≈40 bytes) directly under STATE_DIR and
bind to it without `EINVAL`. If `$HOME` or the configured root are
unusually long, Homeboy fails fast at invocation acquisition with a clear
error naming `sockaddr_un`, the platform-specific limit, the available
headroom, and the `HOMEBOY_INVOCATION_RUNTIME_DIR` override — instead of
letting a downstream workload hit the limit at bind time.

Rig workload object entries can request optional shared-machine primitives:

```json
{
  "bench_workloads": {
    "nodejs": [
      {
        "path": "${package.root}/bench/playground-server.bench.mjs",
        "port_range_size": 8,
        "named_leases": ["playground-browser-profile"]
      }
    ]
  }
}
```

When `port_range_size` is set, Homeboy allocates a non-overlapping local port
range for each child invocation and exports `HOMEBOY_INVOCATION_PORT_BASE` and
`HOMEBOY_INVOCATION_PORT_MAX`. Leases are persisted under Homeboy's local config
directory while the child is running, guarded by a local index lock, and stale
PID leases are pruned on the next allocation.

`named_leases` are for truly shared machine-local resources that cannot be
namespaced with dirs or ports. Conflicts fail before the workload starts and
name the held lease and holder invocation when available.

These primitives are intentionally generic enough for WordPress
Playground-style benchmarks: one invocation can run browser/server/wasm/node
services, consume multiple local ports, keep downloaded or built artifacts in
the artifact dir, and use private temp project dirs without Studio-specific
namespace code.

## Examples

```bash
# Benchmark a component with defaults (10 iterations, 5% regression threshold)
homeboy bench my-component

# Inspect the persisted observation from a completed bench run
homeboy runs show <run-id>

# List related persisted bench runs for comparison loops
homeboy runs list --kind bench --component my-component --rig studio-trunk

# List declared scenarios without executing them
homeboy bench list my-component

# 50 iterations, stricter 2% regression threshold
homeboy bench my-component --iterations 50 --regression-threshold 2.0

# Save a new baseline
homeboy bench my-component --baseline

# Run with auto-ratchet on improvement
homeboy bench my-component --ratchet

# Select a single scenario
homeboy bench my-component --scenario hot_path

# Select two scenarios in a cross-rig comparison
homeboy bench studio --rig studio-trunk,studio-branch \
    --scenario studio-agent-runtime \
    --scenario wp-admin-load

# Share warm state across invocations and run four instances in parallel
homeboy bench my-component --shared-state /tmp/homeboy-bench --concurrency 4

# Pin to a single rig — preflight + rig-scoped baseline
homeboy bench studio --rig studio-trunk

# Pin to one rig and run every component declared in bench.components
homeboy bench --rig mdi-substrates --shared-state /tmp/mdi-bench

# Cross-rig comparison: same workload, two rigs, side-by-side report.
# First rig (`studio-trunk`) is the reference; the diff table expresses
# every other rig's metrics as percent deltas vs the reference.
homeboy bench studio --rig studio-trunk,studio-combined-fixes \
    --iterations 10 \
    --report side-by-side

# Opt into parallel cross-rig execution for side-by-side exploratory runs.
homeboy bench studio --rig studio-agent-sdk,studio-bfb \
    --scenario studio-agent-site-build \
    --rig-concurrency 2

# Three-rig comparison to isolate one PR's contribution.
homeboy bench studio \
    --rig trunk,combined-fixes,combined-fixes-without-3120 \
    --iterations 20
```

## Cross-rig comparison

`--rig <a>,<b>[,<c>...]` runs the same component + workload + iteration
count against each rig and emits a single comparison envelope. By default,
rigs run in sequence for stable CI behavior. Pass `--rig-concurrency <N>`
to opt into bounded parallel rig execution for exploratory or product-demo
workflows where fresh isolated sites should be built in the same wall-clock
window.

### How it runs

For each rig, in input order by default, or in bounded parallel batches when
`--rig-concurrency` is greater than `1`:

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

Multi-rig comparison envelopes include `reports.side_by_side`, a compact
report artifact for demo and product harnesses. It references every rig
result and surfaces:

- per-rig status, exit code, and failure reason
- elapsed time when `elapsed_ms` or `duration_ms` metrics are present
- flattened key metrics, including grouped metrics like `prompt.hash_match`
- artifact paths and URLs, including URL-looking artifact paths

Parallel execution preserves the selected rig order in the output envelope,
so the reference rig and diff interpretation do not change when concurrency
is enabled.

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
    "wordpress": [
      { "path": "${package.root}/bench/studio-admin.php" }
    ]
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
  Entries must be objects with a `path` field. Paths support `~`, `${env.NAME}`, `${components.<id>.path}`, and
  `${package.root}` expansion. `${package.root}` resolves to the installed
  rig package root, so portable rig packages can keep workload files next
  to the rig spec without hardcoded machine paths.

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
      "artifacts": [
        {
          "scenario_id": "agent_boot",
          "run_index": 0,
          "name": "raw_result",
          "path": "bench-artifacts/agent_boot/run-0/raw-result.json",
          "kind": "json",
          "label": "Raw result"
        }
      ],
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

Each rig entry also includes an `artifacts` index when workloads emit
artifact pointers. The full-fidelity data remains nested under
`results.scenarios[].artifacts` and `results.scenarios[].runs[].artifacts`,
but the index makes proof artifacts easy to find in cross-rig output.
`run_index` is zero-based and omitted for scenario-level artifacts that
are not tied to a specific `--runs` iteration.

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
      "memory": { "peak_bytes": 41943040 },
      "artifacts": {
        "raw_result": {
          "path": "bench-artifacts/scenario_slug/raw-result.json",
          "kind": "json",
          "label": "Raw result"
        }
      }
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
- `artifacts` is optional. Values are local paths plus optional `kind` and
  `label` metadata. Homeboy preserves and indexes these pointers but does
  not upload, retain, or diff artifact contents.

### Artifact kind conventions

`kind` is an open string, not a closed enum. Homeboy preserves unknown kinds
so extensions can add new evidence types without requiring a core release.
Use these conventional values when they fit:

- `json` — structured machine-readable data, summaries, or transcripts.
- `directory` — a directory containing multiple related files.
- `playwright-trace` — Playwright trace archive, usually a `.zip` file.
- `screenshot` — browser or UI screenshot evidence.
- `network-log` — captured request/response metadata such as HAR or JSONL.
- `console-log` — browser console output or page-level JavaScript logs.

For browser benchmarks, prefer specific labels such as `Playwright trace`,
`Final screenshot`, `Network log`, or `Console log`. The JSON result keeps
the full artifact map under each scenario, and Homeboy also promotes artifact
pointers into the top-level `artifacts` index. Markdown reports render those
labels and paths so users can find trace and screenshot files without opening
the raw JSON payload.

### Environment variables injected

Bench scripts receive the standard runner contract plus bench-specific
variables:

- `HOMEBOY_BENCH_RESULTS_FILE` — where to write JSON output.
- `HOMEBOY_BENCH_ITERATIONS` — iteration count to use.
- `HOMEBOY_BENCH_RESPONSIVENESS_FILE` — optional JSONL file for workload
  responsiveness pings. Workloads opt in by appending pings such as
  `{ "at": "2026-06-08T00:00:00Z", "t_ms": 1200 }`; Homeboy summarizes
  `missed_ping_count`, `max_ping_gap_ms`, and `last_ping_at` in bench output.
- `HOMEBOY_BENCH_RESPONSIVENESS_MISSED_MS` — ping gap threshold used to
  classify missed responsiveness windows. Defaults to `10000`.
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
