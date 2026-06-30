# Homeboy Extensions reusable workflows

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

## Runtime Agent Full Run

`runtime-agent-full-run.yml` is the generic reusable workflow for a complete
runtime-backed agent run. It owns the GitHub Actions orchestration that callers
should not repeat: dependency materialization, provider/runtime setup, runner
workspace lifecycle, transcript/replay artifact upload, PR comments,
output/evidence projections, `callback_data_json`, before/after workload hooks,
extra mounts, and runtime config defines. The runtime execution primitive is
`runtime-agent-ci/scripts/run-headless-loop.cjs`, which materializes task
requests, runs the selected runtime provider, validates gates, and emits durable
JSON results/events outside GitHub Actions.

The generic workflow requires callers to provide their domain runtime profile,
runtime component dependencies, required abilities, and runtime task/execution
descriptor. Homeboy Extensions assumes the runtime provider contract only;
WordPress/WP Codebox behavior is selected by passing `runtime: wp-codebox`.
Domain ability names and component stacks are caller inputs.

```yaml
jobs:
  run-agent:
    uses: Extra-Chill/homeboy-extensions/.github/workflows/runtime-agent-full-run.yml@v4
    with:
      runtime: wp-codebox
      runtime_ref: main
      profile: example-agent
      runtime_profiles: |
        {
          "example-agent": {
            "id": "example-agent",
            "runtime_task_ability": "example/run-task",
            "ability_requirements": ["example/run-task"]
          }
        }
      runtime_dependencies: '["Example/example-runtime@main"]'
      workload_id: example-agent
      target_repo: Example/example-target
      prompt: ${{ inputs.prompt }}
      runtime_task: '{"ability":"example/run-task","input":{"mode":"review"}}'
      runtime_output_projections: '{"pr_url":"metadata.engine_data.example.pr_url"}'
      callback_data: '{"source":"manual-dispatch"}'
      transcript_artifact_name: example-agent-transcript-${{ github.run_id }}
    secrets: inherit
```

### Migrating Old Wrapper Callers

Removed domain-specific wrappers should migrate to `runtime-agent-full-run.yml`
directly and provide their runtime stack as explicit generic inputs.

Use this mapping when updating old wrapper workflow bodies:

| Old wrapper concept | Generic `runtime-agent-full-run.yml` input |
| --- | --- |
| Runtime selection | `runtime`, `runtime_ref`; `runtime_wordpress_version` only applies to WordPress-capable runtimes. Deprecated compatibility alias: `runtime_provider`. |
| Flow identity | `workload_id`, `workload_label`, `callback_data` |
| Bundle execution | `runtime_execution: {"kind":"bundle","source":"..."}` |
| Direct ability execution | `runtime_task` or `ability_request` / `ability_input` |
| Runtime stack | `runtime_dependencies`, `runtime_components`, `profile`, `runtime_profiles`. Deprecated compatibility alias: `runtime_profile`. |
| Required abilities | `required_abilities` |
| Output projection | `runtime_output_projections`, `evidence_projections` |
| Artifacts | `expected_artifacts`, `artifact_declarations`, `artifact_export_config` |
| WordPress setup | `extra_wp_config_defines`, `runtime_mounts`, `runtime_overlays`, `workload_run_before`, `workload_run_after` |
| Runner workspace | `runner_workspace`, `verification_commands`, `drift_checks`, `writable_paths`, `workspace_contract_checks` |
| GitHub auth | `app_token_repos`, `require_homeboy_app_token`, `allowed_repos` |

The reusable workflow exposes `engine_data_json` as one combined JSON object.
Dynamic per-key `workflow_call` outputs are not possible in GitHub Actions, so
callers should parse this object in their own workflow when they need named
values. It also exposes `auth_mode`, which is `homeboy_app_token` when the
workflow generated and used a Homeboy GitHub App installation token, or
`github_token_fallback` when it used the repository-scoped GitHub Actions token.

