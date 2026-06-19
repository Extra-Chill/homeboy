# Codex Agent Runtime

This package is the first-class Homeboy runtime surface for Codex agent tasks.
It is intentionally separate from WP Codebox provider settings: callers select the
`codex` runtime/backend directly when they want a repository-scoped Codex CLI
process instead of a WordPress sandbox.

## Contract

- Manifest: `codex.json`
- Runtime id: `codex`
- Backend id: `codex`
- Provider id: `codex.agent-task-executor`
- Executor command: `node {{runtime_path}}/scripts/agent/homeboy-codex-agent-task-executor.cjs`
- Default Codex command: `codex`
- Default Codex args: `exec`

The executor accepts one `homeboy/agent-task-request/v1` JSON object on stdin or
through `HOMEBOY_AGENT_TASK_REQUEST`. It validates `executor.backend=codex`,
`executor.runtime=codex`, `task_id`, `instructions`, and `executor.config`, then
invokes the configured Codex command synchronously and emits one
`homeboy/agent-task-outcome/v1` JSON object.

## Configuration

Callers may override the provisional CLI invocation contract with:

- `executor.config.command`, or `HOMEBOY_CODEX_COMMAND`
- `executor.config.command_args`, or JSON array `HOMEBOY_CODEX_COMMAND_ARGS`
- `executor.config.model`
- `executor.config.cwd`
- `executor.config.timeout_seconds`, or `limits.task_timeout_seconds`

Secret values are inherited from the runner environment and are never included in
the outcome. The declared Codex secret env names are:

- `AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN`
- `AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN`
- `AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT`
- `AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID`
- `AI_PROVIDER_OPENAI_CODEX_FEDRAMP`

## Capabilities

The runtime advertises CLI execution, repository workspace materialization,
workspace tools, patch/report artifacts, structured outcomes, provider-owned auth,
provider-owned sessions, and provider-owned cancellation. It does not advertise a
WordPress sandbox or nested OpenCode orchestration.
