# `homeboy db`

## Synopsis

```sh
homeboy db <COMMAND>
```

## Subcommands

### `tables`

```sh
homeboy db tables <project_id> [<subtarget?> ...]
```

### `describe`

```sh
homeboy db describe <project_id> [<subtarget?> <table>]
```

### `query`

```sh
homeboy db query <project_id> [<subtarget?> <sql...>]
```

- Rejects SQL that begins with write-operation keywords (e.g. `INSERT`, `UPDATE`, `DELETE`, `DROP`).

### `delete-row`

```sh
homeboy db delete-row <project_id> <table> <row_id> --confirm
```

### `drop-table`

```sh
homeboy db drop-table <project_id> <table> --confirm
```

### `tunnel`

```sh
homeboy db tunnel <project_id> [--local-port <port>]
```

## JSON output

`homeboy db` returns a `DbOutput` object. Fields vary by action.

Common fields:

- `command`: `db.tables` | `db.describe` | `db.query` | `db.deleteRow` | `db.dropTable` | `db.tunnel`
- `projectId`
- `exitCode`, `success`
- `stdout`, `stderr` (for remote command execution)

Action-specific fields:

- `tables` (for `db.tables`)
- `table` (for `describe`, `deleteRow`, `dropTable`)
- `sql` (for `query`, `deleteRow`, `dropTable`)
- `tunnel` (for `tunnel`): `{ localPort, remoteHost, remotePort, database, user }`

## Exit code

- For remote-command actions: exit code of the remote WP-CLI invocation.
- For `tunnel`: exit code of the local `ssh -L` process.

## Related

- [wp](wp.md)
