# Lab Command

Inspect and use configured Lab runners for remote benchmark execution and
managed follow-up work.

## Synopsis

```bash
homeboy lab status
homeboy lab bench <component> [options] [-- <runner-args>]
homeboy lab extension-sync --runner <runner-id> --source <url-or-path> --id <extension-id> --ref <ref>
```

## Description

The `lab` command surfaces Lab runner availability, benchmark routing, and the
Homeboy-managed follow-up commands to use when a Lab run needs diagnosis or
replay. For normal benchmark runs, prefer `homeboy bench <component>`: Homeboy
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
- `homeboy runner doctor <runner>` probes runner tools, workspace writability,
  artifact storage, and browser readiness.
- `homeboy runner env <runner>` prints the redacted runner job environment.
- `homeboy runner exec <runner> -- <command>` runs a managed follow-up command
  through Homeboy.
- `homeboy runner workspace sync <runner> --path <path> --mode snapshot`
  materializes the current checkout into the Lab workspace before a replay or
  follow-up run.

`homeboy lab bench` delegates to the same benchmark path while making the Lab
intent explicit at the command surface. Its output includes the same managed
follow-up hints so the operator can keep a failed bench inside the Homeboy
workflow.

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
- `bench`: Run a benchmark through the standard benchmark pipeline with Lab
  routing intent.
- `extension-sync`: Install or replace a Lab runner extension from a source and
  ref. Successful output returns the runner id, runner `homeboy_path`, install
  command, and remote execution output; failures surface the runner-side root
  cause in the top-level error.

## Related

- [Bench command](bench.md)
- [Runner command](runner.md)
