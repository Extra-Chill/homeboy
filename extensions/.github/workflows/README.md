# Data Machine Agent CI reusable workflow

## Actions Run Once

`actions-run-once.yml` is a reusable idempotency gate for workflows that must
process one evidence packet exactly once. Call it in `check` mode before the
expensive job, then call it in `mark` mode after validation and fan-out reach
the point that must not repeat.

Use a stable evidence key built from the caller domain, such as
`source_repo/source_pr/source_head_sha/site_slug/fanout_kind`. The workflow
normalizes the key to a GitHub Actions cache marker and returns `should_run`.
Pair the caller workflow with a concurrency group derived from the same key so
parallel events cannot both pass the check before the marker is saved.

```yaml
jobs:
  preflight:
    uses: Extra-Chill/homeboy-extensions/.github/workflows/actions-run-once.yml@main
    with:
      mode: check
      idempotency_key: static-validation/${{ github.repository }}/${{ github.event.pull_request.number }}/${{ github.event.pull_request.head.sha }}/${{ matrix.site }}

  validate:
    needs: preflight
    if: needs.preflight.outputs.should_run == 'true'
    runs-on: ubuntu-latest
    steps:
      - run: ./validate-and-dispatch-fanout

  mark:
    needs: validate
    if: always() && needs.preflight.outputs.should_run == 'true'
    uses: Extra-Chill/homeboy-extensions/.github/workflows/actions-run-once.yml@main
    with:
      mode: mark
      idempotency_key: static-validation/${{ github.repository }}/${{ github.event.pull_request.number }}/${{ github.event.pull_request.head.sha }}/${{ matrix.site }}
```

`mark` should run only after the evidence has been consumed or fan-out has been
scheduled. If the caller wants failed validations to be retryable, mark only on
success. If the caller wants one validation attempt per immutable evidence key,
mark with `if: always()` after the validation job starts producing reviewer
evidence.

`datamachine-agent-ci.yml` wraps the common GitHub Actions shape for running a
Data Machine agent bundle in a disposable WordPress execution substrate.
Consumers provide bundle and flow identifiers, a prompt, and optional output
projections. Agent runs use the WP Codebox substrate; the legacy direct
Playground runner is no longer selectable by callers.
See [`wordpress/docs/AGENT_CI_WP_CODEBOX.md`](../../wordpress/docs/AGENT_CI_WP_CODEBOX.md)
for the WP Codebox contract, runtime surface, and evaluation notes.

The workflow intentionally stays a thin GitHub Actions wrapper around scripts in
`.github/scripts/datamachine-agent-ci/`. Those scripts own GitHub token scope
validation, dependency checkout planning/materialization, runtime setup, runner
config synthesis, engine-data projection, artifact path resolution, and comment
payload preparation. Keep consumer-specific semantics in workflow inputs and
bundle files rather than baking domain policy into those generic helpers.

The reusable workflow exposes `engine_data_json` as one combined JSON object.
Dynamic per-key `workflow_call` outputs are not possible in GitHub Actions, so
callers should parse this object in their own workflow when they need named
values. It also exposes `auth_mode`, which is `homeboy_app_token` when the
workflow generated and used a Homeboy GitHub App installation token, or
`github_token_fallback` when it used the repository-scoped GitHub Actions token.

## GitHub auth modes

The workflow keeps two GitHub authentication modes:

- Same-repo consumer workflows can rely on the built-in `github.token` fallback.
  Branches, commits, PRs, and comments created this way appear as
  `github-actions[bot]`, and `auth_mode` is `github_token_fallback`.
- Central, cross-repo, or private-target workflows should provide
  `HOMEBOY_APP_ID` and `HOMEBOY_APP_PRIVATE_KEY`, set `app_token_repos` to the
  repositories the run must access, and set `require_homeboy_app_token: true` so
  missing app credentials fail before expensive setup. In this mode, `auth_mode`
  is `homeboy_app_token`.

The run summary includes the selected auth mode, target repository, token scope,
and whether the caller required a Homeboy App token. Tokens are never printed.

## Example

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

## World Creator agent example

