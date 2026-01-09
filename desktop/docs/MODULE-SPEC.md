# Module Specification

This document describes the module.json manifest format for Homeboy modules.

## Overview

Modules extend Homeboy with custom functionality. Each module is a self-contained directory with:
- `module.json` - Manifest file (required)
- Entry point script (for Python/Shell modules)
- Optional assets and configuration

## Installation Location

Modules are installed to:
```
~/Library/Application Support/Homeboy/modules/<module-id>/
```

Each Python module gets its own isolated virtual environment at:
```
~/Library/Application Support/Homeboy/modules/<module-id>/venv/
```

Playwright browsers are shared across modules at:
```
~/Library/Application Support/Homeboy/playwright-browsers/
```

## Module Types

### Python Modules
Run Python scripts in an isolated virtual environment. Dependencies are installed per-module.

### Shell Modules
Run shell scripts directly.

### CLI Modules
Run CLI commands against the configured local project installation using the project type's command template. Supports multisite via URL targeting.

CLI modules can be executed from the terminal via `homeboy module run <module-id>`.

## Manifest Schema

### Root Object

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | string | Yes | Unique identifier (lowercase, hyphens) |
| `name` | string | Yes | Display name |
| `version` | string | Yes | Semantic version (e.g., "1.0.0") |
| `icon` | string | Yes | SF Symbol name |
| `description` | string | Yes | Short description |
| `author` | string | Yes | Author name |
| `homepage` | string | No | URL to documentation/repo |
| `runtime` | object | Yes | Runtime configuration |
| `inputs` | array | Yes | Input field definitions |
| `output` | object | Yes | Output configuration |
| `actions` | array | Yes | Action button definitions |
| `settings` | array | Yes | Persistent settings |
| `requires` | object | No | Component dependencies |

### Runtime Object

#### Common Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `type` | string | Yes | `"python"`, `"shell"`, or `"cli"` |

#### Python/Shell Runtime

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `entrypoint` | string | Yes | Script filename (e.g., `"scraper.py"`) |
| `dependencies` | array | No | Python package names (Python only) |
| `playwrightBrowsers` | array | No | Browsers to install (e.g., `["chromium"]`) |

#### CLI Runtime

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `args` | string | No | Args template passed to CLI (e.g., `"datamachine-events test-scraper"`) |
| `defaultSite` | string | No | Default subtarget ID for multisite projects (lowercase) |

### Requirements Object

Modules can declare dependencies on project configuration (components, feature flags, and project type). Modules with unmet requirements appear disabled in the sidebar.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `components` | array | No | Component IDs that must exist in the active project config |
| `features` | array | No | Required feature flags (supported: `hasDatabase`, `hasRemoteDeployment`, `hasRemoteLogs`, `hasLocalCLI`) |
| `projectType` | string | No | Required project type (e.g. `wordpress`) |

### Input Object

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | string | Yes | Unique input identifier |
| `type` | string | Yes | `"text"`, `"stepper"`, `"toggle"`, or `"select"` |
| `label` | string | Yes | Display label |
| `placeholder` | string | No | Placeholder text (for text inputs) |
| `default` | any | No | Default value |
| `min` | int | No | Minimum value (for stepper) |
| `max` | int | No | Maximum value (for stepper) |
| `options` | array | No | Options for select type |
| `arg` | string | Yes | CLI argument name (e.g., `"--tag"`) |

### Output Object

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `schema` | object | Yes | Output data schema |
| `display` | string | Yes | `"table"`, `"json"`, or `"log-only"` |
| `selectable` | bool | Yes | Whether rows can be selected |

#### Schema Object

| Field | Type | Description |
|-------|------|-------------|
| `type` | string | Always `"array"` |
| `items` | object | Column definitions as `{column_name: "string"}` (null for console-only output) |

### Action Object

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | string | Yes | Unique action identifier |
| `label` | string | Yes | Button label |
| `type` | string | Yes | `"builtin"` or `"api"` |

#### Builtin Actions

| Field | Type | Description |
|-------|------|-------------|
| `builtin` | string | `"copy-column"`, `"export-csv"`, or `"copy-json"` |
| `column` | string | Column name (for copy-column) |

#### API Actions

| Field | Type | Description |
|-------|------|-------------|
| `endpoint` | string | API path (e.g., `"/admin/newsletter/bulk-subscribe"`) |
| `method` | string | HTTP method (`"POST"`, `"GET"`, etc.) |
| `requiresAuth` | bool | Whether authentication is required |
| `payload` | object | Request body with template interpolation |

