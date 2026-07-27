# worktree

Manage component-backed task worktrees for generic Homeboy workflows.

## Commands

- `homeboy worktree create <component-id> --branch <branch> [--from <ref>] [--task-url <url>] [--run-id <id>]`
- `homeboy worktree list`
- `homeboy worktree status <id>`
- `homeboy worktree remove <id> [--force]`
- `homeboy worktree cleanup [--apply] [--force] [--cleanup-artifacts]`

For multi-PR orchestration, start with `homeboy --output homeboy-results/worktrees.json worktree list`, then pair each branch with `homeboy triage landing 'owner/repo#number' --drilldown` and any recorded `homeboy runs evidence <run-id>` output. See [Hold a PR fleet](../workflows/hold-a-pr-fleet.md) for the full local-plus-remote handoff loop.

## Safety

Removal refuses dirty worktrees, unpushed commits, primary checkouts, and paths outside the component checkout parent. `--force` only bypasses dirty/unpushed checks; primary checkout and containment gates always apply.

`worktree cleanup` reports a task-worktree cleanup plan by default. Pass `--apply` to remove planned worktrees after the existing safety gates pass. `--dry-run` remains a deprecated plan-only alias for one release; it conflicts with `--apply`.

Use `homeboy cleanup artifacts` to plan rebuildable artifact cleanup, then pass `--apply` when the plan is acceptable. `worktree cleanup --cleanup-artifacts` includes declared rebuildable artifacts from the Homeboy checkout that built the active binary; it remains plan-only unless `--apply` is also passed.
