# OpenCode Agent Runtime

`opencode` is a generic repository-scoped agent-task runtime for Homeboy. It runs
the OpenCode CLI behind the `homeboy/agent-task-request/v1` and
`homeboy/agent-task-outcome/v1` contract, so callers can select OpenCode without
embedding CLI details in their own manifests.

## Runtime Contract

`opencode.json` declares the runtime manifest and executor provider contract:

- `command` and `invocation` point at the runtime-local executor wrapper.
- `runner_readiness` advertises the OpenCode executable check and install hint.
- `workspace_tools` declares the default repository workspace tool ids.
- `provider_defaults.codex` declares Codex OAuth secret env names and source
  metadata.
- `provider_preflight.codex` declares the auth checks callers should run before
  launching OpenCode.

The JavaScript package exports the same provider contract through
`providerContract()`, plus `executeOpenCodeAgentTask()` for the CLI wrapper and
tests.

## Executor Behavior

The executor reads one AgentTaskRequest JSON object from stdin or
`HOMEBOY_AGENT_TASK_REQUEST`, validates `executor.backend: "opencode"`, then runs:

```sh
opencode run [--model <model>] [--agent <agent>] [--variant <variant>] [--title <title>] <instructions>
```

The OpenCode binary is resolved from `executor.config.runtime_bin`,
`executor.config.command`, `HOMEBOY_OPENCODE_COMMAND`, or `opencode` in that
order. Additional leading command args may be supplied with
`executor.config.command_args` or `HOMEBOY_OPENCODE_COMMAND_ARGS` as a JSON array.

The outcome includes status, diagnostics, and bounded metadata. It intentionally
does not include raw child stdout, stderr, argv, or secret environment values.
