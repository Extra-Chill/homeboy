# `homeboy pm2`

## Synopsis

```sh
homeboy pm2 <projectId> [--local] <args...>
```

## Arguments and flags

- `projectId`: project ID
- `--local`: execute locally instead of on the remote server
- `<args...>`: PM2 command and arguments (trailing var args; hyphen values allowed)

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

```json
{
  "projectId": "<projectId>",
  "local": false,
  "args": ["list"],
  "command": "<rendered command string>"
}
```

## Exit code

Exit code matches the executed PM2 command.

## Related

- [module](module.md)
