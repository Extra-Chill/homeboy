# `homeboy changes`

## Synopsis

```sh
homeboy changes <componentId> [--since <tag>] [--git-diffs]
homeboy changes --cwd [--git-diffs]
homeboy changes --json <spec> [--git-diffs]

# Project mode
homeboy changes --project <projectId> [<componentIds...>] [--git-diffs]
homeboy changes <projectId> <componentId> [<componentId>...] [--git-diffs]
```

## Description

Show changes since the latest git tag for one component, multiple components (bulk JSON), all components attached to a project, or the current working directory.

This command reports:

- commits since the last tag (or a user-provided tag via `--since`)
- uncommitted changes in the working tree (including `uncommittedDiff`)
- optionally, a commit-range diff for commits since the baseline (via `--git-diffs`)

Release workflow note:

- `commits[]` is intended as input to help you author complete release notes.
- `uncommitted`/`uncommittedDiff` is a reminder that you have local edits; if they are intended for the release, commit them as scoped changes before version bumping. If they are not intended for the release, resolve them before version bumping.

## Options

- `--cwd`: use current working directory (ad-hoc mode, no component registration required)
- `--cwd`: use current working directory (ad-hoc mode, no component registration required)
- `--json <spec>`: bulk mode input
  - Priority: `--cwd > --json > --project > positional`
  - `<spec>` supports `-` (stdin), `@file.json`, or an inline JSON string
  - Spec format: `{ "componentIds": ["id1", "id2"] }`
- `--project <projectId>`: show changes for all components attached to a project
  - If you also pass positional `<componentIds...>`, Homeboy only returns changes for those components
- `--since <tag>`: tag name to compare against (single-component mode only)
- `--git-diffs`: include commit-range diff content in output

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). `homeboy changes` returns either a single `ChangesOutput` or a bulk `BulkChangesOutput` as `data`.

### Single-component output

```json
{
  "componentId": "<componentId>",
  "path": "<local path>",
  "success": true,
  "latestTag": "<tag>|null",
  "baselineSource": "tag|version_commit|last_n_commits",
  "baselineRef": "<ref>|null",
  "commits": [
    {
      "hash": "<sha>",
      "subject": "<subject>",
      "category": "Feature|Fix|Breaking|Docs|Chore|Other"
    }
  ],
  "uncommitted": {
    "hasChanges": true,
    "staged": ["..."],
    "unstaged": ["..."],
    "untracked": ["..."]
  },
  "uncommittedDiff": "<diff>",
  "diff": "<diff>"
}
```

Notes:

- `uncommittedDiff` is present when the working tree has changes.
- `diff` is included only when `--git-diffs` is used.
- Optional fields like `warning` / `error` may be omitted when unset.

### Bulk output (`--json` or `--project`)

```json
{
  "action": "changes",
  "results": [
    {
      "id": "<componentId>",
      "componentId": "<componentId>",
      "path": "<local path>",
      "success": true,
      "commits": [...],
      "uncommitted": {...},
      "error": null
    }
  ],
  "summary": {
    "total": 2,
    "succeeded": 2,
    "failed": 0
  }
}
```

Notes:

- Each item in `results` contains `id` plus all `ChangesOutput` fields flattened in.
- `error` is set when that component failed; `success` and other fields are omitted on failure.

## Exit code

- `0` when the command succeeds and `summary.failed == 0`.
- `1` in bulk/project modes when `summary.failed > 0`.

> Note: single-target modes (`<componentId>` and `--cwd`) always return exit code `0` on success, even when the underlying git operations report `success: false` in the output.

## jq examples

Extract diffs for scripting:

```sh
# Single mode: extract uncommitted diff
homeboy changes --cwd --git-diffs | jq -r '.data.uncommittedDiff // empty'

# Single mode: extract commit-range diff
homeboy changes --cwd --git-diffs | jq -r '.data.diff // empty'

# Bulk mode: extract all diffs (one per component)
homeboy changes --project myproject --git-diffs | jq -r '.data.results[].diff // empty'

# Bulk mode: list components with uncommitted changes
homeboy changes --project myproject | jq -r '.data.results[] | select(.uncommitted.hasChanges) | .id'
```

## Related

- [git](git.md)
- [version](version.md)
