# Agent Runtime Package Contract

Agent runtime packages live under `agent-runtimes/<runtime-id>/`. They expose
provider commands that Homeboy can invoke without encoding backend-specific
knowledge in core or in domain extensions.

This contract is intentionally backend-neutral. A browser sandbox runtime or a
CLI agent runtime can satisfy the same request and outcome schemas.

## Package Layout

Each runtime package should include:

- `<runtime-id>.json`: package manifest.
- `README.md`: runtime-specific operator notes and provider setup.
- Runnable provider command files referenced by the manifest.
- Tests or fixtures proving the manifest and provider command contract.

The package may include implementation libraries. Homeboy selects runtimes
through the manifest and provider command contract exported by the runtime
package.

## Manifest Fields

The manifest root must declare:

- `schema`: runtime manifest schema, currently `homeboy/agent-runtime-manifest/v1`.
- `id`: stable runtime package id. For standalone installs, Homeboy treats the
  containing directory name as authoritative and may override this value.
- `name`: human-readable runtime name.
- `version`: runtime package contract version.
- `description`: one-sentence runtime summary.
- `agent_task_executors`: non-empty list of provider declarations.

Each `agent_task_executors[]` entry must declare:

- `schema`: provider declaration schema, currently `homeboy/agent-task-executor-provider/v1`.
- `id`: stable provider id. Use a namespaced id such as `runtime-id.provider-id`.
- `label`: human-readable provider label.
- `backend`: backend selector value used by requests, for example `codebox` or `opencode`.
- `invocation.argv`: argv-form command Homeboy runs after interpolation.
- `request_schema`: accepted request schema, currently `homeboy/agent-task-request/v1`.
- `outcome_schema`: emitted outcome schema, currently `homeboy/agent-task-outcome/v1`.
- `request_required_fields`: request paths the provider requires before execution.
- `outcome_statuses`: statuses the provider may emit.
- `failure_classifications`: normalized failure classes used for diagnostics.
- `redacted_metadata_keys`: metadata keys that must never expose secret values.
- `capabilities`: explicit capabilities orchestration may rely on.
- `runner_readiness`: executable, path, or environment checks operators can inspect before dispatch.
- `role_aliases`: mappings from provider-native artifact/output names to generic Homeboy roles.
- `workspace_materialization`: workspace shape required by the provider.
- `secret_requirements` and `secret_env_requirements`: secret names or groups the provider may need. Core lists and resolves explicitly requested env names; runtime packages own provider-specific default selection.
- `provider_defaults`: provider-specific defaults such as model names, secret env names, and source hints.
- `diagnostics`: diagnostic artifacts and metadata the provider writes or returns.
- `status`: lifecycle state, usually `active`, `experimental`, or `deprecated`.
- `integration_contract`: higher-level contract name when the provider serves a domain extension.

Runtime-specific fields are allowed when they are additive. The public manifest
fields give Homeboy core the information it needs to invoke the provider.

Generic runner specs must declare `executor.backend` explicitly. Runtime-specific
planners may provide their own defaults, but the shared contract does not assume
any particular backend.

Runtime ids are canonical package ids. Compatibility aliases may resolve for
existing callers, but resolvers should return explicit deprecation metadata with a
replacement id and quarantine name. New callers should select canonical runtime
ids directly, for example `wp-codebox` instead of the legacy `codebox` alias.
Compatibility aliases are not extension points; new runtime packages and docs
should publish canonical ids only.

## `runtime_path` Interpolation

Provider invocations should reference runtime-local files with `{{runtime_path}}`:

```json
{
  "invocation": {
    "schema": "homeboy/command-invocation/v1",
    "argv": [
      "node",
      "{{runtime_path}}/scripts/agent/example-agent-task-executor.cjs"
    ]
  }
}
```

Homeboy owns interpolation and replaces `{{runtime_path}}` with the installed
runtime package directory before execution. Runtime packages should keep command
paths relative to that directory so linked installs, extracted installs, and
remote runners resolve the same files.

`invocation.argv` passes arguments directly without shell interpretation. Use a
shell executable explicitly in `argv` only when a runtime contract requires
shell semantics. Avoid commands that depend on the monorepo checkout layout
unless that layout is part of the package contract.

## Provider Command Contract

A provider command must:

- Read one JSON request from stdin.
- Validate `schema`, `task_id`, `executor.backend`, and the fields declared in `request_required_fields`.
- Execute using only the request, environment variables, and files under the materialized workspace/runtime paths.
- Write one JSON outcome to stdout.
- Write human diagnostics to stderr only when useful; stderr must not contain secrets.
- Exit `0` after emitting a well-formed terminal outcome, including normalized failure outcomes.
- Exit non-zero only for command-level failures where no valid outcome can be produced.

The emitted outcome must use `outcome_schema` and one of `outcome_statuses`.
Failure outcomes should include a normalized classification from
`failure_classifications` plus enough diagnostic context for Homeboy to route the
result without parsing backend-native logs.

## Runtime Command Provider Boundary

