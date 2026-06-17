# Agent task executor adapter

Agent task executor adapters are the boundary between Homeboy core and the
systems that actually run agent work. Core owns the provider-neutral request,
outcome, artifact, and lifecycle types in `src/core/agent_task.rs`; concrete
backends own launch mechanics, credentials, process/session IDs, and provider
payload parsing.

## Contract

Implement `AgentTaskExecutorAdapter` for a backend and pass that adapter to the
scheduler or fan-out coordinator that dispatches an `AgentTaskRequest`.

Adapters provide these operations:

- `capabilities()` returns the backend name, selector, and supported lifecycle
  features.
- `validate(request)` checks backend-specific policy and required capabilities.
- `prepare_workspace(request)` creates or resolves the workspace the backend can
  operate on.
- `start_task(request, workspace)` starts the work and may return an immediate
  `AgentTaskOutcome` for synchronous backends.
- `poll_progress(handle)` returns async progress, stream events, provider
  payloads, or a terminal outcome.
- `cancel_task(handle)` stops unfinished async work when the poll budget is
  exhausted or a caller aborts the task.
- `collect_artifacts(handle)` returns normalized `AgentTaskArtifact` records.
- `normalize_outcome(request, handle, provider_payload)` converts backend output
  into an `AgentTaskOutcome`.

## Backend ownership

Core should only see generic task types and adapter trait objects. Backend
details stay with the component that knows how to run that backend.

| Backend category | Adapter owner | Core-facing backend string |
|------------------|---------------|----------------------------|
| Runtime sandbox provider | Provider extension integration | provider-defined string |
| CLI/session provider | Local CLI/session integration | `cli`, `opencode`, or provider-defined string |
| Remote runner job | Runner/job integration | `runner` or provider-defined string |

The string values are selectors, not an enum in core. This keeps core open to
new backends without adding provider-specific variants.

## Registration

Extensions register an adapter by exposing an implementation of
`AgentTaskExecutorAdapter` to the scheduler or fan-out coordinator that owns the
task batch. The coordinator selects an adapter by matching
`AgentTaskRequest.executor.backend`, optional `selector`, and
`required_capabilities` against `AgentTaskExecutorCapabilities`.

Adapters should reject incompatible requests in `validate()` with
`AgentTaskFailureClassification::CapabilityMissing`, `PolicyDenied`, or
`InvalidInput` so callers get normalized failure classes.

## Sync and async completion

`start_task()` supports both execution styles:

- Synchronous adapters return `AgentTaskStart { outcome: Some(...) }`.
- Async adapters return a handle and expose progress through `poll_progress()`.

When polling reaches a terminal state with a provider payload, the scheduler
calls `normalize_outcome()` and appends `collect_artifacts()` output. If polling
exceeds the scheduler's configured poll budget, the scheduler calls
`cancel_task()` and returns that normalized cancellation outcome.

## Fleet scheduling policy

Fleet plans use conservative scheduler defaults: one task runs at a time unless
the caller explicitly raises `max_concurrency`. Callers may also set
`max_tasks`/`max_queue_depth` to cap accepted queue depth, and
`per_executor_concurrency` to keep one backend or runner selector from consuming
all global capacity. Per-executor keys are the backend string, or
`backend:selector` when a selector is present.

Backpressure is reported in the aggregate `queue` object. Queue-depth rejections
produce blocked task events, scheduler diagnostics, and normalized failed
outcomes with `PolicyDenied` failure classification so operators can see which
tasks were not started and why.

Retry policy stays executor-agnostic. `max_attempts` bounds per-task attempts,
`max_retries_total` provides a fleet-level retry budget, and
`retryable_failure_classifications` lets callers retry only normalized failure
classes such as `Provider` or `ExecutionFailed`.

## Secret environment

Agent-task requests may declare required provider environment names in
`executor.secret_env`. Homeboy core resolves those names before provider
dispatch, validates that each name has a value, and injects the resolved values
into the provider process environment. Secret values are not included in
outcomes, diagnostics, aggregate JSON, or artifacts.

