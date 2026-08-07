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
- cross refs: `run_id`, `agent_task_run_id`, `runner_job_id`
- artifact/evidence refs
- structured `next_actions` with `label` and exact `command`

`agent_task_record_health` is a full-corpus diagnostic attached by `list` only. `show` and `watch` resolve a single id and leave it null rather than scanning every durable agent-task record to fill it.

`reconciled` is always `false` here. It is emitted because `agent-task status <id> --bridge` is a reconciling read that *writes*, so the two surfaces can legitimately report different states for the same run at the same instant — and calling the reconciling one changes what this one returns next. The flag lets a consumer tell which kind of answer it received.

Human output is a compact table followed by next-action command lines per item.

## Runner federation

Records for a run offloaded to a Lab runner live on that runner until it reports back, so a controller-local read cannot see them. `activity` federates connected Lab runners by default:

- The remote read is `statuses_indexed` — one `/jobs` query against the already-connected session, with no generation reconcile — bounded by `HOMEBOY_READONLY_PROBE_TIMEOUT_SECONDS` (15s default).
- A runner with no connected session is never queried: no session is opened and no network is performed. It appears with `queried: false`.
- A connected runner that fails or times out sets `partial: true` and is named in `runner_federation.runners[].error`. Every other source still returns; a runner outage is never a command failure.
- Federation is skipped entirely when no runner layer is registered in the process.

Opt out with `--no-runners`, or `HOMEBOY_ACTIVITY_FEDERATE_RUNNERS=0` for a whole process (for example a long-lived daemon serving the HTTP activity endpoint).

## Scope

This is a local read model only. List, show, and watch do not reconcile or otherwise mutate persisted state. `show` and `watch` resolve their id through indexed per-provider probes — agent-task lifecycle, observation run, daemon job — and only fall back to a bounded full report when no probe answers. (`list` still refreshes agent-task records through the reconciling lifecycle status read, which writes; making that projection read-only is tracked separately.) Agent-task stale actions target the inspected run with `homeboy agent-task reconcile <run-id> --dry-run`; review the authoritative provider-state preview, then add `--apply` to authorize that one lifecycle mutation. It does not create a daemon, event bus, or offloaded job, and the Lab contract marks it local-only.
