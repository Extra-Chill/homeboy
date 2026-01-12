# `homeboy` root command

## Synopsis

```sh
homeboy [--dry-run] <COMMAND>
```

## Description

`homeboy` is a CLI tool for development and deployment automation.

## Global flags

These are provided by clap:

- `--version` / `-V`: print version and exit
- `--help` / `-h`: print help and exit

Homeboy also defines:

- `--dry-run`: global dry-run mode.
  - Commands that support dry-run avoid writing local files and avoid remote side effects where applicable.

Note: some subcommands also define their own `--dry-run` flag (for example `deploy`, and `doctor cleanup`). This is separate from the global flag.

## Subcommands

See the full list of supported subcommands in the [Commands index](../commands/commands-index.md).
