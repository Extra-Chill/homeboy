# `homeboy git`

## Synopsis

```sh
homeboy git <COMMAND>
```

Output is always JSON-wrapped (see [JSON output contract](../architecture/output-system.md)).

Note: some subcommands accept a `--json` flag for bulk operations.

## Subcommands

### Single Component Mode

- `status [component_id] [--path <path>]`
- `commit [component_id] [message-or-spec] [--json <spec>] [-m <message>] [--staged-only] [--files <paths>...] [--include <paths>...] [--exclude <paths>...] [--path <path>]`
- `push [component_id] [--tags] [--force-with-lease] [--path <path>]`
- `pull [component_id] [--path <path>]`
- `rebase [component_id] [--onto <ref>] [--continue | --abort] [--path <path>]`
- `cherry-pick [--component-id <id>] [refs...] [--pr <number>...] [--continue | --abort] [--path <path>]`
- `tag [component_id] [tag_name] [-m <message>] [--path <path>]`
  - If `tag_name` is omitted, Homeboy tags `v<component version>` (from `homeboy version show`).

When `component_id` is omitted, Homeboy auto-detects the component from the current directory using the registry or a portable `homeboy.json`. `--path` overrides the checkout path for unregistered clones, worktrees, or CI temp directories.

### Commit Options

By default, `commit` stages all changes before committing. Use these flags for granular control:

- `-m, --message <msg>`: Commit message (required in CLI mode, or in JSON body)
- `--staged-only`: Commit only changes that are already staged. Skips the automatic `git add .` step.
- `--files <paths>...`: Stage and commit only the specified files.
- `--include <paths>...`: Alias for `--files` (repeatable).
- `--exclude <paths>...`: Stage all files except the specified paths.

### Rebase and Cherry-pick

- `rebase` defaults to the current branch's tracked upstream (`@{upstream}`), matching `git pull --rebase` semantics. Use `--onto <ref>` to choose a target explicitly.
- `cherry-pick` accepts positional refs, ranges, and repeatable `--pr <number>` flags. PR numbers are resolved with `gh pr view <n> --json commits`.
- Both subcommands support `--continue` and `--abort` after manual conflict resolution.
- `push --force-with-lease` is exposed for the common post-rebase push path. Plain `--force` is intentionally not exposed.

### JSON Spec Mode (commit)

`homeboy git commit` accepts a **JSON spec** for single or bulk commits.

- You can pass the spec positionally: `homeboy git commit <component_id> '<json>'` (auto-detected as JSON)
- Or pass a plain message positionally: `homeboy git commit <component_id> 'Update docs'`
- Or explicitly: `homeboy git commit <component_id> --json '<json>'` (forces JSON mode)
- The JSON spec value supports:
  - an inline JSON string
  - `-` to read from stdin
  - `@file.json` to read from a file

Homeboy auto-detects **single vs bulk** by checking for a top-level `components` array.

### Bulk Mode (--json)

The status, commit, push, and pull subcommands support a `--json` flag for bulk operations across multiple components.

- `status --json '<bulk_ids_input>'`
- `commit --json '<bulk_commit_input>'` (or positional spec)
- `push --json '<bulk_ids_input>'`
- `pull --json '<bulk_ids_input>'`

`BulkIdsInput` uses `component_ids` (snake_case).

## Bulk JSON Input Schemas

### SingleCommitSpec (for commit JSON spec)

```json
{
  "id": "extra-chill-multisite",
  "message": "Update multisite docs",
  "staged_only": false,
  "include_files": ["README.md", "docs/index.md"]
}
```

Notes:

- `id` is optional when you also provide a `<component_id>` positional argument.
- `staged_only` defaults to `false`.
- `include_files` is optional; when present, Homeboy runs `git add -- <files...>` instead of `git add .`.
- `exclude_files` is optional; when present, Homeboy stages all changes and then unstages the excluded paths.

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
- `force_with_lease` is optional (defaults to false), only used for `push`

## JSON Output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../architecture/output-system.md). The object below is the `data` payload.

### Single Component Output

```json
{
  "component_id": "<component_id>",
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

# Auto-detect from current directory or a portable homeboy.json
homeboy git status

# Operate on an unregistered worktree
homeboy git status --path /tmp/my-worktree

# CLI mode
homeboy git commit extra-chill-multisite -m "Update docs"

# Commit only staged changes
homeboy git commit extra-chill-multisite -m "Release notes" --staged-only

# Commit only specific files
homeboy git commit extra-chill-multisite -m "Update docs" --files README.md docs/index.md

# Commit all but exclude paths
homeboy git commit extra-chill-multisite -m "Update docs" --exclude Cargo.lock

# JSON spec mode (single)
homeboy git commit extra-chill-multisite '{"message":"Update docs","files":["README.md"]}'

homeboy git push extra-chill-multisite --tags
homeboy git push --force-with-lease
homeboy git pull extra-chill-multisite
homeboy git rebase --onto origin/main
homeboy git cherry-pick --pr 123
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
