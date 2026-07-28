# Cleanup Retention Scope

`homeboy cleanup --include terminal-runs` is the lifecycle owner for terminal
observation records. Its dry-run output includes each candidate run, its
registered persisted-artifact cleanup plan, and any agent-task lifecycle
directory. Apply revalidates local artifact paths, removes eligible artifact
bytes and lifecycle directories, then removes the terminal database records.
Unsafe existing local artifact paths keep the run and its lifecycle directory.

The existing cleanup inventory remains the only planner. This change does not
add a second cleanup engine.

Retention resolves the observation store and the artifact root once per sweep.
Both used to be re-derived inside every per-run artifact plan, so a default
`--limit 1000` sweep opened thousands of SQLite connections and re-ran the
migration ladder for each one. The apply pass still re-plans each run
immediately before deleting its bytes: the planning loop walks every candidate
before any deletion happens, so the earliest plans are already stale by then.

## Orphaned Artifact Bytes

```bash
homeboy cleanup --include orphaned-artifact-bytes
homeboy cleanup --include orphaned-artifact-bytes --apply
```

Every other artifact-root cleanup surface is driven by `artifacts` rows, so
bytes written before their row existed are structurally invisible to all of
them. This category reclaims the two artifact-root path families that are
created outside any durable journal and are otherwise only reclaimed by an
in-process cleanup branch, which SIGKILL and OOM skip:

- `<artifact_root>/<run_id>/.artifact-<uuid>.staging` — the staging sibling
  written before a file artifact is hard-linked into place.
- `<artifact_root>/_scratch/patch-<label>-<uuid>/` — the working-tree baseline
  copy taken by daemon patch capture, reclaimed only by `impl Drop`.

It is deliberately **not** a generic "filesystem path with no database row"
reaper. The artifact root is a shared namespace: `runner/`, `runner-attach/`,
`runner-exec-attach/`, `agent-task/`, `agent-task-loop-controller/`,
`controller-scratch-recovery/`, `recovered-runner-artifacts/`,
`executor-finalized/`, `preview-consumer/`, and `_scratch/` are all owned by
other subsystems and carry no artifact row at those paths. Artifact bytes are
also written *before* their row is inserted by design, so row absence is the
normal state of a publication that is currently succeeding. Row absence
therefore proves nothing, and the database is not consulted.

Ownership is instead proven by name shape — both families come from a single
private constructor whose format no other writer emits — plus a fixed 24-hour
age floor covering the in-flight window. The floor is not operator-overridable.
Anything else at those paths, including a lookalike name without a parseable
UUID, a staging-shaped directory, a scratch-shaped file, or a symlink, is
reported and kept.

Reported sizes are advisory. Removal is a pure function of name shape, entry
type, age, symlink-freedom, and root containment; a failed size measurement
reports `size_measured: false` and does not change the verdict in either
direction.

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

## Runner Binary Cache Retention

`homeboy cleanup --include runner-binary-caches` composes the runner-owned
binary cache lifecycle into the aggregate planner. Each configured runner is
inventoried independently through direct local or SSH execution; an unavailable
runner becomes a scoped skipped category and does not abort healthy runners.

Only canonical refresh (`homeboy-*`) and dev-sync (`dev/<16-hex>`) slots with a
regular expected binary are eligible after the fixed 24-hour age floor. The
configured `homeboy_path`, slots with open files or process working directories,
symlinks, malformed or partial layouts, and candidates that change identity are
retained. Apply revalidates selection, inode identity, layout, symlink state,
and process ownership immediately before removal.

## Remaining Scope

The following Issue #8648 portions remain independently owned and are not
implemented by terminal-run retention:

- Crash-orphaned `/tmp/hb-<uid>` invocation-root inventory and age-pruning.
- Reverse reconciliation for the subsystem-owned artifact-root subtrees listed
  above. Each needs its own lifecycle owner; none of them can be reclaimed by a
  row join.
- Controller-scratch index compaction for missing or deleted terminal tombstones.
- Aging removed task-worktree records out of workspace registries.
- Detecting collisions between task-worktree and adopted-workspace registry handles.
