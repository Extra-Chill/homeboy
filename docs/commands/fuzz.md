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

## Manifest Shape

Extensions declare fuzz support with a product-agnostic capability block:

```json
{
  "fuzz": {
    "extension_script": "scripts/fuzz.sh",
    "workloads": [
      { "id": "parser", "label": "Parser fuzz" }
    ]
  }
}
```

Rigs can add private fuzz workloads keyed by extension id:

```json
{
  "fuzz": {
    "default_component": "woocommerce"
  },
  "fuzz_workloads": {
    "wordpress": [
      { "path": "${package.root}/fuzz/checkout-create-order.json" }
    ]
  }
}
```

## Output

Both `list` and `run` return JSON envelopes with stable `variant` values:
`list` and `run`.
