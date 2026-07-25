# Cleanup Retention Scope

`homeboy cleanup --include terminal-runs` is the lifecycle owner for terminal
observation records. Its dry-run output includes each candidate run, its
registered persisted-artifact cleanup plan, and any agent-task lifecycle
directory. Apply revalidates local artifact paths, removes eligible artifact
bytes and lifecycle directories, then removes the terminal database records.
Unsafe existing local artifact paths keep the run and its lifecycle directory.

The existing cleanup inventory remains the only planner. This change does not
add a second cleanup engine.

## Lab Failure Retention

Lab offloads delete run-scoped workspaces on every known terminal outcome by
default. `--preserve-workspace-on-failure` is the bounded debugging profile: it
keeps failed or cancelled materialization state, registers it as
`delete_after_ttl` in the workspace lifecycle metadata, and uses
`lab.runner_workspace_ttl` (default `P7D`) for existing runner workspace
pruning. The terminal report identifies the policy, outcome, lifecycle owner,
retained location, and `homeboy runner workspace prune <runner> --apply
--min-age-hours 0` reclaim command.

Detached, in-flight, and otherwise uncertain daemon ownership always
relinquishes the local cleanup handle. Those paths remain fail-closed and are
never treated as terminal deletion or debug-retention outcomes.

## Remaining Scope

The following Issue #8648 portions remain independently owned and are not
implemented by terminal-run retention:

- Crash-orphaned `/tmp/hb-<uid>` invocation-root inventory and age-pruning.
- Controller-scratch index compaction for missing or deleted terminal tombstones.
- Aging removed task-worktree records out of workspace registries.
- Detecting collisions between task-worktree and adopted-workspace registry handles.
