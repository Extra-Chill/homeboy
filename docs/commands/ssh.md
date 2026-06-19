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

- `[ID]`: project ID or server ID (project wins when both exist).
- `--as-server`: force interpretation as a server ID.
- `--user <USER>`: override the SSH user instead of the server's configured user.
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

Non-interactive command responses include `exit_code`, `success`, `result_classification`, and `failure_reason` when the command fails. This makes empty-output commands unambiguous: a command that exits `0` reports `success: true`, while a no-output failure reports the actual exit code and whether Homeboy classified it as a remote command failure or SSH transport failure.

`homeboy ssh` shows the server shell environment. Runner-specific job environment is injected by `homeboy runner exec`; inspect it with `homeboy runner env <runner-id>` or `homeboy runner exec <runner-id> -- printenv NAME`.

Note: the CLI still computes a JSON `data` object internally for this action, but it is not printed in interactive passthrough mode.

## Exit code

Exit code matches the underlying SSH session/command exit code.

## Related

- [server](server.md)
