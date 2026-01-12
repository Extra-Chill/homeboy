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
homeboy module install <git_url> [--id <moduleId>]
```

Installs a module by cloning it into `homeboy/modules/<moduleId>/` (under your OS config directory) and writing `.install.json` so it can be updated later.

### `update`

```sh
homeboy module update <moduleId> [--force]
```

Updates a module by running `git pull --ff-only` in the module directory. If the module has uncommitted changes, `--force` is required.

### `uninstall`

```sh
homeboy module uninstall <moduleId> [--force]
```

Uninstalls a module by deleting its directory. `--force` is required (no interactive prompts).

## Settings

Homeboy builds an **effective settings** map for each module by merging settings across scopes, in order (later scopes override earlier ones):

1. App (`homeboy.json`): `installedModules.<moduleId>.settings`
2. Project (`projects/<projectId>.json`): `modules.<moduleId>.settings`
3. Component (`components/<componentId>.json`): `modules.<moduleId>.settings`

When running a module, Homeboy passes an execution context via environment variables:

- `HOMEBOY_EXEC_CONTEXT_VERSION`: currently `1`
- `HOMEBOY_MODULE_ID`
- `HOMEBOY_SETTINGS_JSON`: merged effective settings (JSON)
- `HOMEBOY_PROJECT_ID` (optional; CLI modules when a project context is used)
- `HOMEBOY_COMPONENT_ID` (optional; when a component context is resolved)

Python modules also receive `PLAYWRIGHT_BROWSERS_PATH` (used when Playwright browsers are installed/configured).

`homeboy doctor scan` validates each scope's `settings` object against the module's manifest.

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). `homeboy module` returns a `ModuleOutput` object as the `data` payload.

`ModuleOutput`:

- `command`: `module.list` | `module.run` | `module.setup` | `module.install` | `module.update` | `module.uninstall`
- `projectId` (only used for `module.list` filter)
- `moduleId`
- `modules`: list output for `module.list`
- `runtimeType`: `python` | `shell` | `cli` (for `run` and `setup`)
- `installed`: `{ url, path }` for `module.install`
- `updated`: `{ url, path }` for `module.update`
- `uninstalled`: `{ path }` for `module.uninstall`

Module entry (`modules[]`):

- `id`, `name`, `version`, `description`
- `runtime` (runtime type as lowercase string)
- `compatible` (with optional `--project`)
- `ready` (runtime readiness)
- `configured` (whether the module is present in `homeboy.json` under `installedModules`)

## Exit code

- `module.run`: exit code of the executed module.
- `module.setup`: `0` on success; when the module runtime is not python, it returns `0` without setup.

## Related

- [project](project.md)
