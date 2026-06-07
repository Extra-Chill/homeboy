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
