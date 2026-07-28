<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy config` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/config.md](../../../commands/config.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy config`

```sh
homeboy config <COMMAND>
```

Manage global Homeboy configuration

| Subcommand | Summary |
| --- | --- |
| `homeboy config show` | Display configuration (merged defaults + file) |
| `homeboy config set` | Set a configuration value at a JSON pointer path |
| `homeboy config remove` | Remove a configuration value at a JSON pointer path |
| `homeboy config reset` | Reset configuration to built-in defaults (deletes homeboy.json) |
| `homeboy config path` | Show the path to homeboy.json |

## `homeboy config show`

```sh
homeboy config show [OPTIONS]
```

Display configuration (merged defaults + file)

| Option | Value | Description |
| --- | --- | --- |
| `--builtin` | flag | Show only built-in defaults (ignore homeboy.json) |

## `homeboy config set`

```sh
homeboy config set [OPTIONS] <POINTER> <VALUE>
```

Set a configuration value at a JSON pointer path

| Argument | Required | Description |
| --- | --- | --- |
| `<POINTER>` | yes | JSON pointer path (e.g., /defaults/deploy/scp_flags) |
| `<VALUE>` | yes | Value to set (JSON) |

| Option | Value | Description |
| --- | --- | --- |
| `--string` | flag | Treat value as a literal string instead of parsing it as JSON |

## `homeboy config remove`

```sh
homeboy config remove <POINTER>
```

Remove a configuration value at a JSON pointer path

| Argument | Required | Description |
| --- | --- | --- |
| `<POINTER>` | yes | JSON pointer path (e.g., /defaults/deploy/scp_flags) |

## `homeboy config reset`

```sh
homeboy config reset
```

Reset configuration to built-in defaults (deletes homeboy.json)

## `homeboy config path`

```sh
homeboy config path
```

Show the path to homeboy.json

