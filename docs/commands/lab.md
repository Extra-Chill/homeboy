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

`homeboy lab status` returns `managed_followups` when Homeboy can select a Lab
runner. Use those commands instead of raw SSH for routine lifecycle work:

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

## Commands

- `status`: Show configured Lab runners and benchmark routing guidance.
- `bench`: Run a benchmark through the standard benchmark pipeline with Lab
  routing intent.
- `extension-sync`: Install or replace a Lab runner extension from a source and
  ref, returning the runner id, runner `homeboy_path`, install command, and
  remote execution output.

## Related

- [Bench command](bench.md)
- [Runner command](runner.md)
