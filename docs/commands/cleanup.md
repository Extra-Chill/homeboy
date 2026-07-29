# cleanup

Remove or inspect reconstructable artifacts that Homeboy can safely recreate.

This is the canonical artifact cleanup path. Worktree lifecycle cleanup is handled by `homeboy worktree cleanup`; artifact removal stays dry-run here until `--apply` is passed.

## `homeboy cleanup artifacts`

Scans the current repository and its managed Git worktrees for built-in and declared artifact paths. The command defaults to dry-run output and only removes files when `--apply` is passed.

Homeboy always treats Rust `target` directories as rebuildable artifacts. Projects can add repo-relative cleanup paths with `artifact_cleanup_paths` in `homeboy.json`.

```bash
homeboy cleanup artifacts
homeboy cleanup artifacts --path /path/to/checkout
homeboy cleanup artifacts --sort size --limit 10
homeboy cleanup artifacts --merged-only --sort size --limit 10
homeboy cleanup artifacts --min-age-days 7
homeboy cleanup artifacts --apply
```

Use `--sort size` to review the largest artifacts first, `--limit N` to bound the reported or removed candidates after sorting, and `--merged-only` to preserve artifacts from worktrees whose branch is not merged into its upstream.

The JSON output includes worktree identity, candidate paths, estimated bytes, skipped reasons, applied rows, a per-worktree `worktrees` roll-up, and a `summary` object. The terminal summary shows bounded candidate rows and points to the JSON output for full large reviews. `summary.invocation_reclaimed_bytes` reports bytes reclaimed by the current command, `summary.remaining_candidate_bytes` reports cleanup candidates still present after the command, and `summary.cumulative_session_reclaimed_bytes` carries the local cumulative total for repeated `--apply` runs against the same repository. Cleanup refuses unsafe path declarations and skips artifact paths that contain tracked or staged source changes, files Git tracks at all (a repository that commits its generated output keeps it), or untracked work that Git does not ignore.

Every candidate reports both `size_bytes` (apparent content size) and `allocated_bytes` (disk actually charged to the tree). Large trees of small files diverge sharply between the two; `allocated_bytes` is what free space reflects after `--apply`.

### Extension-declared artifacts

Installed extensions declare the reconstructable install/build trees they own through `artifact_cleanup` in their manifest. Homeboy resolves those declarations against every managed worktree and applies the same safety policy it applies to built-in paths — nothing about a toolchain is hardcoded in Homeboy.

- Declarations resolve only beside install scopes the extension supports. A scope is a directory carrying the manifest files the declaration names, so an artifact path is a candidate only where that ecosystem actually installs. Nested discovery is depth-bounded and never descends into a declared artifact tree.
- Candidates report `declared_by` (the owning extension), `category`, `readiness`, `liveness`, and `rehydrate_command`. The per-worktree roll-up lists the deduplicated commands that restore each checkout.
- Declarations in category `release_asset` are inventoried and never removed, so packaged output needed for deployment stays distinguishable from development output.
- Checkouts registered as active task worktrees are protected from extension-declared removal by default, because losing an install tree leaves a live checkout unusable until it is rehydrated. Pass `--include-active-worktrees` to reclaim them anyway. Built-in and `homeboy.json` declarations keep their existing behavior.
- `--min-age-days N` requires an artifact to be untouched for N days. A declaration can set its own floor; the stricter of the two applies.

Configured worktree-provider previews run independently with a 30-second cancellation boundary. The output records each provider's typed `outcome`, `inventory_completeness`, elapsed time, and heartbeat count, so a timed-out or failed provider yields a partial inventory without blocking healthy providers. Preview commands remain the only provider operation invoked without `--apply`.

Regular `homeboy cleanup` includes `repo-artifacts` inventory. After an agent-task provider exits, Homeboy also cleans declared rebuildable artifacts from that exact detached attempt worktree before applying its existing source-state and commit safety guard. Active sibling worktrees are not scanned by this lifecycle step.

## Orphaned Artifact Bytes

```bash
homeboy cleanup --include orphaned-artifact-bytes
homeboy cleanup --include orphaned-artifact-bytes --apply
```

