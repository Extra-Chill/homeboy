<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy deps` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/deps.md](../../../commands/deps.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy deps`

```sh
homeboy deps <COMMAND>
```

Manage component dependencies

| Subcommand | Summary |
| --- | --- |
| `homeboy deps status` | Inspect dependency constraints and locked package versions |
| `homeboy deps install` | Install a component's dependencies through its detected providers |
| `homeboy deps update` | Update one package through its dependency provider |
| `homeboy deps stack` | Work with declared downstream dependency stacks |

## `homeboy deps status`

```sh
homeboy deps status [OPTIONS] [COMPONENT]
```

Inspect dependency constraints and locked package versions

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT]` | no | Component ID. When omitted, auto-detected from CWD |

| Option | Value | Description |
| --- | --- | --- |
| `--package` | `<PACKAGE>` | Limit output to one package |
| `--path` | `<PATH>` | Workspace path to operate on directly |

## `homeboy deps install`

```sh
homeboy deps install [OPTIONS] [COMPONENT]
```

Install a component's dependencies through its detected providers

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT]` | no | Component ID. When omitted, auto-detected from CWD |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Workspace path to operate on directly |

## `homeboy deps update`

```sh
homeboy deps update [OPTIONS] <PACKAGE> [COMPONENT]
```

Update one package through its dependency provider

| Argument | Required | Description |
| --- | --- | --- |
| `<PACKAGE>` | yes | Package name, e.g. example-org/block-format-bridge |
| `[COMPONENT]` | no | Component ID. When omitted, auto-detected from CWD |

| Option | Value | Description |
| --- | --- | --- |
| `--to` | `<CONSTRAINT>` | New manifest constraint, e.g. ^0.4 |
| `--path` | `<PATH>` | Workspace path to operate on directly |
| `--no-install` | flag | Skip provider-owned install/lockfile refresh after the manifest update |
| `--rebuild` | flag | Rebuild the component through its generic build capability after updating |

## `homeboy deps stack`

```sh
homeboy deps stack <COMMAND>
```

Work with declared downstream dependency stacks

| Subcommand | Summary |
| --- | --- |
| `homeboy deps stack status` | List declared dependency stack edges |
| `homeboy deps stack plan` | Plan downstream updates for an upstream component/repo |
| `homeboy deps stack apply` | Run downstream update commands for an upstream component/repo |

## `homeboy deps stack status`

```sh
homeboy deps stack status
```

List declared dependency stack edges

## `homeboy deps stack plan`

```sh
homeboy deps stack plan <UPSTREAM>
```

Plan downstream updates for an upstream component/repo

| Argument | Required | Description |
| --- | --- | --- |
| `<UPSTREAM>` | yes | Upstream component or repository identifier from dependency_stack[].upstream |

## `homeboy deps stack apply`

```sh
homeboy deps stack apply [OPTIONS] <UPSTREAM>
```

Run downstream update commands for an upstream component/repo

| Argument | Required | Description |
| --- | --- | --- |
| `<UPSTREAM>` | yes | Upstream component or repository identifier from dependency_stack[].upstream |

| Option | Value | Description |
| --- | --- | --- |
| `--to` | `<CONSTRAINT>` | New manifest constraint to pass to provider-backed default update steps |
| `--dry-run` | flag | Print the command plan without running commands |
| `--no-install` | flag | Skip provider-owned install/lockfile refresh after each manifest update |
| `--rebuild` | flag | Rebuild each downstream component through its generic build capability |
