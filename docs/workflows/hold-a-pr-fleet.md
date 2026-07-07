# Hold A PR Fleet

Use worktree, triage, review, and runs commands when you are holding many PR branches at once and need a concise operator view. This workflow is for local PR-fleet orchestration before merge or cleanup, not deployment.

## Use This When

- Many branches are open across one or more repos.
- You need local worktree state and remote PR state in the same handoff.
- Reviewers need links to tests, run artifacts, or blockers without reading shell scrollback.
- You need to decide which branches are ready, blocked, stale, dirty, or safe to clean up.

## 1. Start With The Local Fleet Ledger

List the worktrees before checking GitHub. The list output is the local source of truth for branch names, task URLs, run IDs, cleanup policy, and whether a worktree still exists.

```bash
homeboy worktree list
homeboy --output homeboy-results/worktrees.json worktree list
```

For each branch that looks stale, dirty, or ready to remove, inspect the safety report instead of guessing from directory names:

```bash
homeboy worktree status <worktree-id>
```

Use the JSON output as the orchestration ledger. The useful fleet-holding fields are:

- `id` - stable local worktree handle.
- `component_id` - repo/component owner for the branch.
- `worktree_path` - local checkout path for targeted verification.
- `branch` - branch to match against PR heads.
- `task_url` - issue, PR, or tracker URL attached at creation time.
- `run_id` - persisted Homeboy run that produced or verified the branch.
- `cleanup_policy` and `state` - cleanup intent and lifecycle state.

## 2. Check Remote PR Landing State

Use `triage landing` when you have explicit PR refs. It classifies mergeability, checks, draft state, and review blockers in one read-only dashboard.

```bash
homeboy triage landing \
  'owner/repo#123' \
  'owner/repo#124' \
  --drilldown
```

For one repo, bare numbers are acceptable when the repo is explicit:

```bash
homeboy triage landing --repo owner/repo 123 124 --drilldown
```

Persist the remote dashboard next to the local worktree ledger:

```bash
homeboy --output homeboy-results/landing.json triage landing --repo owner/repo --drilldown
```

Use `triage --watch` only for a small number of PRs that are actively moving. The snapshot commands above are better for broad fleet handoffs.

## 3. Refresh Evidence Only Where Needed

Run review from the worktree path so the evidence matches the branch being held:

```bash
homeboy review <component-id> --path <worktree-path> --changed-since origin/main --summary
homeboy --output homeboy-results/<branch>-review.json review <component-id> --path <worktree-path> --changed-since origin/main --summary
```

When a branch has a recorded `run_id`, read the stable proof and evidence registry before rerunning expensive checks:

```bash
homeboy runs proof <run-id>
homeboy runs evidence <run-id>
homeboy runs artifacts <run-id>
```

Attach or link the durable artifact/evidence refs in the PR. Treat local-only paths as operator notes, not reviewer-facing evidence.

## 4. Classify Each Branch

Keep the handoff small by assigning one state per branch:

- `ready` - clean worktree, pushed branch, green checks, mergeable PR, current evidence.
- `blocked` - failing checks, requested changes, conflict, missing evidence, or an explicit issue/PR dependency.
- `needs-refresh` - stale base, unknown checks, outdated evidence, or missing latest run proof.
- `dirty-local` - uncommitted changes or unpushed commits in the worktree.
- `cleanup-candidate` - merged/closed branch with a safe worktree status.

A concise fleet handoff usually needs only branch, PR URL, local state, remote state, evidence ref, and next action.

## 5. Clean Up Deliberately

Preview cleanup first:

```bash
homeboy worktree cleanup --dry-run
```

Remove one safe worktree when the branch is merged or no longer needed:

```bash
homeboy worktree remove <worktree-id> --cleanup-branch
```

Use `--force` and unmerged branch cleanup flags only as explicit operator decisions after reading the safety report.

## Reference

- [worktree command](../commands/worktree.md)
- [triage command](../commands/triage.md)
- [review command](../commands/review.md)
- [runs command](../commands/runs.md)
- [Capture evidence](capture-evidence.md)
