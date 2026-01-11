# `homeboy` root command

## Synopsis

```sh
homeboy [--json <spec>] [--dry-run] <COMMAND>
```

## Description

`homeboy` is a CLI tool for development and deployment automation.

## Global flags

These are provided by clap:

- `--version` / `-V`: print version and exit
- `--help` / `-h`: print help and exit

Homeboy also defines:

- `--json <spec>`: JSON input spec override.
  - Use `-` to read from stdin, `@file.json` to read from a file, or provide an inline JSON string.
  - `--json` is global and comes before the subcommand (e.g. `homeboy --json @payload.json changelog add`).
- `--dry-run`: global dry-run mode.
  - Commands that support dry-run avoid writing local files and avoid remote side effects where applicable.
  - Some commands also have their own `--dry-run` flag for command-specific behavior (for example `deploy`, and `doctor cleanup`).

## Subcommands

See the full list of supported subcommands in the [Commands index](../commands/commands-index.md).
