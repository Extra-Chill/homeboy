# Fuzz Command

List and run generic fuzz workloads for a Homeboy component or rig.

## Synopsis

```bash
homeboy fuzz [<component>] [--rig <id>] [--workload <id>] [--run-id <id>] [--seed <seed>] [--inventory <path>] [--gate-profile <measurement|evidence|coverage-complete|strict>] [--require-case-log] [--require-coverage-summary] [--require-result-envelope] [--max-duration <duration>] [--action-model <path>] [--exploration-policy <path>] [--allow-destructive --isolation isolated --isolation-proof <path>] [-- <runner-args>]
homeboy fuzz run [<component>] [--rig <id>] [--workload <id>] [--run-id <id>] [--seed <seed>] [--inventory <path>] [--gate-profile <measurement|evidence|coverage-complete|strict>] [--require-case-log] [--require-coverage-summary] [--require-result-envelope] [--max-duration <duration>] [--action-model <path>] [--exploration-policy <path>] [--allow-destructive --isolation isolated --isolation-proof <path>] [-- <runner-args>]
homeboy fuzz list [<component>] [--rig <id>]
homeboy fuzz plan [<component>] [--rig <id>] [--workload <id>] [--inventory <path>] [--gate-profile <measurement|evidence|coverage-complete|strict>] [--strategy <all|read-only|crud|coverage-gaps>] [--operation <filter>] [--operation-family <family>] [--case-budget <count>] [--duration-budget-seconds <seconds>] [--action-model <path>] [--exploration-policy <path>] [--allow-destructive --isolation isolated --isolation-proof <path>]
homeboy fuzz validate <results-file>
homeboy fuzz report <results-file> [<component>] [--run-id <id>] [--inventory <path>] [--gate-profile <measurement|evidence|coverage-complete|strict>] [--output-envelope <path>]
homeboy fuzz compare <baseline-envelope> <candidate-envelope> [--hotspot-policy <advisory|blocking|off>]
homeboy fuzz replay [<artifact-or-case>] [--artifact <path>] [--case-id <id>] [--run-id <id>] [-- <runner-args>]
homeboy fuzz minimize [<artifact-or-case>] [--artifact <path>] [--case-id <id>] [--run-id <id>] [--dry-run] [-- <runner-args>]
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

Runner scripts also receive `HOMEBOY_FUZZ_ARTIFACTS_DIR`, a generic directory in
the command run directory for raw artifacts that are too specific to normalize in
core during execution. Runners can place case logs, coverage reports, replay
data, minimized reproducers, hotspot sets, and engine-native traces there, then
reference those files from the campaign `artifacts` list or `metadata.artifact_refs`.
`homeboy fuzz run` persists this directory as a `fuzz_artifacts` run artifact and
validates local refs that point inside it when possible. Homeboy core owns the
path contract; extensions own artifact meaning and any runner/offload upload
implementation beyond the persisted fuzz result envelope.

When a runner or extension promotes an artifact directory through `runner exec
--artifact-dir`, Homeboy records each direct file or directory child as a generic
run artifact. A child JSON file with a Homeboy fuzz schema is recognized as typed
evidence: `homeboy/fuzz-result-envelope/v1` becomes `fuzz_result_envelope`,
`homeboy/fuzz-observation-set/v1` becomes `fuzz_observation_set`, and
`homeboy/fuzz-hotspot-set/v1` becomes `fuzz_hotspot_set`. Result envelopes with
an embedded observation set also derive persisted observation and hotspot
artifacts for `runs hotspots` and `runs fuzz-compare`.

Runner-specific schemas such as mutation-isolation or delete-boundary reports are
not Homeboy core schemas. Store them as generic artifacts, or wrap their neutral
measurements in `homeboy/fuzz-observation-set/v1` / `homeboy/fuzz-hotspot-set/v1`
when downstream agents need portable hotspot comparison. The producer owns the
schema meaning; Homeboy core stores the bytes, metadata, and typed fuzz contracts.

Runners that collect action, query, resource, timing, or counter measurements can
emit a `homeboy/fuzz-observation-set/v1` artifact. Each observation includes a
generic `family`, optional `case_id` / `target_id` / `operation_id`, `phase`,
`subject`, `metric`, numeric `value`, `unit`, optional `fingerprint`, and
`sample_count`. Product-specific details stay in observation `metadata` or
flattened extras while Homeboy gets a stable stream for relative hotspot and
coverage analysis.

Fuzz runs are measurement-first by default. `--gate-profile measurement` records
the campaign, artifacts, coverage, observations, and hotspots without requiring
default threshold gates. Use stricter profiles when a workflow is ready to turn
evidence into a pass/fail contract:

- `measurement`: no required artifacts or gates.
- `evidence`: requires replayable result/case/replay evidence and no open findings.
- `coverage-complete`: requires coverage summary and complete target/operation coverage.
- `strict`: requires evidence and complete coverage gates.

Strict proof runs can also require the runner to emit key fuzz artifacts directly
from `homeboy fuzz run`:

```bash
homeboy fuzz run --rig <rig-id> --workload <workload-id> \
  --require-case-log \
  --require-coverage-summary \
  --require-result-envelope