Reclaims crash residue under the artifact root that no `artifacts` row can
describe: staging siblings left behind when a publish is SIGKILLed, and
patch-capture baseline copies whose `Drop` never ran. It is name-scoped rather
than row-scoped on purpose — the artifact root is a shared namespace and
artifact bytes are written before their row exists, so row absence is not
evidence of an orphan. See `docs/cleanup-retention.md` for the full safety
model.

## Shared Cargo Targets

Homeboy-managed Cargo builds use shared stores below its local data directory. Inspect them through the normal cleanup inventory:

```bash
homeboy cleanup --include shared-cargo-targets
homeboy cleanup --include shared-cargo-targets --apply
```

`retention.shared_store_days` defaults to `30`, `retention.shared_store_max_bytes` defaults to `21474836480` (20 GiB), and `retention.shared_store_lease_seconds` defaults to `21600` (6 hours). The age and size budgets select rebuildable stores; the lease window independently protects active workloads. The output's `storage` object records the resolved root, backing filesystem, free bytes/inodes, reserves, managed bytes, protected bytes, and cleanup command. Configure a dedicated root with `cargo_target_root` or `HOMEBOY_CARGO_TARGET_ROOT`; when the root moves, `storage.legacy_discovery_command` explicitly inventories the historical store rather than silently orphaning it. Inventory output is bounded by `retention.limit`; when `next_command` is present, run it to continue from `next_cursor`.

## Runtime Temp

Runtime temp cleanup defaults to `retention.runtime_tmp_days`. Under disk
pressure, an operator can narrow the age floor for entries carrying Homeboy's
owner metadata without making unknown temp directories eligible:

```bash
homeboy cleanup --include runtime-tmp --runtime-tmp-managed-older-than-days 1
homeboy cleanup --include runtime-tmp --runtime-tmp-managed-older-than-days 1 --apply
```

The managed override composes with the existing process identity, pin,
quarantine, path, count, byte, lock, and apply-time revalidation checks.
Unmanaged entries continue to use `retention.runtime_tmp_days`. Structured
output reports these effective floors separately as `runtime_tmp_days` and
`runtime_tmp_managed_days`.

## Automatic Retention

Declare the bounded retention pass through Homeboy's scheduler:

```bash
homeboy schedule add automatic-retention \
  --command "cleanup automatic-retention" \
  --every 1h \
  --on-overlap skip
```

The schedule declaration is the explicit opt-in to unattended mutation. The Homeboy daemon owns cadence, overlap prevention, and stale-run recovery; no external timer is needed. Each pass first reconciles stale agent-task records, then applies the existing terminal-run, persisted-artifact, orphaned-byte, runtime-temp, controller-scratch, controller-runtime, and shared-Cargo cleanup policies. Runner downloads, worktrees, and external providers remain outside unattended scope. `retention.limit` caps each category; Cargo retains its configured aggregate byte budget and active-lease predicate. The controller has a process and cross-process single-flight lock, writes combined pass evidence under the Homeboy data directory, and returns `homeboy cleanup automatic-retention` as the exact resume command.

## Runner Binary Caches

Runner refresh and dev-sync create managed Homeboy binary slots below each runner workspace root. Inventory or reclaim stale slots through the aggregate cleanup surface:

```bash
homeboy cleanup --include runner-binary-caches
homeboy cleanup --include runner-binary-caches --apply
```

The aggregate emits one category per configured runner and uses direct local or SSH execution, so a disconnected runner daemon does not block cleanup. The specialist command is `homeboy runner cache-prune <runner> [--apply]`. Slots must be at least 24 hours old. The configured binary, process-owned slots, symlinks, malformed or interrupted layouts, and entries that change between inventory and apply are preserved.

## One retention policy, many entry points

Cleanup is reachable through the aggregate planner (`homeboy cleanup --include
<category>`) and through a set of category specialists. Every one of them is a
delete path, so every one of them resolves its retention window through a single
policy — `homeboy_core::cleanup::resolve_cleanup_policy`. The resolved policy is
echoed as `retention` in the JSON output of the aggregate and of each specialist
that has one, so a report can never describe a window the deletion did not
apply.

