# cleanup

Remove or inspect reconstructable artifacts that Homeboy can safely recreate.

This is the canonical artifact cleanup path. Worktree lifecycle cleanup is handled by `homeboy worktree cleanup`; artifact removal stays dry-run here until `--apply` is passed.

Aggregate `--apply` submits a durable asynchronous controller job by default. For a bounded, controller-owned cleanup when the daemon is unavailable, explicitly select the existing local execution contract: `homeboy --placement local cleanup --include shared-cargo-targets --apply --full`. This executes synchronously through the same category safety checks, locks, and apply-time revalidation, and reports `execution.placement: "local"` with `durable: false`. Remote cleanup categories retain their normal remote runner behavior.

## `homeboy cleanup artifacts`

Scans the current checkout for built-in and declared artifact paths. The command defaults to dry-run output and only removes files when `--apply` is passed. `--path` always resolves to that exact checkout, including under `--apply`; it cannot select artifacts from sibling worktrees. Use `--all-worktrees` to explicitly discover every Git worktree in the selected repository. The JSON `scope` field reports `exact_checkout` or `repository_worktrees` in both dry-run and apply output.

Homeboy always treats Rust `target` directories as rebuildable artifacts. Projects can add repo-relative cleanup paths with `artifact_cleanup_paths` in `homeboy.json`.

