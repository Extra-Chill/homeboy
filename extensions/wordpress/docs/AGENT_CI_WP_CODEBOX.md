# Data Machine Agent CI on WP Codebox

Homeboy can run a Data Machine agent bundle inside the WP Codebox WordPress
execution substrate from GitHub Actions. The reusable workflow gives agent repos
one CI entry point for booting WordPress, loading dependencies, running the
agent, collecting transcripts, and asserting the expected outcome.

## Why WP Codebox is the substrate

WP Codebox gives agent CI a disposable WordPress runtime instead of a long-lived
server, local database, or per-repo test harness. Each run starts from a declared
environment and exits with structured Homeboy artifacts.

```text
GitHub Actions workflow
        |
        v
Homeboy WordPress extension
        |
        v
WP Codebox WordPress runtime
  disposable site + mounted plugins
        |
        v
WP Codebox command boundary
        |
        v
Data Machine agent runtime
  abilities, WP-CLI, GitHub tools, transcripts, metrics
```

That shape is useful for agents because the model still interacts with real
WordPress APIs while the host stays small and repeatable:

- No host MySQL, local WordPress install, or component-owned PHPUnit bootstrap.
- Runtime dependencies are mounted into the same WP Codebox WordPress workspace
  as the bundle.
- The reusable workflow can bring the standard WordPress agent runtime stack by
  default, so consumers do not repeat Agents API, Data Machine, Data Machine
  Code, or provider plugin refs in every workflow.
- Blueprints and runner config declare the initial WordPress state.
- Workload steps can call PHP files, WP-CLI commands, and registered abilities.
- Homeboy captures metrics, metadata, artifacts, transcripts, and status checks.
- The site is disposable, so destructive WordPress-side state changes disappear
  with the CI job.

## Reusable workflow

Use `.github/workflows/datamachine-agent-ci.yml` from a consumer workflow:

```yaml
jobs:
  run-static-site-agent:
    uses: Extra-Chill/homeboy-extensions/.github/workflows/datamachine-agent-ci.yml@main
    with:
      bundle_path: bundles/static-site-agent
      agent_slug: static-site-agent
      pipeline_slug: static-site-pipeline
      flow_slug: static-site-manual-flow
      target_repo: chubes4/wp-site-generator
      prompt: ${{ inputs.prompt }}
      success_requires_pr: true
      engine_data_outputs: '{"static_site_pr_url":"metadata.engine_data.static_site_agent.pr_url"}'
      comment_pr_summary: true
      transcript_artifact_name: static-site-agent-transcript-${{ github.run_id }}
    secrets: inherit
```

The workflow checks out `homeboy-extensions`, installs the WordPress extension
toolchain, mounts the standard agent runtime and any additional validation
dependencies under `.ci/<repo>`, builds a runner config, and calls
`wordpress/scripts/agent/run-datamachine-agent.sh`.

The reusable workflow always uses WP Codebox for agent CI. Callers cannot select
the legacy direct runner path.

## Fully Custom Agents

The reusable workflow does not hard-code a specific agent. A consumer supplies a
Data Machine agent bundle, pipeline slug, flow slug, prompt, model, dependency
refs, and runner configuration. The bundle can live in the target repository or
in a separate repository through `bundle_repo`, `bundle_ref`, and
`bundle_path_in_repo`.

That means one repository can own an agent while another repository is the
workspace the agent edits. For example, `docs-agent` can own a documentation
agent bundle while `agents-api` invokes that bundle to write docs and open pull
requests against `Automattic/agents-api`.

Consumers can customize:

- Agent behavior through the bundle manifest, prompts, flows, and pipelines.
- AI provider plugins through `provider_plugin`, including the plugin repo, ref,
  subdirectory path, register function, and Data Machine credential mapping.
- Runtime tools through `ability_tools` and `tool_recorders`.
- GitHub scope through `target_repo`, `allowed_repos`, and `app_token_repos`.
- WordPress state through `extra_wp_config_defines`, file mounts, and pre-run
  and post-run hooks.
- Success criteria through `success_requires_pr`, completion outcomes,
  engine-data projections, artifacts, and verifier jobs.

## WP Codebox contract

The runner converts the agent config into a single WP Codebox sandbox run:

- `component_path` points at the consumer checkout.
- `bundle_path` points at the Data Machine agent bundle.
- The workflow checks out and builds WP Codebox whenever `run_agent` is true.
- `include_agent_runtime_dependencies` mounts the standard Data Machine agent
  runtime before consumer-supplied `validation_dependencies`.
