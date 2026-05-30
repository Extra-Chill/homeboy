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
- `--artifact-root <DIR>`: copy persisted run artifacts to a specific directory
- `--runner <RUNNER_ID>`: offload supported hot commands to a connected Homeboy Lab runner

`--output` is a global flag, so pass it before the subcommand:

```sh
homeboy --output /tmp/homeboy-results/review.json review my-component --changed-since=origin/main
```

Resource policy warnings are stderr-only preflight notices. They currently apply
to hot commands such as `bench`, `rig up`, `fleet exec`, and unscoped
`audit` / `lint` / `test` runs when `homeboy doctor resources` sees a warm or
hot machine. They do not block execution; pass `--force-hot` when the extra load
is intentional.


## Subcommands

See the full list of supported subcommands in the [Commands index](../commands/commands-index.md).