TODO: Add a generic runtime-command provider only after the selected runtime owns
a stable command substrate that accepts and returns backend-neutral shapes. The
provider manifest should advertise a separate capability such as
`runtime_command_execution`; agent-task providers currently advertise agent-task
execution through the request and outcome schemas above.

The generic request shape needs these caller-owned inputs:

- `runtime_package`: runtime package id/ref and optional version or checkout ref.
- `recipe`: recipe pack/name/path/ref inputs to materialize the runtime.
- `command_id`: stable command id selected by the caller.
- `args`: JSON object or array passed to the command without shell expansion.
- `env_names`: declared environment variable names the runtime may read; values
  are resolved by Homeboy and never embedded in manifests or diagnostics.
- `workspace`, `limits`, and `artifacts`: materialization, timeout, and expected
  output declarations.

The provider command should emit `homeboy/agent-task-outcome/v1` with normalized
`status`, `failure_classification`, `summary`, `diagnostics`, `artifacts`,
`evidence_refs`, `outputs`, and redacted `metadata`. Backend-native logs and
recipe artifacts may be attached as artifacts, but Homeboy should not parse them
to determine the terminal outcome.

Runtime packages forward package-owned command, args, and env-name declarations
through their executable contract and return stable result envelopes. Product
semantics belong to callers or runtime packages.

## Homeboy Contract Adapter

Extension runtime packages and reusable CI callers should consume generic
Homeboy contract constants through `agent-task-contracts`. That shared package
owns schema identifiers, the default provider fields, secret-env requirement
selectors, redacted metadata keys, artifact/evidence reference projection
helpers, and `homeboy/agent-task-runner-spec/v1` validation/projection into the
generic request fields consumed by executor providers. Extension-specific
exports should re-export the shared package instead of copying schema and
lifecycle validation logic; legacy runtime-CI, agent-runtimes, and
extension-specific paths remain compatibility shims only.

Runtime packages may add backend-specific capabilities, secret names, role
aliases, and metadata keys, but should extend the adapter output instead of
copying schema strings or selector paths into each backend. Domain policy, such
as project-specific defaults, belongs in the caller/runtime package and not in
the generic adapter.

`agent-task-contracts/agent-task-provider-contract.generated.json` is generated
from the pinned core fixture and carries both its own export schema and the
source core-contract schema. Run
`npm --prefix agent-task-contracts run generate:check` to verify it. The core
contract drift test validates both generated artifacts against
`homeboy agent-task contract --format json` when Homeboy is available.

## Host Orchestration Contract

The published Homeboy core fixture is
`agent-runtimes/fixtures/homeboy-agent-task-core-contract.json`. It is the
cross-repo handoff contract for durable host orchestration and runtime package
execution. Homeboy core owns durable queueing, fanout, progress, retries,
artifacts, and promotion. Runtime packages own the executor command and backend
translation behind the provider manifest.

