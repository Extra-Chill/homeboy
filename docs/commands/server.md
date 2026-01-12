# `homeboy server`

## Synopsis

```sh
homeboy server <COMMAND>
```

## Subcommands

### `create`

```sh
homeboy server create <name> --host <host> --user <user> [--port <port>]

- `--port` defaults to `22`.
```

`serverId` is derived from `slugify_id(<name>)`.

### `show`

```sh
homeboy server show <serverId>
```

### `set`

```sh
homeboy server set <serverId> [--name <name>] [--host <host>] [--user <user>] [--port <port>]
```

### `delete`

```sh
homeboy server delete <serverId> --force
```

Note: `--force` is required (no interactive prompt).

### `list`

```sh
homeboy server list
```

### `key`

```sh
homeboy server key <COMMAND>
```

Key subcommands:

- `generate <serverId>`
- `show <serverId>`
- `import <serverId> <private_key_path>`
- `use <serverId> <private_key_path>`
- `unset <serverId>`

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). `homeboy server` returns a single `ServerOutput` object as the `data` payload. Fields are optional based on subcommand.

Top-level fields:

- `command`: action identifier (examples: `server.create`, `server.key.generate`)
- `serverId`: present for single-server actions
- `server`: server configuration (where applicable)
- `servers`: list for `list`
- `updated`: list of updated field names (values are command-specific)
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
