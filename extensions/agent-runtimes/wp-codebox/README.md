# WP Codebox AI Runtime

This package is the first-class WP Codebox agent-task runtime surface used by
Homeboy. It declares the provider contract, runtime-local CLI, and normalized
AgentTaskOutcome conversion for consumers that need a WordPress-capable AI
runtime through the WP Codebox executable and package contracts.

## Runtime Defaults

### CLI Resolution

CLI resolution uses exact configured pins in this order:
`HOMEBOY_WP_CODEBOX_BIN`, `WP_CODEBOX_BIN`,
`HOMEBOY_SETTINGS_WP_CODEBOX_BIN`, and configured runtime settings. A configured
pin must name a usable executable; a dangling pin fails closed and never falls
through to a managed cache or `PATH`. With no configured pin, a present managed
cache is selected (and fails closed if incomplete), followed by `PATH`.

Managed setup records the source Git revision and SHA-256 of the built CLI in
`.homeboy-runtime-identity.json`. Readiness verifies both values, so rebuilding
or modifying the executable requires setup to record a fresh identity.

Managed cache promotion atomically replaces the stable `source` symlink with a
verified immutable release. Releases are retained during updates because a
reader may have resolved an older target before later promotions. Cleanup is a
deferred operator concern until a reader-safe lease or reclamation design exists.

`wp-codebox.json` is the declarative source for executor defaults that callers may
override through the provider contract or task conversion options:

- `capabilities` advertises supported tools and abilities.
- `workspace_tools` declares the default read-only and read-write workspace tool ids.
- `component_path_defaults` maps runtime component contracts, legacy aliases, and
  discovery hints into generic runtime component path keys.
- `provider_metadata` declares the operator-facing provider ids, executor backend,
  runtime id, model fields, and provider-plugin guidance consumed by the WP
  Codebox runtime contract.

The installed WordPress extension manifest declares
`wordpress.wp-codebox.recipe-run` for Homeboy's generic remote recipe-run command.
It uses the runtime CLI descriptor's canonical `wp-codebox` executable with argv
`recipe-run --recipe {recipe} --artifacts {artifacts} --json`; Homeboy resolves the
executable on the selected runner and owns workspace materialization, run
persistence, and artifact promotion. This provider requires Homeboy support from
[#12131](https://github.com/Extra-Chill/homeboy/issues/12131).

The JavaScript executor consumes these manifest fields as product policy. Callers
can supply generic manifest values through the runtime package contract.

Runtime selection and provider selection are separate concerns:

- `executor.backend` selects the generic executor backend, currently `codebox` for
  this runtime.
- `runtime_id` selects the runtime package, currently `wp-codebox`.
- `executor.config.provider` selects the WordPress AI provider id, such as
  `openai`, `codex`, or `claude-code`.
- `executor.config.model` or `executor.model` carries the provider model name as
  runtime metadata. Homeboy forwards the model name to WP Codebox and provider
  plugins through the provider contract.

## Adapter Contract

Homeboy Extensions is a thin adapter for Codebox-owned runtime primitives. It
forwards `wp-codebox/runtime-profile/v1` dependencies, provider plugins, overlays,
env, and mounts as runtime profile data, then invokes WP Codebox through the
runtime package's executable contract.

Codebox owns these primitives:

- `wp-codebox/runtime-profile/v1` for runtime dependencies and mounts.
- `wp-codebox/run-agent-task/v1` for launching a prepared agent task through a Codebox-owned run contract.
- `wp-codebox/agent-task-run-result/v1` for the stable `agent_task_run_result` output produced by the run contract.
- `wp-codebox/parent-tool-bridge/v1` for exposing parent-owned tools inside the sandbox.
- `wp-codebox/provider-credential-boundary/v1` for the provider credential boundary: Homeboy passes only `secret_env` names, while provider plugins or parent control-plane filters resolve values.
- `wp-codebox/provider-runtime-invocation-contract/v1` for runner workspace, transcript, and artifact handoff operations.
- `wp-codebox/evidence-artifact-envelope/v1` for typed artifacts, evidence refs, and run summaries.

`lib/codebox-run-agent-task-contract.js` is the adapter contract for launching
Codebox agent tasks. It builds the `wp-codebox/run-agent-task/v1` request shape
and invokes the stable `run-agent-task` CLI.

Runtime-package callers should invoke `wp-codebox/run-runtime-package`.

GitHub Actions callers should invoke `.github/workflows/runtime-agent-full-run.yml`
directly with `runtime: wp-codebox` and canonical selected-runtime inputs such as
`runtime_mounts`, `runtime_overlays`, `runtime_profiles`, and
`artifact_declarations`. Product-specific composition belongs in caller-owned
workflow setup or runtime input rendering helpers, not a second reusable workflow
surface.

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

The runtime manifest currently advertises agent-task execution. The current
executable path targets the existing task-input runner contract; generic
delegated command execution will use a future manifest capability when WP Codebox
accepts the neutral request shape and returns the neutral result shape directly.
