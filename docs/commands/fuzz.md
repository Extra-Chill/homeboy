# Fuzz Command

Resolve generic fuzz workload contracts for a Homeboy component.

## Synopsis

```bash
homeboy fuzz [<component>] [--rig <id>] [--workload <id>] [--run-id <id>] [--seed <seed>] [--max-duration <duration>] [-- <runner-args>]
homeboy fuzz run [<component>] [--rig <id>] [--workload <id>] [--run-id <id>] [--seed <seed>] [--max-duration <duration>] [-- <runner-args>]
homeboy fuzz list [<component>] [--rig <id>]
```

## Description

`fuzz` is the generic contract surface for future fuzz runners. Core owns the
command shape, manifest schema, JSON envelope, and Lab portability metadata.
Concrete fuzz engines remain extension-owned.

The initial `run` implementation returns a structured `planned` contract. It
does not execute a fuzzer yet.

With `--rig <id>`, `fuzz` resolves the rig component path and extension config,
uses `fuzz.default_component` when no component is passed, and includes
rig-owned `fuzz_workloads` for the selected extension.

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
