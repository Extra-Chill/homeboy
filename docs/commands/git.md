# `homeboy git`

## Synopsis

```sh
homeboy git <COMMAND>
```

Git operations for Homeboy components, worktrees, portable checkouts, and GitHub issue / pull request workflows.

Most commands emit Homeboy's structured JSON envelope when appropriate. See the [JSON output contract](../architecture/output-system.md). Some subcommands also accept `--json` for bulk input.

## Component Resolution

Most core git verbs can run without a positional component ID:

```sh
homeboy git status
homeboy git commit -m "Update docs"
homeboy git push
```

Homeboy resolves the target checkout in this order:

1. **Explicit component ID plus `--path`**: trust both values. Use this when a command needs a known component identity but should operate on a specific checkout.
2. **`--path <path>` only**: operate on that checkout, discovering the component from a portable `homeboy.json` at the path or its git root. If no portable config exists, Homeboy derives the component ID from the path basename.
3. **Explicit component ID only**: resolve the component from Homeboy's registry.
4. **Neither ID nor `--path`**: auto-detect from the current directory using the registry, then a portable `homeboy.json` in the current checkout or git root.

Use `--path` for worktrees, CI runners, ad-hoc clones, or headless scripts where the process CWD is not already the checkout you want:

```sh
homeboy git status --path /Users/chubes/Developer/homeboy@docs-refresh-git-command
homeboy git push --path /tmp/homeboy-ci-checkout
```

## Core Verbs

### Status

```sh
homeboy git status [component_id] [--path <path>]
```

Shows git status for one checkout. `component_id` is optional when Homeboy can detect the component from CWD or `--path`.

Bulk status is available through `--json`:

```sh
homeboy git status --json '{"component_ids":["homeboy","data-machine"]}'
```

### Commit

```sh
homeboy git commit [component_id] [message-or-spec]
homeboy git commit -m "Update docs"
homeboy git commit --staged-only -m "Use staged changes only"
homeboy git commit --files README.md docs/index.md -m "Update docs"
homeboy git commit --exclude Cargo.lock -m "Update docs"
```

By default, `commit` stages all changes before committing. Use `--staged-only`, `--files`, `--include`, or `--exclude` for narrower staging.

`commit` also accepts a JSON spec. The spec can be passed positionally, through `--json`, from stdin with `-`, or from a file with `@file.json`:

```sh
homeboy git commit '{"message":"Update docs","include_files":["README.md"]}'
homeboy git commit --json '@commit.json'
```

Homeboy auto-detects single vs bulk commit specs by checking for a top-level `components` array.

### Push

```sh
homeboy git push [component_id] [--tags] [--force-with-lease] [--path <path>]
```

`push --force-with-lease` is the safe post-rebase force-push path. It refuses to overwrite the remote if it has commits the local ref has not seen. Plain `--force` is intentionally not exposed.

```sh
homeboy git push
homeboy git push --tags
homeboy git push --force-with-lease
```

Bulk push is available through `--json`:

```sh
homeboy git push --json '{"component_ids":["homeboy","data-machine"],"tags":true}'
```

### Pull

```sh
homeboy git pull [component_id] [--path <path>]
```

Pulls remote changes for one checkout. Like `status`, `push`, and `commit`, the component ID is optional when CWD or `--path` can identify the component.

### Rebase

```sh
homeboy git rebase [component_id] [--onto <ref>] [--continue | --abort] [--path <path>]
```

Without `--onto`, `rebase` uses the current branch's tracked upstream (`@{upstream}`), matching `git pull --rebase` semantics. Use `--onto <ref>` to choose the target explicitly:

```sh
homeboy git rebase --onto origin/main
homeboy git rebase --continue
homeboy git rebase --abort
```

On conflict, Homeboy returns a failed result with git's stderr. Resolve conflicts manually, then re-run with `--continue` or `--abort`.

### Cherry-pick

```sh
homeboy git cherry-pick [refs...] [--pr <number>...] [--continue | --abort] [--path <path>]
```

`cherry-pick` accepts SHAs, branch names, ranges such as `<a>..<b>`, and repeatable `--pr <number>` flags. PR numbers are resolved with `gh pr view <n> --json commits`.

```sh
homeboy git cherry-pick abc1234 def5678
homeboy git cherry-pick feature-a..feature-b
homeboy git cherry-pick --pr 123 --pr 124
homeboy git cherry-pick --continue
homeboy git cherry-pick --abort
```

Use `-c, --component-id <id>` when running from outside the target checkout without `--path`.

### Tag

```sh
homeboy git tag [component_id] [tag_name] [-m <message>] [--path <path>]
```

If `tag_name` is omitted, Homeboy tags `v<component version>` from `homeboy version show`.

## GitHub Issue Workflows

```sh
homeboy git issue create <component_id> --title <title> [--body <body> | --body-file <path>] [--label <label>...]
homeboy git issue comment <component_id> --number <n> [--body <body> | --body-file <path>]
homeboy git issue find <component_id> [--title <title>] [--label <label>...] [--state open|closed|all] [--limit <n>]
homeboy git issue close <component_id> --number <n> [--reason completed|not-planned] [--comment <body> | --comment-file <path>]
homeboy git issue edit <component_id> --number <n> [--title <title>] [--body <body> | --body-file <path>] [--add-label <label>...] [--remove-label <label>...]
```

Issue commands require a component ID, but they also accept `--path` to discover component metadata from a portable checkout:

