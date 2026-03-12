# Extension Specification

This document describes the `homeboy.json` extension manifest format used by the Homeboy CLI and (via the CLI) the desktop app.

## Overview

Extensions extend Homeboy with custom functionality. Each extension is a self-contained directory with:
- `homeboy.json` - Manifest file (required)
- Optional scripts (executed by the CLI based on `runtime.*` commands)
- Optional assets and configuration

## Installation location

Homeboy Desktop is macOS-only.

The desktop app stores installed extensions under its `AppPaths` root:

- Desktop config root (single source of truth: `AppPaths`):
  ```
  ~/Library/Application Support/Homeboy/
  ```
- Extensions:
  ```
  ~/Library/Application Support/Homeboy/extensions/<extension-id>/
  ```

Homeboy Desktop installs/links extensions by running the CLI (`homeboy extension install ...`), but the desktop app does not assume it shares the same on-disk config root as the CLI (`dirs::config_dir()/homeboy`).

Note: the CLI embeds its core documentation in the binary (see `homeboy docs`).
## Extension responsibilities

Homeboy extensions can:

- define **project type behavior** for the platform (discovery, CLI templates, DB templates, deploy verification, version parsing patterns, etc.)
- define **executable tools** (optional `runtime.*` section) runnable via `homeboy extension run`

The manifest is a single unified `homeboy.json` file; extensions include only fields they need.

For the authoritative runtime behavior, see [`homeboy/docs/commands/extension.md`](../../homeboy/docs/commands/extension.md).

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
| `configSchema` | string | No | Project-type config schema identifier (platform behavior) |
| `discovery` | object | No | Project discovery commands (platform behavior) |
| `cli` | object | No | Project-type CLI template (platform behavior) |
| `database` | object | No | Project-type DB templates (platform behavior) |
| `deploy` | array | No | Deployment verification rules (platform behavior) |
| `versionPatterns` | array | No | Version parsing rules by file/extension (platform behavior) |
| `build` | object | No | Build behavior (platform behavior) |
| `commands` | array | No | Additional CLI command names this extension provides (platform behavior) |
| `runtime` | object | No | Executable runtime configuration (for `homeboy extension run`) |
| `inputs` | array | No | Input field definitions (for executable extensions) |
| `output` | object | No | Output configuration (for executable extensions) |
| `actions` | array | No | Action button definitions (for executable extensions) |
| `settings` | array | No | Persistent settings (merged across scopes by the CLI) |
| `requires` | object | No | Extension/component requirements for activation |

### Runtime Object

Executable extensions use the CLI runtime contract described in [`homeboy/docs/commands/extension.md`](../../homeboy/docs/commands/extension.md).

The manifest's `runtime` object configures shell commands that the CLI runs (for example `runCommand`, `setupCommand`, and optional `readyCheck`). The CLI injects execution context and merged settings via environment variables.

### Requirements Object

Extensions can declare dependencies on project configuration (components, feature flags, and project type). Extensions with unmet requirements appear disabled in the sidebar.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `components` | array | No | Component IDs that must exist in the active project config |
| `features` | array | No | Required feature flags (supported: `hasDatabase`, `hasRemoteDeployment`, `hasRemoteLogs`, `hasCLI`) |
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
- `{{settings.key}}` - Value from extension settings

### Setting Object

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | string | Yes | Setting key |
| `type` | string | Yes | `"text"`, `"toggle"`, or `"stepper"` |
| `label` | string | Yes | Display label |
| `placeholder` | string | No | Help text |
| `default` | any | No | Default value |

## Extension output

For `homeboy extension run`, the CLI streams the extension process output to the console and determines success/failure based on exit code.

If your extension is intended for use in Homeboy Desktop's dynamic UI, structure your extension `homeboy.json` using the **inputs/output/actions/settings** fields documented in this file.

## Examples

### Desktop-focused extension (dynamic UI)

```json
{
  "id": "example-tool",
  "name": "Example Tool",
  "version": "1.0.0",
  "icon": "hammer",
  "description": "Example extension demonstrating inputs/output/actions",
  "author": "Your Name",

  "inputs": [
    {
      "id": "url",
      "type": "text",
      "label": "URL",
      "placeholder": "https://example.com",
      "arg": "--url"
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
    }
  ],

  "settings": []
}
```


## Project configuration

Project configuration requirements (local environment settings, subtargets, and CLI template variables) are defined by the CLI and the active project type.

Use the CLI as the source of truth:

- `homeboy docs project`
- `homeboy docs extension`
