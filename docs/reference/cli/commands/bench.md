<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy bench` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/bench.md](../../../commands/bench.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy bench`

```sh
homeboy bench [OPTIONS] [COMPONENT] [ARGS]... [COMMAND]
```

Run performance benchmarks for a component

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT]` | no | Component ID (optional — auto-detected from CWD if omitted) |
| `[ARGS]...` | no | Additional arguments to pass to the bench runner (must follow --) |

| Option | Value | Description |
| --- | --- | --- |
| `--json` | flag | Print the full JSON output instead of the compact human summary. The compact summary is the default for terminals; the full structured payload is always written to `--output <file>` and is printed to stdout with this flag. No data differs between the two — only the default presentation is compact |
| `--path` | `<PATH>` | Override the component checkout path for this invocation |
| `--extension` | `<ID>` | One-shot extension override for the current invocation |
| `--iterations` | `<ITERATIONS>` | Iterations per scenario (default 10). Forwarded to the runner via HOMEBOY_BENCH_ITERATIONS. Individual extensions may clamp |
| `--warmup` | `<N>` | Warmup iterations to run before measured iterations. Forwarded to the runner via HOMEBOY_BENCH_WARMUP_ITERATIONS. When omitted, rig bench.warmup_iterations may provide the value; otherwise the runner keeps its own default |
| `--runs` | `<COUNT>` | Number of repetitions (independent substrate spawns). Default 1 preserves today's exact behaviour. This is a numeric COUNT, not a proof label — use --run-id to tag a run with a stable identifier. When > 1, the bench dispatcher is invoked N times in sequence and per-scenario metrics carry both the cross-run p50 (top-level, unchanged shape) and a runs array with each run's raw metrics, plus a runs_summary object with n/min/max/mean/stdev/cv_pct/p50/p95 |
| `--run-id` | `<ID>` | Caller-supplied stable proof label for this run. Forwarded to component bench scripts via HOMEBOY_BENCH_RUN_ID so a run can be correlated across systems (CI logs, dashboards, proof archives). This is NOT a repetition count — use --runs for that. Components whose bench runner does not consume HOMEBOY_BENCH_RUN_ID simply ignore it; homeboy emits a notice rather than a hard error |
| `--shared-state` | `<DIR>` | Directory shared across bench runner instances |
| `--concurrency` | `<CONCURRENCY>` | Number of concurrent bench runner instances. When `--matrix` is used, this controls scheduler task concurrency |
| `--matrix` | `<NAME=VALUE[,VALUE...]>` | Matrix axis in NAME=value,value form. Repeat for multiple axes |
| `--runner-pool` | `<BACKEND>` | Generic agent-task executor backend/runner pool for matrix fan-out |
| `--max-tasks` | `<N>` | Cap the number of matrix cells accepted by the scheduler |
| `--max-queue-depth` | `<N>` | Cap the scheduler queue depth for matrix cells |
| `--expect-artifact` | `<NAME>` | Artifact name expected from each matrix cell |
| `--baseline` | flag | Persist the current run as the new baseline |
| `--ignore-baseline` | flag | Skip baseline comparison for this run |
| `--ratchet` | flag | Auto-update the baseline when the current run improves on it |
| `--regression-threshold` | `<PERCENT>` | p95 regression tolerance as a percentage. A scenario regresses when its current p95_ms exceeds baseline.p95_ms * (1 + threshold/100) |
| `--settings-json-file` | `<FILE>` | Load typed setting overrides from a JSON object file. Repeatable |
| `--setting` | `<KEY=VALUE>` | String setting override. Repeatable |
| `--setting-json` | `<SETTING_JSON>` | Typed-JSON setting override. Repeatable |
| `--json-summary` | flag | Print compact machine-readable summary (for CI wrappers) |
| `--status-file` | `<PATH>` | Write machine-readable long-loop heartbeat/status JSON to this path. The file is updated when the observation starts and again when it finishes or errors |
| `--report` | `<REPORT>` | Include a combined comparison report artifact. Currently supports `side-by-side` for multi-rig bench comparisons Values: `side-by-side`. |
| `--rig` | `<RIG_ID[,RIG_ID...]>` | Run bench against one or more homeboy rigs |
| `--rig-order` | `<RIG_ORDER>` | Order to use when running a multi-rig comparison. `input` preserves the --rig list order and keeps the first rig as the comparison reference. `reverse` flips the order so users can repeat the same comparison with the opposite cold/warm position when rigs share external daemon or cache state Values: `input`, `reverse`. |
| `--rig-concurrency` | `<RIG_CONCURRENCY>` | Number of rigs to run concurrently during a multi-rig comparison. Default 1 preserves stable sequential CI behavior. Values greater than 1 opt into bounded parallel rig execution |
| `--scenario` | `<SCENARIO_ID>` | Only run matching benchmark scenario ids. Repeat to select multiple |
| `--profile` | `<PROFILE>` | Run the named rig-defined bench profile |
| `--ci-profile` | `<ID>` | Run using env and passthrough args from a single extension-declared CI bench profile |
| `--ignore-default-baseline` | flag | Skip auto-upgrading single-rig runs into a comparison even when the rig spec declares `bench.default_baseline_rig`. Use with `--baseline` / `--ratchet` against a rig that normally auto-pairs, or to bench the candidate alone |

| Subcommand | Summary |
| --- | --- |
| `homeboy bench matrix` | Run a local settings matrix and aggregate child bench runs |
| `homeboy bench list` | List declared benchmark scenarios without executing them |

## `homeboy bench matrix`

```sh
homeboy bench matrix [OPTIONS] [COMPONENT] [ARGS]...
```

Run a local settings matrix and aggregate child bench runs

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT]` | no | Component ID (optional — auto-detected from CWD if omitted) |
| `[ARGS]...` | no | Additional arguments to pass to the bench runner (must follow --) |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Override the component checkout path for this invocation |
| `--extension` | `<ID>` | One-shot extension override for the current invocation |
| `--iterations` | `<ITERATIONS>` | Iterations per scenario (default 10). Forwarded to the runner via HOMEBOY_BENCH_ITERATIONS. Individual extensions may clamp |
| `--warmup` | `<N>` | Warmup iterations to run before measured iterations. Forwarded to the runner via HOMEBOY_BENCH_WARMUP_ITERATIONS. When omitted, rig bench.warmup_iterations may provide the value; otherwise the runner keeps its own default |
| `--runs` | `<COUNT>` | Number of repetitions (independent substrate spawns). Default 1 preserves today's exact behaviour. This is a numeric COUNT, not a proof label — use --run-id to tag a run with a stable identifier. When > 1, the bench dispatcher is invoked N times in sequence and per-scenario metrics carry both the cross-run p50 (top-level, unchanged shape) and a runs array with each run's raw metrics, plus a runs_summary object with n/min/max/mean/stdev/cv_pct/p50/p95 |
| `--run-id` | `<ID>` | Caller-supplied stable proof label for this run. Forwarded to component bench scripts via HOMEBOY_BENCH_RUN_ID so a run can be correlated across systems (CI logs, dashboards, proof archives). This is NOT a repetition count — use --runs for that. Components whose bench runner does not consume HOMEBOY_BENCH_RUN_ID simply ignore it; homeboy emits a notice rather than a hard error |
| `--shared-state` | `<DIR>` | Directory shared across bench runner instances |
| `--concurrency` | `<CONCURRENCY>` | Number of concurrent bench runner instances. When `--matrix` is used, this controls scheduler task concurrency |
| `--matrix` | `<NAME=VALUE[,VALUE...]>` | Matrix axis in NAME=value,value form. Repeat for multiple axes |
| `--runner-pool` | `<BACKEND>` | Generic agent-task executor backend/runner pool for matrix fan-out |
| `--max-tasks` | `<N>` | Cap the number of matrix cells accepted by the scheduler |
| `--max-queue-depth` | `<N>` | Cap the scheduler queue depth for matrix cells |
| `--expect-artifact` | `<NAME>` | Artifact name expected from each matrix cell |
| `--baseline` | flag | Persist the current run as the new baseline |
| `--ignore-baseline` | flag | Skip baseline comparison for this run |
| `--ratchet` | flag | Auto-update the baseline when the current run improves on it |
| `--regression-threshold` | `<PERCENT>` | p95 regression tolerance as a percentage. A scenario regresses when its current p95_ms exceeds baseline.p95_ms * (1 + threshold/100) |
| `--settings-json-file` | `<FILE>` | Load typed setting overrides from a JSON object file. Repeatable |
| `--setting` | `<KEY=VALUE>` | String setting override. Repeatable |
| `--setting-json` | `<SETTING_JSON>` | Typed-JSON setting override. Repeatable |
| `--json-summary` | flag | Print compact machine-readable summary (for CI wrappers) |
| `--status-file` | `<PATH>` | Write machine-readable long-loop heartbeat/status JSON to this path. The file is updated when the observation starts and again when it finishes or errors |
| `--report` | `<REPORT>` | Include a combined comparison report artifact. Currently supports `side-by-side` for multi-rig bench comparisons Values: `side-by-side`. |
| `--rig` | `<RIG_ID[,RIG_ID...]>` | Run bench against one or more homeboy rigs |
| `--rig-order` | `<RIG_ORDER>` | Order to use when running a multi-rig comparison. `input` preserves the --rig list order and keeps the first rig as the comparison reference. `reverse` flips the order so users can repeat the same comparison with the opposite cold/warm position when rigs share external daemon or cache state Values: `input`, `reverse`. |
| `--rig-concurrency` | `<RIG_CONCURRENCY>` | Number of rigs to run concurrently during a multi-rig comparison. Default 1 preserves stable sequential CI behavior. Values greater than 1 opt into bounded parallel rig execution |
| `--scenario` | `<SCENARIO_ID>` | Only run matching benchmark scenario ids. Repeat to select multiple |
| `--profile` | `<PROFILE>` | Run the named rig-defined bench profile |
| `--ci-profile` | `<ID>` | Run using env and passthrough args from a single extension-declared CI bench profile |
| `--ignore-default-baseline` | flag | Skip auto-upgrading single-rig runs into a comparison even when the rig spec declares `bench.default_baseline_rig`. Use with `--baseline` / `--ratchet` against a rig that normally auto-pairs, or to bench the candidate alone |
| `--setting-matrix` | `<NAME=VALUE[,VALUE...]>` | Settings matrix axis in NAME=value,value form. Repeat the flag or pass multiple axes after it, e.g. --setting-matrix clients=10,100 rounds=3 |

## `homeboy bench list`

```sh
homeboy bench list [OPTIONS] [COMPONENT] [ARGS]...
```

List declared benchmark scenarios without executing them

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT]` | no | Component ID (optional — auto-detected from CWD if omitted) |
| `[ARGS]...` | no | Additional arguments to pass to the bench runner (must follow --) |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Override the component checkout path for this invocation |
| `--extension` | `<ID>` | One-shot extension override for the current invocation |
| `--rig` | `<RIG_ID>` | Discover scenarios using a rig's component path, extension config, and rig-declared bench workloads |
| `--scenario` | `<SCENARIO_ID>` | Only list matching benchmark scenario ids. Repeat to select multiple |
| `--settings-json-file` | `<FILE>` | Load typed setting overrides from a JSON object file. Repeatable |
| `--setting` | `<KEY=VALUE>` | String setting override. Repeatable |
| `--setting-json` | `<SETTING_JSON>` | Typed-JSON setting override. Repeatable |

