# Lab Command

Hidden compatibility shortcut for Lab routing helpers. Keep using `homeboy lab
...` when you already know the shortcut; use [`homeboy runner`](runner.md) for
the discoverable runner and Lab offload surface.

`lab` is intentionally hidden from top-level help. It remains available so older
automation and operator notes keep working while discoverable guidance lives in
`runner` docs.

Treat `homeboy lab` as a routing helper, not a benchmark executor. Benchmark
workloads should start at `homeboy bench`; Lab routing then selects a runner when
the command and configuration support offload.

## Synopsis

```bash
homeboy lab status
homeboy lab extension-sync --runner <runner-id> --source <url-or-path> --id <extension-id> --ref <ref>
```

## Description

The `lab` command remains a compatibility shortcut for Lab runner availability
and the Homeboy-managed follow-up commands to use when a Lab run needs diagnosis
or replay. For normal benchmark runs, use `homeboy bench <component>`: Homeboy
automatically selects a Lab runner when the component declares
`lab.preferred_runner` or when exactly one Lab runner is configured.

`homeboy lab status` resolves the default or inferred Lab runner and includes its
readiness report in `selected_runner`. Pass `--runner <runner-id>` only when you
need to inspect a specific non-default runner.

`homeboy lab status` always returns run/artifact discovery commands in
`managed_followups`, and adds runner diagnostics when Homeboy can select a Lab
runner. Use those commands instead of raw SSH or runner path spelunking for
routine lifecycle work:

- `homeboy runs list --limit 5` finds recent persisted run records.
- `homeboy runs latest-run --kind bench` resolves the latest benchmark run id.
- `homeboy runs artifacts <run-id>` lists recorded run artifacts through
  Homeboy.
- `homeboy runs evidence <run-id>` shows the stable evidence summary and
  reviewer-facing commands for one persisted run.
- `homeboy runs refs --kind bench --limit 10` lists recent benchmark run and
  artifact refs when you need shareable evidence handles.
- `homeboy runner doctor <runner>` probes runner tools, workspace writability,
  artifact storage, and browser readiness.
- `homeboy runner env <runner>` prints the redacted runner job environment.
- `homeboy runner disconnect <runner> && homeboy runner connect <runner>`
  restarts the active runner daemon so Lab offload uses the currently configured
  runner-side Homeboy binary.
- `homeboy upgrade --force --upgrade-runner <runner>` refreshes the configured
  runner-side Homeboy binary when the binary itself is behind merged fixes.
- `homeboy runner exec <runner> -- <command>` runs a managed follow-up command
  through Homeboy.
- `homeboy runner workspace sync <runner> --path <path> --mode snapshot`
  materializes the current checkout into the Lab workspace before a replay or
  follow-up run.

Component or extension settings can declare runner-managed dependency sources
with `validation_dependencies`. Lab workspace sync resolves those dependencies,
checks freshness, runs install/build lifecycle, materializes them beside the
primary workspace, and returns dependency provenance in the generic
`validation_dependencies` output field. Use that contract for eval/bench source
overrides instead of ad-hoc workflow-specific environment exports or hardcoded
runner paths.

Lab status and offloaded run metadata include `runner_homeboy`, which names the
configured runner Homeboy executable, the active daemon version/build identity
when connected, stale-daemon details, and the exact refresh/upgrade commands.
For `agent-task run-plan --runner homeboy-lab`, check this field before assuming
the Lab runner has picked up a merged CLI fix. If the daemon identity is stale,
run the refresh command pair; if the configured executable is old, run the
upgrade command first and reconnect the runner.

Lab offload always re-enters Homeboy through the runner's configured executable.
Synced source checkouts are task workspaces, not active Homeboy binaries. Preflight
metadata includes `source_checkout` with the local path, Git branch, SHA, remote,
and dirty state, and stderr prints the source checkout ref/path beside the active
runner Homeboy command before remote execution starts. Legacy
`lab.self_command_prefix` values in `homeboy.json` are ignored and are not
preserved by portable config rewrites.

`homeboy lab extension-sync` updates a Lab runner's installed extension through
the runner API. Use it to pin a runner-side runtime dependency, such as the
installed Homeboy Extensions `wordpress` extension, to a specific source ref
without writing a manual `homeboy runner exec ... extension install` command.
When `--runner` is omitted, Homeboy uses `lab.preferred_runner` or the only
inferable SSH Lab runner.

When `--source` is an existing directory on the controller, Homeboy first
materializes that directory into the runner workspace with snapshot sync and
passes the runner-side `_lab_workspaces/...` path to `homeboy extension install`.
URL sources and paths that do not exist on the controller are forwarded as-is,
which preserves runner-local source paths.

When runner-backed Lab commands fail, Homeboy promotes runner-side Homeboy JSON
errors from stdout, stderr, or job event messages into the top-level command
error. The promoted error includes the runner id, job id, remote cwd, command,
exit code, parsed `runner_error`, and the full runner execution payload for deep
debugging.

## Commands

- `status`: Show configured Lab runners and benchmark routing guidance.
- `extension-sync`: Install or replace a Lab runner extension from a source and
  ref. Successful output returns the runner id, runner `homeboy_path`, install
  command, and remote execution output; failures surface the runner-side root
  cause in the top-level error.

## Related

- [Bench command](bench.md)
- [Runner command](runner.md)
