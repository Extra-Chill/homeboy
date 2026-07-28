<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy extension` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/extension.md](../../../commands/extension.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy extension`

```sh
homeboy extension <COMMAND>
```

Execute CLI-compatible extensions

| Subcommand | Summary |
| --- | --- |
| `homeboy extension list` | Show available extensions with compatibility status |
| `homeboy extension diff-installed` | Compare installed extension revisions with their current checkout HEADs |
| `homeboy extension show` | Show detailed information about a extension |
| `homeboy extension run` | Execute a extension |
| `homeboy extension setup` | Run the extension's setup command (if defined) |
| `homeboy extension install` | Install a extension from a git URL or local path |
| `homeboy extension refresh` | Refresh an extension: uninstall any existing install, then reinstall |
| `homeboy extension relink` | Relink an installed symlinked extension to a new local source path |
| `homeboy extension dev-run` | Sync local extension source to a runner, refresh it there, then run a command |
| `homeboy extension install-for-component` | Install every extension configured by a component |
| `homeboy extension update` | Update an installed extension (git pull) |
| `homeboy extension uninstall` | Uninstall a extension |
| `homeboy extension action` | Execute a extension action (API call or builtin) |
| `homeboy extension exec` | Run a tool from a extension's vendor directory |
| `homeboy extension set` | Update extension manifest fields |

## `homeboy extension list`

```sh
homeboy extension list [OPTIONS]
```

Show available extensions with compatibility status

| Option | Value | Description |
| --- | --- | --- |
| `-p`, `--project` | `<PROJECT>` | Project ID to filter compatible extensions |
| `--skip-ready-check` | flag | Report installed metadata only. Skips each extension's ready_check, so `ready_reason` is `ready_check_skipped` instead of a live answer |

## `homeboy extension diff-installed`

```sh
homeboy extension diff-installed [OPTIONS] [EXTENSION_ID]
```

Compare installed extension revisions with their current checkout HEADs

| Argument | Required | Description |
| --- | --- | --- |
| `[EXTENSION_ID]` | no | Optional extension ID to inspect |

| Option | Value | Description |
| --- | --- | --- |
| `--runner` | `<RUNNER>` | Inspect the extension install visible to a configured runner |

## `homeboy extension show`

```sh
homeboy extension show [OPTIONS] <EXTENSION_ID>
```

Show detailed information about a extension

| Argument | Required | Description |
| --- | --- | --- |
| `<EXTENSION_ID>` | yes | Extension ID |

| Option | Value | Description |
| --- | --- | --- |
| `--skip-ready-check` | flag | Report installed metadata only. Skips the extension's ready_check, so `ready_reason` is `ready_check_skipped` instead of a live answer |

## `homeboy extension run`

```sh
homeboy extension run [OPTIONS] <EXTENSION_ID> [ARGS]...
```

Execute a extension

| Argument | Required | Description |
| --- | --- | --- |
| `<EXTENSION_ID>` | yes | Extension ID |
| `[ARGS]...` | no | Arguments to pass to the extension (for CLI extensions) |

| Option | Value | Description |
| --- | --- | --- |
| `-p`, `--project` | `<PROJECT>` | Project ID (defaults to active project) |
| `-c`, `--component` | `<COMPONENT>` | Component ID (required when ambiguous) |
| `-i`, `--input` | `<INPUT>` | Input values as key=value pairs |
| `--step` | `<STEP>` | Run only specific steps (comma-separated, e.g. --step test,lint) |
| `--skip` | `<SKIP>` | Skip specific steps (comma-separated, e.g. --skip analyze,lint) |
| `--stream` | flag | Stream output directly to terminal (default: auto-detect based on TTY) |
| `--no-stream` | flag | Disable streaming and capture output (default: auto-detect based on TTY) |

## `homeboy extension setup`

```sh
homeboy extension setup <EXTENSION_ID>
```

Run the extension's setup command (if defined)

| Argument | Required | Description |
| --- | --- | --- |
| `<EXTENSION_ID>` | yes | Extension ID |

## `homeboy extension install`

```sh
homeboy extension install [OPTIONS] <SOURCE>
```

Install a extension from a git URL or local path

| Argument | Required | Description |
| --- | --- | --- |
| `<SOURCE>` | yes | Git URL or local path to extension directory |

