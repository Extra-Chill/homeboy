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
- `--base-path <path>`: optional remote base path
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
  "projectId": "<projectId>",
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
  "projectId": "<projectId>",
  "project": { },
  "updated": ["domain", "serverId"],
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
  "projectId": "<projectId>",
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
  "projectId": "<projectId>",
  "components": {
    "action": "list",
    "projectId": "<projectId>",
    "componentIds": ["<componentId>", "<componentId>"],
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

Replaces the full `componentIds` list on the project (deduped, order-preserving). Component IDs must exist in `homeboy component list`.

You can also do this via `project set` by merging `componentIds`:

```sh
homeboy project set <projectId> --json '{"componentIds":["chubes-theme","chubes-blocks"]}'
```

Example:

```sh
homeboy project components set chubes chubes-theme chubes-blocks chubes-contact chubes-docs chubes-games
```

JSON output:

```json
{
  "command": "project.components.set",
  "projectId": "<projectId>",
  "components": {
    "action": "set",
    "projectId": "<projectId>",
    "componentIds": ["<componentId>", "<componentId>"],
    "components": [ { } ]
  },
  "updated": ["componentIds"]
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
  "projectId": "<projectId>",
  "pin": {
    "action": "list",
    "projectId": "<projectId>",
    "type": "file|log",
    "items": [
      {
        "path": "<path>",
        "label": "<label>|null",
        "displayName": "<display-name>",
        "tailLines": 100
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
  "projectId": "<projectId>",
  "pin": {
    "action": "add",
    "projectId": "<projectId>",
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
  "projectId": "<projectId>",
  "pin": {
    "action": "remove",
    "projectId": "<projectId>",
    "type": "file|log",
    "removed": { "path": "<path>", "type": "file|log" }
  }
}
```

## Related

- [JSON output contract](../json-output/json-output-contract.md)