- `wp_codebox_mounts` adds fixture files such as the CI driver plugin.
- `bench_env` forwards credentials and the serialized runner config into the
  sandbox.
- `workload_run_before` and `workload_run_after` attach setup and verifier hooks
  around the agent run inside the same sandbox scenario.
- `transcript_dir` controls where exported conversation artifacts are written.
- `success_requires_pr` can require the agent to open or reuse a pull request.
- `tool_recorders` can force tool parameters and project tool results into
  `metadata.engine_data`.
- `engine_key` and `tool_results_key` control where built-in tool capture and
  fallback pull request data are recorded.
- `runner_workspace` can provision a Data Machine Code worktree. The default
  mode prepends the workspace handle to the agent prompt so current consumers
  can explicitly ask the agent to work in that checkout and open a PR.
- `runner_workspace.expose_to_agent: false` enables runner-owned capture
  mode. It preserves the natural task prompt, keeps workspace tool calls scoped
  to the provisioned handle when those tools are used, then captures final git
  status/diff, commits, pushes, and opens or reuses a fallback PR after the run.
- `ability_tools` can expose additional WordPress abilities as tools during the
  agent run.
- `enable_terminal_actions` exposes terminal actions through the WordPress
  extension's `agent-terminal-actions` helper for runner-owned tools that must
  execute like a real command. In WP Codebox agent runs, the default
  `run_wp_cli` tool posts to the authenticated runtime WP-CLI bridge injected by
  `wp-codebox.agent-sandbox-run`; the bridge executes the command with WP
  Codebox's `wordpress.wp-cli` primitive inside the active WP Codebox runtime
  and returns exit code, stdout, and stderr. Use `wp_cli_tool_name` only when a
  consumer needs a different agent-facing tool name.
- `pipeline_step_patches` and `flow_step_patches` can adjust imported bundle
  step configuration before execution.
- `fallback_pull_request` can open a PR when files were written but the agent did
  not explicitly call the PR tool.

The reusable workflow reports the selected GitHub token path as `auth_mode` and
in the run summary. Same-repo consumers can use the repository-scoped
`github.token` fallback, which posts as `github-actions[bot]`. Central,
cross-repo, and private-target runs should provide Homeboy GitHub App secrets,
set `app_token_repos` to every repository the run must access, and enable
`require_homeboy_app_token` so missing app credentials fail before agent setup.

Inside WP Codebox, `datamachine-agent-workload.php` installs the bundle,
configures the provider, starts the Data Machine flow, drains queued work,
records tool results, exports the transcript, and writes a Homeboy scenario
result. The workflow passes the runner config to the WP Codebox CLI, mounts
provider/runtime plugins, and reads back generated artifacts from the run
workspace.

## Agent-task executor provider

The WordPress extension also exposes a Homeboy-native executor provider contract
for generic agent tasks. This is the stable boundary between Homeboy orchestration
and extension-owned execution backends. Homeboy selects a provider, passes a
generic request, and consumes a generic outcome; backend-specific transport,
runtime boot, sandbox recipes, browser control, and cleanup stay inside the
provider extension.

Provider discovery uses `homeboy/agent-task-executor-provider/v1`:

```json
{
  "schema": "homeboy/agent-task-executor-provider/v1",
  "id": "wordpress.codebox-agent-task-executor",
  "label": "WP Codebox agent task executor",
  "backend": "codebox",
  "command": "node {{extension_path}}/scripts/agent/homeboy-codebox-agent-task-executor.cjs",
  "request_schema": "homeboy/agent-task-request/v1",
  "outcome_schema": "homeboy/agent-task-outcome/v1",
  "request_required_fields": ["schema", "task_id", "executor.backend", "instructions"],
  "outcome_statuses": ["succeeded", "failed", "no_op", "unable_to_remediate", "timeout", "provider_error"],
  "failure_classifications": ["provider", "timeout", "execution_failed"],
  "redacted_metadata_keys": ["secret_env_values", "secretEnvValues", "secrets"],
  "capabilities": ["browser_runtime", "wordpress_sandbox", "workspace_mounts", "workspace_tools", "artifact_materialization", "patch_artifacts", "verification_artifacts", "run_registry", "cleanup_observability", "screenshots", "structured_outcome", "datamachine_bundle_execution"],
  "status": "active",
  "integration_contract": "wp-codebox-cli/agent-task-run",
  "runtime_gap_trackers": []
}
```

