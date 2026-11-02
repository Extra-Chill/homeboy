## Summary
Cook could reject an already-converged issue destination as existing PR work after a transport failure pinned a stale tracking ref. Capturing the remote base now refreshes that tracking ref alongside the SHA fetch, so subsequent admission retains the converged base.

## What changed
- Fetch the verified remote SHA into `refs/remotes/origin/<base>` under the existing remote-tracking authority lock.
- Preserve immutable SHA selection and avoid writing FETCH_HEAD.
- Add a hermetic linked-worktree regression covering remote advancement, SHA materialization, and readmission during a transport failure.
- Retain existing protection that routes genuine unique destination commits to PR rework.

## Reproduction
Run `cargo test -p homeboy-agents cook_workspace_base_readmission_after_sha_fetch_converges`.
Before the fix, the fixture left origin/main at T1 while the destination reached remote T2. Readmission pinned T1 through `local_tracking_cook_base`, then `converge_destination` returned `pr_route(branch, None)` from its `issue_owned && ahead != 0` branch with “existing PR work.” After the fix, the same test passes: capture updates the tracking ref to T2 and readmission converges without routing to PR rework.

## Compatibility
Additive correction to remote-tracking freshness; no new fallback or compatibility path. Open-PR and unique-commit routing remain unchanged.

## Used for
AI inspected the issue and code, reproduced the stale-pin failure, implemented the tracking-ref correction, and ran regression checks.

Closes #16969
