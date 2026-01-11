# `homeboy` root command

## Synopsis

```sh
homeboy [--json <spec>] <COMMAND>
```

## Description

`homeboy` is a CLI tool for development and deployment automation.

## Global flags

These are provided by clap:

- `--version` / `-V`: print version and exit
- `--help` / `-h`: print help and exit

Homeboy also defines:

- `--json <spec>`: enable JSON input mode for a command.
  - Currently supported only for `homeboy changelog`.
  - For all other commands, using `--json` returns a `validation.invalid_argument` error.

## Subcommands

See the full list of supported subcommands in the [Commands index](../commands/commands-index.md).
