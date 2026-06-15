# Component Schema

Component configuration defines buildable and deployable units stored in `components/<id>.json`.

## Schema

```json
{
  "id": "string",
  "name": "string",
  "local_path": "string",
  "remote_path": "string",
  "build_artifact": "string",
  "extract_command": "string",
  "version_targets": [
    {
      "file": "string",
      "pattern": "string",
      "artifact_path": "string"
    }
  ],
  "changelog_target": "string",
  "hooks": {
    "pre:version:bump": ["shell command"],
    "post:version:bump": ["shell command"],
    "post:release": ["shell command"],
    "post:deploy": ["shell command"]
  },
  "scripts": {
    "lint": ["shell command"],
    "test": ["shell command"],
    "build": ["shell command"],
    "bench": ["shell command"],
    "trace": ["shell command"]
  },
  "extensions": {},
  "release": {}
}
```

## Fields

### Required Fields

- **`id`** (string): Unique component identifier, derived from `local_path` directory name (lowercased)
- **`local_path`** (string): Absolute path to local **source / git checkout** directory, `~` is expanded
- **`remote_path`** (string): Remote path relative to project `base_path` (the **deploy target**)
- **`build_artifact`** (string): Build artifact path relative to `local_path`, must include filename

> **Important:** `local_path` must point to a **git repository / source checkout**, not the production deploy target. The deploy target is derived from `project.base_path + component.remote_path`. If `local_path` points to the deployed directory, builds will run inside production and uncommitted-changes checks will fail (the directory isn't a git repo). This is a common misconfiguration after server migrations.

### Optional Fields

- **`name`** (string): Human-readable component name, defaults to `id`
- **`extract_command`** (string): Command to execute after artifact upload, runs inside target directory
  - Supports template variables: `{artifact}`, `{targetDir}`
- **`version_targets`** (array): List of version detection patterns
  - **`file`** (string): Path to file containing version (relative to `local_path`). This is the **source** path that the version bump writes to.
  - **`pattern`** (string): Regex pattern to extract version (first capture group)
  - **`artifact_path`** (string, optional): Path to verify inside the deploy artifact (ZIP) when it differs from `file`. The bump writes `file` (git-tracked source), while pre-deploy verification looks for `artifact_path` inside the shipped artifact. Use this for `@wordpress/scripts` plugins that bump source `blocks/<block>/block.json` but ship the compiled `build/<block>/block.json` (the `blocks/` source dir is excluded from the ZIP). When unset, verification falls back to `file`.
- **`changelog_target`** (string): Path to changelog file (relative to `local_path`)
- **`extensions`** (object): Extension-specific settings
  - Keys are extension IDs (e.g., `"wordpress"`, `"rust"`)
  - Values are flat extension setting objects; `version` is reserved for extension version constraints
- **`scripts`** (object): Component-owned shell commands for extension-shaped capabilities
  - Supported keys: `lint`, `test`, `build`, `bench`, `trace`
  - Each value is an array of shell commands run sequentially in `local_path`
  - Resolution order is `scripts.<capability>` first, then linked extension support, then not-applicable
  - Scripts receive the same runner env paths (`HOMEBOY_COMPONENT_ID`, `HOMEBOY_COMPONENT_PATH`, `HOMEBOY_RUN_DIR` and sidecar file vars when relevant) as extension runners, with `HOMEBOY_EXTENSION_ID=component-script`
  - Use `scripts.build`, not `build_command`; `build_command` is still only a diagnostic output field.
- **`release`** (object): Component-scoped release configuration
  - **`enabled`** (boolean): Whether release pipeline is enabled
  - **`steps`** (array): Release step definitions
  - **`settings`** (object): Release pipeline settings

### Runtime Requirements

`homeboy component env` reports runtime requirements in a generic `runtimes` map:

```json
{
  "command": "component.env",
  "id": "example",
  "extension": "example-extension",
  "runtimes": {
    "php": { "version": "8.2", "source": "component" },
    "node": { "version": "22", "source": "extension:example-extension" }
  }
}
```

Runtime IDs are extension-owned strings. `source` is `component` for component config or detector output and `extension:<id>` for extension-provided defaults. Component extension settings and detector output must use the canonical `runtimes` map with object values containing `version`.

### Hook Fields

- **`hooks`** (object): Lifecycle hook commands keyed by event name
  - `pre:version:bump`: Commands to run before version targets are updated
  - `post:version:bump`: Commands to run after version files are updated
  - `post:release`: Commands to run after the release pipeline completes
  - `post:deploy`: Commands to run on the deployment target after deploy
  - Each value is an array of shell commands run sequentially

## local_path vs remote_path

```
local_path (source)                          remote_path (deploy target)
┌──────────────────────────┐                ┌──────────────────────────────────────────┐
│ ~/repos/extrachill-api/  │  ── build ──▶  │ /var/www/site/wp-content/plugins/        │
│ (git checkout, builds    │    deploy      │ extrachill-api/                          │
│  run here)               │                │ (project.base_path + remote_path)        │
└──────────────────────────┘                └──────────────────────────────────────────┘
```

- **`local_path`** — where the code lives on your dev machine (a git repo)
- **`remote_path`** — where it gets deployed on the server (relative to the project's `base_path`)

Setting `local_path` to the same directory as the deploy target is a misconfiguration — builds would run in production and `homeboy deploy` would fail uncommitted-changes checks.

## Example

```json
{
  "id": "extrachill-api",
  "name": "Extra Chill API",
  "local_path": "/Users/dev/extrachill-api",
  "remote_path": "wp-content/plugins/extrachill-api",
  "build_artifact": "build/extrachill-api.zip",
  "extract_command": "unzip -o {{artifact}} && rm {{artifact}}",
  "version_targets": [
    {
      "file": "composer.json",
      "pattern": "\"version\":\\s*\"([^\"]+)\""
    }
  ],
  "changelog_target": "CHANGELOG.md",
  "hooks": {
    "pre:version:bump": [
      "./scripts/verify-generated-sources"
    ],
    "post:version:bump": [
      "./scripts/refresh-versioned-artifacts"
    ],
    "post:release": [
      "echo 'Release complete!'"
    ]
  },
  "extensions": {
    "wordpress": {
      "php_version": "8.1"
    }
  },
  "release": {
    "enabled": true,
    "steps": [
      {
        "id": "preflight.test",
        "type": "preflight.test",
        "label": "Run Tests"
      }
    ]
  }
}
```

## Version Target Format

Version targets use regex to extract semantic versions from files. The pattern must include a capture group for the version string.

Common patterns:
- Composer: `\"version\":\\s*\"([^\"]+)\"`
- Cargo: `^version\\s*=\\s*\"([^\"]+)\"`
- Package.json: `\"version\":\\s*\"([^\"]+)\"`
- WordPress plugin header: `Version:\\s*([\\d.]+)`

## Extract Command Context

The `extract_command` runs inside the target directory after artifact upload. The working directory is:
- `project.base_path + component.remote_path`

Available template variables:
- **`{artifact}`** - The uploaded artifact filename only
- **`{targetDir}`** - Full target directory path

## Storage Location

Components are stored as individual JSON files under the OS config directory:
- **macOS/Linux**: `~/.config/homeboy/components/<id>.json`
- **Windows**: `%APPDATA%\homeboy\components\<id>.json`

## Related

- [Component command](../commands/component.md) - Manage component configuration
- [Hooks system](../architecture/hooks.md) - Lifecycle hooks for version and release operations
- [Project schema](project-schema.md) - How components link to projects
- [Extension manifest schema](extension-manifest-schema.md) - Extension configuration structure
