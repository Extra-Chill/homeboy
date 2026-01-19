# `homeboy release`

## Synopsis

```sh
homeboy release <COMMAND>
```

## Description

`homeboy release` plans and runs component-scoped release pipelines using the `release` configuration. It replaces GitHub Actions by coordinating versioning, committing, tagging, packaging, and module-backed publishing steps locally.

## Recommended Workflow

```sh
# 1. Review changes since last release
homeboy changes <component_id>

# 2. Plan the release (validates configuration, shows auto-inserted steps)
homeboy release plan <component_id>

# 3. Execute the release pipeline
homeboy release run <component_id>
```

## Subcommands

### `plan`

```sh
homeboy release plan <component_id>
```

Generates an ordered release plan without executing any steps.

Notes:

- Release config is read from the component (`components/<id>.json`).
- If no release config exists for the component, the command errors and suggests adding one via `homeboy component set`.
- Module actions are resolved from `component.modules`.
- **Prerequisites validation**: The plan command validates release prerequisites and surfaces warnings:
  - Empty changelog: "No unreleased changelog entries. Run `homeboy changelog add` first."
  - Subsection headers only: "Changelog has subsection headers but no items. Add entries with `homeboy changelog add`."
  - No changelog configured: "No changelog configured for this component."

### `run`

```sh
homeboy release run <component_id>
```

Executes the release pipeline steps defined in the component `release` block.

Notes:

- Steps run in parallel when dependencies allow it.
- Any step depending on a failed/missing step is skipped.
- Release actions use module definitions configured in `component.modules`.
- Release payload includes version, tag, notes, artifacts, component_id, and local_path.
- `module.run` steps execute module runtime commands as part of the pipeline.

## Pipeline steps

Release pipelines support two step types:

- **Core steps**: `build`, `changes`, `version`, `git.commit`, `git.tag`, `git.push`
- **Module-backed steps**: any custom step type implemented as a module action named `release.<step_type>`

### Core step: `git.commit`

Commits release changes (version bumps, changelog updates) before tagging.

**Auto-insert behavior**: If your pipeline has a `git.tag` step but no `git.commit` step, a `git.commit` step is automatically inserted before `git.tag`. This ensures version changes are committed before tagging.

**Default commit message**: `release: v{version}`

**Custom message**:
```json
{
  "id": "git.commit",
  "type": "git.commit",
  "config": {
    "message": "chore: release v1.2.3"
  }
}
```

### Pre-flight validation

Before executing the pipeline, `release run` validates:

1. **Working tree status**: If uncommitted changes exist and no `git.commit` step is present, the command fails early with actionable guidance.

This prevents `cargo publish --locked` and similar commands from failing mid-pipeline due to dirty working trees.

### Pipeline step: `module.run`

Use `module.run` to execute a module runtime command as part of the release pipeline.

Example step configuration:

```json
{
  "id": "scrape",
  "type": "module.run",
  "needs": ["build"],
  "config": {
    "module": "bandcamp-scraper",
    "inputs": [
      { "id": "artist", "value": "some-artist" }
    ],
    "args": ["--verbose"]
  }
}
```

- `config.module` is required.
- `config.inputs` is optional; each entry must include `id` and `value`.
- `config.args` is optional; each entry is a CLI arg string.
- Output includes `stdout`, `stderr`, `exitCode`, `success`, and the release payload.

### Release payload

All module-backed release steps receive a shared payload:

```json
{
  "release": {
    "version": "1.2.3",
    "tag": "v1.2.3",
    "notes": "- Added feature",
    "component_id": "homeboy",
    "local_path": "/path/to/repo",
    "artifacts": [
      { "path": "dist/homeboy-macos.zip", "type": "binary", "platform": "macos" }
    ]
  }
}
```

When a step provides additional config, it is included as `payload.config` alongside `payload.release`.

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

```json
{
  "command": "release.plan",
  "plan": {
    "component_id": "<component_id>",
    "enabled": true,
    "steps": [
      {
        "id": "build",
        "type": "build",
        "label": "Build",
        "needs": [],
        "config": {},
        "status": "ready",
        "missing": []
      }
    ],
    "warnings": [],
    "hints": []
  }
}
```

```json
{
  "command": "release.run",
  "run": {
    "component_id": "<component_id>",
    "enabled": true,
    "result": {
      "status": "success",
      "warnings": [],
      "summary": {
        "total_steps": 5,
        "succeeded": 5,
        "failed": 0,
        "skipped": 0,
        "missing": 0,
        "next_actions": []
      },
      "steps": [
        {
          "id": "build",
          "type": "build",
          "status": "success",
          "missing": [],
          "warnings": [],
          "hints": [],
          "data": {}
        },
        {
          "id": "publish",
          "type": "publish",
          "status": "success",
          "missing": [],
          "warnings": [],
          "hints": [],
          "data": {
            "release": {
              "version": "1.2.3",
              "tag": "v1.2.3",
              "notes": "- Added feature",
              "artifacts": [
                { "path": "dist/homeboy-macos.zip", "type": "binary", "platform": "macos" }
              ],
              "component_id": "homeboy"
            }
          }
        }
      ]
    }
  }
}
```

### Pipeline status values

- `success` - All steps completed successfully
- `partial_success` - Some steps succeeded, others failed (idempotent retry is safe)
- `failed` - All executed steps failed
- `skipped` - Pipeline disabled or all steps skipped due to failed dependencies
- `missing` - Required module actions not found

### Idempotent retry

Publish steps are designed to be idempotent:

- **GitHub releases**: If tag exists, assets are updated via `--clobber`
- **crates.io**: If version already published, step skips gracefully

This allows safe retry after `partial_success` without manual cleanup.
```

## Related

- [component](component.md)
- [module](module.md)

