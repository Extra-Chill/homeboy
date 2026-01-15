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
homeboy module install <source> [--id <moduleId>]
```

Installs a module into Homeboy's modules directory.

- If `<source>` is a git URL, Homeboy clones it and writes `sourceUrl` into the installed module's `<moduleId>.json` manifest.
- If `<source>` is a local path, Homeboy symlinks the directory into the modules directory.

### `update`

```sh
homeboy module update <moduleId>
```

Updates a git-cloned module.

- If the module is symlinked, Homeboy returns an error (linked modules are updated at the source directory).
- Update runs without an extra confirmation flag.
- Homeboy reads `sourceUrl` from the module's manifest to report the module URL in JSON output.

### `uninstall`

```sh
homeboy module uninstall <moduleId>
```

Uninstalls a module.

- If the module is **symlinked**, Homeboy removes the symlink (the source directory is preserved).
- If the module is **git-cloned**, Homeboy deletes the module directory.

### `action`

```sh
homeboy module action <moduleId> <actionId> [-p|--project <projectId>] [--data <json>]
```

Executes an action defined in the module manifest.

- For `type: "api"` actions, `--project` is required.
- `--data` accepts a JSON array string of selected result rows (passed through to template variables like `{{selected}}`).

## Settings

Homeboy builds an **effective settings** map for each module by merging settings across scopes, in order (later scopes override earlier ones):

1. Project (`projects/<projectId>.json`): `scoped_modules.<moduleId>.settings`
2. Component (`components/<componentId>.json`): `scoped_modules.<moduleId>.settings`

When running a module, Homeboy passes an execution context via environment variables:

- `HOMEBOY_EXEC_CONTEXT_VERSION`: currently `1`
- `HOMEBOY_MODULE_ID`
- `HOMEBOY_SETTINGS_JSON`: merged effective settings (JSON)
- `HOMEBOY_PROJECT_ID` (optional; when a project context is used)
- `HOMEBOY_COMPONENT_ID` (optional; when a component context is resolved)

Modules can define additional environment variables via `runtime.env` in their manifest.

Module settings validation currently happens during module execution (and may also be checked by other commands). There is no dedicated validation-only command in the CLI.

`homeboy module run` requires the module to be installed/linked under the Homeboy modules directory (discovered by scanning `<config dir>/homeboy/modules/<moduleId>/<moduleId>.json`). There is no separate "installedModules in global config" requirement.

## Runtime Configuration

Executable modules define their runtime behavior in their module manifest (`modules/<moduleId>/<moduleId>.json`):

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

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). `homeboy module` returns a tagged `ModuleOutput` object as `data`.

Top-level variants (`data.command`):

- `module.list`: `{ project_id?, modules: ModuleEntry[] }`
- `module.run`: `{ module_id, project_id? }`
- `module.setup`: `{ module_id }`
- `module.install`: `{ module_id, source, path, linked }`
- `module.update`: `{ module_id, url, path }`
- `module.uninstall`: `{ module_id, path, was_linked }`
- `module.action`: `{ module_id, action_id, project_id?, response }`

Module entry (`modules[]`):

- `id`, `name`, `version`, `description`
- `runtime`: `executable` (has runtime config) or `platform` (no runtime config)
- `compatible` (with optional `--project`)
- `ready` (runtime readiness based on `readyCheck`)
- `configured`: currently always `true` for discovered modules (reserved for future richer config state)
- `linked`: whether the module is symlinked
- `path`: module directory path (may be empty if unknown)

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
