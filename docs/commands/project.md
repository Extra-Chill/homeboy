# `homeboy project`

## Synopsis

```sh
homeboy project <COMMAND>
```

This command accepts the global flag `--dry-run` (see [Root command](../cli/homeboy-root-command.md)).

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
homeboy project create [--json <spec>] [--skip-existing] [name] [domain] \
  [--module <moduleId>]... [--server-id <serverId>] [--base-path <path>] [--table-prefix <prefix>]
```

Options:

- `--json <spec>`: JSON input spec for create/update (single object or array; see below)
- `--skip-existing`: skip items that already exist (JSON mode only)
- `--server-id <serverId>`: optional server ID (CLI mode)
- `--base-path <path>`: optional remote base path (CLI mode)
- `--table-prefix <prefix>`: optional table prefix (only used by modules that care about table naming)

Arguments (CLI mode):

- `name`: project name
- `domain`: public site domain
- `--module <moduleId>`: enable a module for the project (repeatable). Example: `--module wordpress`

JSON mode:

- `<spec>` accepts `-` (stdin), `@file.json`, or an inline JSON string.
- Payload format:

```json
{
  "op": "project.create",
  "data": { "name": "...", "domain": "...", "modules": ["wordpress"] }
}
```

Bulk payload (`data` as an array) is also supported.

JSON output:

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

CLI mode:

```json
{
  "command": "project.create",
  "projectId": "<projectId>",
  "project": { "id": "<projectId>", "config": {} }
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
homeboy project set <projectId> [--name <name>] [--domain <domain>] [--module <moduleId>]... [--server-id <serverId>] [--base-path <path>] [--table-prefix <prefix>] [--module-setting <moduleId.key=value>]... [--component-ids <ids>]
```

Arguments:

- `<projectId>`: project ID

Options:

- `--name <name>`: project name
- `--domain <domain>`: public site domain
- `--module <moduleId>`: replace the `modules` list (repeatable)
- `--server-id <serverId>`: server ID
- `--base-path <path>`: remote base path
- `--table-prefix <prefix>`: table prefix
- `--module-setting <moduleId.key=value>`: set a module setting (repeatable)
- `--component-ids <ids>`: replace component IDs (comma-separated)

JSON output:

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

```json
{
  "command": "project.set",
  "projectId": "<projectId>",
  "project": {
    "id": "<projectId>",
    "config": { }
  },
  "updated": ["domain", "serverId"],
  "import": null
}
```

### `repair`

```sh
homeboy project repair <projectId>
```

Repairs a project file whose name doesn't match the stored project name.

Arguments:

- `<projectId>`: project ID (file stem)

JSON output:

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

```json
{
  "command": "project.repair",
  "projectId": "<projectId>",
  "project": {
    "id": "<projectId>",
    "config": { }
  },
  "updated": ["id"],
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
      "name": "<name>",
      "domain": "<domain>",
      "modules": ["<moduleId>"]
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
  "project": {
    "id": "<projectId>",
    "config": { }
  },
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

You can also do this via `project set`:

```sh
homeboy project set <projectId> --component-ids chubes-theme,chubes-blocks
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
