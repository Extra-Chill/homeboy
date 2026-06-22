# Fuzz Command

List and run generic fuzz workloads for a Homeboy component or rig.

## Synopsis

```bash
homeboy fuzz [<component>] [--rig <id>] [--workload <id>] [--run-id <id>] [--seed <seed>] [--inventory <path>] [--max-duration <duration>] [-- <runner-args>]
homeboy fuzz run [<component>] [--rig <id>] [--workload <id>] [--run-id <id>] [--seed <seed>] [--inventory <path>] [--max-duration <duration>] [-- <runner-args>]
homeboy fuzz list [<component>] [--rig <id>]
homeboy fuzz plan [<component>] [--rig <id>] [--workload <id>] [--inventory <path>]
homeboy fuzz validate <results-file>
homeboy fuzz report <results-file> [<component>] [--run-id <id>] [--inventory <path>] [--output-envelope <path>]
homeboy fuzz replay [<artifact-or-case>] [--artifact <path>] [--case-id <id>] [--run-id <id>] [-- <runner-args>]
```

## Description

`fuzz` is the generic contract surface for fuzz runners. Core owns the command
shape, manifest schema, JSON envelope, persisted run/artifact evidence, and Lab
portability metadata. Concrete fuzz engines remain extension-owned.

With `--rig <id>`, `fuzz` resolves the rig component path and extension config,
uses `fuzz.default_component` when no component is passed, and includes
rig-owned `fuzz_workloads` for the selected extension.

## Operator Workflow

Start by listing the workloads declared by the rig and the selected extension:

```bash
homeboy fuzz list --rig <rig-id>
```

`fuzz list` is the declared-workload view. It is not proof that a workload ran.
Use the listed workload id in `fuzz run`, then use `runs show` or `runs evidence`
to inspect the executable/proven state and recorded artifacts.

Run one workload through the fuzz command surface:

```bash
homeboy fuzz run --rig <rig-id> --workload <workload-id>
```

The run output should include a persisted run id. Inspect the recorded evidence
through `homeboy runs` instead of opening temporary runner paths directly:

```bash
homeboy runs show <run-id>
homeboy runs artifact get <run-id> <artifact-id> --output <path>
```

`homeboy runs show` renders the compact summary, coverage metadata when the
runner provides it, and fetch commands for recorded artifacts such as failing
cases, repro cases, and coverage reports. Use `homeboy runs artifact get` for
artifact bytes that are stored locally or mirrored from a runner.

Runner scripts receive `HOMEBOY_FUZZ_RESULTS_FILE` pointing at
`fuzz-results.json` in the command run directory. When a runner writes a
`homeboy/fuzz-campaign/v1` campaign object there, `homeboy fuzz run` parses it
and returns it as `results` in the JSON envelope. Malformed JSON fails the run
instead of being treated as proof.

`homeboy fuzz plan --inventory <path>`, `homeboy fuzz run --inventory <path>`,
and `homeboy fuzz report --inventory <path>`
accept a `homeboy/fuzz-target-inventory/v1` JSON file with discovered
`surfaces`, `targets`, `workloads`, and `seeds`. Homeboy validates the inventory,
merges it with the generated target inventory and declared workload metadata,
embeds it in generated result envelope metadata when reporting, and exposes the
path to runners as `HOMEBOY_FUZZ_INVENTORY_FILE`. The inventory contract is
product-neutral; product-specific details belong in `metadata` or flattened extra
fields on the inventory items.

Operations keep the free-form `kind` string for product-owned semantics and can
also carry a canonical `family` for cross-runner coverage reporting. When
`family` is omitted, Homeboy normalizes known neutral kinds and HTTP-style verbs
to families such as `read`, `create`, `update`, `delete`, `list`, `search`,
`navigate`, `render`, `query`, `load`, `submit`, `block_render`, and
`performance_probe`. Unknown `kind` values remain valid and are preserved without
a canonical family.

```json
{
  "id": "endpoint-list",
  "kind": "GET",
  "family": "read"
}
```

Campaigns can include a product-neutral coverage summary:

```json
{
  "schema": "homeboy/fuzz-campaign/v1",
  "id": "campaign-1",
  "safety_class": "read_only",
  "coverage_summary": {
    "schema": "homeboy/fuzz-coverage-summary/v1",
    "declared_targets": 2,
    "executable_targets": 2,
    "proven_targets": 2,
    "declared_operations": 4,
    "executable_operations": 4,
    "proven_operations": 4,
    "skipped_targets": [
      { "id": "target-3", "reason": "auth_required" }
    ],
    "surface_summaries": [
      {
        "id": "surface-a",
        "kind": "api",
        "declared_targets": 2,
        "executable_targets": 2,
        "proven_targets": 2,
        "declared_operations": 3,
        "executable_operations": 3,
        "proven_operations": 3
      }
    ],
    "kind_summaries": [
      {
        "id": "read",
        "kind": "operation_kind",
        "declared_targets": 1,
        "executable_targets": 1,
        "proven_targets": 1,
        "declared_operations": 2,
        "executable_operations": 2,
        "proven_operations": 2
      }
    ],
    "artifact_ids": ["coverage-report"]
  }
}
```

