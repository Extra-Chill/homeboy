# Fuzz Command

Resolve generic fuzz workload contracts for a Homeboy component.

## Synopsis

```bash
homeboy fuzz [<component>] [--workload <id>] [--run-id <id>] [--seed <seed>] [--max-duration <duration>] [-- <runner-args>]
homeboy fuzz run [<component>] [--workload <id>] [--run-id <id>] [--seed <seed>] [--max-duration <duration>] [-- <runner-args>]
homeboy fuzz list [<component>]
```

## Description

`fuzz` is the generic contract surface for future fuzz runners. Core owns the
command shape, manifest schema, JSON envelope, and Lab portability metadata.
Concrete fuzz engines remain extension-owned.

The initial `run` implementation returns a structured `planned` contract. It
does not execute a fuzzer yet.

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

## Output

Both `list` and `run` return JSON envelopes with stable `variant` values:
`list` and `run`.
