# Generic Fanout/Reconcile Workflow

`runtime-agent-ci/lib/generic-fanout-reconcile-workflow.js` and `runtime-agent-ci/lib/fanout-reconcile-runner.js` provide executor-neutral fanout/reconcile primitives. `runtime-agent-ci/scripts/homeboy-generic-fanout-reconcile.cjs` exposes the shared planner/reconciler through JSON files, and `wordpress/scripts/agent/homeboy-generic-fanout-reconcile.cjs` remains as a compatibility entrypoint for existing WordPress-lane callers.

This helper is the planner/reconciler side of the audit fanout boundary, not a runtime provider. Runtime packages, provider names, credentials, WordPress setup, sandbox recipes, and provider task schemas stay behind the `audit-fanout-runtime-provider` interface. The current audit fanout implementation is the quarantined WP Codebox lane, which maps grouped audit findings to `wp-codebox/task-input/v1` requests and executes them through Codebox-owned task runner contracts.

## Plan

```bash
node runtime-agent-ci/scripts/homeboy-generic-fanout-reconcile.cjs \
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
    "backend": "caller-provided-executor",
    "task": { "name": "process-generic-group", "group": "{{group.key}}" }
  }
}
```

Supported template values include `{{group.key}}`, `{{group.index}}`, `{{group.items}}`, `{{group.item_count}}`, `{{group.item_ids}}`, and `{{orchestrator.<field>}}`. If the whole string is a template expression, arrays and objects remain typed instead of being stringified.

## Reconcile

After a caller executes the task requests using its chosen runtime, pass records back in:

```bash
node runtime-agent-ci/scripts/homeboy-generic-fanout-reconcile.cjs \
  --config fanout-config.json \
  --plan fanout-plan.json \
  --records task-records.json \
  --output fanout-result.json
```

Records are matched by `id`, `task_id`, `sandbox_session_id`, or `group_key`. Successful statuses default to `completed`, `success`, and `passed`; override with `success_statuses` in config. Outcomes default to the record `outcome` field; override with `outcome_path`.

## Finding Packets

`runtime-agent-ci` and `runtime-agent-ci/lib/generic-fanout-reconcile-workflow.js` export helpers for diagnostic/finding packet inputs. `wordpress/lib/generic-fanout-reconcile-workflow.js` re-exports the same helpers for existing entrypoints.

- `normalizeFindingPacketItems(packets, policy)` flattens packet-level `findings` or `diagnostics` arrays into generic items with stable packet/finding IDs.
- `materializeFindingPacketFanoutConfig({ packets, policy, ...config })` applies policy-driven grouping and returns the generic config/groups/items needed by the planner.
- `createFindingPacketFanoutPlan(input)` creates the fanout plan directly from packets.
- `createFindingPacketReconcileInput({ packets, policy, plan, records })` normalizes executed task records into the input shape accepted by `createGenericFanoutReconcileResult()`.

Grouping policy is data-driven. By default packets group by `finding.type` and `finding.severity`; callers can pass `group_by`/`groupBy` path arrays or a `group_key_template` such as `{{finding.type}}:{{finding.severity}}`. The default task request carries generic `item_ids`, `packet_ids`, `finding_count`, and `inputs.findings`; callers can still provide their own `task_request_template` and opaque `runtime_execution` descriptor.

Execution descriptors are caller-owned and opaque to this helper. Keep provider names, credentials, WordPress setup, sandbox recipes, repository-specific instructions, and provider-specific request details such as `wp-codebox/task-input/v1` in caller config or implementation-specific workflow examples, not in this generic reconcile helper.
