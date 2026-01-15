# `homeboy db`

## Synopsis

```sh
homeboy db <COMMAND>
```

## Subcommands

### `tables`

```sh
homeboy db tables <projectId> [<subtarget>] [<args...>]
```

### `describe`

```sh
homeboy db describe <projectId> [<subtarget>] <table>
```

Notes:

- Subtargets are only recognized if the project has `subTargets` configured.
- The first trailing arg is treated as `<subtarget>` if it matches by slug or name; otherwise it is treated as the `<table>`.

### `query`

```sh
homeboy db query <projectId> [<subtarget>] <sql...>
```

Note: `query` is intended for SELECT-only operations. Non-SELECT statements are rejected.

### `search`

```sh
homeboy db search <projectId> <table> --column <column> --pattern <pattern> [options]
```

Options:

- `--column <name>` (required): Column to search
- `--pattern <value>` (required): Search pattern (LIKE match by default)
- `--exact`: Use exact match (`=`) instead of LIKE
- `--limit <n>`: Maximum rows to return (default: 100)
- `--subtarget <name>`: Optional subtarget

Examples:

```sh
# Find users with email containing "gmail"
homeboy db search mysite wp_users --column user_email --pattern gmail

# Find posts with exact status
homeboy db search mysite wp_posts --column post_status --exact --pattern publish
```

### `delete-row`

```sh
homeboy db delete-row <projectId> [<subtarget>] <table> <rowId>
```

Notes:

- `<rowId>` must be numeric.

### `drop-table`

```sh
homeboy db drop-table <projectId> [<subtarget>] <table>
```

### `tunnel`

```sh
homeboy db tunnel <projectId> [--local-port <port>]
```

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). `homeboy db` returns a `DbOutput` object as the `data` payload. Fields vary by action.

Common fields:

- `command`: `db.tables` | `db.describe` | `db.query` | `db.search` | `db.delete_row` | `db.drop_table` | `db.tunnel`
- `project_id`
- `exit_code`, `success`
- `stdout`, `stderr` (for remote command execution)

Action-specific fields:

- `tables` (for `db.tables`)
- `table` (for `describe`, `delete_row`, `drop_table`)
- `sql` (for `query`, `delete_row`, `drop_table`)
- `tunnel` (for `tunnel`): `{ local_port, remote_host, remote_port, database, user }`

## Exit code

- For remote-command actions: exit code of the underlying remote database CLI command (as defined by the enabled module's `database.cli` templates).
- For `tunnel`: exit code of the local `ssh -L` process.

## Related

- [module](module.md)
