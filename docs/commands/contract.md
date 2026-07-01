# `homeboy contract`

Inspect and export Homeboy-owned generic contract metadata.

## Synopsis

```sh
homeboy contract <COMMAND>
```

## Subcommands

- `list` - list registered core-owned data contracts
- `show <schema-id-or-name>` - show one registered contract by schema ID or registry name
- `constants <contract-id>` - export stable constants for one contract surface
- `export --dir <dir>` - write machine-consumable contract JSON files
- `validate <schema-id> --file <path>` - validate a JSON file against a registered contract
- `normalize <kind>` - validate and normalize contract values from JSON input

## Constants

```sh
homeboy contract constants artifact-manifest
homeboy contract constants all
```

`constants` returns the standard Homeboy JSON envelope. The payload exposes stable
schema IDs, artifact file names, and accepted reviewer-facing reference schemes
without requiring downstream consumers to link Rust.

Accepted contract IDs include `all`, `artifact-manifest`, `loop`,
`secret-env-plan`, `run-location-index`, and `reviewer-facing-ref`.

## Registry

```sh
homeboy contract list
homeboy contract show secret-env-plan
```

The registry is the central source for contract names, schema IDs, titles,
owners, summaries, and Rust type anchors.

## Export

```sh
homeboy contract export --dir ./contracts
```

Writes contract registry, public output variants, and schema catalog JSON files
for cross-language contract tests and automation.

## Validate

```sh
homeboy contract validate homeboy/secret-env-plan/v1 --file ./secret-env-plan.json
```

Use `validate` to check a JSON file against one of the registered generic
Homeboy contracts and receive a standard JSON envelope with the validation
result.

## Normalize

```sh
homeboy contract normalize artifact-ref --input '"https://example.com/artifact.json"'
homeboy contract normalize run-lifecycle-status --input '{"status":"timed_out"}'
```

Use `normalize` when automation needs to validate contract values and receive a
canonical classification in the standard JSON envelope.
