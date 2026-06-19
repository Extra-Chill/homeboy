# Local Shell Agent Runtime

`local-shell` is a generic non-WordPress runtime for deterministic agent task loops. It runs an explicit command supplied by the `AgentTaskRequest` and returns a normalized `AgentTaskOutcome`.

This runtime is intentionally small:

- It reads one `homeboy/agent-task-request/v1` JSON object from stdin.
- It accepts only `executor.backend: "local-shell"`.
- It runs `executor.config.command` with optional `args`, `cwd`, `env`, and `timeout_seconds`.
- It emits one `homeboy/agent-task-outcome/v1` JSON object to stdout.
- It records bounded command metadata and exit status, but not raw child stdout, stderr, or command arguments.

The runtime does not know about WordPress, Data Machine, WPSG, WP Codebox, or any domain workflow. Callers own domain policy and decide which commands are safe to pass.
