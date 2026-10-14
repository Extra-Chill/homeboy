# `homeboy workload`

Run, compare, replay and inspect **extension-declared workload kinds** (#15840).

A workload kind (`fuzz`, `bench`, `db-perf`, ...) is not built into Homeboy. An
extension declares it under `workloads.<kind>` in its manifest, and
`homeboy workload --kind <kind>` runs whatever is declared. Core reads one
generic result envelope and compares runs by the metrics and findings key the
kind declares. Everything kind-specific stays in the extension and in the
envelope's opaque `payload`.

## Subcommands

- `list --kind <k> [component] [--rig <id>]` lists the kind's workloads:
  static `workloads`, JSON-pointer probes over the component, and the runner's
  own listing when the kind declares a `script` probe (`--no-script-probe`
  skips it). It also reports the kind's capabilities.
- `run --kind <k> [component] [--rig <id>] [--workload <id>]` runs one
  workload through the shared extension runner, persists it as a run of kind
  `<k>` with the envelope as its `workload_result` artifact, and records the
  envelope's findings in the shared findings store (`homeboy runs findings`).
  `--baseline`, `--ignore-baseline` and `--ratchet` gate the run against the
  baseline stored under `baselines."workload.<k>"` in the component's
  `homeboy.json`. `run` is Lab-portable: with `--runner <id>` or
  `--placement lab` it executes on the Lab runner.
- `compare --kind <k> --from-run <a> --to-run <b>` compares two persisted
  runs. Exits 1 when the candidate regressed.
- `replay --kind <k> --run-id <id> --case-id <case>` and `minimize ...` run the
  kind's declared `replay_command` / `minimize_command` with the case env.
  `--dry-run` prints the command and env without executing. By default they
  run against the component and checkout the source run used.
- `inspect <run-id> [--kind <k>] [--full]` shows a persisted run: status,
  cases, metrics, findings and artifacts; `--full` adds the whole envelope.

`campaign` is a declared capability (`workloads.<k>.campaign`) but has no
generic command yet.

## Manifest: `workloads.<kind>`

```json
"workloads": {
  "fuzz": {
    "script": "scripts/workload/fuzz-workload.sh",
    "env": ["HOMEBOY_NODE_FUZZ_SCRIPT"],
    "runtime_helpers": [{ "id": "runtime-settings" }],
    "workloads": [{ "id": "parser", "label": "Parser" }],
    "list_probes": [
      { "type": "json_pointer", "path": "package.json", "pointer": "/scripts/fuzz", "id": "package-script-fuzz" },
      { "type": "script" }
    ],
    "result_schema": "homeboy/fuzz-campaign/v1",
    "replay_command": "bash \"$HOMEBOY_EXTENSION_PATH/scripts/workload/fuzz-replay.sh\"",
    "minimize_command": "bash \"$HOMEBOY_EXTENSION_PATH/scripts/workload/fuzz-minimize.sh\"",
    "campaign": false,
    "compare": {
      "metrics": [
        { "name": "cases_failed", "direction": "lower_is_better" },
        { "name": "p95_ms", "direction": "lower_is_better", "regression_threshold": 10 }
      ],
      "findings_key": "fingerprint"
    },
    "safety_class": "read_only"
  }
}
```

The kind id is a lowercase slug and is published as the Extension API
capability `workload.<kind>`. When several linked extensions declare the same
kind, ownership follows the usual rule:
`capability_extensions."workload.<kind>"`, then `composition.includes`.
Unknown keys are refused.

## Runner environment

| Variable | Meaning |
|---|---|
| `HOMEBOY_WORKLOAD_KIND` | The kind being executed. |
| `HOMEBOY_WORKLOAD_RESULTS_FILE` | Where the runner writes its envelope (or, in list-only mode, `{"workloads": [...]}`). |
| `HOMEBOY_WORKLOAD_ARTIFACT_DIR` | Where the runner writes artifacts; envelope artifact paths are relative to it. |
| `HOMEBOY_WORKLOAD_LIST_ONLY` | `1` for the script list probe, else `0`. |
| `HOMEBOY_WORKLOAD_ID` | Selected workload id. |
| `HOMEBOY_WORKLOAD_RUN_ID` | This run's id; for replay/minimize, the source run's id. |
| `HOMEBOY_WORKLOAD_SEED` | Requested seed, or the replayed case's seed. |
| `HOMEBOY_WORKLOAD_CASE_ID` | Replayed or minimized case. |
| `HOMEBOY_WORKLOAD_CASE_MODE` | `replay` or `minimize`. |
| `HOMEBOY_WORKLOAD_CASE_ARTIFACT_REF` | The case's `artifact_ref`, as declared. |
| `HOMEBOY_WORKLOAD_CASE_ARTIFACT_FILE` | The persisted file it resolved to, when it did. |
| `HOMEBOY_WORKLOAD_SOURCE_RESULTS_FILE` | The source run's persisted envelope. |
| `HOMEBOY_WORKLOAD_ALLOW_DESTRUCTIVE` | `1` only for an authorized destructive run. |
| `HOMEBOY_WORKLOAD_ISOLATION_PROOF_FILE` | The verified isolation proof, when authorized. |

## Result envelope (`homeboy/workload-result/v1`)

```json
{
  "schema": "homeboy/workload-result/v1",
  "kind": "fuzz",
  "workload_id": "parser",
  "status": "pass | fail | error | skipped",
  "findings": [{ "tool": "fuzz", "rule": "parse", "severity": "high", "message": "...", "fingerprint": "throw:parse:empty" }],
  "cases": [{ "case_id": "c2", "seed": "42", "artifact_ref": "c2-input", "status": "fail" }],
  "metrics": { "cases_failed": 1 },
  "artifacts": [{ "name": "c2-input", "path": "cases/c2.json", "mime": "application/json" }],
  "payload": { "schema": "homeboy/fuzz-campaign/v1" }
}
```

Findings use the shared `HomeboyFinding` shape. Unknown envelope keys are
refused, and so is an envelope whose `kind` differs from the executed kind.
Every listed artifact must exist, and every case `artifact_ref` must name a
listed artifact or a file in the artifact directory. When the kind declares
`result_schema`, `payload.schema` must match it.

A run passes only when the runner exits 0 and the envelope says `pass` (or
`skipped`). An envelope that claims `pass` from a failing runner is an error.

## Compare and gate

The single generic adapter compares two envelopes:

- **Metrics.** Only declared metrics gate. A declared metric regresses when it
  moves in the wrong `direction` by more than `regression_threshold` percent
  of the baseline value (any move when no threshold is declared), or when the
  candidate stops reporting it. Undeclared metrics are reported, never judged.
- **Findings** are matched by `fingerprint` (falling back to
  `tool|rule|message`). A new finding regresses; a resolved finding improves.
- **Status.** Going from pass to not passing regresses.
- **Cases** that newly fail are advisories.

`--ratchet` re-saves the baseline when the candidate improved without
regressing.

## Destructive kinds

A kind declaring `"safety_class": "destructive"` is refused by `run`, `replay`
and `minimize` unless the operator passes both `--allow-destructive` and
`--isolation-proof <path>` with a valid `homeboy/isolation-proof/v1`. The
proof must declare a disposable runtime that is torn down, a mutation boundary,
who verified it, and at least one proof artifact. Nothing is generated on the
operator's behalf. The proof is recorded as the run's `isolation_proof`
artifact.
