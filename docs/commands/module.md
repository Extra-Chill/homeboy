# `homeboy module`

## Synopsis

```sh
homeboy module <COMMAND>
```

## Subcommands

### `list`

```sh
homeboy module list [--project <project_id>]
```

### `run`

```sh
homeboy module run <module_id> [--project <project_id>] [--input <key=value>...] [<args...>]
```

- `--input` repeats; each value must be in `KEY=value` form.
- Trailing `<args...>` are passed to CLI-type modules.

### `setup`

```sh
homeboy module setup <module_id>
```

## JSON output

`ModuleOutput`:

- `command`: `module.list` | `module.run` | `module.setup`
- `projectId` (only used for `module.list` filter)
- `moduleId`
- `modules`: list output for `module.list`
- `runtimeType`: `python` | `shell` | `cli` (for `run` and `setup`)

Module entry (`modules[]`):

- `id`, `name`, `version`, `description`
- `runtime` (runtime type as lowercase string)
- `compatible` (with optional `--project`)
- `ready` (module readiness)

## Exit code

- `module.run`: exit code of the executed module.
- `module.setup`: `0` on success; when the module runtime is not python, it returns `0` without setup.

## Related

- [project](project.md)
