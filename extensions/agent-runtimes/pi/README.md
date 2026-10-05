# Pi Agent Runtime

`agent-runtimes/pi` runs [Pi](https://github.com/earendil-works/pi) headless for
Homeboy agent task requests (`homeboy/agent-task-request/v1` with
`executor.backend: pi`) and reports a normalized `homeboy/agent-task-outcome/v1`.

## How it runs

```sh
pi --mode json [--session <artifacts>/<task>-pi-session.jsonl | --no-session] [--model <provider/model>] "<instructions>"
```

- Runs in the request workspace, with stdin empty and only allowlisted environment.
- The model comes from `executor.config.model`, `executor.model` or `model`.
- Instructions over 100 KiB are written to a file in the artifacts directory and
  the agent is told to read it (one argv entry is limited to 128 KiB on Linux).
- With an artifacts directory, the Pi session file is written at its top level (`<task>-pi-session.jsonl`). It is evidence, and also Homeboy's liveness signal: Pi appends to it as it works, while process output only arrives when Pi exits.

## Outcome

The JSONL event stream is read for the final assistant message:

| Pi result | Outcome |
|---|---|
| exit 0, final `stopReason` `stop` | `succeeded`, summary from the final text |
| final `stopReason` `error` / `aborted`, or retries exhausted | `provider_error` with Pi's error message |
| final `stopReason` `length` | `failed` (`agent_task.pi_output_limit`) |
| exit 0 without an assistant message | `provider_error` (`agent_task.pi_no_result`) |
| nonzero exit | `failed` with the stderr tail |

Metadata carries the provider, model, stop reason, turns, tool calls and token
usage with cost. Stdout and stderr are saved as artifacts.

## Configure

- `executor.config.command` / `HOMEBOY_PI_COMMAND`: the Pi executable (default `pi`).
- `executor.config.command_args` / `HOMEBOY_PI_COMMAND_ARGS` (JSON array): default `["--mode", "json"]`.
  Keep `--mode json`; the outcome is read from that stream.
- Credentials are Pi's own: its auth file under the agent dir (`PI_CODING_AGENT_DIR`
  passes through) or provider API key env declared via `secret_env` / `env_allowlist`.

Readiness (`scripts/agent/homeboy-pi-provider-readiness.cjs`) resolves the
executable and runs `pi --version`; a missing binary is not ready.

Pi has no MCP client, so this runtime does not claim `runtime_tool_attachment`.

## Tests

```sh
node tests/pi-agent-task-executor-boundary.test.js
node tests/pi-runtime-readiness.test.js
```
