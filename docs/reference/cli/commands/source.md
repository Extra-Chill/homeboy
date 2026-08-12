<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
cargo run -p homeboy-cli --bin generate-cli-reference
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy source` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/source.md](../../../commands/source.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy source`

```sh
homeboy source <COMMAND>
```

Inspect sealed source-package admissibility without staging resources

| Subcommand | Summary |
| --- | --- |
| `homeboy source package` | Check whether a directory satisfies the sealed Lab source-package policy |

## `homeboy source package`

```sh
homeboy source package <COMMAND>
```

Check whether a directory satisfies the sealed Lab source-package policy

| Subcommand | Summary |
| --- | --- |
| `homeboy source package check` | Scan a source directory without creating any Homeboy resources |

## `homeboy source package check`

```sh
homeboy source package check [OPTIONS]
```

Scan a source directory without creating any Homeboy resources

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<ROOT>` | Source directory to inspect |
