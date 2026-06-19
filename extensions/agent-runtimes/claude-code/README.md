# Claude Code Agent Runtime

`claude-code` is a standalone Homeboy agent-task runtime contract for the Claude Code provider. It declares the provider manifest, OAuth environment contract, and a thin executor boundary for callers that need to route generic `homeboy/agent-task-request/v1` payloads to a configured adapter.

## OAuth Environment

The provider uses OAuth credentials supplied through environment variables:

- `AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN` is required.
- `AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN` is optional.
- `AI_PROVIDER_CLAUDE_CODE_EXPIRES_AT` is optional.

The manifest advertises only those `AI_PROVIDER_CLAUDE_CODE_*` variables. Callers own credential refresh and pass the values through Homeboy's secret environment handling.

## Executor Boundary

The included executor validates the generic Homeboy request and requires an explicit adapter command through `executor.config.command` or `HOMEBOY_CLAUDE_CODE_AGENT_TASK_COMMAND`. The request is passed to that adapter as JSON on stdin, and the wrapper emits a normalized `homeboy/agent-task-outcome/v1` without copying adapter stdout, stderr, command arguments, or credential values into the outcome.

This package intentionally keeps runtime policy minimal. Workspace setup, tool exposure, credential refresh, and adapter installation remain caller-owned concerns.
