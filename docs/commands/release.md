# `homeboy release`

## Synopsis

```sh
homeboy release [OPTIONS] [COMPONENTS]...
```

By default Homeboy auto-detects the bump from commit history. Use `--bump <major|minor|patch|VERSION>` to force a bump type or explicit version.

## Options

- `--dry-run`: Preview the release plan without executing
- `--project <PROJECT>` / `-p <PROJECT>`: Release components from a project
- `--outdated`: With `--project`, release only components with unreleased commits
- `--path <PATH>`: Override local path for a single-component release
- `--apply`: Confirm risky real release modes such as `--deploy`, `--recover`, `--retag`, `--head`, or bare `--skip-checks`
- `--deploy`: Deploy this component to all projects that use it after release
- `--recover`: Recover from an interrupted release
- `--head`: Finish the release pipeline for the version commit and tag already checked out at HEAD
- `--from-artifacts <DIR>`: With `--head`, attach/publish existing artifacts from a directory instead of running `release.package`
- `--skip-checks`: Skip pre-release lint/test checks
- `--bump <BUMP>`: Force `major`, `minor`, `patch`, or an explicit version like `2.0.0`
- `--force-lower-bump`: Allow a forced bump lower than the commit-derived recommendation
- `--skip-publish`: Skip publish/package steps; useful when CI publishes after the tag is pushed
- `--no-github-release`: Skip GitHub Release creation while still tagging and pushing
- `--git-identity <IDENTITY>`: Configure git identity for release commits/tags; use `bot` or `Name <email>`

## Description

`homeboy release` executes component releases: detects or applies a version bump, finalizes generated changelog entries, commits, tags, pushes, and optionally publishes release artifacts. Use `--dry-run` to preview the release plan without making changes.

`--head` is for CI jobs where another step already created the release commit and tag, but Homeboy should still own the rest of the release lifecycle. It keeps the safe preflight checks, skips changelog/version/git mutation steps, populates release state from the version and tag at HEAD, then runs `release.package` (unless `--from-artifacts` is provided), `github.release`, `release.publish`, cleanup, and post-release hooks through the normal pipeline.

Risky real release modes require explicit `--apply`: `--deploy`, `--recover`, `--retag`, `--head`, and bare `--skip-checks`. Dry-run previews never require `--apply`, and granular skips such as `--skip-checks=lint` keep the normal release flow because other quality gates remain active.

When `--bump` requests a lower keyword bump than Homeboy detects from releasable commits, release execution requires confirmation in an interactive terminal. Non-interactive runs must pass `--force-lower-bump`; otherwise Homeboy refuses before creating release artifacts, commits, tags, or pushes. Dry-run still returns the plan and semver recommendation for review.

## Recommended Workflow

```sh
# 1. Review changes since last release
homeboy changes <component_id>

# 2. Preview the release (validates configuration, shows plan)
homeboy release <component_id> --dry-run

# 3. Execute the release
homeboy release <component_id>
```

### Finish an already-tagged release

```sh
# Build/package artifacts somewhere else, then let Homeboy create/update the
# GitHub Release and run publish hooks without re-tagging.
homeboy release <component_id> --head --from-artifacts ./artifacts --skip-checks --apply
```

## Release Pipeline

The release command coordinates versioning, committing, tagging, and pushing.

## Pipeline steps

Release pipelines support two step types:

- **Core steps**: `build`, `changes`, `version`, `git.commit`, `git.tag`, `git.push`
- **Extension-backed steps**: publish/package/prepare behavior implemented by extension release actions such as `release.prepare`, `release.package`, or `release.publish.<target>`

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

- **Changelog**: May have uncommitted entries (these will be finalized during release)
- **Version targets**: May be staged (though unusual)

These files are modified during the release anyway and included in the release commit.

Any other uncommitted changes will cause the release to fail with guidance to commit first.

### Extension-backed release behavior

Older release docs may mention generic `extension.run` steps. Current release
execution is intentionally narrower: the executable release plan is built from
known core steps plus extension-backed release actions declared in extension
manifests.

Use extension manifests for release-specific prepare, package, and publish
actions. Core keeps the release graph generic; extensions own platform-specific
work.

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

For automation, run a dry-run and persist the structured payload with the global
`--output <PATH>` flag:

```sh
homeboy --output release-plan.json release <component_id> --dry-run
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

Use this in CI to inspect Homeboy's commit-derived recommendation before tagging/publish. Pass `--bump` when automation or a maintainer needs to override the detected bump. If automation intentionally publishes a lower keyword bump than `recommended_bump`, pass `--force-lower-bump` with the release command.

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

### Skipped releases

When Homeboy classifies the commit range as having no releasable commits (e.g. docs-only or guidance-only changes), the release is skipped: no tag, no release commit, and no GitHub Release are produced. The command result reports `status: "skipped"` with a `skipped_reason` (`no-releasable-commits`, `major-requires-flag`, or `release-already-at-head`) and an actionable force hint.

A skipped release is **not** reported as success: the process exits with code `5` and the JSON envelope reports `success: false`, even though `data` still carries the full result payload. This lets operators and CI distinguish a no-op release from a real one. To force a release when the skip is intentional, re-run with `--bump` (the hint echoes the exact command, including flags like `--skip-checks`).

### Idempotent retry

Publish steps are designed to be idempotent:

- **GitHub releases**: If tag exists, assets are updated via `--clobber`
- **crates.io**: If version already published, step skips gracefully

GitHub release CI publishes to crates.io when the repository has a `CARGO_REGISTRY_TOKEN`
secret configured.

This allows safe retry after `partial_success` without manual cleanup.

## Related

- [component](component.md)
- [extension](extension.md)
