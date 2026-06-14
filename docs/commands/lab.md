# Lab Command

Inspect and use configured Lab runners for remote benchmark execution.

## Synopsis

```bash
homeboy lab status
homeboy lab bench <component> [options] [-- <runner-args>]
homeboy lab extension-sync --runner <runner-id> --source <url-or-path> --id <extension-id> --ref <ref>
```

## Description

The `lab` command surfaces Lab runner availability and provides a discoverable
benchmark entry point for remote execution. For normal benchmark runs, prefer
`homeboy bench <component>`: Homeboy automatically selects a Lab runner when the
component declares `lab.preferred_runner` or when exactly one Lab runner is
configured.

`homeboy lab bench` delegates to the same benchmark path while making the Lab
intent explicit at the command surface.

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
