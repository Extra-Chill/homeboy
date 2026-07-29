# Cleanup Retention Scope

`homeboy cleanup --include terminal-runs` is the lifecycle owner for terminal
observation records, and since #10316 it is the only surface for them: the
`homeboy runs retention` specialist carried no argument the aggregate could not
express and was deleted. Its dry-run output includes each candidate run, its
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

## Runner Download Cache Retention

```bash
homeboy cleanup --include runner-downloads
homeboy cleanup --include runner-downloads --apply
```

This category is **opt-in only**: a bare `homeboy cleanup --apply` does not
sweep it.

`<artifact-root>/runner` is produced by exactly one writer — the default output
path of `homeboy_lab_runner::evidence::download::download_remote_artifact`,
which lays bytes down as `<artifact-root>/runner/<runner-id>/<run-id>/<file>`.
Every reachable caller of it is a fetch someone asked for (`runs artifact get`,
`runs artifacts <run-id> --pull`, `lab apply`, evidence mirroring, the HTTP
artifact endpoint), and `runs artifact get` reports that path back as the
location of the operator's file. So this is not scratch; it is the operator's
copy.

Before #10564 the implementation was an unconditional `fs::remove_dir_all` of
the whole root. Its only checks were path-containment ones — `--run-id` requires
`--runner`, each filter must be a single normal path component, the root must be
a real directory — which prove the deletion stays *inside* the cache and prove
nothing about whether the bytes are dead. A bare sweep therefore deleted
artifacts pulled seconds earlier.

The predicate now requires all of:

- **Ownership by name shape.** A candidate must be a real directory at exactly
  `runner/<a>/<b>` with no symlink at either level — the only shape the single
  writer emits. Loose files, symlinks, and non-canonical depths are reported and
  never removed. As with orphaned artifact bytes, the database is deliberately
  not joined: bytes here are written before, and usually without, any local
  `artifacts` row, so row absence is the normal state of a download that is
  succeeding.
- **Age floor over the newest byte.** The newest modification time anywhere in
  the cache directory must be at least 24 hours old (the shared
  `cleanup::RUNNER_MIN_AGE_HOURS`). Taking the newest, not the oldest, is what
  makes one fresh pull re-arm the whole cache directory. The floor is not
  operator-overridable.
- **Non-terminal-run veto.** The observation store is consulted only in the
  *retain* direction. A running run matching the `<run-id>` component retains the
  cache; a missing row never authorizes removal.
- **Fail closed.** An unreadable or future-dated mtime, an unwalkable subtree, a
  path that does not resolve inside the artifact root, an observation store that
  cannot be opened, or a running-run scan that hit its bound all retain.

Removal is per cache directory, so a stale cache and a fresh one under the same
runner are decided independently; the cache root itself is never removed, and an
emptied `<runner-id>` directory is pruned only by a non-recursive `remove_dir`
that refuses a non-empty directory. Sizes are advisory: a failed measurement
reports `size_measured: false` and moves the verdict in neither direction.
`--runner` and `--run-id` narrow which candidates are considered; they never
waive a check.

The remaining gap, and the reason for opt-in: the writer emits the same name
shape for an operator's deliberate pull and for an internal auto-fetch, so
"reclaimable transient" and "the operator's copy" are not distinguishable by
name. Making them distinguishable requires the writer to tag its output.

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

## Retained Storage Accounting

`homeboy cleanup retained-storage` explains where disk went without deleting
anything. It accumulates read-only plans from the controller-runtime,
shared-Cargo-target, controller-scratch, and runtime-temp categories, from the
SQLite observation index, and — since #10316 — from the artifact root itself
through `persisted-run-artifacts`, `runner-downloads`, and
`orphaned-artifact-bytes`. Before that the one command whose purpose is "where
did my disk go" never called `artifacts::root()` at all.

Records carry a `liveness`, and `reclaimable` is reported separately from the
retained totals. "Cleanup cannot free this" and "cleanup has not freed this yet"
are different answers, so `retained_bytes` and `reclaimable_bytes` are never
summed together. `safe_next_commands` names a reclaim command for every category
the report can produce a record for.

The SQLite row is explicitly scoped to the index, not the artifact bytes it
indexes; the artifact payloads are accounted for by the artifact-root categories
and dwarf the database.

Protected persisted artifacts are summarized as one counted record rather than
one zero-byte row each: only rows classified for removal carry a measured size,
and an unmeasured size is reported as zero and never inferred.

The additive `filesystem` object is the root-accounting contract. It inventories
every top-level data-root entry, including `artifacts`, `cargo-targets`,
`controller-runtimes`, `controller-scratch`, and the SQLite store, and labels
unrecognized entries `unknown/unmanaged`. It also includes configured artifact
roots outside the data root as `managed/external`. `apparent_bytes` is the sum
of file lengths; `physical_bytes` is allocated filesystem blocks. Sparse files,
hard links shared across top-level stores, directory metadata, and allocation
granularity can make the top-level sum differ from root physical usage; the
report states the direction and byte difference rather than hiding it. Existing
`retained_bytes` and lifecycle aggregates remain unchanged for consumers.

## Remaining Scope

The following Issue #8648 portions remain independently owned and are not
implemented by terminal-run retention:

- Crash-orphaned `/tmp/hb-<uid>` invocation-root inventory and age-pruning.
- Reverse reconciliation for the subsystem-owned artifact-root subtrees listed
  above. Each needs its own lifecycle owner; none of them can be reclaimed by a
  row join.
- Controller-scratch index compaction for missing or deleted terminal tombstones.
- Aging removed task-worktree records out of workspace registries.
- Re-evaluating whether `runner-downloads` can rejoin the bare sweep now that
  the writer tags its output (#10585). The tag exists and cleanup reads it, but
  every cache directory predating the tag is untagged and therefore retained, so
  the default-sweep decision needs its own evidence after a backfill window.
- Detecting collisions between task-worktree and adopted-workspace registry handles.