This fixture is a generated artifact, not a hand-maintained mirror. Its core
region is consumed directly from Homeboy core's published contract via
`homeboy agent-task contract --format json` (core's `agent_task_core_contract()`),
and the extensions-owned fanout/reconcile + orchestration overlay below is
derived from the runtime's own JS constant modules. Regenerate it with
`node agent-runtimes/fixtures/generate-homeboy-agent-task-core-contract.cjs`
(or `npm run fixture:agent-task-core-contract` from `wordpress/`). The
`agent-task-core-contract-drift` test asserts byte-for-byte equality between the
committed fixture and the generator output whenever the homeboy binary is
available, so the fixture can never silently drift from core.

Generic fanout/reconcile schemas are part of that fixture:

- `homeboy/generic-fanout-reconcile-config/v1`
- `homeboy/fanout-reconcile-plan/v1`
- `homeboy/fanout-reconcile-run/v1`
- `homeboy/generic-fanout-reconcile-result/v1`
- `homeboy/generic-fanout-reconcile-reconciliation/v1`
- `homeboy/generic-finding-packet-fanout-config/v1`

When a host fanout lane executes agent-task providers, record status is the host
orchestration status and outcome status remains the provider terminal status.
The canonical bridge is:

| Agent-task outcome status | Fanout record status |
| --- | --- |
| `succeeded` | `completed` |
| `no_op` | `completed` |
| `unable_to_remediate` | `failed` |
| `provider_error` | `failed` |
| `timeout` | `failed` |
| `failed` | `failed` |
| `follow_up_issue` | `failed` |
| `cancelled` | `failed` |

Fanout runners may also emit `missing_record` for a planned task with no returned
execution record. The run status vocabulary is `incomplete`, `completed`, and
`failed`. Backend-native statuses should be preserved inside provider outcome
metadata or diagnostics, not promoted into host orchestration status fields.

## Secret Requirements

Runtime manifests should declare secret inputs by name, never by value:

```json
{
  "secret_requirements": [
    {
      "name": "EXAMPLE_RUNTIME_TOKEN",
      "required": false,
      "purpose": "Authenticates optional provider calls."
    }
  ]
}
```

Provider commands receive secret values through environment variables or the
request's secret-name declarations. They must redact secret-like metadata in
outcomes and diagnostics. If a runtime adds a secret-bearing metadata key, it
must add that key to `redacted_metadata_keys`, preferably with
`extendRedactedMetadataKeys()` from the adapter.

### Persistent Runtime Environment

An extension setup adapter can expose a persistent, non-secret runner resource
to job-private execution by writing `HOMEBOY_RUNTIME_ENV_<NAME>=<value>` to
`GITHUB_ENV`. The full-run configuration projects `<NAME>` into `runtime_env`.
This is for stable paths and similar runner mechanics, not credentials; names
ending in `TOKEN`, `SECRET`, `PASSWORD`, `CREDENTIAL`, or `KEY`, and values
containing line breaks, are rejected. Setup-projected values override matching
workflow input values because they describe the resource materialized for that
specific runner job.

## Capability Declarations

### Run-scoped scratch

Runtimes that advertise the `run_scoped_scratch` capability consume the controller-provided
attempt scratch root through `executor.config.runtime_env.TMPDIR`. The runtime passes that value
to its provider process without replacing it with an ambient shared temporary directory.

Homeboy owns scratch allocation, run and attempt ownership metadata, active leases, evidence
promotion ordering, retention, and cleanup preview/apply reporting. A runtime does not create,
delete, lease, or retain the root: that would duplicate controller lifecycle state and could
remove an active attempt's scratch.

### Runtime tool attachment

Providers that advertise `runtime_tool_attachment` accept Homeboy-resolved runtime tools and
project every validated tool into the backend's native tool configuration. Homeboy owns tool
declaration resolution and readiness; providers reject unresolved declarations and preserve the
resolved command, environment, secret-name, capability-evidence, and runtime-owned lifecycle
boundary during attachment.

Capabilities are selection and orchestration promises. Declare a capability only
when the provider can satisfy it for every request accepted by that provider.

Use stable, backend-neutral names where possible, such as:

- `workspace_materialization`
- `structured_outcome`
- `diagnostic_artifacts`
- `patch_artifacts`
- `verification_artifacts`
- `browser_runtime`
- `ability_execution`
- `agent_bundle_execution`
- `workflow_execution`

Backend-specific capabilities are acceptable when they are intentionally part of
selection, but they should not replace the generic capability when a generic one
applies.

## Runtime Execution Contracts

Providers that can execute generic runtime descriptors should declare
`runtime_execution_contracts` on the executor provider. Each key is a generic
`runtime_execution.kind`; each value maps that kind to a provider-specific
runtime-profile field and the capabilities required to use it:

```json
{
  "runtime_execution_contracts": {
    "bundle": {
      "ability_field": "runtime_bundle_ability",
      "required_capabilities": ["agent_bundle_execution"]
    },
    "workflow": {
      "ability_field": "runtime_workflow_ability",
      "required_capabilities": ["workflow_execution"]
    }
  }
}
```

Caller configs can then use `runtime_execution: { "kind": "bundle", ... }`
without embedding a provider-specific ability string. The selected adapter maps
the generic kind to its own runtime-profile ability field and fails closed when
the provider does not advertise the required capability. Direct `ability`
execution remains explicit: callers supply the ability they intend to invoke.

## Workspace Materialization

`workspace_materialization` declares what the provider expects before it starts.
Common fields:

- `cwd`: working-directory mode, such as `git_checkout`, `runtime_package`, or `request_workspace`.
- `requires_git`: whether the workspace must be a git checkout.
- `write_scope`: where the provider may write, such as `workspace`, `artifacts`, or `none`.
- `artifact_paths`: relative paths the provider may create or update.

The provider must not infer workspace shape from any current runtime unless that
shape is declared here.

Caller-owned wrappers should pass domain-specific runtime requirements explicitly.
For example, a caller can supply its ability provider, runtime components,
workspace-tool, and ability-policy defaults before invoking the generic WP
Codebox provider.

## Outcome And Diagnostic Contracts

Provider outcomes should include:

- `schema`: the outcome schema.
- `task_id`: copied from the request.
- `status`: terminal status from `outcome_statuses`.
- `summary`: concise human-readable result.
- `diagnostics`: structured diagnostics suitable for logs, PR comments, and issue routing.
- `artifacts`: typed artifact references when files are produced.
- `metadata`: redacted provider metadata.

Diagnostics should distinguish provider setup failures, request validation
failures, execution failures, timeouts, and successful no-op outcomes. Backend
native logs can be attached as artifacts, but the normalized outcome is the
contract Homeboy consumes.

## Default-Backend Policy Ownership

Runtime packages declare what they can do. They do not decide which backend is
the default for a domain workflow.

Default-backend policy belongs to the caller that understands the domain and
deployment context, for example a Homeboy extension, workflow, component config,
or operator-supplied setting. One workflow may choose a browser sandbox by
default; a different workflow may choose another runtime with the same generic
capabilities. Homeboy core should route explicit requests and evaluate declared
capabilities, not hard-code a global default backend.

## Static Contract Fixture

`tests/fixtures/agent-runtime-manifest.json` is the smallest reference fixture
for this contract. It is intentionally outside `agent-runtimes/` so it validates
the generic manifest shape without advertising a smoke-test runtime as an
installable adapter.
