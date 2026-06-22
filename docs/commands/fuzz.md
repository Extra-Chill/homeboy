# Fuzz Command

List and run generic fuzz workloads for a Homeboy component or rig.

## Synopsis

```bash
homeboy fuzz [<component>] [--rig <id>] [--workload <id>] [--run-id <id>] [--seed <seed>] [--max-duration <duration>] [-- <runner-args>]
homeboy fuzz run [<component>] [--rig <id>] [--workload <id>] [--run-id <id>] [--seed <seed>] [--max-duration <duration>] [-- <runner-args>]
homeboy fuzz list [<component>] [--rig <id>]
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

Full-coverage claims need persisted proof artifacts. A neutral coverage summary
can report declared, executable, and proven counts; operation totals; skipped
reason codes; and case/manifest artifacts. Treat missing `proven` counts or
missing coverage/case artifacts as incomplete evidence, not as full coverage.

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

Both `list` and `run` return JSON envelopes with stable `variant` values:
`list` and `run`.

`run.execution.results_file` is the path advertised to the runner through
`HOMEBOY_FUZZ_RESULTS_FILE`. `run.results` is present only when the runner wrote
a valid campaign result file.