```

These flags preserve the default permissive runner contract unless requested. In
strict mode, `--require-case-log` requires a campaign artifact with id or kind
`case-log` / `case_log`; `--require-coverage-summary` requires either a campaign
`coverage_summary` or a `coverage-summary` / `coverage_summary` artifact; and
`--require-result-envelope` requires a `result-envelope` / `result_envelope`
artifact. Missing strict artifacts fail the run after extension execution, so
the runner stdout/stderr and raw results path remain available for diagnosis.

`homeboy fuzz plan --inventory <path>`, `homeboy fuzz run --inventory <path>`,
and `homeboy fuzz report --inventory <path>`
accept a `homeboy/fuzz-target-inventory/v1` JSON file with discovered
`surfaces`, `targets`, `workloads`, and `seeds`. Homeboy validates the inventory,
merges it with the generated target inventory and declared workload metadata,
embeds it in generated result envelope metadata when reporting, and exposes the
path to runners as `HOMEBOY_FUZZ_INVENTORY_FILE`. The inventory contract is
product-neutral; product-specific details belong in `metadata` or flattened extra
fields on the inventory items.

`homeboy fuzz plan --inventory <path>` emits a
`homeboy/fuzz-execution-request/v1` request in the command output. The request
metadata includes the planner strategy, selected target ids, selected operation
families, selected operation ids, seed/corpus refs, effective budgets, isolation
requirements, selected gate profile, required artifact ids, gate ids, inventory
provenance, and skipped target or operation reasons. The planner is product-neutral: it uses inventory-declared
operation families and safety classes, not product-specific target names.

`homeboy fuzz plan --action-model <path>` and
`homeboy fuzz run --action-model <path>` accept a
`homeboy/fuzz-action-model/v1` JSON contract. The action model declares generic
actions with ids, free-form kinds, optional canonical operation families,
non-negative weights, opaque input generator refs, preconditions, effects, and
invariants. Homeboy validates the schema/version and preserves the contract in
the execution request metadata; concrete generation and execution behavior stays
runner-owned.

```json
{
  "schema": "homeboy/fuzz-action-model/v1",
  "version": 1,
  "id": "generic-actions",
  "actions": [
    {
      "id": "resource.read",
      "kind": "read",
      "family": "read",
      "weight": 3.0,
      "input_generators": ["generator:resource-id"],
      "preconditions": ["resource.exists"],
      "effects": ["observation.recorded"],
      "invariants": ["resource.integrity"]
    }
  ]
}
```

`homeboy fuzz plan --exploration-policy <path>` and
`homeboy fuzz run --exploration-policy <path>` accept a
`homeboy/fuzz-exploration-policy/v1` JSON contract. The policy declares generic
planning constraints such as action model refs, action weights, case and duration
budgets, reset cadence, replay seed refs, corpus refs, and invariants. Homeboy
validates and embeds the policy without implementing downstream exploration.

```json
{
  "schema": "homeboy/fuzz-exploration-policy/v1",
  "version": 1,
  "id": "bounded-exploration",
  "action_model_ref": "generic-actions",
  "action_weights": { "resource.read": 3.0 },
  "case_budget": 50,
  "duration_budget_seconds": 300,
  "reset_cadence": "after_each_case",
  "replay_seed_ref": "seed:stable-1",
  "corpus_refs": ["corpus:generic-fixture"],
  "invariants": ["resource.integrity"]
}
```

Selection strategies are intentionally small. `all` selects supported
non-destructive inventory operations. `read-only` selects read-like families,
`crud` selects create/update/delete families, and `coverage-gaps` currently uses
the same neutral selection surface as `all` while recording the requested
strategy for downstream runners that can prioritize gaps. Repeat `--operation`
to filter by operation id, operation kind, or canonical family. Repeat
`--operation-family` to filter by canonical family. Unknown or non-canonical
operation families are preserved in the inventory and reported under skipped
operations with reason `unsupported`; destructive surfaces are skipped with
reason `destructive`.

Destructive fuzz is an explicit contract. `--allow-destructive` enables
destructive selection only when `--isolation isolated` and `--isolation-proof`
point at a complete `homeboy/isolation-proof/v1` JSON document. Homeboy does not
infer destructive support from runner environment variables, Lab placement, or
provider features. Missing or incomplete proof fails planning/request validation;
it is not downgraded to a compatibility fallback.

The proof contract is product-neutral. Provider-specific identifiers can appear
only as opaque `provider_ref` or artifact refs; Homeboy core interprets the
generic safety fields:

```json
{
  "schema": "homeboy/isolation-proof/v1",
  "version": 1,
  "runtime_kind": "ephemeral-runner",
  "provider_ref": { "id": "opaque-provider-owned-ref" },
  "disposable": true,
  "teardown_required": true,
  "mutation_boundary": "runner-workspace",
  "proof_artifacts": [
    { "kind": "log", "ref": "artifact://isolation-proof" }
  ],
  "verified_by": "lab-controller"
}
```

For destructive planning or execution, `disposable` and `teardown_required` must
be `true`; `runtime_kind`, `mutation_boundary`, `verified_by`, and at least one
`proof_artifacts` entry must be present. The teardown proof represents discard of
the disposable mutation boundary, not restoration of a durable environment.
Existing runner/provider capabilities such as `snapshot_ref` or `reset_supported`
can be included as optional evidence, but Homeboy does not require rollback,
restore, reset, or checkpoint support for destructive fuzzing inside an explicit
disposable boundary. `homeboy fuzz plan` and `homeboy fuzz run` embed the accepted
proof in the `homeboy/fuzz-execution-request/v1` request as `isolation_proof`.

Operations keep the free-form `kind` string for product-owned semantics and can
also carry a canonical `family` for cross-runner coverage reporting. When
`family` is omitted, Homeboy normalizes known neutral kinds and HTTP-style verbs
to families such as `read`, `create`, `update`, `delete`, `list`, `search`,
`navigate`, `render`, `query`, `load`, `submit`, and `performance_probe`.
Product-specific render kinds, such as template rendering, should keep
their precise meaning in `kind`, `target.kind`, tags, or metadata while using
the generic `render` family for cross-runner reporting. Unknown `kind` values
remain valid and are preserved without a canonical family.

Targets, operations, findings, and hotspots may include optional `source_refs`
for generic code/corpus/config coverage pointers. Source ref meaning is
producer-owned; Homeboy preserves the refs for cross-artifact reporting without
embedding product-specific source taxonomies.

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

Replay and minimize resolve a validated contract before execution. The output
includes the campaign/envelope ids, selected case id, matching `replay` metadata
when present, passthrough args, and environment variables for the originating
extension-owned replay or minimization runner:

```text
HOMEBOY_FUZZ_REPLAY_ARTIFACT_FILE
HOMEBOY_FUZZ_REPLAY_CASE_ID
HOMEBOY_FUZZ_REPLAY_ID
HOMEBOY_FUZZ_REPLAY_SEED
HOMEBOY_FUZZ_REPLAY_ARTIFACT_ID
HOMEBOY_FUZZ_RUN_ID
HOMEBOY_FUZZ_REPLAY_RUN_ID
```

`homeboy fuzz replay` executes extension-owned `fuzz.replay_command` when a
component/rig extension context declares one. `homeboy fuzz minimize` executes
extension-owned `fuzz.minimize_command` using the same artifact, case, replay
metadata, env, placeholder, and passthrough-argument contract. Both commands
support `--dry-run` to inspect metadata and command generation without execution.

Homeboy does not fake replay or minimization without a resolved
component/extension context. If the extension manifest omits the relevant command,
the CLI returns `unsupported` and prints the resolved contract. Concrete replay
and minimization behavior belongs to extension scripts.

Manifest commands support placeholders that Homeboy shell-quotes before
execution: `{artifact}`, `{artifact_file}`, `{case}`, `{case_id}`, `{run_id}`,
`{replay}`, `{replay_id}`, `{seed}`, `{replay_seed}`, `{artifact_id}`,
`{case_artifact}`, and `{replay_artifact_id}`. Additional CLI args after `--`
are appended to the rendered extension command.

## Portable Fuzz Evidence Bundles

`homeboy runs export --run <run-id> --output <dir>` exports runs, artifacts,
trace spans, findings, and test failures as a portable observation bundle. When a
file artifact is available locally, the bundle includes its bytes under
`artifact-bytes/`, records refs/checksums/sizes in `artifact_bytes.json`, and
stamps the exported artifact with `bundle://...`, SHA-256, size, and
`metadata_json.portable_bundle`. Missing local files and directories remain
metadata-only refs.

