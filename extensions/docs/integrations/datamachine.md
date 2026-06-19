# Data Machine Integrations

Homeboy Extensions keeps Data Machine caller examples out of the generic runtime
contract docs. These examples show how Data Machine environments can use generic
Homeboy seams without making Data Machine policy part of the base contract.

## Data Machine Code Promotion Provider

Homeboy core can promote agent-task patch artifacts through an external
workspace provider command. This repo provides a Data Machine Code-backed
provider script for environments that use managed DMC worktrees:

```bash
homeboy agent-task promote aggregate.json \
  --to-worktree repo@branch-slug \
  --provider-command ./scripts/datamachine-code-promotion-provider.sh
```

The provider reads `homeboy/agent-task-promotion-apply-request/v1` JSON on
stdin and writes `homeboy/agent-task-promotion-apply-response/v1` JSON on
stdout. It keeps the DMC-specific `studio wp datamachine-code ...` shellouts in
Homeboy Extensions instead of Homeboy core.

## Runtime Agent Full-Run Callers

Data Machine bundle callers should call `.github/workflows/runtime-agent-full-run.yml`
directly and provide their runtime stack as explicit generic inputs.

```yaml
jobs:
  run-world-creator:
    uses: Extra-Chill/homeboy-extensions/.github/workflows/runtime-agent-full-run.yml@v4
    with:
      runtime_provider: wp-codebox
      runtime_ref: main
      runtime_profile: datamachine-agent-ci
      runtime_profiles: >-
        {"datamachine-agent-ci":{"id":"datamachine-agent-ci","runtime_task_ability":"datamachine/run-agent-bundle","runtime_bundle_ability":"datamachine/run-agent-bundle","ability_requirements":["datamachine/run-agent-bundle"]}}
      runtime_dependencies: '["Automattic/agents-api@main","Extra-Chill/data-machine@main","Extra-Chill/data-machine-code@main"]'
      workload_id: world-creator-day-cycle-flow
      workload_label: Run world-creator Data Machine agent
      target_repo: chubes4/world-of-wordpress
      prompt: ${{ inputs.prompt }}
      runtime_execution: '{"kind":"bundle","source":"bundles/world-creator"}'
      runtime_wordpress_version: beta
      required_abilities: |
        ["datamachine/create-or-update-github-file", "datamachine/daily-memory-write"]
      success_requires_pr: true
      runtime_output_projections: '{"world_creator_pr_url":"metadata.engine_data.world_creator.pr_url"}'
      transcript_artifact_name: world-creator-transcript-${{ github.run_id }}
    secrets: inherit
```

Data Machine-specific ability names, source bundles, tool policy, and runtime
dependencies belong in caller inputs or runtime profiles. Homeboy Extensions only
forwards the generic runtime task contract.