Workflows that need additional WordPress sandbox configuration can pass
JSON-string inputs through to the runner config without changing the reusable
workflow. This shape covers the `world-of-wordpress` migration that needs MDI
primary mode, a `db.php` drop-in mount, pre-run bootstrap work, daily memory,
and extra ability assertions.

```yaml
jobs:
  run-world-creator:
    uses: Extra-Chill/homeboy-extensions/.github/workflows/datamachine-agent-ci.yml@v3
    with:
      bundle_path: bundles/world-creator
      agent_slug: world-creator
      pipeline_slug: world-creator-pipeline
      flow_slug: world-creator-day-cycle-flow
      target_repo: chubes4/world-of-wordpress
      prompt: ${{ inputs.prompt }}
      validation_dependencies: chubes4/world-of-wordpress@main,chubes4/markdown-database-integration@main
      agent_runtime: wp-codebox
      agent_runtime_ref: main
      runtime_wordpress_version: beta
      max_turns: 16
      step_budget: 20
      time_budget_ms: 900000
      extra_wp_config_defines: |
        {
          "MARKDOWN_DB_MODE": "primary",
          "MARKDOWN_DB_CONTENT_DIR": "/wordpress/wp-content/plugins/world-of-wordpress/content"
        }
      runtime_mounts: |
        [
          "${{ github.workspace }}/.ci/markdown-database-integration/db.php:/wordpress/wp-content/db.php:readonly"
        ]
      workload_run_before: |
        [
          { "type": "php", "file": "world-creator-bootstrap.php" }
        ]
      daily_memory_enabled: true
      extra_required_abilities: |
        ["datamachine/create-or-update-github-file", "datamachine/daily-memory-write"]
      success_requires_pr: true
      engine_data_outputs: '{"world_creator_pr_url":"metadata.engine_data.world_creator.pr_url"}'
      transcript_artifact_name: world-creator-transcript-${{ github.run_id }}
    secrets: inherit
```

## Inputs worth calling out

- Agent CI runs through the selected `agent_runtime`. Today the only supported value is `wp-codebox`, and the workflow checks out/builds `Automattic/wp-codebox` for that runtime.
- `agent_runtime_ref` controls the selected runtime ref.
- `include_agent_runtime_dependencies` defaults to `true` and checks out the standard WordPress agent runtime stack: `Automattic/agents-api`, `Extra-Chill/data-machine`, `Extra-Chill/data-machine-code`, and the provider plugin.
- `agents_api_ref`, `data_machine_ref`, `data_machine_code_ref`, and `openai_provider_ref` control runtime dependency refs. `openai_provider_ref` defaults to `trunk` for the built-in OpenAI preset.
- `provider_plugin` is a JSON object with `repo`, `ref`, `path`, `register_function`, and `credentials` keys. When `provider: openai`, an empty object preserves the existing OpenAI provider defaults.
- `validation_dependencies` accepts additional `OWNER/REPO@REF` entries and checks each out under `.ci/<repo>`. Entries without `@REF` use the repository default branch.
- `bundle_path` is resolved relative to the consumer checkout.
- `bundle_repo`, `bundle_ref`, and `bundle_path_in_repo` let a consumer run against a bundle stored in another repository. The runner clones that repository before mounting the bundle into the WordPress substrate.
- `app_token_repos` scopes the Homeboy GitHub App token and defaults to `target_repo`. Use it when the workflow needs app-token access to more than the target repository.
- `require_homeboy_app_token` fails before agent setup when Homeboy App credentials are missing. Enable it for central, cross-repo, and private-target runs; leave it false for same-repo consumers that intentionally use `github.token` fallback.
- `allowed_repos` is a JSON array of `OWNER/REPO` entries exposed to the injected GitHub profile. It defaults to `[target_repo]`.
- `engine_key` and `tool_results_key` control where built-in GitHub tool captures are written in `metadata.engine_data`.
- `dry_run` is intended for workflow smoke tests only; production consumers should leave it `false`.
- `transcript_artifact_name` controls artifact upload. An empty value skips upload.
- `extra_wp_config_defines` must be a JSON object and is merged into the runner config `wp_config_defines`.
- `runtime_mounts` adds selected-runtime mounts. It must be a JSON array.
- `runtime_overlays` forwards runtime overlay entries to the runner config. It must be a JSON array.
- `workload_run_before`, `workload_run_after`, and `extra_required_abilities` must be JSON arrays.
- `workload_run_after` runs post-agent verifier hooks in the same WordPress scenario, so consumers can assert the agent left WordPress in a valid state.
- `ability_tools` adds WordPress ability-backed tools to the agent loop. It must be a JSON array.
- `tool_recorders` configures tool-result projection, forced parameters, and engine-data capture. It must be a JSON array.
- `pipeline_step_patches` and `flow_step_patches` modify imported bundle step config before the flow runs. They must be JSON arrays.
- `runner_workspace` provisions a WP Codebox-managed runner workspace before the agent runs. By default it is agent-visible: the runner prepends the workspace handle and branch to the prompt and forces workspace tools to that handle. Set `expose_to_agent: false` for runner-owned capture mode; the natural prompt is preserved, workspace tools remain scoped when used, and the runner publishes captured workspace changes through the WP Codebox runner publication API after completion.
- `runner_workspace.capture_changes` defaults to `true` only when `expose_to_agent: false`; set it explicitly to disable hidden-mode publication or to enable runner-owned capture while still exposing the workspace handle.
- `verification_commands` and `drift_checks` run through the WP Codebox runner workspace command API, so remote runner workspaces do not require Homeboy to know backend-local paths.
- If runner-owned workspace publication is unavailable or fails, the run fails as `write_without_pr`; Homeboy does not compose alternate PR fallback calls.

