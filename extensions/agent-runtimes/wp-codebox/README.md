# WP Codebox Agent Runtime

This package is the first-class WP Codebox agent-task runtime used by Homeboy.
It owns the provider contract, AgentTaskRequest-to-WP-Codebox request mapping,
runtime CLI, and normalized AgentTaskOutcome conversion.

WordPress extension code consumes this runtime. Internal WordPress wrapper paths
may exist for tests and scripts, but new runtime consumers should import this
package or invoke the runtime-local CLI.
