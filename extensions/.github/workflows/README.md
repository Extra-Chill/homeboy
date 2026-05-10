# Data Machine Agent CI reusable workflow

`datamachine-agent-ci.yml` wraps the common GitHub Actions shape for running a
Data Machine agent bundle in WordPress Playground. Consumers provide bundle and
flow identifiers, dependency refs, a prompt, and optional output projections.

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

## Inputs worth calling out

- `validation_dependencies` accepts `OWNER/REPO@REF` entries and checks each out under `.ci/<repo>`.
- `bundle_path` is resolved relative to the consumer checkout.
- `app_token_repos` scopes the Homeboy GitHub App token and defaults to `target_repo`.
- `dry_run` is intended for workflow smoke tests only; production consumers should leave it `false`.
- `transcript_artifact_name` controls artifact upload. An empty value skips upload.
