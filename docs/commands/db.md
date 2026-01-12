# `homeboy db`

## Synopsis

```sh
homeboy db <COMMAND>
```

## Subcommands

### `tables`

```sh
homeboy db tables <projectId> [<subtarget?> <args...>]
```

### `describe`

```sh
homeboy db describe <projectId> [<subtarget?> <table>]
```

### `query`

```sh
homeboy db query <projectId> [<subtarget?> <sql...>]
```

Note: `query` is intended for SELECT-only operations. Non-SELECT statements are rejected.

### `delete-row`

```sh
homeboy db delete-row <projectId> <table> <rowId> --confirm
```

Note: `--confirm` is required.

### `drop-table`

```sh
homeboy db drop-table <projectId> <table> --confirm
```

Note: `--confirm` is required.

### `tunnel`

```sh
homeboy db tunnel <projectId> [--local-port <port>]
```

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). `homeboy db` returns a `DbOutput` object as the `data` payload. Fields vary by action.

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
