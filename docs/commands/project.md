# `homeboy project`

## Synopsis

```sh
homeboy project <COMMAND>
```

## Subcommands

### `create`

```sh
homeboy project create <name> <domain> <project_type> [--server-id <id>] [--base-path <path>] [--table-prefix <prefix>] [--activate]
```

- `--activate`: set the new project as the active project.

JSON output:

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

```json
{
  "command": "project.create",
  "projectId": "<id>",
  "project": {
    "id": "<id>",
    "config": { }
  }
}
```

### `set`

```sh
homeboy project set <project_id> [--name <name>] [--domain <domain>] [--project-type <type>] [--server-id <id>] [--base-path <path>] [--table-prefix <prefix>]
```

JSON output:

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

```json
{
  "command": "project.set",
  "projectId": "<id>",
  "project": {
    "id": "<id>",
    "config": { }
  },
  "updated": ["domain", "serverId"]
}
```

### `repair`

```sh
homeboy project repair <project_id>
```

Repairs a project file whose filename (project id) does not match its stored project name.

JSON output:

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

```json
{
  "command": "project.repair",
  "projectId": "<id>",
  "project": {
    "id": "<id>",
    "config": { }
  },
  "updated": ["id"]
}
```

### `list`

```sh
homeboy project list [--current]
```

- `--current`: return only the active project ID.

JSON output (`list`):

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

```json
{
  "command": "project.list",
  "activeProjectId": "<id>|null",
  "projects": [
    {
      "id": "<id>",
      "name": "<name>",
      "domain": "<domain>",
      "projectType": "<type>",
      "active": true
    }
  ]
}
```

JSON output (`--current`):

```json
{
  "command": "project.current",
  "activeProjectId": "<id>|null",
  "projects": null
}
```

### `show`

```sh
homeboy project show [project_id]
```

- `project_id` (optional): if omitted, uses the active project.

JSON output:

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

```json
{
  "command": "project.show",
  "projectId": "<id>",
  "project": {
    "id": "<id>",
    "config": { }
  }
}
```

`project` is the serialized `ProjectRecord` (`{ id, config }`).

### `switch`

```sh
homeboy project switch <project_id>
```

JSON output:

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

```json
{
  "command": "project.switch",
  "projectId": "<id>",
  "project": {
    "id": "<id>",
    "config": { }
  }
}
```

### `pin`

```sh
homeboy project pin <COMMAND>
```

#### `pin list`

```sh
homeboy project pin list <project_id> --type <file|log>
```

JSON output:

```json
{
  "command": "project.pin.list",
  "projectId": "<id>",
  "pin": {
    "action": "list",
    "projectId": "<id>",
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
homeboy project pin add <project_id> <path> --type <file|log> [--label <label>] [--tail <lines>]
```

JSON output:

```json
{
  "command": "project.pin.add",
  "projectId": "<id>",
  "pin": {
    "action": "add",
    "projectId": "<id>",
    "type": "file|log",
    "added": { "path": "<path>", "type": "file|log" }
  }
}
```

#### `pin remove`

```sh
homeboy project pin remove <project_id> <path> --type <file|log>
```

JSON output:

```json
{
  "command": "project.pin.remove",
  "projectId": "<id>",
  "pin": {
    "action": "remove",
    "projectId": "<id>",
    "type": "file|log",
    "removed": { "path": "<path>", "type": "file|log" }
  }
}
```

## Related

- [JSON output contract](../json-output/json-output-contract.md)
