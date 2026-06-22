# `homeboy manifest`

## Synopsis

```sh
homeboy manifest
```

## Description

Prints the recursive command manifest as the standard Homeboy JSON envelope. The
payload includes visible and hidden command paths, aliases, mutation/operator
safety metadata, dry-run flags, output-contract notes, Lab routing metadata,
dangerous flags, and command docs paths.

Use this command for automation that needs a machine-readable view of the CLI
surface. Human command discovery should use `homeboy --help`, command-specific
`--help`, or `homeboy docs`.
