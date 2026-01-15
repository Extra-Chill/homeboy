# `homeboy component`

Manage standalone component configurations stored under `components/<id>.json`.

## Synopsis

```sh
homeboy component [OPTIONS] <COMMAND>
```


## Subcommands

### `create`

```sh
homeboy component create [OPTIONS] --local-path <path> --remote-path <path> --build-artifact <path>
```

The component ID is derived from the `--local-path` directory name (lowercased). For example, `--local-path /path/to/extrachill-api` creates a component with ID `extrachill-api`.

Options:

- `--json <spec>`: JSON input spec for create/update (supports single or bulk)
- `--skip-existing`: skip items that already exist (JSON mode only)
- `--local-path <path>`: absolute path to local source directory (required; ID derived from directory name; `~` is expanded)
- `--remote-path <path>`: remote path relative to project `base_path` (required)
- `--build-artifact <path>`: build artifact path relative to `local_path` (required)
- `--version-target <TARGET>`: version target in format `file` or `file::pattern` (repeatable)
- `--build-command <command>`: build command to run in `local_path` (required for `homeboy build`)
- `--extract-command <command>`: command to run after upload (optional; supports `{artifact}` and `{targetDir}`)

#### Extract Command Execution Context

The `extract_command` runs **inside the target directory**. During deploy, Homeboy:
1. Creates the target directory (`remote_path` joined to project `base_path`)
2. Uploads the artifact into that directory
3. cd's into the target directory
4. Executes your `extract_command`

**Template variables:**
- `{artifact}` - The uploaded artifact filename (not a path, just the filename)
- `{targetDir}` - The full target directory path

**Important:** Since the command runs inside the target directory, your extract logic must account for where files end up relative to the current directory. The agent configuring the component must understand the build output structure to write a correct extract_command.

### `show`

```sh
homeboy component show <id>
```

### `set`

```sh
homeboy component set <id> --json <JSON>
homeboy component set <id> '<JSON>'
homeboy component set --json <JSON>   # id may be provided in JSON body
```

Updates a component by merging a JSON object into `components/<id>.json`.

Options:

- `--json <JSON>`: JSON object to merge into config (supports `@file` and `-` for stdin)

Notes:

- `set` no longer supports individual field flags; use `--json` and provide the fields you want to update.
- If the JSON contains an `id` field that differs from `<id>`, the component is automatically renamed first (equivalent to calling `rename`), then the remaining fields are merged. Project references are updated automatically.

### `delete`

```sh
homeboy component delete <id>
```

Deletion is safety-checked:

- If the component is referenced by one or more projects, the command errors and asks you to remove it from those projects first.

### `rename`

```sh
homeboy component rename <id> <new-id>
```

Renames a component by changing its ID and rewriting any project files that reference the old ID.

Notes:

- `new-id` is lowercased before writing.
- The component is moved from `components/<old-id>.json` to `components/<new-id>.json`.
- Project references are updated by rewriting each project config that uses the component.

Example:

```sh
homeboy component rename extra-chill-api extrachill-api
```

### `list`

```sh
homeboy component list
```

### `projects`

```sh
homeboy component projects <id>
```

Lists all projects that reference the given component. Returns both project IDs and full project objects.

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

`homeboy component` returns a `ComponentOutput` object.

```json
{
  "command": "component.create|component.show|component.set|component.delete|component.rename|component.list|component.projects",
  "component_id": "<id>|null",
  "success": true,
  "updated_fields": ["local_path", "remote_path"],
  "component": {},
  "components": [],
  "import": null,
  "project_ids": ["project-1", "project-2"],
  "projects": []
}
```

Notes:

- In JSON import mode (`homeboy component create --json ...`), `command` is still `component.create` and `import` is populated.
- `updated_fields` is empty for all actions except `set`/`rename`.
- `rename` does not include the old ID; capture it from your input if needed.
- `project_ids` and `projects` are only populated for `component.projects`.


## Related

- [build](build.md)
- [deploy](deploy.md)
- [project](project.md)
- [JSON output contract](../json-output/json-output-contract.md)
