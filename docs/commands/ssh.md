# `homeboy ssh`

## Synopsis

```sh
# Non-interactive discovery (JSON output):
homeboy ssh list

# Complete redacted records, including aliases and runner configuration:
homeboy ssh list --full

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
- `--cwd <REMOTE_PATH>`: start in this remote directory. It overrides a project's configured base path and applies to project and server targets.
- `[COMMAND...]` (optional): command to execute (omit for interactive shell).
  - Recommended form: `homeboy ssh <id> -- <command...>` (supports multiple args cleanly)
  - Put all Homeboy flags/options **before** `--` (everything after `--` is treated as part of the remote command)
  - If you need shell operators (`&&`, `|`, redirects), pass a single quoted string: `homeboy ssh <id> "cd /var/www && ls | head"`


## JSON output

### `ssh list`

> Note: output is wrapped in the global JSON envelope described in the [JSON output contract](../architecture/output-system.md). The object below is `data`.

```json
{
  "action": "List",
  "schema": "homeboy/ssh-list/v1",
  "operator_summary": {
    "identity": "ssh list",
    "state": "configured",
    "next_action": "homeboy ssh <server-id> -- <command>"
  },
  "servers": [
    {
      "id": "...",
      "host": "...",
      "user": "...",
      "port": 22,
      "kind": null,
      "runner_configured": false
    }
  ],
  "truncation": { "servers": { "shown": 1, "omitted": 0 } }
}
```

The default is a bounded operational projection. It limits target count and
field sizes, validates the final JSON response envelope, redacts sensitive
values, and includes `truncation` replay metadata. Use `homeboy ssh list --full`
for complete redacted server records; `--full` is intentionally unbounded.

### Connect (`homeboy ssh [OPTIONS] [ID] [-- <COMMAND...>]`)

The connect action uses an interactive SSH session and does not print the JSON envelope (it is treated as passthrough output).

When a command is provided, it is executed non-interactively and Homeboy captures stdout/stderr into the JSON response.

Project targets use their configured base path by default. Use `--cwd` to override that path or to select a working directory for a server target. This applies to interactive sessions, structured commands, and `--raw` commands:

```sh
homeboy ssh sandbox --cwd /home/wpcom/public_html -- php bin/example.php
homeboy ssh sandbox --cwd /home/wpcom/public_html
```

Structured command responses include `requested_cwd` and `effective_cwd`, so callers can distinguish an explicit override from the resolved project base path.

Piped stdin is streamed byte-for-byte to the remote command, including binary data. Homeboy closes remote stdin after local EOF and reports a nonzero result if the local stream cannot be delivered. This supports normal Unix composition without staging an intermediate file:

```sh
git format-patch -1 --stdout | homeboy ssh homeboy-lab -- git -C /srv/project am
printf 'payload' | homeboy ssh homeboy-lab -- sha256sum
```

Non-interactive command responses include `exit_code`, `success`, `result_classification`, and `failure_reason` when the command fails. This makes empty-output commands unambiguous: a command that exits `0` reports `success: true`, while a no-output failure reports the actual exit code and whether Homeboy classified it as a remote command failure or SSH transport failure.

`homeboy ssh` shows the server shell environment. Runner-specific job environment is injected by `homeboy runner exec`; inspect it with `homeboy runner env <runner-id>` or `homeboy runner exec <runner-id> -- printenv NAME`.

Note: the CLI still computes a JSON `data` object internally for this action, but it is not printed in interactive passthrough mode.

## Exit code

Exit code matches the underlying SSH session/command exit code.

## Related

- [server](server.md)
