# `homeboy pm2`

## Synopsis

```sh
homeboy pm2 <projectId> <args...>
```

## Arguments and flags

- `projectId`: project ID
- `<args...>`: PM2 command and arguments (trailing var args; hyphen values allowed)

## Execution

Commands execute locally if the project has no `serverId` configured, or via SSH if `serverId` is set.

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

```json
{
  "projectId": "<projectId>",
  "args": ["list"],
  "command": "<rendered command string>"
}
```

## Exit code

Exit code matches the executed PM2 command.

## Related

- [module](module.md)
