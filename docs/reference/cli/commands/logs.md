<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy logs` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/logs.md](../../../commands/logs.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy logs`

```sh
homeboy logs <COMMAND>
```

Remote log viewing

| Subcommand | Summary |
| --- | --- |
| `homeboy logs list` | List pinned log files |
| `homeboy logs show` | Show log file content (shows all pinned logs if path omitted) |
| `homeboy logs clear` | Clear log file contents |
| `homeboy logs search` | Search log file for pattern |

## `homeboy logs list`

```sh
homeboy logs list <PROJECT_ID>
```

List pinned log files

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |

## `homeboy logs show`

```sh
homeboy logs show [OPTIONS] <PROJECT_ID> [PATH]
```

Show log file content (shows all pinned logs if path omitted)

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `[PATH]` | no | Log file path (optional - shows all pinned logs if omitted) |

| Option | Value | Description |
| --- | --- | --- |
| `-n`, `--lines` | `<LINES>` | Number of lines to show |
| `-f`, `--follow` | flag | Follow log output (like tail -f) |
| `--local` | flag | Execute locally instead of via SSH (for when running on the target server) |

## `homeboy logs clear`

```sh
homeboy logs clear [OPTIONS] <PROJECT_ID> <PATH>
```

Clear log file contents

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<PATH>` | yes | Log file path |

| Option | Value | Description |
| --- | --- | --- |
| `--local` | flag | Execute locally instead of via SSH |

## `homeboy logs search`

```sh
homeboy logs search [OPTIONS] <PROJECT_ID> <PATH> <PATTERN>
```

Search log file for pattern

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<PATH>` | yes | Log file path |
| `<PATTERN>` | yes | Search pattern |

| Option | Value | Description |
| --- | --- | --- |
| `-i`, `--ignore-case` | flag | Case insensitive search |
| `-n`, `--lines` | `<LINES>` | Limit to last N lines before searching |
| `-C`, `--context` | `<CONTEXT>` | Lines of context around matches |
| `--local` | flag | Execute locally instead of via SSH |

