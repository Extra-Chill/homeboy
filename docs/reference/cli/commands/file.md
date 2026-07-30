<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy file` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/file.md](../../../commands/file.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy file`

```sh
homeboy file <COMMAND>
```

Remote file operations

| Subcommand | Summary |
| --- | --- |
| `homeboy file list` | List directory contents |
| `homeboy file read` | Read file content |
| `homeboy file write` | Write content to file (from stdin) |
| `homeboy file mkdir` | Create a directory |
| `homeboy file delete` | Delete a file or directory |
| `homeboy file rename` | Rename or move a file |
| `homeboy file find` | Find files by name pattern |
| `homeboy file grep` | Search file contents |
| `homeboy file download` | Download a file or directory from remote server |
| `homeboy file copy` | Copy a file or path between local and remote targets |
| `homeboy file sync` | Sync a directory between local and remote targets without deleting extras |
| `homeboy file edit` | Edit file with line-based or pattern-based operations |

## `homeboy file list`

```sh
homeboy file list <PROJECT_ID> <PATH>
```

List directory contents

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<PATH>` | yes | Remote directory path |

## `homeboy file read`

```sh
homeboy file read [OPTIONS] <PROJECT_ID> <PATH>
```

Read file content

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<PATH>` | yes | Remote file path |

| Option | Value | Description |
| --- | --- | --- |
| `--raw` | flag | Output raw content only (no JSON wrapper) |

## `homeboy file write`

```sh
homeboy file write [OPTIONS] <PROJECT_ID> <PATH>
```

Write content to file (from stdin)

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<PATH>` | yes | Remote file path |

| Option | Value | Description |
| --- | --- | --- |
| `--apply` | flag | Apply the destructive write. Without this flag, prints a plan only |

## `homeboy file mkdir`

```sh
homeboy file mkdir [OPTIONS] <PROJECT_ID> <PATH>
```

Create a directory

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<PATH>` | yes | Remote directory path |

| Option | Value | Description |
| --- | --- | --- |
| `--apply` | flag | Apply the directory creation. Without this flag, prints a plan only |

## `homeboy file delete`

```sh
homeboy file delete [OPTIONS] <PROJECT_ID> <PATH>
```

Delete a file or directory

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<PATH>` | yes | Remote path to delete |

| Option | Value | Description |
| --- | --- | --- |
| `-r`, `--recursive` | flag | Delete directories recursively |
| `--apply` | flag | Apply the destructive delete. Without this flag, prints a plan only |

## `homeboy file rename`

```sh
homeboy file rename [OPTIONS] <PROJECT_ID> <OLD_PATH> <NEW_PATH>
```

Rename or move a file

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<OLD_PATH>` | yes | Current path |
| `<NEW_PATH>` | yes | New path |

| Option | Value | Description |
| --- | --- | --- |
| `--apply` | flag | Apply the rename/move. Without this flag, prints a plan only |

## `homeboy file find`

```sh
homeboy file find [OPTIONS] <PROJECT_ID> <PATH>
```

Find files by name pattern

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<PATH>` | yes | Directory path to search |

| Option | Value | Description |
| --- | --- | --- |
| `--name` | `<NAME>` | Filename pattern (glob, e.g., "*.php") |
| `--file-type` | `<type>` | File type: f (file), d (directory), l (symlink) |
| `--max-depth` | `<MAX_DEPTH>` | Maximum directory depth |

## `homeboy file grep`

```sh
homeboy file grep [OPTIONS] <PROJECT_ID> <PATH> <PATTERN>
```

Search file contents

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<PATH>` | yes | Directory path to search |
| `<PATTERN>` | yes | Search pattern |

| Option | Value | Description |
| --- | --- | --- |
| `--name` | `<NAME>` | Filter files by name pattern (e.g., "*.php") |
| `--max-depth` | `<MAX_DEPTH>` | Maximum directory depth |
| `-i`, `--ignore-case` | flag | Case insensitive search |

## `homeboy file download`

```sh
homeboy file download [OPTIONS] <PROJECT_ID> <PATH> [LOCAL_PATH]
```

Download a file or directory from remote server

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<PATH>` | yes | Remote file path |
| `[LOCAL_PATH]` | no | Local destination path (defaults to current directory) |

| Option | Value | Description |
| --- | --- | --- |
| `-r`, `--recursive` | flag | Download directories recursively |

## `homeboy file copy`

```sh
homeboy file copy [OPTIONS] <SOURCE> <DESTINATION>
```

Copy a file or path between local and remote targets

| Argument | Required | Description |
| --- | --- | --- |
| `<SOURCE>` | yes | Source: local path or server_id:/path |
| `<DESTINATION>` | yes | Destination: local path or server_id:/path |

| Option | Value | Description |
| --- | --- | --- |
| `-r`, `--recursive` | flag | Copy directories recursively |
| `-c`, `--compress` | flag | Compress data during transfer |
| `--dry-run` | flag | Show what would be copied without doing it |
| `--exclude` | `<EXCLUDE>` | Exclude patterns for recursive server-to-server copies |

## `homeboy file sync`

```sh
homeboy file sync [OPTIONS] <SOURCE> <DESTINATION>
```

Sync a directory between local and remote targets without deleting extras

| Argument | Required | Description |
| --- | --- | --- |
| `<SOURCE>` | yes | Source: local path or server_id:/path |
| `<DESTINATION>` | yes | Destination: local path or server_id:/path |

| Option | Value | Description |
| --- | --- | --- |
| `-c`, `--compress` | flag | Compress data during transfer |
| `--dry-run` | flag | Show what would be copied without doing it |
| `--exclude` | `<EXCLUDE>` | Exclude patterns for recursive server-to-server copies |

## `homeboy file edit`

```sh
homeboy file edit [OPTIONS] <PROJECT_ID> <FILE_PATH>
```

Edit file with line-based or pattern-based operations

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<FILE_PATH>` | yes | Remote file path |

| Option | Value | Description |
| --- | --- | --- |
| `-n`, `--dry-run` | flag | Show changes without applying |
| `-f`, `--force` | flag | Apply even if multiple pattern matches (warns by default) |
| `--replace-line` | `<REPLACE_LINE>` | _no help text_ |
| `--replace-line-content` | `<CONTENT>` | _no help text_ |
| `--insert-after` | `<INSERT_AFTER>` | _no help text_ |
| `--insert-after-content` | `<CONTENT>` | _no help text_ |
| `--insert-before` | `<INSERT_BEFORE>` | _no help text_ |
| `--insert-before-content` | `<CONTENT>` | _no help text_ |
| `--delete-line` | `<DELETE_LINE>` | _no help text_ |
| `--delete-lines` | `<START> <END>` | _no help text_ |
| `--replace-pattern` | `<PATTERN>` | _no help text_ |
| `--replace-pattern-content` | `<CONTENT>` | _no help text_ |
| `--replace-all-pattern` | `<REPLACE_ALL_PATTERN>` | _no help text_ |
| `--replace-all-content` | `<CONTENT>` | _no help text_ |
| `--delete-pattern` | `<PATTERN>` | _no help text_ |
| `--append` | `<CONTENT>` | _no help text_ |
| `--prepend` | `<CONTENT>` | _no help text_ |
