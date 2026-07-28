<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy db` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/db.md](../../../commands/db.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy db`

```sh
homeboy db <COMMAND>
```

Database operations

| Subcommand | Summary |
| --- | --- |
| `homeboy db status` | Show local Homeboy observation-store status |
| `homeboy db tables` | List database tables |
| `homeboy db describe` | Show table structure |
| `homeboy db query` | Execute SELECT query |
| `homeboy db search` | Search table by column value |
| `homeboy db delete-row` | Delete a row from a table |
| `homeboy db drop-table` | Drop a database table |
| `homeboy db tunnel` | Open SSH tunnel to database |

## `homeboy db status`

```sh
homeboy db status
```

Show local Homeboy observation-store status

## `homeboy db tables`

```sh
homeboy db tables <PROJECT_ID> [ARGS]...
```

List database tables

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `[ARGS]...` | no | Optional subtarget |

## `homeboy db describe`

```sh
homeboy db describe <PROJECT_ID> [ARGS]...
```

Show table structure

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `[ARGS]...` | no | Optional subtarget and table name |

## `homeboy db query`

```sh
homeboy db query <PROJECT_ID> [ARGS]...
```

Execute SELECT query

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `[ARGS]...` | no | Optional subtarget and SQL query |

## `homeboy db search`

```sh
homeboy db search [OPTIONS] <PROJECT_ID> <TABLE>
```

Search table by column value

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<TABLE>` | yes | Table name |

| Option | Value | Description |
| --- | --- | --- |
| `--column` | `<COLUMN>` | Column to search |
| `--pattern` | `<PATTERN>` | Search pattern |
| `--exact` | flag | Use exact match instead of LIKE |
| `--limit` | `<LIMIT>` | Maximum rows to return |
| `--subtarget` | `<SUBTARGET>` | Optional subtarget |

## `homeboy db delete-row`

```sh
homeboy db delete-row [OPTIONS] <PROJECT_ID> [ARGS]...
```

Delete a row from a table

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `[ARGS]...` | no | Table name and row ID |

| Option | Value | Description |
| --- | --- | --- |
| `--apply` | flag | Apply the destructive mutation. Without this flag, prints a plan only |

## `homeboy db drop-table`

```sh
homeboy db drop-table [OPTIONS] <PROJECT_ID> [ARGS]...
```

Drop a database table

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `[ARGS]...` | no | Table name |

| Option | Value | Description |
| --- | --- | --- |
| `--apply` | flag | Apply the destructive mutation. Without this flag, prints a plan only |

## `homeboy db tunnel`

```sh
homeboy db tunnel [OPTIONS] <PROJECT_ID>
```

Open SSH tunnel to database

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |

| Option | Value | Description |
| --- | --- | --- |
| `--local-port` | `<LOCAL_PORT>` | Local port to bind |

