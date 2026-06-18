# Generic Fanout/Reconcile Workflow

`wordpress/scripts/agent/homeboy-generic-fanout-reconcile.cjs` exposes the shared fanout/reconcile runner through JSON files. It is domain-neutral: callers provide item artifacts, grouping rules, task request templates, and runtime execution descriptors.

## Plan

```bash
node wordpress/scripts/agent/homeboy-generic-fanout-reconcile.cjs \
  --config fanout-config.json \
  --items items.json \
  --output fanout-plan.json
```

Minimal config:

```json
{
  "schema": "homeboy/generic-fanout-reconcile-config/v1",
  "orchestrator": { "id": "example", "run_id": "run-1", "plan_id": "plan-1" },
  "group_key_path": "category",
  "task_request_template": {
    "id": "task-{{group.key}}",
    "group_key": "{{group.key}}",
    "item_ids": "{{group.item_ids}}",
    "instructions": "Process {{group.key}} with {{group.item_count}} item(s).",
    "inputs": { "items": "{{group.items}}" }
  },
  "runtime_execution": {
    "backend": "caller-provided-runtime",
    "task": { "name": "process-generic-group", "group": "{{group.key}}" }
  }
}
```

Supported template values include `{{group.key}}`, `{{group.index}}`, `{{group.items}}`, `{{group.item_count}}`, `{{group.item_ids}}`, and `{{orchestrator.<field>}}`. If the whole string is a template expression, arrays and objects remain typed instead of being stringified.

## Reconcile

After a caller executes the task requests using its chosen runtime, pass records back in:

```bash
node wordpress/scripts/agent/homeboy-generic-fanout-reconcile.cjs \
  --config fanout-config.json \
  --plan fanout-plan.json \
  --records task-records.json \
  --output fanout-result.json
```

Records are matched by `id`, `task_id`, `sandbox_session_id`, or `group_key`. Successful statuses default to `completed`, `success`, and `passed`; override with `success_statuses` in config. Outcomes default to the record `outcome` field; override with `outcome_path`.

Runtime-specific descriptors are caller-owned. Keep provider names, credentials, WordPress setup, sandbox recipes, and repository-specific instructions in caller config or workflow examples, not in this generic helper.
