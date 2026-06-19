# Pi Agent Runtime

`agent-runtimes/pi` is a conservative Homeboy agent runtime integration for Pi.
The exact Pi runtime contract is intentionally not assumed here.

The executor accepts `homeboy/agent-task-request/v1` requests with
`executor.backend` set to `pi` and emits `homeboy/agent-task-outcome/v1`.
Without an explicit command it returns `no_op` with diagnostics explaining how to
configure the adapter. Invalid requests and failed configured commands return
`provider_error`.

## Configure

Set one of these when a concrete Pi adapter command exists:

- `executor.config.command`
- `HOMEBOY_PI_COMMAND`

Optional arguments can be supplied with `executor.config.command_args` or
`HOMEBOY_PI_COMMAND_ARGS` as a JSON string array.

The configured command receives the full request JSON on stdin and in the
`HOMEBOY_AGENT_TASK_REQUEST` environment variable. A successful command exit is
reported as `no_op` until Pi has a stable normalized outcome contract.

## Contract

Print the provider contract:

```sh
node scripts/agent/homeboy-pi-agent-task-executor.cjs --provider-contract
```

Run the focused boundary test:

```sh
npm run test:pi-agent-task-executor-boundary
```
