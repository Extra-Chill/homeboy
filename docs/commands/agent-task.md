# agent-task

Run provider-neutral task plans through Homeboy's durable agent-task lifecycle.

Homeboy owns durable orchestration and provider-neutral outcomes. Runtime
providers own backend-specific execution. For the provider fanout ownership seam,
see [`docs/architecture/provider-fanout-boundary.md`](../architecture/provider-fanout-boundary.md).

## Boundaries

`agent-task` is split into four operator-facing seams:

- **Lifecycle:** durable run submission, execution, inspection, cancellation, and retry.
- **Cook/review:** workspace task conveniences that compose lifecycle runs with promotion, gates, and PR finalization.
- **Provider:** executor discovery, machine-readable contracts, and redacted auth readiness.
- **Prompt store:** Homeboy-owned markdown prompts for reusable cook/controller input.
- **Loop/controller:** durable multi-agent loop state with on/off, revolutions, handoffs, continuation policy, and resume/stop controls.

## Subcommands

### Lifecycle

| Subcommand | Purpose |
|---|---|
| `run-plan` | Run an `AgentTaskPlan` through extension-declared executor providers. |
| `run <run-id>` | Execute a previously submitted durable run. |
| `run-next` | Claim and execute the oldest queued durable run. |
| `submit` | Persist an agent-task plan and return a durable run id without executing it. |
| `status <run-id>` | Read durable run status. |
| `list [--limit <n>]` | List durable runs, newest first. |
| `active [--limit <n>] [--reconcile [--dry-run]]` | List queued and running durable runs, newest first, or reconcile stale active records. |
| `latest [--limit <n>]` | Show the latest durable run. |
| `logs <run-id>` | Read durable run scheduler events. |
| `artifacts <run-id>` | List artifacts and evidence refs recorded for a completed run. |
| `replay-provider-boundary <run-id> [--task <task-id>]` | Hydrate the latest raw executor input and print provider-boundary fields without relaunching a provider. |
| `cancel <run-id>` | Mark a queued or stale-running durable run as cancelled. |
| `resume <run-id>` | Resume a queued or stale-running durable run. |
| `retry <run-id>` | Submit a fresh durable run from an existing run's plan. |
| `prompts save\|list\|show\|remove` | Manage markdown prompts in Homeboy-owned storage. |

`agent-task list`, `agent-task active`, and `agent-task latest` accept `--limit <n>` to cap discovery output. `agent-task active --reconcile` cancels stale, suspect, or unreconciled active records through the durable lifecycle path; add `--dry-run` to report candidates without mutating records.

`agent-task replay-provider-boundary <run-id>` is a focused inspect/replay path for
provider-boundary debugging. It loads saved `executor-input` evidence, projects the
normalized `runtime_task`, provider config, `runtime_component_paths`,
`runtime_env`, artifact declarations, and package descriptor, then persists the
inspection as `provider-boundary-replay` evidence. Use `--task <task-id>` for
multi-task runs.

### Durable Fanout Batches

Use `agent-task fanout submit-batch` when a caller has many independent tasks and
needs durable lifecycle records for every child run. Homeboy persists one parent
batch record plus one queued `agent-task` run per packet/task, so callers can
drive execution with `agent-task run-next` or existing runner/lab queue loops and
later reconcile status/artifacts without in-process Promise fanout or manual
collation.

```bash
homeboy agent-task fanout submit-batch --input @packets.json --batch-id audit-wave-1
homeboy agent-task run-next
homeboy agent-task fanout status audit-wave-1
homeboy agent-task fanout artifacts audit-wave-1
```

`submit-batch` is intentionally provider-neutral: packets still carry ordinary
`AgentTaskRequest` executor contracts, and child runs use the existing
`agent-task` lifecycle. Dependent workflow plans are rejected because their
ordering and output bindings belong in the existing single-run `fanout submit` /
`run-plan` scheduler path.

### Cook/Review

| Subcommand | Purpose |
|---|---|
| `cook` | Run one workspace task through the patch-artifact handoff workflow. |
| `fanout cook-batch <issue-url>... --repo <repo>` | One-command multi-issue cook setup: derive prompts, create/reuse DMC worktrees, generate PR metadata, and return status/resume commands. |
| `fanout plan\|submit\|run-plan` | Normalize, inspect, or run a batch of independent cooks, each with its own worktree/branch/PR. |
| `fanout submit-batch\|status\|artifacts` | Submit and inspect durable batches of independent `AgentTaskPlan` tasks. |
| `review <run-id>` | Build a durable aggregate review envelope from run state, logs, artifacts, and promotion hints. |
| `promote <source>` | Promote a completed generic patch artifact into a managed worktree. |
| `finalize-pr` | Finalize a green cook run into a review-ready pull request. |
| `gate-feedback` | Convert deterministic gate results into a cook retry or stop decision. |