Callers that compose workflow inputs before invoking `runtime-agent-full-run.yml`
can use the `homeboy-runtime-agent-ci/runtime-workflow-inputs` package export,
the `homeboy-render-runtime-workflow-inputs` CLI, or
`.github/actions/render-runtime-workflow-inputs`. These surfaces accept
`runtime`, `runtime_profile` as either an id or JSON object,
`runtime_profiles`, `tool_profile`, and runtime mount arrays, then emit
selected-runtime workflow input JSON with a canonical `profile` output without
exposing WP Codebox ability names, CLI paths, or schemas to the downstream
workflow.

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

## Runtime Bundle Example

Workflows that need a WP Codebox-backed bundle can pass the runtime profile,
dependencies, WordPress sandbox configuration, pre-run bootstrap work, and extra
ability assertions through the generic full-run inputs by selecting
`runtime: wp-codebox`.

```yaml
jobs:
  run-example-agent:
    uses: Extra-Chill/homeboy-extensions/.github/workflows/runtime-agent-full-run.yml@v4
    with:
      runtime: wp-codebox
      runtime_ref: main
      profile: example-agent-ci
      runtime_profiles: >-
        {"example-agent-ci":{"id":"example-agent-ci","runtime_task_ability":"example/run-task","runtime_bundle_ability":"example/run-agent-bundle","capabilities":["ability_execution","agent_bundle_execution"],"runtime_execution_contracts":{"bundle":{"ability_field":"runtime_bundle_ability","required_capabilities":["agent_bundle_execution"]}},"ability_requirements":["example/run-agent-bundle"]}}
      runtime_dependencies: '["Example/runtime-plugin@main"]'
      workload_id: example-agent-flow
      workload_label: Run example agent
      target_repo: Example/project
      prompt: ${{ inputs.prompt }}
      runtime_execution: '{"kind":"bundle","source":"bundles/example-agent"}'
      validation_dependencies: Example/project@main
      runtime_wordpress_version: beta
      max_turns: 16
      step_budget: 20
      time_budget_ms: 900000
      extra_wp_config_defines: |
        {
          "EXAMPLE_RUNTIME_MODE": "primary"
        }
      runtime_mounts: |
        [
          "${{ github.workspace }}/.ci/example-runtime-plugin:/wordpress/wp-content/plugins/example-runtime-plugin:readonly"
        ]
      workload_run_before: |
        [
          { "type": "php", "file": "world-creator-bootstrap.php" }
        ]
      runtime_config: '{"daily_memory_enabled":true}'
      required_abilities: |
        ["example/create-artifact", "example/publish-result"]
      success_requires_pr: true
      runtime_output_projections: '{"example_pr_url":"metadata.engine_data.example.pr_url"}'
      transcript_artifact_name: example-agent-transcript-${{ github.run_id }}
    secrets: inherit
```

## Inputs worth calling out

