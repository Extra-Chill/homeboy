# `homeboy ssh`

## Synopsis

```sh
homeboy ssh <project_id> [command]
```

## Arguments

- `project_id`: project ID
- `command` (optional): if provided, executes a single command; otherwise opens an interactive SSH session.

## JSON output

```json
{
  "project_id": "<id>",
  "command": "<string>|null"
}
```

## Exit code

Exit code matches the underlying SSH session/command exit code.

## Related

- [server](server.md)
