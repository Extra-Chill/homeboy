# agent-task

Run provider-neutral task plans through Homeboy's durable agent-task lifecycle.

Homeboy owns durable orchestration and provider-neutral outcomes. Runtime
providers own backend-specific execution. For the provider fanout ownership seam,
see [`docs/architecture/provider-fanout-boundary.md`](../architecture/provider-fanout-boundary.md).

## Dispatch

`agent-task dispatch` builds a durable task plan from common repo-cooking inputs
without requiring hand-authored provider JSON:

```bash
homeboy agent-task dispatch \
  --repo data-machine \
  --cwd /path/to/worktree \
  --provider-config @provider-config.json \
  --client-context @client-context.json \
  --prompt @task.txt
```

Homeboy core treats `--client-context` as an optional opaque JSON object. Client
adapters may include whatever correlation data they need to reconcile their own
notifications or UI state, but Homeboy does not interpret transport-specific
identifiers in core lifecycle state. Provider-specific execution settings belong
in `--provider-config`; durable lifecycle commands remain headless and can be
claimed later with `agent-task run` or `agent-task run-next`.

## Headless Fleet-Cooking Review

The authoritative non-chat workflow is the durable `agent-task` lifecycle. Chat
clients, Discord threads, GitHub Actions, cron, and terminal operators can all
submit the same run id, inspect it later, and promote selected artifacts without
depending on transport-local state.

```bash
run_id="homeboy-3357-$(date +%s)"

homeboy agent-task dispatch \
  --repo homeboy \
  --cwd /path/to/homeboy@fix-issue \
  --task-url https://github.com/Extra-Chill/homeboy/issues/3357 \
  --concurrency 4 \
  --attempts 2 \
  --run-id "$run_id" \
  --queue-only \
  --prompt @task.txt

# A daemon or later terminal process can claim work without chat history.
homeboy agent-task run-next

# One review envelope contains lifecycle state, logs, artifacts, aggregate
# reconciliation, promotion candidates, and next actions.
homeboy agent-task review "$run_id" \
  --to-worktree homeboy@fix-issue-3357-agent-task-non-chat-flow
```

`agent-task review` returns `homeboy/agent-task-review/v1` with:

- `record`: the durable run record from `status`.
- `logs`: scheduler events from queued or completed lifecycle state.
- `artifacts`: artifacts and evidence refs from the completed aggregate.
- `aggregate_review`: apply/retry/issue-report/review candidate reconciliation.
- `promotion_candidates`: generated `homeboy agent-task promote <run-id>` command
  arrays for apply candidates, completed with `--to-worktree` when supplied.
- `transport.chat_state_required: false`, making Homeboy the source of truth.

This is the terminal/daemon-owned review surface for fleet cooking. Kimaki or any
other chat UI should submit, poll, render, and call these commands rather than
owning scheduling, state, artifacts, reconciliation, or promotion.

`agent-task status`, `logs`, `artifacts`, and `review` are read-only durable
lifecycle inspection commands. They do not start workloads and are not gated by
warm-machine resource policy; use `homeboy runner exec <runner> -- homeboy
agent-task status <run-id>` when the durable state lives on a Lab runner host.

## Deterministic Smoke Gate

Issue #3392 is covered by a no-secret fixture plan at
`tests/fixtures/agent_task_smoke_plan.json`. It exercises the operator path
without provider credentials, chat state, or long-running external services.

Run it from a disposable Homeboy worktree:

```bash
run_id="agent-task-smoke-$(date +%s)"
target_worktree="homeboy@fix-3392-agent-task-smoke"

homeboy agent-task submit \
  --plan @tests/fixtures/agent_task_smoke_plan.json \
  --run-id "$run_id"

homeboy agent-task status "$run_id"
homeboy agent-task logs "$run_id"
homeboy agent-task run "$run_id"
# Or let a generic worker claim the oldest queued durable run:
# homeboy agent-task run-next
homeboy agent-task status "$run_id"
homeboy agent-task artifacts "$run_id"
homeboy agent-task review "$run_id" \
  --to-worktree "$target_worktree"
homeboy agent-task promote "$run_id" \
  --to-worktree "$target_worktree" \
  --dry-run
```

The gate passes when:

