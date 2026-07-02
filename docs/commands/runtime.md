# `homeboy runtime`

Inspect Homeboy core-bundled runtime assets used by extension runners.

## Runtime Packages

Homeboy discovers installable runtime packages from `~/.config/homeboy/agent-runtimes/<runtime-id>/<runtime-id>.json`. Extension repositories can ship shared runtime packages in their top-level `<extension-repo>/agent-runtimes/` directory; `homeboy extension install` copies that directory into the Homeboy config area.

Runtime package manifests declare generic executor providers through `agent_task_executors`. Core consumes provider identity, backend, command, capabilities, readiness, role aliases, workspace materialization, and secret requirement/default declarations for selection, listing, interpolation, and redacted execution setup. Backend-specific orchestration remains inside the runtime package command.

Provider commands can use `{{runtime_path}}`, and Homeboy injects `HOMEBOY_RUNTIME_PATH`, `HOMEBOY_AGENT_RUNTIME_ID`, and `HOMEBOY_AGENT_RUNTIME_PATH` when executing runtime-package providers.

Refresh a package from a local checkout on a Lab runner in one command:

```bash
homeboy --runner <runner-id> runtime refresh <runtime-id> --source <local-runtime-source> --allow-dirty-lab-workspace
```

When Lab offload sees `runtime refresh --source <local directory>`, it snapshots that source directory to the runner, rewrites `--source` to the runner path, and records source identity metadata including branch, SHA, remote, and dirty state. Use `--allow-dirty-lab-workspace` when intentionally refreshing from uncommitted local changes.

## Helper Paths

Resolve the materialized path for a core-bundled runtime helper:

```bash
homeboy runtime helper path runner-prelude.sh
homeboy runtime helper path HOMEBOY_RUNTIME_COMMAND_CAPTURE
```

The command accepts only known helper filenames or their corresponding injected
`HOMEBOY_RUNTIME_*` environment variable names. It resolves the same helper
assets that Homeboy automatically materializes and injects into extension runner
environments; it is not a runtime package browser, extension asset resolver, or
arbitrary config-path lookup.

Use `--plain` when a shell wrapper needs a sourceable path without parsing JSON:

```bash
source "$(homeboy runtime helper path --plain runner-prelude.sh)"
```
