# `homeboy artifact-postprocess`

Run a generic artifact postprocess plan over persisted artifact roots.

## Synopsis

```sh
homeboy artifact-postprocess [OPTIONS] <PLAN>
```

## Arguments

- `<PLAN>` - artifact postprocess plan JSON file, `@file` spec, or `-` for stdin.

## Options

- `--artifact-root-id <ID>` - artifact root id from the plan to use as `HOMEBOY_ARTIFACT_POSTPROCESS_ARTIFACT_ROOT`.
- `--input-root-id <ID>` - optional artifact root id from the plan to expose as `${run.input}`.
- `--result <PATH>` - write the bare artifact-postprocess result contract to this path.

Global options such as `--output <PATH>` are also accepted.

## Output

The command returns the standard Homeboy JSON envelope. The payload includes the
selected plan file, selected artifact root ids, optional result file, and the
`homeboy/artifact-postprocess-result/v1` result contract.

When `--result` is provided, Homeboy writes the bare result contract to that path
in addition to the normal command envelope.

## Contract

`artifact-postprocess` consumes the generic
`homeboy/artifact-postprocess/v1` plan contract. Plans declare persisted artifact
roots and helper-driven actions without product-specific semantics.

See [artifact postprocess runner contract](../architecture/artifact-postprocess-runner-contract.md).
