# `homeboy ssh`

## Synopsis

```sh
# non-interactive discovery (JSON output):
homeboy ssh list

# connect (interactive when <command> is omitted):
homeboy ssh <id> [command]
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

- `<id>`: a project ID or server ID (the CLI resolves which one you mean). Required unless `--project` or `--server` is used.
- `--project <projectId>`: force project resolution
- `--server <serverId>`: force server resolution
- `[command]` (optional): single command string token to execute. Omit for an interactive session.

Note: `command` is a single positional argument (not `command...`). If you need to run complex commands, wrap them in a shell (for example: `homeboy ssh <id> "sh"`).

## JSON output

### `ssh list`

> Note: output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is `data.payload`.

```json
{
  "action": "list",
  "servers": [
    {
      "id": "...",
      "name": "...",
      "host": "...",
      "user": "...",
      "port": 22,
      "identityFile": null
    }
  ]
}
```

### Connect (`homeboy ssh <id> [command]`)

The connect action uses an interactive SSH session and does not print the JSON envelope (it is treated as passthrough output).

When `command` is provided, it is passed to the remote shell via the interactive session.

## Exit code

Exit code matches the underlying SSH session/command exit code.

## Related

- [server](server.md)
