<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy server` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/server.md](../../../commands/server.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy server`

```sh
homeboy server <COMMAND>
```

Manage SSH server configurations

| Subcommand | Summary |
| --- | --- |
| `homeboy server create` | Register a new SSH server |
| `homeboy server show` | Display server configuration |
| `homeboy server set` | Modify server settings |
| `homeboy server delete` | Remove a server configuration |
| `homeboy server list` | List all configured servers |
| `homeboy server connect` | Open a managed SSH control-master session for this server |
| `homeboy server status` | Check whether a managed SSH session is live |
| `homeboy server disconnect` | Close a managed SSH control-master session |
| `homeboy server key` | Manage SSH keys |

## `homeboy server create`

```sh
homeboy server create [OPTIONS] [ID]
```

Register a new SSH server

| Argument | Required | Description |
| --- | --- | --- |
| `[ID]` | no | Server ID (CLI mode) |

| Option | Value | Description |
| --- | --- | --- |
| `--json` | `<JSON>` | JSON input spec for create/update (supports single or bulk) |
| `--skip-existing` | flag | Skip items that already exist (JSON mode only) |
| `--host` | `<HOST>` | SSH host |
| `--user` | `<USER>` | SSH username |
| `--port` | `<PORT>` | SSH port (default: 22) |

## `homeboy server show`

```sh
homeboy server show <SERVER_ID>
```

Display server configuration

| Argument | Required | Description |
| --- | --- | --- |
| `<SERVER_ID>` | yes | Server ID |

## `homeboy server set`

```sh
homeboy server set [OPTIONS] [ID]
```

Modify server settings

| Argument | Required | Description |
| --- | --- | --- |
| `[ID]` | no | Entity ID (optional if provided in JSON body) |

| Option | Value | Description |
| --- | --- | --- |
| `--json` | `<JSON>` | JSON object to merge into the entity (supports @file and - for stdin) |
| `--base64` | `<BASE64>` | Base64-encoded JSON object (bypasses shell escaping issues) |
| `--replace` | `<FIELD>` | Replace these fields instead of merging arrays |

## `homeboy server delete`

```sh
homeboy server delete <SERVER_ID>
```

Remove a server configuration

| Argument | Required | Description |
| --- | --- | --- |
| `<SERVER_ID>` | yes | Server ID |

## `homeboy server list`

```sh
homeboy server list
```

List all configured servers

## `homeboy server connect`

```sh
homeboy server connect <SERVER_ID>
```

Open a managed SSH control-master session for this server

| Argument | Required | Description |
| --- | --- | --- |
| `<SERVER_ID>` | yes | Server ID |

## `homeboy server status`

```sh
homeboy server status <SERVER_ID>
```

Check whether a managed SSH session is live

| Argument | Required | Description |
| --- | --- | --- |
| `<SERVER_ID>` | yes | Server ID |

## `homeboy server disconnect`

```sh
homeboy server disconnect <SERVER_ID>
```

Close a managed SSH control-master session

| Argument | Required | Description |
| --- | --- | --- |
| `<SERVER_ID>` | yes | Server ID |

## `homeboy server key`

```sh
homeboy server key <COMMAND>
```

Manage SSH keys

| Subcommand | Summary |
| --- | --- |
| `homeboy server key generate` | Generate a new SSH key pair and set it for this server |
| `homeboy server key show` | Display the public SSH key |
| `homeboy server key import` | Import an existing SSH private key and set it for this server |
| `homeboy server key use` | Use an existing SSH private key file path for this server |
| `homeboy server key unset` | Unset the server SSH identity file (use normal SSH resolution) |

## `homeboy server key generate`

```sh
homeboy server key generate <SERVER_ID>
```

Generate a new SSH key pair and set it for this server

| Argument | Required | Description |
| --- | --- | --- |
| `<SERVER_ID>` | yes | Server ID |

## `homeboy server key show`

```sh
homeboy server key show <SERVER_ID>
```

Display the public SSH key

| Argument | Required | Description |
| --- | --- | --- |
| `<SERVER_ID>` | yes | Server ID |

## `homeboy server key import`

```sh
homeboy server key import <SERVER_ID> <PRIVATE_KEY_PATH>
```

Import an existing SSH private key and set it for this server

| Argument | Required | Description |
| --- | --- | --- |
| `<SERVER_ID>` | yes | Server ID |
| `<PRIVATE_KEY_PATH>` | yes | Path to private key file |

## `homeboy server key use`

```sh
homeboy server key use <SERVER_ID> <PRIVATE_KEY_PATH>
```

Use an existing SSH private key file path for this server

| Argument | Required | Description |
| --- | --- | --- |
| `<SERVER_ID>` | yes | Server ID |
| `<PRIVATE_KEY_PATH>` | yes | Path to private key file |

## `homeboy server key unset`

```sh
homeboy server key unset <SERVER_ID>
```

Unset the server SSH identity file (use normal SSH resolution)

| Argument | Required | Description |
| --- | --- | --- |
| `<SERVER_ID>` | yes | Server ID |
