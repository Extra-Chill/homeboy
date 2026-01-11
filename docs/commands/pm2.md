# `homeboy pm2`

## Synopsis

```sh
homeboy pm2 <project_id> [--local] <args...>
```

## Arguments and flags

- `project_id`: project ID
- `--local`: execute locally instead of on the remote server
- `<args...>`: PM2 command and arguments (trailing var args; hyphen values allowed)

## JSON output

```json
{
  "project_id": "<id>",
  "local": false,
  "args": ["list"],
  "command": "<rendered command string>"
}
```

## Exit code

Exit code matches the executed PM2 command.

## Related

- [wp](wp.md)