```bash
homeboy cleanup artifacts
homeboy cleanup artifacts --path /path/to/checkout
homeboy cleanup artifacts --path /path/to/checkout --all-worktrees
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

Aggregate cleanup gives every selected category its own isolated process group and 30-second wall-clock deadline, within a 120-second deadline for the complete invocation. A category that reaches its deadline is terminated with its descendants and returns a typed `cleanup.category_timeout` row containing `elapsed_ms`, `timeout_ms`, `last_progress`, `inventory_completeness: "partial"`, and an exact `continuation_command`; later independent categories still run while aggregate time remains. Excluded categories are not spawned, so excluding `task-worktrees` bypasses its registry discovery, locks, and safety probes rather than merely hiding its output. Durable aggregate apply records the active category in `cleanup status`, retains complete partial evidence in `cleanup status <job-id> --full`, and finishes the durable job as failed rather than reporting a category failure as job success.

`homeboy cleanup retained-storage` uses the same 30-second process-group boundary around its read-only source inventories and filesystem reconciliation. Its progress identifies the active source; a deadline returns typed partial evidence and `homeboy cleanup retained-storage --limit <N>` as the safe continuation instead of blocking on a recursive walk.

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

## Leaked Test Homes

Homeboy's test isolation builds each test a private `HOME` from a `TempDir`.
`TempDir` reclaims on `Drop`, and `Drop` cannot run when the process is *killed*
rather than unwound — OOM kill, harness timeout, `SIGKILL` from a supervisor. A
home that materialized a controller runtime keeps a private copy of the debug
binary, so one abandoned entry is hundreds of megabytes.

```bash
homeboy cleanup --include leaked-test-homes
homeboy cleanup --include leaked-test-homes --apply
```

`runtime-tmp` does not see these. It scans `HOMEBOY_RUNTIME_TMPDIR`; the leak
lands wherever `tempfile` resolves `TMPDIR`. On a host that moved `TMPDIR` to a
dedicated volume those are different directories, and `runtime-tmp` truthfully
reports zero while gigabytes sit unreclaimed. This category scans `$TMPDIR`
first, then `/tmp`, `/var/tmp`, and `/dev/shm`, and **reports every root it
considered** — including the ones it could not read, with the reason. A zero is
always attributable to a directory rather than to an assumption.

Reclaim requires three independent proofs, and a live test's home fails the
second one:

1. The directory name starts with `hb-test-` and sits directly under a scanned
   root. The scan never recurses, never follows a symlink, never touches a file.
2. The PID stamped into the name is not this process and is not running. An
   entry whose owner is alive is classified `owner_alive` and is unreachable by
   every reclaim path here — no age, no budget, no flag. PID reuse fails safe: a
   recycled PID makes an abandoned entry look alive, so it survives to a later
   pass. A name carrying *no* PID means unknown, never unowned.
3. The entry is at least an hour old.

The byte ceiling (2 GiB retained) relaxes step 3, and only step 3, for the
oldest entries that already passed step 2. An age window alone cannot bound a
directory accumulating hundreds of megabytes per kill at an unbounded rate.

## Release Artifacts

`homeboy release` copies every published asset into
`<artifact-root>/release/<repo>/<version>/` so a retry, a repair command, or a
deploy reaches the exact published bytes without a rebuild. Nothing removed
those copies until #14223: the store had no count, byte, or age bound of any
kind, and on one host reached **6.1 GB** — 6.0 GB of it a single repository
holding fourteen ~435 MB builds published inside a nine-day window, while
upstream had already moved several minor versions past every one of them.

```bash
homeboy cleanup --include release-artifacts
homeboy cleanup --include release-artifacts --apply
```

`orphaned-artifact-bytes` cannot reach this store and never will. These
directories are referenced by durable release records, so that category
correctly inventories **zero** candidates against them. They are live, not
orphaned, which is why bounding them needs a policy of its own.

Deleting them is safe because every entry is a local copy of bytes already
published to a GitHub Release under an immutable tag. The remote copy is the
source of truth; the local one is a cache that avoids a rebuild. Losing an old
entry costs a download, not a release.

Two budgets apply, both **per repository**, and the stricter one wins:

- `retention.release_artifact_max_count` — versions retained per repository
  (default: 5).
- `retention.release_artifact_max_bytes` — retained bytes per repository
  (default: 2 GiB).

Both exist because per-release payloads span two orders of magnitude across
repositories. A count that preserves a small repository's whole history lets a
large one hold gigabytes; a byte ceiling that bounds the large one would
needlessly truncate the small one's history. Whichever limit a repository's own
payload size makes binding is the one that governs it.

Three rules constrain every removal:

1. **The newest release is never pruned.** Rank 0 for a repository always
   carries a retention reason, whatever the budgets say. A bound cannot empty a
   repository's directory — not even `--release-max-count 0`.
2. **A release published within the last hour is never pruned.** The fixed floor
   covers an in-flight publication. Rank alone does not cover this: two releases
   cut back to back put the second at rank 1, past a tight count budget, while
   its publication is still running.
3. **Retention is monotone in age.** Once a repository's byte budget is
   exhausted every *older* entry is eligible too, so pruning a newer entry while
   keeping an older one is unreachable.

Reported bytes are **hardlink-corrected**. Each version directory holds its
payload under two names — a numbered durable copy and the canonical upload name
GitHub derives an asset name from — and those are the same inode, because
staging hardlinks first and only copies if the link fails. Summing `st_size`
across directory entries reports roughly **twice** the disk a removal returns.
Each version reports `size_bytes` (what a removal actually frees), the naive
`logical_bytes`, and the `hardlink_duplicate_bytes` difference, so the
correction is visible rather than something to trust.

`candidate_count` and `estimated_bytes` count genuinely removable versions only.
Each version's `eligible` is computed as `retention_reasons.is_empty()` and is
never assigned any other way, so a populated reason forces `eligible: false`
structurally and cleanup cannot advertise reclaim the apply path will not
perform.

## Automatic Retention

Starting the Homeboy daemon installs the bounded retention pass by default:

```bash
homeboy daemon ensure-running
```

The installed `automatic-retention` schedule runs hourly with `--on-overlap skip`. The daemon owns cadence, overlap prevention, and stale-run recovery; no external timer is needed. Disable it to opt out:

```bash
homeboy schedule disable automatic-retention
```

To change its cadence, replace the installed declaration:

```bash
homeboy schedule add automatic-retention \
  --command "cleanup automatic-retention" \
  --every 6h \
  --on-overlap skip \
  --force
