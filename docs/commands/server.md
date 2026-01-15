# `homeboy server`

## Synopsis

```sh
homeboy server <COMMAND>
```

## Subcommands

### `create`

```sh
homeboy server create [--json <spec>] [--skip-existing] <id> --host <host> --user <user> [--port <port>]

- `--port` defaults to `22`.
- When `--json` is provided, CLI mode arguments are not required.
- `id` is the server ID (not a display name); it should match what you’ll reference from projects.
```

`serverId` is the `<id>` you provide (CLI mode) or the `id` field in the JSON body (JSON mode).

### `show`

```sh
homeboy server show <serverId>
```

### `set`

```sh
homeboy server set <serverId> --json <JSON>
homeboy server set <serverId> '<JSON>'
homeboy server set --json <JSON>   # serverId may be provided in JSON body
```

Updates a server by merging a JSON object into `servers/<id>.json`.

Options:

- `--json <JSON>`: JSON object to merge into config (supports `@file` and `-` for stdin)

### `delete`

```sh
homeboy server delete <serverId>
```

Deletion is safety-checked:

- If any project references this server ID, the command errors and asks you to update/delete those projects first.

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
- `server_id`: present for single-server actions
- `server`: server configuration (where applicable)
- `servers`: list for `list`
- `updated`: list of updated field names (values are command-specific)
- `deleted`: list of deleted IDs
- `key`: object for key actions

Key payload (`key`):

- `action`: `generate` | `show` | `import` | `use` | `unset`
- `server_id`
- `public_key` (when available)
- `identity_file` (when set/known)
- `imported` (original path used for import; `~` is expanded)

## Related

- [ssh](ssh.md)
