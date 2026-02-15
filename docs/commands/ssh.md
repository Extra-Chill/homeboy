# `homeboy ssh`

## Synopsis

```sh
# Non-interactive discovery (JSON output):
homeboy ssh list

# Connect (interactive when no COMMAND is provided):
homeboy ssh [OPTIONS] [ID] [-- <COMMAND...>]
```

## Subcommands

### `list`

Lists configured SSH server targets. This is safe for CI/headless usage.

```sh
homeboy ssh list
```

## Arguments and flags

- `[ID]`: project ID or server ID (project wins when both exist). Optional when using `--project` or `--server`.
- `--project <PROJECT>`: force project resolution
- `--server <SERVER>`: force server resolution
- `[COMMAND...]` (optional): command to execute (omit for interactive shell).
  - Recommended form: `homeboy ssh <id> -- <command...>` (supports multiple args cleanly)
  - Put all Homeboy flags/options **before** `--` (everything after `--` is treated as part of the remote command)
  - If you need shell operators (`&&`, `|`, redirects), pass a single quoted string: `homeboy ssh <id> "cd /var/www && ls | head"`


## JSON output

### `ssh list`

> Note: output is wrapped in the global JSON envelope described in the [JSON output contract](../architecture/output-system.md). The object below is `data`.

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
      "identity_file": null
    }
  ]
}
```

Note: `action` is produced by the tagged enum output (`SshOutput`).

### Connect (`homeboy ssh [OPTIONS] [ID] [-- <COMMAND...>]`)

The connect action uses an interactive SSH session and does not print the JSON envelope (it is treated as passthrough output).

When a command is provided, it is executed non-interactively and Homeboy captures stdout/stderr into the JSON response.

Note: the CLI still computes a JSON `data` object internally for this action, but it is not printed in interactive passthrough mode.

## Exit code

Exit code matches the underlying SSH session/command exit code.

## Related

- [server](server.md)
