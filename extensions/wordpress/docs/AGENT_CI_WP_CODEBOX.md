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
- `playground_file_mounts` adds fixture files such as the CI driver plugin. The
  setting name is inherited from the previous direct-runner contract and is kept
  as part of the current workflow input surface.
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
- `enable_terminal_actions` exposes host-side terminal actions through the
  WordPress extension's `agent-terminal-actions` helper for runner-owned tools
  that must execute like a real shell command. The default `run_wp_cli` tool runs
  a `wp_cli` action as `wp ...` through `bash -lc` with the runtime root as the
  command boundary and returns exit code, stdout, and stderr. Use
  `wp_cli_tool_name` only when a consumer needs a different agent-facing tool
  name.
- `pipeline_step_patches` and `flow_step_patches` can adjust imported bundle
  step configuration before execution.
- `fallback_pull_request` can open a PR when files were written but the agent did
  not explicitly call the PR tool.

Inside WP Codebox, `datamachine-agent-workload.php` installs the bundle,
configures the provider, starts the Data Machine flow, drains queued work,
records tool results, exports the transcript, and writes a Homeboy scenario
result. The workflow passes the runner config to the WP Codebox CLI, mounts
provider/runtime plugins, and reads back generated artifacts from the run
workspace.

## Runner config surface

Most consumers should use `.github/workflows/datamachine-agent-ci.yml` rather than
building runner config directly. The reusable workflow forwards these generic
knobs to `run-datamachine-agent.sh`:

- Bundle location: `bundle_path`, `bundle_repo`, `bundle_ref`, `bundle_path_in_repo`.
- Agent selection: `agent_slug`, `pipeline_slug`, `flow_slug`, `prompt`, `provider`, `model`.
- Provider plugin: `provider_plugin`, with OpenAI defaults preserved when omitted for `provider: openai`.
- WordPress runtime: `include_agent_runtime_dependencies`, runtime dependency refs, `playground_wordpress`, `wp_codebox_ref`, `extra_wp_config_defines`, `extra_playground_file_mounts`, `workload_run_before`, `workload_run_after`.
- GitHub access: `target_repo`, `app_token_repos`, `allowed_repos`, `engine_key`, `tool_results_key`.
- Agent limits: `max_turns`, `step_budget`, `time_budget_ms`.
- Assertions and outputs: `success_requires_pr`, `success_completion_outcomes`, `engine_data_outputs`, `artifact_export_config`, `transcript_artifact_name`, `replay_bundle_artifact_name`.
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