- `submit` returns a durable `run_id` immediately with `state: "queued"`.
- Pre-run `status` and `logs` show the queued fixture cell.
- `run` exits successfully and writes the aggregate lifecycle record.
- Post-run `status` shows `state: "succeeded"`.
- `artifacts` lists a patch artifact, an agent result artifact, and a transcript evidence ref.
- `review` returns a `homeboy/agent-task-review/v1` envelope with `transport.chat_state_required: false`, aggregate reconciliation, and promotion candidates.
- `promote <run-id> --dry-run` resolves the aggregate from the durable run id and reports the selected non-empty patch plus changed files without requiring the operator to look up `aggregate_path` manually.

When promotion runs without `--dry-run`, each `--verify <command>` is treated as
a visible deterministic gate in the promoted worktree. Promotion reports gate
results as `deterministic_gates[]` using
`homeboy/agent-task-gate-report/v1`. Failed visible gates set promotion
`status: "gate_failed"`, exit nonzero, and include
`failure_evidence.agent_feedback` plus stdout/stderr tails so the next cook-loop
agent task can receive exact failure context instead of a generic shell error.

Use `--private-verify <command>` for orchestrator-only completion gates that
should decide completion without exposing hidden evaluator details to the next
agent attempt. Private gate reports still appear in the promotion report for
human/orchestrator evidence, but `agent-task gate-feedback` applies
`--private-gate-reveal <policy>` before building the follow-up request. Supported
policies are `summary-only` (default), `redacted`, `no-detail`, and
`full-evidence`. Visible gate failures continue to provide full deterministic
evidence to the agent.

`agent-task gate-feedback` converts a promotion report and the original
`AgentTaskRequest` into a provider-neutral cook-loop decision:

```bash
homeboy agent-task gate-feedback \
  --promotion @promotion.json \
  --source-task @source-task.json \
  --source-run-id "$run_id" \
  --attempt 1 \
  --max-attempts 3 \
  --current-diff @current.diff
```

The command returns `homeboy/agent-task-cook-loop-report/v1`. Red gates with
remaining budget produce `status: "retry_requested"` and a complete
`follow_up_request` containing the failed command, exit status, log tails,
changed files, patch artifact ref, current diff context, and source run/task
refs. Red gates with exhausted budget return `status: "retries_exhausted"`.
Green promotion returns `status: "green_completed"` and no follow-up task.

Queued runs that should not execute can be cancelled without chat/session state:

```bash
homeboy agent-task cancel "$run_id" --reason "not selected by controller"
```

`cancel` marks queued runs and stale-running records as `cancelled` in the
durable lifecycle store. It refuses to claim live provider cancellation for an
active runner process until a provider-owned cancellation channel is available.

## Component Contracts

Agent-task plans may declare generic top-level `component_contracts`. Homeboy
preserves these objects as executor request inputs and does not attach product,
provider, or sandbox-specific semantics to them:

```json
{
  "schema": "homeboy/agent-task-plan/v1",
  "plan_id": "site-generation-loop",
  "component_contracts": [
    {
      "slug": "domain-component",
      "path": "/workspace/domain-component",
      "loadAs": "plugin",
      "activate": true
    }
  ],
  "tasks": []
}
```

When a plan is Lab-offloaded, controller-local `component_contracts[].path`
values are discovered, synced, and remapped with the same local-to-remote
workspace mapping used for provider configs, runtime component paths, provider
plugin paths, workspace roots, and path-valued settings. Lab offload evidence
records the original and remapped paths in `workspace_mapping.workspaces` using
the `component_contract` role.

When the intended checkout already exists on a Lab runner, dispatch from that
runner-side checkout through `runner exec` instead of forcing a controller-local
hot run:

```bash
homeboy runner exec homeboy-lab \
  --cwd /srv/homeboy/checkouts/homeboy \
  -- homeboy agent-task cook \
    --repo homeboy \
    --cwd /srv/homeboy/checkouts/homeboy \
    --prompt @task.txt
```

`runner exec` marks non-local jobs as runner-hosted, so nested `agent-task cook`
commands pass the non-interactive resource preflight without `--force-hot`.

## Dispatch Workspaces

`agent-task dispatch` accepts generic Homeboy workspace inputs and does not
resolve product-specific workspace handles itself.

Use `--cwd <PATH>` when the caller already knows the checkout or worktree path:

```bash
homeboy agent-task dispatch \
  --repo homeboy \
  --cwd /path/to/homeboy@fix-issue \
  --prompt @task.txt
```

Use `--workspace <ID_OR_PATH>` for a Homeboy-managed task worktree ID or an
existing workspace path:

```bash
homeboy worktree create homeboy --branch fix/issue-123
homeboy agent-task dispatch \
  --workspace homeboy@fix-issue-123 \
  --prompt @task.txt
```

