# `homeboy component`

Manage standalone component configurations stored under `components/<id>.json`.

## Synopsis

```sh
homeboy component [OPTIONS] <COMMAND>
```


## Subcommands

### `create`

```sh
homeboy component create [OPTIONS]
```

The component ID comes from an existing `homeboy.json` `id` when present; otherwise it is derived from the `--local-path` directory name (lowercased). For example, `--local-path /path/to/extrachill-api` creates a component with ID `extrachill-api`.

Options:

- `--local-path <path>`: absolute path to local **source / git checkout** directory (required; ID derived from directory name; `~` is expanded). Must be a git repo — not the production deploy target (see [component schema](../reference/schemas/component-schema.md#local_path-vs-remote_path))
- `--remote-path <path>`: remote path relative to project `base_path` (required for deployable components unless already present in `homeboy.json`)
- `--build-artifact <path>`: build artifact path relative to `local_path` (required for artifact deploys; must include a filename)
- `--version-target <TARGET>`: version target in format `file` or `file::pattern` (repeatable)
- `--version-targets <JSON>`: version targets as a JSON array (supports `@file.json` and `-` for stdin)
- `--extract-command <command>`: command to run after upload (optional; supports `{artifact}` and `{targetDir}`)
- `--changelog-target <path>`: changelog path relative to `local_path`
- `--extension <extension>`: extension this component uses (repeatable)
- `--project <project>`: attach the component to a project after creation

Legacy JSON bulk create flags are rejected. `component create` now initializes
repo-owned `homeboy.json` from explicit flags, then registers the component for
ID-based discovery.

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
homeboy component set <id> --base64 <BASE64_JSON>
homeboy component set --json <JSON>   # id may be provided in JSON body
homeboy component set <id> --changelog-target "CHANGELOG.md"   # dedicated flag
```

Updates a component by merging a JSON object into `components/<id>.json`.

Options:

- `--json <JSON>`: JSON object to merge into config (supports `@file` and `-` for stdin)
- `--base64 <BASE64_JSON>`: Base64-encoded JSON object for shell-sensitive payloads
- `--replace <field>`: replace array fields instead of union (repeatable)
- Dedicated flags for common fields: `--local-path`, `--remote-path`, `--build-artifact`, `--extract-command`, `--changelog-target`, `--version-target`, and `--extension`

Arbitrary field updates must use `--json` or `--base64`. Positional JSON, positional `key=value`, and trailing arbitrary `--key value` updates are not accepted.

```sh
homeboy component set my-plugin --json '{"type":"plugin","docs_dir":"docs"}'
```

Notes:

- If the JSON contains an `id` field that differs from `<id>`, the component is automatically renamed first (equivalent to calling `rename`), then the remaining fields are merged. Project references are updated automatically.
- `remote_url` and `triage_remote_url` must be GitHub remotes (`https://github.com/<owner>/<repo>.git` or `git@github.com:<owner>/<repo>.git`). Local filesystem paths and unsupported providers are rejected when writing component config.
- Use `null` in JSON to clear a field (for example, `{"changelog_target": null}`).

#### Release configuration

Components may define a `release` block for component-scoped release planning. You can set it with:

```sh
homeboy component set <id> --json '{"release": {"enabled": true, "steps": []}}'
```

Components also define extension usage via `extensions`:

```sh
homeboy component set <id> --json '{"extensions": {"github": {}, "rust": {}}}'
```

`homeboy build`, `homeboy lint`, `homeboy test`, `homeboy bench`, and `homeboy trace` check component-owned `scripts.<capability>` first, then linked extensions. Components do not support a standalone `build_command` field; use `scripts.build` for component-owned shell builds.

```json
{
  "release": {
    "enabled": true,
    "steps": [
      { "id": "build", "type": "build", "label": "Build", "needs": [], "config": {} }
    ],
    "settings": { "distTarget": "homeboy" }
  }
}
```

#### Setting changelog_target

To configure changelog tracking for a component:

```sh
# Using dedicated flag (recommended)
homeboy component set <id> --changelog-target "CHANGELOG.md"
homeboy component set <id> --changelog-target "docs/CHANGELOG.md"

# Using JSON format
homeboy component set <id> --json '{"changelog_target": "docs/CHANGELOG.md"}'
```

Note: `changelog_target` is a string path relative to `local_path`, not an object.

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

### `shared`

```sh
homeboy component shared [id]
```

Shows which components are shared across projects.

Without an ID, returns a map of all components and the projects using them:

```sh
homeboy component shared
# → my-plugin: [site-a, site-b, site-c]
# → homeboy: [project-1, project-2]
```

With an ID, shows only projects using that specific component:

```sh
homeboy component shared my-plugin
# → [site-a, site-b, site-c]
```

This is useful for:
- Understanding component distribution across your projects
- Planning coordinated deployments with `deploy --shared`
- Identifying candidates for fleet grouping

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../architecture/output-system.md). The object below is the `data` payload.

`homeboy component` returns a `ComponentOutput` object.

```json
{
  "command": "component.create|component.show|component.set|component.delete|component.rename|component.list|component.projects|component.shared|component.env|component.reconcile|component.artifacts",
  "component_id": "<id>|null",
  "success": true,
  "updated_fields": ["local_path", "remote_path"],
  "component": {},
  "components": [],
  "project_ids": ["project-1", "project-2"],
  "projects": [],
  "shared": {}
}
```

Notes:

- `updated_fields` is populated for mutations such as `set`, `rename`, `reconcile --apply`, and `artifacts --apply`.
- `rename` does not include the old ID; capture it from your input if needed.
- `project_ids` and `projects` are only populated for `component.projects`.
- `shared` is only populated for `component.shared`.


## Related

- [build](build.md)
- [deploy](deploy.md)
- [fleet](fleet.md)
- [project](project.md)
- [JSON output contract](../architecture/output-system.md)
