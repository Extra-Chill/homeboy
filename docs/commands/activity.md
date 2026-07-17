# `homeboy activity`

Unified read-only activity surface for orchestrators and operators asking what Homeboy is doing now and what just finished.

## Usage

```bash
homeboy activity
homeboy activity list --limit 50
homeboy activity show <id>
homeboy activity watch <id> --timeout 30m
```

`<id>` resolves across observation run ids, agent-task run ids, and runner daemon job ids.

## Output

JSON output uses the standard command-result envelope with `data.schema = homeboy/activity-report/v1`. The activity payload normalizes observation runs, agent-task lifecycle records, daemon jobs, and connected runner sessions into `ActivityItem` records with:

- `id`, `kind`, `source_store`, `state`
- timestamps: `created_at`, `updated_at`, `finished_at`
- runner refs: `runner_id`, `job_id`, `transport`
- cross refs: `run_id`, `agent_task_run_id`, `runner_job_id`
- artifact/evidence refs
- structured `next_actions` with `label` and exact `command`

Human output is a compact table followed by next-action command lines per item.

## Scope

This is a local read model only. List, show, and watch do not reconcile or otherwise mutate persisted state. Use the structured `reconcile` actions when shown, such as `homeboy runs reconcile` or `homeboy agent-task active --reconcile`, to invoke the existing explicit reconciliation services. It does not create a daemon, event bus, or offloaded job, and the Lab contract marks it local-only.
