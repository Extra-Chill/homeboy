# `homeboy server`

## Synopsis

```sh
homeboy server <COMMAND>
```

## Subcommands

## Homeboy Lab Onboarding

For a normal Lab machine, use one ID for the machine and its runner capability:

```sh
homeboy server create homeboy-lab --host 192.168.86.63 --user chubes --port 22
homeboy runner enable homeboy-lab --workspace-root /home/chubes/Developer --concurrency-limit 4 --artifact-policy copy
homeboy runner connect homeboy-lab
```

This records `homeboy-lab` as the SSH server and enables runner settings on the same server record.

### `create`

```sh
homeboy server create [--json <spec>] [--skip-existing] <id> --host <host> --user <user> [--port <port>]

- `--port` defaults to `22`.
- When `--json` is provided, CLI mode arguments are not required.
- `id` is the server ID (not a display name); it should match what you’ll reference from projects.
```

`server_id` is the `<id>` you provide (CLI mode) or the `id` field in the JSON body (JSON mode).

### `show`

```sh
homeboy server show <server_id>
```

### `set`

```sh
homeboy server set <server_id> --json <JSON>
homeboy server set <server_id> --base64 <BASE64_JSON>
homeboy server set <server_id> --json '{"auth":{"mode":"key_plus_password_controlmaster"}}'
homeboy server set --json <JSON>   # server_id may be provided in JSON body
```

Updates a server by merging a JSON object into `servers/<id>.json`. Runner-capable servers store their runner settings under the nested `runner` object; `homeboy runner enable <server_id>` is the preferred CLI for adding that capability.

Arbitrary server updates must use `--json` or `--base64`; positional JSON, positional `key=value`, and dotted-path compatibility updates are not accepted.

Use `null` in JSON to clear a field (for example, `{"identity_file": null}`).

Options:

- `--json <JSON>`: JSON object to merge into config (supports `@file` and `-` for stdin)
- `--replace <field>`: replace array fields instead of union (repeatable)

### `delete`

```sh
homeboy server delete <server_id>
```

Deletion is safety-checked:

- If any project references this server ID, the command errors and asks you to update/delete those projects first.

### `list`

```sh
homeboy server list
```

### `connect`

```sh
homeboy server connect <server_id>
```

Opens an operator-authenticated SSH control-master session for servers configured with `auth.mode = "key_plus_password_controlmaster"`. Homeboy reuses the session for later server-backed commands without storing the password.

### `status`

```sh
homeboy server status <server_id>
```

Checks whether the configured control-master session is live.

### `disconnect`

```sh
homeboy server disconnect <server_id>
```

Closes the configured control-master session.

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

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../architecture/output-system.md). `homeboy server` returns a single `ServerOutput` object as the `data` payload. Fields are optional based on subcommand.

Top-level fields:

- `command`: action identifier (examples: `server.create`, `server.key.generate`)
- `server_id`: present for single-server actions
- `server`: server configuration (where applicable)
- `servers`: list for `list`
- `updated`: list of updated field names (values are command-specific)
- `deleted`: list of deleted IDs
- `key`: object for key actions
- `session`: object for managed SSH session actions

Key payload (`key`):

- `action`: `generate` | `show` | `import` | `use` | `unset`
- `server_id`
- `public_key` (when available)
- `identity_file` (when set/known)
- `imported` (original path used for import; `~` is expanded)

Session payload (`session`):

- `action`: `connect` | `status` | `disconnect`
- `server_id`
- `control_path`
- `persist`
- `live`
- `stdout`
- `stderr`

## Related

- [ssh](ssh.md)
