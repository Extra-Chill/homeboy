# WP Codebox runtime workflow migration

`runtime-agent-full-run.yml` is the runtime-neutral GitHub Actions shell. New
WordPress/WP Codebox callers should keep WP-specific setup in their caller
workflow or a local wrapper job, then invoke the generic shell with
`runtime: wp-codebox` and canonical input names.

## Canonical call shape

```yaml
jobs:
  run-wp-codebox-agent:
    uses: Extra-Chill/homeboy-extensions/.github/workflows/runtime-agent-full-run.yml@v4
    with:
      runtime: wp-codebox
      runtime_ref: main
      profile: example-agent-ci
      runtime_profiles: >-
        {"example-agent-ci":{"id":"example-agent-ci","runtime_task_ability":"example/run-task","runtime_bundle_ability":"example/run-agent-bundle","capabilities":["ability_execution","agent_bundle_execution"],"runtime_execution_contracts":{"bundle":{"ability_field":"runtime_bundle_ability","required_capabilities":["agent_bundle_execution"]}},"ability_requirements":["example/run-agent-bundle"]}}
      runtime_dependencies: '["Example/runtime-plugin@main"]'
      workload_id: example-agent-flow
      target_repo: Example/project
      runtime_execution: '{"kind":"bundle","source":"bundles/example-agent"}'
      runtime_wordpress_version: beta
      extra_wp_config_defines: '{"EXAMPLE_RUNTIME_MODE":"primary"}'
      runtime_mounts: '["${{ github.workspace }}/.ci/example-runtime-plugin:/wordpress/wp-content/plugins/example-runtime-plugin:readonly"]'
      required_abilities: '["example/run-agent-bundle"]'
      runtime_output_projections: '{"example_pr_url":"metadata.engine_data.example.pr_url"}'
      transcript_artifact_name: example-agent-transcript-${{ github.run_id }}
    secrets: inherit
```

## Deprecated aliases

These aliases are compatibility-only for existing callers. New wrappers should
emit the canonical names.

| Deprecated alias | Canonical input |
| --- | --- |
| `runtime_provider` | `runtime` |
| `backend` | `runtime` |
| `runtime_profile` | `profile` |
| `tool_policy` | `tool_profile` |
| `tool_recorders` | `evidence_projections` for generic projections; keep `tool_recorders` only when a WP Codebox runner still requires its legacy forced-parameter behavior. |

The old `codebox` runtime id is not a generic runtime id. Use `wp-codebox`.

## Wrapper guidance

A product-owned wrapper should translate product vocabulary to the canonical
inputs above before calling `runtime-agent-full-run.yml`. Keep ability names,
WordPress bootstrap hooks, plugin mounts, and WP Codebox artifact declaration
schemas in that wrapper or caller repository. The generic workflow should only
see selected runtime inputs, generic execution descriptors, and projection maps.
