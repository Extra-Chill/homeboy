# `homeboy project`

## Synopsis

```sh
homeboy project [OPTIONS] <COMMAND>
```

## Common Workflows

### Linking Components to a Project

After creating a repo-owned component config, attach that checkout to a project:

```sh
homeboy project components attach-path my-project /path/to/component-repo

# Or replace all component attachments at once
homeboy project components set my-project --json '[{"id":"component-1","local_path":"/repo/component-1","remote_path":"wp-content/plugins/component-1"}]'
```

`attach-path` discovers the component from the checkout's `homeboy.json` and
adds a project attachment for that repo path.

## Subcommands

### `list`

```sh
homeboy project list
```

### `show`

```sh
homeboy project show <project_id>
```

Arguments:

- `<project_id>`: project ID

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
- `--server-id <server_id>`: optional server ID
- `--base-path <path>`: optional base path (local or remote depending on server configuration)
- `--table-prefix <prefix>`: optional table prefix (only used by extensions that care about table naming)

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

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../architecture/output-system.md). The object below is the `data` payload.

CLI mode:

```json
{
  "command": "project.create",
  "project_id": "<project_id>",
  "project": { }
}
```

JSON mode:

```json
{
  "command": "project.create",
  "import": {
    "results": [{ "id": "<project_id>", "action": "created|updated|skipped|error" }],
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

- **Extension CLI tools** (`homeboy wp`, `homeboy cargo`, or extension-provided verbs) - execute in local shell
- **Database** (`homeboy db`) - uses extension templates, executes locally
- **Logs** (`homeboy logs`) - reads files from `base_path`
- **Files** (`homeboy file`) - browses/edits files at `base_path`
- **Extension platform behaviors** - project discovery, version patterns, etc.

### What Requires a Server

Only these commands require `server_id`:
- `homeboy deploy` - uploads artifacts to remote server
- `homeboy db tunnel` - creates SSH tunnel for database access

## Subcommands (continued)

### `set`

```sh
homeboy project set <project_id> --json <JSON>
homeboy project set <project_id> --base64 <BASE64_JSON>
homeboy project set --json <JSON>   # project_id may be provided in JSON body
```

Updates a project by merging a JSON object into `projects/<id>.json`.
Arbitrary project updates must use `--json` or `--base64`; positional JSON and positional `key=value` updates are not accepted.

Options:

- `--json <JSON>`: JSON object to merge into config (supports `@file` and `-` for stdin)
- `--base64 <BASE64_JSON>`: Base64-encoded JSON object for shell-sensitive payloads
- `--replace <field>`: replace array fields instead of union (repeatable)

Notes:

- `set` no longer supports individual field flags; use `--json` and provide the fields you want to update.
- Use `null` in JSON to clear a field (for example, `{"server_id": null}`).

JSON output:

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../architecture/output-system.md). The object below is the `data` payload.

```json
{
  "command": "project.set",
  "project_id": "<project_id>",
  "project": { },
  "updated": ["domain", "server_id"],
  "import": null
}
```

JSON output (`list`):

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../architecture/output-system.md). The object below is the `data` payload.

```json
{
  "command": "project.list",
  "projects": [
    {
      "id": "<project_id>",
      "domain": "<domain>"
    }
  ]
}
```

JSON output (`show`):

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../architecture/output-system.md). The object below is the `data` payload.

```json
{
  "command": "project.show",
  "project_id": "<project_id>",
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
homeboy project components list <project_id>
```

Lists component IDs and the resolved component configs.

JSON output:

```json
{
  "command": "project.components.list",
  "project_id": "<project_id>",
  "components": {
    "action": "list",
    "project_id": "<project_id>",
    "component_ids": ["<component_id>", "<component_id>"],
    "components": [ { } ]
  }
}
```

#### `components attach-path`

```sh
homeboy project components attach-path <project_id> <local_path>
```

Attaches a repo path for a project component discovered via that repo's
`homeboy.json`.

#### `components remove`

```sh
homeboy project components remove <project_id> <component_id> [<component_id>...]
```

Removes components from the project. Errors if any provided component ID is not currently attached.

#### `components clear`

```sh
homeboy project components clear <project_id>
```

Removes all components from the project.

#### `components set`

```sh
homeboy project components set <project_id> --json '<attachments-json>'
```

Replaces the full component attachment list on the project. The JSON payload is
an array of attachments with fields such as `id`, `local_path`, and
`remote_path`.

You can also do this via `project set` by merging `components`:

```sh
homeboy project set <project_id> --json '{"components":[{"id":"chubes-theme","local_path":"/repo/chubes-theme","remote_path":"wp-content/themes/chubes-theme"}]}'
```

Example:

```sh
homeboy project components set chubes --json '[{"id":"chubes-theme","local_path":"/repo/chubes-theme","remote_path":"wp-content/themes/chubes-theme"}]'
```

JSON output:

```json
{
  "command": "project.components.set",
  "project_id": "<project_id>",
  "components": {
    "action": "set",
    "project_id": "<project_id>",
    "component_ids": ["<component_id>", "<component_id>"],
    "components": [ { } ]
  },
  "updated": ["components"]
}
```

### `delete`

```sh
homeboy project delete <project_id>
```

Deletes a project configuration.

### `init`

```sh
homeboy project init <project_id>
```

Initializes the configured project directory.

### `status`

```sh
homeboy project status <project_id>
homeboy project status <project_id> --health-only
```

Shows live server health and component versions for a project. Use
`--health-only` to skip component version checks.

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
  "project_id": "<project_id>",
  "pin": {
    "action": "list",
    "project_id": "<project_id>",
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
homeboy project pin add <project_id> <path> --type <file|log> [--label <label>] [--tail <lines>]
```

JSON output:

```json
{
  "command": "project.pin.add",
  "project_id": "<project_id>",
  "pin": {
    "action": "add",
    "project_id": "<project_id>",
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
  "project_id": "<project_id>",
  "pin": {
    "action": "remove",
    "project_id": "<project_id>",
    "type": "file|log",
    "removed": { "path": "<path>", "type": "file|log" }
  }
}
```

### `rename`

```sh
homeboy project rename <project_id> <new_id>
```

Renames a project by changing its ID.

Notes:

- `new_id` is lowercased before writing.
- The project is moved from `projects/<old-id>.json` to `projects/<new-id>.json`.
- Component references are updated automatically.

Example:

```sh
homeboy project rename my-project my-new-project
```

JSON output:

```json
{
  "command": "project.rename",
  "project_id": "<project_id>",
  "new_id": "<new_id>",
  "renamed": true
}
```

### `remove`

```sh
homeboy project remove <project_id> --json '<JSON>'
homeboy project remove <project_id> '<JSON>'
```

Removes items from project configuration arrays.

Options:

- `--json <JSON>`: JSON object to remove from config (supports `@file` and `-` for stdin)

Note: Use this to remove specific fields or array items from project configuration.
Use `project components remove` for component attachments.

Example:

```sh
# Remove a shared table entry
homeboy project remove my-project --json '{"shared_tables": ["wp_users"]}'
```

JSON output:

```json
{
  "command": "project.remove",
  "project_id": "<project_id>",
  "removed": ["shared_tables"],
  "project": {}
}
```

## Related

- [JSON output contract](../architecture/output-system.md)
