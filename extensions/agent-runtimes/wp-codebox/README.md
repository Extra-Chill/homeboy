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
policy in code. Existing Data Machine and GitHub-oriented defaults remain declared
there for compatibility, while alternate callers can supply their own generic
manifest values without adding new runtime-specific branches.
