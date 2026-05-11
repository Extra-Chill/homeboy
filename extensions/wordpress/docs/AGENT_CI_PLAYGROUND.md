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

## Runtime contract

The runner converts the agent config into a single Playground bench workload:

- `component_path` points at the consumer checkout.
- `bundle_path` points at the Data Machine agent bundle.
- `validation_dependencies` are mounted as local plugins or support checkouts.
- `playground_file_mounts` adds fixture files such as the CI driver plugin.
- `bench_env` forwards credentials and the serialized runner config into
  PHP-WASM.
- `transcript_dir` controls where exported conversation artifacts are written.
- `success_requires_pr` can require the agent to open or reuse a pull request.

Inside Playground, `datamachine-agent-workload.php` installs the bundle, configures
the provider, starts the Data Machine flow, drains queued work, records tool
results, exports the transcript, and writes a Homeboy scenario result.

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
