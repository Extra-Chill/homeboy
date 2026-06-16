# WP Codebox Agent Runtime

This package is the first-class WP Codebox agent-task runtime surface used by
Homeboy. It exposes the provider contract, AgentTaskRequest-to-WP-Codebox
request mapping, runtime CLI, and normalized AgentTaskOutcome conversion.

The installed WordPress extension carries the runnable implementation so its
provider command works without a sibling `agent-runtimes` checkout. New runtime
consumers should import this package or invoke the runtime-local CLI.
