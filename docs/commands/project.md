# `homeboy project`

## Synopsis

```sh
homeboy project [OPTIONS] <COMMAND>
```

## Common Workflows

### Linking Components to a Project

After creating components, link them to a project:

```sh
# Add components to existing project
homeboy project components add my-project component-1 component-2

# Or set all components at once (replaces existing)
homeboy project components set my-project component-1 component-2
```

Components must exist (created via `homeboy component create`) before linking.

## Subcommands

### `list`

```sh
homeboy project list
```

### `show`

```sh
homeboy project show <projectId>
```

Arguments:

- `<projectId>`: project ID

### `create`

```sh
homeboy project create [OPTIONS] [<id>] [<domain>]
```

`create` supports two modes:

- **CLI mode**: pass `[<id>] [<domain>]` as positional arguments.
- **JSON mode**: pass `--json <spec>` (CLI mode arguments are not required).

Options:

- `--json <spec>`: JSON input spec for create/update (single object or bulk; see below)
- `--skip-existing`: skip items that already exist (JSON mode only)
- `--server-id <serverId>`: optional server ID
- `--base-path <path>`: optional base path (local or remote depending on server configuration)
- `--table-prefix <prefix>`: optional table prefix (only used by modules that care about table naming)

Arguments (CLI mode):

- `[<id>]`: project ID
- `[<domain>]`: public site domain

JSON mode:

- `<spec>` accepts `-` (stdin), `@file.json`, or an inline JSON string.
- The payload is the project object (single or array for bulk).

Single:

```json
{ "id": "my-project", "domain": "example.com" }
```

Bulk:

```json
[
  { "id": "my-project", "domain": "example.com" },
  { "id": "my-project-2", "domain": "example.com" }
]
```

JSON output:

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

CLI mode:

```json
{
  "command": "project.create",
  "project_id": "<projectId>",
  "project": { }
}
```

JSON mode:

```json
{
  "command": "project.create",
  "import": {
    "results": [{ "id": "<projectId>", "action": "created|updated|skipped|error" }],
    "created": 1,
    "updated": 0,
    "skipped": 0,
    "errors": 0
  }
}
```

## Local Projects

Projects without a `--server-id` execute commands locally instead of via SSH. Homeboy is environment-agnostic - it works the same way regardless of whether your local environment uses Docker, native installs, or any other setup.

### Creating a Local Project

```sh
homeboy project create <id> <domain> --base-path <local-path>
```

Example:

```sh
homeboy project create my-site my-site.local \
    --base-path "/path/to/site/public"
```

### What Works Locally

All commands execute locally when no `server_id` is configured:

- **CLI tools** (`homeboy wp`, `homeboy composer`) - execute in local shell
- **Database** (`homeboy db`) - uses module templates, executes locally
- **Logs** (`homeboy logs`) - reads files from `base_path`
- **Files** (`homeboy files`) - browses/edits files at `base_path`
- **Module platform behaviors** - project discovery, version patterns, etc.

### What Requires a Server

Only these commands require `server_id`:
- `homeboy deploy` - uploads artifacts to remote server
- `homeboy db tunnel` - creates SSH tunnel for database access

## Subcommands (continued)

### `set`

```sh
homeboy project set <projectId> --json <JSON>
homeboy project set <projectId> '<JSON>'
homeboy project set --json <JSON>   # projectId may be provided in JSON body
```

Updates a project by merging a JSON object into `projects/<id>.json`.

Options:

- `--json <JSON>`: JSON object to merge into config (supports `@file` and `-` for stdin)

Notes:

- `set` no longer supports individual field flags; use `--json` and provide the fields you want to update.

JSON output:

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

```json
{
  "command": "project.set",
  "project_id": "<projectId>",
  "project": { },
  "updated": ["domain", "server_id"],
  "import": null
}
```

JSON output (`list`):

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

```json
{
  "command": "project.list",
  "projects": [
    {
      "id": "<projectId>",
      "domain": "<domain>"
    }
  ]
}
```

JSON output (`show`):

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

```json
{
  "command": "project.show",
  "project_id": "<projectId>",
  "project": { },
  "import": null
}
```

`project` is the serialized `ProjectRecord` (`{ id, config }`).

### `components`

```sh
homeboy project components <COMMAND>
```

Manage the list of components associated with a project.

#### `components list`

```sh
homeboy project components list <projectId>
```

Lists component IDs and the resolved component configs.

JSON output:

```json
{
  "command": "project.components.list",
  "project_id": "<projectId>",
  "components": {
    "action": "list",
    "project_id": "<projectId>",
    "component_ids": ["<componentId>", "<componentId>"],
    "components": [ { } ]
  }
}
```

#### `components add`

```sh
homeboy project components add <projectId> <componentId> [<componentId>...]
```

Adds components to the project if they are not already present.

#### `components remove`

```sh
homeboy project components remove <projectId> <componentId> [<componentId>...]
```

Removes components from the project. Errors if any provided component ID is not currently attached.

#### `components clear`

```sh
homeboy project components clear <projectId>
```

Removes all components from the project.

#### `components set`

```sh
homeboy project components set <projectId> <componentId> [<componentId>...]
```

Replaces the full `component_ids` list on the project (deduped, order-preserving). Component IDs must exist in `homeboy component list`.

You can also do this via `project set` by merging `component_ids`:

```sh
homeboy project set <projectId> --json '{"component_ids":["chubes-theme","chubes-blocks"]}'
```

Example:

```sh
homeboy project components set chubes chubes-theme chubes-blocks chubes-contact chubes-docs chubes-games
```

JSON output:

```json
{
  "command": "project.components.set",
  "project_id": "<projectId>",
  "components": {
    "action": "set",
    "project_id": "<projectId>",
    "component_ids": ["<componentId>", "<componentId>"],
    "components": [ { } ]
  },
  "updated": ["component_ids"]
}
```

### `pin`

```sh
homeboy project pin <COMMAND>
```

#### `pin list`

```sh
homeboy project pin list <projectId> --type <file|log>
```

JSON output:

```json
{
  "command": "project.pin.list",
  "project_id": "<projectId>",
  "pin": {
    "action": "list",
    "project_id": "<projectId>",
    "type": "file|log",
    "items": [
      {
        "path": "<path>",
        "label": "<label>|null",
        "display_name": "<display-name>",
        "tail_lines": 100
      }
    ]
  }
}
```

#### `pin add`

```sh
homeboy project pin add <projectId> <path> --type <file|log> [--label <label>] [--tail <lines>]
```

JSON output:

```json
{
  "command": "project.pin.add",
  "project_id": "<projectId>",
  "pin": {
    "action": "add",
    "project_id": "<projectId>",
    "type": "file|log",
    "added": { "path": "<path>", "type": "file|log" }
  }
}
```

#### `pin remove`

```sh
homeboy project pin remove <projectId> <path> --type <file|log>
```

JSON output:

```json
{
  "command": "project.pin.remove",
  "project_id": "<projectId>",
  "pin": {
    "action": "remove",
    "project_id": "<projectId>",
    "type": "file|log",
    "removed": { "path": "<path>", "type": "file|log" }
  }
}
```

## Related

- [JSON output contract](../json-output/json-output-contract.md)