#### Multi-Issue Cook Batch

Use `agent-task fanout cook-batch` when an operator has a set of GitHub issues
that should each get an isolated branch, worktree, cook run, deterministic gates,
and PR finalization defaults. The command accepts issue URLs directly, derives
the batch-cook plan, queues DMC worktree creation from `origin/main`, and returns
one structured status envelope with the generated plan plus resume commands.

```bash
homeboy agent-task fanout cook-batch \
  --repo homeboy \
  --verify 'cargo test --lib' \
  --backend sandbox \
  --selector wordpress.sandbox-agent-task-executor \
  https://github.com/Extra-Chill/homeboy/issues/6453 \
  https://github.com/Extra-Chill/homeboy/issues/6454
```

Add `--dry-run` to inspect the derived branch/worktree names and batch-cook spec
without creating worktrees. Add `--run-plan` after reviewing provider readiness
to execute the generated batch immediately. When DMC worktree creation is blocked
by an active lock or another queue issue, the output reports `status: blocked`,
lists the exact worktree rows and retry commands, and exits non-zero before any
provider process starts.

The generated plan uses the existing
`homeboy/agent-task-batch-cook-fanout-plan/v1` contract. That means operators can
save the returned `plan` object and resume with:

```bash
homeboy agent-task fanout run-plan --input @batch-cook-plan.json
```

Prompt templates can be customized with `--prompt-template`; placeholders are
`{issue_url}`, `{issue_ref}`, `{repo}`, `{branch}`, and `{worktree}`. PR titles,
commit messages, source refs, and AI disclosure defaults are derived per issue
unless the generated plan is edited before `fanout run-plan`.

## Lab Guardrails

Use global `--lab-only` (alias `--no-local-execution`) with long-running or
patch-producing `agent-task cook` waves that must not execute
provider processes on the controller. If Lab routing cannot select or prepare a
runner, Homeboy fails before local execution instead of falling back.

Use global `--detach-after-handoff` with `--runner <runner-id>` when the Lab job is
expected to outlive the local shell. Homeboy returns after the runner daemon
accepts the job and prints follow/cancel commands instead of waiting for remote
provider completion.

`--force-hot --allow-local-hot` is safe only when local execution on this
controller is intentional. For agent-task waves with concurrency greater than 1
or multiple tasks, Homeboy prints `HOMEBOY_LOCAL_FANOUT_WARNING` before provider
processes start. Compact `agent-task status` includes `execution_location` as
`local` or `runner:<id>`.

### Provider

| Subcommand | Purpose |
|---|---|
| `providers` | List extension-declared executor providers and optional secret/backend readiness. |
| `contract` | Export Homeboy's machine-readable agent-task core contract metadata. |
| `auth` | Configure and inspect provider authentication secrets. |

`agent-task contract --format=json` includes `agent_runtime_handshake`, the
Homeboy-owned extension-facing protocol registry for runtime capability
manifests, readiness checks, resolved execution contracts, materialization plans,
secret env plans, and result/artifact declarations. The registry is generic by
design: extensions provide runtime-specific declarations and results, while
Homeboy owns schema ids, required wire fields, redaction boundaries, and resolved
handoff vocabulary. See
[`docs/architecture/agent-runtime-contract-handshake.md`](../architecture/agent-runtime-contract-handshake.md).

### Prompt Store

`agent-task prompts` stores markdown prompt files under Homeboy's data directory,
not under the current repo/worktree. Save prompt content with inline text,
`@file`, or `-` for stdin, then reference it from `cook` or controller specs
with `prompt:<id>` anywhere a prompt string is accepted.

```bash
homeboy agent-task prompts save issue-123 --input @prompt.md
homeboy agent-task prompts list
homeboy agent-task cook --repo homeboy --prompt prompt:issue-123
```

Existing prompt inputs remain compatible: `--prompt @file`, `--prompt -`, inline
prompt text, repeated `--task`, and `--tasks @tasks.json` still use the existing
resolution behavior unless the prompt string starts with `prompt:`.

