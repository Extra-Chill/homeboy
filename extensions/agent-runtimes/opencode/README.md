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

OpenCode's `--model` argument selects the run session model, but agent-local
configuration can still control built-in agents such as `build` and `title`.
For deterministic run-scoped selection, the executor also injects
`OPENCODE_CONFIG_CONTENT` with:

- `model` and `agent.build.model` from `executor.config.model`,
  `executor.model`, or top-level `model`.
- `small_model` from `executor.config.small_model` or
  `executor.config.smallModel` when provided.
- `agent.title.disable: true` for every agent-task run, after ambient config
  content is layered. Homeboy owns durable task, run, and pull request identity,
  so OpenCode session-title generation is not used. There is no title opt-in in
  this executor; a provider title failure therefore cannot affect coding work.

The title-disable overlay does not change the requested primary build model.
Ambient `agent.title` configuration may supply other fields, but cannot re-enable
title generation for an agent-task run.

Direct runtime verification can be done with a temporary config overlay, without
editing global OpenCode config:

```sh
OPENCODE_CONFIG_CONTENT='{"model":"opencode-go/kimi-k2.7-code","agent":{"build":{"model":"opencode-go/kimi-k2.7-code"}}}' opencode run --model opencode-go/kimi-k2.7-code 'Report the active provider/model for the build agent.'
```

The OpenCode binary is resolved from `executor.config.runtime_bin`,
`executor.config.command`, or `opencode` in that order. Additional leading
command args may be supplied with
`executor.config.command_args` or `HOMEBOY_OPENCODE_COMMAND_ARGS` as a JSON array.

The outcome includes status, diagnostics, and bounded metadata. It intentionally
does not include raw child stdout, stderr, argv, or secret environment values.
