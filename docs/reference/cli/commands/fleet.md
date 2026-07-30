<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy fleet` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/fleet.md](../../../commands/fleet.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy fleet`

```sh
homeboy fleet <COMMAND>
```

Manage fleets (groups of projects)

| Subcommand | Summary |
| --- | --- |
| `homeboy fleet create` | Create a new fleet |
| `homeboy fleet show` | Display fleet configuration |
| `homeboy fleet set` | Update fleet configuration |
| `homeboy fleet delete` | Delete a fleet |
| `homeboy fleet list` | List all fleets |
| `homeboy fleet add` | Add a project to a fleet |
| `homeboy fleet remove` | Remove a project from a fleet |
| `homeboy fleet projects` | Show projects in a fleet |
| `homeboy fleet components` | Show component usage across a fleet |
| `homeboy fleet status` | Show live component versions and server health across a fleet (via SSH) |
| `homeboy fleet check` | Check component drift across a fleet (compares local vs remote) |
| `homeboy fleet exec` | Run a command across all projects in a fleet via SSH |

## `homeboy fleet create`

```sh
homeboy fleet create [OPTIONS] <ID>
```

Create a new fleet

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Fleet ID |

| Option | Value | Description |
| --- | --- | --- |
| `-p`, `--projects` | `<PROJECTS>` | Project IDs to include (comma-separated or repeated) |
| `-d`, `--description` | `<DESCRIPTION>` | Description of the fleet |

## `homeboy fleet show`

```sh
homeboy fleet show <ID>
```

Display fleet configuration

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Fleet ID |

## `homeboy fleet set`

```sh
homeboy fleet set [OPTIONS] [ID]
```

Update fleet configuration

| Argument | Required | Description |
| --- | --- | --- |
| `[ID]` | no | Entity ID (optional if provided in JSON body) |

| Option | Value | Description |
| --- | --- | --- |
| `--json` | `<JSON>` | JSON object to merge into the entity (supports @file and - for stdin) |
| `--base64` | `<BASE64>` | Base64-encoded JSON object (bypasses shell escaping issues) |
| `--replace` | `<FIELD>` | Replace these fields instead of merging arrays |

## `homeboy fleet delete`

```sh
homeboy fleet delete <ID>
```

Delete a fleet

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Fleet ID |

## `homeboy fleet list`

```sh
homeboy fleet list
```

List all fleets

## `homeboy fleet add`

```sh
homeboy fleet add [OPTIONS] <ID>
```

Add a project to a fleet

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Fleet ID |

| Option | Value | Description |
| --- | --- | --- |
| `-p`, `--project` | `<PROJECT>` | Project ID to add |

## `homeboy fleet remove`

```sh
homeboy fleet remove [OPTIONS] <ID>
```

Remove a project from a fleet

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Fleet ID |

| Option | Value | Description |
| --- | --- | --- |
| `-p`, `--project` | `<PROJECT>` | Project ID to remove |

## `homeboy fleet projects`

```sh
homeboy fleet projects <ID>
```

Show projects in a fleet

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Fleet ID |

## `homeboy fleet components`

```sh
homeboy fleet components <ID>
```

Show component usage across a fleet

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Fleet ID |

## `homeboy fleet status`

```sh
homeboy fleet status [OPTIONS] <ID>
```

Show live component versions and server health across a fleet (via SSH)

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Fleet ID |

| Option | Value | Description |
| --- | --- | --- |
| `--cached` | flag | Use locally cached versions instead of live SSH check |
| `--health-only` | flag | Show only server health metrics, skip component versions |

## `homeboy fleet check`

```sh
homeboy fleet check [OPTIONS] <ID>
```

Check component drift across a fleet (compares local vs remote)

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Fleet ID |

| Option | Value | Description |
| --- | --- | --- |
| `--outdated` | flag | Only show components that need updates |

## `homeboy fleet exec`

```sh
homeboy fleet exec [OPTIONS] <ID> [COMMAND]...
```

Run a command across all projects in a fleet via SSH

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Fleet ID |
| `[COMMAND]...` | no | Command to execute on each project's server |

| Option | Value | Description |
| --- | --- | --- |
| `--check` | flag | Show what would execute without running anything |
| `--apply` | flag | Confirm the command should execute over SSH on every project in the fleet |
| `--user` | `<USER>` | Override the SSH user for this execution (instead of each server's configured user) |
