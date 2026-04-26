# Portable Component Config (`homeboy.json`)

A `homeboy.json` file in a repo root defines portable component configuration that travels with the code. Clone a repo, run one command, and homeboy knows how to build, test, version, and deploy it.

## Schema

```json
{
  "remote_path": "string",
  "build_artifact": "string",
  "extract_command": "string",
  "version_targets": [
    {
      "file": "string",
      "pattern": "string"
    }
  ],
  "changelog_target": "string",
  "extensions": {
    "extension_id": {}
  }
}
```

All fields are optional. The component `id` is derived from the directory name, and `local_path` is always machine-specific (provided at registration time).

## Example

```json
{
  "remote_path": "wp-content/plugins/data-machine",
  "version_targets": [
    {
      "file": "data-machine.php",
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
| `id` | Derived from directory name automatically |

### What goes in homeboy.json

| Field | Description |
|-------|-------------|
| `remote_path` | Deploy target relative to project `base_path` |
| `build_artifact` | Build output path relative to repo root |
| `extract_command` | Post-upload command (supports `{artifact}`, `{targetDir}`) |
| `version_targets` | Version detection patterns |
| `changelog_target` | Path to changelog file |
| `extensions` | Extension configuration (e.g., `{"wordpress": {}}`) |

Build, lint, test, bench, and review behavior comes from linked extensions. For one-off shell commands, use a rig `command` step; component-level `build_command` is not supported.

## Precedence

CLI flags override `homeboy.json` values. This lets teams share a base config while individuals customize for their environment:

```
homeboy.json (repo)  →  CLI flags (override)  →  ~/.config/homeboy/components/ (stored)
```

## Related

- [Component schema](component-schema.md) - Full component configuration reference
- [Component command](../commands/component.md) - CLI reference