- Agent CI runs through the selected `runtime`. Empty runtime input selects `local-shell`. Deprecated compatibility aliases remain available only for existing callers: `runtime_provider` maps to `runtime`, and the legacy `codebox` runtime id maps to canonical `wp-codebox`. Runtime metadata is discovered from `agent-runtimes/<runtime>/<runtime>.json` or another manifest JSON adjacent to the runtime.
- `profile` is the runtime profile selector. Deprecated compatibility alias: `runtime_profile`.
- `runtime_ref` controls the selected runtime ref.
- `runtime_execution` declares bundle, workflow, or ability execution. When `runtime_task` or `ability_request` is supplied, the workflow builds a direct runtime task instead.
- `runtime_task` forwards a generic `{ "ability", "input" }` object to the runtime task executor.
- `ability_request` and `ability_input` are a shorthand for direct ability execution. `ability_input` is merged into `ability_request.input`.
- `runtime_output_projections` maps named outputs to dotted paths in the provider runtime result.
- Generic `runtime-agent-full-run.yml` callers can use `runtime_execution` for ability, bundle, or workflow descriptors and pass `runtime_output_projections` / `evidence_projections` through to the selected runtime config. Bundle and workflow descriptors derive the provider operation from the selected runtime profile's `runtime_execution_contracts`, so callers do not need to provide `runtime_task.ability` for generic package runs.
- `component_contracts` forwards explicit runtime component/plugin contracts to the selected runtime adapter. WP Codebox maps them through its `wp-codebox/runtime-profile/v1` payload.
- WP Codebox executor paths accept caller-supplied component contracts, runtime overlays, mounts, task payload, provider defaults, tool profiles, and declarative runtime requirements through the generic runtime workflow input renderer. Domain policy belongs in caller inputs and runtime profiles, not in the generic WP Codebox provider manifest.
- `runtime_dependencies` checks out the explicit runtime component stack and forwards those paths to the selected runtime adapter.
- `tool_profile` is the runtime-neutral tool policy input. The selected runtime adapter maps it into runtime-owned workflow fields; for WP Codebox that becomes the sandbox tool policy. Deprecated compatibility alias: `tool_policy`.
- `provider_plugin` is a JSON object with `repo`, `ref`, `path`, `register_function`, and `provider_secret_env` keys. The generic workflow does not choose a provider plugin or secret for callers; provider dependencies and credential mappings are explicit caller inputs, and runtime manifests advertise provider-specific defaults/capabilities. Deprecated compatibility alias: `credentials`; generated config uses `provider_secret_env_mapping`.
- `validation_dependencies` accepts additional `OWNER/REPO@REF` entries and checks each out under `.ci/<repo>`. Entries without `@REF` use the repository default branch.
- Bundle sources in `runtime_execution` are resolved relative to the consumer checkout unless the caller materializes external bundle sources through dependencies or validation checkouts.
- `app_token_repos` scopes the Homeboy GitHub App token and defaults to `target_repo`. Use it when the workflow needs app-token access to more than the target repository.
- `require_homeboy_app_token` fails before agent setup when Homeboy App credentials are missing. Enable it for central, cross-repo, and private-target runs; leave it false for same-repo consumers that intentionally use `github.token` fallback.
- `allowed_repos` is a JSON array of `OWNER/REPO` entries exposed to the injected GitHub profile. It defaults to `[target_repo]`.
- `engine_key` and `tool_results_key` control where built-in GitHub tool captures are written in `metadata.engine_data`.
- `dry_run` is intended for workflow smoke tests only; production consumers should leave it `false`.
- `transcript_artifact_name` controls artifact upload. An empty value skips upload.
- `extra_wp_config_defines` must be a JSON object and is merged into the runner config `wp_config_defines`.
- `runtime_mounts` adds selected-runtime mounts. It must be a JSON array.
- `runtime_overlays` forwards runtime overlay entries to the Codebox runtime profile payload. It must be a JSON array; WP Codebox owns field-level overlay schema validation.
- `workload_run_before`, `workload_run_after`, and `required_abilities` must be JSON arrays.
- `proof_profile` controls controller-loop proof evidence. `artifact_only` is the generic default and does not require preview or PR/publication evidence, `cook_to_pr` requires durable preview plus pull-request evidence, and `none` declares no extra proof requirements. Explicit `controller_loop_proof` / `controller_loop_proof_policy` config still overrides profile fields.
- `workload_run_after` runs post-agent verifier hooks in the same WordPress scenario, so consumers can assert the agent left WordPress in a valid state.
- `ability_tools` adds WordPress ability-backed tools to the agent loop. It must be a JSON array.
- `evidence_projections` maps provider operation results to named runtime outputs or artifact refs. Deprecated compatibility alias: `tool_recorders`, only for existing callers that also need forced parameters.
- `pipeline_step_patches` and `flow_step_patches` modify imported bundle step config before the flow runs. They must be JSON arrays.
- `runner_workspace` provisions a selected-runtime runner workspace before the agent runs. WP Codebox uses its runner workspace API for this path. By default it is agent-visible: the runner prepends the workspace handle and branch to the prompt and forces workspace tools to that handle. Set `expose_to_agent: false` for runner-owned capture mode; the natural prompt is preserved, workspace tools remain scoped when used, and the runner publishes captured workspace changes through the selected runtime after completion.
- `runner_workspace.capture_changes` defaults to `true` only when `expose_to_agent: false`; set it explicitly to disable hidden-mode publication or to enable runner-owned capture while still exposing the workspace handle.
- `verification_commands` and `drift_checks` run through the WP Codebox runner workspace command API, so remote runner workspaces do not require Homeboy to know backend-local paths.
- If runner-owned workspace publication is unavailable or fails, the run fails as `write_without_pr`; Homeboy does not compose alternate PR fallback calls.