External workspace managers should resolve their own handles to local paths and
call dispatch with `--cwd <resolved-path>`.

When `agent-task cook` or `agent-task dispatch` is Lab-offloaded with a
patch-producing provider, `--cwd` must point at a clean git checkout with
`remote.origin.url` configured. Homeboy uses that contract to materialize a real
runner-side git checkout/worktree before provider dispatch so generated files can
come back as patch artifacts. Non-git directories, dirty worktrees, and checkouts
without `origin` fail on the controller before offload with a supported-path
diagnostic; use a Homeboy worktree or another clean checkout
for write-capable agent tasks.

## Provider Runtime Contracts

Agent runtime manifests may declare portable provider contracts that Homeboy uses
before and after execution without learning provider-specific APIs. These fields
belong on each `agent_task_executors[]` entry:

```json
{
  "schema": "homeboy/agent-task-executor-provider/v1",
  "id": "example.default",
  "backend": "example",
  "command": "example-provider",
  "request_schema": "homeboy/agent-task-request/v1",
  "outcome_schema": "homeboy/agent-task-outcome/v1",
  "secret_env_requirements": [
    {
      "env": ["EXAMPLE_API_TOKEN"],
      "secret_env_sources": {
        "EXAMPLE_API_TOKEN": { "kind": "env", "name": "EXAMPLE_API_TOKEN" }
      }
    }
  ],
  "runner_readiness": [
    {
      "id": "example-auth",
      "label": "Example provider auth",
      "secret_env": ["EXAMPLE_API_TOKEN"],
      "remediation": "Configure EXAMPLE_API_TOKEN with homeboy agent-task auth."
    }
  ],
  "workspace_materialization": {
    "cwd": "git_checkout",
    "requires_git": true,
    "write_scope": "workspace",
    "artifact_paths": ["artifacts"]
  },
  "timeout_artifact_discovery": {
    "config_path_keys": ["provider_artifact_root"],
    "paths": ["/var/tmp/example-provider/latest"],
    "artifact_patterns": [
      {
        "kind": "metrics",
        "filename_patterns": ["*-metrics.ndjson"],
        "mime": "application/x-ndjson",
        "metadata": { "role": "telemetry" }
      }
    ]
  }
}
```

Homeboy treats these declarations as generic contracts:

- `secret_env_requirements` and `runner_readiness` describe required secret env
  names and redacted readiness probes without exposing values.
- `workspace_materialization` describes the checkout shape a provider needs; it
  does not name any workspace manager or product runtime.
- `timeout_artifact_discovery` extends timeout evidence recovery with declared
  paths, request metadata/config path keys, and typed filename/extension patterns.
  Discovered files are normalized into `AgentTaskArtifact` entries with generic
  `kind`, `mime`, and opaque metadata.
- Provider-specific sessions, APIs, artifact namespaces, and backend payloads stay
  outside Homeboy core and are represented only as artifacts, evidence refs,
  diagnostics, workflow steps, or opaque metadata.

## Durable Loop Controllers

`agent-task controller` stores domain-agnostic controller state for multi-day
multi-agent loops. The controller record lives outside any single agent-task run
and can reference runs, artifacts, gates, reviews, waits, and human-ready work by
stable ids instead of copying every payload inline.

Create and inspect a controller:

```bash
homeboy agent-task controller init transformer-loop \
  --phase generate \
  --config-version transformer-v1

homeboy agent-task controller status transformer-loop
homeboy agent-task controller list
```

Apply external events, such as CI completion, PR review, human merge, scheduled
wakeups, or artifact availability:

```bash
homeboy agent-task controller apply-event transformer-loop \
  --event-type github.pr.merged \
  --event-key Extra-Chill/homeboy#123 \
  --entity-id pr:123 \
  --payload @event.json
```

The payload may include a `policy` object using
`homeboy/agent-task-loop-controller/v1` action names such as `spawn_task`,
`fan_out`, `spawn_controller`, `spawn_subloop`, `wait_for_controller`, `join`,
`retry`, `request_changes`, `run_gates`, `wait_for_event`, `mark_human_ready`,
`complete`, `abandon`, and `escalate`. Actions with deterministic `dedupe_key`
values are recorded once, so replaying a resumed controller does not duplicate
already-open tasks, child controllers, or PR work.

