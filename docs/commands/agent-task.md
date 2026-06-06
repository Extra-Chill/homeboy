# agent-task

Run provider-neutral task plans through Homeboy's durable agent-task lifecycle.

Homeboy owns durable orchestration and provider-neutral outcomes. Runtime
providers own backend-specific execution. For the provider fanout ownership seam,
see [`docs/architecture/provider-fanout-boundary.md`](../architecture/provider-fanout-boundary.md).

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
- `promote <run-id> --dry-run` resolves the aggregate from the durable run id and reports the selected non-empty patch plus changed files without requiring the operator to look up `aggregate_path` manually.

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