### Controller

| Subcommand | Purpose |
|---|---|
| `compile-loop` | Compile a declarative loop definition into an agent-task plan. |
| `loop define\|status\|resume\|stop` | Operate durable defined loops with explicit on/off, revolutions, continuation policy, and handoffs. |
| `controller` | Create, inspect, and resume durable multi-agent loop controller state. |

`agent-task controller run-from-spec <SPEC> --max-actions <N>` is the stable
bounded loop primitive for headless callers. It materializes an optional spec
generator or repo-authored spec, applies `--inputs` and repeated
`--policy-result` envelopes, initializes durable controller state, executes up to
`N` pending controller actions, and returns one persisted status envelope with the
materialized spec, controller initialization report, per-action results, final
controller status, and artifact/status lineage recorded by the normal agent-task
lifecycle.

```bash
homeboy agent-task controller run-from-spec @controller.json \
  --inputs @run-inputs.json \
  --policy-result @policy-result.json \
  --max-actions 5 \
  --dispatch-backend fixture
```

The command stops when no executable action remains, a terminal controller state
is reached, an action fails, or `--max-actions` is reached. `--max-iterations` is
accepted as an alias for `--max-actions` for loop-oriented callers. Execution
remains provider-neutral: controller actions use their declared generic request
shape, and `--dispatch-backend`, `--dispatch-selector`, `--dispatch-model`, and
`--dispatch-provider-config` only provide defaults when an action omits them.

Controller spec materialization commands are portable Lab commands:
`controller from-spec --resume`, `controller run-from-spec`, and
`controller materialize` auto-select the configured default Lab runner when
global `--runner` is omitted. Use `--runner <id>` to choose a specific runner, or
`--force-hot --allow-local-hot` only when controller-machine execution is
intentional.

## Internal Bridge

`agent-task tool` is a hidden provider-runtime bridge. It remains parseable for
runtime adapters that dispatch tool requests through Homeboy, but it is omitted
from `homeboy agent-task --help` and the visible command surface because it is
not an operator-facing workflow.

## Controller Events

`agent-task controller events` is the stable generic primitive for applying an
external event to a durable controller. It accepts the same provider-neutral event
shape as `agent-task controller apply-event` and returns
`homeboy/agent-task-loop-controller-event-result/v1` with the updated controller
record and any actions created by event policy evaluation.

```bash
homeboy agent-task controller events "$loop_id" \
  --event-type task.completed \
  --event-id task-123-completed \
  --event-key task#123 \
  --entity-id entity-123 \
  --payload @event.json
```

Use `events` for downstream integrations that need a stable generic controller
event contract. `apply-event` remains the explicit event-application spelling and
uses the same request and response contract.

## Controller Spec Materialization

`agent-task controller materialize <SPEC> --inputs <JSON>` is the generic seam for
repos that have a loop spec plus per-run inputs, but should not carry repo-local
`build-homeboy-controller-run-spec` scripts. It returns
`homeboy/agent-task-loop-spec-materialization/v1` with a cloned materialized spec;
it does not initialize durable controller state or mutate the source spec file.

The inputs payload may contain `inputs` and `metadata` objects. `inputs` are
merged into each workflow's `inputs`, and `metadata` is merged into top-level spec
metadata. Explicit values override same-named workflow input or metadata keys.

```bash
homeboy agent-task controller materialize \
  @.github/homeboy/controllers/site-loop.json \
  --inputs @run-inputs.json
```

Domains that evaluate their own policies can pass deterministic policy decisions
without teaching Homeboy the policy semantics:

```bash
homeboy agent-task controller materialize \
  @.github/homeboy/controllers/site-loop.json \
  --policy-result @policy-result.json
```

The generic policy result envelope is:

```json
{
  "policy_id": "example-policy",
  "policy_inputs": { "requested_tier": "foundation" },
  "policy_results": { "selected_tier": "foundation", "decision": "hold" },
  "provenance": { "source": "policy-evaluator", "sha256": "..." }
}
```

`policy_id` is required and must be unique per materialization. The other fields
are optional JSON objects. Homeboy projects `policy_inputs` and `policy_results`
under the same keys in every workflow's `inputs`, keyed by `policy_id`, and records
the full envelope under top-level `metadata.policy_materialization`. Homeboy does
not evaluate expressions, choose tiers, generate random seeds, or interpret the
policy result; repo-owned evaluators supply the envelope.

