# `homeboy git`

## Synopsis

```sh
homeboy git <COMMAND>
```

Output is always JSON-wrapped (see [JSON output contract](../json-output/json-output-contract.md)).

Note: some subcommands accept a `--json` flag for bulk operations.

## Subcommands

### Single Component Mode

- `status <componentId>`
- `commit <componentId> [message-or-spec] [--json <spec>] [-m <message>] [--staged-only] [--files <paths>...]`
- `push <componentId> [--tags]`
- `pull <componentId>`
- `tag <componentId> [tagName] [-m <message>]`
  - If `tagName` is omitted, Homeboy tags `v<component version>` (from `homeboy version show`).

### Commit Options

By default, `commit` stages all changes before committing. Use these flags for granular control:

- `-m, --message <msg>`: Commit message (required in CLI mode, or in JSON body)
- `--staged-only`: Commit only changes that are already staged. Skips the automatic `git add .` step.
- `--files <paths>...`: Stage and commit only the specified files.

### CWD Mode (--cwd)

All subcommands support `--cwd` for ad-hoc operations in any git directory without requiring component registration:

- `status --cwd`
- `commit --cwd [message] [-m <message>] [--staged-only] [--files <paths>...] [--json <spec>]`
- `push --cwd [--tags]` (or omit `--cwd` and omit `<componentId>`)
- `pull --cwd` (or omit `--cwd` and omit `<componentId>`)
- `tag --cwd <tagName> [-m <message>]`
  - Tag name is **required** when using `--cwd` (or when omitting `<componentId>`), since there is no component version to derive from.

**CWD commit examples:**

```sh
# Positional message (auto-shifted from component_id position)
homeboy git commit --cwd "Fix the bug"

# Explicit -m flag (also works)
homeboy git commit --cwd -m "Fix the bug"

# JSON spec with --cwd
homeboy git commit --cwd --json '{"message":"Fix bug","staged_only":true}'
```

### JSON Spec Mode (commit)

`homeboy git commit` accepts a **JSON spec** for single or bulk commits.

- You can pass the spec positionally: `homeboy git commit <componentId> '<json>'` (auto-detected as JSON)
- Or pass a plain message positionally: `homeboy git commit <componentId> 'Update docs'`
- Or explicitly: `homeboy git commit <componentId> --json '<json>'` (forces JSON mode)
- The JSON spec value supports:
  - an inline JSON string
  - `-` to read from stdin
  - `@file.json` to read from a file

Homeboy auto-detects **single vs bulk** by checking for a top-level `components` array.

### Bulk Mode (--json)

All subcommands except `tag` support a `--json` flag for bulk operations across multiple components.

- `status --json '<BulkIdsInput>'`
- `commit --json '<BulkCommitInput>'` (or positional spec)
- `push --json '<BulkIdsInput>'`
- `pull --json '<BulkIdsInput>'`

## Bulk JSON Input Schemas

### SingleCommitSpec (for commit JSON spec)

```json
{
  "id": "extra-chill-multisite",
  "message": "Update multisite docs",
  "staged_only": false,
  "files": ["README.md", "docs/index.md"]
}
```

Notes:

- `id` is optional when you also provide a `<componentId>` positional argument (or use `--cwd`).
- `staged_only` defaults to `false`.
- `files` is optional; when present, Homeboy runs `git add -- <files...>` instead of `git add .`.

### BulkCommitInput (for commit)

```json
{
  "components": [
    { "id": "extra-chill-multisite", "message": "Update multisite docs" },
    { "id": "extra-chill-api", "message": "Update API docs" }
  ]
}
```

### BulkIdsInput (for status, push, pull)

```json
{
  "component_ids": ["extra-chill-multisite", "extra-chill-api"],
  "tags": true
}
```

Notes:
- `tags` field is optional (defaults to false), only used for `push`

## JSON Output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

### Single Component Output

```json
{
  "component_id": "<componentId>",
  "path": "<local path>",
  "action": "status|commit|push|pull|tag",
  "success": true,
  "exit_code": 0,
  "stdout": "<stdout>",
  "stderr": "<stderr>"
}
```

### Bulk Output

```json
{
  "action": "status|commit|push|pull",
  "results": [
    {
      "component_id": "extra-chill-multisite",
      "path": "/path/to/component",
      "action": "commit",
      "success": true,
      "exit_code": 0,
      "stdout": "[main abc1234] Update multisite docs\n 2 files changed",
      "stderr": ""
    },
    {
      "component_id": "extra-chill-api",
      "path": "/path/to/component",
      "action": "commit",
      "success": false,
      "exit_code": 1,
      "stdout": "",
      "stderr": "error: nothing to commit"
    }
  ],
  "summary": {
    "total": 2,
    "succeeded": 1,
    "failed": 1
  }
}
```

Notes:

- `commit` returns a successful result with `stdout` set to `Nothing to commit, working tree clean` when there are no changes.
- Bulk operations continue processing all components even if some fail; the summary reports total/succeeded/failed counts.
- Bulk outputs are `BulkGitOutput { action, results, summary }` where `results` is a list of `GitOutput` objects (not the generic bulk envelope used by some other commands).

## Exit code

- Single mode: exit code matches the underlying `git` command.
- Bulk mode (`--json`): `0` if all components succeeded; `1` if any failed.

## Examples

### Single Component

```sh
homeboy git status extra-chill-multisite

# CLI mode
homeboy git commit extra-chill-multisite -m "Update docs"

# Commit only staged changes
homeboy git commit extra-chill-multisite -m "Release notes" --staged-only

# Commit only specific files
homeboy git commit extra-chill-multisite -m "Update docs" --files README.md docs/index.md

# JSON spec mode (single)
homeboy git commit extra-chill-multisite '{"message":"Update docs","files":["README.md"]}'

homeboy git push extra-chill-multisite --tags
homeboy git pull extra-chill-multisite
homeboy git tag extra-chill-multisite v1.0.0 -m "Release 1.0.0"
```

### Bulk Operations

```sh
# Bulk commit with per-component messages
homeboy git commit --json '{"components":[{"id":"extra-chill-multisite","message":"Update multisite docs"},{"id":"extra-chill-api","message":"Update API docs"}]}'

# Bulk commit with staged-only per component
homeboy git commit --json '{"components":[{"id":"extra-chill-multisite","message":"Release prep","staged_only":true}]}'

# Bulk status check
homeboy git status --json '{"component_ids":["extra-chill-multisite","extra-chill-api","extra-chill-users"]}'

# Bulk push with tags
homeboy git push --json '{"component_ids":["extra-chill-multisite","extra-chill-api"],"tags":true}'

# Bulk pull
homeboy git pull --json '{"component_ids":["extra-chill-multisite","extra-chill-api"]}'
```

## Related

- [version](version.md)
