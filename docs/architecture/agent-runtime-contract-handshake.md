# Agent Runtime Contract Handshake

Homeboy owns the generic protocol for agent runtime handoff. Extensions own the
runtime-specific facts that populate that protocol. Core records schema names,
required fields, redaction boundaries, and phase order; it does not know how to
run Rust, Node.js, WordPress, or any other runtime.

The CLI-visible registry is exported by `homeboy agent-task contract` under
`agent_runtime_handshake` with schema
`homeboy/agent-runtime-contract-handshake/v1`.

## Phases

1. `runtime_capability_manifest` is extension-provided. It declares runtime
   identity, executor providers, capabilities, and runtime-owned declarations via
   `homeboy/agent-runtime-manifest/v1` and
   `homeboy/agent-task-executor-provider/v1`.
2. `readiness_checks` is extension-provided. It declares generic readiness probes
   such as secret env names, env-path checks, executable candidates, and
   remediation text. Homeboy evaluates the generic shape without branching on a
   runtime implementation.
3. `materialization_plan` is Homeboy-resolved. It turns extension declarations
   into `homeboy/agent-runtime-materialization-plan/v1` so runners can route,
   copy, mount, or diagnose runtime inputs.
4. `secret_env_plan` is Homeboy-resolved. Extensions name required env vars;
   Homeboy resolves redacted status and handoff with `homeboy/secret-env-plan/v1`.
5. `resolved_execution_contract` is Homeboy-resolved. It binds the selected
   provider, runtime id/path, readiness checks, workspace summary, capabilities,
   and secret env plan reference/object in
   `homeboy/resolved-agent-runtime-execution-contract/v1`.
6. `result_artifact_declarations` is provider-result-provided. Runtime providers
   report outcome status and artifacts/evidence using Homeboy's generic outcome
   and artifact schemas.

## Boundary

Extensions provide declarations and results. Homeboy provides the contract
vocabulary, validates required wire fields through serde-backed structs, resolves
generic plans, and keeps secret values out of extension-visible contract exports.

Runtime implementation belongs in extensions. Adding a new runtime should add or
update extension manifests/providers, not Homeboy core branches for runtime
languages, package managers, CMSs, or framework-specific behavior.
