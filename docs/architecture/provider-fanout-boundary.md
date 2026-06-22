# Provider fanout boundary

Homeboy core owns durable orchestration and provider-neutral evidence. Runtime
providers own backend-specific execution details. The seam is the
`AgentTaskRequest`/`AgentTaskOutcome` adapter boundary.

## Ownership map

| Layer | Homeboy owns | Runtime providers own |
|-------|--------------|-----------------------|
| Operator surface | `homeboy agent-task`, `bench --matrix`, dispatch/review/promotion flows | Provider APIs, runtime entrypoints, and backend-specific operator surfaces |
| Planning policy | repo, tracker, branch, worktree, matrix/fleet, retry, timeout, cancellation, reconciliation, queue, and backpressure policy | runtime graph validation, bounded provider concurrency, backend dependency execution |
| Durable state | Homeboy run ids, submitted plans, aggregate records, logs, artifacts, evidence refs, review summaries, promotion/apply state | provider session ids, worker ids, runtime event streams, artifact namespaces, provider-local aggregation/conflict payloads |
| Schemas | `homeboy/agent-task-request/v1`, `homeboy/agent-task-outcome/v1`, `homeboy/agent-task-artifact/v1`, `homeboy/agent-task-aggregate/v1` | provider-owned request/result/event schemas |
| Evidence | provider-neutral artifact/evidence refs, diagnostics, workflow steps, outputs, and follow-up decisions | runtime artifact refs, worker/session refs, progress events, sandbox-specific diagnostics |

## Narrow seam

Homeboy submits provider-neutral `AgentTaskRequest` tasks to an executor provider.
The provider may translate the task into any backend-specific single-task or
fanout request, but Homeboy core does not depend on provider runtime field names.

The public operator meaning of `agent-task fanout` is batch cook: many
independent cooks, each with its own worktree/branch and its own PR
finalization. Generic provider-neutral fanout scheduling remains an internal
implementation seam for adapters and schedulers that need it; it is not the
public CLI contract.

Public batch-cook fanout lowers each cook into the normal cook-loop path, so
Homeboy owns dispatch, promotion, deterministic gates, commit/push, and PR
finalization per cook. Provider adapters still receive one normalized
`AgentTaskRequest` at a time and return one normalized `AgentTaskOutcome` at a
time.

The provider returns a normalized `AgentTaskOutcome`:

- `status`, `summary`, and `failure_classification` use Homeboy outcome values.
- `artifacts[]` contains only Homeboy `AgentTaskArtifact` records.
- `evidence_refs[]` points at provider sessions, event streams, manifests, or
  worker results through URI-style refs.
- `workflow.steps[]` can describe planner, worker, validator, repair, or
  aggregation phases in Homeboy's generic workflow evidence shape.
- `metadata` may include opaque provider refs such as fanout id, parent session
  id, worker ids, schema name, or version.

## Rules

- Homeboy core treats fanout payloads as opaque provider payloads until the
  owning adapter normalizes them.
- Homeboy schemas do not duplicate provider session, worker, artifact namespace,
  runtime event, or conflict payload fields.
- Provider schemas keep caller metadata opaque; they do not encode Homeboy issue,
  PR, worktree, queue, retry, or promotion semantics.
- Provider refs are durable enough for Homeboy to persist and reconcile, but
  Homeboy does not parse them beyond generic `kind`, `uri`, `label`, and opaque
  metadata.
- Promotion and apply decisions remain Homeboy policy even when the patch or
  evidence came from a runtime provider.

## Representative normalization

The fixture at `tests/fixtures/provider_fanout_payload.json` models a generic
provider fanout result. The contract test in `tests/output_contracts_test.rs`
normalizes it into Homeboy's generic `AgentTaskOutcome` shape and asserts that
provider session/worker details remain provider refs or opaque metadata rather
than new Homeboy core schema fields.

Provider-specific fixture coverage belongs in the provider layer that owns that
backend integration.

## Related trackers

- Homeboy fanout epic: https://github.com/Extra-Chill/homeboy/issues/3206
- Homeboy async lifecycle: https://github.com/Extra-Chill/homeboy/issues/3278
- Homeboy provider-native handles: https://github.com/Extra-Chill/homeboy/issues/3286
- Homeboy fleet cooking migration: https://github.com/Extra-Chill/homeboy/issues/3357
- Boundary tracker: https://github.com/Extra-Chill/homeboy/issues/3578
