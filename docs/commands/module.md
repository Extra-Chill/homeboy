# `homeboy module`

## Synopsis

```sh
homeboy module <COMMAND>
```

## Subcommands

### `list`

```sh
homeboy module list [-p|--project <projectId>]
```

### `run`

```sh
homeboy module run <moduleId> [-p|--project <projectId>] [-c|--component <componentId>] [-i|--input <key=value>]... [<args...>]
```

- `--project` is required when the module needs project context.
- `--component` is required when component context is ambiguous.
- `--input` repeats; each value must be in `KEY=value` form.
- Trailing `<args...>` are passed to CLI-type modules.

### `setup`

```sh
homeboy module setup <moduleId>
```

### `install`

```sh
homeboy module install <url> [--id <moduleId>]
```

Installs a module by cloning it into Homeboy's modules directory (under your OS config directory) and writing `.install.json` so it can be updated later.

### `update`

```sh
homeboy module update <moduleId> [--force]
```

Updates a module by running `git pull --ff-only` in the module directory. If the module has uncommitted changes, `--force` is required.

### `uninstall`

```sh
homeboy module uninstall <moduleId> [--force]
```

Uninstalls a module by deleting its directory. If `--force` is not provided, Homeboy errors (there is no interactive prompt).

### `link`

```sh
homeboy module link <path> [--id <moduleId>]
```

Symlinks a local module directory into the Homeboy modules directory for development.

### `unlink`

```sh
homeboy module unlink <moduleId>
```

Removes a symlinked module entry from the Homeboy modules directory (preserves the source directory).

### `action`

```sh
homeboy module action <moduleId> <actionId> [-p|--project <projectId>] [--data <json>]
```

Executes an action defined in the module manifest.

- For `type: "api"` actions, `--project` is required.
- `--data` accepts a JSON array string of selected result rows (passed through to template variables like `{{selected}}`).

## Settings

Homeboy builds an **effective settings** map for each module by merging settings across scopes, in order (later scopes override earlier ones):

1. App (`homeboy.json`): `installedModules.<moduleId>.settings`
2. Project (`projects/<projectId>.json`): `modules.<moduleId>.settings`
3. Component (`components/<componentId>.json`): `modules.<moduleId>.settings`

When running a module, Homeboy passes an execution context via environment variables:

- `HOMEBOY_EXEC_CONTEXT_VERSION`: currently `1`
- `HOMEBOY_MODULE_ID`
- `HOMEBOY_SETTINGS_JSON`: merged effective settings (JSON)
- `HOMEBOY_PROJECT_ID` (optional; when a project context is used)
- `HOMEBOY_COMPONENT_ID` (optional; when a component context is resolved)

Modules can define additional environment variables via `runtime.env` in their manifest.

Module settings validation currently happens during module execution (and may also be checked by other commands). There is no dedicated validation-only command in the CLI.

`homeboy module run` requires the module to be present in app config (`homeboy.json`) under `installedModules` (i.e., installed/linked and recorded).

## Runtime Configuration

Executable modules define their runtime behavior in their module manifest (`modules/<moduleId>/homeboy.json`):

```json
{
  "runtime": {
    "runCommand": "./venv/bin/python3 {{entrypoint}} {{args}}",
    "setupCommand": "python3 -m venv venv && ./venv/bin/pip install -r requirements.txt",
    "readyCheck": "test -f ./venv/bin/python3",
    "entrypoint": "main.py",
    "env": {
      "MY_VAR": "{{modulePath}}/data"
    }
  }
}
```

- `runCommand`: Shell command to execute the module. Template variables: `{{modulePath}}`, `{{entrypoint}}`, `{{args}}`, plus project context vars.
- `setupCommand`: Optional shell command to set up the module (run during install/update).
- `readyCheck`: Optional shell command to check if module is ready (exit 0 = ready).
- `env`: Optional environment variables to set when running.

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). `homeboy module` returns a `ModuleOutput` object as the `data` payload.

`ModuleOutput`:

- `command`: `module.list` | `module.run` | `module.setup` | `module.install` | `module.update` | `module.uninstall`
- `projectId` (only used for `module.list` filter)
- `moduleId`
- `modules`: list output for `module.list`
- `installed`: `{ url, path }` for `module.install`
- `updated`: `{ url, path }` for `module.update`
- `uninstalled`: `{ path }` for `module.uninstall`

Module entry (`modules[]`):

- `id`, `name`, `version`, `description`
- `runtime`: `executable` (has runtime config) or `platform` (no runtime config)
- `compatible` (with optional `--project`)
- `ready` (runtime readiness based on `readyCheck`)
- `configured` (whether the module is present in `homeboy.json` under `installedModules`)

## Exit code

- `module.run`: exit code of the executed module's `runCommand`.
- `module.setup`: `0` on success; if no `setupCommand` defined, returns `0` without action.

## Module-provided commands and docs

Modules can provide their own top-level CLI commands and documentation topics.

Discover what’s available on your machine:

```sh
homeboy docs --list
```

Render a module-provided topic:

```sh
homeboy docs <topic>
```

Because module commands and docs are installed locally, the core CLI documentation stays focused on the module system rather than any specific module-provided commands.

## Related

- [docs](docs.md)
- [project](project.md)
