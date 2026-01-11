# `homeboy ssh`

## Synopsis

```sh
# non-interactive discovery:
homeboy ssh list

# connect:
homeboy ssh [id] [command]
homeboy ssh --project <projectId> [command]
homeboy ssh --server <serverId> [command]
```

## Subcommands

### `list`

Lists configured SSH server targets. This is safe for CI/headless usage.

```sh
homeboy ssh list
```

## Arguments and flags

- `id`: a project ID or server ID (the CLI resolves which one you mean)
- `--project <projectId>`: force project resolution
- `--server <serverId>`: force server resolution
- `command` (optional): single token; executes that command, otherwise interactive session.

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

```json
{
  "resolvedType": "project|server",
  "projectId": "<projectId>|null",
  "serverId": "<serverId>",
  "command": "<string>|null"
}
```

## Exit code

Exit code matches the underlying SSH session/command exit code.

## Related

- [server](server.md)
