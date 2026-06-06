# Homeboy and WP Codebox fanout boundary

Homeboy and WP Codebox both support generic agent fanout, but they own different
layers. Homeboy owns durable orchestration and provider-neutral evidence. WP
Codebox owns runtime fanout execution inside Codebox. The seam is the
`AgentTaskRequest`/`AgentTaskOutcome` adapter boundary.

## Ownership map

| Layer | Homeboy owns | WP Codebox owns |
|-------|--------------|-----------------|
| Operator surface | `homeboy agent-task`, `bench --matrix`, dispatch/review/promotion flows | Codebox runtime APIs and browser/server runtime entrypoints |
| Planning policy | repo, tracker, branch, worktree, matrix/fleet, retry, timeout, cancellation, reconciliation, and queue/backpressure policy | runtime worker graph validation, bounded Codebox concurrency, runtime dependency execution |
| Durable state | Homeboy run ids, submitted plans, aggregate records, logs, artifacts, evidence refs, review summaries, promotion/apply state | Codebox session ids, worker ids, runtime event streams, artifact namespaces, runtime-local aggregation/conflict payloads |
| Schemas | `homeboy/agent-task-request/v1`, `homeboy/agent-task-outcome/v1`, `homeboy/agent-task-artifact/v1`, `homeboy/agent-task-aggregate/v1` | `wp-codebox/agent-fanout-request/v1`, `wp-codebox/agent-fanout-plan/v1`, `wp-codebox/agent-fanout-worker/v1`, `wp-codebox/agent-fanout-result/v1`, `wp-codebox/agent-fanout-event/v1` |
| Evidence | provider-neutral artifact/evidence refs, diagnostics, workflow steps, outputs, and follow-up decisions | runtime artifact refs, worker/session refs, browser/server progress events, sandbox-specific diagnostics |

## Narrow seam

Homeboy submits provider-neutral `AgentTaskRequest` tasks to a Codebox executor
provider. The provider may translate the task into a Codebox fanout request or a
single Codebox task, but Homeboy core does not depend on Codebox runtime field
names.

The Codebox provider returns a normalized `AgentTaskOutcome`:

- `status`, `summary`, and `failure_classification` use Homeboy outcome values.
- `artifacts[]` contains only Homeboy `AgentTaskArtifact` records.
- `evidence_refs[]` points at Codebox sessions, event streams, manifests, or
  worker results through URI-style refs.
- `workflow.steps[]` can describe planner, worker, validator, repair, or
  aggregation phases in Homeboy's generic workflow evidence shape.
- `metadata` may include opaque provider refs such as Codebox fanout id, parent
  session id, worker ids, schema name, or version.

## Rules

- Homeboy core treats Codebox fanout payloads as opaque provider payloads until
  the Codebox adapter normalizes them.
- Homeboy schemas do not duplicate Codebox session, worker, artifact namespace,
  browser/server event, or conflict payload fields.
- Codebox schemas keep caller metadata opaque; they do not encode Homeboy issue,
  PR, worktree, queue, retry, or promotion semantics.
- Provider refs are durable enough for Homeboy to persist and reconcile, but
  Homeboy does not parse them beyond generic `kind`, `uri`, `label`, and opaque
  metadata.
- Promotion and apply decisions remain Homeboy policy even when the patch or
  evidence came from a Codebox runtime.

## Representative normalization

The fixture at `tests/fixtures/codebox_fanout_provider_payload.json` models a
Codebox `wp-codebox/agent-fanout-result/v1` payload. The contract test in
`tests/output_contracts_test.rs` normalizes it into Homeboy's generic
`AgentTaskOutcome` shape and asserts that Codebox session/worker details remain
provider refs or opaque metadata rather than new Homeboy core schema fields.

## Related trackers

- Homeboy fanout epic: https://github.com/Extra-Chill/homeboy/issues/3206
- Homeboy async lifecycle: https://github.com/Extra-Chill/homeboy/issues/3278
- Homeboy provider-native handles: https://github.com/Extra-Chill/homeboy/issues/3286
- Homeboy fleet cooking migration: https://github.com/Extra-Chill/homeboy/issues/3357
- Boundary tracker: https://github.com/Extra-Chill/homeboy/issues/3578
- WP Codebox fanout epic: https://github.com/Automattic/wp-codebox/issues/679
- WP Codebox fanout schemas: https://github.com/Automattic/wp-codebox/issues/683