`agent-task controller from-spec` keeps its existing behavior: it reads a complete
repo-authored controller spec, applies dispatch defaults for the spec checkout,
and initializes or resumes durable controller state. Use `materialize` first when
the spec needs deterministic run-input expansion before `from-spec`.

`agent-task controller validate-proof <JSON>` validates a proof, materialized spec,
or controller record without writing controller state. It returns
`homeboy/proof-validation/v1` and exits non-zero when the input is not ready for a
deterministic reviewer handoff.

The validator is provider-neutral. It checks Homeboy proof envelopes for declared
artifact references, reviewer-visible evidence refs, completed gates, and unresolved
proof gaps. For `controller materialize` output, it reuses the generic loop compiler
diagnostics so unsupported controller-only joins, gates, or graph shapes are reported
instead of being silently accepted. For controller records, completed records must
include a terminal outcome and must not retain pending actions.

```bash
homeboy agent-task controller materialize @controller.json --inputs @run.json \
  --output-file materialized.json

homeboy agent-task controller validate-proof @materialized.json
homeboy agent-task controller validate-proof @proof.json
```

## Loop Spec Compilation

`agent-task compile-loop --definition <SPEC>` compiles a declarative loop spec into
an executable `homeboy/agent-task-plan/v1` without submitting or running it. It
accepts Homeboy's native `homeboy/agent-task-loop-definition/v1` shape and the
repo-authored workflow-oriented loop spec shape used by WPSG-style controllers.

Repo-style compilation is intentionally deterministic: workflow ids become task
ids, artifact producers are wired to consumers through `output_dependencies`, and
declared emitted artifacts become `artifact_outputs`. Controller-only sections
such as transition policies, phases, arbitrary actions, initial events, and
entity fan-out are rejected with explicit diagnostics instead of being ignored.

Repo-style specs may also declare an `artifact_graph` edge list. The narrow
compiler support is deliberately limited to direct one-producer, one-consumer
artifact flow:

```json
{
  "artifact_graph": {
    "edges": [
      {
        "artifact_id": "site_plan",
        "from_workflow_id": "plan-site",
        "to_workflow_id": "build-site",
        "required": true
      }
    ]
  }
}
```

`compile-loop` validates graph edges against declared artifacts and workflow
`emits`/`consumes`, then materializes supported edges as `output_dependencies`
and `artifact_outputs`. The controller path exposes the same edge records in
workflow `client_context.artifact_graph_edges` and includes graph producers in
`artifact_dependencies.producer_workflow_ids`. Fan-out graph edges, joins, gates,
and retry policy remain controller-only follow-ups and produce deterministic
diagnostics instead of partial compilation.

## Durable Loops

`agent-task loop` is reserved for defined, durable multi-agent loops. A loop is
not a one-shot PR cook. It persists controller state, tracks whether it is on or
off, counts revolutions, records continuation policy, and resumes or stops
handoffs explicitly.

```bash
homeboy agent-task loop define @.github/homeboy/controllers/site-loop.json \
  --on \
  --revolution-limit 5

homeboy agent-task loop status site-loop
homeboy agent-task loop resume site-loop
homeboy agent-task loop stop site-loop
```

Use `loop define --off` to register or update loop state without executing
handoffs. Use `loop define --on --resume` when the operator wants to initialize
the controller and immediately run pending handoffs. `loop resume` refuses to
run off loops and stops once the persisted or supplied revolution limit is
reached.

## Cook

`agent-task cook` is the one-shot end-to-end PR workflow. It dispatches an agent,
promotes the selected patch into the target worktree, runs deterministic gates,
retries red gates within the configured budget, then commits, pushes, and opens or
updates a PR.

```bash
homeboy agent-task cook \
  --repo sample-plugin \
  --cwd /path/to/worktree \
  --to-worktree sample-plugin@fix-issue \
  --provider-config @provider-config.json \
  --client-context @client-context.json \
  --verify "npm test" \
  --prompt @task.txt
```

Homeboy core treats `--client-context` as an optional opaque JSON object. Client
adapters may include whatever correlation data they need to reconcile their own
notifications or UI state, but Homeboy does not interpret transport-specific
identifiers in core lifecycle state. Provider-specific execution settings belong
in `--provider-config`; durable lifecycle commands remain headless and can be
inspected later with `agent-task status`, `agent-task logs`, or `agent-task review`.

