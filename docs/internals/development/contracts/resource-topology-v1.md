# Resource Topology v1

`homeboy-resource-topology-contract` defines one read-only snapshot schema:
`homeboy/resource-topology-snapshot/v1`.

The snapshot contains only stable `kind` and `id` resource references, declared
directed edges, caller-selected roots, and typed unresolved-reference evidence.
The initial resource kinds are `component`, `project`, `server`, `fleet`, and
`runner`. Its initial edge kinds are `fleet_contains_project`,
`project_targets_server`, `project_uses_component`, and `runner_uses_server`.

The contract does not own or reproduce persisted `Component`, `Project`,
`Server`, `Fleet`, or `Runner` configuration. It also excludes effective
component resolution, readiness, health, drift, deployment routing, runner
admission, lifecycle, actions, retries, and execution. A resolver may use the
canonical loaders owned by those subsystems to produce a snapshot while retaining
missing references as `unresolved_reference` diagnostics.

This is independent of Extension API v1: topology names Homeboy resources and
their declared relationships; Extension API describes extension capability
discovery and invocation. It is likewise separate from Runner API, control-plane
run records, and daemon execution envelopes, which carry their own execution
and lifecycle contracts.

Within v1, optional fields may be added. Changing a resource or edge identity,
the meaning of an existing edge, or a required field requires a new major schema.

## Resolution and inspection

`homeboy-core` resolves caller-selected roots with the canonical component,
project, server, and fleet loaders. It follows only declared edges and returns a
partial snapshot when a referenced resource is absent. The absent reference is
reported as `unresolved_reference`; it is not represented as a configured
resource or silently discarded.

`Runner` remains owned by `homeboy-lab-runner`. Its `RunnerSpec` conversion is
still the configuration seam; that subsystem supplies runner/server associations
to the core resolver rather than moving runner configuration into this contract.

Inspect a root through the CLI with `homeboy topology <kind> <id>`, or through
the local HTTP API with `GET /topology/:kind/:id`. Both surfaces are additive and
return the same versioned snapshot without changing existing component, fleet,
or runner output schemas. The HTTP endpoint includes server-backed runners;
the CLI additionally includes the runner registry's local runner identities.
