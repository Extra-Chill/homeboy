# `homeboy release`

## Synopsis

```sh
homeboy release <COMPONENT> <BUMP_TYPE> [OPTIONS]
```

Where `<BUMP_TYPE>` is `patch`, `minor`, or `major`.

Also available as: `homeboy version bump <COMPONENT> <BUMP_TYPE> [OPTIONS]`

## Options

- `--dry-run`: Preview the release plan without executing
- `--deploy`: Deploy this component to all projects that use it after release
- `--recover`: Recover from an interrupted release
- `--skip-checks`: Skip pre-release lint/test checks
- `--allow-underbump`: Override semver guardrail when requested bump is lower than commit-derived recommendation

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
- **Extension-backed steps**: any custom step type implemented as a extension action named `release.<step_type>`

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

### Working tree requirements

Release requires a clean working tree, with two exceptions:

- **Changelog**: May have uncommitted entries from `homeboy changelog add`
- **Version targets**: May be staged (though unusual)

These files are modified during the release anyway and included in the release commit.

Any other uncommitted changes will cause the release to fail with guidance to commit first.

### Pipeline step: `extension.run`

Use `extension.run` to execute a extension runtime command as part of the release pipeline.

Example step configuration:

```json
{
  "id": "scrape",
  "type": "extension.run",
  "needs": ["build"],
  "config": {
    "extension": "bandcamp-scraper",
    "inputs": [
      { "id": "artist", "value": "some-artist" }
    ],
    "args": ["--verbose"]
  }
}
```

- `config.extension` is required.
- `config.inputs` is optional; each entry must include `id` and `value`.
- `config.args` is optional; each entry is a CLI arg string.
- Output includes `stdout`, `stderr`, `exitCode`, `success`, and the release payload.

### Release payload

All extension-backed release steps receive a shared payload:

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

### CI / bot semver recommendation example

For automation, run dry-run JSON and consume:

```sh
homeboy release <component_id> patch --dry-run --json
```

Semver recommendation is exposed at:

- `data.result.plan.semver_recommendation`

Example payload:

```json
{
  "latest_tag": "v0.56.1",
  "range": "v0.56.1..HEAD",
  "commits": [
    {
      "sha": "abc1234",
      "subject": "feat(test): changed-since impact-scoped test execution",
      "commit_type": "feature",
      "breaking": false
    }
  ],
  "recommended_bump": "minor",
  "requested_bump": "patch",
  "is_underbump": true,
  "reasons": [
    "abc1234 feat(test): changed-since impact-scoped test execution"
  ]
}
```

Use this in CI to block accidental under-bumps before tagging/publish.

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
- `missing` - Required extension actions not found

### Idempotent retry

Publish steps are designed to be idempotent:

- **GitHub releases**: If tag exists, assets are updated via `--clobber`
- **crates.io**: If version already published, step skips gracefully

This allows safe retry after `partial_success` without manual cleanup.
```

## Related

- [component](component.md)
- [extension](extension.md)