`--backend` selects the generic executor backend, `--dispatch-provider-id` (also
accepted as `--selector`) selects a specific provider id for that backend, and
`--model` is only a provider-owned model override. Provider ids come from
`homeboy agent-task providers`; they are not model names or provider families.

## Fanout/Reconcile

`agent-task fanout` means batch cook: many independent one-shot cooks launched
from one plan. Each cook declares its own target worktree and optional head
branch, runs through the same cook-loop path as a single PR cook, and finalizes
its own pull request when deterministic gates pass.

The public input shape is `homeboy/agent-task-batch-cook-fanout-plan/v1`:

```json
{
  "schema": "homeboy/agent-task-batch-cook-fanout-plan/v1",
  "fanout_id": "audit-batch-2026-06-21",
  "cooks": [
    {
      "cook_id": "finding-a",
      "prompt": "Fix finding A",
      "repo": "homeboy",
      "to_worktree": "homeboy@fix-finding-a",
      "head": "fix/finding-a",
      "verify": ["homeboy test homeboy"]
    }
  ]
}
```

Generic task fanout is not a public operator contract. Existing
`homeboy/agent-task-plan/v1`, `homeboy/agent-task-fanout-plan/v1`, packet arrays,
and `tasks`/`packets` objects are rejected by `agent-task fanout`; internal
schedulers may still use provider-neutral fanout machinery behind a clearer
batch-cook surface.

Cook entries accept dispatch fields such as `prompt`, `tasks`, `repo`, `cwd`,
`workspace`, `task_url`, `backend`, `selector`, `model`, `secret_env`,
`provider_config`, and `client_context`. Review fields include `to_worktree`,
`provider_command`, `verify`, `private_verify`, `max_attempts`, `base`, `head`,
`title`, `commit_message`, `protected_branches`, `ai_tool`, and `ai_used_for`.
Each cook must declare at least one deterministic `verify` or `private_verify`
gate so PR finalization is reviewer-ready.

```bash
homeboy agent-task fanout submit \
  --input @batch-cooks.json \
  --fanout-id audit-batch-2026-06-21 \
  --backend codex \
  --selector openai-codex
```

`fanout submit` prints the exact per-cook commands for runner or operator
execution. `fanout run-plan` executes each cook through the cook-loop service and
returns a batch summary with each child cook result; successful child cooks open
or update their own PRs.

## Headless Fleet-Cooking Review

The authoritative non-chat workflow is the durable `agent-task` lifecycle. Chat
clients, Discord threads, GitHub Actions, cron, and terminal operators can all
submit the same run id, inspect it later, and promote selected artifacts without
depending on transport-local state.

