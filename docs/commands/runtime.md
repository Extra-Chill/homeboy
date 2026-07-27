# `homeboy runtime`

Inspect Homeboy core-bundled runtime assets used by extension runners.

## Runtime Packages

Homeboy discovers installable runtime packages from `~/.config/homeboy/agent-runtimes/<runtime-id>/<runtime-id>.json`. Extension repositories can ship shared runtime packages in their top-level `<extension-repo>/agent-runtimes/` directory; `homeboy extension install` copies that directory into the Homeboy config area.

Runtime package manifests declare generic executor providers through `agent_task_executors`. Core consumes provider identity, backend, `invocation.argv`, capabilities, readiness, role aliases, workspace materialization, and secret requirement/default declarations for selection, listing, interpolation, and redacted execution setup. Backend-specific orchestration remains inside the runtime package invocation.

Provider invocation arguments can use `{{runtime_path}}`, and Homeboy injects `HOMEBOY_RUNTIME_PATH`, `HOMEBOY_AGENT_RUNTIME_ID`, and `HOMEBOY_AGENT_RUNTIME_PATH` when executing runtime-package providers.

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

## Controller Runtime Pruning

`homeboy runtime controller-prune` is the specialist command behind the
`controller_runtimes` cleanup category. It removes only content-addressed
controller pins that no nonterminal durable run and no active admission
generation still reference.

```bash
homeboy runtime controller-prune
homeboy runtime controller-prune --apply
```

Pruning honors the configured retention policy — `retention.controller_runtime_days`,
`retention.controller_runtime_max_bytes`, and `retention.limit` — resolved from
the same helper `homeboy cleanup --include controller-runtimes` uses, so the two
entry points always apply the identical window. The resolved policy is echoed in
the command output as `min_age_seconds`, `max_total_bytes`, and `limit`.

`--ignore-retention` discards that window and purges every unreferenced pin. It
is destructive and never the default; reference-based retention still applies,
but the operator's age and size budget does not.