Discovery fields mean:

- `id` is the stable provider identifier used in decision evidence and logs.
- `backend` is a short selector for routing. It is not permission to put backend
  imports or assumptions in Homeboy core.
- `command` is the executable adapter entry point. Homeboy passes the generic
  request on stdin or via `HOMEBOY_AGENT_TASK_REQUEST`.
- `request_schema` and `outcome_schema` declare the wire schemas accepted and
  emitted by the provider.
- `request_required_fields` lists the minimum fields a scheduler must populate
  before dispatch.
- `capabilities` declares what orchestration may rely on when selecting a
  provider. New providers should add capabilities only when they can produce the
  corresponding outcome evidence.
- `outcome_statuses`, `failure_classifications`, and `redacted_metadata_keys`
  document the stable vocabulary consumers can use without knowing the backend.
- `status` marks maturity. `active` means the provider uses the stable backend
  contract advertised by `integration_contract`.
- `runtime_gap_trackers` lists active upstream runtime blockers. Closed blockers
  should be removed instead of kept as stale provider metadata.

Generic `homeboy/agent-task-request/v1` requests are backend-neutral. Required
fields are:

- `schema`: exactly `homeboy/agent-task-request/v1`.
- `task_id`: stable id echoed in the outcome and provider decision evidence.
- `executor.backend`: the backend selector advertised by provider discovery.
- `instructions`: the natural-language task prompt for the agent runtime.

Optional request fields should stay generic:

- `group_key` groups related tasks for matrix runs or dashboards.
- `parent_plan_id` links the task to a higher-level Lab plan or issue.
- `executor.model` selects the model; `executor.config` carries provider-owned
  runtime details such as plugin paths, secret env names, runtime mounts, and
  adapter binary paths.
- `inputs` carries structured task context such as title, audit findings, or
  orchestrator metadata.
- `source_refs` carries issue, PR, rig, workload, or artifact references used to
  trace why the task exists.
- `workspace` describes the workspace policy; providers decide how that maps to
  their runtime mounts or worktree tooling.
- `policy` describes read/write/apply expectations in Homeboy terms.
- `limits` carries generic limits such as `task_timeout_seconds` or `timeout_ms`.
- `expected_artifacts` names the evidence the provider should try to return.

Generic `homeboy/agent-task-outcome/v1` outcomes must include:

- `schema`: exactly `homeboy/agent-task-outcome/v1`.
- `task_id`: copied from the request.
- `status`: one of `succeeded`, `failed`, `no_op`, `unable_to_remediate`,
  `timeout`, or `provider_error`.
- `summary`: a human-readable result summary.
- `artifacts`: zero or more `homeboy/agent-task-artifact/v1` records.
- `evidence_refs`: clickable or resolvable references to artifacts, previews,
  logs, transcripts, issues, PRs, runs, or replay bundles.
- `diagnostics`: structured diagnostics with `class`, `message`, and redacted
  `data`.
- `metadata`: public, redacted metadata for dashboards and decision evidence.

Failure classification is separate from status so schedulers can decide whether
to retry, escalate, or mark the task cooked:

- `provider` means provider setup, credentials, dependency boot, or backend
  service access failed before the task could be judged.
- `timeout` means the provider exceeded the task or runner time budget.
- `execution_failed` means the agent/runtime ran but failed, could not remediate,
  or produced an unsuccessful task result.

Provider metadata must be safe to surface in run summaries and PR comments.
Backends may include secret env names for traceability, but values and secret
bags must be redacted recursively. The current public redaction keys are
`secret_env_values`, `secretEnvValues`, and `secrets`; adapters should extend the
provider discovery list if they add another secret-bearing metadata key.

Artifacts and evidence refs are Homeboy-owned shapes even when their content is
backend-specific. Artifacts should include `kind`, and where available `id`,
`path`, `url`, `sha256`, `size_bytes`, and redacted `metadata`. Evidence refs
should include `kind`, `uri`, and an optional `label`. Local filesystem paths are
acceptable for machine-run artifacts; reviewer-facing evidence should point to a
reachable issue, PR, artifact bundle, replay, or committed report.

`homeboy-codebox-agent-task-executor.cjs` is the provider entry point. It keeps
Homeboy core Codebox-agnostic by translating the generic `AgentTaskRequest` into
the stable `wp-codebox/task-input/v1` shape accepted by `wp codebox
agent-task-run`, then translating Codebox output back to `AgentTaskOutcome` with
Homeboy-native artifacts, evidence refs, diagnostics, and failure classifications.

