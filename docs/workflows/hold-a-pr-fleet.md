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

## 3. Run The Compact Landing Loop

When a fleet is close to landing, repeat this loop until every PR is either landed or has one concrete blocker. Keep the PR order explicit when there are dependent branches.

```bash
homeboy triage landing --repo owner/repo --ordered 123 124 125 --drilldown --output homeboy-results/landing.json
homeboy git pr readiness <component-id> --number 123 --output homeboy-results/123-readiness.json
homeboy git pr land owner/repo 123 124 125 --dry-run --output homeboy-results/land-dry-run.json
```

Use the landing snapshot first. It gives the fleet-wide view: PR state, check state, mergeability, review state, suggested next command, and matching local worktree records when Homeboy can find them.

Use readiness for the first PR you intend to merge next. It gives a narrow merge/no-merge answer without probing by attempting a merge.

Use `git pr land --dry-run` before any merge train. The dry run confirms the sequence Homeboy would attempt and where it would pause. Run without `--dry-run` only when the ready prefix is correct and you intend to merge those PRs.

Recommended next actions:

- `ready`: clean local worktree, pushed branch, non-draft PR, mergeable state, terminal green required checks, no blocking review decision. Include in the next `git pr land` ready prefix.
- `waiting`: pending or missing checks on the current head. Re-run `triage landing` or `git pr readiness` after checks report; do not merge based on stale proof.
- `refresh`: base is stale, mergeability is dirty/behind, or an ordered landing report shows a dependent branch needs a rebase after an earlier PR lands. Use `homeboy git pr refresh <component-id> <number-or-url> --strategy rebase --push` from a clean checkout.
- `local-cleanup`: landing output reports dirty or unpushed matching local worktrees. Inspect `homeboy worktree status <worktree-id>` and either commit/push, discard only intentional scratch changes, or stop and hand off the blocker.
- `blocked`: failed checks, requested changes, unresolved conflicts, unknown mergeability after retry, missing evidence, or an external dependency. Capture the blocker in the fleet handoff instead of leaving it implicit.

## 4. Refresh Evidence Only Where Needed

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

## 5. Classify Each Branch

Keep the handoff small by assigning one state per branch:

- `ready` - clean worktree, pushed branch, green checks, mergeable PR, current evidence.
- `blocked` - failing checks, requested changes, conflict, missing evidence, or an explicit issue/PR dependency.
- `needs-refresh` - stale base, unknown checks, outdated evidence, or missing latest run proof.
- `dirty-local` - uncommitted changes or unpushed commits in the worktree.
- `cleanup-candidate` - merged/closed branch with a safe worktree status.

A concise fleet handoff usually needs only branch, PR URL, local state, remote state, evidence ref, and next action.

Example handoff row:

| PR | Branch | Local | Remote | Evidence | Next action |
|----|--------|-------|--------|----------|-------------|
| `owner/repo#123` | `feature/a` | clean/pushed | mergeable/green | `run_...` | land first |
| `owner/repo#124` | `feature/b` | clean/pushed | behind after `#123` | `run_...` | refresh after `#123` lands |
| `owner/repo#125` | `feature/c` | dirty-local | checks failed | missing | fix blocker before sequencing |

## 6. Clean Up Deliberately

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