`homeboy runs import <dir>` validates bundled artifact byte checksums and sizes
before importing. Imported artifacts with valid bundle bytes point at the bundled
file path, preserving bytes for downstream inspection without relying on the
producer machine's original local path.

Full-coverage claims need persisted proof artifacts. A neutral coverage summary
can report declared, executable, and proven counts; operation totals; skipped
reason codes; and case/manifest artifacts. Treat missing `proven` counts or
missing coverage/case artifacts as incomplete evidence, not as full coverage.
`homeboy fuzz validate` and `homeboy fuzz report` evaluate coverage completeness
gates from `coverage_summary`: `target-coverage-complete` and
`operation-coverage-complete` pass only when every declared target/operation is
proven, or when the summary explicitly declares zero targets/operations. Missing
`coverage_summary` fails those completeness gates.

Use `homeboy fuzz compare` to compare a persisted baseline result envelope with a
candidate envelope:

```bash
homeboy fuzz compare baseline-envelope.json candidate-envelope.json
```

The command emits a `homeboy/fuzz-compare/v1` JSON artifact with coverage, case
status, finding severity, required artifact, gate-status, and hotspot deltas. The
blocking compare status is `worse` when candidate coverage drops, failure rate
increases, critical findings appear, required artifacts go missing, a gate changes
from passed to failed, or hotspot regressions are compared with
`--hotspot-policy blocking`. It is `better` when only blocking improvements are
present, and `same` when no blocking tracked deltas change.

