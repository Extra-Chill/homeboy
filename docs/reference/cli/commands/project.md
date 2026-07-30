<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy project` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/project.md](../../../commands/project.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy project`

```sh
homeboy project <COMMAND>
```

Manage project configuration

| Subcommand | Summary |
| --- | --- |
| `homeboy project list` | List all configured projects |
| `homeboy project show` | Show project configuration |
| `homeboy project resolve-path` | Resolve a filesystem path to its configured Homeboy project |
| `homeboy project create` | Create a new project |
| `homeboy project set` | Update project configuration fields |
| `homeboy project remove` | Remove items from project configuration arrays |
| `homeboy project rename` | Rename a project (changes ID) |
| `homeboy project components` | Manage project components |
| `homeboy project pin` | Manage pinned files and logs |
| `homeboy project delete` | Delete a project configuration |
| `homeboy project init` | Initialize a project directory |
| `homeboy project status` | Show live server health and component versions for a project |

## `homeboy project list`

```sh
homeboy project list
```

List all configured projects

## `homeboy project show`

```sh
homeboy project show <PROJECT_ID>
```

Show project configuration

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |

## `homeboy project resolve-path`

```sh
homeboy project resolve-path <PATH>
```

Resolve a filesystem path to its configured Homeboy project

| Argument | Required | Description |
| --- | --- | --- |
| `<PATH>` | yes | Filesystem path inside a project base_path |

## `homeboy project create`

```sh
homeboy project create [OPTIONS] [ID] [DOMAIN]
```

Create a new project

| Argument | Required | Description |
| --- | --- | --- |
| `[ID]` | no | Project ID (CLI mode) |
| `[DOMAIN]` | no | Public site domain (CLI mode) |

| Option | Value | Description |
| --- | --- | --- |
| `--json` | `<JSON>` | JSON input spec for create/update (supports single or bulk) |
| `--skip-existing` | flag | Skip items that already exist (JSON mode only) |
| `--server-id` | `<SERVER_ID>` | Optional server ID |
| `--base-path` | `<BASE_PATH>` | Optional remote base path |
| `--table-prefix` | `<TABLE_PREFIX>` | Optional table prefix |

## `homeboy project set`

```sh
homeboy project set [OPTIONS] [ID]
```

Update project configuration fields

| Argument | Required | Description |
| --- | --- | --- |
| `[ID]` | no | Entity ID (optional if provided in JSON body) |

| Option | Value | Description |
| --- | --- | --- |
| `--json` | `<JSON>` | JSON object to merge into the entity (supports @file and - for stdin) |
| `--base64` | `<BASE64>` | Base64-encoded JSON object (bypasses shell escaping issues) |
| `--replace` | `<FIELD>` | Replace these fields instead of merging arrays |

## `homeboy project remove`

```sh
homeboy project remove [OPTIONS] [PROJECT_ID] [SPEC]
```

Remove items from project configuration arrays

| Argument | Required | Description |
| --- | --- | --- |
| `[PROJECT_ID]` | no | Project ID (optional if provided in JSON body) |
| `[SPEC]` | no | JSON spec (positional, supports @file and - for stdin) |

| Option | Value | Description |
| --- | --- | --- |
| `--json` | `<JSON>` | Explicit JSON spec (takes precedence over positional) |

## `homeboy project rename`

```sh
homeboy project rename <PROJECT_ID> <NEW_ID>
```

Rename a project (changes ID)

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Current project ID |
| `<NEW_ID>` | yes | New project ID |

## `homeboy project components`

```sh
homeboy project components <COMMAND>
```

Manage project components

| Subcommand | Summary |
| --- | --- |
| `homeboy project components list` | List associated components |
| `homeboy project components set` | Replace project components with the provided list |
| `homeboy project components attach-path` | Rebase matching project components discovered below a monorepo checkout |
| `homeboy project components attach-paths` | Attach multiple repo paths, retaining per-path diagnostics |
| `homeboy project components remove` | Remove one or more components |
| `homeboy project components clear` | Remove all components |

## `homeboy project components list`

```sh
homeboy project components list <PROJECT_ID>
```

List associated components

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |

## `homeboy project components set`

```sh
homeboy project components set [OPTIONS] <PROJECT_ID>
```

Replace project components with the provided list

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |

| Option | Value | Description |
| --- | --- | --- |
| `--json` | `<JSON>` | JSON array of attachments: [{"id":"foo","local_path":"/repo","remote_path":"wp-content/plugins/foo"}] |

## `homeboy project components attach-path`