## External Bundle And Tool Recording

Consumers can keep an agent bundle in one repository while
running it against another repository. The reusable workflow handles the bundle
checkout and passes tool recorder config to the WordPress runner.

```yaml
jobs:
  run-external-agent:
    uses: Extra-Chill/homeboy-extensions/.github/workflows/runtime-agent-full-run.yml@v4
    with:
      runtime: wp-codebox
      runtime_ref: main
      profile: example-agent-ci
      runtime_profiles: >-
        {"example-agent-ci":{"id":"example-agent-ci","runtime_task_ability":"example/run-task","runtime_bundle_ability":"example/run-agent-bundle","capabilities":["ability_execution","agent_bundle_execution"],"runtime_execution_contracts":{"bundle":{"ability_field":"runtime_bundle_ability","required_capabilities":["agent_bundle_execution"]}},"ability_requirements":["example/run-agent-bundle"]}}
      runtime_dependencies: '["Example/runtime-plugin@main"]'
      runtime_execution: '{"kind":"bundle","source":".ci/example-agent/bundles/example-agent"}'
      workload_id: example-agent-flow
      workload_label: Run example agent runtime bundle
      target_repo: Automattic/agents-api
      validation_dependencies: ExampleOrg/example-agent@main
      app_token_repos: ExampleOrg/target-repo,ExampleOrg/example-agent
      require_homeboy_app_token: true
      allowed_repos: '["ExampleOrg/target-repo", "ExampleOrg/example-agent"]'
      tool_results_key: github_tool_results
      evidence_projections: |
        [
          {
            "operation": "create_or_update_github_file",
            "outputs": {
              "path": "parameters.path",
              "commit_sha": "result.commit.sha"
            }
          }
        ]
      success_requires_pr: false
      transcript_artifact_name: example-agent-transcript-${{ github.run_id }}
    secrets: inherit
```

Use `evidence_projections` when a consumer needs selected provider operation
results copied into stable runner outputs.

## Provider plugin examples

Provider plugins and credentials are explicit. Map each provider option to one
of the generic provider secret env names, then pass that secret in the reusable
workflow call:

```yaml
jobs:
  run-agent:
    uses: Extra-Chill/homeboy-extensions/.github/workflows/runtime-agent-full-run.yml@v4
    with:
      runtime: wp-codebox
      profile: example-agent
      runtime_profiles: >-
        {"example-agent":{"id":"example-agent","runtime_task_ability":"example/run-task","ability_requirements":["example/run-task"]}}
      workload_id: example-flow
      target_repo: Extra-Chill/example
      provider: example-provider
      model: gpt-5.5
      provider_plugin: |
        {
          "repo": "Example/example-ai-provider",
          "ref": "main",
          "path": ".",
          "register_function": "Example\\AiProvider\\register_provider",
          "provider_secret_env": {
            "connectors_ai_example_api_key": "PROVIDER_SECRET_1"
          }
        }
    secrets:
      PROVIDER_SECRET_1: ${{ secrets.EXAMPLE_PROVIDER_API_KEY }}
```