This provider maps the generic request onto WP Codebox's parent runner contract,
not low-level recipe fields. Lab offload and runner transport can select this
provider through discovery, but should keep planning, scheduling, retry, and
dashboard logic tied to the `homeboy/agent-task-request/v1` and
`homeboy/agent-task-outcome/v1` schemas rather than Codebox-specific fields.

Set `executor.config.execution_kind` to `datamachine_bundle` when the task should
run the Data Machine workload path instead of the generic sandbox prompt path.
The provider still emits the same `homeboy/agent-task-outcome/v1` shape, but the
WP Codebox recipe mounts the Homeboy WordPress extension, exports
`HOMEBOY_DATAMACHINE_AGENT_CONFIG`, and runs
`datamachine-agent-workload.php` through the workload wrapper.

The provider shells through `wp-codebox agent-task-run --input-file=<json>
--json`. WP Codebox owns sandbox recipe synthesis, runtime lifecycle, and
artifact capture behind that stable command; Homeboy keeps only request/outcome
adaptation and redaction logic in this extension.

## Runner config surface

Most consumers should use `.github/workflows/datamachine-agent-ci.yml` rather than
building runner config directly. The reusable workflow forwards these generic
knobs to `run-datamachine-agent.sh`:

- Bundle location: `bundle_path`, `bundle_repo`, `bundle_ref`, `bundle_path_in_repo`.
- Agent selection: `agent_slug`, `pipeline_slug`, `flow_slug`, `prompt`, `provider`, `model`.
- Provider plugin: `provider_plugin`, with OpenAI defaults preserved when omitted for `provider: openai`.
- WordPress runtime: `include_agent_runtime_dependencies`, runtime dependency refs, `wp_codebox_wordpress_version`, `wp_codebox_ref`, `extra_wp_config_defines`, `extra_wp_codebox_mounts`, `workload_run_before`, `workload_run_after`.
- GitHub access: `target_repo`, `app_token_repos`, `require_homeboy_app_token`, `allowed_repos`, `engine_key`, `tool_results_key`.
- Agent limits: `max_turns`, `step_budget`, `time_budget_ms`.
- Assertions and outputs: `success_requires_pr`, `success_completion_outcomes`, `engine_data_outputs`, `artifact_export_config`, `transcript_artifact_name`, `replay_bundle_artifact_name`.
- Eval projection: `wp_gym_benchmark_mode` turns missing wp-gym replay/evidence references into errors.
- Extension points: `extra_required_abilities`, `ability_tools`, `tool_recorders`, `enable_terminal_actions`, `wp_cli_tool_name`, `pipeline_step_patches`, `flow_step_patches`, `runner_workspace`, `fallback_pull_request`.

`bundle_repo` is for cross-repo consumers. The shell runner clones the bundle
repository, points `bundle_path` at the cloned bundle inside WP Codebox, and adds
that checkout to the mounted validation dependencies. This lets a repository such
as `agents-api` run a bundle owned by `docs-agent` without copying the bundle or
maintaining a bespoke runner script.

`tool_recorders` are the main migration path for custom bootstrap files that only
wrap GitHub tools. A recorder can attach forced parameters, capture selected input
or output fields, and write them under a stable `metadata.engine_data` key for
later `engine_data_outputs` projection.

`runner_workspace.expose_to_agent` defaults to `true` for backwards
compatibility. Set it to `false` for task-sandbox runs where the agent should see
only the natural task request. Hidden mode enables runner-owned change capture by
default; set `runner_workspace.capture_changes: false` only when a consumer wants
hidden prompt behavior without post-run workspace publication.

`artifact_export_config` writes bundle-file artifacts such as agent daily memory
back to the target repository. Full job artifact JSON is noisier and is disabled
by default; set `artifact_export_config: '{"include_job_artifacts":true}'` for
deep sandbox runs that need committed job metadata beside the bundle.

`provider_plugin` lets callers replace the OpenAI provider preset without
changing the workload. The reusable workflow checks out the configured plugin,
passes the configured register function to `datamachine-agent-workload.php`, and
copies credential env values into `bench_env` for the workload to store as Data
Machine options. OpenAI callers can keep using the default preset:

```yaml
with:
  provider: openai
  model: gpt-5.5
secrets: inherit
```

## Agent-task plan example

The same Data Machine bundle run can be expressed as a Homeboy agent-task plan,
which lets `homeboy agent-task run-plan --plan ...` replace a bespoke workflow
wrapper such as `wp-site-generator`'s site-generation loop:

```json
{
  "schema": "homeboy/agent-task-plan/v1",
  "plan_id": "site-generation-loop",
  "tasks": [
    {
      "schema": "homeboy/agent-task-request/v1",
      "task_id": "site-generation-loop/static-site-agent",
      "group_key": "site-generation-loop",
      "executor": {
        "backend": "codebox",
        "model": "gpt-5.5",
        "config": {
          "execution_kind": "datamachine_bundle",
          "provider": "openai",
          "provider_plugin_paths": ["/components/ai-provider-for-openai"],
          "agents_api": "/components/agents-api",
          "data_machine": "/components/data-machine",
          "data_machine_code": "/components/data-machine-code",
          "homeboy_extensions": "/components/homeboy-extensions/wordpress",
          "wp_codebox_bin": "/components/wp-codebox/packages/wp-codebox/dist/cli.js",
          "bundle_path": "bundles/static-site-agent",
          "agent_slug": "static-site-agent",
          "pipeline_slug": "static-site-pipeline",
          "flow_slug": "static-site-manual-flow",
          "target_repo": "chubes4/wp-site-generator",
          "success_requires_pr": true,
          "engine_data_outputs": {
            "static_site_pr_url": "metadata.engine_data.static_site_agent.pr_url"
          },
          "tool_recorders": [
            {
              "tool": "github/create-pull-request",
              "engine_data_path": "static_site_agent.pr_url"
            }
          ],
          "pipeline_step_patches": [],
          "flow_step_patches": [],
          "transcript_artifact_name": "static-site-agent-transcript",
          "replay_bundle_artifact_name": "static-site-agent-replay"
        }
      },
      "instructions": "Generate the requested WordPress site and open or reuse a pull request with the materialized source.",
      "inputs": {
        "title": "Run static site agent"
      },
      "limits": {
        "task_timeout_seconds": 1800
      },
      "expected_artifacts": [
        "datamachine-transcript",
        "datamachine-replay-bundle",
        "datamachine-pull-request"
      ]
    }
  ]
}
```

Run it with:

```bash
homeboy agent-task run-plan --plan .homeboy/plans/site-generation-loop.json
```

For bundle repositories outside the target checkout, use `bundle_repo`,
`bundle_ref`, and `bundle_path_in_repo` instead of a local `bundle_path`. Runtime
stack mounts, overlays, provider plugins, tool recorders, engine-data outputs,
artifact exports, transcript settings, and flow/pipeline step patches all remain
provider config so Homeboy core does not need GitHub Actions-specific wiring.

Generic providers should provide a full plugin object and map Data Machine option
names to generic provider secret env names:

```yaml
with:
  provider: example-provider
  model: example-model
  provider_plugin: |
    {
      "repo": "Example/example-ai-provider",
      "ref": "main",
      "path": ".",
      "register_function": "Example\\AiProvider\\register_provider",
      "credentials": {
        "connectors_ai_example_api_key": "PROVIDER_SECRET_1"
      }
    }
secrets:
  PROVIDER_SECRET_1: ${{ secrets.EXAMPLE_PROVIDER_API_KEY }}
```

## Outputs and assertions

The reusable workflow exposes stable outputs:

- `job_status` from the workload metadata.
- `transcript_json` path when transcript export is enabled.
- `transcript_summary` path when a summary artifact is available.
- `engine_data_json`, a caller-declared projection of agent engine data.

Use `success_requires_pr: true` when the task is only successful if the agent
opens or reuses a pull request. Use `engine_data_outputs` for additional
consumer-specific assertions such as a generated PR URL, published artifact path,
or scenario result field.

Set `replay_bundle_artifact_name` to publish a redacted replay bundle alongside
the run. The bundle snapshots the scenario envelope, initial WordPress state,
prompt, runner config, provider/model/seed metadata, transcript/action log
references, and grader metadata. The runner also attaches
`artifacts.replay_bundle.path` and `metadata.playground_review` to the scenario
result JSON so downstream JSONL publishers can link failure rows back to the
bundle. The metadata key keeps the historical name until the artifact schema is
renamed.