```

Existing declarations, including disabled schedules, are preserved when the daemon starts. Each pass first reconciles stale agent-task records, then applies the existing terminal-run, persisted-artifact, orphaned-byte, runtime-temp, leaked-test-home, controller-scratch, controller-runtime, shared-Cargo, and reconstructable repo-artifact cleanup policies. Leaked test homes are unattended for the reason they exist: the process that would have cleaned up after itself was killed, so nothing reclaims them by hand. Repo-artifact retention uses controller-accessible registered workspace roots, one global largest-first `retention.limit`, `retention.reconstructable_artifact_days`, and `retention.automatic_retention_max_run_seconds`; a configured `retention.reconstructable_artifact_reserve_bytes` enables early retention under free-space pressure. The output records reclaimed and retained evidence. It also prunes remote lab workspaces and runner binary caches: both accumulate on hosts an operator rarely inspects, and both are cursor-paginated and age-floored, so they fit the pass's wall-clock budget. A disconnected or failing runner degrades only its own category. Runner downloads and task worktrees remain outside unattended scope, because both can hold inputs or uncommitted work that is still live. Cargo retains its configured aggregate byte budget and active-lease predicate. The controller has a process and cross-process single-flight lock, writes combined pass evidence under the Homeboy data directory, and returns `homeboy cleanup automatic-retention` as the exact resume command.

Terminal agent-task convergence also runs bounded reconstructable artifact retention over controller-accessible task workspace roots. It is best-effort: inaccessible roots and retention failures are retained as terminal evidence while aggregate persistence continues. Later scheduled retention reevaluates registered roots after the age floor expires.

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
| `terminal-runs` | — (aggregate only) | Terminal observation records, their artifact bytes, and lifecycle directories | Durable run row in a terminal state | `retention.terminal_run_days`; unsafe local artifact paths keep the run |
| `persisted-run-artifacts` | `homeboy runs artifact cleanup-persisted` | Persisted artifact files/directories and their DB rows | `artifacts` row joined to a terminal run | `retention.terminal_run_days`; active/unknown run state, non-local bytes, out-of-root paths, and symlinks are skipped |
| `orphaned-artifact-bytes` | — (aggregate only) | Two crash-residue name families under the artifact root | Name shape from a single private constructor plus a parsed UUID; the database is deliberately **not** consulted | Fixed 24h floor, not operator-overridable; a failed size measurement changes the verdict in neither direction |
| `runner-downloads` (opt-in only) | `homeboy runs artifact cleanup-downloads` | Cache directories under `<artifact-root>/runner` | Canonical `<runner-id>/<run-id>` shape emitted by the single writer, **plus an explicit `internal_fetch` intent marker** (or no marker at all, under `--include-untagged`); the database is deliberately **not** joined | Fixed 24h floor over the *newest* byte in the cache directory, plus a non-terminal-run veto; an unreadable or `operator_pull` marker always retains, and an absent marker retains unless `--include-untagged` is passed. Narrowed by `--runner`/`--run-id`, which never waive the predicate |
| `runner-binary-caches` | `homeboy runner cache-prune <runner>` | Unselected managed Homeboy binary slots on a runner | Canonical `homeboy-*` / `dev/<16-hex>` slot layout with a regular expected binary | 24h floor; configured binary, process-owned slots, symlinks, and malformed layouts preserved; selection revalidated immediately before removal |
| `remote-lab-workspaces` | `homeboy runner workspace prune <runner>` | Orphaned runner-side Lab workspaces | `homeboy/runner-workspace/v1` metadata plus a resolvable `local_path`; never outside `_lab_workspaces`. A workspace is also reachable when its *exact* durable owner run is terminal and its lease is `delete_on_success` — an existing controller-side source path is not evidence the runner copy is live | 24h floor; pending apply-back or an unexpired lifecycle TTL preserves the workspace. Live, unavailable, ambiguous, or malformed run authority all retain |
| `runtime-tmp` | `homeboy self cleanup-runtime-tmp` | Orphaned Homeboy runtime temp entries | Owner id recorded in the entry | `retention.runtime_tmp_days` plus byte/count budgets; entries whose owner process is running are preserved |
| `leaked-test-homes` | — (aggregate only) | Isolated test homes abandoned by killed test processes, under `$TMPDIR`, `/tmp`, `/var/tmp`, `/dev/shm` | `hb-test-<pid>-` filename marker directly under a scanned root, plus a liveness probe on that PID; the database is deliberately **not** consulted | Fixed 1h floor, not operator-overridable, plus a 2 GiB retained-byte ceiling that relaxes the floor for the oldest *abandoned* entries only. A running owner, an unrecorded owner, a symlink, and a non-directory are all unreachable |
| `release-artifacts` | — (aggregate only) | Superseded durable release copies under `<artifact-root>/release/<repo>/<version>/` | Version directory under a repository directory in the release store; the database is deliberately **not** consulted. `orphaned-artifact-bytes` cannot reach these — they are referenced by durable release records and correctly inventory as zero orphans | `retention.release_artifact_max_count` (default 5) and `retention.release_artifact_max_bytes` (default 2 GiB), both per repository, stricter wins. The newest release of every repository is structurally unreachable, a fixed 1h floor covers an in-flight publication, and retention is monotone in age. Sizes are hardlink-corrected, so the numbered and canonical copies of one payload are billed once |
| `controller-scratch` | — (aggregate only) | Released controller scratch resources, including ephemeral attempt Git worktrees | Scratch index ownership with pid liveness; a linked worktree is proved by Git's own two-way `.git`/`gitdir` pointers, never by a database join | Per-resource retention window (P7D) unless `--older-than-days` is typed. A linked attempt worktree is additionally retained unless every commit reachable from its HEAD is already reachable from a branch, tag, or remote-tracking ref in its source repository. Removal goes through `git worktree remove`; a worktree that cannot be unregistered is reported and retained, never deleted behind Git |
| `shared-cargo-targets` | — (aggregate only) | Shared Cargo target stores | Store layout below Homeboy's data directory | `retention.shared_store_days` and byte budget; an unexpired lease preserves the store independently |
| `controller-runtimes` | `homeboy runtime controller-prune` | Unreferenced immutable controller runtime identities | Content-addressed pin path not referenced by a nonterminal durable record or the active generation, under the admission lock | `retention.controller_runtime_days` and byte budget; `--ignore-retention` is the explicit destructive opt-out |

### Why the specialists still exist

A specialist survives only when it accepts a *narrowing* argument the aggregate
cannot express, or when it is the operator escape hatch for a policy the
aggregate deliberately never applies:

- `cleanup artifacts` — `--path`, `--all-worktrees`, `--self`, `--temp-root`, `--sort`,
  `--merged-only`, `--min-age-days`, `--include-active-worktrees`.
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

### Draining the untagged backlog: `--include-untagged`

Retaining untagged bytes forever is the right default and the wrong terminal
state — the pre-tagging backlog only grows, and nothing else will ever reach it
(#11128). `--include-untagged` is the explicit operator opt-in that makes the
`absent` row above eligible:

```sh
homeboy cleanup --include runner-downloads --include-untagged
homeboy cleanup --include runner-downloads --include-untagged --apply
```

It widens *what* is eligible, and nothing else:

- The **default is unchanged.** Without the flag, untagged still fails closed,
  and its retain reason now names the flag that would release it.
- The **age floor still applies.** `--include-untagged` never lowers the fixed
  24h floor; a stale untagged cache beside a fresh one is still decided per
  directory, on the newest byte in each.
- **`operator_pull` is still never reclaimable.** The flag is about *unrecorded*
  intent, not about overriding a recorded operator claim.
- An **unreadable marker still retains.** A marker that exists but will not parse
  may be an `operator_pull` whose bytes on disk went bad, so uncertainty about a
  *present* marker is not what this widens.
- The **liveness veto is still honoured.** A non-terminal run claiming the
  `<run-id>` retains its cache, flag or no flag.

The plan says so: the sweep reports `include_untagged: true`, and every row it
releases under the opt-in carries a reason naming it rather than the
internal-fetch reason. The flag is off in the unattended retention pass and in
the degraded (store-shut) sweep — widening a delete predicate is a decision an
operator makes at a terminal, and there is nobody there to make it.

The category stays out of the bare sweep for now. Re-evaluating that is a
separate decision with its own evidence, once the backfill window has passed;
being absent from a default sweep is cheap and reversible, and a wrong delete is
neither.

The bytes stay fully visible: `homeboy cleanup retained-storage` accounts for
them in two halves (what a sweep would reclaim, and what it is holding on to)
and names `homeboy cleanup --include runner-downloads` as the reclaim command.