**Template Variables:**
- `{{selected}}` - Array of selected result rows
- `{{settings.key}}` - Value from module settings

### Setting Object

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | string | Yes | Setting key |
| `type` | string | Yes | `"text"`, `"toggle"`, or `"stepper"` |
| `label` | string | Yes | Display label |
| `placeholder` | string | No | Help text |
| `default` | any | No | Default value |

## Script Output Contract

### Python/Shell Modules

Scripts must output to:
- **stderr**: Real-time progress logs (streamed to console)
- **stdout**: Final JSON result

#### Output JSON Format

```json
{
  "success": true,
  "results": [
    { "column1": "value1", "column2": "value2" }
  ],
  "errors": ["optional warning messages"]
}
```

The `results` array must match the schema defined in `output.schema.items`.

### CLI Modules

CLI modules stream all output (stdout and stderr) to the console. The success/failure is determined by the command's exit code.

## Examples

### Python Module (Scraper)

```json
{
  "id": "example-scraper",
  "name": "Example Scraper",
  "version": "1.0.0",
  "icon": "magnifyingglass",
  "description": "Example module demonstrating the manifest format",
  "author": "Your Name",

  "runtime": {
    "type": "python",
    "entrypoint": "scraper.py",
    "dependencies": ["requests", "beautifulsoup4"]
  },

  "inputs": [
    {
      "id": "url",
      "type": "text",
      "label": "URL",
      "placeholder": "https://example.com",
      "arg": "--url"
    },
    {
      "id": "depth",
      "type": "stepper",
      "label": "Depth",
      "default": 2,
      "min": 1,
      "max": 10,
      "arg": "--depth"
    }
  ],

  "output": {
    "schema": {
      "type": "array",
      "items": {
        "title": "string",
        "link": "string"
      }
    },
    "display": "table",
    "selectable": true
  },

  "actions": [
    {
      "id": "copy-links",
      "label": "Copy Links",
      "type": "builtin",
      "builtin": "copy-column",
      "column": "link"
    },
    {
      "id": "export",
      "label": "Export CSV",
      "type": "builtin",
      "builtin": "export-csv"
    }
  ],

  "settings": []
}
```

### CLI Module (Scraper Tester)

```json
{
  "id": "datamachine-scraper-tester",
  "name": "Scraper Tester",
  "version": "1.0.0",
  "icon": "ant",
  "description": "Test Data Machine event scrapers against venue URLs",
  "author": "Extra Chill",
  "homepage": "https://github.com/Extra-Chill/data-machine",

  "runtime": {
    "type": "cli",
    "args": "datamachine-events test-scraper",
    "defaultSite": "events"
  },

  "requires": {
    "components": ["datamachine-events"],
    "projectType": "wordpress"
  },

  "inputs": [
    {
      "id": "target_url",
      "type": "text",
      "label": "Target URL",
      "placeholder": "https://venue.com/events",
      "arg": "--target_url"
    }
  ],

  "output": {
    "schema": {
      "type": "array",
      "items": null
    },
    "display": "log-only",
    "selectable": false
  },

  "actions": [],
  "settings": []
}
```

## Project Configuration Requirements

### Local CLI Settings

For CLI modules to work, the active project configuration must include `localCLI` settings:

```json
{
  "localCLI": {
    "sitePath": "/path/to/project/root",
    "domain": "your-site.local",
    "cliPath": "/optional/path/to/cli"
  }
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `sitePath` | string | Yes | Path to local project root (e.g., WordPress public directory) |
| `domain` | string | No | Local development domain (e.g., `testing-grounds.local`) |
| `cliPath` | string | No | Explicit path to CLI binary (uses project type default if omitted) |

### Subtarget Settings

For projects with multiple targets (e.g., WordPress multisite), configure `subTargets`:

```json
{
  "subTargets": [
    { "id": "main", "name": "Main Site", "domain": "example.com", "number": 1, "isDefault": true },
    { "id": "blog", "name": "Blog", "domain": "blog.example.com", "number": 2, "isDefault": false }
  ]
}
```

When a CLI module runs on a project with subtargets, the module UI displays a site selector for targeting a specific subtarget.

### CLI Module Execution

CLI modules can be executed in two ways:

1. **Homeboy.app GUI**: Select a module from the sidebar, configure inputs, and click Run
2. **Terminal**: Use `homeboy module run <module-id> [--project <project>] [args...]`

The CLI execution uses the project type's command template with variable substitution for `{{sitePath}}`, `{{domain}}`, `{{cliPath}}`, and `{{args}}`.
