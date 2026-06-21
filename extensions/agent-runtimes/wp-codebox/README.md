# WP Codebox AI Runtime

This package is the first-class WP Codebox agent-task runtime surface used by
Homeboy. It declares the provider contract, runtime-local CLI, and normalized
AgentTaskOutcome conversion for consumers that need a WordPress-capable AI
runtime without embedding WP Codebox details in their own extension manifests.

## Runtime Defaults

`wp-codebox.json` is the declarative source for executor defaults that callers may
override through the provider contract or task conversion options:

- `capabilities` advertises supported tools and abilities.
- `workspace_tools` declares the default read-only and read-write workspace tool ids.
- `component_path_defaults` maps runtime component contracts, legacy aliases, and
  discovery hints into generic runtime component path keys.
- `provider_metadata` declares the operator-facing provider ids, executor backend,
  runtime id, model fields, and provider-plugin guidance without requiring Homeboy
  core to know WordPress or Codebox provider details.

The JavaScript executor consumes these manifest fields instead of owning product
policy in code. Callers can supply generic manifest values without adding new
runtime-specific branches.

Runtime selection and provider selection are separate concerns:

- `executor.backend` selects the generic executor backend, currently `codebox` for
  this runtime.
- `runtime_id` selects the runtime package, currently `wp-codebox`.
- `executor.config.provider` selects the WordPress AI provider id, such as
  `openai`, `codex`, or `claude-code`.
- `executor.config.model` or `executor.model` carries the provider model name as
  opaque runtime metadata. The runtime forwards it to WP Codebox and provider
  plugins; Homeboy core does not interpret model names.

## Adapter Boundary

Homeboy Extensions is a thin adapter for Codebox-owned runtime primitives. It
forwards `wp-codebox/runtime-profile/v1` dependencies, provider plugins, overlays,
env, and mounts as runtime profile data. It no longer expands profile
dependencies into `component_contracts` / `extra_plugins` or injects parent tool
bridge environment variables.

Codebox owns these primitives:

- `wp-codebox/runtime-profile/v1` for runtime dependencies and mounts.
- `wp-codebox/run-agent-task/v1` for launching a prepared agent task through a Codebox-owned run contract.
- `wp-codebox/agent-task-run-result/v1` for the stable `agent_task_run_result` output produced by the run contract.
- `wp-codebox/parent-tool-bridge/v1` for exposing parent-owned tools inside the sandbox.
- `wp-codebox/provider-credential-boundary/v1` for the provider credential boundary: Homeboy passes only `secret_env` names, while provider plugins or parent control-plane filters resolve values.
- `wp-codebox/provider-runtime-invocation-contract/v1` for runner workspace, transcript, and artifact handoff operations.
- `wp-codebox/evidence-artifact-envelope/v1` for typed artifacts, evidence refs, and run summaries.

`lib/codebox-run-agent-task-contract.js` is the adapter boundary for launching
Codebox agent tasks. It builds the preferred `wp-codebox/run-agent-task/v1`
request shape and selects the stable `run-agent-task` CLI when the installed
Codebox package advertises it. Older packages continue through the legacy
`agent-task-run` / `wp-codebox/task-input/v1` path, but that compatibility is
kept behind this adapter so the follow-up swap can remove the fallback without
changing Homeboy callers.

Provider credentials stay outside adapter payloads. The adapter forwards
`secret_env` names and a `provider_credential_boundary` descriptor, and rejects
raw credential fields such as `secret_env_values` or `credentials` before a task
request reaches WP Codebox.

## Delegated Run Preparation

`lib/delegated-run-contract.js` defines neutral `homeboy/delegated-run-request/v1`
and `homeboy/delegated-run-result/v1` normalization helpers for a future generic
delegated command or agent-run capability. The helpers intentionally use only
backend-neutral fields such as `execution.type`, `execution.argv`,
`execution.agent`, `input`, `workspace`, `limits`, `artifacts`, `diagnostics`, and
`metadata`.

The runtime manifest does not advertise a delegated-run capability yet. The
current executable path still targets the existing task-input runner contract, so
Homeboy should not select this runtime for generic delegated command execution
until the substrate accepts the neutral request shape and returns the neutral
result shape directly.
