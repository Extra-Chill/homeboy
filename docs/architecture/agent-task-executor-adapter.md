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

| Backend | Adapter owner | Core-facing backend string |
|---------|---------------|----------------------------|
| WP Codebox | Homeboy Extensions WordPress integration | `codebox` |
| CLI/OpenCode session | Local CLI/session integration | `cli` or `opencode` |
| Remote runner job | Runner/job integration | `runner` |

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

## Provider payloads

Provider payloads are intentionally opaque `serde_json::Value` objects until
they reach the owning adapter. Core stores, forwards, and redacts generic task
structures; adapters are responsible for interpreting backend-specific payloads
and returning normalized Homeboy artifacts, diagnostics, evidence refs, and
status values.