```sh
homeboy project components attach-path [OPTIONS] <PROJECT_ID> <LOCAL_PATH>
```

Rebase matching project components discovered below a monorepo checkout

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<LOCAL_PATH>` | yes | Local repo path containing homeboy.json |

| Option | Value | Description |
| --- | --- | --- |
| `--dry-run` | flag | Preview every nested component path rebase without updating project config |

## `homeboy project components attach-paths`

```sh
homeboy project components attach-paths [OPTIONS] <PROJECT_ID> [LOCAL_PATHS]...
```

Attach multiple repo paths, retaining per-path diagnostics

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `[LOCAL_PATHS]...` | no | Local repo paths containing homeboy.json |

| Option | Value | Description |
| --- | --- | --- |
| `--input` | `<INPUT>` | JSON array of {"path":"/repo","reference":"caller-owned-ref"} inputs |
| `--failure-policy` | `<FAILURE_POLICY>` | Continue all inputs or stop after the first failure Values: `continue`, `fail-fast`. |
| `--worktree-policy` | `<WORKTREE_POLICY>` | Include git worktrees or report them as skipped Values: `include`, `skip`. |

## `homeboy project components remove`

```sh
homeboy project components remove <PROJECT_ID> [COMPONENT_IDS]...
```

Remove one or more components

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `[COMPONENT_IDS]...` | no | Component IDs |

## `homeboy project components clear`

```sh
homeboy project components clear <PROJECT_ID>
```

Remove all components

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |

## `homeboy project pin`

```sh
homeboy project pin <COMMAND>
```

Manage pinned files and logs

| Subcommand | Summary |
| --- | --- |
| `homeboy project pin list` | List pinned items |
| `homeboy project pin add` | Pin a file or log |
| `homeboy project pin remove` | Unpin a file or log |
| `homeboy project pin update` | Update an existing pinned file or log |
| `homeboy project pin rename` | Rename the path for an existing pinned file or log |

## `homeboy project pin list`

```sh
homeboy project pin list [OPTIONS] <PROJECT_ID>
```

List pinned items

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |

| Option | Value | Description |
| --- | --- | --- |
| `--type` | `<TYPE>` | Item type: file or log Values: `file`, `log`. |

## `homeboy project pin add`

```sh
homeboy project pin add [OPTIONS] <PROJECT_ID> <PATH>
```

Pin a file or log

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<PATH>` | yes | Path to pin (relative to basePath or absolute) |

| Option | Value | Description |
| --- | --- | --- |
| `--type` | `<TYPE>` | Item type: file or log Values: `file`, `log`. |
| `--label` | `<LABEL>` | Optional display label |
| `--tail` | `<TAIL>` | Number of lines to tail (logs only) |

## `homeboy project pin remove`

```sh
homeboy project pin remove [OPTIONS] <PROJECT_ID> <PATH>
```

Unpin a file or log

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<PATH>` | yes | Path to unpin |

| Option | Value | Description |
| --- | --- | --- |
| `--type` | `<TYPE>` | Item type: file or log Values: `file`, `log`. |

## `homeboy project pin update`

```sh
homeboy project pin update [OPTIONS] <PROJECT_ID> <PATH>
```

Update an existing pinned file or log

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<PATH>` | yes | Path to update |

| Option | Value | Description |
| --- | --- | --- |
| `--type` | `<TYPE>` | Item type: file or log Values: `file`, `log`. |
| `--label` | `<LABEL>` | Optional display label |
| `--tail` | `<TAIL>` | Number of lines to tail (logs only) |

## `homeboy project pin rename`

```sh
homeboy project pin rename [OPTIONS] <PROJECT_ID> <OLD_PATH> <NEW_PATH>
```

Rename the path for an existing pinned file or log

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<OLD_PATH>` | yes | Current pinned path |
| `<NEW_PATH>` | yes | New pinned path |

| Option | Value | Description |
| --- | --- | --- |
| `--type` | `<TYPE>` | Item type: file or log Values: `file`, `log`. |

## `homeboy project delete`

```sh
homeboy project delete <PROJECT_ID>
```

Delete a project configuration

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |

## `homeboy project init`

```sh
homeboy project init <PROJECT_ID>
```

Initialize a project directory

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |

## `homeboy project status`

```sh
homeboy project status [OPTIONS] <PROJECT_ID>
```

Show live server health and component versions for a project

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |

| Option | Value | Description |
| --- | --- | --- |
| `--health-only` | flag | Show only server health metrics, skip component versions |
