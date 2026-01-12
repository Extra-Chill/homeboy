# `homeboy project`

## Synopsis

```sh
homeboy project <COMMAND>
```

## Subcommands

### `list`

```sh
homeboy project list [--current]
```

- `--current`: return only the active project ID.

### `show`

```sh
homeboy project show [<projectId>]
```

- `projectId` (optional): if omitted, uses the active project.

### `create`


```sh
homeboy project create <name> <domain> <project_type> [--server-id <serverId>] [--base-path <path>] [--table-prefix <prefix>] [--activate]
```

- `--activate`: set the new project as the active project.

JSON output:

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

```json
{
  "command": "project.create",
  "projectId": "<projectId>",
  "project": {
    "id": "<projectId>",
    "config": { }
  }
}
```

### `set`

```sh
homeboy project set <projectId> [--name <name>] [--domain <domain>] [--project-type <type>] [--server-id <serverId>] [--base-path <path>] [--table-prefix <prefix>]
```

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
  "updated": ["domain", "serverId"]
}
```

### `repair`

```sh
homeboy project repair <projectId>
```

Repairs a project file whose filename (project id) does not match its stored project name.

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
  "updated": ["id"]
}
```

JSON output (`list`):

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

```json
{
  "command": "project.list",
  "activeProjectId": "<projectId>|null",
  "projects": [
    {
      "id": "<projectId>",
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
  "activeProjectId": "<projectId>|null",
  "projects": null
}
```

JSON output:

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

```json
{
  "command": "project.show",
  "projectId": "<projectId>",
  "project": {
    "id": "<projectId>",
    "config": { }
  }
}
```

`project` is the serialized `ProjectRecord` (`{ id, config }`).

### `switch`

```sh
homeboy project switch <projectId>
```

JSON output:

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

```json
{
  "command": "project.switch",
  "projectId": "<projectId>",
  "project": {
    "id": "<projectId>",
    "config": { }
  }
}
```

### `components`

```sh
homeboy project components <COMMAND>
```

Manage the list of components associated with a project.

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