The runner also projects each Data Machine agent scenario into
`metadata.wp_gym_eval_row`. This is a compatibility scaffold for the canonical
wp-gym row tracked in https://github.com/Automattic/wp-gym/issues/117. The row
uses `schema_name: "wp-gym.eval_artifact_row"`, `schema_version: 0`, and
`projection_version: "homeboy-extensions.compat.1"` until the upstream wp-gym
validator lands. Evaluation semantics live under `evaluation`; Homeboy, Data
Machine, Data Machine Code, GitHub workflow/PR, verifier, policy, and artifact
references live under `orchestration`. Missing replay, transcript, runtime trace,
verifier, policy, workflow, PR, or artifact evidence is reported in
`compatibility_gaps`. Set reusable workflow input `wp_gym_benchmark_mode: true`
or runner config `wp_gym_eval.benchmark_mode: true` to turn those gaps into
projection errors for benchmark-mode jobs.

Every WP Codebox-backed Data Machine agent result includes generic runner
evidence at `metadata.runner_evidence` using schema
`homeboy/datamachine-agent-runner-evidence/v1`. This report is benchmark-neutral
and records the effective prompt/instruction surface, tool/ability summaries,
workspace refs and dirty state, provider/model/runtime IDs, WP Codebox artifact
paths, and redaction policy markers. Secret-like keys and inline token/password
markers are redacted by default using `[redacted]`.

When WP Codebox emits a runtime reference manifest path, the runner preserves it
as `metadata.wp_codebox.runtime_reference_manifest`, exposes the path as
`artifacts.wp_codebox_runtime_reference_manifest`, and links it from
`metadata.evidence_references.references.wp_codebox_runtime_reference_manifest`.
The preservation path accepts compatible manifest path names without requiring a
final WP Codebox #222 schema, and redacts secret-like manifest fields before
embedding payload data in reports.

Final-state review URLs are only emitted when the caller supplies a hosted review
URL in runner config or scenario metadata. The default bundle records
`playground_review.available=false` and `final_state.available=false` rather than
pretending an encoded initial blueprint is a final-state replay.

## Why this can support agent evaluation

The same contract is close to an agent evaluation environment:

- Initial state: WordPress blueprint, mounted dependencies, bundle files, and
  configured WordPress constants.
- Actions: registered abilities, WP-CLI commands, GitHub tools, and Data Machine
  pipeline steps.
- Observations: tool results, WordPress state, logs, transcripts, metrics, and
  artifacts.
- Rewards: passing tests, expected WordPress state, PR creation, artifact shape,
  benchmark movement, and policy checks.
- Termination: max turns, step budget, time budget, or flow completion.

For reinforcement-learning style use, prefer explicit reward functions over weak
proxy goals. For example, reward a verified diff plus passing checks rather than
only rewarding that a pull request URL exists. Mock or scope external services
when reproducibility matters.

WordPress grader workloads should return the shared reward payload used by the
bench runner:

```json
{
  "success": false,
  "reward": 0.75,
  "done": true,
  "grade": {
    "max_score": 1,
    "score": 0.75,
    "checks": [
      { "id": "valid_block_markup", "passed": true, "score": 0.4, "max_score": 0.4 }
    ]
  }
}
```

Use `success` for binary pass/fail completion. Use `reward` and
`grade.checks` for partial credit, with `reward` bounded from `0` to `1` and
per-check scores bounded by their `max_score`. The runner exposes aggregate
metrics such as `reward_mean`, while the structured details remain in
`metadata.grade` for JSONL consumers.

Scenario manifests can keep visible grading and hidden verification separate:

```json
{
  "grader_file": "graders/visible-grade.php",
  "verifier_files": ["verifiers/no-grader-mutation.php"],
  "forbidden_mutations": ["graders/**", "scenarios/**"],
  "required_active_plugins": ["data-machine/data-machine.php"]
}
```

The bench runner appends verifier PHP files after the manifest `run` steps and
grader file. Verifiers return the same reward payload shape as graders, so
hidden policy failures appear in `metadata.grade` and downstream JSONL rows.
Use graders for task-visible success criteria, verifiers for anti-tamper checks
and environment policy checks, and the `forbidden_mutations` /
`required_active_plugins` metadata fields to describe the policy being enforced.

## Related files

- `.github/workflows/datamachine-agent-ci.yml` is the reusable workflow.
- `.github/workflows/README.md` documents workflow inputs and examples.
- `wordpress/scripts/agent/run-datamachine-agent.sh` builds the WordPress
  workload config and dispatches the selected runtime.
- `wordpress/scripts/agent/datamachine-agent-workload.php` runs the agent inside
  WordPress.
- `wordpress/tests/fixtures/datamachine-agent-ci-driver/` provides the stable
  plugin path used for workloads and transcript artifacts.
