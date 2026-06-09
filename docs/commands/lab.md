# Lab Command

Inspect and use configured Lab runners for remote benchmark execution.

## Synopsis

```bash
homeboy lab status
homeboy lab bench <component> [options] [-- <runner-args>]
```

## Description

The `lab` command surfaces Lab runner availability and provides a discoverable
benchmark entry point for remote execution. For normal benchmark runs, prefer
`homeboy bench <component>`: Homeboy automatically selects a Lab runner when the
component declares `lab.preferred_runner` or when exactly one Lab runner is
configured.

`homeboy lab bench` delegates to the same benchmark path while making the Lab
intent explicit at the command surface.

## Commands

- `status`: Show configured Lab runners and benchmark routing guidance.
- `bench`: Run a benchmark through the standard benchmark pipeline with Lab
  routing intent.

## Related

- [Bench command](bench.md)
- [Runner command](runner.md)
