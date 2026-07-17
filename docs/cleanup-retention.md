# Cleanup Retention Scope

`homeboy cleanup --include terminal-runs` is the lifecycle owner for terminal
observation records. Its dry-run output includes each candidate run, its
registered persisted-artifact cleanup plan, and any agent-task lifecycle
directory. Apply revalidates local artifact paths, removes eligible artifact
bytes and lifecycle directories, then removes the terminal database records.
Unsafe existing local artifact paths keep the run and its lifecycle directory.

The existing cleanup inventory remains the only planner. This change does not
add a second cleanup engine.

## Remaining Scope

The following Issue #8648 portions remain independently owned and are not
implemented by terminal-run retention:

- Crash-orphaned `/tmp/hb-<uid>` invocation-root inventory and age-pruning.
- Controller-scratch index compaction for missing or deleted terminal tombstones.
- Aging removed task-worktree records out of workspace registries.
- Detecting collisions between task-worktree and adopted-workspace registry handles.