## External bundle and tool recording example

Consumers such as `docs-agent` can keep the agent bundle in one repository while
running it against another repository. The reusable workflow handles the bundle
checkout and passes tool recorder config to the WordPress runner.

```yaml
jobs:
  run-docs-agent:
    uses: Extra-Chill/homeboy-extensions/.github/workflows/datamachine-agent-ci.yml@v3
    with:
      bundle_path: bundles/docs-agent
      bundle_repo: https://github.com/Automattic/docs-agent.git
      bundle_ref: main
      bundle_path_in_repo: bundles/docs-agent
      agent_slug: docs-agent
      pipeline_slug: docs-agent-pipeline
      flow_slug: docs-agent-flow
      target_repo: Automattic/agents-api
      app_token_repos: Automattic/agents-api,Automattic/docs-agent
      require_homeboy_app_token: true
      allowed_repos: '["Automattic/agents-api", "Automattic/docs-agent"]'
      engine_key: docs_agent
      tool_results_key: github_tool_results
      tool_recorders: |
        [
          {
            "tool": "create_or_update_github_file",
            "engine_data_key": "github_tool_results",
            "forced_parameters": {
              "repo": "Automattic/agents-api",
              "branch": "${{ github.ref_name }}"
            },
            "fields": {
              "path": "parameters.path",
              "commit_sha": "result.commit.sha"
            }
          }
        ]
      success_requires_pr: false
      transcript_artifact_name: docs-agent-transcript-${{ github.run_id }}
    secrets: inherit
```

Use tool recorders when a consumer currently has a custom bootstrap file whose
only job is to force GitHub tool parameters or copy selected tool results into
`metadata.engine_data`.

## Provider plugin examples

OpenAI remains the compatibility preset. Existing callers can keep omitting
`provider_plugin` and pass `OPENAI_API_KEY` through inherited secrets:

```yaml
jobs:
  run-agent:
    uses: Extra-Chill/homeboy-extensions/.github/workflows/datamachine-agent-ci.yml@v3
    with:
      bundle_path: bundles/example-agent
      agent_slug: example-agent
      pipeline_slug: example-pipeline
      flow_slug: example-flow
      target_repo: Extra-Chill/example
      provider: openai
      model: gpt-5.5
    secrets: inherit
```

Non-OpenAI providers supply the plugin checkout and Data Machine credential
mapping explicitly. Map each Data Machine option to one of the generic provider
secret env names, then pass that secret in the reusable workflow call:

```yaml
jobs:
  run-agent:
    uses: Extra-Chill/homeboy-extensions/.github/workflows/datamachine-agent-ci.yml@v3
    with:
      bundle_path: bundles/example-agent
      agent_slug: example-agent
      pipeline_slug: example-pipeline
      flow_slug: example-flow
      target_repo: Extra-Chill/example
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
