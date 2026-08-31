# Extension API v1

The Extension API is the transport-neutral contract through which Homeboy
describes installed extensions and negotiates compatibility. Its serialized v1
types live in `homeboy_extension_contract::api::v1`; catalog projection and
negotiation behavior live in `homeboy_core::extension::catalog`.

This boundary is independent of any extension family. A language toolchain, a
deployment provider, and an agent runtime are all catalog entries described by
the same identity, capability, readiness, and execution-requirement vocabulary.

## Versioning

API versions are numeric majors. Homeboy currently advertises major `1`.
Clients send every major they understand in an
`ExtensionApiHandshakeRequest`; Homeboy deterministically selects the highest
shared major or returns a typed compatibility failure.

The wire schemas are:

- `homeboy/extension-api-descriptor/v1`
- `homeboy/extension-api-handshake-request/v1`
- `homeboy/extension-api-handshake-response/v1`
- `homeboy/extension-api-catalog-request/v1`
- `homeboy/extension-api-catalog-response/v1`
- `homeboy/extension-api-resolve-request/v1`
- `homeboy/extension-api-resolve-response/v1`
- `homeboy/extension-api-readiness-request/v1`
- `homeboy/extension-api-readiness-response/v1`
- `homeboy/extension-api-invoke-request/v1`
- `homeboy/extension-api-invoke-response/v1`

Additive optional fields may be added within v1. Changes to identity,
capability meaning, compatibility decisions, or required fields require a new
major version. Numeric versions remain parseable by older clients so an unknown
future major produces `no_shared_api_version` instead of a deserialization
failure.

The existing `HOMEBOY_EXEC_CONTEXT_VERSION=2` is a separate child-process
environment protocol. It is not the Extension API version.

## Descriptor

`ExtensionApiDescriptor` projects an installed manifest into:

- extension identity and source revision
- open capability IDs with independent contract versions and schema references
- runtime and toolchain readiness declarations
- runner-neutral runtime requirements
- the declared Homeboy version constraint

Manifest command strings and local extension paths are not part of this public
descriptor. They remain implementation details behind catalog and invocation
services.

## Handshake

`extension::catalog::negotiate_api` evaluates two independent compatibility
dimensions:

1. The client and Homeboy must share an Extension API major.
2. The installed extension's declared Homeboy version constraint must accept the
   running controller.

Failures are typed as `invalid_handshake_schema`, `no_shared_api_version`,
`invalid_homeboy_version_constraint`, or `homeboy_version_incompatible`.
Responses always advertise Homeboy's supported versions. A descriptor is
returned only when an API major was selected.

## Catalog And Resolve

`extension::catalog::list_api` lists every installed extension in ascending ID
order. Valid entries carry their v1 descriptor and compatibility result.
Malformed manifests and broken installations remain in the catalog as `invalid`
entries with safe typed diagnostics; catalog projection never silently drops
them.

`extension::catalog::resolve_api` resolves one explicit extension ID and open
capability ID. It returns the same descriptor and compatibility values exposed
by catalog, plus the selected capability descriptor. Failure codes distinguish
invalid request schemas, unsupported API majors, missing or invalid extensions,
incompatible extensions, and capabilities the selected extension does not
provide.

Component-aware owner election remains application policy over this primitive.
The v1 wire contract does not serialize Homeboy's internal `Component` or
`Project` models.

Core component resolution reads capability candidates and invalid-installation
diagnostics from one v1 catalog snapshot. Explicit ownership,
`composition.includes` primacy, and genuine ambiguity remain application policy
over those stable descriptors; execution-context assembly loads the selected
manifest only for internal script paths and settings.

File-type providers use open capability IDs: `fingerprint.<extension>`,
`format.<extension>`, and `refactor.<extension>`. Resolution matches those IDs
in the same catalog and returns only the selected extension ID. Manifest paths
and script declarations remain private execution details.

## Readiness