Nested controller actions are first-class state primitives. `spawn_controller`
and its `spawn_subloop` alias record a parent-visible child controller ref with
the parent loop id, spawning action id, optional entity id, request payload, and
dedupe key. Controller records also include optional `parent_loop_id`,
`parent_action_id`, and `parent_entity_id` fields so spawned child records can
carry their parent provenance directly. `wait_for_controller` puts the parent in
`waiting` state and records a wait that is satisfied when `controller status`
observes the child controller in a terminal state (`completed`, `failed`,
`human_ready`, `abandoned`, or `escalated` by default). Autonomous execution of
pending spawn/wait actions is still owned by #3905; until that lands, these
primitives define the durable schema, idempotency, and status visibility that
the runner will execute.

Mark work as explicitly ready for a human handoff:

```bash
homeboy agent-task controller mark-human-ready transformer-loop \
  --entity-id pr:123 \
  --reason "gates passed and review approved"
```

Gate bundles are represented as structured checks and results. Existing
`--verify` command gates are compatible as the simplest `command` check type;
long-running loops can reuse named bundles across repos and persist normalized
`passed`, `failed`, or `warn` results against a loop, entity, PR, finding, or
run.

## Fixture Backend

The built-in `fixture` backend is intentionally narrow. It exists for smoke
proofs and unit tests, not production task execution. A successful fixture cell
writes:

- `changes.patch` as a non-empty unified diff.
- `agent-result.json` as a structured `homeboy/agent-task-outcome/v1` artifact.
- `transcript.log` as transcript evidence.

Useful fixture `executor.config` fields:

- `artifact_root`: directory where fixture artifacts are written.
- `changed_file`: diff path recorded in the generated patch.
- `metadata`: optional JSON object copied into the fixture outcome metadata.
- `mode`: omit or set to `success`; set to `empty_patch` or `empty_runtime_bundle` for classification checks.

## Output-Driven DAG Phases

`agent-task run-plan` supports backend-neutral output dependencies with a
plan-level `output_dependencies` map keyed by downstream task id. A task with
bindings waits until every declared upstream task has a terminal outcome, selects
values from prior `homeboy/agent-task-outcome/v1` payloads with JSON Pointer,
renders `{{outputs.<name>}}` placeholders into the downstream request, then
dispatches the generated task.

Example:

```json
{
  "schema": "homeboy/agent-task-plan/v1",
  "plan_id": "site-generator-static-fanout",
  "tasks": [
    {
      "schema": "homeboy/agent-task-request/v1",
      "task_id": "idea",
      "executor": { "backend": "provider" },
      "instructions": "Create the GitHub issue for this site idea."
    },
    {
      "schema": "homeboy/agent-task-request/v1",
      "task_id": "design",
      "executor": {
        "backend": "provider",
        "config": {
          "github_issue": "{{outputs.issue_number}}"
        }
      },
      "instructions": "Build the design for GitHub issue #{{outputs.issue_number}}."
    }
  ],
  "output_dependencies": {
    "design": {
      "bindings": {
        "issue_number": {
          "task_id": "idea",
          "path": "/metadata/github/issue_number",
          "required": true
        }
      }
    }
  }
}
```

Supported rendering targets:

- `instructions`
- `inputs`
- `executor.config`
- `workspace.materialization`
- `metadata`
- `expected_artifacts`

If a field is exactly `{{outputs.<name>}}`, Homeboy preserves the selected JSON
value type. Inline placeholders render as strings. If a required binding is
missing, the downstream task is not sent to the provider; the aggregate records a
`skipped` scheduler event, increments `totals.skipped`, and writes a no-op
outcome with diagnostic class `output_dependency_missing`.

Use `depends_on` for ordering-only edges that do not bind values:

```json
{
  "output_dependencies": {
    "static-build": {
      "depends_on": ["design"],
      "bindings": {
        "issue_number": {
          "task_id": "idea",
          "path": "/metadata/github/issue_number"
        }
      }
    }
  }
}
```

## Failure Classifications

The deterministic smoke and existing provider path expose these failure classes:

| Case | Diagnostic/classification |
| --- | --- |
| no-op or empty patch | `agent_task.fixture_empty_patch` plus promotion rejecting `promotion refuses an empty patch artifact` |
| provider timeout | `agent_task.provider_timeout`, `failure_classification: "timeout"` |
| missing secrets/preflight | `agent_task.secret_env_missing`, `failure_classification: "invalid_input"` |
| empty runtime bundle | `agent_task.fixture_empty_runtime_bundle` |
| stale/non-terminal status | `status` annotates running records with `metadata.stale_running` and `metadata.stale_running_reason` |