| Category | Specialist | What it deletes | Ownership proof | Age / liveness rule |
| --- | --- | --- | --- | --- |
| `repo-artifacts` | `homeboy cleanup artifacts` | Declared reconstructable build/install trees in repo worktrees | Built-in path table plus repo/extension declarations, resolved only beside a matching install scope | `--min-age-days` composed with any declaration floor (stricter wins); Git-tracked or dirty trees and active task worktrees preserved |
| `task-worktrees` | `homeboy worktree cleanup --cleanup-branches` | Registered task worktrees and their branches | Worktree registry membership | Unmerged branches preserved unless explicitly allowed |
| `worktree-providers` | `homeboy cleanup worktrees --all-providers` | Provider-owned external worktrees | Delegated to each configured provider | Provider-owned; a timed-out provider yields a partial inventory and blocks nothing |
| `terminal-runs` | — (aggregate only) | Terminal observation records, their artifact bytes, and lifecycle directories | Durable run row in a terminal state | `retention.terminal_run_days`; unsafe local artifact paths keep the run |
| `persisted-run-artifacts` | `homeboy runs artifact cleanup-persisted` | Persisted artifact files/directories and their DB rows | `artifacts` row joined to a terminal run | `retention.terminal_run_days`; active/unknown run state, non-local bytes, out-of-root paths, and symlinks are skipped |
| `orphaned-artifact-bytes` | — (aggregate only) | Two crash-residue name families under the artifact root | Name shape from a single private constructor plus a parsed UUID; the database is deliberately **not** consulted | Fixed 24h floor, not operator-overridable; a failed size measurement changes the verdict in neither direction |
| `runner-downloads` (opt-in only) | `homeboy runs artifact cleanup-downloads` | Cache directories under `<artifact-root>/runner` | Canonical `<runner-id>/<run-id>` shape emitted by the single writer, **plus an explicit `internal_fetch` intent marker**; the database is deliberately **not** joined | Fixed 24h floor over the *newest* byte in the cache directory, plus a non-terminal-run veto; an absent, unreadable, or `operator_pull` marker retains, as does any unreadable state. Narrowed by `--runner`/`--run-id`, which never waive the predicate |
| `runner-binary-caches` | `homeboy runner cache-prune <runner>` | Unselected managed Homeboy binary slots on a runner | Canonical `homeboy-*` / `dev/<16-hex>` slot layout with a regular expected binary | 24h floor; configured binary, process-owned slots, symlinks, and malformed layouts preserved; selection revalidated immediately before removal |
| `remote-lab-workspaces` | `homeboy runner workspace prune <runner>` | Orphaned runner-side Lab workspaces | `homeboy/runner-workspace/v1` metadata plus a resolvable `local_path`; never outside `_lab_workspaces`. A workspace is also reachable when its *exact* durable owner run is terminal and its lease is `delete_on_success` — an existing controller-side source path is not evidence the runner copy is live | 24h floor; pending apply-back or an unexpired lifecycle TTL preserves the workspace. Live, unavailable, ambiguous, or malformed run authority all retain |
| `runtime-tmp` | `homeboy self cleanup-runtime-tmp` | Orphaned Homeboy runtime temp entries | Owner id recorded in the entry | `retention.runtime_tmp_days` plus byte/count budgets; entries whose owner process is running are preserved |
| `controller-scratch` | — (aggregate only) | Released controller scratch resources, including ephemeral attempt Git worktrees | Scratch index ownership with pid liveness; a linked worktree is proved by Git's own two-way `.git`/`gitdir` pointers, never by a database join | Per-resource retention window (P7D) unless `--older-than-days` is typed. A linked attempt worktree is additionally retained unless every commit reachable from its HEAD is already reachable from a branch, tag, or remote-tracking ref in its source repository. Removal goes through `git worktree remove`; a worktree that cannot be unregistered is reported and retained, never deleted behind Git |
| `shared-cargo-targets` | — (aggregate only) | Shared Cargo target stores | Store layout below Homeboy's data directory | `retention.shared_store_days` and byte budget; an unexpired lease preserves the store independently |
| `controller-runtimes` | `homeboy runtime controller-prune` | Unreferenced immutable controller runtime identities | Content-addressed pin path not referenced by a nonterminal durable record or the active generation, under the admission lock | `retention.controller_runtime_days` and byte budget; `--ignore-retention` is the explicit destructive opt-out |

### Why the specialists still exist

A specialist survives only when it accepts a *narrowing* argument the aggregate
cannot express, or when it is the operator escape hatch for a policy the
aggregate deliberately never applies:

- `cleanup artifacts` — `--path`, `--self`, `--temp-root`, `--sort`,
  `--merged-only`, `--min-age-days`, `--include-active-worktrees`.
- `cleanup worktrees` — `--provider` selection.
- `runs artifact cleanup-persisted` — `--run-id`, `--kind`, `--type`,
  `--run-kind`, `--component`.
- `runs artifact cleanup-downloads` — `--runner`, `--run-id`.
- `runner workspace prune` / `runner cache-prune` — per-runner targeting plus
  `--passes`/`--cursor` pagination against a single remote host.
- `self cleanup-runtime-tmp` — `--prefix`.
- `runtime controller-prune` — `--ignore-retention`, the explicit destructive
  purge the aggregate never offers.
`homeboy runs retention` had nothing on that list — its `--apply`,
`--older-than-days`, and `--limit` were exactly the aggregate's — so it was
deleted. `homeboy cleanup --include terminal-runs` is the only surface for
terminal observation-record retention.

What is *not* a reason for a specialist to exist is a retention window. Those all
resolve through the shared policy now.

### Fail-closed rules

- An unset flag means "use the configured value", never a command-local literal.
- `--runtime-tmp-managed-older-than-days` narrows only metadata-backed runtime
  temp entries. Unmanaged entries keep `retention.runtime_tmp_days`, so an age
  override never turns unknown ownership into deletion authority.
- A negative window or non-positive limit is rejected on every entry point,
  including when it arrives from configuration rather than from an argument.
- A record budget that cannot be represented resolves to zero inspected records,
  never to an unbounded sweep.
- Sizes and liveness probes are advisory. They rank and explain; they never
  prove ownership, and a failed measurement moves a verdict in neither
  direction.
- `terminal_only` on persisted-artifact cleanup is not operator-overridable:
  releasing evidence for a run that is still executing, or whose state cannot be
  read, is data loss rather than a retention preference.
- The `runner-downloads` age floor is likewise not operator-overridable, and the
  category is excluded from the bare sweep (below).

## Why `runner-downloads` is opt-in only

A bare `homeboy cleanup --apply` sweeps every category *except*
`runner-downloads`. Pass `--include runner-downloads` to reach it.

Every other category reclaims bytes Homeboy produced as a byproduct of its own
work: build targets, scratch, temp trees, crash residue, remote workspaces.
`<artifact-root>/runner` is different in kind. One writer produces the whole
tree — the default output path of
`homeboy_lab_runner::evidence::download::download_remote_artifact` — and every
caller of it is a fetch someone asked for: `homeboy runs artifact get`, `homeboy
runs artifacts <run-id> --pull`, `lab apply`, evidence mirroring, and the HTTP
artifact endpoint. `runs artifact get` then hands that exact path back as the
location of the operator's file.

Before #10564 this category was an unconditional `rm -rf` of the whole root with
no age floor, no liveness check, and no ownership proof of the contents, so a
bare sweep deleted artifacts an operator had pulled seconds earlier. The
predicate above fixes the acute data-loss case.

The writer now also records *why* each cache directory exists (#10585). It
writes a `.homeboy-download.json` sidecar inside the cache directory holding the
strongest claim made on it, and cleanup reads it:

| marker | verdict |
| --- | --- |
| absent | retain — untagged bytes are treated as operator-owned |
| unreadable or unparseable | retain |
| `operator_pull` | retain |
| `internal_fetch` | eligible; the age floor and the liveness veto still decide |

Only an explicit `internal_fetch` relaxes anything, and operator ownership is
sticky: a cache directory that has served one operator pull never downgrades. In
practice this means **every cache directory written before this change is
retained**, because none of them carries a marker. That is deliberate — the
alternative is inferring intent from bytes that never recorded it — and it means
`--include runner-downloads` reclaims nothing until internal fetches have been
re-run against the tagging writer. The bytes stay visible in
`cleanup retained-storage` and in the per-row `intent` field the whole time.

The category stays out of the bare sweep for now. Re-evaluating that is a
separate decision with its own evidence, once the backfill window has passed;
being absent from a default sweep is cheap and reversible, and a wrong delete is
neither.

The bytes stay fully visible: `homeboy cleanup retained-storage` accounts for
them in two halves (what a sweep would reclaim, and what it is holding on to)
and names `homeboy cleanup --include runner-downloads` as the reclaim command.
