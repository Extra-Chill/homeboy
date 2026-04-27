# `homeboy` root command

## Synopsis

```sh
homeboy [OPTIONS] <COMMAND>
```

## Description

`homeboy` is a CLI tool for development and deployment automation.

## Global flags

These are provided by clap:

- `--version` / `-V`: print version and exit
- `--help` / `-h`: print help and exit
- `--output <PATH>`: write the structured JSON envelope to a file in addition to stdout

`--output` is a global flag, so pass it before the subcommand:

```sh
homeboy --output /tmp/homeboy-results/review.json review my-component --changed-since=origin/main
```


## Subcommands

See the full list of supported subcommands in the [Commands index](../commands/commands-index.md).