Coverage summaries can include selector breakdowns in `surface_summaries` and
`kind_summaries`. Each selector row uses the same declared, executable, proven,
and skipped-count shape as the aggregate summary, allowing gates and reports to
show which surface or operation kind is incomplete without embedding any
product-specific taxonomy.

Use standardized skip reason codes when a declared target or operation is not
executable in the current campaign: `unsafe`, `destructive`, `auth_required`,
`unavailable`, `legacy`, `unsupported`, and `config_required`. The codes are
reported in `coverage_completeness.skipped_reason_counts` for `fuzz validate`
and `fuzz report`, including per-selector counts for `surface_summaries` and
`kind_summaries`.

`homeboy fuzz replay` resolves replay metadata from a product-neutral campaign
or result envelope artifact. Pass a `homeboy/fuzz-campaign/v1` or
`homeboy/fuzz-result-envelope/v1` JSON file as the positional argument, or pass
it with `--artifact <path>` and use the positional argument as the case id:

```bash
homeboy fuzz replay fuzz-results.json --case-id case-1
homeboy fuzz replay case-1 --artifact fuzz-results.json
```

Replay currently returns a validated `dry_run` contract rather than executing a
runner. The output includes the campaign/envelope ids, selected case id, matching
`replay` metadata when present, passthrough args, and environment variables for
the originating extension-owned replay runner:

```text
HOMEBOY_FUZZ_REPLAY_ARTIFACT_FILE
HOMEBOY_FUZZ_REPLAY_CASE_ID
HOMEBOY_FUZZ_REPLAY_ID
HOMEBOY_FUZZ_REPLAY_SEED
HOMEBOY_FUZZ_REPLAY_ARTIFACT_ID
HOMEBOY_FUZZ_RUN_ID
```

Homeboy does not fake replay execution without a resolved component/extension
context. Extension scripts own concrete replay execution.

Full-coverage claims need persisted proof artifacts. A neutral coverage summary
can report declared, executable, and proven counts; operation totals; skipped
reason codes; and case/manifest artifacts. Treat missing `proven` counts or
missing coverage/case artifacts as incomplete evidence, not as full coverage.
`homeboy fuzz validate` and `homeboy fuzz report` evaluate coverage completeness
gates from `coverage_summary`: `target-coverage-complete` and
`operation-coverage-complete` pass only when every declared target/operation is
proven, or when the summary explicitly declares zero targets/operations. Missing
`coverage_summary` fails those completeness gates.

Fuzz workloads do not have a benchmark fallback. If `homeboy fuzz run` cannot
execute the selected workload, fix the fuzz runner, rig declaration, or Lab
routing and re-run `homeboy fuzz run`; do not substitute `homeboy bench` as fuzz
proof. Benchmark runs measure performance and baseline deltas, while fuzz runs
exercise generated or discovered cases and preserve fuzz-specific case evidence.

Heavy fuzz campaigns should run through Homeboy's offloaded Lab path. Use the
normal `homeboy fuzz run --rig ... --workload ...` command and let Lab routing
select the runner, or pass a runner explicitly with the global `--runner <id>`
flag when required. The reviewer-facing proof is the persisted run plus artifact
refs surfaced by `homeboy runs show` and `homeboy runs artifact get`, not a
controller-local hot run or a benchmark surrogate.

If `homeboy fuzz` is present in source but unavailable on a Lab runner, compare
the controller and runner Homeboy versions with `homeboy lab status --runner
<id>`. The status output includes command availability checks such as
`homeboy fuzz --help`; refresh or upgrade the runner binary before rerunning the
campaign.

## Manifest Shape

Extensions declare fuzz support with a product-agnostic capability block:

```json
{
  "fuzz": {
    "workloads": [
      { "id": "parser", "label": "Parser fuzz" }
    ]
  }
}
```

The `fuzz` block is valid manifest support with only workload metadata. Add
`"extension_script": "scripts/fuzz.sh"` when the extension is ready to execute
workloads through `homeboy fuzz run`.

Rigs can add private fuzz workloads keyed by extension id:

```json
{
  "fuzz": {
    "default_component": "package"
  },
  "fuzz_workloads": {
    "generic": [
      { "path": "${package.root}/fuzz/parser.json" }
    ]
  }
}
```

## Output

`contract`, `list`, `plan`, `run`, `validate`, `report`, and `replay` return
JSON envelopes with stable `variant` values.

`run.execution.results_file` is the path advertised to the runner through
`HOMEBOY_FUZZ_RESULTS_FILE`. `run.results` is present only when the runner wrote
a valid campaign result file.
