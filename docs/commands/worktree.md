# worktree

Manage component-backed task worktrees for generic Homeboy workflows.

Creation, resolution, listing, status, and cleanup use Homeboy's native
task-worktree registry. Homeboy creates each task worktree from its registered
local component checkout and records its lifecycle, owner run, and cleanup
policy in that registry.

## Commands

- `homeboy worktree create <component-id> --branch <branch> [--from <ref>] [--task-url <url>] [--run-id <id>]`
- `homeboy worktree list`
- `homeboy worktree status <id>`
- `homeboy worktree remove <id> [--force]`
- `homeboy worktree cleanup [--apply] [--force] [--cleanup-artifacts]`

For multi-PR orchestration, start with `homeboy --output homeboy-results/worktrees.json worktree list`, then pair each branch with `homeboy git pr readiness <component-id> --number <n>` and any recorded `homeboy runs evidence <run-id>` output. See [Hold a PR fleet](../workflows/hold-a-pr-fleet.md) for the full local-plus-remote handoff loop.

## Safety

Removal refuses dirty worktrees, unpushed commits, primary checkouts, and paths outside the component checkout parent. `--force` only bypasses dirty/unpushed checks; primary checkout and containment gates always apply.

### Orphan records

An active record is an *orphan* only when its worktree path **and** its recorded `source_checkout` no longer exist on disk (for example, a task worktree created from a since-deleted `/tmp` scratch clone). For an orphan:

- `homeboy worktree status <id>` reports the record instead of failing: the safety report has `worktree_missing: true` and `source_checkout_missing: true`, with reasons naming the remediation command.
- `homeboy worktree remove <id>` retires the record as a metadata-only operation: it marks the record `removed` and appends a reaping audit entry. Nothing exists to delete, so it touches no filesystem path and inspects or deletes no branch in any other checkout (even with `--cleanup-branch`). A live owner lease still refuses removal.
- A new `worktree create` for the same handle that fails identity verification against the orphan names `homeboy worktree remove <id>` as the remediation.

If either the worktree directory or the source checkout still exists, the record is not an orphan and every gate above still applies.

`worktree cleanup` reports a task-worktree cleanup plan by default. Pass `--apply` to remove planned worktrees after the existing safety gates pass. `--dry-run` names the plan-only default explicitly, as on every plan-default mutation command; it conflicts with `--apply`.

Use `homeboy cleanup artifacts` to plan rebuildable artifact cleanup, then pass `--apply` when the plan is acceptable. `worktree cleanup --cleanup-artifacts` includes declared rebuildable artifacts from the Homeboy checkout that built the active binary; it remains plan-only unless `--apply` is also passed.