| Option | Value | Description |
| --- | --- | --- |
| `--id` | `<ID>` | Override extension id |
| `--ref` | `<REVISION>` | Git ref to check out for URL installs (branch, tag, or commit) |
| `--replace` | flag | Replace an existing extension install/link |

## `homeboy extension refresh`

```sh
homeboy extension refresh [OPTIONS] <SOURCE>
```

Refresh an extension: uninstall any existing install, then reinstall

Idempotent core-owned replacement for CI's hardcoded uninstall/install sequence. Safe to re-run; a missing prior install is not an error.

| Argument | Required | Description |
| --- | --- | --- |
| `<SOURCE>` | yes | Git URL or local path to extension directory |

| Option | Value | Description |
| --- | --- | --- |
| `--id` | `<ID>` | Override extension id |
| `--ref` | `<REVISION>` | Git ref to check out for URL installs (branch, tag, or commit) |

## `homeboy extension relink`

```sh
homeboy extension relink <EXTENSION_ID> <SOURCE>
```

Relink an installed symlinked extension to a new local source path

| Argument | Required | Description |
| --- | --- | --- |
| `<EXTENSION_ID>` | yes | Extension ID |
| `<SOURCE>` | yes | Local path to extension directory |

## `homeboy extension dev-run`

```sh
homeboy extension dev-run [OPTIONS] <EXTENSION_ID> <COMMAND>...
```

Sync local extension source to a runner, refresh it there, then run a command

| Argument | Required | Description |
| --- | --- | --- |
| `<EXTENSION_ID>` | yes | Extension ID |
| `<COMMAND>...` | yes | Command and arguments to execute on the runner after refresh |

| Option | Value | Description |
| --- | --- | --- |
| `--source` | `<SOURCE>` | Local extension source directory to sync to the runner |
| `--runner` | `<RUNNER>` | Runner ID |

## `homeboy extension install-for-component`

```sh
homeboy extension install-for-component [OPTIONS]
```

Install every extension configured by a component

| Option | Value | Description |
| --- | --- | --- |
| `--source` | `<SOURCE>` | Git URL or local path to extension repository/directory |
| `--path` | `<PATH>` | Component path containing homeboy.json (defaults to current directory) |

## `homeboy extension update`

```sh
homeboy extension update [OPTIONS] [EXTENSION_ID]
```

Update an installed extension (git pull)

| Argument | Required | Description |
| --- | --- | --- |
| `[EXTENSION_ID]` | no | Extension ID (omit with --all to update everything) |

| Option | Value | Description |
| --- | --- | --- |
| `--all` | flag | Update all installed extensions |
| `--force` | flag | Force update even with uncommitted changes |

## `homeboy extension uninstall`

```sh
homeboy extension uninstall <EXTENSION_ID>
```

Uninstall a extension

| Argument | Required | Description |
| --- | --- | --- |
| `<EXTENSION_ID>` | yes | Extension ID |

## `homeboy extension action`

```sh
homeboy extension action [OPTIONS] <EXTENSION_ID> <ACTION_ID>
```

Execute a extension action (API call or builtin)

| Argument | Required | Description |
| --- | --- | --- |
| `<EXTENSION_ID>` | yes | Extension ID |
| `<ACTION_ID>` | yes | Action ID |

| Option | Value | Description |
| --- | --- | --- |
| `-p`, `--project` | `<PROJECT>` | Project ID (required for API actions) |
| `--data` | `<DATA>` | JSON array of selected data rows |

## `homeboy extension exec`

```sh
homeboy extension exec [OPTIONS] <EXTENSION_ID> <ARGS>...
```

Run a tool from a extension's vendor directory

| Argument | Required | Description |
| --- | --- | --- |
| `<EXTENSION_ID>` | yes | Extension ID |
| `<ARGS>...` | yes | Command and arguments to run |

| Option | Value | Description |
| --- | --- | --- |
| `-c`, `--component` | `<COMPONENT>` | Component ID (sets working directory to component path) |

## `homeboy extension set`

```sh
homeboy extension set [OPTIONS] [EXTENSION_ID]
```

Update extension manifest fields

| Argument | Required | Description |
| --- | --- | --- |
| `[EXTENSION_ID]` | no | Extension ID (optional if provided in JSON body) |

| Option | Value | Description |
| --- | --- | --- |
| `--json` | `<JSON>` | JSON object to merge into manifest (supports @file and - for stdin) |
| `--replace` | `<FIELD>` | Replace these fields instead of merging arrays |

