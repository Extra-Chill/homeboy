# `homeboy release`

## Synopsis

```sh
homeboy release <COMPONENT> <BUMP_TYPE> [OPTIONS]
```

Where `<BUMP_TYPE>` is `patch`, `minor`, or `major`.

Also available as: `homeboy version bump <COMPONENT> <BUMP_TYPE> [OPTIONS]`

## Options

- `--dry-run`: Preview the release plan without executing
- `--no-tag`: Skip creating git tag
- `--no-push`: Skip pushing to remote
- `--no-commit`: Fail if uncommitted changes exist (strict mode)
- `--commit-message <MESSAGE>`: Custom message for pre-release commit

## Description

`homeboy release` executes a component release: bumps version, finalizes changelog, commits, tags, and optionally pushes. Use `--dry-run` to preview the release plan without making changes.

## Recommended Workflow

```sh
# 1. Review changes since last release
homeboy changes <component_id>

# 2. Preview the release (validates configuration, shows plan)
homeboy release <component_id> patch --dry-run

# 3. Execute the release
homeboy release <component_id> patch
```

## Release Pipeline

The release command coordinates versioning, committing, tagging, and pushing.

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

### Pre-release commit

By default, `homeboy release` automatically commits any uncommitted changes before proceeding with the release:

```sh
# Auto-commits uncommitted changes with default message
homeboy release <component> patch

# Auto-commits with custom message
homeboy release <component> minor --commit-message "final tweaks"

# Strict mode: fail if uncommitted changes exist
homeboy release <component> patch --no-commit
```

The auto-commit:
- Stages all changes (staged, unstaged, untracked)
- Creates a commit with message "pre-release changes" (or custom via `--commit-message`)
- Proceeds with version bump, tagging, and push

Use `--no-commit` to preserve the previous strict behavior that fails on uncommitted changes.

### Pre-flight validation

Before executing the pipeline, `release` validates:

1. **Working tree status**: If `--no-commit` is specified and uncommitted changes exist, the command fails early with actionable guidance.

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

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../architecture/output-system.md). The object below is the `data` payload.

With `--dry-run`:

```json
{
  "command": "release",
  "result": {
    "component_id": "<component_id>",
    "bump_type": "patch",
    "dry_run": true,
    "no_tag": false,
    "no_push": false,
    "no_commit": false,
    "plan": {
      "component_id": "<component_id>",
      "enabled": true,
      "steps": [...],
      "warnings": [],
      "hints": []
    }
  }
}
```

Without `--dry-run`:

```json
{
  "command": "release",
  "result": {
    "component_id": "<component_id>",
    "bump_type": "patch",
    "dry_run": false,
    "no_tag": false,
    "no_push": false,
    "no_commit": false,
    "run": {
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
      "steps": [...]
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

