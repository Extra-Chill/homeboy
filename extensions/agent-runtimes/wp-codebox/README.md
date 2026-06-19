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

The JavaScript executor consumes these manifest fields instead of owning product
policy in code. Callers can supply generic manifest values without adding new
runtime-specific branches.

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
