# `homeboy cleanup`

## Synopsis

```sh
homeboy cleanup [component-id] [options]
```

## Description

Identify config drift, stale state, and hygiene issues in component configurations. Checks for broken paths, dead version targets, unused extensions, and other config-level problems that accumulate as projects evolve.

Without a component ID, checks all registered components.

## Arguments

- `[component-id]`: Component to check (optional — omit to check all components)

## Options

- `--severity <LEVEL>`: Show only issues of a specific severity: `error`, `warning`, `info`
- `--category <CATEGORY>`: Show only issues in a specific category: `local_path`, `remote_path`, `version_targets`, `extensions`

## Health Checks

Cleanup runs four categories of config health checks:

### `local_path`

- **Error**: `local_path` is empty
- **Error**: `local_path` is relative (must be absolute)
- **Error**: `local_path` does not exist on disk

### `remote_path`

- **Info**: `remote_path` is empty (deploy will not work)

### `version_targets`

- **Error**: Version target file does not exist at the expected path
- **Warning**: Version target file exists but the configured pattern doesn't match any version string
- **Warning**: Version target has no pattern and no extension provides a default for the file type

### `extensions`

- **Error**: Linked extension could not be loaded (missing or malformed manifest)
- **Info**: Linked extension has no build, lint, test, or CLI capabilities

## Examples

```sh
# Check a single component
homeboy cleanup my-plugin

# Check all registered components
homeboy cleanup

# Show only errors (skip warnings and info)
homeboy cleanup --severity error

# Show only local_path issues
homeboy cleanup --category local_path

# Show only extension issues for a specific component
homeboy cleanup my-plugin --category extensions
```

## JSON Output

Single component:

```json
{
  "success": true,
  "data": {
    "command": "cleanup",
    "component_id": "my-plugin",
    "total_issues": 2,
    "result": {
      "component_id": "my-plugin",
      "summary": { "config_issues": 2 },
      "config_issues": [
        {
          "severity": "error",
          "category": "local_path",
          "message": "local_path does not exist: /old/path/to/plugin",
          "fix_hint": "homeboy component set my-plugin --local-path \"/correct/path\""
        },
        {
          "severity": "info",
          "category": "remote_path",
          "message": "remote_path is empty. Deploy will not work.",
          "fix_hint": "homeboy component set my-plugin --remote-path \"server:/path/to/deploy\""
        }
      ]
    },
    "hints": ["2 config issue(s) found. Review and fix with `homeboy component set`."]
  }
}
```

All components:

```json
{
  "success": true,
  "data": {
    "command": "cleanup",
    "total_issues": 5,
    "results": [
      {
        "component_id": "plugin-a",
        "summary": { "config_issues": 3 },
        "config_issues": ["..."]
      },
      {
        "component_id": "plugin-b",
        "summary": { "config_issues": 2 },
        "config_issues": ["..."]
      }
    ],
    "hints": ["5 total issue(s) across 2 component(s)."]
  }
}
```

## Fix Hints

Each issue includes a `fix_hint` field with the exact `homeboy component set` command to resolve it. Copy-paste the hint to fix the issue.

## Exit Code

- `0`: Always (cleanup is informational, does not fail on issues)

## Related

- [audit](audit.md) — code-level convention drift and structural analysis
- [component](component.md) — manage component configurations
- [JSON output contract](../architecture/output-system.md)