```sh
homeboy git issue find homeboy --state open --limit 10
homeboy git issue create homeboy --path /tmp/homeboy --title "docs: clarify git workflow" --body-file /tmp/body.md
```

Use `close --reason not-planned` for intentional wontfix decisions. Homeboy's reconciliation tooling treats GitHub's not-planned state as the durable signal not to re-file the issue.

## GitHub Pull Request Workflows

```sh
homeboy git pr create <component_id> --base <base> --head <head> --title <title> [--body <body> | --body-file <path>] [--draft]
homeboy git pr edit <component_id> --number <n> [--title <title>] [--body <body> | --body-file <path>]
homeboy git pr find <component_id> [--base <base>] [--head <head>] [--state open|closed|merged|all] [--limit <n>]
homeboy git pr comment <component_id> --number <n> [comment mode flags]
```

Like issue commands, PR commands accept `--path` to discover component metadata from a portable checkout:

```sh
homeboy git pr create homeboy --base main --head docs-refresh-git-command --title "docs: refresh git command workflows" --body-file /tmp/pr.md
homeboy git pr find homeboy --head docs-refresh-git-command --state open
```

### PR Comments

`homeboy git pr comment` supports three comment modes:

1. **Plain comment**: no marker flags. Homeboy appends a fresh comment.
2. **Sticky whole-body comment**: `--key <key>` finds or updates one comment tagged with `<!-- homeboy:key=<key> -->`. The provided body replaces the whole managed comment body.
3. **Sectioned managed comment**: `--comment-key <outer> --section-key <inner>` updates one section inside a shared comment tagged with `<!-- homeboy:comment-key=<outer> -->`. Other sections are preserved.

Sectioned comments are used by Homeboy Action to keep lint, test, audit, and tooling metadata in one managed PR comment without clobbering sibling sections:

```sh
homeboy git pr comment homeboy \
  --number 123 \
  --comment-key homeboy-ci-results \
  --section-key lint \
  --section-order lint,test,audit \
  --header "## Homeboy Results" \
  --footer-file /tmp/tooling.md \
  --body-file /tmp/lint.md
```

`--key` mode and `--comment-key` / `--section-key` mode are mutually exclusive. In sectioned mode, existing headers and footers are preserved when omitted; passing `--footer` or `--footer-file` replaces the stored footer.

## JSON Input Schemas

### SingleCommitSpec

```json
{
  "id": "homeboy",
  "message": "Update git docs",
  "staged_only": false,
  "include_files": ["docs/commands/git.md"]
}
```

Notes:

- `id` is optional when the component is supplied positionally or auto-detected from CWD.
- `staged_only` defaults to `false`.
- `include_files` stages only the listed paths.
- `exclude_files` stages all changes and then unstages the excluded paths.

### BulkCommitInput

```json
{
  "components": [
    { "id": "homeboy", "message": "Update git docs" },
    { "id": "data-machine", "message": "Update workflow docs" }
  ]
}
```

### BulkIdsInput

```json
{
  "component_ids": ["homeboy", "data-machine"],
  "tags": true,
  "force_with_lease": false
}
```

`tags` and `force_with_lease` are only used by `push`.

## JSON Output

> Note: command output is wrapped in the global JSON envelope described in the [JSON output contract](../architecture/output-system.md). The examples below show the `data` payload.

### Single Component Output

```json
{
  "component_id": "homeboy",
  "path": "/Users/chubes/Developer/homeboy@docs-refresh-git-command",
  "action": "status|commit|push|pull|tag|rebase|cherry-pick",
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
      "component_id": "homeboy",
      "path": "/path/to/homeboy",
      "action": "commit",
      "success": true,
      "exit_code": 0,
      "stdout": "[main abc1234] Update git docs\n 1 file changed",
      "stderr": ""
    }
  ],
  "summary": {
    "total": 1,
    "succeeded": 1,
    "failed": 0
  }
}
```

Notes:

- `commit` returns a successful result with `stdout` set to `Nothing to commit, working tree clean` when there are no changes.
- Bulk operations continue processing all components even if some fail; the summary reports total, succeeded, and failed counts.
- Bulk outputs are `BulkGitOutput { action, results, summary }`, where `results` is a list of `GitOutput` objects.

## Exit Code

- Single mode: exit code matches the underlying `git` or `gh` command.
- Bulk mode (`--json`): `0` if all components succeeded; `1` if any failed.

## Examples

```sh
# Auto-detect the component from the current checkout
homeboy git status
homeboy git commit -m "Update docs"
homeboy git push

# Operate on a worktree or CI checkout without changing CWD
homeboy git status --path /Users/chubes/Developer/homeboy@docs-refresh-git-command
homeboy git commit --path /Users/chubes/Developer/homeboy@docs-refresh-git-command -m "Update docs"

# Use explicit component IDs when operating outside a component checkout
homeboy git status homeboy
homeboy git pull homeboy
homeboy git tag homeboy v1.0.0 -m "Release 1.0.0"

# Rebase, then safely update the remote branch
homeboy git rebase --onto origin/main
homeboy git push --force-with-lease

# Pick commits by ref or by GitHub PR number
homeboy git cherry-pick abc1234
homeboy git cherry-pick --pr 123

# GitHub issue and PR helpers
homeboy git issue find homeboy --label audit --state open
homeboy git pr create homeboy --base main --head docs-refresh-git-command --title "docs: refresh git command workflows" --body-file /tmp/pr.md
homeboy git pr comment homeboy --number 123 --key docs-check --body-file /tmp/comment.md
```

## Related

- [stack](stack.md)
- [version](version.md)