```bash
run_id="homeboy-3357-$(date +%s)"

homeboy agent-task cook \
  --repo homeboy \
  --cwd /path/to/homeboy@fix-issue \
  --to-worktree homeboy@fix-issue-3357-agent-task-non-chat-flow \
  --task-url https://github.com/Extra-Chill/homeboy/issues/3357 \
  --concurrency 4 \
  --attempts 2 \
  --verify "homeboy test homeboy" \
  --run-id "$run_id" \
  --prompt @task.txt

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

## Provider Contracts

`agent-task providers` returns `capability_contract` with Homeboy-owned schema ids
for executor provider manifests, requests, and outcomes. Extensions should read
that metadata, or import the matching `homeboy::core::agent_tasks::provider`
constants, instead of copying schema strings into downstream code. Provider
manifests may omit `schema`, `request_schema`, and `outcome_schema`; Homeboy
defaults them to the current core contract ids.

Use `homeboy agent-task providers --backend <backend> --validate-readiness` to
fail fast when the selected backend is registered but its declared runner
readiness is not usable in the current environment. Lab offload runs this check
on the selected runner before `agent-task cook` dispatches work internally, so a
missing provider executable/config blocks the run before a multi-cell task wave
is queued.

## Repo-Local Gate Tasks

Use `execution_kind: repo_local_gate` for deterministic, repo-local gate
evaluation that should run inside the task workspace without selecting an AI
runtime. The gate executor runs direct `argv` or a relative `script` path without
a shell, rejects paths that escape `workspace.root`, materializes JSON inputs as
`<INPUT_KEY>_PATH` files, materializes declared JSON outputs as
`<OUTPUT_KEY>_PATH` files, and returns `homeboy/agent-task-outcome/v1` with
typed artifacts and `outputs.<key>` payloads.

Minimal task config:

```json
{
  "execution_kind": "repo_local_gate",
  "script": ".github/scripts/evaluate-publish-gate.mjs",
  "inputs": {
    "import_validation_result": "{{outputs.import_validation_result}}",
    "visual_parity_artifact": "{{outputs.visual_parity_artifact}}"
  },
  "artifact_outputs": {
    "static_site_publish_gate": {
      "schema": "example/StaticSitePublishGate/v1",
      "type": "StaticSitePublishGate"
    }
  }
}
```

Gate scripts should read JSON from the generated input path env vars and write
JSON to the generated output path env vars. For example,
`IMPORT_VALIDATION_RESULT_PATH`, `VISUAL_PARITY_ARTIFACT_PATH`, and
`STATIC_SITE_PUBLISH_GATE_PATH`. `node_script` is accepted only as a legacy alias
for existing plans; new plans should use `repo_local_gate` so the contract stays
portable beyond Node.

`agent-task contract --format=json` returns `homeboy/agent-task-core-contract/v1`,
the machine-readable Homeboy-owned contract export for downstream integrations.
It includes schema ids, provider capability metadata, status/failure enum values,
and default redaction policy metadata without naming or depending on any specific
executor provider.

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

When `promote <run-id>` applies a patch, Homeboy records `metadata.latest_promotion`
on the durable run. That status event includes the source run id, source task id,
patch artifact id/path, target worktree, discovered target branch/head when
available, changed files, and an operator notification. `agent-task status
<run-id>` surfaces the latest promotion so callers can tell whether promotion
completed or is blocked without spelunking Lab artifact paths.

When promotion runs without `--dry-run`, each `--verify <command>` is treated as
a visible deterministic gate in the promoted worktree. Promotion reports gate
results as `deterministic_gates[]` using
`homeboy/agent-task-gate-report/v1`. Failed visible gates set promotion
`status: "gate_failed"`, exit nonzero, and include
`failure_evidence.agent_feedback` plus stdout/stderr tails so the next cook
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
`AgentTaskRequest` into a provider-neutral cook feedback decision:

```bash
homeboy agent-task gate-feedback \
  --promotion @promotion.json \
  --source-task @source-task.json \
  --source-run-id "$run_id" \
  --attempt 1 \
  --max-attempts 3 \
  --current-diff @current.diff
```

The command returns `homeboy/agent-task-cook-feedback-report/v1`. Red gates with
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

When the intended checkout already exists on a Lab runner, cook from that
runner-side checkout through `runner exec` instead of forcing a controller-local
hot run:

```bash
homeboy runner exec homeboy-lab \
  --cwd /srv/homeboy/checkouts/homeboy \
  -- homeboy agent-task cook \
    --repo homeboy \
    --cwd /srv/homeboy/checkouts/homeboy \
    --to-worktree homeboy@remote-cook \
    --verify "homeboy test homeboy" \
    --prompt @task.txt
```

`runner exec` marks non-local jobs as runner-hosted, so nested `agent-task cook`
commands pass the non-interactive resource preflight without `--force-hot`.

## Cook Workspaces

`agent-task cook` accepts generic Homeboy workspace inputs and does not
resolve product-specific workspace handles itself.

Use `--cwd <PATH>` when the caller already knows the checkout or worktree path:

```bash
homeboy agent-task cook \
  --repo homeboy \
  --cwd /path/to/homeboy@fix-issue \
  --to-worktree homeboy@fix-issue \
  --verify "homeboy test homeboy" \
  --prompt @task.txt
```

Use `--workspace <ID_OR_PATH>` for a Homeboy-managed task worktree ID or an
existing workspace path:

```bash
homeboy worktree create homeboy --branch fix/issue-123
homeboy agent-task cook \
  --workspace homeboy@fix-issue-123 \
  --to-worktree homeboy@fix-issue-123 \
  --verify "homeboy test homeboy" \
  --prompt @task.txt
```

External workspace managers should resolve their own handles to local paths and
call cook with `--cwd <resolved-path>`.

When `agent-task cook` is Lab-offloaded with a
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

`retry` and `request_changes` are executable generic controller actions.
`retry` queues a new durable agent-task run from the target run's original plan
and records parent/child run lineage on the controller. `request_changes` records
a normalized feedback artifact with `status: "changes_requested"` against the
target run so downstream agents and reviewers can consume the same controller
state without product-specific glue.

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
