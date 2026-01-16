# `homeboy release`

## Synopsis

```sh
homeboy release <COMMAND>
```

## Description

`homeboy release` plans and runs component-scoped release pipelines using the `release` configuration. It is designed to replace the need for GitHub Actions by coordinating versioning, tagging, packaging, and module-backed publishing steps locally.

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

- **Core steps**: `build`, `changes`, `version`, `git.tag`, `git.push`
- **Module-backed steps**: any custom step type implemented as a module action named `release.<step_type>`

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

## Related

- [component](component.md)
- [module](module.md)

