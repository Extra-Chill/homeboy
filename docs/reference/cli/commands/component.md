<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy component` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/component.md](../../../commands/component.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy component`

```sh
homeboy component <COMMAND>
```

Manage standalone component configurations

| Subcommand | Summary |
| --- | --- |
| `homeboy component create` | Initialize portable component config for a repo |
| `homeboy component show` | Display component configuration |
| `homeboy component set` | Update component configuration fields |
| `homeboy component delete` | Delete a component configuration |
| `homeboy component rename` | Rename a component (changes ID directly) |
| `homeboy component list` | List all available components |
| `homeboy component projects` | List projects using this component |
| `homeboy component shared` | Show which components are shared across projects |
| `homeboy component env` | Detect runtime environment requirements from the component's source files |
| `homeboy component setup` | Prepare a component for build/test: install its declared extensions and install its dependencies through detected providers |
| `homeboy component reconcile` | Inspect and optionally repair stale standalone registry local_path data |
| `homeboy component artifacts` | Report or remove declared reconstructable artifacts for a component |

## `homeboy component create`

```sh
homeboy component create [OPTIONS]
```

Initialize portable component config for a repo

| Option | Value | Description |
| --- | --- | --- |
| `--local-path` | `<LOCAL_PATH>` | Absolute path to local source directory (writes homeboy.json there) |
| `--remote-path` | `<REMOTE_PATH>` | Remote path relative to project basePath |
| `--build-artifact` | `<BUILD_ARTIFACT>` | Build artifact path relative to localPath |
| `--version-target` | `<TARGET>` | Version targets in the form "file" or "file::pattern" (repeatable). For complex patterns, use --version-targets @file.json to avoid shell escaping |
| `--version-targets` | `<JSON>` | Version targets as JSON array (supports @file.json and - for stdin) |
| `--extract-command` | `<EXTRACT_COMMAND>` | Extract command to run after upload (e.g., "unzip -o {{artifact}} && rm {{artifact}}") |
| `--changelog-target` | `<CHANGELOG_TARGET>` | Path to changelog file relative to localPath |
| `--extension` | `<EXTENSION>` | Extension(s) this component uses (e.g., a runtime/framework extension id). Repeatable |
| `--project` | `<PROJECT>` | Attach component to a project after creation |

## `homeboy component show`

```sh
homeboy component show [OPTIONS] [ID]
```

Display component configuration

| Argument | Required | Description |
| --- | --- | --- |
| `[ID]` | no | Component ID (optional when --path is provided) |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Discover component from a directory's homeboy.json instead of the registry |

## `homeboy component set`

```sh
homeboy component set [OPTIONS] [ID]
```

Update component configuration fields

Supports dedicated flags for common fields (e.g., --local-path, --changelog-target) as well as --json/--base64 for arbitrary object updates.

| Argument | Required | Description |
| --- | --- | --- |
| `[ID]` | no | Entity ID (optional if provided in JSON body) |

| Option | Value | Description |
| --- | --- | --- |
| `--json` | `<JSON>` | JSON object to merge into the entity (supports @file and - for stdin) |
| `--base64` | `<BASE64>` | Base64-encoded JSON object (bypasses shell escaping issues) |
| `--replace` | `<FIELD>` | Replace these fields instead of merging arrays |
| `--local-path` | `<LOCAL_PATH>` | Absolute path to local source directory |
| `--remote-path` | `<REMOTE_PATH>` | Remote path relative to project basePath |
| `--build-artifact` | `<BUILD_ARTIFACT>` | Build artifact path relative to localPath |
| `--extract-command` | `<EXTRACT_COMMAND>` | Extract command to run after upload (e.g., "unzip -o {{artifact}} && rm {{artifact}}") |
| `--changelog-target` | `<CHANGELOG_TARGET>` | Path to changelog file relative to localPath |
| `--version-target` | `<TARGET>` | Version targets in the form "file" or "file::pattern" (repeatable). Same format as `component create --version-target` |
| `--extension` | `<EXTENSION>` | Extension(s) this component uses (e.g., a runtime/framework extension id). Repeatable |

## `homeboy component delete`

```sh
homeboy component delete <ID>
```

Delete a component configuration

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Component ID |

## `homeboy component rename`

```sh
homeboy component rename <ID> <NEW_ID>
```

Rename a component (changes ID directly)

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Current component ID |
| `<NEW_ID>` | yes | New component ID (should match repository directory name) |

## `homeboy component list`

```sh
homeboy component list
```

List all available components

## `homeboy component projects`

```sh
homeboy component projects <ID>
```

List projects using this component

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Component ID |

## `homeboy component shared`

```sh
homeboy component shared [ID]
```

Show which components are shared across projects

| Argument | Required | Description |
| --- | --- | --- |
| `[ID]` | no | Specific component ID to check (optional, shows all if omitted) |

## `homeboy component env`

```sh
homeboy component env [OPTIONS] [ID]
```

Detect runtime environment requirements from the component's source files.

Reads extension-specific metadata to determine what runtime versions the component needs. Outputs generic runtime requirement JSON suitable for CI environment setup.

| Argument | Required | Description |
| --- | --- | --- |
| `[ID]` | no | Component ID (optional when --path is provided) |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Discover component from a directory's homeboy.json |

## `homeboy component setup`

```sh
homeboy component setup [OPTIONS] [ID]
```

Prepare a component for build/test: install its declared extensions and install its dependencies through detected providers.

Core-owned replacement for hardcoded CI install/refresh + per-ecosystem dependency setup. The package manager is chosen by detection and manifest config, never by shell literals. CI calls this instead of orchestrating the sequence itself.

| Argument | Required | Description |
| --- | --- | --- |
| `[ID]` | no | Component ID (optional when --path is provided) |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Discover component from a directory's homeboy.json |
| `--source` | `<SOURCE>` | Source (git URL or local path) to install the component's configured extensions from. Omit to skip extension install (deps only) |
| `--skip-dependencies` | flag | Skip the dependency install step (extensions only) |

## `homeboy component reconcile`

```sh
homeboy component reconcile [OPTIONS] <ID>
```

Inspect and optionally repair stale standalone registry local_path data

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Component ID |

| Option | Value | Description |
| --- | --- | --- |
| `--apply` | flag | Apply a safe discovered repair instead of reporting only |

## `homeboy component artifacts`

```sh
homeboy component artifacts [OPTIONS] [ID]
```

Report or remove declared reconstructable artifacts for a component

| Argument | Required | Description |
| --- | --- | --- |
| `[ID]` | no | Component ID (optional when --path is provided or run from a component checkout) |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Discover component from a directory's homeboy.json instead of the registry |
| `--apply` | flag | Remove reported artifact paths instead of dry-run reporting only |

