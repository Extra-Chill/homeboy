# `homeboy issues`

Reconcile findings against an issue tracker.

## Synopsis

```sh
homeboy issues <COMMAND>
```

## Subcommands

- `reconcile` — reconcile a finding stream against an issue tracker
- `reconcile-run` — reconcile audit/lint/test JSON outputs from one CI output directory
- `build-findings` — convert native command output into the canonical reconcile input shape

## `reconcile-run`

```sh
homeboy issues reconcile-run <component> \
  --path <workspace> \
  --output-dir <dir> \
  --commands audit,lint,test \
  --run-url <url> \
  --apply
```

`reconcile-run` is a typed wrapper around the existing `issues reconcile --from-output` pipeline. It discovers `<command>.json` files in the output directory, skips missing outputs with structured warnings, reports malformed outputs as failures, and returns aggregate created/updated/closed/failure totals for CI consumers.

If `--output-dir` is omitted, Homeboy reads `HOMEBOY_OUTPUT_DIR`.

## Related

- [audit](audit.md)
- [lint](lint.md)
- [test](test.md)
- [report](report.md)
