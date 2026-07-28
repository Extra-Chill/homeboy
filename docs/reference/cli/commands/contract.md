<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy contract` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/contract.md](../../../commands/contract.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy contract`

```sh
homeboy contract <COMMAND>
```

Inspect, export, validate, and normalize Homeboy contract metadata

| Subcommand | Summary |
| --- | --- |
| `homeboy contract list` | List core-owned data contracts |
| `homeboy contract show` | Show one core-owned data contract by schema id or registry name |
| `homeboy contract export` | Export machine-consumable Homeboy contract JSON files |
| `homeboy contract validate` | Validate a JSON file against a registered generic Homeboy contract |
| `homeboy contract constants` | Export Homeboy-owned constants for a generic contract surface |
| `homeboy contract normalize` | Validate and normalize generic contract values |
| `homeboy contract materialize` | Materialize generic contract envelopes from declarative inputs |
| `homeboy contract manifest` | Print the recursive command safety, docs, and output manifest |

## `homeboy contract list`

```sh
homeboy contract list
```

List core-owned data contracts

## `homeboy contract show`

```sh
homeboy contract show <SCHEMA_ID_OR_NAME>
```

Show one core-owned data contract by schema id or registry name

| Argument | Required | Description |
| --- | --- | --- |
| `<SCHEMA_ID_OR_NAME>` | yes | Schema id or short registry name |

## `homeboy contract export`

```sh
homeboy contract export [OPTIONS]
```

Export machine-consumable Homeboy contract JSON files

| Option | Value | Description |
| --- | --- | --- |
| `--dir` | `<DIR>` | Directory to receive exported JSON contract files |

## `homeboy contract validate`

```sh
homeboy contract validate [OPTIONS] <SCHEMA_ID>
```

Validate a JSON file against a registered generic Homeboy contract

| Argument | Required | Description |
| --- | --- | --- |
| `<SCHEMA_ID>` | yes | Contract schema id to validate against |

| Option | Value | Description |
| --- | --- | --- |
| `--file` | `<PATH>` | JSON file to validate |

## `homeboy contract constants`

```sh
homeboy contract constants <CONTRACT_ID>
```

Export Homeboy-owned constants for a generic contract surface

| Argument | Required | Description |
| --- | --- | --- |
| `<CONTRACT_ID>` | yes | Contract ID: all, artifact-manifest, artifact-paths, loop, secret-env-plan, resource-lifecycle-index, host-mutation-lifecycle, run-location-index, runner-execution-record, path-materialization-plan, run-outcome-envelope, run-artifact-files, runtime-artifacts, runner-artifact-manifest-ref, reviewer-facing-ref |

## `homeboy contract normalize`

```sh
homeboy contract normalize [OPTIONS] <KIND>
```

Validate and normalize generic contract values

| Argument | Required | Description |
| --- | --- | --- |
| `<KIND>` | yes | Contract value kind to normalize Values: `artifact-ref`, `path-materialization-plan`, `run-lifecycle-status`, `runner-execution-record`, `run-outcome-envelope`. |

| Option | Value | Description |
| --- | --- | --- |
| `--input` | `<JSON>` | JSON value to normalize. If omitted, stdin is read |
| `--input-file` | `<PATH>` | Read JSON value to normalize from a file |

## `homeboy contract materialize`

```sh
homeboy contract materialize [OPTIONS] <KIND>
```

Materialize generic contract envelopes from declarative inputs

| Argument | Required | Description |
| --- | --- | --- |
| `<KIND>` | yes | Contract value kind to materialize Values: `secret-env-handoff`, `secret-env-plan`. |

| Option | Value | Description |
| --- | --- | --- |
| `--input` | `<JSON>` | JSON request to materialize. If omitted, stdin is read |
| `--input-file` | `<PATH>` | Read JSON request to materialize from a file |

## `homeboy contract manifest`

```sh
homeboy contract manifest
```

Print the recursive command safety, docs, and output manifest

