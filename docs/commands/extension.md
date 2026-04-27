# `homeboy extension`

## Synopsis

```sh
homeboy extension <COMMAND>
```

## Subcommands

### `list`

```sh
homeboy extension list [-p|--project <project_id>]
```

### `show`

```sh
homeboy extension show <extension_id>
```

Print detailed manifest, runtime, compatibility, and readiness information for one installed extension.

### `run`

```sh
homeboy extension run <extension_id> [-p|--project <project_id>] [-c|--component <component_id>] [-i|--input <key=value>]... [--stream|--no-stream] [<args...>]
```

- `--project` is required when the extension needs project context.
- `--component` is required when component context is ambiguous.
- `--input` repeats; each value must be in `KEY=value` form.
- `--stream` forces streaming output directly to terminal.
- `--no-stream` disables streaming and captures output.
- By default, Homeboy auto-detects streaming behavior based on TTY.
- Trailing `<args...>` are passed to CLI-type extensions.

### `set`

```sh
homeboy extension set [extension_id] --json <JSON> [--replace <field>]...
homeboy extension set [extension_id] --json '<JSON>'
```

Updates a extension manifest by merging a JSON object into the extension config.

Options:

- `--json <JSON>`: JSON object to merge into config (supports `@file` and `-` for stdin)
- `--replace <field>`: replace array fields instead of union (repeatable)

Notes:

- Use `null` in JSON to clear a field (for example, `{"commands": null}`).

### `setup`

```sh
homeboy extension setup <extension_id>
```

### `install`

```sh
homeboy extension install <source> [--id <extension_id>]
```

Installs a extension into Homeboy's extensions directory.

- If `<source>` is a git URL, Homeboy clones it and writes `sourceUrl` into the installed extension's `<extension_id>.json` manifest.
- If `<source>` is a local path, Homeboy symlinks the directory into the extensions directory.

### `update`

```sh
homeboy extension update <extension_id>
```

Updates a git-cloned extension.

- If the extension is symlinked, Homeboy returns an error (linked extensions are updated at the source directory).
- Update runs without an extra confirmation flag.
- Homeboy reads `sourceUrl` from the extension's manifest to report the extension URL in JSON output.

### `uninstall`

```sh
homeboy extension uninstall <extension_id>
```

Uninstalls a extension.

- If the extension is **symlinked**, Homeboy removes the symlink (the source directory is preserved).
- If the extension is **git-cloned**, Homeboy deletes the extension directory.

### `action`

```sh
homeboy extension action <extension_id> <action_id> [-p|--project <project_id>] [--data <json>]
```

Executes an action defined in the extension manifest.

- For `type: "api"` actions, `--project` is required.
- `--data` accepts a JSON array string of selected result rows (passed through to template variables like `{{selected}}`).

### `exec`

```sh
homeboy extension exec <extension_id> [-c|--component <component_id>] -- <command...>
```

Runs a tool from the extension's vendor/runtime directory. When `--component` is provided, the command runs with that component's path as the working directory.

## Settings

Homeboy builds an **effective settings** map for each extension by merging settings across scopes, in order (later scopes override earlier ones):

1. Project (`projects/<project_id>.json`): `extensions.<extension_id>.settings`
2. Component (`components/<component_id>.json`): `extensions.<extension_id>.settings`

When running a extension, Homeboy passes an execution context via environment variables:

- `HOMEBOY_EXEC_CONTEXT_VERSION`: currently `2`
- `HOMEBOY_EXTENSION_ID`
- `HOMEBOY_SETTINGS_JSON`: merged effective settings (JSON)
- `HOMEBOY_PROJECT_ID` (optional; when a project context is used)
- `HOMEBOY_EXTENSION_PATH`: absolute path to extension directory
- `HOMEBOY_PROJECT_PATH` (optional; absolute path to project directory)
- `HOMEBOY_COMPONENT_ID` (optional; when a component context is resolved)
- `HOMEBOY_COMPONENT_PATH` (optional; absolute path to component directory)
- `HOMEBOY_STEP` / `HOMEBOY_SKIP` (optional; comma-separated step filters)

Extensions can define additional environment variables via `runtime.env` in their manifest.

`homeboy extension run` and `extension.run` pipeline steps share the same execution core (template vars, settings JSON, and env handling). Both paths keep the same CLI output contract while sharing internal execution behavior.

Extension settings validation happens during extension execution and may also be checked by other commands. Use the top-level `homeboy validate [component]` command for component parse/compile validation without running the full test suite.

`homeboy extension run` requires the extension to be installed/linked under the Homeboy extensions directory (discovered by scanning `<config dir>/homeboy/extensions/<extension_id>/<extension_id>.json`). There is no separate "installedModules in global config" requirement.

## Runtime Configuration

Executable extensions define their runtime behavior in their extension manifest (`extensions/<extension_id>/<extension_id>.json`):

```json
{
  "runtime": {
    "run_command": "./venv/bin/python3 {{entrypoint}} {{args}}",
    "setup_command": "python3 -m venv venv && ./venv/bin/pip install -r requirements.txt",
    "ready_check": "test -f ./venv/bin/python3",
    "entrypoint": "main.py",
    "env": {
      "MY_VAR": "{{extensionPath}}/data"
    }
  }
}
```

- `run_command`: Shell command to execute the extension. Template variables: `{{extensionPath}}`, `{{entrypoint}}`, `{{args}}`, plus project context vars.
- `setup_command`: Optional shell command to set up the extension (run during install/update).
- `ready_check`: Optional shell command to check if extension is ready (exit 0 = ready).
- `env`: Optional environment variables to set when running.

## Release Configuration

Release steps can be backed by extension actions named `release.<step_type>`.

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../architecture/output-system.md). `homeboy extension` returns a tagged `ExtensionOutput` object as `data`.

Top-level variants (`data.command`):

- `extension.list`: `{ project_id?, extensions: ExtensionEntry[] }`
- `extension.show`: `{ extension: ExtensionDetail }`
- `extension.run`: `{ extension_id, project_id? }`
- `extension.setup`: `{ extension_id }`
- `extension.install`: `{ extension_id, source, path, linked }`
- `extension.update`: `{ extension_id, url, path }`
- `extension.update_all`: `{ updated: UpdateEntry[], skipped: string[] }`
- `extension.uninstall`: `{ extension_id, path, was_linked }`
- `extension.action`: `{ extension_id, action_id, project_id?, response }`
- `extension.exec`: `{ extension_id, exit_code?, stdout?, stderr? }`
- `extension.set`: `{ extension_id, updated_fields }` or `{ batch }` for JSON batch updates

Extension entry (`extensions[]`):

- `id`, `name`, `version`, `description`
- `runtime`: `executable` (has runtime config) or `platform` (no runtime config)
- `compatible` (with optional `--project`)
- `ready` (runtime readiness based on `readyCheck`)
- `configured`: currently always `true` for discovered extensions (reserved for future richer config state)
- `linked`: whether the extension is symlinked
- `path`: extension directory path (may be empty if unknown)

## Exit code

- `extension.run`: exit code of the executed extension's `runCommand`.
- `extension.setup`: `0` on success; if no `setupCommand` defined, returns `0` without action.

## Extension-provided commands and docs

Extensions can provide their own top-level CLI commands and documentation topics.

Discover what’s available on your machine:

```sh
homeboy docs list
```

Render a extension-provided topic:

```sh
homeboy docs <topic>
```

Because extension commands and docs are installed locally, the core CLI documentation stays focused on the extension system rather than any specific extension-provided commands.

## Related

- [docs](docs.md)
- [project](project.md)
