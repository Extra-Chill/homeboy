# `homeboy auth`

## Synopsis

```sh
homeboy auth <COMMAND>
```

## Description

Manage project API secrets in the OS keychain.

Secrets are scoped by project ID and variable name. `source: "keychain"` API variables use service `homeboy` and account `<project-id>:<variable-name>`.

Generic HTTP auth profiles are separate reusable keychain entries for commands
that accept `--auth-profile <profile>`. They are managed under `homeboy auth
profile` and can store Basic auth credentials or Bearer tokens without embedding
secrets in shell history.

## Subcommands

### `login`

```sh
homeboy auth login --project <project_id> [--identifier <username_or_email>] [--password <password>]
```

If `--identifier` or `--password` are omitted, Homeboy prompts on stderr and reads from stdin.

`login` runs a configured login flow when the project defines one. For static API tokens, use `set`.

### `set`

```sh
homeboy auth set --project <project_id> <variable> [value]
```

Stores a variable value in the OS keychain. If `value` is omitted, Homeboy prompts and reads the value from stdin.

Example:

```sh
homeboy auth set --project wpcloud-api token
```

### `get`

```sh
homeboy auth get --project <project_id> <variable> [--redacted]
```

Reads a variable from the OS keychain. Use `--redacted` to confirm presence without printing the secret.

### `remove`

```sh
homeboy auth remove --project <project_id> <variable>
```

Deletes one variable from the OS keychain.

### `logout`

```sh
homeboy auth logout --project <project_id>
```

Deletes keychain-backed variables configured in the project's `api.auth.variables` map.

### `status`

```sh
homeboy auth status --project <project_id>
```

Reports whether configured auth variables are available without printing secret values.

### `profile`

```sh
homeboy auth profile set-basic <profile> [--username <username>] [--password <password>]
homeboy auth profile set-bearer <profile> [--token <token>]
homeboy auth profile status <profile>
homeboy auth profile remove <profile>
```

Manages reusable auth profiles for generic HTTP requests. Omit secret values to
prompt securely instead of passing them on the command line.

- `set-basic`: store a username/password profile.
- `set-bearer`: store a bearer token profile.
- `status`: report whether the profile exists without printing the secret.
- `remove`: delete the stored profile.

## Output

JSON output is wrapped in the global envelope.

`data` is one of:

- `{ "command": "login", "project_id": "...", "success": true }`
- `{ "command": "set", "project_id": "...", "variable": "token", "stored": true }`
- `{ "command": "get", "project_id": "...", "variable": "token", "value": "********", "redacted": true }`
- `{ "command": "remove", "project_id": "...", "variable": "token", "removed": true }`
- `{ "command": "logout", "project_id": "...", "removed": 1 }`
- `{ "command": "status", "project_id": "...", "authenticated": true, "variables": [...] }`
- `{ "profile": "...", "kind": "basic", "stored": true }`
- `{ "profile": "...", "available": true, "kind": "bearer" }`
- `{ "profile": "...", "removed": 3 }`

Note: project-scoped auth outputs include a `command` field. Auth profile outputs
use the profile result structs directly. Fields use snake_case (`project_id`).

## Related

- [api](api.md)
- [project](project.md)
- [JSON output contract](../architecture/output-system.md)
