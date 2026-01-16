# `homeboy changes`

## Synopsis

```sh
homeboy changes <component_id> [--since <tag>] [--git-diffs]
homeboy changes --cwd [--git-diffs]
homeboy changes --json <spec> [--git-diffs]

# Project mode
homeboy changes --project <project_id> [<component_ids...>] [--git-diffs]
homeboy changes <project_id> <component_id> [<component_id>...] [--git-diffs]
```

## Description

Show changes since the latest git tag for one component, multiple components (bulk JSON), all components attached to a project, or the current working directory.

This command reports:

- commits since the last tag (or a user-provided tag via `--since`)
- uncommitted changes in the working tree (including `uncommittedDiff`)
- optionally, a commit-range diff for commits since the baseline (via `--git-diffs`)

Release workflow note:

- `commits[]` is intended as input to help you author complete release notes.
- `uncommitted`/`uncommitted_diff` is a reminder that you have local edits; if they are intended for the release, commit them as scoped changes before version bumping. If they are not intended for the release, resolve them before version bumping.
- Use `homeboy changelog add` to capture release notes before running `homeboy version bump` or `homeboy release`.

## Options

- `--cwd`: use current working directory (ad-hoc mode, no component registration required)
- `--json <spec>`: bulk mode input
  - Priority: `--cwd > --json > --project > positional`
  - `<spec>` supports `-` (stdin), `@file.json`, or an inline JSON string
  - Spec format: `{ "component_ids": ["id1", "id2"] }`
- `--project <project_id>`: show changes for all components attached to a project
  - If you also pass positional `<component_ids...>`, Homeboy only returns changes for those components
- `--since <tag>`: tag name to compare against (single-component mode only)
- `--git-diffs`: include commit-range diff content in output

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). `homeboy changes` returns either a single `ChangesOutput` or a bulk `BulkChangesOutput` as `data`.

### Single-component output

```json
{
  "component_id": "<component_id>",
  "path": "<local path>",
  "success": true,
  "latest_tag": "<tag>|null",
  "baseline_source": "tag|version_commit|last_n_commits",
  "baseline_ref": "<ref>|null",
  "commits": [
    {
      "hash": "<sha>",
      "subject": "<subject>",
      "category": "Feature|Fix|Breaking|Docs|Chore|Other"
    }
  ],
  "uncommitted": {
    "has_changes": true,
    "staged": ["..."],
    "unstaged": ["..."],
    "untracked": ["..."],
    "hint": "Large untracked list detected..."
  },
  "uncommitted_diff": "<diff>",
  "diff": "<diff>"
}
```

Notes:

- `uncommitted_diff` is present when the working tree has changes.
- `diff` is included only when `--git-diffs` is used.
- `uncommitted.hint` appears when untracked output is unusually large.
- Optional fields like `warning` / `error` may be omitted when unset.

### Bulk output (`--json` or `--project`)

```json
{
  "action": "changes",
  "results": [
    {
      "id": "<component_id>",
      "component_id": "<component_id>",
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

## jq examples

Extract diffs for scripting:

```sh
# Single mode: extract uncommitted diff
homeboy changes --cwd --git-diffs | jq -r '.data.uncommitted_diff // empty'

# Single mode: extract commit-range diff
homeboy changes --cwd --git-diffs | jq -r '.data.diff // empty'

# Bulk mode: extract all diffs (one per component)
homeboy changes --project myproject --git-diffs | jq -r '.data.results[].diff // empty'

# Bulk mode: list components with uncommitted changes
homeboy changes --project myproject | jq -r '.data.results[] | select(.uncommitted.has_changes) | .id'
```

## Related

- [git](git.md)
- [version](version.md)
