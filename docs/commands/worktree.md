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

For multi-PR orchestration, start with `homeboy --output /tmp/homeboy-results/worktrees.json worktree list`, then pair each branch with `homeboy git pr readiness <component-id> --number <n>` and any recorded `homeboy runs evidence <run-id>` output. See [Hold a PR fleet](../workflows/hold-a-pr-fleet.md) for the full local-plus-remote handoff loop.

## Safety

Removal refuses dirty worktrees, unpushed commits, primary checkouts, and paths outside the component checkout parent. `--force` only bypasses dirty/unpushed checks; primary checkout and containment gates always apply.

### Unrecorded linked worktrees

`status <path>` and `remove <path>` also accept linked Git worktrees absent from this host's registry. Status classifies these as `orphan_unrecorded_worktree` in its disposition and safety evidence, not as active task ownership. Paths belonging to an existing active record resolve to that record instead.

Removal requires a clean tree (including untracked, non-ignored work), no stash created on that branch or at HEAD, no active Cargo build, and commits preserved upstream. Preservation means HEAD is reachable from a remote-tracking ref, matches a merged GitHub PR's head, or its content is contained in the remote default branch (including squash/rebase merges). An unavailable preservation probe retains the worktree. `--force` never bypasses orphan checks. Primary checkouts, subdirectory handles, paths outside the checkout parent, and the caller's current directory remain protected. Removal deletes the whole worktree, including ignored `target/` output, but preserves the local branch even with `--cleanup-branch`; no registry manifest is invented.

Cleanup discovers unrecorded worktrees through registered and recorded source checkouts and offers those that pass the same orphan checks. Paged cleanup inspects these after the final registry page. The orphan pass has a fifteen-second budget and individual safety probes have a five-second bound; rerun cleanup if its continuation is returned. Orphan removal always preserves local branches.

Reserve refusals include bounded orphan size estimates (including build output) and an inventory action. Partial estimates are explicitly labeled as lower bounds. Recorded cleanup emits inspection progress to stderr and bounds each worktree's Git inspection to ten seconds, further limited by any page deadline.

### Orphan records

An active record is an *orphan* only when its worktree path **and** its recorded `source_checkout` no longer exist on disk (for example, a task worktree created from a since-deleted `/tmp` scratch clone). For an orphan:

- `homeboy worktree status <id>` reports the record instead of failing: the safety report has `worktree_missing: true` and `source_checkout_missing: true`, with reasons naming the remediation command.
- `homeboy worktree remove <id>` retires the record as a metadata-only operation: it marks the record `removed` and appends a reaping audit entry. Nothing exists to delete, so it touches no filesystem path and inspects or deletes no branch in any other checkout (even with `--cleanup-branch`). A live owner lease still refuses removal.
- A new `worktree create` for the same handle that fails identity verification against the orphan names `homeboy worktree remove <id>` as the remediation.

If the worktree directory still exists, every removal gate above still applies. Cleanup also reconciles active records whose worktree path is gone, even when their source checkout remains: apply retires metadata under the registry lock and workspace claim fence, refuses live owner leases, and preserves the local branch without requiring lifecycle-owner finalization. Plan mode lists these as candidates rather than inventory reconciliation blockers.

`worktree cleanup` reports a task-worktree cleanup plan by default. Pass `--apply` to remove planned worktrees after the existing safety gates pass. `--dry-run` names the plan-only default explicitly, as on every plan-default mutation command; it conflicts with `--apply`. Cleanup nominates every worktree plain `worktree remove` would accept without `--force`: a branch merged into the cleanup base ref (including GitHub squash/rebase merges, via the shared content-containment probe) is terminal evidence on its own, so a merged, clean, unclaimed worktree is nominated whether or not its lifecycle owner recorded a succeeded finalization. Unmerged run-owned work still waits for that explicit finalization, and dirty or claimed worktrees stay retained.

Use `homeboy cleanup artifacts` to plan rebuildable artifact cleanup, then pass `--apply` when the plan is acceptable. `worktree cleanup --cleanup-artifacts` includes declared rebuildable artifacts from the Homeboy checkout that built the active binary; it remains plan-only unless `--apply` is also passed.
