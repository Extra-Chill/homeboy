<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy ssh` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/ssh.md](../../../commands/ssh.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy ssh`

```sh
homeboy ssh [OPTIONS] [TARGET] [COMMAND]... [COMMAND]
```

SSH into a project server or configured server

| Argument | Required | Description |
| --- | --- | --- |
| `[TARGET]` | no | Target ID (project or server; project wins when ambiguous) |
| `[COMMAND]...` | no | Command to execute (omit for interactive shell) |

| Option | Value | Description |
| --- | --- | --- |
| `--as-server` | flag | Force interpretation as server ID |
| `--user` | `<USER>` | Override the SSH user (instead of the server's configured user) |
| `--raw` | flag | Write only the remote command's stdout to local stdout (and its stderr to local stderr), exiting with the remote exit code. Ideal for piping a remote export straight into a file. Combine with `--output <path>` to also persist the structured envelope. Requires a non-interactive command |
| `--timeout` | `<TIMEOUT>` | Bound the complete non-interactive SSH command, in seconds. Progress remains on stderr so `--raw` preserves remote stdout |

| Subcommand | Summary |
| --- | --- |
| `homeboy ssh list` | List configured SSH server targets |

## `homeboy ssh list`

```sh
homeboy ssh list
```

List configured SSH server targets