Relative hotspot comparison is measurement-first by default. Hotspots are compared
by stable hotspot id and Homeboy records rank, relative-score, and value deltas
without requiring a product-specific threshold. The default
`--hotspot-policy advisory` classifies new or hotter hotspots as advisory
regressions, sets `advisory_status` to `worse`, and leaves the blocking `status`
unchanged. Use `--hotspot-policy blocking` when a workflow has decided that
relative hotspot regressions should fail the compare, or `--hotspot-policy off`
to keep raw hotspot deltas without advisory/blocking classification.

Measurement-first production fuzzing can compare two envelopes like this:

```bash
homeboy fuzz compare baseline-envelope.json candidate-envelope.json \
  --hotspot-policy advisory
```

The resulting `hotspot_summary` reports the policy, hotspot status,
advisory/blocking regression counts, improvements, new hotspots, and resolved
hotspots. Individual `deltas.hotspot_deltas[]` entries include
`classification` values such as `advisory_regression`, `blocking_regression`,
`measured_regression`, `advisory_improvement`, `blocking_improvement`,
`measured_improvement`, and `unchanged`.

Persisted run artifacts can be compared without local artifact paths:

```bash
homeboy runs fuzz-compare --from-run fuzz-baseline --to-run fuzz-candidate \
  --hotspot-policy advisory
homeboy runs hotspots --baseline-run fuzz-baseline --candidate-run fuzz-candidate
```

`runs hotspots` consumes persisted typed fuzz observation and hotspot artifacts
and returns a cohort comparison without threshold or gate semantics.

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
the controller and runner Homeboy versions with `homeboy runner status <id>`.
The status output includes command availability checks such as
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

Extensions can also publish generic campaign metadata. Homeboy surfaces these
fields in `fuzz run` output without interpreting product-specific runner
details.

Fuzz workloads can declare a generic lifecycle contract when a mutable runtime
must be prepared, seeded, snapshotted, reset, rolled back, or torn down safely:

```json
{
  "fuzz": {
    "extension_script": "scripts/fuzz.sh",
    "case_artifact": "failing-case",
    "corpus_artifacts": ["seed-corpus", "generated-corpus"],
    "seed": "default-seed",
    "replay_command": "runner replay {case_artifact}",
    "minimize_command": "runner minimize {case_artifact}",
    "result_schema": "homeboy/fuzz-campaign/v1",
    "artifact_retention": "persisted-run-artifacts",
    "workloads": [
      {
        "id": "parser",
        "label": "Parser fuzz",
        "lifecycle": {
          "schema": "homeboy/lifecycle-contract/v1",
          "phases": [
            { "id": "prepare", "phase": "prepare", "extension_hook": "runtime.prepare" },
            { "id": "snapshot", "phase": "snapshot", "extension_hook": "runtime.snapshot" },
            { "id": "reset", "phase": "reset", "extension_hook": "runtime.reset" },
            { "id": "teardown", "phase": "teardown", "extension_hook": "runtime.teardown" }
          ]
        }
      }
    ]
  }
}
```

When a runner executes lifecycle phases, `run.results.lifecycle.snapshot_refs`
records the snapshot refs that reviewers and replay tooling can trace. See
`docs/architecture/lifecycle-contracts.md` for the shared shape.

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

`contract`, `list`, `plan`, `run`, `validate`, `report`, `replay`, and
`minimize` return JSON envelopes with stable `variant` values.

`run.execution.results_file` is the path advertised to the runner through
`HOMEBOY_FUZZ_RESULTS_FILE`. `run.results` is present only when the runner wrote
a valid campaign result file.

`run.campaign_contract` always includes `case_artifact`, `corpus_artifacts`,
`seed`, `replay_command`, `minimize_command`, `result_schema`,
`artifact_retention`, and `unsupported`. Missing extension metadata is rendered
as empty/null values and named in `unsupported`, so automation can distinguish an
unsupported replay/minimize/corpus contract from a runner that provided one.
