# Homeboy Lab Desktop Cockpit

Homeboy Desktop treats Lab runners as Homeboy core execution targets. The app shells out to the `homeboy runner` CLI contracts and talks to the loopback daemon URL returned by `homeboy runner connect`; it does not implement SSH, tunnel, or daemon transport logic itself.

## Consumed Core Contracts

- `homeboy runner list` provides configured local and SSH runner records.
- `homeboy runner doctor <runner-id>` provides readiness, resources, capabilities, and checks.
- `homeboy runner connect|status|disconnect <runner-id>` owns daemon bootstrap and tunnel session state.
- Daemon `GET /jobs`, `GET /jobs/:id`, `GET /jobs/:id/events`, and `POST /jobs/:id/cancel` provide active job state.
- Daemon `POST /audit|/lint|/test|/bench` queues non-mutating job-ready runs where the connected runner exposes them.
- Daemon `GET /runs/:id/artifacts/sync` and `GET /runs/:id/artifacts/:artifact-id` provide artifact listing and retrieval.

## Current Execution Dependency

First-class runner execution is still tracked in Extra-Chill/homeboy#2529. Until that lands, Desktop keeps local execution as the default and limits the Lab cockpit to connected daemon job-ready endpoints. When Homeboy core adds `homeboy <command> --runner` or equivalent stable command wrappers, Desktop should route existing action surfaces through those CLI wrappers instead of adding transport code.
