# `homeboy server`

## Synopsis

```sh
homeboy server <COMMAND>
```

## Subcommands

### `create`

```sh
homeboy server create <name> --host <host> --user <user> [--port <port>]
```

### `show`

```sh
homeboy server show <server_id>
```

### `set`

```sh
homeboy server set <server_id> [--name <name>] [--host <host>] [--user <user>] [--port <port>]
```

### `delete`

```sh
homeboy server delete <server_id> --force
```

### `list`

```sh
homeboy server list
```

### `key`

```sh
homeboy server key <COMMAND>
```

Key subcommands:

- `generate <server_id>`
- `show <server_id>`
- `import <server_id> <private_key_path>`
- `use <server_id> <private_key_path>`
- `unset <server_id>`

## JSON output

`homeboy server` returns a single `ServerOutput` object. Fields are optional based on subcommand.

Top-level fields:

- `command`: action identifier (examples: `server.create`, `server.key.generate`)
- `serverId`: present for single-server actions
- `server`: server configuration (where applicable)
- `servers`: list for `list`
- `updated`: list of updated field names
- `deleted`: list of deleted IDs
- `key`: object for key actions

Key payload (`key`):

- `action`: `generate` | `show` | `import` | `use` | `unset`
- `serverId`
- `publicKey` (when available)
- `identityFile` (when set/known)
- `imported` (path used for import)

## Related

- [ssh](ssh.md)
