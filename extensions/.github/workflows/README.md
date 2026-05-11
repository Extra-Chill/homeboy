# Data Machine Agent CI reusable workflow

`datamachine-agent-ci.yml` wraps the common GitHub Actions shape for running a
Data Machine agent bundle in WordPress Playground. Consumers provide bundle and
flow identifiers, dependency refs, a prompt, and optional output projections.
See [`wordpress/docs/AGENT_CI_PLAYGROUND.md`](../../wordpress/docs/AGENT_CI_PLAYGROUND.md)
for the full Playground sandbox model, runtime contract, and evaluation notes.

The reusable workflow exposes `engine_data_json` as one combined JSON object.
Dynamic per-key `workflow_call` outputs are not possible in GitHub Actions, so
callers should parse this object in their own workflow when they need named
values.

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
      validation_dependencies: Automattic/agents-api@main,Extra-Chill/data-machine@main,Extra-Chill/data-machine-code@main,WordPress/ai-provider-for-openai@main
      success_requires_pr: true
      engine_data_outputs: '{"static_site_pr_url":"metadata.engine_data.static_site_agent.pr_url"}'
      comment_pr_summary: true
      transcript_artifact_name: static-site-agent-transcript-${{ github.run_id }}
    secrets: inherit
```

## World Creator migration example

Workflows that need additional WordPress Playground configuration can pass
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
      validation_dependencies: chubes4/world-of-wordpress@main,chubes4/markdown-database-integration@main,Automattic/agents-api@main,Extra-Chill/data-machine@main,Extra-Chill/data-machine-code@main,WordPress/ai-provider-for-openai@main
      playground_wordpress: beta
      max_turns: 16
      step_budget: 20
      time_budget_ms: 900000
      extra_wp_config_defines: |
        {
          "MARKDOWN_DB_MODE": "primary",
          "MARKDOWN_DB_CONTENT_DIR": "/wordpress/wp-content/plugins/world-of-wordpress/content"
        }
      extra_playground_file_mounts: |
        [
          {
            "from_dependency": "markdown-database-integration",
            "from": "db.php",
            "to": "/wordpress/wp-content/db.php"
          }
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

- `validation_dependencies` accepts `OWNER/REPO@REF` entries and checks each out under `.ci/<repo>`.
- `bundle_path` is resolved relative to the consumer checkout.
- `bundle_repo`, `bundle_ref`, and `bundle_path_in_repo` let a consumer run against a bundle stored in another repository. The runner clones that repository before mounting the bundle into Playground.
- `app_token_repos` scopes the Homeboy GitHub App token and defaults to `target_repo`.
- `allowed_repos` is a JSON array of `OWNER/REPO` entries exposed to the injected GitHub profile. It defaults to `[target_repo]`.
- `dry_run` is intended for workflow smoke tests only; production consumers should leave it `false`.
- `transcript_artifact_name` controls artifact upload. An empty value skips upload.
- `extra_wp_config_defines` must be a JSON object and is merged into the runner config `wp_config_defines`.
- `extra_playground_file_mounts`, `workload_run_before`, and `extra_required_abilities` must be JSON arrays.
- `ability_tools` adds WordPress ability-backed tools to the agent loop. It must be a JSON array.
- `tool_recorders` configures tool-result projection, forced parameters, and engine-data capture. It must be a JSON array.
- `pipeline_step_patches` and `flow_step_patches` modify imported bundle step config before the flow runs. They must be JSON arrays.
- `fallback_pull_request` opens or reuses a PR when files were written but the agent did not call the PR tool. It must be a JSON object.

## External bundle and tool recording example

Consumers such as `docs-agent` can keep the agent bundle in one repository while
running it against another repository. The reusable workflow handles the bundle
checkout and passes tool recorder config to the Playground runner.

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
      allowed_repos: '["Automattic/agents-api", "Automattic/docs-agent"]'
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
