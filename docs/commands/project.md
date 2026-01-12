# `homeboy project`

## Synopsis

```sh
homeboy project <COMMAND>
```

This command accepts the global flags `--json` and `--dry-run` (see [Root command](../cli/homeboy-root-command.md)).

## Subcommands

### `list`

```sh
homeboy project list [--current]
```

Options:

- `--current`: show only the active project ID.

### `show`

```sh
homeboy project show [<projectId>]
```

Arguments:

- `<projectId>` (optional): project ID (uses active project if not specified)

### `create`


```sh
homeboy project create <name> <domain> <project_type> [--server-id <serverId>] [--base-path <path>] [--table-prefix <prefix>] [--activate]
```

Arguments:

- `<name>`: project name
- `<domain>`: public site domain
- `<project_type>`: project type (e.g. `wordpress`)

Options:

- `--server-id <serverId>`: optional server ID
- `--base-path <path>`: optional remote base path
- `--table-prefix <prefix>`: optional WordPress table prefix
- `--activate`: switch active project after create

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

Note: `project set` does not currently update `componentIds`. Use `homeboy project components set` instead.

Arguments:

- `<projectId>`: project ID

Options:

- `--name <name>`: project name
- `--domain <domain>`: public site domain
- `--project-type <type>`: project type (e.g. `wordpress`)
- `--server-id <serverId>`: server ID
- `--base-path <path>`: remote base path
- `--table-prefix <prefix>`: WordPress table prefix

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

#### `components set`

```sh
homeboy project components set <projectId> <componentId> [<componentId>...]
```

Replaces the full `componentIds` list on the project (deduped, order-preserving). Component IDs must exist in `homeboy component list`.

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
