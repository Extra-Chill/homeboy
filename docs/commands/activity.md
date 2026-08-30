# `homeboy activity`

Unified read-only activity surface for orchestrators and operators asking what Homeboy is doing now and what just finished.

## Usage

```bash
homeboy activity
homeboy activity list --limit 50
homeboy activity list --no-runners
homeboy activity show <id>
homeboy activity watch <id> --timeout 30m
homeboy agent-task watch <id>          # alias for `activity watch`
```

`<id>` resolves across observation run ids, agent-task run ids, cook ids, and runner daemon job ids.

## Output

JSON output uses the standard command-result envelope with `data.schema = homeboy/activity-report/v1`. The activity payload normalizes observation runs, agent-task lifecycle records, daemon jobs, and connected runner sessions into `ActivityItem` records with:

- `id`, `kind`, `source_store`, `state`
- timestamps: `created_at`, `updated_at`, `finished_at`
- runner refs: `runner_id`, `job_id`, `transport`
- cross refs: `run_id`, `runner_job_id` (execution)
- structured `next_actions` with `label` and exact `command`

The default (and `--limit`-bounded) view compacts every retained record to that identity surface plus at most two follow-up actions per record: artifact/evidence ref rosters, per-store `source_projections`, `state_conflicts`, task-identity enumerations, and the raw command line are omitted, and the report-level `next_actions` rollup still covers every retained record's full action set (capped at 20 commands). The whole serialized response — items, refs, lifted next actions, artifacts, evidence, and the human table — is bounded end to end by the display limit, and records the `truncation` object claims were omitted surface nowhere else in the payload (#13617). Full per-record detail is available through `activity list --all --limit <count>`, `activity show <id>`, and the artifact/evidence commands.

`agent_task_record_health` is a full-corpus diagnostic attached by `list` only; the default view carries its counts without the per-record sample ids, which `--all` retains. `show` and `watch` resolve a single id and leave it null rather than scanning every durable agent-task record to fill it.

`reconciled` is always `false` here. Activity and every `agent-task status` mode are pure reads; explicit `agent-task reconcile` operations report their mutations separately.

## Counts: executing work vs open resources

Activity items carry two work classes. **Executing work** is a unit of work the system runs to completion — an observation run, an agent-task record, a daemon job, or a runner-resident job. **Open resources** are inventory that work uses: worktree-provider records, whose presence says nothing about whether anything is executing in them (#13620).

`counts` separates the classes:

- `active`, `queued`, and `running` count executing work only. An open worktree projects state `running` because it is held, and is deliberately excluded from these counts so execution liveness is never inflated by inventory.
- `open_resources` counts held resource inventory: worktrees without a terminal disposition, including degraded-but-held ones.
- `total` counts every record in the report across both classes.
- the remaining state buckets (`succeeded`, `failed`, `stale`, …) describe executing work; a worktree's terminal disposition is resource history, visible in `items`.

`zero_executing_work` is the machine-readable maintenance precondition: `true` only when the report shows no queued or running executing work **and** is not `partial` (a connected runner that did not answer could be holding executing work this report cannot see). Open resources never affect it — an operator may hold worktrees open while zero work executes. Assert it on a `list` report; a `show` report's counts describe only the resolved item.

Human output is a compact table preceded by a one-line summary presenting both dimensions, e.g. `activity: total=110 executing=1 (running=1 queued=0) open_resources=3 failed=9 stale=2`, followed by next-action command lines per item.

The default view prioritizes active work, scans a bounded extra window for stale
projections, and omits stale rows that would crowd out current records. Its
`truncation` object states omitted record and next-action counts explicitly.
Use `homeboy activity list --all --limit <count>` to retain every collected
record and action, or `homeboy activity show <id>` for a direct record lookup.

## Runner federation

Records for a run offloaded to a Lab runner live on that runner until it reports back, so a controller-local read cannot see them. `activity` federates connected Lab runners by default:

- The remote read is `statuses_indexed` — one `/jobs` query against the already-connected session, with no generation reconcile — bounded by `HOMEBOY_READONLY_PROBE_TIMEOUT_SECONDS` (15s default).
- A runner with no connected session is never queried: no session is opened and no network is performed. It appears with `queried: false`.
- A connected runner that fails or times out sets `partial: true` and is named in `runner_federation.runners[].error`. Every other source still returns; a runner outage is never a command failure.
- Federation is skipped entirely when no runner layer is registered in the process.

Opt out with `--no-runners`, or `HOMEBOY_ACTIVITY_FEDERATE_RUNNERS=0` for a whole process (for example a long-lived daemon serving the HTTP activity endpoint).

## Scope

This is a local read model only. List, show, and watch do not reconcile or otherwise mutate persisted state. `show` and `watch` resolve their id through indexed per-provider probes — agent-task lifecycle, observation run, daemon job — and only fall back to a bounded full report when no probe answers. Agent-task stale actions target the inspected run with `homeboy agent-task reconcile <run-id> --dry-run`; review the authoritative provider-state preview, then add `--apply` to authorize that one lifecycle mutation. It does not create a daemon, event bus, or offloaded job, and the Lab contract marks it local-only.