Resolution order is:

- The current process environment using the declared name.
- `~/.config/homeboy/agent-task-secrets.json` entries.

The optional local config file uses provider-agnostic sources:

```json
{
  "secrets": {
    "PROVIDER_TOKEN": {
      "source": "env",
      "env_var": "CI_PROVIDER_TOKEN"
    },
    "LOCAL_PROVIDER_TOKEN": {
      "source": "keychain",
      "scope": "agent-task",
      "name": "LOCAL_PROVIDER_TOKEN"
    }
  }
}
```

Use `source: "env"` in CI when a runner exposes a differently named variable.
Use `source: "keychain"` for local operator machines that store secrets through
Homeboy's OS keychain integration. Missing declared names produce a structured
`agent_task.secret_env_missing` preflight outcome before the provider process is
spawned.

## Provider payloads

Provider payloads are intentionally opaque `serde_json::Value` objects until
they reach the owning adapter. Core stores, forwards, and redacts generic task
structures; adapters are responsible for interpreting backend-specific payloads
and returning normalized Homeboy artifacts, diagnostics, evidence refs, and
status values.

## Portable runtime manifest contract

Provider manifests may include `runtime_contract` when a runtime returns stable
runtime-shaped output that should be normalized into canonical Homeboy fields.
This keeps Homeboy responsible for durable state, fanout, promotion, status,
notifications, and canonical artifacts while each runtime adapter owns its launch
mechanics and native payload vocabulary.

```json
{
  "id": "wp-codebox.codex",
  "backend": "codebox",
  "capabilities": ["structured_outcome"],
  "runtime_contract": {
    "capabilities": ["sandbox", "artifacts", "agent_result"],
    "lifecycle_states": {
      "execution_states": {
        "queued": "queued",
        "running": "running",
        "complete": "succeeded",
        "failed": "failed"
      },
      "outcome_statuses": {
        "complete": "succeeded",
        "no_changes": "no_op",
        "failed": "failed",
        "timed_out": "timeout"
      }
    },
    "normalization": {
      "status_path": "outputs.codebox.state",
      "summary_path": "outputs.codebox.summary",
      "output_artifacts": [
        {
          "name": "patch",
          "type": "patch",
          "artifact_schema": "text/x-patch",
          "path": "outputs.codebox.artifacts.patch",
          "kind": "patch",
          "mime": "text/x-patch"
        },
        {
          "name": "agent-report",
          "type": "agent_report",
          "artifact_schema": "application/json",
          "path": "outputs.codebox.artifacts.report",
          "kind": "report",
          "mime": "application/json"
        }
      ]
    }
  }
}
```

The normalization hook is intentionally manifest-driven and small:

- `capabilities` names runtime capabilities; scheduler routing still uses the
  existing provider `capabilities` / request `required_capabilities` contract.
- `lifecycle_states.execution_states` documents native runtime lifecycle names
  and their canonical `AgentTaskExecutionState` values for async adapters.
- `lifecycle_states.outcome_statuses` maps native terminal runtime status names
  to canonical `AgentTaskOutcomeStatus` values.
- `normalization.status_path` and `summary_path` read dotted paths from the
  provider outcome's `outputs` or `metadata` objects.
- `normalization.output_artifacts` maps runtime output paths to canonical
  `AgentTaskArtifact` records and, when `type` or `artifact_schema` is present,
  matching `AgentTaskTypedArtifact` records.

For Codebox, the adapter should expose the native Codebox result under a stable
`outputs.codebox` object and declare the mapping above. Homeboy then treats
Codebox like any other runtime adapter: it records the run, evaluates canonical
artifacts, promotes patches, reports status, and leaves Codebox-specific session
details inside the runtime payload.

For the fanout boundary, see
[Provider fanout boundary](provider-fanout-boundary.md).
