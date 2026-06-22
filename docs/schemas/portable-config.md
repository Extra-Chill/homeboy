# Portable Component Config (`homeboy.json`)

A `homeboy.json` file in a repo root defines portable component configuration that travels with the code. Clone a repo, run one command, and homeboy knows how to build, test, version, and deploy it.

## Schema

```json
{
  "id": "string",
  "remote_path": "string",
  "build_artifact": "string",
  "deploy_together": ["component-id"],
  "extract_command": "string",
  "version_targets": [
    {
      "file": "string",
      "pattern": "string",
      "artifact_path": "string"
    }
  ],
  "changelog_target": "string",
  "extensions": {
    "extension_id": {}
  }
}
```

The component `id` field is required and must be a non-empty string. All other fields are optional. `local_path` is always machine-specific and provided at registration or resolution time.

## Example

```json
{
  "id": "sample-plugin",
  "remote_path": "wp-content/plugins/sample-plugin",
  "version_targets": [
    {
      "file": "sample-plugin.php",
      "pattern": "Version:\\s*([0-9.]+)"
    }
  ],
  "changelog_target": "docs/CHANGELOG.md",
  "extensions": {
    "wordpress": {}
  }
}
```

## Usage

### Initialize or refresh repo config

```bash
# Initialize repo-owned homeboy.json from supported create flags
homeboy component create --local-path /path/to/repo --remote-path wp-content/plugins/my-plugin

# If homeboy.json already exists, preserve unknown fields and refresh known fields
homeboy component create --local-path /path/to/repo

# Override any supported field from the CLI:
homeboy component create --local-path /path/to/repo --changelog-target "CHANGELOG.md"
```

### What stays local (not in homeboy.json)

| Field | Why |
|-------|-----|
| `local_path` | Absolute path, varies per machine |

### What goes in homeboy.json

| Field | Description |
|-------|-------------|
| `id` | Required stable component identifier |
| `remote_path` | Deploy target relative to project `base_path` |
| `build_artifact` | Build output path relative to repo root |
| `deploy_together` | Component IDs that must be deployed in the same operation as this component |
| `extract_command` | Post-upload command (supports `{artifact}`, `{targetDir}`) |
| `version_targets` | Version detection patterns (`file`, `pattern`, optional `artifact_path`) |
| `changelog_target` | Path to changelog file |
| `scripts` | Optional component-owned `lint`, `test`, `build`, `bench`, and `trace` shell commands |
| `extensions` | Extension configuration (e.g., `{"wordpress": {}}`) |

Build, lint, test, bench, and trace behavior resolves from `scripts.<capability>` first, then linked extensions. Use `scripts.build` for component-owned shell builds; component-level `build_command` is not supported.

Use `deploy_together` for coupled components that are versioned or built separately but must stay in sync at runtime. When a deploy selection includes one member of a declared group without the rest, Homeboy fails the plan before building or uploading.

## Precedence

CLI flags override `homeboy.json` values. This lets teams share a base config while individuals customize for their environment:

```
homeboy.json (repo)  →  CLI flags (override)  →  ~/.config/homeboy/components/ (stored)
```

## Related

- [Component schema](component-schema.md) - Full component configuration reference
- [Component command](../commands/component.md) - CLI reference
