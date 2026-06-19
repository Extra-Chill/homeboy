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
- `--force-hot`: suppress resource policy warnings for intentionally hot commands
- `--allow-local-hot`: allow `--force-hot` portable Lab commands to run locally when a default Lab runner exists
- `--artifact-root <DIR>`: copy persisted run artifacts to a specific directory
- `--runner <RUNNER_ID>`: route commands with portable Lab offload support to a connected Homeboy Lab runner
- `--allow-local-fallback`: permit a selected Lab runner to fall back to local execution after offload preflight fails
- `--allow-dirty-lab-workspace`: permit Lab git workspace materialization to overwrite a dirty runner-side checkout

`--output` is a global flag, so pass it before the subcommand:

```sh
homeboy --output /tmp/homeboy-results/review.json review my-component --changed-since=origin/main
```

Resource policy warnings are stderr-only preflight notices. They currently apply
to hot commands such as `bench`, `rig up`, `fleet exec`, full-workspace
`audit` / `lint` / `test` runs, and changed-scope `audit` / `lint` / `test`
runs when `homeboy doctor resources` sees a warm or hot machine. Non-interactive
hot commands fail fast unless the work is routed through Lab/runner-hosted
execution or the caller explicitly accepts local pressure. For portable hot
commands with a default Lab runner, `--force-hot` does not implicitly keep
execution local; pass `--runner <id>` to offload or add `--allow-local-hot` only
when local controller-machine execution is intentional.

Not every hot command is offloadable. Lab offload only applies to commands with
a portable runner contract; local-only hot commands keep running locally and
explain why `--runner` is unavailable.


## Subcommands

See the full list of supported subcommands in the [Commands index](../commands/commands-index.md).

## Hidden Compatibility Aliases

`homeboy list` remains accepted as a hidden, deprecated alias for top-level help.
It is omitted from normal help and the commands index; prefer `homeboy --help`.
