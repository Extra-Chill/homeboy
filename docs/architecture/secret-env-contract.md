# Secret Env Contract

Homeboy core owns the generic contract for passing required secret environment variables across runners, agent tasks, trace workloads, lab offload, and extensions.

The contract is intentionally small:

- Secret env names are normalized by trimming whitespace, removing empty names, sorting, and deduplicating.
- A `SecretEnvPlan` describes public env, required secret env names, provider credential mappings, and redaction policy without storing secret values.
- A resolver tries ordered value providers and returns materialized `(name, value)` pairs plus status metadata.
- Status metadata records only `name`, `configured`, and `source`; it never includes resolved values.
- Missing required names produce a structured error with normalized missing names and redacted status metadata.
- Provider command execution receives `HOMEBOY_AGENT_TASK_SECRET_ENV_PLAN_JSON`, a serialized `SecretEnvPlan` containing normalized env names, env-name mappings, and redacted configured/missing status metadata. It never contains resolved secret values.

Value providers remain domain-owned. Core does not know where a secret comes from beyond the provider's source label. Current consumers can provide process env, config, keychain, remote runner, or extension-specific providers without adding domain semantics to the shared contract.

Use this primitive when a workflow needs to declare or resolve required secret env names. Add command-specific storage, fallback order, and remediation text at the consumer boundary. Providers should consume the canonical plan instead of recomputing provider-default secret env merges.
