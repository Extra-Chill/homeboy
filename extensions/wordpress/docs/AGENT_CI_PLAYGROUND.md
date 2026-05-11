# Data Machine Agent CI in WordPress Playground

Homeboy can run a Data Machine agent bundle inside WordPress Playground from
GitHub Actions. The reusable workflow gives agent repos one CI entry point for
booting WordPress, loading dependencies, running the agent, collecting
transcripts, and asserting the expected outcome.

## Why Playground is the sandbox

WordPress Playground gives agent CI a disposable WordPress runtime instead of a
long-lived server, local database, or per-repo test harness. Each run starts from
a declared environment and exits with structured Homeboy artifacts.

```text
GitHub Actions workflow
        |
        v
Homeboy WordPress extension
        |
        v
WordPress Playground
  PHP-WASM + embedded SQLite + mounted plugins
        |
        v
Data Machine agent runtime
  abilities, WP-CLI, GitHub tools, transcripts, metrics
```

That shape is useful for agents because the model still interacts with real
WordPress APIs while the host stays small and repeatable:

- No host MySQL, local WordPress install, or component-owned PHPUnit bootstrap.
- Runtime dependencies are mounted into the same Playground site as the bundle.
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
      validation_dependencies: Automattic/agents-api@main,Extra-Chill/data-machine@main,Extra-Chill/data-machine-code@main,WordPress/ai-provider-for-openai@main
      success_requires_pr: true
      engine_data_outputs: '{"static_site_pr_url":"metadata.engine_data.static_site_agent.pr_url"}'
      comment_pr_summary: true
      transcript_artifact_name: static-site-agent-transcript-${{ github.run_id }}
    secrets: inherit
```

The workflow checks out `homeboy-extensions`, installs the WordPress extension
toolchain, mounts validation dependencies under `.ci/<repo>`, builds a runner
config, and calls `wordpress/scripts/agent/run-datamachine-agent.sh`.

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
- Runtime tools through `ability_tools` and `tool_recorders`.
- GitHub scope through `target_repo`, `allowed_repos`, and `app_token_repos`.
- WordPress state through `extra_wp_config_defines`, file mounts, and pre-run
  and post-run hooks.
- Success criteria through `success_requires_pr`, completion outcomes,
  engine-data projections, artifacts, and verifier jobs.

## Runtime contract

The runner converts the agent config into a single Playground bench workload:

- `component_path` points at the consumer checkout.
- `bundle_path` points at the Data Machine agent bundle.
- `validation_dependencies` are mounted as local plugins or support checkouts.
- `playground_file_mounts` adds fixture files such as the CI driver plugin.
- `bench_env` forwards credentials and the serialized runner config into
  PHP-WASM.
- `workload_run_before` and `workload_run_after` attach setup and verifier hooks
  around the agent run inside the same Playground scenario.
- `transcript_dir` controls where exported conversation artifacts are written.
- `success_requires_pr` can require the agent to open or reuse a pull request.
- `tool_recorders` can force tool parameters and project tool results into
  `metadata.engine_data`.
- `engine_key` and `tool_results_key` control where built-in tool capture and
  fallback pull request data are recorded.
- `ability_tools` can expose additional WordPress abilities as tools during the
  agent run.
- `pipeline_step_patches` and `flow_step_patches` can adjust imported bundle
  step configuration before execution.
- `fallback_pull_request` can open a PR when files were written but the agent did
  not explicitly call the PR tool.

Inside Playground, `datamachine-agent-workload.php` installs the bundle, configures
the provider, starts the Data Machine flow, drains queued work, records tool
results, exports the transcript, and writes a Homeboy scenario result.

## Runner config surface

Most consumers should use `.github/workflows/datamachine-agent-ci.yml` rather than
building runner config directly. The reusable workflow forwards these generic
knobs to `run-datamachine-agent.sh`:

- Bundle location: `bundle_path`, `bundle_repo`, `bundle_ref`, `bundle_path_in_repo`.
- Agent selection: `agent_slug`, `pipeline_slug`, `flow_slug`, `prompt`, `provider`, `model`.
- WordPress runtime: `playground_wordpress`, `extra_wp_config_defines`, `extra_playground_file_mounts`, `workload_run_before`, `workload_run_after`.
- GitHub access: `target_repo`, `app_token_repos`, `allowed_repos`, `engine_key`, `tool_results_key`.
- Agent limits: `max_turns`, `step_budget`, `time_budget_ms`.
- Assertions and outputs: `success_requires_pr`, `success_completion_outcomes`, `engine_data_outputs`, `artifact_export_config`, `transcript_artifact_name`.
- Extension points: `extra_required_abilities`, `ability_tools`, `tool_recorders`, `pipeline_step_patches`, `flow_step_patches`, `fallback_pull_request`.

`bundle_repo` is for cross-repo consumers. The shell runner clones the bundle
repository, points `bundle_path` at the cloned bundle inside Playground, and adds
that checkout to the mounted validation dependencies. This lets a repository such
as `agents-api` run a bundle owned by `docs-agent` without copying the bundle or
maintaining a bespoke runner script.

`tool_recorders` are the main migration path for custom bootstrap files that only
wrap GitHub tools. A recorder can attach forced parameters, capture selected input
or output fields, and write them under a stable `metadata.engine_data` key for
later `engine_data_outputs` projection.

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

## Why this can support agent evaluation

The same contract is close to an agent evaluation environment:

- Initial state: Playground blueprint, mounted dependencies, bundle files, and
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

## Related files

- `.github/workflows/datamachine-agent-ci.yml` is the reusable workflow.
- `.github/workflows/README.md` documents workflow inputs and examples.
- `wordpress/scripts/agent/run-datamachine-agent.sh` builds the Playground
  workload config.
- `wordpress/scripts/agent/datamachine-agent-workload.php` runs the agent inside
  WordPress.
- `wordpress/tests/fixtures/datamachine-agent-ci-driver/` provides the stable
  plugin path used for workloads and transcript artifacts.
