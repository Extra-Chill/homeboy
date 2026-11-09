# `homeboy` root command

## Synopsis

```sh
homeboy [OPTIONS] <COMMAND>
```

## Description

`homeboy` is headless automation for agentic software engineering workflows. It
keeps local developers, CI, scheduled jobs, and coding agents on the same
component-aware command surface and structured evidence contract.

## Global flags

These are provided by clap:

- `--version` / `-V`: print version and exit
- `--help` / `-h`: print help and exit
- `--output <PATH>`: write the structured JSON envelope to a file in addition to stdout
- `--placement <auto|local|lab|lab-or-local>`: select automatic placement, intentional controller execution, required Lab execution, or Lab execution that may fall back to the controller after offload preflight fails
- `--artifact-root <DIR>`: copy persisted run artifacts to a specific directory
- `--runner <RUNNER_ID>`: route commands with portable Lab offload support to a connected Homeboy Lab runner
- `--allow-dirty-lab-workspace`: permit Lab git workspace materialization to overwrite a dirty runner-side checkout

`--output` is a global flag, so pass it before the subcommand:

```sh
homeboy --output /tmp/homeboy-results/review.json review my-component --changed-since=origin/main
```

Resource policy warnings are stderr-only preflight notices. They currently apply
to hot commands such as `bench`, `rig up`, full-workspace
`lint` / `test` runs, and changed-scope `lint` / `test`
runs when `homeboy self doctor` sees a warm or hot machine. Non-interactive
hot commands fail fast unless the work is routed through Lab/runner-hosted
execution or the caller explicitly accepts local pressure. For portable hot
commands with a default Lab runner, Homeboy auto-selects the default runner
when `--runner` is omitted.
Use `--placement local` only when controller-machine execution is intentional;
use `--placement lab` when local fallback is unacceptable. Removed placement
flags are rejected with a migration error; Homeboy provides no legacy support.
When `--runner <RUNNER_ID>` explicitly routes a portable command, resource
output reports controller preflight and transport overhead separately from the
runner workload; it does not recommend the supplied runner again or describe
the workload as starting locally. Any authorized local fallback is reported by
the Lab offload transition with its exact reason.

Not every hot command is offloadable. Lab offload only applies to commands with
a portable runner contract; local-only hot commands keep running locally and
explain why `--runner` is unavailable.


## Subcommands

Run `homeboy --help` for the exact supported subcommands, or
`homeboy self docs commands/commands-index` for the runtime-derived index.

Automation that needs command safety, docs, output, and Lab metadata should use
the first-class recursive manifest surface:

```sh
homeboy contract manifest
```

## Removed commands

Commands removed on purpose are listed in one registry,
`crates/homeboy-cli/src/removed_commands.rs` (`REMOVED_COMMANDS`). Each entry
records the command path (for example `rig sync`), its replacement or no
replacement, a one-line migration hint, and the Homeboy version that removed it.

Invoking a registered path exits with code `2` and a typed `command.removed`
error instead of clap's bare `unrecognized subcommand`. Stderr names the removed
path, removal version, replacement, and hint; stdout carries the
`homeboy/command-result/v3` envelope whose `diagnostics.details` holds
`removed_path`, `replacement` (`null` when nothing replaces it), `hint`, and
`removed_in`. Unknown commands that are not in the registry keep the normal clap
error. When a release removes a command, add its path to the registry.
