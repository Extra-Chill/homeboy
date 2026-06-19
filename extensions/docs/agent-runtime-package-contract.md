# Agent Runtime Package Contract

Agent runtime packages live under `agent-runtimes/<runtime-id>/`. They expose
provider commands that Homeboy can invoke without encoding backend-specific
knowledge in core or in domain extensions.

This contract is intentionally backend-neutral. A browser sandbox runtime, a
local shell fixture, or a CLI agent runtime can all satisfy the same request and
outcome schemas.

## Package Layout

Each runtime package should include:

- `<runtime-id>.json`: package manifest.
- `README.md`: runtime-specific operator notes and provider setup.
- Runnable provider command files referenced by the manifest.
- Tests or fixtures proving the manifest and provider command contract.

The package may include implementation libraries, but Homeboy selects runtimes
through the manifest and provider command contract, not by importing private
runtime modules.

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
- `backend`: backend selector value used by requests, for example `fake-runtime` or `codebox`.
- `command`: shell command Homeboy runs after interpolation.
- `request_schema`: accepted request schema, currently `homeboy/agent-task-request/v1`.
- `outcome_schema`: emitted outcome schema, currently `homeboy/agent-task-outcome/v1`.
- `request_required_fields`: request paths the provider requires before execution.
- `outcome_statuses`: statuses the provider may emit.
- `failure_classifications`: normalized failure classes used for diagnostics.
- `redacted_metadata_keys`: metadata keys that must never expose secret values.
- `capabilities`: explicit capabilities orchestration may rely on.
- `workspace_materialization`: workspace shape required by the provider.
- `secret_requirements`: environment variable names or groups the provider needs.
- `diagnostics`: diagnostic artifacts and metadata the provider writes or returns.
- `status`: lifecycle state, usually `active`, `experimental`, or `deprecated`.
- `integration_contract`: higher-level contract name when the provider serves a domain extension.

Runtime-specific fields are allowed, but they should be additive. A runtime
should not require Homeboy core to understand backend-private settings just to
invoke the provider.

Generic runner specs must declare `executor.backend` explicitly. Runtime-specific
planners may provide their own defaults, but the shared contract does not assume
any particular backend.

## `runtime_path` Interpolation

Provider commands should reference runtime-local files with `{{runtime_path}}`:

```json
{
  "command": "node {{runtime_path}}/scripts/agent/example-agent-task-executor.cjs"
}
```

Homeboy owns interpolation and replaces `{{runtime_path}}` with the installed
runtime package directory before execution. Runtime packages should keep command
paths relative to that directory so linked installs, extracted installs, and
remote runners resolve the same files.

Provider commands may use normal shell syntax, but portable single-executable
commands are preferred. Avoid commands that depend on the monorepo checkout
layout unless that layout is part of the package contract.

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

## Homeboy Contract Adapter

Extension runtime packages should consume generic Homeboy contract constants
through `runtime-agent-ci/lib/agent-task-provider-contract.js`. That adapter is the
local compatibility seam while the matching Homeboy core contracts are released.
It owns schema identifiers, the default provider fields, secret-env requirement
selectors, redacted metadata keys, and artifact/evidence reference projection
helpers.

Reusable CI callers that need to describe a runner without embedding workflow
glue should consume `runtime-agent-ci/lib/agent-task-runner-contract.js`. That
adapter owns `homeboy/agent-task-runner-spec/v1` validation and projection into
the generic request fields consumed by executor providers. Extension-specific
exports should re-export that adapter instead of copying schema and lifecycle
validation logic.

Runtime packages may add backend-specific capabilities, secret names, role
aliases, and metadata keys, but should extend the adapter output instead of
copying schema strings or selector paths into each backend. Domain policy, such
as WordPress or project-specific defaults, belongs in the caller/runtime package and
not in the generic adapter.

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

## Capability Declarations

Capabilities are selection and orchestration promises. Declare a capability only
when the provider can satisfy it for every request accepted by that provider.

Use stable, backend-neutral names where possible, such as:

- `workspace_materialization`
- `structured_outcome`
- `diagnostic_artifacts`
- `patch_artifacts`
- `verification_artifacts`
- `browser_runtime`

Backend-specific capabilities are acceptable when they are intentionally part of
selection, but they should not replace the generic capability when a generic one
applies.

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

## Fake Runtime Fixture

`agent-runtimes/fake-runtime` is the smallest reference fixture for this
contract. It accepts `homeboy/agent-task-request/v1`, requires an explicit
`executor.backend`, writes an outcome JSON file plus a transcript log artifact,
and emits `homeboy/agent-task-outcome/v1` without any secret inputs. It exists to
prove future runtimes can satisfy Homeboy's generic runtime package shape without
copying another runtime package structure or domain behavior.

## Local Shell Runtime

`agent-runtimes/local-shell` is the smallest generic non-WordPress runtime. It
runs an explicit local command from the `AgentTaskRequest` and emits a
normalized `AgentTaskOutcome`. It is intended for deterministic loops where the
caller owns domain policy and command safety.
