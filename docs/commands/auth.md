# `homeboy auth`

## Synopsis

```sh
homeboy auth <COMMAND>
```

## Description

Authenticate with a project’s API and store credentials in the OS keychain.

Authentication is scoped per project ID.

## Subcommands

### `login`

```sh
homeboy auth login --project <projectId> [--identifier <usernameOrEmail>] [--password <password>]
```

If `--identifier` or `--password` are omitted, Homeboy prompts on stderr and reads from stdin.

### `logout`

```sh
homeboy auth logout --project <projectId>
```

### `status`

```sh
homeboy auth status --project <projectId>
```

## Output

JSON output is wrapped in the global envelope.

`data` is one of:

- `{ "command": "login", "project_id": "...", "success": true }`
- `{ "command": "logout", "project_id": "..." }`
- `{ "command": "status", "project_id": "...", "authenticated": true }`

Note: `command` is a tagged enum value (`login|logout|status`), and fields use snake_case (`project_id`).

## Related

- [api](api.md)
- [project](project.md)
- [JSON output contract](../json-output/json-output-contract.md)