`extension::catalog::readiness_api_batch` returns readiness evidence for explicit
installed extension IDs from one discovery pass. Callers choose `cached` to read
matching evidence without running extension code or `probe` to execute declared
runtime probes within Homeboy's existing timeout and recursion guards.

The response distinguishes `ready`, `not_ready`, `unknown`, and `timed_out` and
preserves cache age, probe duration, timeout, diagnostic, and follow-up command
evidence. Missing and invalid installations use the same typed operation
failures as catalog and resolve.

CLI extension inventory and startup command-health discovery consume v1 catalog
and readiness responses. Their legacy presentation fields remain CLI adapters;
core no longer maintains a parallel `ExtensionSummary` projection.

## Read-Only Invocation

`extension::invoke::invoke_api` synchronously executes one explicitly selected
non-mutating capability after resolving it through v1. Requests carry the
extension ID, capability ID, JSON input, and an explicit working directory.
Core keeps script paths private, bounds captured output, and accepts only JSON
stdout. Resolution, process, and output failures use typed operation failure
codes. Process failures optionally include `process` evidence with the exit
code, bounded stdout and stderr, and parsed stdout when it was valid JSON. The
optional field is omitted for existing successful invocation responses.

The `compiler-warnings`, `compiler-warning-fixes`, and
`refactor.<file-extension>` capabilities are adopters. Their descriptors
reference versioned input and output schemas; audit and refactor consume
invocation responses rather than loading manifests or running scripts directly.
Refactor commands remain extension-owned JSON payloads under the shared
`homeboy/refactor-analysis-input/v1` and `homeboy/refactor-analysis-output/v1`
schema references. Component-linked providers take precedence when they offer
the requested capability, with installed providers as the deterministic
fallback.

This synchronous operation is intentionally limited to analysis. It does not
perform durable mutation and therefore has no idempotency, cancellation,
reconciliation, activity, or terminal-result lifecycle.

## Contract Classification

`homeboy-extension-contract` predates the stable API and contains several kinds
of portable data. Only `api` is a stable Extension API module in this slice.
The rest is classified explicitly so package membership is not mistaken for API
stability.

| Classification | Modules | Direction |
| --- | --- | --- |
| Stable Extension API | `api` | Versioned public descriptor, handshake, discovery, readiness, and read-only invocation envelopes. |
| Stable API candidates | `capability`, `core_compat`, `exec_context`, `runtime_helper`, `sidecar_config` | Reuse or reference from future v1 operations after their wire semantics are reviewed. |
| Extension-owned domain contracts | `action_types`, `agent_task_executor_declaration`, `autofix_config`, `bench_artifact`, `bench_diagnostics`, `bench_distribution`, `bench_gate`, `bench_metric_preset`, `bench_responsiveness`, `bench_result`, `bench_results`, `bench_stage`, `ci_config`, `ci_context`, `external_check_detail_resolver`, `external_storage_retention`, `fuzz_config`, `lint_result`, `lint_results`, `notification_transport_config`, `source_metadata_repair`, `test_analysis`, `test_drift`, `test_duration`, `test_inventory_config`, `test_parsing`, `test_result`, `test_results`, `test_workflow`, `trace_config`, `trace_parsing`, `trace_preview`, `trace_results`, `trace_spec`, `update_output`, `worktree_retention` | Remain portable domain schemas; the Extension API references their schema IDs rather than absorbing their fields. |
| Manifest and implementation detail | `extension_contract_producer`, `hook_event`, `manifest`, `manifest_action_config`, `manifest_artifact_cleanup`, `manifest_capabilities`, `manifest_capability_config`, `manifest_deploy_config`, `manifest_test_config`, `manifest_toolchain_config`, `runner_contract`, `version` | Inputs and helpers used to build or execute descriptors. They are not a stable service API. |

## Next Operations

The read-only invocation operation deliberately does not define a durable
invocation lifecycle. Subsequent v1 slices will add idempotent mutation, cancel,
reconcile, activity, and terminal-result contracts anchored to canonical
control-plane references from issue #13697.
